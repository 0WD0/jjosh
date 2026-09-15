use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use jj_lib::backend;
use jj_lib::backend::CommitId;
use jj_lib::backend::Signature;
use jj_lib::backend::TreeId;
use jj_lib::commit::Commit;
use jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::op_store::View;
use jj_lib::project::BindingTarget;
use jj_lib::project::ConnectionId;
use jj_lib::project::ProjectId;
use jj_lib::project::ScopedRemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::MutableRepo;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPath;

use crate::native_source::NativeSource;

pub(crate) struct Imported {
    /// The complete parent closure, parent-first, not just visible commits.
    pub commits: Vec<Commit>,
    pub stripped_signatures: usize,
    pub ids: HashMap<CommitId, CommitId>,
    /// Source commits reused by change ID instead of rewritten.
    pub grafts: Vec<(CommitId, CommitId)>,
}

enum CommitVisit {
    Read(CommitId),
    Write(CommitId),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProjectVersionMetadata {
    description: String,
    author: Signature,
    committer: Signature,
}

impl From<&backend::Commit> for ProjectVersionMetadata {
    fn from(commit: &backend::Commit) -> Self {
        Self {
            description: commit.description.clone(),
            author: commit.author.clone(),
            committer: commit.committer.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProjectVersionKey {
    metadata: ProjectVersionMetadata,
    root_tree: Merge<TreeId>,
    conflict_labels: Merge<String>,
}

impl From<&backend::Commit> for ProjectVersionKey {
    fn from(commit: &backend::Commit) -> Self {
        Self {
            metadata: commit.into(),
            root_tree: commit.root_tree.clone(),
            conflict_labels: commit.conflict_labels.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct ExistingProjectVersion {
    id: CommitId,
    parents: Vec<CommitId>,
}

/// Content identity fallback for Git histories which lost jj's change-id header.
///
/// Native local imports preserve source ChangeIds, while a later ordinary Git export can
/// legitimately omit the `change-id` header. The raw Git commit then gets a synthetic ChangeId
/// when fetched again. Index only destination commits whose ordinary metadata occurs in this
/// source graph, and compare the complete project subtree plus already-remapped parents before
/// reusing one. This is deliberately stricter than matching descriptions or trees alone.
async fn index_existing_project_versions(
    source: &NativeSource,
    dest: &dyn jj_lib::repo::Repo,
    mount: &RepoPath,
    already_mapped: &HashMap<CommitId, CommitId>,
) -> Result<HashMap<ProjectVersionKey, Vec<ExistingProjectVersion>>> {
    let source_metadata: HashSet<_> = source
        .commits
        .iter()
        .filter(|(id, commit)| {
            !already_mapped.contains_key(*id)
                && commit.change_id
                    == jj_lib::git_backend::synthetic_change_id_from_git_commit_id(id)
        })
        .map(|(_, commit)| ProjectVersionMetadata::from(commit))
        .collect();
    if source_metadata.is_empty() {
        return Ok(HashMap::new());
    }
    let root = dest.store().root_commit_id();
    let mut pending: Vec<_> = dest.view().heads().iter().cloned().collect();
    let mut visited = HashSet::new();
    let mut versions: HashMap<ProjectVersionKey, Vec<ExistingProjectVersion>> = HashMap::new();
    while let Some(id) = pending.pop() {
        if id == *root || !visited.insert(id.clone()) {
            continue;
        }
        let commit = dest.store().get_commit_async(&id).await?;
        pending.extend(commit.parent_ids().iter().cloned());
        let metadata = ProjectVersionMetadata::from(commit.store_commit().as_ref());
        if !source_metadata.contains(&metadata)
            || !crate::native_project::commit_has_project_tree(&commit, mount).await?
        {
            continue;
        }
        let projected = crate::native_project::project_tree(&commit, mount).await?;
        let key = ProjectVersionKey {
            metadata,
            root_tree: projected.tree_ids().clone(),
            conflict_labels: projected.labels().as_merge().clone(),
        };
        versions
            .entry(key)
            .or_default()
            .push(ExistingProjectVersion {
                id,
                parents: commit.parent_ids().to_vec(),
            });
    }
    Ok(versions)
}

fn find_existing_project_version(
    versions: &HashMap<ProjectVersionKey, Vec<ExistingProjectVersion>>,
    source: &backend::Commit,
    mount: &RepoPath,
) -> Result<Option<CommitId>> {
    let Some(candidates) = versions.get(&ProjectVersionKey::from(source)) else {
        return Ok(None);
    };
    let mut matches = candidates
        .iter()
        .filter(|candidate| candidate.parents == source.parents);
    let Some(candidate) = matches.next() else {
        return Ok(None);
    };
    if let Some(other) = matches.next() {
        anyhow::bail!(
            "Project history at {} has multiple content-identical versions {} and {}; cannot \
             recover a lost Git change identity",
            mount.as_internal_file_string(),
            candidate.id.hex(),
            other.id.hex()
        );
    }
    Ok(Some(candidate.id.clone()))
}

/// Explicitly flatten foreign scopes into disconnected aliases of the outer
/// project. Source scope is encoded only here, never guessed by remote lookup.
pub(crate) struct RemoteImport {
    pub source: RemoteNameBuf,
    pub physical: RemoteNameBuf,
    pub connection: ConnectionId,
    pub alias: ScopedRemoteName,
}

pub(crate) fn plan_remotes(view: &View, project: &ProjectId) -> Result<Vec<RemoteImport>> {
    for (key, target) in &view.project_observations {
        ensure!(
            key.remote != REMOTE_NAME_FOR_LOCAL_GIT_REPO || target.iter().all(Option::is_none),
            "Source local Git observation {}@{} cannot own project conversion evidence",
            key.name.as_str(),
            key.remote.as_str()
        );
    }
    for (connection, names) in &view.project_state.remote_names {
        if names.iter().flatten().next().is_some() {
            ensure!(
                view.remote_connections.iter().any(|(remote, owner)| {
                    remote.as_str() != REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str()
                        && owner.as_resolved().and_then(Option::as_ref) == Some(connection)
                }),
                "Source scoped remote has no resolved physical connection mapping"
            );
        }
    }
    let remotes: std::collections::BTreeSet<_> = view
        .remote_views
        .keys()
        .chain(view.remote_connections.keys())
        .chain(view.observed_remote_connections.keys())
        .filter(|remote| remote.as_str() != REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str())
        .collect();
    let mut result = Vec::new();
    for remote in remotes {
        let identity =
            jj_lib::view::remote_identity::resolve(view, remote).map_err(anyhow::Error::msg)?;
        let owner = identity.map(|identity| identity.connection);
        if owner.is_none() && !view.remote_views.contains_key(remote) {
            continue;
        }
        let alias = identity.and_then(|identity| identity.scoped_name);
        // Older recorded states may lack aliases. Their explicit binding
        // scope and verbatim physical name are the only adoption evidence.
        let legacy_project = if alias.is_none() {
            owner
                .map(|connection| {
                    view.project_state
                        .binding_for_connection(connection)
                        .map_err(anyhow::Error::msg)
                })
                .transpose()?
                .flatten()
                .and_then(|(_, binding)| match &binding.target {
                    BindingTarget::Project(project) => Some(project),
                    BindingTarget::RepositoryView => None,
                })
        } else {
            None
        };
        let local: &jj_lib::ref_name::RemoteName =
            alias.map_or(remote.as_ref(), |alias| alias.name.as_ref());
        let source_project = alias.map(|alias| &alias.project).or(legacy_project);
        let name: RemoteNameBuf = if let Some(source_project) = source_project {
            if view.project_state.projects.contains_key(source_project) {
                view.project_state
                    .validate_project(source_project)
                    .map_err(anyhow::Error::msg)?;
            }
            let label = view
                .project_state
                .labels
                .iter()
                .find_map(|(label, target)| {
                    (target.as_resolved().and_then(Option::as_ref) == Some(source_project))
                        .then(|| label.clone())
                })
                .unwrap_or_else(|| source_project.hex());
            format!("{}@{label}", local.as_str()).into()
        } else {
            local.to_owned()
        };
        let connection = ConnectionId::generate();
        result.push(RemoteImport {
            source: remote.clone(),
            physical: format!("jjosh-{}", connection.hex()).into(),
            connection,
            alias: ScopedRemoteName {
                project: project.clone(),
                name,
            },
        });
    }
    Ok(result)
}

pub(crate) fn install_remote_names(view: &mut View, remotes: Vec<RemoteImport>) {
    let mut source_views = std::mem::take(&mut view.remote_views);
    for remote in remotes {
        if let Some(observations) = source_views.remove(&remote.source) {
            view.remote_views
                .insert(remote.physical.clone(), observations);
        }
        view.observed_remote_connections.insert(
            remote.physical,
            Merge::resolved(Some(remote.connection.clone())),
        );
        view.observed_remote_names
            .insert(remote.connection, Merge::resolved(Some(remote.alias)));
    }
}

/// Rewrite captured native history without selecting or publishing a view.
/// The caller owns destination policy and the single publishing transaction.
pub(crate) async fn rewrite_graph(
    source: &NativeSource,
    dest: &mut MutableRepo,
    scope: &str,
    mount: &RepoPath,
    mut ids: HashMap<CommitId, CommitId>,
) -> Result<Imported> {
    let source_backend = source.store.backend();
    let dest_store = dest.store().clone();
    let dest_backend = dest_store.backend();
    // Object deduplication below is by ID, independent of path. That is valid
    // for Git objects, but not necessarily for other native backends.
    for backend in [source_backend, dest_backend] {
        ensure!(
            backend.name() == "git" && backend.commit_id_length() == 20,
            "native import requires Git SHA-1 source and destination backends"
        );
    }
    ensure!(
        !source
            .view
            .remote_views
            .keys()
            .any(|name| name.as_str().contains("jjosh-native-")),
        "Source contains unsupported legacy native correspondence bookmarks"
    );
    source.copy_objects_to(jj_lib::git::get_git_backend(&dest_store)?)?;
    let existing_versions = index_existing_project_versions(source, dest, mount, &ids).await?;

    // A captured source may also retain hidden canonical versions referenced
    // only by private conversion anchors. Rewrite those without exposing heads.
    let mut roots: Vec<_> = source.commits.keys().cloned().collect();
    roots.sort_unstable();
    let mut pending: Vec<_> = roots.into_iter().rev().map(CommitVisit::Read).collect();
    let mut active = HashSet::new();
    let lift = !ids.is_empty();
    ids.insert(
        source.store.root_commit_id().clone(),
        dest_store.root_commit_id().clone(),
    );
    let mut origins = HashMap::new();
    let mut trees = HashMap::new();
    let mut commits = Vec::new();
    let mut grafts = Vec::new();
    let mut stripped_signatures = 0;
    // Enter/exit DFS visits each commit and parent edge once. It is iterative
    // so a long history does not consume the Rust call stack.
    while let Some(visit) = pending.pop() {
        match visit {
            CommitVisit::Read(id) => {
                if ids.contains_key(&id) {
                    continue;
                }
                let commit = source
                    .commits
                    .get(&id)
                    .with_context(|| format!("missing source {scope} native commit {id}"))?;
                if let Some(existing) =
                    crate::native_project::existing_same_project_version(dest, mount, commit)
                        .await?
                {
                    ids.insert(id.clone(), existing.clone());
                    grafts.push((id, existing));
                    continue;
                }
                ensure!(
                    active.insert(id.clone()),
                    "cyclic native parent graph at {id}"
                );
                ensure!(
                    !commit.parents.is_empty(),
                    "non-root native commit {id} has no parents"
                );
                pending.push(CommitVisit::Write(id));
                pending.extend(commit.parents.iter().rev().cloned().map(CommitVisit::Read));
            }
            CommitVisit::Write(old_id) => {
                let mut intended = source.commits[&old_id].clone();
                for parent in &mut intended.parents {
                    *parent = ids[parent].clone();
                }
                intended.predecessors.clear();
                if intended.secure_sig.take().is_some() {
                    stripped_signatures += 1;
                }
                if intended.change_id
                    == jj_lib::git_backend::synthetic_change_id_from_git_commit_id(&old_id)
                    && let Some(existing) =
                        find_existing_project_version(&existing_versions, &intended, mount)?
                {
                    ids.insert(old_id.clone(), existing.clone());
                    grafts.push((old_id.clone(), existing));
                    active.remove(&old_id);
                    continue;
                }
                for tree_id in intended.root_tree.iter() {
                    if !trees.contains_key(tree_id) {
                        let prefixed = if tree_id == source_backend.empty_tree_id() {
                            dest_backend.empty_tree_id().clone()
                        } else {
                            crate::native_project::prefix_tree(dest_store.as_ref(), mount, tree_id)
                                .await?
                        };
                        trees.insert(tree_id.clone(), prefixed);
                    }
                }
                // Mapping each term, rather than merging trees, retains the
                // signed terms, their order, and the separate conflict labels.
                intended.root_tree = intended.root_tree.map(|id| trees[id].clone());
                if lift {
                    let tree =
                        crate::native_project::inherit_other_projects(dest, mount, &intended)
                            .await?;
                    intended.root_tree = tree.tree_ids().clone();
                    intended.conflict_labels = tree.labels().as_merge().clone();
                }
                let mapped = dest_store
                    .write_commit(intended.clone(), None)
                    .await
                    .with_context(|| format!("writing source {scope} native commit {old_id}"))?;
                ensure!(
                    mapped.store_commit().as_ref() == &intended,
                    "destination backend changed native metadata for source {scope} commit \
                     {old_id}; import cannot preserve this commit (possible Git identity \
                     collision)"
                );
                // Some backend normalizations are visible only when reading
                // again, not in the value returned by write_commit's cache.
                let persisted = dest_backend.read_commit(mapped.id()).await?;
                ensure!(
                    persisted == intended,
                    "destination backend did not retain native metadata for source {scope} commit \
                     {old_id}"
                );
                if let Some(other) = origins.insert(mapped.id().clone(), old_id.clone()) {
                    anyhow::bail!(
                        "source {scope} commits {other} and {old_id} collapse to {} after \
                         removing signatures/predecessors; import cannot preserve their distinct \
                         identities",
                        mapped.id()
                    );
                }
                ids.insert(old_id.clone(), mapped.id().clone());
                active.remove(&old_id);
                dest.index_commits(std::slice::from_ref(&mapped)).await?;
                commits.push(mapped);
            }
        }
    }

    Ok(Imported {
        commits,
        stripped_signatures,
        ids,
        grafts,
    })
}

fn map_reference(target: &RefTarget, ids: &HashMap<CommitId, CommitId>) -> RefTarget {
    RefTarget::from_merge(
        target
            .as_merge()
            .map(|term| term.as_ref().map(|id| ids[id].clone())),
    )
}

pub(crate) fn map_outer_view(
    mut view: View,
    scope: &str,
    ids: &HashMap<CommitId, CommitId>,
) -> View {
    // @git observes the source's local Git backend, not a second upstream.
    // Destination @git is generated from the destination's own Git export.
    view.remote_views.remove(REMOTE_NAME_FOR_LOCAL_GIT_REPO);
    view.remote_views
        .retain(|_, remote| !remote.bookmarks.is_empty() || !remote.tags.is_empty());
    view.head_ids = view.head_ids.iter().map(|id| ids[id].clone()).collect();
    for refs in [&mut view.local_bookmarks, &mut view.local_tags] {
        *refs = std::mem::take(refs)
            .into_iter()
            .map(|(name, target)| {
                (
                    crate::ref_names::local_name(scope, name.as_str()).into(),
                    map_reference(&target, ids),
                )
            })
            .collect();
    }
    view.remote_views = view
        .remote_views
        .into_iter()
        .map(|(name, mut remote)| {
            for refs in [&mut remote.bookmarks, &mut remote.tags] {
                *refs = std::mem::take(refs)
                    .into_iter()
                    .map(|(name, mut remote_ref)| {
                        remote_ref.target = map_reference(&remote_ref.target, ids);
                        (
                            crate::ref_names::local_name(scope, name.as_str()).into(),
                            remote_ref,
                        )
                    })
                    .collect();
            }
            (name, remote)
        })
        .collect();
    view.wc_commit_ids.clear();
    // These are observations of the foreign backend, not destination Git refs
    // or real destination workspaces.
    view.git_refs.clear();
    view.git_heads.clear();
    // Do not activate source workspace roles in the destination.
    view.wc_sparse_patterns.clear();
    // This import wraps the source in one new outer project. Foreign identities
    // and observation ownership cannot become active nested projects or local
    // connection authority here.
    view.project_state = Default::default();
    view.remote_connections.clear();
    view.observed_remote_connections.clear();
    view.observed_remote_names.clear();
    view.project_observations.clear();
    view
}
