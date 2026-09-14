use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail, ensure};
use jj_lib::backend::CommitId;
use jj_lib::git::ImportedRemoteMapping;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::{RefTarget, View};
use jj_lib::project::{
    BindingId, BindingRecord, BindingTarget, ConnectionId, ObservationKind, ProjectId,
};
use jj_lib::ref_name::{RefName, RefNameBuf, RemoteNameBuf};
use jj_lib::repo_path::RepoPath;

/// An imported fragment, with destination names and source commit IDs.
/// Preparing and reserving this fragment never activates a source endpoint.
pub(crate) struct Plan {
    pub view: View,
    pub remotes: BTreeMap<RemoteNameBuf, ImportedRemoteMapping>,
}

/// `configured_remotes` covers all effective remote sections; each value is true
/// if any section declares a URL or push URL, including empty/implicit values.
pub(crate) fn prepare(
    source: &View,
    destination: &View,
    name: &str,
    mount: &RepoPath,
    configured_remotes: &BTreeMap<RemoteNameBuf, bool>,
) -> Result<Plan> {
    crate::native_project::validate_project(name)?;
    ensure!(
        !mount.is_root(),
        "Preserving projects requires a non-root source mount"
    );
    validate_source(source)?;
    check_id_collisions(source, destination, mount)?;

    let mut view = source.clone();
    view.git_refs.clear();
    view.git_heads.clear();
    view.wc_commit_ids.clear();
    view.wc_sparse_patterns.clear();
    for (id, target) in &mut view.project_state.projects {
        *target = target.map(|record| {
            record.as_ref().map(|record| {
                let mut record = record.clone();
                record.canonical_root = record
                    .canonical_root
                    .components()
                    .fold(mount.to_owned(), |root, component| root.join(component));
                record
            })
        });
        for record in target.iter().flatten() {
            for (other_id, other) in &destination.project_state.projects {
                for other in other.iter().flatten() {
                    ensure!(
                        record.name != other.name,
                        "Source project {id} name {:?} is already claimed by destination project {other_id}",
                        record.name
                    );
                    ensure!(
                        !record.canonical_root.starts_with(&other.canonical_root)
                            && !other.canonical_root.starts_with(&record.canonical_root),
                        "Imported project {} root {} overlaps destination project {} root {}",
                        record.name,
                        record.canonical_root.as_internal_file_string(),
                        other.name,
                        other.canonical_root.as_internal_file_string()
                    );
                }
            }
        }
    }
    let destination_view = jj_lib::view::View::new(destination.clone(), false);
    for (label, target) in &source.project_state.labels {
        ensure!(
            !destination.project_state.labels.contains_key(label),
            "Source project label {label:?} is already present in the destination"
        );
        if target.is_present() {
            destination_view
                .check_project_label_available(label)
                .map_err(anyhow::Error::msg)?;
            ensure!(
                !destination
                    .git_refs
                    .keys()
                    .any(|reference| suffix(reference.as_str()) == Some(label.as_str())),
                "Project label {label:?} would adopt existing literal Git references; migrate them explicitly before importing"
            );
        }
    }

    let map_name = |reference: &RefName| -> Result<RefNameBuf> {
        if suffix(reference.as_str()).is_some_and(|label| {
            source
                .project_state
                .labels
                .get(label)
                .is_some_and(Merge::is_present)
        }) {
            return Ok(reference.to_owned());
        }
        // A prefix does not change a trailing #label. It must not turn a source
        // literal into a destination-scoped reference.
        ensure!(
            !suffix(reference.as_str()).is_some_and(|label| destination
                .project_state
                .labels
                .get(label)
                .is_some_and(Merge::is_present)),
            "Source literal reference {:?} would acquire a destination project scope; rename it before importing",
            reference.as_str()
        );
        Ok(format!("{name}/{}", reference.as_str()).into())
    };
    view.local_bookmarks = source
        .local_bookmarks
        .iter()
        .map(|(reference, target)| Ok((map_name(reference)?, target.clone())))
        .collect::<Result<_>>()?;
    view.local_tags = source
        .local_tags
        .iter()
        .map(|(reference, target)| Ok((map_name(reference)?, target.clone())))
        .collect::<Result<_>>()?;
    check_ref_collisions(
        &view.local_bookmarks,
        &destination.local_bookmarks,
        "bookmark",
    )?;
    check_ref_collisions(&view.local_tags, &destination.local_tags, "tag")?;

    let source_remotes: BTreeSet<_> = source
        .remote_views
        .keys()
        .chain(source.remote_connections.keys())
        .chain(source.project_observations.keys().map(|key| &key.remote))
        .chain(configured_remotes.keys())
        .collect();
    let destination_remotes: BTreeSet<_> = destination
        .remote_views
        .keys()
        .chain(destination.remote_connections.keys())
        .chain(
            destination
                .project_observations
                .keys()
                .map(|key| &key.remote),
        )
        .collect();
    let mut remotes = BTreeMap::new();
    let mut physical_names = BTreeSet::new();
    for remote in source_remotes {
        let active_connection = source
            .remote_connections
            .get(remote)
            .and_then(Merge::as_resolved)
            .and_then(Option::as_ref);
        let required_capability = active_connection
            .map(|connection| source.project_state.binding_for_connection(connection))
            .transpose()
            .map_err(anyhow::Error::msg)?
            .flatten()
            .is_some();
        ensure!(
            !configured_remotes.get(remote).copied().unwrap_or(false) || required_capability,
            "Source remote {} has a configured endpoint without an active project binding. \
             Regular Git fetch cannot continue its history after mounting it; choose outer-project \
             import with --nested, or keep this source in a separate repository",
            remote.as_str()
        );
        let mut connection = active_connection;
        // Detached observations still own immutable identity, but do not create
        // an active remote_connections entry in the imported view.
        for observation in source
            .project_observations
            .iter()
            .filter(|(key, _)| &key.remote == remote)
            .flat_map(|(_, target)| target.iter().flatten())
        {
            ensure!(
                connection.is_none_or(|connection| connection == &observation.connection_id),
                "Source remote {} has incompatible retained connection identities",
                remote.as_str()
            );
            connection = Some(&observation.connection_id);
        }
        let project_owned = connection.is_some_and(|connection| {
            source
                .project_state
                .remote_names
                .get(connection)
                .is_some_and(Merge::is_present)
                || binding_definitions(source)
                    .any(|(_, binding)| &binding.connection_id == connection)
        });
        let physical: RemoteNameBuf = if project_owned {
            format!("jjosh-{}", connection.unwrap().hex()).into()
        } else {
            // Git's remote-ref parser splits the physical remote at the first
            // slash; only the reference name, not this key, may use NAME/path.
            ensure!(
                !remote.as_str().contains('/'),
                "Source physical remote {:?} contains a slash; rename it before preserving projects",
                remote.as_str()
            );
            format!("{name}-{}", remote.as_str()).into()
        };
        jj_lib::git::validate_remote_name(&physical)
            .with_context(|| format!("Invalid imported physical remote {:?}", physical.as_str()))?;
        ensure!(
            physical_names.insert(physical.clone()),
            "Source remotes map to the same physical remote {}; resolve their connection ownership before importing",
            physical.as_str()
        );
        ensure!(
            !destination_remotes.contains(&physical),
            "Imported physical remote {} is already present in the destination",
            physical.as_str()
        );
        let reference_prefix = if project_owned {
            String::new()
        } else {
            format!("{name}/")
        };
        remotes.insert(
            remote.clone(),
            ImportedRemoteMapping {
                bookmark_destination: format!(
                    "refs/remotes/{}/{reference_prefix}",
                    physical.as_str()
                ),
                tag_destination: format!(
                    "{}{}/{reference_prefix}",
                    jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
                    physical.as_str()
                ),
                destination: physical,
                connection: connection.cloned(),
                required_capability,
            },
        );
    }
    view.remote_views = source
        .remote_views
        .iter()
        .map(|(remote, refs)| {
            let mut refs = refs.clone();
            refs.bookmarks = refs
                .bookmarks
                .into_iter()
                .map(|(reference, target)| Ok((map_name(&reference)?, target)))
                .collect::<Result<_>>()?;
            refs.tags = refs
                .tags
                .into_iter()
                .map(|(reference, target)| Ok((map_name(&reference)?, target)))
                .collect::<Result<_>>()?;
            Ok((remotes[remote].destination.clone(), refs))
        })
        .collect::<Result<_>>()?;
    view.remote_connections = source
        .remote_connections
        .iter()
        .map(|(remote, owner)| (remotes[remote].destination.clone(), owner.clone()))
        .collect();
    view.project_observations = source
        .project_observations
        .iter()
        .map(|(key, target)| {
            let mut key = key.clone();
            key.remote = remotes[&key.remote].destination.clone();
            // Revision observations are keyed by raw OID, not by a reference name.
            if key.kind != ObservationKind::Revision {
                key.name = map_name(&key.name)?;
            }
            Ok((key, target.clone()))
        })
        .collect::<Result<_>>()?;
    Ok(Plan { view, remotes })
}

fn suffix(name: &str) -> Option<&str> {
    name.rsplit_once('#').map(|(_, label)| label)
}

fn binding_definitions(view: &View) -> impl Iterator<Item = (&BindingId, &BindingRecord)> {
    view.project_state
        .bindings
        .iter()
        .flat_map(|(id, target)| target.iter().flatten().map(move |record| (id, record)))
        .chain(
            view.project_observations
                .values()
                .flat_map(|target| target.iter().flatten())
                .map(|observation| (&observation.binding_id, &observation.binding)),
        )
}

fn validate_source(source: &View) -> Result<()> {
    // The operation store validates record encodings. Apply the same semantic
    // diagnostics used by project check, then inspect detached/negative evidence
    // as well: it must remain valid after relocating all projects.
    for (id, binding) in binding_definitions(source) {
        ensure!(
            !matches!(binding.target, BindingTarget::RepositoryView),
            "Cannot preserve RepositoryView binding {id}: whole-repository conversions cannot be relocated under a mount. Keep this repository separate, or choose outer-project import with --nested"
        );
    }
    let source_view = jj_lib::view::View::new(source.clone(), false);
    if let Some(diagnostic) = source_view.project_diagnostics().first() {
        bail!(
            "Resolve source project metadata before importing: {}",
            diagnostic.message
        );
    }
    let mut definitions = BTreeMap::new();
    let mut scopes = BTreeMap::new();
    for (id, binding) in binding_definitions(source) {
        if let Some(previous) = definitions.insert(id, binding) {
            ensure!(
                previous == binding,
                "Source binding {id} has inconsistent immutable definitions"
            );
        }
        let BindingTarget::Project(project) = &binding.target else {
            unreachable!()
        };
        source
            .project_state
            .validate_project(project)
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!("Source binding {id} requires its retained project definition")
            })?;
        if let Some(previous) = scopes.insert(&binding.connection_id, project) {
            ensure!(
                previous == project,
                "Source connection {} has incompatible retained project scopes",
                binding.connection_id
            );
        }
        if let Some(identity) = source
            .project_state
            .remote_names
            .get(&binding.connection_id)
            .and_then(Merge::as_resolved)
            .and_then(Option::as_ref)
        {
            ensure!(
                &identity.project == project,
                "Source binding {id} conflicts with its connection's project scope"
            );
        }
    }
    for (connection, names) in &source.project_state.remote_names {
        if names.is_present() {
            ensure!(source.remote_connections.values().any(|owners| owners.as_resolved().and_then(Option::as_ref) == Some(connection)),
                "Source scoped connection {connection} has no physical remote mapping; reconnect or retire it before importing");
        }
    }
    for (key, target) in &source.project_observations {
        for observation in target.iter().flatten() {
            ensure!(
                observation.connection_id == observation.binding.connection_id,
                "Source observation {}@{} has incompatible binding and connection identities",
                key.name.as_str(),
                key.remote.as_str()
            );
            if let Some(owner) = source
                .remote_connections
                .get(&key.remote)
                .and_then(Merge::as_resolved)
                .and_then(Option::as_ref)
            {
                ensure!(
                    owner == &observation.connection_id,
                    "Source observation {}@{} belongs to a different connection",
                    key.name.as_str(),
                    key.remote.as_str()
                );
            }
            if let Some(label) = suffix(key.name.as_str())
                && let Some(target) = source.project_state.labels.get(label)
            {
                ensure!(
                    target
                        .as_resolved()
                        .and_then(Option::as_ref)
                        .is_some_and(|project| observation.binding.target
                            == BindingTarget::Project(project.clone())),
                    "Source observation {}@{} has an incompatible project label",
                    key.name.as_str(),
                    key.remote.as_str()
                );
            }
        }
    }
    Ok(())
}

fn project_ids(view: &View) -> BTreeSet<&ProjectId> {
    view.project_state
        .projects
        .keys()
        .chain(
            view.project_state
                .labels
                .values()
                .flat_map(|target| target.iter().flatten()),
        )
        .chain(
            view.project_state
                .remote_names
                .values()
                .flat_map(|target| target.iter().flatten())
                .map(|name| &name.project),
        )
        .chain(
            binding_definitions(view).filter_map(|(_, binding)| match &binding.target {
                BindingTarget::Project(project) => Some(project),
                BindingTarget::RepositoryView => None,
            }),
        )
        .collect()
}

fn connection_ids(view: &View) -> BTreeSet<&ConnectionId> {
    view.project_state
        .remote_names
        .keys()
        .chain(
            view.remote_connections
                .values()
                .flat_map(|target| target.iter().flatten()),
        )
        .chain(binding_definitions(view).map(|(_, binding)| &binding.connection_id))
        .chain(
            view.project_observations
                .values()
                .flat_map(|target| target.iter().flatten())
                .map(|observation| &observation.connection_id),
        )
        .collect()
}

fn check_id_collisions(source: &View, destination: &View, mount: &RepoPath) -> Result<()> {
    let occupied_projects = project_ids(destination);
    for id in project_ids(source) {
        if occupied_projects.contains(id) {
            if let Some(source_record) = source
                .project_state
                .projects
                .get(id)
                .and_then(Merge::as_resolved)
                .and_then(Option::as_ref)
            {
                let root = source_record
                    .canonical_root
                    .components()
                    .fold(mount.to_owned(), |root, component| root.join(component));
                if let Some(destination_record) = destination
                    .project_state
                    .projects
                    .get(id)
                    .and_then(Merge::as_resolved)
                    .and_then(Option::as_ref)
                {
                    ensure!(
                        destination_record.canonical_root == root,
                        "Source project ID {id} has a same-ID conflicting placement: destination root {}, imported root {}. The existing identity cannot be relocated by import",
                        destination_record.canonical_root.as_internal_file_string(),
                        root.as_internal_file_string()
                    );
                }
            }
            bail!(
                "Source project ID {id} is already present in the destination; preserving projects cannot import the same identity twice"
            );
        }
    }
    let occupied_bindings: BTreeSet<_> = destination
        .project_state
        .bindings
        .keys()
        .chain(binding_definitions(destination).map(|(id, _)| id))
        .collect();
    for id in source
        .project_state
        .bindings
        .keys()
        .chain(binding_definitions(source).map(|(id, _)| id))
    {
        ensure!(
            !occupied_bindings.contains(id),
            "Source binding ID {id} is already present in the destination"
        );
    }
    let occupied_connections = connection_ids(destination);
    for id in connection_ids(source) {
        ensure!(
            !occupied_connections.contains(id),
            "Source connection ID {id} is already present in the destination"
        );
    }
    Ok(())
}

fn check_ref_collisions(
    source: &BTreeMap<RefNameBuf, RefTarget>,
    destination: &BTreeMap<RefNameBuf, RefTarget>,
    kind: &str,
) -> Result<()> {
    for name in source.keys() {
        ensure!(
            !destination.contains_key(name),
            "Imported {kind} {:?} is already present in the destination; rename it before importing",
            name.as_str()
        );
    }
    Ok(())
}

impl Plan {
    /// Reserve names and metadata in a disposable preflight view. Source commit
    /// IDs are intentional here; callers must never publish this view.
    pub fn reserve_into(&self, destination: &mut View) {
        self.extend_into(destination, None);
    }

    /// The caller supplies the complete source-to-destination commit mapping.
    pub fn merge_into(&self, destination: &mut View, ids: &HashMap<CommitId, CommitId>) {
        self.extend_into(destination, Some(ids));
    }

    fn extend_into(&self, destination: &mut View, ids: Option<&HashMap<CommitId, CommitId>>) {
        let map_id = |id: &CommitId| ids.map_or_else(|| id.clone(), |ids| ids[id].clone());
        let map_reference = |target: &RefTarget| {
            RefTarget::from_merge(target.as_merge().map(|id| id.as_ref().map(&map_id)))
        };
        if ids.is_some() {
            destination
                .head_ids
                .extend(self.view.head_ids.iter().map(&map_id));
        }
        destination.local_bookmarks.extend(
            self.view
                .local_bookmarks
                .iter()
                .map(|(name, target)| (name.clone(), map_reference(target))),
        );
        destination.local_tags.extend(
            self.view
                .local_tags
                .iter()
                .map(|(name, target)| (name.clone(), map_reference(target))),
        );
        destination
            .remote_views
            .extend(self.view.remote_views.iter().map(|(name, remote)| {
                let mut remote = remote.clone();
                for reference in remote
                    .bookmarks
                    .values_mut()
                    .chain(remote.tags.values_mut())
                {
                    reference.target = map_reference(&reference.target);
                }
                (name.clone(), remote)
            }));
        destination
            .project_state
            .projects
            .extend(self.view.project_state.projects.clone());
        destination
            .project_state
            .bindings
            .extend(self.view.project_state.bindings.clone());
        destination
            .project_state
            .labels
            .extend(self.view.project_state.labels.clone());
        destination
            .project_state
            .remote_names
            .extend(self.view.project_state.remote_names.clone());
        destination
            .remote_connections
            .extend(self.view.remote_connections.clone());
        destination
            .project_observations
            .extend(self.view.project_observations.iter().map(|(key, target)| {
                let target = target.map(|observation| {
                    observation.as_ref().map(|observation| {
                        let mut observation = observation.clone();
                        for term in &mut observation.terms {
                            term.canonical = term.canonical.as_ref().map(&map_id);
                        }
                        observation
                    })
                });
                (key.clone(), target)
            }));
    }
}
