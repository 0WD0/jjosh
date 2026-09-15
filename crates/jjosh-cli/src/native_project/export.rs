//! Whole-project export, shared by all selected refs of one immutable binding.
//!
//! Computation is batch-local. Each result retains its own reachable evidence so that
//! accepting one ref never records the unpublished history of a rejected sibling.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Result, ensure};
use jj_lib::backend::{CommitId, TreeId};
use jj_lib::commit::Commit;
use jj_lib::merge::Merge;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;

/// A root in a shared, parent-first arena. Indices avoid recursive destruction of deep DAGs.
#[derive(Clone)]
pub(crate) struct ExportedProject {
    head: usize,
    history: Arc<[ExportNode]>,
}

struct ExportNode {
    raw: CommitId,
    // None at previously witnessed boundaries; these are not new publication evidence.
    canonical: Option<CommitId>,
    parents: Vec<usize>,
}

impl ExportedProject {
    pub(crate) fn id(&self) -> &CommitId {
        &self.history[self.head].raw
    }

    /// Walk only this result's new correspondence, not all other roots prepared in the batch.
    pub(crate) fn publications(&self) -> impl Iterator<Item = (&CommitId, &CommitId)> {
        let mut pending = vec![(self.head, false)];
        let mut visited = HashSet::new();
        let mut emitted = HashSet::new();
        std::iter::from_fn(move || {
            while let Some((index, ready)) = pending.pop() {
                let node = &self.history[index];
                if ready {
                    if let Some(canonical) = &node.canonical
                        && emitted.insert(&node.raw)
                    {
                        return Some((&node.raw, canonical));
                    }
                } else if visited.insert(index) {
                    pending.push((index, true));
                    pending.extend(node.parents.iter().map(|&parent| (parent, false)));
                }
            }
            None
        })
    }
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

/// Project several heads using one ancestry walk and one immutable correspondence snapshot.
/// Results follow HEADS order, including duplicates; empty history cannot be published.
pub(crate) async fn export_projects(
    repo: &dyn Repo,
    mount: &RepoPath,
    heads: &[Commit],
    known: &HashMap<CommitId, CommitId>,
) -> Result<Vec<ExportedProject>> {
    for head in heads {
        ensure!(
            !super::project_tree(head, mount).await?.has_conflict(),
            "Project mount {} has unresolved conflicts at the publication revision",
            mount.as_internal_file_string()
        );
    }
    let mut reverse = HashMap::new();
    for (raw, canonical) in known {
        if let Some(previous) = reverse.insert(canonical.clone(), raw.clone()) {
            ensure!(
                previous == *raw,
                "Project has ambiguous reverse history correspondence at {canonical}"
            );
        }
    }
    let backend = jj_lib::git::get_git_backend(repo.store())?;
    let root = repo.store().root_commit_id();
    let mut nodes = vec![ExportNode {
        raw: root.clone(),
        canonical: None,
        parents: Vec::new(),
    }];
    let mut mapped = HashMap::from([(
        root.clone(),
        Projected {
            node: 0,
            tree: Merge::resolved(repo.store().empty_tree_id().clone()),
        },
    )]);
    let mut pending: Vec<_> = heads
        .iter()
        .rev()
        .map(|head| Visit::Read(head.id().clone()))
        .collect();
    while let Some(visit) = pending.pop() {
        match visit {
            Visit::Read(id) => {
                if mapped.contains_key(&id) {
                    continue;
                }
                let commit = repo.store().get_commit_async(&id).await?;
                if let Some(raw) = reverse.get(&id) {
                    let tree = super::project_tree(&commit, mount).await?;
                    mapped.insert(
                        id,
                        Projected {
                            node: nodes.len(),
                            tree: tree.tree_ids().clone(),
                        },
                    );
                    nodes.push(ExportNode {
                        raw: raw.clone(),
                        canonical: None,
                        parents: Vec::new(),
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
                    .filter(|&index| seen.insert(&nodes[index].raw))
                    .collect();
                if parents.len() > 1 {
                    parents.retain(|&index| nodes[index].raw != *root);
                }
                let same_parent = if let [parent] = parents.as_slice() {
                    commit.parent_ids().iter().find(|id| {
                        nodes[mapped[*id].node].raw == nodes[*parent].raw
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
                let mut contents = commit.store_commit().as_ref().clone();
                contents.parents = parents
                    .iter()
                    .map(|&index| nodes[index].raw.clone())
                    .collect();
                contents.root_tree = tree.tree_ids().clone();
                contents.conflict_labels = tree.labels().as_merge().clone();
                contents.predecessors.clear();
                contents.secure_sig = None;
                let (raw, exported) = backend.write_commit_for_export(contents.clone())?;
                ensure!(
                    exported == contents,
                    "Backend changed metadata while projecting {}",
                    commit.id()
                );
                mapped.insert(
                    commit.id().clone(),
                    Projected {
                        node: nodes.len(),
                        tree: tree.tree_ids().clone(),
                    },
                );
                nodes.push(ExportNode {
                    canonical: (!known.contains_key(&raw)).then(|| commit.id().clone()),
                    raw,
                    parents,
                });
            }
        }
    }
    let history: Arc<[ExportNode]> = nodes.into();
    heads
        .iter()
        .map(|head| {
            let head = mapped[head.id()].node;
            ensure!(
                history[head].raw != *root,
                "Selected history contains no project content to publish"
            );
            Ok(ExportedProject {
                head,
                history: history.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, parents: Vec<usize>, witnessed: bool) -> ExportNode {
        ExportNode {
            raw: CommitId::from_bytes(&id.to_be_bytes()),
            canonical: (!witnessed).then(|| CommitId::from_bytes(&(id + 100_000).to_be_bytes())),
            parents,
        }
    }

    #[test]
    fn results_share_storage_but_not_sibling_publication_evidence() {
        let history: Arc<[ExportNode]> = vec![
            node(0, vec![], true),
            node(1, vec![0], false),
            node(2, vec![1], false),
            node(3, vec![1], false),
            node(4, vec![2, 3], false),
        ]
        .into();
        let exported = |head| ExportedProject {
            head,
            history: history.clone(),
        };
        let ids = |head| {
            exported(head)
                .publications()
                .map(|(raw, _)| raw.clone())
                .collect::<HashSet<_>>()
        };
        assert_eq!(
            ids(2),
            HashSet::from([history[1].raw.clone(), history[2].raw.clone()])
        );
        assert_eq!(
            ids(3),
            HashSet::from([history[1].raw.clone(), history[3].raw.clone()])
        );
        assert_eq!(exported(4).publications().count(), 4);
        assert_eq!(exported(0).publications().count(), 0);
    }

    #[test]
    fn deep_histories_are_iterated_and_dropped_without_recursion() {
        let mut history = vec![node(0, vec![], true)];
        for i in 1..50_000 {
            history.push(node(i, vec![i as usize - 1], false));
        }
        let exported = ExportedProject {
            head: history.len() - 1,
            history: history.into(),
        };
        assert_eq!(exported.publications().count(), 49_999);
        drop(exported);
    }
}
