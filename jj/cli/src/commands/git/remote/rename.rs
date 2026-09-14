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
use jj_lib::local_state;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;

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

    /// Resolve the remote within this project
    #[arg(long)]
    project: Option<String>,
}

pub async fn cmd_git_remote_rename(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteRenameArgs,
) -> Result<(), CommandError> {
    super::require_integrated_local_state(command)?;
    let mut workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let extra_paths: Vec<_> = command.config_env().maybe_repo_config_path(ui)?.into_iter().collect();
    let journal = local_state::begin(workspace_command.repo(), &extra_paths).await?;
    crate::git_remote::check_repo_config_unchanged(command.raw_config())?;
    let old = super::resolve_management_remote(
        &workspace_command,
        args.old.as_str(),
        args.project.as_deref(),
    )?;
    let mut git_repo = git::get_git_repo(workspace_command.repo().store())?;
    git_repo.reload().map_err(crate::command_error::user_error)?;
    let inspection =
        git::inspect_remote_management(workspace_command.repo().view(), &git_repo, &old)?;
    let connection = inspection.connection();
    super::check_management_binding(command, &workspace_command, &old, connection)?;
    if inspection.owns_config() {
        crate::git_remote::check_remote(command, &workspace_command, &old)?;
    }
    let identity = workspace_command
        .repo()
        .view()
        .remote_identity(&old)
        .map_err(crate::command_error::user_error)?
        .and_then(|identity| identity.scoped_name)
        .cloned();
    let (new, specified_scope) =
        super::new_remote_name(&workspace_command, &args.new, args.project.as_deref())?;
    let scope = identity.as_ref().map(|identity| &identity.project);
    if specified_scope
        .as_ref()
        .is_some_and(|requested| Some(requested) != scope)
    {
        return Err(crate::command_error::user_error(
            "Rename cannot move a remote to another scope",
        ));
    }
    super::ensure_available_remote_name(&workspace_command, &new, scope)?;
    let local_old = workspace_command
        .repo()
        .view()
        .remote_local_name(&old)
        .to_owned();
    let display_old = workspace_command.repo().view().remote_qualified_name(&old);
    let labels =
        scope.map(|project| super::project_config_labels(workspace_command.repo().view(), project));
    let mut options = git::GitRemoteManagementOptions {
        extra_config_keys: git::MANAGED_REMOTE_KEYS,
        ..Default::default()
    };
    options.repo_config = rename_remote_in_repo_config(
        ui,
        command.raw_config(),
        workspace_command.repo().view(),
        &local_old,
        &new,
        labels.as_deref(),
    )?;
    let mut tx = workspace_command.start_transaction();
    tx.bind_local_state(&journal)?;
    if let Some(mut identity) = identity {
        let connection = connection.ok_or_else(|| {
            crate::command_error::user_error("Scoped remote has no logical connection")
        })?;
        tx.repo_mut()
            .view_mut()
            .archive_remote_observations(&old)
            .map_err(crate::command_error::user_error)?;
        git::commit_remote_management_config(
            tx.repo().store(),
            &old,
            options.repo_config.as_ref(),
            Some(&journal),
        )?;
        identity.name = new.clone();
        tx.repo_mut()
            .view_mut()
            .project_state_mut()
            .remote_names
            .insert(
                connection.clone(),
                jj_lib::merge::Merge::resolved(Some(identity)),
            );
    } else {
        tx.repo_mut()
            .view_mut()
            .archive_remote_observations(&new)
            .map_err(crate::command_error::user_error)?;
        git::rename_remote_with_options(tx.repo_mut(), &old, &new, &options)?;
    }
    if tx.repo().has_changes() {
        tx.finish_with_git_import_export_lock(
            ui,
            format!("rename git remote {display_old} to {}", new.as_symbol()),
            &git_lock,
        )
        .await?;
    } else {
        // Do not print "Nothing changed."
        journal.commit_local()?;
    }
    journal.complete().await?;
    Ok(())
}
