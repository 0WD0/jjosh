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

use gix::Url;
use gix::remote::Direction;
use jj_lib::git;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{BindingId, ConnectionId};
use jj_lib::merge::Merge;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::git_util::absolute_git_url;
use crate::ui::Ui;

/// Add a Git remote
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteAddArgs {
    /// The remote's name
    remote: RemoteNameBuf,

    /// The remote's URL or path
    ///
    /// Local path will be resolved to absolute form.
    #[arg(value_hint = clap::ValueHint::Url)]
    url: String,

    /// The URL used for push
    ///
    /// Local path will be resolved to absolute form
    #[arg(long, value_hint = clap::ValueHint::Url)]
    push_url: Option<String>,

    #[command(flatten)]
    binding: crate::git_remote::GitRemoteBindingArgs,
}

pub async fn cmd_git_remote_add(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteAddArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper_no_snapshot(ui).await?;
    let url = absolute_git_url(command.cwd(), &args.url)?;
    let push_url = args
        .push_url
        .as_deref()
        .map(|url| absolute_git_url(command.cwd(), url))
        .transpose()?;

    super::ensure_empty_remote_name(workspace_command.repo().view(), &args.remote)?;
    let git_repo = git::get_git_repo(workspace_command.repo().store())?;
    if git::try_find_active_remote(&git_repo, &args.remote)?.is_some() {
        return Err(git::GitRemoteManagementError::RemoteAlreadyExists(args.remote.clone()).into());
    }
    let connection = ConnectionId::generate();
    let binding = super::prepare_binding(command, &workspace_command, &args.remote, &connection, &args.binding)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let journal = git::begin_remote_management(workspace_command.repo().store(), &workspace_command.repo().operation().id().hex(), &[])?;
    journal.expect_remote(&args.remote, true, Some(&connection), binding.is_some())?;
    let mut tx = workspace_command.start_transaction();
    git::add_remote(tx.repo_mut(), &args.remote, &url, push_url.as_deref())?;
    let mut keys = vec![(args.remote.clone(), "jjosh-connectionId".into(), Some(connection.hex()))];
    if let Some(binding) = binding {
        keys.push((args.remote.clone(), "jjosh-requiredCapability".into(), Some("jjosh-v1".into())));
        keys.push((args.remote.clone(), "jjosh-readOnly".into(), Some((args.binding.read_only || (!args.binding.writable && push_url.is_none())).to_string())));
        tx.repo_mut().view_mut().project_state_mut().bindings.insert(BindingId::generate(), Merge::resolved(Some(binding)));
        tx.repo_mut().view_mut().store_view_mut().remote_connections.insert(args.remote.clone(), Merge::resolved(Some(connection.clone())));
    }
    git::set_remote_config_keys(tx.repo().store(), &keys)?;
    warn_if_remote_url_matches(ui, tx.repo(), &args.remote, &url, push_url.as_deref())?;
    journal.expect_operation(tx.repo().view())?;
    tx.finish_with_git_import_export_lock(ui, format!("add git remote {}", args.remote.as_symbol()), &git_lock).await?;
    journal.complete()?;
    Ok(())
}

/// Warns if the new configured URL exactly matches an existing configured URL.
///
/// URL equivalence is not reliable. For example, HTTPS, SSH, .git suffixes, and
/// redirects can identify the same repository. Git URL rewrite rules can also
/// make different configured strings resolve to the same destination, so this
/// is only an exact-match warning. A remote without a configured push URL is
/// treated as pushing to its fetch URL, matching Git's behavior.
fn warn_if_remote_url_matches(
    ui: &Ui,
    repo: &dyn Repo,
    new_remote: &RemoteName,
    new_fetch_url: &str,
    new_push_url: Option<&str>,
) -> Result<(), CommandError> {
    let git_repo = git::get_git_repo(repo.store())?;
    // Git uses the fetch URL for push when no push URL is configured.
    let new_push_url = new_push_url.unwrap_or(new_fetch_url);
    let mut warned = false;
    for remote_name in git_repo.remote_names() {
        // Ignore the remote that was just added.
        if remote_name == new_remote.as_str() {
            continue;
        }
        // Ignore empty or unloadable remote sections for this advisory warning.
        let Some(Ok(remote)) = git_repo.try_find_remote_without_url_rewrite(&**remote_name) else {
            continue;
        };
        let remote_fetch_url = remote.url(Direction::Fetch).map(Url::to_bstring);
        let remote_push_url = remote.url(Direction::Push).map(Url::to_bstring);
        let remote_url_matches = [remote_fetch_url, remote_push_url]
            .iter()
            .flatten()
            .any(|remote_url| remote_url == new_fetch_url || remote_url == new_push_url);
        // Don't print the URL itself because remote URLs can contain credentials,
        // such as user:password or token path segments.
        if remote_url_matches {
            writeln!(
                ui.warning_default(),
                "Remote {remote_name} already uses the same URL."
            )?;
            warned = true;
        }
    }
    if warned {
        writeln!(
            ui.hint_default(),
            "If this was a mistake, run `jj git remote remove {new_remote}`.",
            new_remote = new_remote.as_symbol()
        )?;
    }
    Ok(())
}
