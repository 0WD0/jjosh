// Copyright 2026 The Jujutsu Authors
// SPDX-License-Identifier: Apache-2.0

use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use jj_lib::git;
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;

use crate::cli_util::CommandHelper;
use crate::command_error::{CommandError, user_error};
use crate::ui::Ui;

/// Forget a remote's cached observations and tracking, without removing its connection.
///
/// Clears restored ownership and conversion evidence for detached aliases, allowing
/// explicit reuse after a connection was deleted. Active scoped connection names,
/// local bookmarks, tags, projects, bindings, source objects and endpoint leases remain.
/// No network request is made. The alias need not have a configured connection.
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteForgetObservationsArgs {
    remote: RemoteNameBuf,

    /// Resolve the remote within this project
    #[arg(long)]
    project: Option<String>,
}

pub async fn cmd_git_remote_forget_observations(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteForgetObservationsArgs,
) -> Result<(), CommandError> {
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error("Forgetting observations requires the current integrated operation"));
    }
    if args.remote.as_str() == git::REMOTE_NAME_FOR_LOCAL_GIT_REPO.as_str() {
        return Err(user_error("The local Git observation is not a remote connection"));
    }
    let mut workspace = command.workspace_helper_no_snapshot(ui).await?;
    let git_lock = workspace.lock_git_import_export()?;
    let git_repo = git::get_git_repo(workspace.repo().store())?;
    git::ensure_no_pending_remote_management(&git_repo)?;
    let old_view = workspace.repo().view().store_view();
    let view = workspace.repo().view();
    let project = args
        .project
        .as_deref()
        .map(|name| view.project_state().project_by_name(name).map(|(id, _)| id))
        .transpose()
        .map_err(user_error)?;
    let mut candidates = git::get_all_remote_names(workspace.repo().store())?;
    candidates.extend(old_view.remote_connections.keys().cloned());
    candidates.extend(old_view.remote_views.keys().cloned());
    candidates.extend(
        old_view
            .project_observations
            .keys()
            .map(|key| key.remote.clone()),
    );
    candidates.sort();
    candidates.dedup();
    let remote = crate::git_remote::resolve_remote_selector_in_view(
        view,
        &candidates,
        args.remote.as_str(),
        project.as_ref(),
    )?;
    let identity = view.remote_identity(&remote).map_err(user_error)?;
    let display_name = view.remote_qualified_name(&remote);
    if identity.is_some() && !crate::git_remote::capabilities(command).contains(&"jjosh-v1") {
        return Err(user_error(
            "Forgetting scoped observations requires capability jjosh-v1",
        ));
    }
    if old_view
        .project_observations
        .keys()
        .any(|key| key.remote == remote)
        && !crate::git_remote::capabilities(command).contains(&"jjosh-v1")
    {
        return Err(user_error(
            "Forgetting converted observations requires capability jjosh-v1",
        ));
    }
    let prefixes = [
        format!("refs/remotes/{}/", remote.as_str()),
        format!("{}{}/", git::REMOTE_TAG_REF_NAMESPACE, remote.as_str()),
    ];
    let mut edits = Vec::new();
    for prefix in &prefixes {
        for reference in git_repo.references().map_err(user_error)?.prefixed(prefix.as_str()).map_err(user_error)? {
            let reference = reference.map_err(user_error)?;
            edits.push(RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::MustExistAndMatch(reference.target().into_owned()),
                    log: RefLog::AndReference,
                },
                name: reference.name().to_owned(),
                deref: false,
            });
        }
    }
    let mut cleared = old_view.clone();
    cleared.remote_views.remove(&remote);
    // Active scoped names need their bridge independently of cached refs.
    // Detached imported names retire with their last observations, permitting reuse.
    if identity.is_none() || git::try_find_active_remote(&git_repo, &remote)?.is_none() {
        if let Some(connection) = cleared
            .remote_connections
            .get(&remote)
            .and_then(|owner| owner.as_resolved())
            .and_then(Option::as_ref)
        {
            cleared.project_state.remote_names.remove(connection);
        }
        cleared.remote_connections.remove(&remote);
    }
    cleared
        .project_observations
        .retain(|key, _| key.remote != remote);
    cleared.git_refs.retain(|name, _| {
        !prefixes
            .iter()
            .any(|prefix| name.as_str().starts_with(prefix))
    });
    if &cleared == old_view && edits.is_empty() {
        return Err(user_error(format!(
            "No observations or cached refs for remote {display_name}"
        )));
    }
    // Acquire all reference locks before creating a recovery record or mutating state.
    let prepared = if edits.is_empty() {
        None
    } else {
        Some(git_repo.refs.transaction().prepare(
            edits.clone(),
            gix::lock::acquire::Fail::Immediately,
            gix::lock::acquire::Fail::Immediately,
        ).map_err(user_error)?)
    };
    let journal = git::begin_remote_management(
        workspace.repo().store(), &workspace.repo().operation().id().hex(), &[],
    )?;
    journal.record_ref_edits(&git_repo, &edits)?;
    let mut tx = workspace.start_transaction();
    tx.repo_mut().set_view(cleared);
    journal.expect_operation(tx.repo().view())?;
    if let Some(prepared) = prepared {
        prepared.commit(git_repo.committer().transpose().map_err(user_error)?).map_err(user_error)?;
    }
    tx.finish_with_git_import_export_lock(
        ui,
        format!("forget observations for git remote {display_name}"),
        &git_lock,
    )
    .await?;
    journal.complete()?;
    Ok(())
}
