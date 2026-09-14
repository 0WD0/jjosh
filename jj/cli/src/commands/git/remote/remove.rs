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

use clap_complete::ArgValueCandidates;
use jj_lib::git;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::object_id::ObjectId as _;

use super::super::remove_remote_from_repo_config;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::complete;
use crate::ui::Ui;

/// Remove a Git remote and forget its bookmarks
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteRemoveArgs {
    /// The remote's name
    #[arg(add = ArgValueCandidates::new(complete::git_remotes))]
    remote: RemoteNameBuf,

    /// Resolve the remote within this project
    #[arg(long)]
    project: Option<String>,
}

pub async fn cmd_git_remote_remove(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteRemoveArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let remote = crate::git_remote::resolve_remote_selector(
        &workspace_command,
        args.remote.as_str(),
        args.project.as_deref(),
    )?;
    crate::git_remote::check_remote(command, &workspace_command, &remote)?;
    let view = workspace_command.repo().view();
    let identity = view
        .remote_identity(&remote)
        .map_err(crate::command_error::user_error)?;
    let labels = identity.map(|identity| super::project_config_labels(view, &identity.project));
    let local_name = view.remote_local_name(&remote);
    let display_name = view.remote_qualified_name(&remote);
    let mut options = git::GitRemoteManagementOptions {
        extra_config_keys: git::MANAGED_REMOTE_KEYS,
        ..Default::default()
    };
    options.repo_config = remove_remote_from_repo_config(
        ui,
        command.raw_config(),
        view,
        local_name,
        labels.as_deref(),
    )?;
    let extra_paths = options
        .repo_config
        .iter()
        .map(|file| file.path().to_owned())
        .collect::<Vec<_>>();
    let journal = git::begin_remote_management(
        workspace_command.repo().store(),
        &workspace_command.repo().operation().id().hex(),
        &extra_paths,
    )?;
    let git_repo = git::get_git_repo(workspace_command.repo().store())?;
    let connection =
        git::remote_connection_id(&git_repo, &remote).map_err(crate::command_error::user_error)?;
    journal.expect_remote(&remote, false, connection.as_ref(), false)?;
    let mut tx = workspace_command.start_transaction();
    git::remove_remote_with_options(tx.repo_mut(), &remote, &options)?;
    if let Some(connection) = connection {
        tx.repo_mut()
            .view_mut()
            .project_state_mut()
            .remote_names
            .remove(&connection);
        tx.repo_mut()
            .view_mut()
            .project_state_mut()
            .bindings
            .retain(|_, value| {
                !value
                    .iter()
                    .flatten()
                    .any(|binding| binding.connection_id == connection)
            });
    }
    let view = tx.repo_mut().view_mut().store_view_mut();
    view.remote_connections.remove(&remote);
    view.project_observations
        .retain(|key, _| key.remote != remote);
    journal.expect_operation(tx.repo().view())?;
    if tx.repo().has_changes() {
        tx.finish_with_git_import_export_lock(
            ui,
            format!("remove git remote {display_name}"),
            &git_lock,
        )
        .await?;
    } else {
        // Do not print "Nothing changed." for the remote named "git".
    }
    journal.complete()?;
    Ok(())
}
