//! Read-only adapter from the shared subtree projection to JJ's ordinary push checks.

use std::collections::{HashMap, HashSet};

use futures::stream::LocalBoxStream;
use futures::{StreamExt as _, TryStreamExt as _};
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::{GitPushValidationCommit, GitPushValidationStream};
use jj_lib::backend::CommitId;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use jj_lib::revset::RevsetEvaluationError;

pub(crate) fn commits<'a>(
    repo: &'a dyn Repo,
    mount: &'a RepoPath,
    candidates: LocalBoxStream<'a, Result<CommitId, RevsetEvaluationError>>,
    known: impl Future<Output = Result<HashMap<CommitId, CommitId>, CommandError>> + 'a,
) -> GitPushValidationStream<'a> {
    futures::stream::once(async move {
        // Only project projection needs graph planning. Preserve the original revset order
        // for diagnostics; don't keep a second collection of loaded Commit objects.
        let ids: Vec<_> = candidates.try_collect().await?;
        if ids.is_empty() {
            return Ok(futures::stream::empty().boxed_local());
        }
        let selected: HashSet<_> = ids.iter().collect();
        let known = super::history::reverse_anchors(known.await?).map_err(user_error)?;
        let history = super::history::project(
            repo,
            mount,
            &ids,
            // Policy exclusions aren't projection boundaries: an excluded parent may
            // still collapse into a shared ancestor or the synthetic project root.
            |id| known.get(id).filter(|_| !selected.contains(id)).cloned(),
            |commit, _tree, _parents| {
                Ok((
                    commit.id().clone(),
                    selected.contains(commit.id()).then(|| commit.clone()),
                ))
            },
        )
        .await
        .map_err(user_error)?;
        let mut surviving: HashMap<_, _> = history
            .nodes
            .into_iter()
            .filter_map(|node| {
                node.value.map(|commit| {
                    (
                        commit.id().clone(),
                        GitPushValidationCommit {
                            commit,
                            has_conflict: node.has_conflict,
                        },
                    )
                })
            })
            .collect();
        Ok::<_, CommandError>(
            futures::stream::iter(
                ids.into_iter()
                    .filter_map(move |id| surviving.remove(&id).map(Ok)),
            )
            .boxed_local(),
        )
    })
    .try_flatten()
    .boxed_local()
}
