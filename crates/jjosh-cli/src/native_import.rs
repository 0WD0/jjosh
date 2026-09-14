use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::op_store::RefTarget;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{BindingTarget, ConnectionId, ProjectId, ScopedRemoteName};
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::op_store::View;
use jj_lib::ref_name::RefName;
use jj_lib::repo::MutableRepo;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPath;

use crate::native_source::NativeSource;

pub(crate) struct Imported {
    pub view: View,
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


/// Explicitly flatten foreign scopes into disconnected aliases of the outer
/// project. Source scope is encoded only here, never guessed by remote lookup.
pub(crate) struct RemoteImport {
    pub source: RemoteNameBuf,
    pub physical: RemoteNameBuf,
    pub connection: ConnectionId,
    pub alias: ScopedRemoteName,
}

pub(crate) fn plan_remotes(view: &View, project: &ProjectId) -> Result<Vec<RemoteImport>> {
    for (connection, names) in &view.project_state.remote_names {
        if names.iter().flatten().next().is_some() {
            ensure!(view.remote_connections.values().any(|owner| {
                owner.as_resolved().and_then(Option::as_ref) == Some(connection)
            }), "Source scoped remote has no resolved physical connection mapping");
        }
    }
    let remotes: std::collections::BTreeSet<_> = view.remote_views.keys()
        .chain(view.remote_connections.keys()).collect();
    let mut aliases = std::collections::BTreeMap::new();
    let mut result = Vec::new();
    for remote in remotes {
        let owner = view.remote_connections.get(remote)
            .map(|owner| owner.as_resolved().context("Resolve source remote connection ownership before importing"))
            .transpose()?.and_then(Option::as_ref);
        if owner.is_none() && !view.remote_views.contains_key(remote) {
            continue;
        }
        if remote.as_str() == jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str()
            && let Some(observations) = view.remote_views.get(remote)
            && observations.bookmarks.iter().all(|(name, reference)| view.local_bookmarks.get(name) == Some(&reference.target))
            && observations.tags.iter().all(|(name, reference)| view.local_tags.get(name) == Some(&reference.target))
        {
            continue;
        }
        let alias = owner.and_then(|connection| view.project_state.remote_names.get(connection))
            .map(|alias| alias.as_resolved().context("Resolve source scoped remote names before importing"))
            .transpose()?.and_then(Option::as_ref);
        // Version 1/2 bundles did not have aliases. Their explicit binding
        // scope and verbatim physical name are the only adoption evidence.
        let legacy_project = if alias.is_none() {
            owner.map(|connection| view.project_state.binding_for_connection(connection)
                .map_err(anyhow::Error::msg)).transpose()?.flatten()
                .and_then(|(_, binding)| match &binding.target {
                    BindingTarget::Project(project) => Some(project),
                    BindingTarget::RepositoryView => None,
                })
        } else { None };
        let local: &jj_lib::ref_name::RemoteName = alias.map_or(remote.as_ref(), |alias| alias.name.as_ref());
        let local = if local == jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
            jj_lib::ref_name::RemoteName::new("git@source")
        } else {
            local
        };
        let source_project = alias.map(|alias| &alias.project).or(legacy_project);
        let name: RemoteNameBuf = if let Some(source_project) = source_project {
            view.project_state.validate_project(source_project).map_err(anyhow::Error::msg)?;
            let label = view.project_state.labels.iter().find_map(|(label, target)| {
                (target.as_resolved().and_then(Option::as_ref) == Some(source_project)).then_some(label)
            }).context("Source scoped remote has no resolved stable project label")?;
            format!("{}@{label}", local.as_str()).into()
        } else {
            local.to_owned()
        };
        if let Some(previous) = aliases.insert(name.clone(), remote.clone()) {
            anyhow::bail!("Imported remote alias {} collides between source remotes {} and {}; rename a source remote before importing",
                name.as_str(), previous.as_str(), remote.as_str());
        }
        let connection = ConnectionId::generate();
        result.push(RemoteImport {
            source: remote.clone(),
            physical: format!("jjosh-{}", connection.hex()).into(),
            connection,
            alias: ScopedRemoteName { project: project.clone(), name },
        });
    }
    Ok(result)
}

pub(crate) fn install_remote_names(view: &mut View, remotes: Vec<RemoteImport>) {
    let mut source_views = std::mem::take(&mut view.remote_views);
    for remote in remotes {
        if let Some(observations) = source_views.remove(&remote.source) {
            view.remote_views.insert(remote.physical.clone(), observations);
        }
        view.remote_connections.insert(remote.physical, Merge::resolved(Some(remote.connection.clone())));
        view.project_state.remote_names.insert(remote.connection, Merge::resolved(Some(remote.alias)));
    }
}

/// Copy a recorded native view without publishing it or selecting a workspace.
/// The caller owns destination validation and the single publishing transaction.
pub(crate) async fn import_source(
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
        "Source contains legacy native correspondence bookmarks; run `jjosh native migrate` in \
         the source and re-export any bundle"
    );
    source.copy_objects_to(jj_lib::git::get_git_backend(&dest_store)?)?;
    let source_view = &source.view;
    for workspace in source_view.wc_commit_ids.keys() {
        let role = format!("workspace/{}", workspace.as_str());
        ensure!(
            !source_view
                .local_bookmarks
                .contains_key(RefName::new(&role)),
            "source {scope} bookmark {role:?} collides with its workspace role; rename the \
             bookmark before importing"
        );
    }

    let source_native_view = jj_lib::view::View::new(source_view.clone(), false);
    let mut roots: Vec<_> = source_native_view
        .all_referenced_commit_ids()
        .cloned()
        .collect();
    roots.sort_unstable();
    roots.dedup();
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
        view: map_view(source_view.clone(), scope, &ids),
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

fn map_view(mut view: View, scope: &str, ids: &HashMap<CommitId, CommitId>) -> View {
    // @git observes the source's local Git backend, not a second upstream.
    // Preserve only observations that carry information absent from local refs.
    if let Some(git) = view
        .remote_views
        .get_mut(jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO)
    {
        git.bookmarks
            .retain(|name, reference| view.local_bookmarks.get(name) != Some(&reference.target));
        git.tags
            .retain(|name, reference| view.local_tags.get(name) != Some(&reference.target));
    }
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
    for (workspace, id) in std::mem::take(&mut view.wc_commit_ids) {
        // Collisions were rejected before any objects were written.
        view.local_bookmarks.insert(
            crate::ref_names::local_name(scope, &format!("workspace/{}", workspace.as_str()))
                .into(),
            RefTarget::normal(ids[&id].clone()),
        );
    }
    // These are observations of the foreign backend, not destination Git refs
    // or real destination workspaces. Source @git refs above remain namespaced.
    view.git_refs.clear();
    view.git_heads.clear();
    // Source workspaces become bookmark roles, not mounted target workspaces.
    view.wc_sparse_patterns.clear();
    // This import wraps the complete source in one new outer project. Foreign
    // identities and observation ownership are preserved by bundles, but cannot
    // become active nested projects or local connection authority here.
    view.project_state = Default::default();
    view.remote_connections.clear();
    view.project_observations.clear();
    view
}
