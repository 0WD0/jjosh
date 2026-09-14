// Copyright 2020-2023 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod add;
mod forget_observations;
mod list;
mod remove;
mod rename;
mod set_url;

use clap::Subcommand;

use jj_lib::git;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{BindingId, BindingRecord, ConnectionId};
use jj_lib::ref_name::{RemoteName, RemoteNameBuf};
use jj_lib::repo::Repo as _;
use crate::cli_util::WorkspaceCommandHelper;
use crate::command_error::user_error;
use crate::git_remote::GitRemoteBindingArgs;
use self::add::GitRemoteAddArgs;
use self::add::cmd_git_remote_add;
use self::forget_observations::{GitRemoteForgetObservationsArgs, cmd_git_remote_forget_observations};
use self::list::GitRemoteListArgs;
use self::list::cmd_git_remote_list;
use self::remove::GitRemoteRemoveArgs;
use self::remove::cmd_git_remote_remove;
use self::rename::GitRemoteRenameArgs;
use self::rename::cmd_git_remote_rename;
use self::set_url::GitRemoteSetUrlArgs;
use self::set_url::cmd_git_remote_set_url;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Manage Git remotes
///
/// The Git repo will be a bare git repo stored inside the `.jj/` directory.
#[derive(Subcommand, Clone, Debug)]
pub enum RemoteCommand {
    Add(GitRemoteAddArgs),
    Attach(GitRemoteAttachArgs),
    Recover(GitRemoteRecoverArgs),
    ForgetObservations(GitRemoteForgetObservationsArgs),
    List(GitRemoteListArgs),
    Remove(GitRemoteRemoveArgs),
    Rename(GitRemoteRenameArgs),
    SetUrl(GitRemoteSetUrlArgs),
}

pub async fn cmd_git_remote(
    ui: &mut Ui,
    command: &CommandHelper,
    subcommand: &RemoteCommand,
) -> Result<(), CommandError> {
    match subcommand {
        RemoteCommand::Add(args) => cmd_git_remote_add(ui, command, args).await,
        RemoteCommand::Attach(args) => cmd_attach(ui, command, args).await,
        RemoteCommand::Recover(args) => cmd_recover(ui, command, args).await,
        RemoteCommand::ForgetObservations(args) => cmd_git_remote_forget_observations(ui, command, args).await,
        RemoteCommand::List(args) => cmd_git_remote_list(ui, command, args).await,
        RemoteCommand::Remove(args) => cmd_git_remote_remove(ui, command, args).await,
        RemoteCommand::Rename(args) => cmd_git_remote_rename(ui, command, args).await,
        RemoteCommand::SetUrl(args) => cmd_git_remote_set_url(ui, command, args).await,
    }
}

/// Explicitly bind an existing, unused ordinary connection.
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteAttachArgs {
    remote: RemoteNameBuf,
    #[command(flatten)]
    binding: GitRemoteBindingArgs,
}

/// Recover an interrupted local remote change without network access.
#[derive(clap::Args, Clone, Debug)]
#[group(required = true, multiple = false)]
pub struct GitRemoteRecoverArgs {
    /// Restore saved config and refs; requires the original operation.
    #[arg(long)]
    rollback: bool,
    /// Keep local changes after verifying current binding and owner consistency.
    #[arg(long)]
    accept: bool,
}

fn ensure_empty_remote_name(
    view: &jj_lib::view::View,
    remote: &RemoteName,
) -> Result<(), CommandError> {
    if view.store_view().remote_connections.contains_key(remote)
        || view.store_view().project_observations.keys().any(|key| key.remote == remote)
        || view.all_remote_bookmarks().any(|(symbol, _)| symbol.remote == remote)
        || view.all_remote_tags().any(|(symbol, _)| symbol.remote == remote) {
        return Err(user_error(format!("Remote name {remote} still owns observations or tracking; use `git remote forget-observations {remote}` before reusing it", remote = remote.as_str())));
    }
    Ok(())
}

fn prepare_binding(
    command: &CommandHelper,
    workspace: &WorkspaceCommandHelper,
    remote: &RemoteName,
    connection: &ConnectionId,
    args: &GitRemoteBindingArgs,
) -> Result<Option<BindingRecord>, CommandError> {
    if !args.is_managed() {
        return Ok(None);
    }
    if !crate::git_remote::capabilities(command).contains(&"jjosh-v1") {
        return Err(user_error(
            "Conversion arguments require capability jjosh-v1",
        ));
    }
    command
        .git_remote_extension()
        .unwrap()
        .prepare_binding(workspace, remote, connection, args)
        .map(Some)
}

async fn cmd_attach(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteAttachArgs,
) -> Result<(), CommandError> {
    let mut workspace = command.workspace_helper_no_snapshot(ui).await?;
    crate::git_remote::check_remote(command, &workspace, &args.remote)?;
    let git_repo = git::get_git_repo(workspace.repo().store())?;
    if git::try_find_active_remote(&git_repo, &args.remote)?.is_none() {
        return Err(git::GitRemoteManagementError::NoSuchRemote(args.remote.clone()).into());
    }
    if git::remote_required_capability(&git_repo, &args.remote).is_some() {
        return Err(user_error("Connection already has a binding; create a new connection instead"));
    }
    let connection = git::remote_connection_id(&git_repo, &args.remote).map_err(user_error)?.unwrap_or_else(ConnectionId::generate);
    if workspace.repo().view().project_state().binding_for_connection(&connection).map_err(user_error)?.is_some() {
        return Err(user_error("Connection already has an active binding"));
    }
    if workspace.repo().view().all_remote_bookmarks().any(|(symbol, _)| symbol.remote == args.remote)
        || workspace.repo().view().all_remote_tags().any(|(symbol, _)| symbol.remote == args.remote)
        || workspace.repo().view().store_view().project_observations.keys().any(|key| key.remote == args.remote) {
        return Err(user_error("Attach requires an unused remote; explicitly migrate its existing observations and tracking"));
    }
    let binding = prepare_binding(
        command,
        &workspace,
        &args.remote,
        &connection,
        &args.binding,
    )?
    .ok_or_else(|| {
        user_error(
            "Attach requires an explicit representation (--whole, --filter, --view, or --like)",
        )
    })?;
    let git_lock = workspace.lock_git_import_export()?;
    let journal = git::begin_remote_management(
        workspace.repo().store(),
        &workspace.repo().operation().id().hex(),
        &[],
    )?;
    journal.expect_remote(&args.remote, true, Some(&connection), true)?;
    let mut tx = workspace.start_transaction();
    git::set_remote_config_keys(
        tx.repo().store(),
        &[
            (
                args.remote.clone(),
                "jjosh-connectionId".into(),
                Some(connection.hex()),
            ),
            (
                args.remote.clone(),
                "jjosh-requiredCapability".into(),
                Some("jjosh-v1".into()),
            ),
        ],
    )?;
    tx.repo_mut()
        .view_mut()
        .project_state_mut()
        .bindings
        .insert(BindingId::generate(), Merge::resolved(Some(binding)));
    tx.repo_mut()
        .view_mut()
        .store_view_mut()
        .remote_connections
        .insert(args.remote.clone(), Merge::resolved(Some(connection)));
    journal.expect_operation(tx.repo().view())?;
    tx.finish_with_git_import_export_lock(
        ui,
        format!("attach git remote {}", args.remote.as_symbol()),
        &git_lock,
    )
    .await?;
    journal.complete()?;
    Ok(())
}

async fn cmd_recover(ui: &mut Ui, command: &CommandHelper, args: &GitRemoteRecoverArgs) -> Result<(), CommandError> {
    let workspace = command.workspace_helper_no_snapshot(ui).await?;
    let _git_lock = workspace.lock_git_import_export()?;
    git::recover_remote_management(workspace.repo().store(), workspace.repo().view(), &workspace.repo().operation().id().hex(), args.accept, crate::git_remote::capabilities(command))?;
    writeln!(ui.status(), "Recovered local remote change.")?;
    Ok(())
}
