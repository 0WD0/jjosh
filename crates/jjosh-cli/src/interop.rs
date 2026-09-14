use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use jj_cli::cli_util::WorkspaceCommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_lib::commit::Commit;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;

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

/// Josh operates on Git histories. A native conflict's Git representation is
/// transport data, not a resolved tree that can safely be filtered or pushed.
pub(crate) async fn check_projectable_repo_history(
    repo: &dyn jj_lib::repo::Repo,
    commit: &Commit,
) -> Result<(), CommandError> {
    let store = repo.store();
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
                "Revision {} contains a native conflict; resolve it before projecting this history.",
                id.hex()
            )));
        }
        pending.extend(ancestor.parent_ids().iter().cloned());
    }
    Ok(())
}

/// Validate explicitly received source objects without consulting ref namespaces.
pub(crate) fn check_raw_projectable_history(
    transaction: &josh_core::cache::Transaction,
    roots: impl IntoIterator<Item = gix_hash::ObjectId>,
) -> Result<(), CommandError> {
    let mut pending: Vec<_> = roots.into_iter().collect();
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
                "Source revision {id} carries native conflict trees; resolve them before projecting this history"
            )));
        }
        pending.extend(commit.parent_ids());
    }
    Ok(())
}
