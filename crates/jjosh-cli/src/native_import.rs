use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use jj_lib::backend::Backend;
use jj_lib::backend::CommitId;
use jj_lib::backend::FileId;
use jj_lib::backend::SymlinkId;
use jj_lib::backend::Tree;
use jj_lib::backend::TreeId;
use jj_lib::backend::TreeValue;
use jj_lib::backend::{self};
use jj_lib::commit::Commit;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::op_store::View;
use jj_lib::ref_name::RefName;
use jj_lib::repo::ReadonlyRepo;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPath;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::repo_path::RepoPathComponentBuf;

use crate::native_bundle::Bundle;

pub(crate) struct Imported {
    pub view: View,
    /// The complete parent closure, parent-first, not just visible commits.
    pub commits: Vec<Commit>,
    pub stripped_signatures: usize,
}

enum CommitVisit {
    Read(CommitId),
    Write(CommitId, backend::Commit),
}

/// Copy a recorded native view without publishing it or selecting a workspace.
/// The caller owns destination validation and the single publishing transaction.
pub(crate) async fn import_bundle(
    source: &Bundle,
    dest: &Arc<ReadonlyRepo>,
    scope: &str,
) -> Result<Imported> {
    let component = RepoPathComponentBuf::new(scope.to_owned())?;
    let source_backend = source.repo.store().backend();
    let dest_backend = dest.store().backend();
    // Object deduplication below is by ID, independent of path. That is valid
    // for Git objects, but not necessarily for other native backends.
    for backend in [source_backend, dest_backend] {
        ensure!(
            backend.name() == "git" && backend.commit_id_length() == 20,
            "native import requires Git SHA-1 source and destination backends"
        );
    }
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
    let mut ids = HashMap::from([(
        source.repo.store().root_commit_id().clone(),
        dest.store().root_commit_id().clone(),
    )]);
    let mut origins = HashMap::new();
    let mut objects = ObjectImporter {
        source: source_backend,
        dest: dest_backend,
        component,
        trees: HashSet::new(),
        files: HashSet::new(),
        symlinks: HashSet::new(),
        prefixed_trees: HashMap::new(),
    };
    let mut commits = Vec::new();
    let mut stripped_signatures = 0;
    // Enter/exit DFS visits each commit and parent edge once. It is iterative
    // so a long history does not consume the Rust call stack.
    while let Some(visit) = pending.pop() {
        match visit {
            CommitVisit::Read(id) => {
                if ids.contains_key(&id) {
                    continue;
                }
                ensure!(
                    active.insert(id.clone()),
                    "cyclic native parent graph at {id}"
                );
                let commit = source
                    .commits
                    .get(&id)
                    .with_context(|| format!("missing source {scope} native commit {id}"))?
                    .clone();
                ensure!(
                    !commit.parents.is_empty(),
                    "non-root native commit {id} has no parents"
                );
                let parents = commit.parents.clone();
                pending.push(CommitVisit::Write(id, commit));
                pending.extend(parents.into_iter().rev().map(CommitVisit::Read));
            }
            CommitVisit::Write(old_id, mut intended) => {
                for parent in &mut intended.parents {
                    *parent = ids[parent].clone();
                }
                intended.predecessors.clear();
                if intended.secure_sig.take().is_some() {
                    stripped_signatures += 1;
                }
                for tree_id in intended.root_tree.iter() {
                    objects.prefix_tree(tree_id).await.with_context(|| {
                        format!("copying source {scope} tree for commit {old_id}")
                    })?;
                }
                // Mapping each term, rather than merging trees, retains the
                // signed terms, their order, and the separate conflict labels.
                intended.root_tree = intended
                    .root_tree
                    .map(|id| objects.prefixed_trees[id].clone());
                let mapped = dest
                    .store()
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
                commits.push(mapped);
            }
        }
    }

    Ok(Imported {
        view: map_view(source_view.clone(), scope, &ids),
        commits,
        stripped_signatures,
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
    view.head_ids = view.head_ids.iter().map(|id| ids[id].clone()).collect();
    for refs in [&mut view.local_bookmarks, &mut view.local_tags] {
        *refs = std::mem::take(refs)
            .into_iter()
            .map(|(name, target)| {
                (
                    format!("{scope}/{}", name.as_str()).into(),
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
                        (format!("{scope}/{}", name.as_str()).into(), remote_ref)
                    })
                    .collect();
            }
            (format!("{scope}-{}", name.as_str()).into(), remote)
        })
        .collect();
    for (workspace, id) in std::mem::take(&mut view.wc_commit_ids) {
        // Collisions were rejected before any objects were written.
        view.local_bookmarks.insert(
            format!("{scope}/workspace/{}", workspace.as_str()).into(),
            RefTarget::normal(ids[&id].clone()),
        );
    }
    // These are observations of the foreign backend, not destination Git refs
    // or real destination workspaces. Source @git refs above remain namespaced.
    view.git_refs.clear();
    view.git_heads.clear();
    // Source workspaces become bookmark roles, not mounted target workspaces.
    view.wc_sparse_patterns.clear();
    view
}

struct ObjectImporter<'a> {
    source: &'a dyn Backend,
    dest: &'a dyn Backend,
    component: RepoPathComponentBuf,
    trees: HashSet<TreeId>,
    files: HashSet<FileId>,
    symlinks: HashSet<SymlinkId>,
    prefixed_trees: HashMap<TreeId, TreeId>,
}

enum TreeVisit {
    Read(RepoPathBuf, RepoPathBuf, TreeId),
    Write(RepoPathBuf, TreeId, Tree),
}

impl ObjectImporter<'_> {
    async fn prefix_tree(&mut self, id: &TreeId) -> Result<()> {
        if self.prefixed_trees.contains_key(id) {
            return Ok(());
        }
        self.copy_tree(id).await?;
        let prefixed = if id == self.source.empty_tree_id() {
            // An empty source commit must not acquire an empty scope directory.
            self.dest.empty_tree_id().clone()
        } else {
            let tree = Tree::from_sorted_entries(vec![(
                self.component.clone(),
                TreeValue::Tree(id.clone()),
            )]);
            self.dest.write_tree(RepoPath::root(), &tree).await?
        };
        self.prefixed_trees.insert(id.clone(), prefixed);
        Ok(())
    }

    async fn copy_tree(&mut self, id: &TreeId) -> Result<()> {
        let mut pending = vec![TreeVisit::Read(
            RepoPathBuf::root(),
            RepoPath::root().join(&self.component),
            id.clone(),
        )];
        let mut active = HashSet::new();
        while let Some(visit) = pending.pop() {
            match visit {
                TreeVisit::Read(source_path, dest_path, id) => {
                    if self.trees.contains(&id) {
                        continue;
                    }
                    ensure!(
                        active.insert(id.clone()),
                        "cyclic native tree at {source_path:?} ({id})"
                    );
                    let tree = self
                        .source
                        .read_tree(&source_path, &id)
                        .await
                        .with_context(|| format!("reading tree {id} at {source_path:?}"))?;
                    let mut children = Vec::new();
                    for entry in tree.entries() {
                        let source_entry = source_path.join(entry.name());
                        let dest_entry = dest_path.join(entry.name());
                        match entry.value() {
                            TreeValue::Tree(child) => children.push(TreeVisit::Read(
                                source_entry,
                                dest_entry,
                                child.clone(),
                            )),
                            TreeValue::File { id, copy_id, .. } => {
                                ensure!(
                                    copy_id.as_bytes().is_empty(),
                                    "tracked copy metadata at {source_entry:?} is unsupported by \
                                     the destination Git backend"
                                );
                                if !self.files.contains(id) {
                                    let mut contents = self
                                        .source
                                        .read_file(&source_entry, id)
                                        .await
                                        .with_context(|| {
                                            format!("reading file {id} at {source_entry:?}")
                                        })?;
                                    let copied =
                                        self.dest.write_file(&dest_entry, &mut contents).await?;
                                    ensure!(
                                        copied == *id,
                                        "Git file identity changed at {source_entry:?}"
                                    );
                                    self.files.insert(id.clone());
                                }
                            }
                            TreeValue::Symlink(id) => {
                                if !self.symlinks.contains(id) {
                                    let target = self
                                        .source
                                        .read_symlink(&source_entry, id)
                                        .await
                                        .with_context(|| {
                                            format!("reading symlink {id} at {source_entry:?}")
                                        })?;
                                    let copied =
                                        self.dest.write_symlink(&dest_entry, &target).await?;
                                    ensure!(
                                        copied == *id,
                                        "Git symlink identity changed at {source_entry:?}"
                                    );
                                    self.symlinks.insert(id.clone());
                                }
                            }
                            // A gitlink refers to a commit in a different Git
                            // repository, not this native commit parent graph.
                            TreeValue::GitSubmodule(_) => {}
                        }
                    }
                    pending.push(TreeVisit::Write(dest_path, id, tree));
                    pending.extend(children.into_iter().rev());
                }
                TreeVisit::Write(path, id, tree) => {
                    let copied = self.dest.write_tree(&path, &tree).await?;
                    ensure!(copied == id, "Git tree identity changed at {path:?}");
                    active.remove(&id);
                    self.trees.insert(id);
                }
            }
        }
        Ok(())
    }
}
