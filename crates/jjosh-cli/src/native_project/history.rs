//! Shared subtree-history projection for validation and native export.
//!
//! Boundaries come from immutable raw correspondence, not metadata-policy exclusions.
//! Validation can omit retained values without stopping projection of their ancestry.
//! A project name or another remote's publication state is not correspondence evidence.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use futures::TryStreamExt as _;
use jj_lib::backend::{CommitId, TreeId};
use jj_lib::commit::Commit;
use jj_lib::fileset::{FilePattern, FilesetExpression};
use jj_lib::index::Index;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use jj_lib::revset::{RevsetExpression, RevsetFilterPredicate};

pub(super) struct Node<T> {
    pub id: CommitId,
    /// None for the synthetic root, witnessed boundaries, and caller-excluded values.
    pub value: Option<T>,
    pub parents: Vec<usize>,
    pub has_conflict: bool,
}

pub(super) struct History<T> {
    pub nodes: Vec<Node<T>>,
    /// Indices in input order, including duplicate heads and heads collapsed into parents.
    pub heads: Vec<usize>,
}

#[derive(Clone)]
struct Projected {
    node: usize,
    tree: Merge<TreeId>,
}

enum Visit {
    Read(CommitId),
    Write(Commit, Vec<CommitId>),
}

/// Precomputed subtree reachability scope used to bypass large non-project side branches.
struct PruneScope<'a> {
    index: &'a dyn Index,
    boundaries: Vec<CommitId>,
    touched_descendants: HashSet<CommitId>,
    shortcuts: HashMap<CommitId, Vec<CommitId>>,
}

impl<'a> PruneScope<'a> {
    async fn new(
        repo: &'a dyn Repo,
        mount: &RepoPath,
        heads: &[CommitId],
        known: &super::Anchors<'_>,
        allow_boundary: &impl Fn(&CommitId) -> bool,
    ) -> Result<Option<Self>> {
        let index = repo.index();
        let root = repo.store().root_commit_id();
        let mut indexed_heads = Vec::new();
        for id in heads {
            if id == root {
                continue;
            }
            if !index.has_id(id).await? {
                return Ok(None);
            }
            indexed_heads.push(id);
        }
        if indexed_heads.is_empty() {
            return Ok(None);
        }
        let top_heads = index.heads(&mut indexed_heads.into_iter()).await?;
        let mut boundaries = Vec::new();
        let mut stop_boundaries = Vec::new();
        let mut boundary_set = HashSet::new();
        for id in known.canonical_boundary_tips() {
            if id == root || !boundary_set.insert(id.clone()) || !index.has_id(id).await? {
                continue;
            }
            if allow_boundary(id) {
                stop_boundaries.push(id.clone());
            }
            boundaries.push(id.clone());
        }
        if boundaries.len() > 1 {
            boundaries = index.heads(&mut boundaries.iter()).await?;
        }
        if stop_boundaries.len() > 1 {
            stop_boundaries = index.heads(&mut stop_boundaries.iter()).await?;
        }
        let heads_expr = RevsetExpression::commits(top_heads.clone());
        let scope_expr = if stop_boundaries.is_empty() {
            heads_expr.ancestors()
        } else {
            RevsetExpression::commits(stop_boundaries).range(&heads_expr)
        };
        let files_expr = RevsetExpression::filter(RevsetFilterPredicate::File(
            FilesetExpression::pattern(FilePattern::PrefixPath(mount.to_owned())),
        ));
        let touched_expr = scope_expr
            .intersection(&files_expr)
            .dag_range_to(&heads_expr);
        let revset = touched_expr.evaluate(repo)?;
        let touched_descendants: HashSet<CommitId> = revset.stream().try_collect().await?;
        let mut exits = Vec::new();
        let mut seen_exits = HashSet::new();
        for id in &touched_descendants {
            let commit = repo.store().get_commit_async(id).await?;
            for parent in commit.parent_ids() {
                if parent != root
                    && !touched_descendants.contains(parent)
                    && !boundary_set.contains(parent)
                    && seen_exits.insert(parent.clone())
                    && index.has_id(parent).await?
                {
                    exits.push(parent.clone());
                }
            }
        }
        for id in &top_heads {
            if id != root
                && !touched_descendants.contains(id)
                && !boundary_set.contains(id)
                && seen_exits.insert(id.clone())
                && index.has_id(id).await?
            {
                exits.push(id.clone());
            }
        }
        let mut empty_exits = Vec::new();
        let mut nonempty_exits = Vec::new();
        for id in exits {
            let commit = repo.store().get_commit_async(&id).await?;
            if super::commit_has_project_tree(&commit, mount).await? {
                nonempty_exits.push(id);
            } else {
                empty_exits.push(id);
            }
        }
        let mut shortcuts = HashMap::new();
        let mut work = Vec::new();
        if !empty_exits.is_empty() {
            work.push(empty_exits);
        }
        if !nonempty_exits.is_empty() {
            work.push(nonempty_exits);
        }
        while let Some(mut chunk) = work.pop() {
            let no_mount_history = RevsetExpression::commits(chunk.clone())
                .ancestors()
                .intersection(&files_expr)
                .evaluate(repo)?
                .is_empty()
                .await?;
            if no_mount_history {
                for id in chunk {
                    shortcuts.insert(id, vec![root.clone()]);
                }
            } else if chunk.len() == 1 {
                if !boundaries.is_empty() {
                    let id = chunk.pop().unwrap();
                    let ca = index.common_ancestors(&[id.clone()], &boundaries).await?;
                    if !ca.contains(&id) && ca.len() == 1 {
                        shortcuts.insert(id, ca);
                    }
                }
            } else {
                let mid = chunk.len() / 2;
                let right = chunk.split_off(mid);
                work.push(right);
                work.push(chunk);
            }
        }
        Ok(Some(Self {
            index,
            boundaries,
            touched_descendants,
            shortcuts,
        }))
    }

    async fn effective_parents(&self, commit: &Commit, root: &CommitId) -> Result<Vec<CommitId>> {
        let id = commit.id();
        if let Some(parents) = self.shortcuts.get(id) {
            return Ok(parents.clone());
        }
        if self.touched_descendants.contains(id) || !self.index.has_id(id).await? {
            return Ok(commit.parent_ids().to_vec());
        }
        let common = if self.boundaries.is_empty() {
            Vec::new()
        } else {
            self.index
                .common_ancestors(&[id.clone()], &self.boundaries)
                .await?
        };
        if common.contains(id) || common.len() > 1 {
            return Ok(commit.parent_ids().to_vec());
        }
        if common.is_empty() {
            Ok(vec![root.clone()])
        } else {
            Ok(common)
        }
    }
}

/// Walk each reachable commit once, stopping at explicit boundaries. Only EMIT may write
/// transport commits; validation supplies a read-only callback. The same resolved subtree,
/// ordered-parent deduplication, synthetic-root removal, and empty-commit rule serve both.
pub(super) async fn project<T>(
    repo: &dyn Repo,
    mount: &RepoPath,
    heads: &[CommitId],
    known: &mut super::Anchors<'_>,
    allow_boundary: impl Fn(&CommitId) -> bool,
    mut emit: impl FnMut(&Commit, &MergedTree, Vec<CommitId>) -> Result<(CommitId, Option<T>)>,
) -> Result<History<T>> {
    let prune = PruneScope::new(repo, mount, heads, known, &allow_boundary).await?;
    let root = repo.store().root_commit_id();
    let mut nodes = vec![Node {
        id: root.clone(),
        value: None,
        parents: Vec::new(),
        has_conflict: false,
    }];
    let mut mapped = HashMap::from([(
        root.clone(),
        Projected {
            node: 0,
            tree: Merge::resolved(repo.store().empty_tree_id().clone()),
        },
    )]);
    let mut pending: Vec<_> = heads.iter().rev().cloned().map(Visit::Read).collect();
    while let Some(visit) = pending.pop() {
        match visit {
            Visit::Read(id) => {
                if mapped.contains_key(&id) {
                    continue;
                }
                let commit = repo.store().get_commit_async(&id).await?;
                if allow_boundary(&id)
                    && let Some(representative) = known.resolve_canonical(&id).await?
                {
                    let tree = super::project_tree(&commit, mount).await?;
                    mapped.insert(
                        id,
                        Projected {
                            node: nodes.len(),
                            tree: tree.tree_ids().clone(),
                        },
                    );
                    nodes.push(Node {
                        id: representative,
                        value: None,
                        parents: Vec::new(),
                        has_conflict: tree.has_conflict(),
                    });
                    continue;
                }
                let parents = match &prune {
                    Some(prune) => prune.effective_parents(&commit, root).await?,
                    None => commit.parent_ids().to_vec(),
                };
                pending.push(Visit::Write(commit, parents.clone()));
                pending.extend(parents.into_iter().map(Visit::Read));
            }
            Visit::Write(commit, effective_parents) => {
                let tree = super::project_tree(&commit, mount).await?;
                let mut seen = HashSet::new();
                let mut parents: Vec<_> = effective_parents
                    .iter()
                    .map(|id| mapped[id].node)
                    .filter(|&index| seen.insert(&nodes[index].id))
                    .collect();
                // A branch containing only out-of-scope changes projects to root(). It is
                // neutral, not a distinct merge parent requiring a description or identity.
                if parents.len() > 1 {
                    parents.retain(|&index| nodes[index].id != *root);
                }
                let same_parent = if let [parent] = parents.as_slice() {
                    effective_parents.iter().find(|id| {
                        nodes[mapped[*id].node].id == nodes[*parent].id
                            && mapped[*id].tree == *tree.tree_ids()
                    })
                } else {
                    None
                };
                if let Some(parent) = same_parent {
                    mapped.insert(commit.id().clone(), mapped[parent].clone());
                    continue;
                }
                if parents.is_empty() {
                    parents.push(0);
                }
                let parent_ids: Vec<_> = parents.iter().map(|&i| nodes[i].id.clone()).collect();
                let (id, value) = emit(&commit, &tree, parent_ids)?;
                mapped.insert(
                    commit.id().clone(),
                    Projected {
                        node: nodes.len(),
                        tree: tree.tree_ids().clone(),
                    },
                );
                nodes.push(Node {
                    id,
                    value,
                    parents,
                    has_conflict: tree.has_conflict(),
                });
            }
        }
    }
    Ok(History {
        heads: heads.iter().map(|id| mapped[id].node).collect(),
        nodes,
    })
}
