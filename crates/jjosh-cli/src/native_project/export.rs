//! Whole-project export, shared by all selected refs of one immutable binding.
//!
//! Computation is batch-local. Each result retains its own reachable evidence so that
//! accepting one ref never records the unpublished history of a rejected sibling.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Result, ensure};
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
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
    let heads: Vec<_> = heads.iter().map(|head| head.id().clone()).collect();
    let projected = super::history::project(
        repo,
        mount,
        &heads,
        |id| reverse.get(id).cloned(),
        |commit, tree, parents| {
            let mut contents = commit.store_commit().as_ref().clone();
            contents.parents = parents;
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
            Ok((raw, commit.id().clone()))
        },
    )
    .await?;
    let history: Arc<[ExportNode]> = projected
        .nodes
        .into_iter()
        .map(|node| ExportNode {
            canonical: node.value.filter(|_| !known.contains_key(&node.id)),
            raw: node.id,
            parents: node.parents,
        })
        .collect();
    projected
        .heads
        .into_iter()
        .map(|head| {
            ensure!(
                &history[head].raw != repo.store().root_commit_id(),
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
