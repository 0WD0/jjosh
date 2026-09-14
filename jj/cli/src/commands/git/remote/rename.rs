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

use super::super::rename_remote_in_repo_config;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::complete;
use crate::ui::Ui;

/// Rename a Git remote
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteRenameArgs {
    /// The name of an existing remote
    #[arg(add = ArgValueCandidates::new(complete::git_remotes))]
    old: RemoteNameBuf,

    /// The desired name for `old`
    new: RemoteNameBuf,
}

pub async fn cmd_git_remote_rename(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteRenameArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let git_lock = workspace_command.lock_git_import_export()?;
    crate::git_remote::check_remote(command, &workspace_command, &args.old)?;
    super::ensure_empty_remote_name(workspace_command.repo().view(), &args.new)?;
    let mut options = git::GitRemoteManagementOptions {
        extra_config_keys: git::MANAGED_REMOTE_KEYS,
        ..Default::default()
    };
    options.repo_config =
        rename_remote_in_repo_config(ui, command.raw_config(), &args.old, &args.new)?;
    let extra_paths = options.repo_config.iter().map(|file| file.path().to_owned()).collect::<Vec<_>>();
    let journal = git::begin_remote_management(workspace_command.repo().store(), &workspace_command.repo().operation().id().hex(), &extra_paths)?;
    let git_repo = git::get_git_repo(workspace_command.repo().store())?;
    let connection = git::remote_connection_id(&git_repo, &args.old).map_err(crate::command_error::user_error)?;
    journal.expect_remote(&args.old, false, connection.as_ref(), false)?;
    journal.expect_remote(&args.new, true, connection.as_ref(), git::remote_required_capability(&git_repo, &args.old).is_some())?;
    let mut tx = workspace_command.start_transaction();
    git::rename_remote_with_options(tx.repo_mut(), &args.old, &args.new, &options)?;
    let view = tx.repo_mut().view_mut().store_view_mut();
    if let Some(owner) = view.remote_connections.remove(&args.old) {
        view.remote_connections.insert(args.new.clone(), owner);
    }
    let old_keys: Vec<_> = view.project_observations.keys().filter(|key| key.remote == args.old).cloned().collect();
    for key in old_keys {
        let value = view.project_observations.remove(&key).unwrap();
        view.project_observations.insert(jj_lib::project::ObservationKey { remote: args.new.clone(), ..key }, value);
    }
    journal.expect_operation(tx.repo().view())?;
    if tx.repo().has_changes() {
        tx.finish_with_git_import_export_lock(
            ui,
            format!(
                "rename git remote {old} to {new}",
                old = args.old.as_symbol(),
                new = args.new.as_symbol()
            ),
            &git_lock,
        )
        .await?;
    } else {
        // Do not print "Nothing changed."
    }
    journal.complete()?;
    Ok(())
}
