// Copyright 2024 The Jujutsu Authors
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
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::complete;
use crate::git_util::absolute_git_url;
use crate::ui::Ui;

/// Set the URL of a Git remote
///
/// A restored logical remote without local configuration can be reconnected by
/// supplying a fetch URL. Its connection identity, binding and tracking remain
/// unchanged. Configuration belonging to a different connection is not reused.
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteSetUrlArgs {
    /// The remote's name
    #[arg(add = ArgValueCandidates::new(complete::git_remotes))]
    remote: RemoteNameBuf,

    /// Resolve the remote within this project
    #[arg(long)]
    project: Option<String>,

    /// The URL or path to fetch from
    ///
    /// This is a short form, equivalent to using the explicit --fetch.
    ///
    /// Local path will be resolved to absolute form.
    #[arg(value_hint = clap::ValueHint::Url)]
    url: Option<String>,

    /// The URL or path to push to
    ///
    /// Local path will be resolved to absolute form.
    #[arg(long, value_hint = clap::ValueHint::Url)]
    push: Option<String>,

    /// The URL or path to fetch from
    ///
    /// Local path will be resolved to absolute form.
    #[arg(long, value_hint = clap::ValueHint::Url, conflicts_with = "url")]
    fetch: Option<String>,
}

pub async fn cmd_git_remote_set_url(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteSetUrlArgs,
) -> Result<(), CommandError> {
    super::require_integrated_local_state(command)?;
    let workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let _git_lock = workspace_command.lock_git_import_export()?;
    let journal = local_state::begin(workspace_command.repo(), &[]).await?;
    let view = workspace_command.repo().view();
    let remote = super::resolve_management_remote(
        &workspace_command,
        args.remote.as_str(),
        args.project.as_deref(),
    )?;
    let mut git_repo = git::get_git_repo(workspace_command.repo().store())?;
    git_repo.reload().map_err(user_error)?;
    let inspection = git::inspect_remote_management(view, &git_repo, &remote)?;
    let connection = inspection.connection();
    super::check_management_binding(command, &workspace_command, &remote, connection)?;
    git::check_remote_owner(view, &remote, connection).map_err(user_error)?;
    if inspection.has_config && !inspection.owns_config() {
        return Err(user_error(format!(
            "Remote {} has local configuration owned by another connection; deliberately remove \
             or rename that configuration before reconnecting this logical remote",
            remote.as_symbol(),
        )));
    }
    if inspection.has_config {
        crate::git_remote::check_remote(command, &workspace_command, &remote)?;
    }
    let process_url = |url: Option<&String>| {
        url.map(|url| absolute_git_url(command.cwd(), url))
            .transpose()
    };

    let fetch_url = process_url(args.url.as_ref().or(args.fetch.as_ref()))?;
    let push_url = process_url(args.push.as_ref())?;
    if !inspection.has_config {
        if connection.is_none() {
            return Err(git::GitRemoteManagementError::NoSuchRemote(remote.clone()).into());
        }
        let url = fetch_url
            .as_deref()
            .ok_or_else(|| user_error("Reconnecting a disconnected remote requires a fetch URL"))?;
        // Validate before creating any local configuration or journal.
        git_repo.remote_at(url).map_err(user_error)?;
        if let Some(url) = &push_url {
            git_repo.remote_at(url.as_str()).map_err(user_error)?;
        }
    }
    if !inspection.has_config {
        let connection = connection.expect("validated logical connection");
        let managed = view
            .project_state()
            .binding_for_connection(connection)
            .map_err(user_error)?
            .is_some();
        let mut keys = vec![(
            remote.clone(),
            "jjosh-connectionId".into(),
            Some(connection.hex()),
        )];
        if managed {
            keys.push((
                remote.clone(),
                "jjosh-requiredCapability".into(),
                Some("jjosh-v1".into()),
            ));
        }
        // Only reconnect local configuration. Keep the operation's historical
        // tracking state exactly as restored, including an absent remote view.
        git::create_remote_config(
            workspace_command.repo().store(),
            &remote,
            fetch_url.as_deref().expect("validated fetch URL"),
            push_url.as_deref(),
            Some(&journal),
        )?;
        git::set_remote_config_keys(workspace_command.repo().store(), &keys, Some(&journal))?;
    }

    if inspection.has_config {
        git::set_remote_urls(
            workspace_command.repo().store(),
            &remote,
            fetch_url.as_deref(),
            push_url.as_deref(),
            Some(&journal),
        )?;
    }
    journal.commit_local()?;
    journal.complete().await?;
    Ok(())
}
