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
    let mut options = command
        .git_remote_extension()
        .map(|extension| {
            extension.prepare_remote_management(&workspace_command, &args.old, Some(&args.new))
        })
        .transpose()?
        .unwrap_or_default();
    options.repo_config =
        rename_remote_in_repo_config(ui, command.raw_config(), &args.old, &args.new)?;
    let mut tx = workspace_command.start_transaction();
    git::rename_remote_with_options(tx.repo_mut(), &args.old, &args.new, &options)?;
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
    Ok(())
}
