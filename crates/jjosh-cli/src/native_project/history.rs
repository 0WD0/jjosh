//! Shared subtree-history projection for validation and native export.
//!
//! Boundaries come from immutable raw correspondence, not metadata-policy exclusions.
//! Validation can omit retained values without stopping projection of their ancestry.
//! A project name or another remote's publication state is not correspondence evidence.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, ensure};
use jj_lib::backend::{CommitId, TreeId};
use jj_lib::commit::Commit;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;

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
    Write(Commit),
}

pub(super) fn reverse_anchors(
    known: impl IntoIterator<Item = (CommitId, CommitId)>,
) -> Result<HashMap<CommitId, CommitId>> {
    let known = known.into_iter();
    let mut reverse = HashMap::with_capacity(known.size_hint().0);
    for (raw, canonical) in known {
        match reverse.entry(canonical) {
            Entry::Vacant(entry) => {
                entry.insert(raw);
            }
            Entry::Occupied(entry) => {
                ensure!(
                    entry.get() == &raw,
                    "Project has ambiguous reverse history correspondence at {}",
                    entry.key()
                );
            }
        }
    }
    Ok(reverse)
}

/// Walk each reachable commit once, stopping at explicit boundaries. Only EMIT may write
/// transport commits; validation supplies a read-only callback. The same resolved subtree,
/// ordered-parent deduplication, synthetic-root removal, and empty-commit rule serve both.
pub(super) async fn project<T>(
    repo: &dyn Repo,
    mount: &RepoPath,
    heads: &[CommitId],
    boundary: impl Fn(&CommitId) -> Option<CommitId>,
    mut emit: impl FnMut(&Commit, &MergedTree, Vec<CommitId>) -> Result<(CommitId, Option<T>)>,
) -> Result<History<T>> {
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
                if let Some(representative) = boundary(&id) {
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
                let parents = commit.parent_ids().to_vec();
                pending.push(Visit::Write(commit));
                pending.extend(parents.into_iter().map(Visit::Read));
            }
            Visit::Write(commit) => {
                let tree = super::project_tree(&commit, mount).await?;
                let mut seen = HashSet::new();
                let mut parents: Vec<_> = commit
                    .parent_ids()
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
                    commit.parent_ids().iter().find(|id| {
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
