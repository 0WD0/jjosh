//! Read-only adapter from the shared subtree projection to JJ's ordinary push checks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use futures::stream::LocalBoxStream;
use futures::{StreamExt as _, TryStreamExt as _};
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::{GitPushValidationCommit, GitPushValidationStream};
use jj_lib::backend::CommitId;
use jj_lib::project::BindingId;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use jj_lib::revset::RevsetEvaluationError;

pub(crate) fn commits<'a>(
    repo: &'a dyn Repo,
    mount: &'a RepoPath,
    candidates: LocalBoxStream<'a, Result<CommitId, RevsetEvaluationError>>,
    git_path: &'a Path,
    self_binding: &'a BindingId,
    other_whole_bindings: Vec<BindingId>,
) -> GitPushValidationStream<'a> {
    futures::stream::once(async move {
        // Only project projection needs graph planning. Preserve the original revset order
        // for diagnostics; don't keep a second collection of loaded Commit objects.
        let ids: Vec<_> = candidates.try_collect().await?;
        if ids.is_empty() {
            return Ok(futures::stream::empty().boxed_local());
        }
        let selected: HashSet<_> = ids.iter().collect();
        let transaction = crate::interop::open_josh_transaction(git_path, true)?;
        let mut known = super::Anchors::new(repo, &transaction);
        known
            .load_binding(self_binding, false)
            .map_err(user_error)?;
        for id in &other_whole_bindings {
            if super::is_offline_binding(&transaction, id).map_err(user_error)? {
                known.load_binding(id, true).map_err(user_error)?;
            }
        }
        let history = super::history::project(
            repo,
            mount,
            &ids,
            &mut known,
            // Policy exclusions aren't projection boundaries: an excluded parent may
            // still collapse into a shared ancestor or the synthetic project root.
            |id| !selected.contains(id),
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
