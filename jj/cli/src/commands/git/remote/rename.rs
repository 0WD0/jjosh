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

    /// Resolve the remote within this project
    #[arg(long)]
    project: Option<String>,
}

pub async fn cmd_git_remote_rename(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteRenameArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let old = crate::git_remote::resolve_remote_selector(&workspace_command, args.old.as_str(), args.project.as_deref())?;
    crate::git_remote::check_remote(command, &workspace_command, &old)?;
    let identity = workspace_command.repo().view().remote_identity(&old)
        .map_err(crate::command_error::user_error)?.cloned();
    let (new, specified_scope) = super::new_remote_name(&workspace_command, &args.new, args.project.as_deref())?;
    let scope = identity.as_ref().map(|identity| &identity.project);
    if specified_scope.as_ref().is_some_and(|requested| Some(requested) != scope) {
        return Err(crate::command_error::user_error("Rename cannot move a remote to another scope"));
    }
    super::ensure_available_remote_name(&workspace_command, &new, scope)?;
    let local_old = workspace_command.repo().view().remote_local_name(&old).to_owned();
    let display_old = workspace_command.repo().view().remote_qualified_name(&old);
    let labels = scope.map(|project| super::project_config_labels(workspace_command.repo().view(), project));
    let mut options = git::GitRemoteManagementOptions {
        extra_config_keys: git::MANAGED_REMOTE_KEYS,
        ..Default::default()
    };
    options.repo_config =
        rename_remote_in_repo_config(ui, command.raw_config(), workspace_command.repo().view(), &local_old, &new, labels.as_deref())?;
    let extra_paths = options.repo_config.iter().map(|file| file.path().to_owned()).collect::<Vec<_>>();
    let journal = git::begin_remote_management(workspace_command.repo().store(), &workspace_command.repo().operation().id().hex(), &extra_paths)?;
    let git_repo = git::get_git_repo(workspace_command.repo().store())?;
    let connection = git::remote_connection_id(&git_repo, &old).map_err(crate::command_error::user_error)?;
    let managed = git::remote_required_capability(&git_repo, &old).is_some();
    let mut tx = workspace_command.start_transaction();
    if let Some(mut identity) = identity {
        let connection = connection.as_ref().ok_or_else(|| crate::command_error::user_error("Scoped remote has no configured connection"))?;
        journal.expect_remote(&old, true, Some(connection), managed)?;
        git::commit_remote_management_config(tx.repo().store(), &old, options.repo_config.as_ref())?;
        identity.name = new.clone();
        tx.repo_mut().view_mut().project_state_mut().remote_names.insert(
            connection.clone(), jj_lib::merge::Merge::resolved(Some(identity)),
        );
    } else {
        journal.expect_remote(&old, false, connection.as_ref(), false)?;
        journal.expect_remote(&new, true, connection.as_ref(), managed)?;
        git::rename_remote_with_options(tx.repo_mut(), &old, &new, &options)?;
        let view = tx.repo_mut().view_mut().store_view_mut();
        if let Some(owner) = view.remote_connections.remove(&old) {
            view.remote_connections.insert(new.clone(), owner);
        }
        let old_keys: Vec<_> = view.project_observations.keys().filter(|key| key.remote == old).cloned().collect();
        for key in old_keys {
            let value = view.project_observations.remove(&key).unwrap();
            view.project_observations.insert(jj_lib::project::ObservationKey { remote: new.clone(), ..key }, value);
        }
    }
    journal.expect_operation(tx.repo().view())?;
    if tx.repo().has_changes() {
        tx.finish_with_git_import_export_lock(
            ui,
            format!("rename git remote {display_old} to {}", new.as_symbol()),
            &git_lock,
        )
        .await?;
    } else {
        // Do not print "Nothing changed."
    }
    journal.complete()?;
    Ok(())
}
