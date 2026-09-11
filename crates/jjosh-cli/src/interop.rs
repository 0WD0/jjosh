use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use jj_cli::cli_util::WorkspaceCommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_lib::commit::Commit;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;
use jj_lib::store::Store;

pub(crate) fn sha1_git_repo_path(
    workspace_command: &WorkspaceCommandHelper,
) -> Result<PathBuf, CommandError> {
    let git_backend = jj_lib::git::get_git_backend(workspace_command.repo().store())?;
    if git_backend.git_repo().object_hash().len_in_bytes() != 20 {
        return Err(user_error(
            "jjosh currently supports only SHA-1 Git repositories",
        ));
    }
    Ok(git_backend.git_repo_path().to_owned())
}

pub(crate) fn open_josh_transaction(
    git_repo_path: &std::path::Path,
    ephemeral: bool,
) -> Result<josh_core::cache::Transaction, CommandError> {
    let mut context = josh_core::cache::TransactionContext::new(
        git_repo_path,
        Arc::new(josh_core::cache::CacheStack::new()),
    );
    if ephemeral {
        context = context.ephemeral();
    } else {
        context = context.with_mem_odb_limit(josh_cli::MAX_MEM_PACK_SIZE);
    }
    context
        .open()
        .map_err(|err| user_error_with_message("Failed to open the Git repository for Josh", err))
}

pub(crate) fn commit_as_josh_oid(commit: &Commit) -> Result<gix_hash::ObjectId, CommandError> {
    gix_hash::ObjectId::try_from(commit.id().as_bytes()).map_err(|err| {
        user_error_with_message("Could not convert the jj commit ID to a Git object ID", err)
    })
}

pub(crate) fn commit_id_from_josh_oid(oid: gix_hash::ObjectId) -> jj_lib::backend::CommitId {
    jj_lib::backend::CommitId::from_bytes(oid.as_bytes())
}

pub(crate) fn tree_from_josh_oid(store: Arc<Store>, tree_oid: gix_hash::ObjectId) -> MergedTree {
    MergedTree::resolved(
        store,
        jj_lib::backend::TreeId::from_bytes(tree_oid.as_bytes()),
    )
}

pub(crate) fn check_git_state(workspace: &WorkspaceCommandHelper) -> Result<(), CommandError> {
    let backend = jj_lib::git::get_git_backend(workspace.repo().store())?;
    let repo = backend.git_repo();
    if jj_lib::git::has_pending_imports(workspace.repo().view(), &repo)
        .map_err(|err| user_error_with_message("Failed to inspect pending Git imports", err))?
    {
        return Err(user_error(
            "Git refs have unimported changes; run jjosh git import separately before continuing",
        ));
    }
    if workspace.working_copy_shared_with_git() {
        let repo = backend
            .open_git_repo_at_workdir(workspace.workspace_root())
            .map_err(|err| {
                user_error_with_message("Failed to inspect the colocated Git repository", err)
            })?;
        if repo.state().is_some() {
            return Err(user_error(
                "Finish or abort the ongoing Git operation before continuing",
            ));
        }
        let mut head = repo
            .head()
            .map_err(|err| user_error_with_message("Failed to inspect Git HEAD", err))?;
        let actual = head
            .try_peel_to_id()
            .map_err(|err| user_error_with_message("Failed to resolve Git HEAD", err))?
            .map(|id| jj_lib::backend::CommitId::from_bytes(id.as_bytes()));
        let recorded = workspace.repo().view().git_head(workspace.workspace_name());
        if recorded.as_normal() != actual.as_ref() || recorded.has_conflict() {
            return Err(user_error(
                "Git HEAD has unimported changes; reconcile the Git checkout with jj separately before continuing",
            ));
        }
    }
    Ok(())
}

/// Josh operates on Git histories. A native conflict's Git representation is
/// transport data, not a resolved tree that can safely be filtered or pushed.
pub(crate) async fn check_projectable_history(
    workspace: &WorkspaceCommandHelper,
    commit: &Commit,
) -> Result<(), CommandError> {
    let store = workspace.repo().store();
    if commit.id() == store.root_commit_id() {
        return Err(user_error("The root commit cannot be projected"));
    }
    let mut pending = vec![commit.id().clone()];
    let mut visited = HashSet::new();
    while let Some(id) = pending.pop() {
        if id == *store.root_commit_id() || !visited.insert(id.clone()) {
            continue;
        }
        let ancestor = store.get_commit_async(&id).await?;
        if ancestor.has_conflict() {
            return Err(user_error(format!(
                "Revision {} contains a native conflict; resolve it before projecting this history. Native bundles can transport unresolved conflicts without projecting them.",
                id.hex()
            )));
        }
        pending.extend(ancestor.parent_ids().iter().cloned());
    }
    Ok(())
}

/// Inspect received Git objects before Josh writes any projected refs. Native
/// metadata must not be lazily synthesized merely to perform this check.
pub(crate) fn check_projectable_remote(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
) -> Result<(), CommandError> {
    let mut pending = Vec::new();
    transaction
        .for_each_ref_prefixed(&format!("refs/josh/remotes/{remote}/"), |_, id| {
            pending.push(id);
            Ok(())
        })
        .map_err(|err| user_error_with_message("Failed to inspect fetched source refs", err))?;
    let mut visited = HashSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let commit = josh_core::objects::CommitData::read(transaction.odb(), id)
            .map_err(|err| user_error_with_message("Failed to read fetched source history", err))?;
        let parsed = commit
            .parsed()
            .map_err(|err| user_error_with_message("Failed to parse fetched source commit", err))?;
        if parsed.extra_headers().find("jj:trees").is_some() {
            return Err(user_error(format!(
                "Source revision {id} carries native conflict trees; resolve them before projecting this history, or use a native bundle to transport them unchanged"
            )));
        }
        pending.extend(commit.parent_ids());
    }
    Ok(())
}
