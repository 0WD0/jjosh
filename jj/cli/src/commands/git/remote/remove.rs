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
}

pub async fn cmd_git_remote_remove(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteRemoveArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let mut options = command
        .git_remote_extension()
        .map(|extension| {
            extension.prepare_remote_management(&workspace_command, &args.remote, None)
        })
        .transpose()?
        .unwrap_or_default();
    options.repo_config = remove_remote_from_repo_config(ui, command.raw_config(), &args.remote)?;
    let mut tx = workspace_command.start_transaction();
    git::remove_remote_with_options(tx.repo_mut(), &args.remote, &options)?;
    if tx.repo().has_changes() {
        tx.finish_with_git_import_export_lock(
            ui,
            format!("remove git remote {}", args.remote.as_symbol()),
            &git_lock,
        )
        .await?;
    } else {
        // Do not print "Nothing changed." for the remote named "git".
    }
    Ok(())
}
