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

pub(crate) fn tree_from_josh_oid(store: Arc<Store>, tree_oid: gix_hash::ObjectId) -> MergedTree {
    MergedTree::resolved(
        store,
        jj_lib::backend::TreeId::from_bytes(tree_oid.as_bytes()),
    )
}
