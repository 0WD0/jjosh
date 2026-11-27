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

mod clone;
mod colocation;
mod export;
mod fetch;
mod import;
mod init;
mod push;
mod remote;
mod root;

use std::io::Write as _;

use clap::Subcommand;
use jj_lib::config::ConfigFile;
use jj_lib::config::ConfigLayer;
use jj_lib::config::ConfigSource;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::ref_name::RemoteRefSymbol;
use jj_lib::ref_name::RemoteRefSymbolBuf;
use jj_lib::revset;

use self::clone::GitCloneArgs;
use self::clone::cmd_git_clone;
use self::colocation::GitColocationCommand;
use self::colocation::cmd_git_colocation;
use self::export::GitExportArgs;
use self::export::cmd_git_export;
use self::fetch::GitFetchArgs;
use self::fetch::cmd_git_fetch;
use self::import::GitImportArgs;
use self::import::cmd_git_import;
use self::init::GitInitArgs;
use self::init::cmd_git_init;
use self::push::GitPushArgs;
use self::push::cmd_git_push;
pub use self::push::is_push_operation;
use self::remote::RemoteCommand;
use self::remote::cmd_git_remote;
use self::root::GitRootArgs;
use self::root::cmd_git_root;
use crate::cli_util::CommandHelper;
use crate::cli_util::WorkspaceCommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error_with_message;
use crate::config::ConfigEnv;
use crate::config::RawConfig;
use crate::config::existing_repo_config_file;
use crate::ui::Ui;

/// Commands for working with Git remotes and the underlying Git repo
///
/// See this [comparison], including a [table of commands].
///
/// [comparison]:
///     https://docs.jj-vcs.dev/latest/git-comparison/.
///
/// [table of commands]:
///     https://docs.jj-vcs.dev/latest/git-command-table
#[derive(Subcommand, Clone, Debug)]
pub enum GitCommand {
    Clone(GitCloneArgs),
    #[command(subcommand)]
    Colocation(GitColocationCommand),
    Export(GitExportArgs),
    Fetch(GitFetchArgs),
    Import(GitImportArgs),
    Init(GitInitArgs),
    Push(GitPushArgs),
    #[command(subcommand)]
    Remote(RemoteCommand),
    Root(GitRootArgs),
}

pub async fn cmd_git(
    ui: &mut Ui,
    command: &CommandHelper,
    subcommand: &GitCommand,
) -> Result<(), CommandError> {
    match subcommand {
        GitCommand::Clone(args) => cmd_git_clone(ui, command, args).await,
        GitCommand::Colocation(subcommand) => cmd_git_colocation(ui, command, subcommand).await,
        GitCommand::Export(args) => cmd_git_export(ui, command, args).await,
        GitCommand::Fetch(args) => cmd_git_fetch(ui, command, args).await,
        GitCommand::Import(args) => cmd_git_import(ui, command, args).await,
        GitCommand::Init(args) => cmd_git_init(ui, command, args).await,
        GitCommand::Push(args) => cmd_git_push(ui, command, args).await,
        GitCommand::Remote(args) => cmd_git_remote(ui, command, args).await,
        GitCommand::Root(args) => cmd_git_root(ui, command, args).await,
    }
}

pub fn maybe_add_gitignore(workspace_command: &WorkspaceCommandHelper) -> Result<(), CommandError> {
    if workspace_command.working_copy_shared_with_git() {
        std::fs::write(
            workspace_command
                .workspace_root()
                .join(".jj")
                .join(".gitignore"),
            "/*\n",
        )
        .map_err(|e| user_error_with_message("Failed to write .jj/.gitignore file", e))
    } else {
        Ok(())
    }
}

const TRUNK_CONFIG_NAME: [&str; 2] = ["revset-aliases", "trunk()"];

#[derive(Clone, Copy, Debug)]
struct RepoPresets<'a> {
    remote: &'a RemoteName,
    fetch_bookmarks: Option<&'a [String]>,
    fetch_tags: Option<&'a [String]>,
    trunk: Option<RemoteRefSymbol<'a>>,
}

impl RepoPresets<'_> {
    fn is_default(self) -> bool {
        let Self {
            remote: _,
            fetch_bookmarks,
            fetch_tags,
            trunk,
        } = self;
        fetch_bookmarks.is_none() && fetch_tags.is_none() && trunk.is_none()
    }
}

/// Saves trunk and default fetch settings to repo-level config file.
fn write_repo_presets(
    ui: &Ui,
    config_env: &ConfigEnv,
    presets: RepoPresets<'_>,
) -> Result<(), CommandError> {
    if presets.is_default() {
        return Ok(()); // Don't initialize config directory
    }
    let Some(config_path) = config_env.repo_config_path(ui)? else {
        // We couldn't find the user's home directory, so we skip this step.
        return Ok(());
    };
    let mut file = ConfigFile::load_or_empty(ConfigSource::Repo, config_path)?;
    if let Some(exprs) = presets.fetch_bookmarks {
        file.set_value(
            ["remotes", presets.remote.as_str(), "fetch-bookmarks"],
            join_string_expressions(exprs),
        )
        .expect("initial repo config shouldn't have invalid values");
    }
    if let Some(exprs) = presets.fetch_tags {
        file.set_value(
            ["remotes", presets.remote.as_str(), "fetch-tags"],
            join_string_expressions(exprs),
        )
        .expect("initial repo config shouldn't have invalid values");
    }
    if let Some(symbol) = presets.trunk {
        file.set_value(TRUNK_CONFIG_NAME, symbol.to_string())
            .expect("initial repo config shouldn't have invalid values");
        writeln!(
            ui.status(),
            "Setting the revset alias `trunk()` to `{symbol}`.",
        )?;
    }
    file.save()?;
    Ok(())
}

/// Prepare an atomic, repo-local move or removal of exact remote settings tables.
/// Qualified keys are logical aliases, never the underlying physical Git handles.
pub(crate) fn prepare_remote_settings_scope(
    config: &RawConfig,
    aliases: &[(RemoteNameBuf, Option<String>)],
) -> Result<Option<ConfigFile>, CommandError> {
    let mut file = existing_repo_config_file(config);
    let mut sources = std::collections::BTreeSet::new();
    let mut destinations = std::collections::BTreeSet::new();
    for (old, new) in aliases {
        if !sources.insert(old.as_str())
            || new.as_deref().is_some_and(|new| !destinations.insert(new))
        {
            return Err(crate::command_error::user_error(
                "Remote settings changes contain duplicate names",
            ));
        }
        for name in std::iter::once(old.as_str()).chain(new.as_deref()) {
            for layer in config.as_ref().layers() {
                let item = layer.look_up_item(["remotes", name]).map_err(|_| {
                    crate::command_error::user_error("Remote settings parent must be a table")
                })?;
                if item.is_some()
                    && (layer.source != ConfigSource::Repo
                        || layer.path.as_deref() != file.as_ref().map(|file| file.path()))
                {
                    return Err(crate::command_error::user_error(format!(
                        "Remote settings for {name:?} are not owned by the repo-local config; \
                         move them there explicitly before changing scope"
                    )));
                }
                if item.is_some() && new.as_deref() == Some(name) && name != old.as_str() {
                    return Err(crate::command_error::user_error(format!(
                        "Remote settings for {name:?} already exist"
                    )));
                }
            }
        }
    }
    let mut changed = false;
    if let Some(file) = &mut file
        && let Some(remotes) = file
            .data_mut()
            .as_table_mut()
            .get_mut("remotes")
            .and_then(|item| item.as_table_like_mut())
    {
        for (old, new) in aliases {
            if let Some(item) = remotes.remove(old.as_str()) {
                changed = true;
                if let Some(new) = new {
                    remotes.insert(new, item);
                }
            }
        }
    }
    Ok(if changed { file } else { None })
}

/// Prepares preset trunk and remote settings for the remote-management transaction.
fn rename_remote_in_repo_config(
    ui: &Ui,
    config: &RawConfig,
    view: &jj_lib::view::View,
    old_remote: &RemoteName,
    new_remote: &RemoteName,
    project_labels: Option<&[String]>,
) -> Result<Option<ConfigFile>, CommandError> {
    let file = if let Some(labels) = project_labels {
        let aliases = labels
            .iter()
            .map(|label| {
                (
                    RemoteNameBuf::from(format!("{}#{label}", old_remote.as_str())),
                    Some(format!("{}#{label}", new_remote.as_str())),
                )
            })
            .collect::<Vec<_>>();
        prepare_remote_settings_scope(config, &aliases)?
            .or_else(|| existing_repo_config_file(config))
    } else {
        existing_repo_config_file(config)
    };
    let Some(mut file) = file else {
        return Ok(None);
    };

    if let Some(labels) = project_labels {
        update_remote_defaults(&mut file, Some(labels), old_remote, Some(new_remote));
        return Ok(Some(file));
    }
    // [remotes.<old_remote>] -> [remotes.<new_remote>]
    if let Some(remotes_item) = file.data_mut().as_table_mut().get_mut("remotes")
        && let Some(remotes_table) = remotes_item.as_table_like_mut()
        && let Some(old_item) = remotes_table.remove(old_remote.as_str())
    {
        remotes_table.insert(new_remote.as_str(), old_item);
    }

    update_remote_defaults(&mut file, None, old_remote, Some(new_remote));

    // trunk = <name>@<old_remote> -> <name>@<new_remote>
    if let Some(old_symbol) = get_trunk_symbol(file.layer())
        && old_symbol.remote == old_remote
        && crate::git_remote::parse_remote_selector_scope(view, old_symbol.name.as_str(), None)?
            .1
            .is_none()
    {
        let new_symbol = old_symbol.name.to_remote_symbol(new_remote);
        file.set_value(TRUNK_CONFIG_NAME, new_symbol.to_string())
            .expect("old value was string");
        writeln!(
            ui.status(),
            "Updating the revset alias `trunk()` to `{new_symbol}`.",
        )?;
    }

    Ok(Some(file))
}

/// Prepares removal of repo settings without changing their source file.
fn remove_remote_from_repo_config(
    ui: &Ui,
    config: &RawConfig,
    view: &jj_lib::view::View,
    old_remote: &RemoteName,
    project_labels: Option<&[String]>,
) -> Result<Option<ConfigFile>, CommandError> {
    let file = if let Some(labels) = project_labels {
        let aliases = labels
            .iter()
            .map(|label| {
                (
                    RemoteNameBuf::from(format!("{}#{label}", old_remote.as_str())),
                    None,
                )
            })
            .collect::<Vec<_>>();
        prepare_remote_settings_scope(config, &aliases)?
            .or_else(|| existing_repo_config_file(config))
    } else {
        existing_repo_config_file(config)
    };
    let Some(mut file) = file else {
        return Ok(None);
    };
    if let Some(labels) = project_labels {
        update_remote_defaults(&mut file, Some(labels), old_remote, None);
        return Ok(Some(file));
    }

    // [remotes.<old_remote>]
    if let Some(remotes_item) = file.data_mut().as_table_mut().get_mut("remotes")
        && let Some(remotes_table) = remotes_item.as_table_like_mut()
    {
        remotes_table.remove(old_remote.as_str());
    }

    update_remote_defaults(&mut file, None, old_remote, None);

    // trunk = <name>@<old_remote>
    if let Some(old_symbol) = get_trunk_symbol(file.layer())
        && old_symbol.remote == old_remote
        && crate::git_remote::parse_remote_selector_scope(view, old_symbol.name.as_str(), None)?
            .1
            .is_none()
    {
        file.delete_value(TRUNK_CONFIG_NAME)
            .expect("old value was string");
        writeln!(
            ui.status(),
            "Resetting the revset alias `trunk()` to default value.",
        )?;
    }

    Ok(Some(file))
}

/// Retarget exact local names only, leaving patterns and other scopes untouched.
fn update_remote_defaults(
    file: &mut ConfigFile,
    project_labels: Option<&[String]>,
    old_remote: &RemoteName,
    new_remote: Option<&RemoteName>,
) {
    let Some(git) = file
        .data_mut()
        .as_table_mut()
        .get_mut("git")
        .and_then(|item| item.as_table_like_mut())
    else {
        return;
    };
    if let Some(labels) = project_labels {
        let Some(projects) = git
            .get_mut("projects")
            .and_then(|item| item.as_table_like_mut())
        else {
            return;
        };
        for label in labels {
            if let Some(defaults) = projects
                .get_mut(label)
                .and_then(|item| item.as_table_like_mut())
            {
                update_remote_default_table(defaults, old_remote, new_remote);
            }
        }
    } else {
        update_remote_default_table(git, old_remote, new_remote);
    }
}

fn update_remote_default_table(
    defaults: &mut dyn toml_edit::TableLike,
    old_remote: &RemoteName,
    new_remote: Option<&RemoteName>,
) {
    let replace = |value: &mut toml_edit::Value| {
        let Some(text) = value.as_str() else {
            return false;
        };
        let explicit = text.strip_prefix("exact:");
        if explicit.unwrap_or(text) != old_remote.as_str() {
            return false;
        }
        let Some(new_remote) = new_remote else {
            return true;
        };
        let text = if explicit.is_some() {
            format!("exact:{}", new_remote.as_str())
        } else {
            new_remote.as_str().to_owned()
        };
        let mut replacement = toml_edit::Value::from(text);
        *replacement.decor_mut() = value.decor().clone();
        *value = replacement;
        false
    };
    for direction in ["fetch", "push"] {
        let Some(value) = defaults
            .get_mut(direction)
            .and_then(|item| item.as_value_mut())
        else {
            continue;
        };
        if let Some(values) = value.as_array_mut() {
            let mut index = 0;
            while index < values.len() {
                if replace(values.get_mut(index).expect("valid array index")) {
                    values.remove(index);
                } else {
                    index += 1;
                }
            }
        } else if replace(value) {
            defaults.remove(direction);
        }
    }
}

fn get_trunk_symbol(layer: &ConfigLayer) -> Option<RemoteRefSymbolBuf> {
    if let Ok(Some(trunk_item)) = layer.look_up_item(TRUNK_CONFIG_NAME)
        && let Some(trunk_str) = trunk_item.as_str()
        && let Ok(revset::ExpressionKind::RemoteSymbol(symbol)) =
            revset::parse_program(trunk_str).map(|node| node.kind)
    {
        Some(symbol)
    } else {
        None
    }
}

fn join_string_expressions(exprs: &[String]) -> String {
    match exprs {
        [] => "~*".to_owned(),  // no matches
        _ => exprs.join(" | "), // no parentheses since | is the weakest operator
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lower")]
enum ObjectHash {
    Sha1,
    Sha256,
}

impl From<ObjectHash> for gix::hash::Kind {
    fn from(value: ObjectHash) -> Self {
        match value {
            ObjectHash::Sha1 => Self::Sha1,
            ObjectHash::Sha256 => Self::Sha256,
        }
    }
}
