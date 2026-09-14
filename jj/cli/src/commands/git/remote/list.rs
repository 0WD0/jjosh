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

use std::io::Write as _;

use bstr::BString;
use gix::Remote;
use jj_lib::git;
use jj_lib::ref_name::RemoteName;
use jj_lib::repo::Repo as _;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// List Git remotes
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteListArgs {
    /// List local aliases in this project
    #[arg(long)]
    project: Option<String>,
}

pub async fn cmd_git_remote_list(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteListArgs,
) -> Result<(), CommandError> {
    let workspace_command = command.workspace_helper(ui).await?;
    let git_repo = git::get_git_repo(workspace_command.repo().store())?;
    let view = workspace_command.repo().view();
    let project = args
        .project
        .as_deref()
        .map(|name| view.project_state().project_by_name(name).map(|(id, _)| id))
        .transpose()
        .map_err(crate::command_error::user_error)?;
    let mut entries = Vec::new();
    for remote_name in git_repo.remote_names() {
        let Ok(remote_name) = str::from_utf8(&remote_name).map(RemoteName::new) else {
            continue; // ignore non-UTF-8 remote names which we don't support
        };
        let Some(remote) = git::try_find_active_remote(&git_repo, remote_name)? else {
            continue; // ignore empty [remote "<name>"] section
        };
        if let Some(project) = &project {
            if !view
                .remote_in_scope(remote_name, Some(project))
                .map_err(crate::command_error::user_error)?
            {
                continue;
            }
        } else {
            view.remote_identity(remote_name)
                .map_err(crate::command_error::user_error)?;
        }
        let display_name = if project.is_some() {
            view.remote_local_name(remote_name).as_str().to_owned()
        } else {
            view.remote_qualified_name(remote_name)
        };
        let fetch_url = get_url(&remote, gix::remote::Direction::Fetch);
        let push_url = get_url(&remote, gix::remote::Direction::Push);
        entries.push((display_name, fetch_url, push_url));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (display_name, fetch_url, push_url) in entries {
        if fetch_url == push_url {
            writeln!(ui.stdout(), "{display_name} {fetch_url}")?;
        } else {
            writeln!(ui.stdout(), "{display_name} {fetch_url} (push: {push_url})")?;
        }
    }
    Ok(())
}

fn get_url(remote: &Remote, direction: gix::remote::Direction) -> BString {
    remote
        .url(direction)
        .map(|url| url.to_bstring())
        .unwrap_or_else(|| "<no URL>".into())
}
