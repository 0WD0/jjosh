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

mod add;
mod list;
mod remove;
mod rename;
mod set_url;

use clap::Subcommand;
use jj_lib::git;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::BindingId;
use jj_lib::project::BindingRecord;
use jj_lib::project::BindingTarget;
use jj_lib::project::ConnectionId;
use jj_lib::project::ProjectId;
use jj_lib::project::ScopedRemoteName;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;

use self::add::GitRemoteAddArgs;
use self::add::cmd_git_remote_add;
use self::list::GitRemoteListArgs;
use self::list::cmd_git_remote_list;
use self::remove::GitRemoteRemoveArgs;
use self::remove::cmd_git_remote_remove;
use self::rename::GitRemoteRenameArgs;
use self::rename::cmd_git_remote_rename;
use self::set_url::GitRemoteSetUrlArgs;
use self::set_url::cmd_git_remote_set_url;
use crate::cli_util::CommandHelper;
use crate::cli_util::WorkspaceCommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::git_remote::GitRemoteBindingArgs;
use crate::ui::Ui;

/// Manage Git remotes
///
/// The Git repo will be a bare git repo stored inside the `.jj/` directory.
#[derive(Subcommand, Clone, Debug)]
pub enum RemoteCommand {
    Add(GitRemoteAddArgs),
    Attach(GitRemoteAttachArgs),
    List(GitRemoteListArgs),
    Remove(GitRemoteRemoveArgs),
    Rename(GitRemoteRenameArgs),
    SetUrl(GitRemoteSetUrlArgs),
}

pub async fn cmd_git_remote(
    ui: &mut Ui,
    command: &CommandHelper,
    subcommand: &RemoteCommand,
) -> Result<(), CommandError> {
    match subcommand {
        RemoteCommand::Add(args) => cmd_git_remote_add(ui, command, args).await,
        RemoteCommand::Attach(args) => cmd_attach(ui, command, args).await,
        RemoteCommand::List(args) => cmd_git_remote_list(ui, command, args).await,
        RemoteCommand::Remove(args) => cmd_git_remote_remove(ui, command, args).await,
        RemoteCommand::Rename(args) => cmd_git_remote_rename(ui, command, args).await,
        RemoteCommand::SetUrl(args) => cmd_git_remote_set_url(ui, command, args).await,
    }
}

/// Explicitly bind an existing, unused ordinary connection.
#[derive(clap::Args, Clone, Debug)]
pub struct GitRemoteAttachArgs {
    remote: RemoteNameBuf,
    #[command(flatten)]
    binding: GitRemoteBindingArgs,
}

/// Parse a new logical name without accepting physical handles as aliases.
fn new_remote_name(
    workspace: &WorkspaceCommandHelper,
    name: &RemoteName,
    project: Option<&str>,
) -> Result<(RemoteNameBuf, Option<ProjectId>), CommandError> {
    let state = workspace.repo().view().project_state();
    let requested_scope = project
        .map(|name| state.project_by_name(name).map(|(id, _)| id))
        .transpose()
        .map_err(user_error)?;
    let (local, scope) = crate::git_remote::parse_remote_selector_scope(
        workspace.repo().view(),
        name.as_str(),
        requested_scope.as_ref(),
    )?;
    let local = RemoteNameBuf::from(local);
    git::validate_remote_name(&local).map_err(user_error)?;
    Ok((local, scope))
}

fn ensure_available_remote_name(
    workspace: &WorkspaceCommandHelper,
    name: &RemoteName,
    project: Option<&ProjectId>,
) -> Result<(), CommandError> {
    let view = workspace.repo().view();
    if project.is_some() && name.as_str().contains('#') {
        return Err(user_error(
            "Scoped remote names cannot contain the reserved '#' qualifier",
        ));
    }
    if project.is_some()
        && view.project_state().remote_names.values().any(|value| {
            value
                .iter()
                .flatten()
                .any(|identity| Some(&identity.project) == project && identity.name == name)
        })
    {
        return Err(user_error(format!(
            "Remote {} already exists in this scope",
            name.as_symbol()
        )));
    }
    if project.is_none()
        && git::get_git_repo(workspace.repo().store())?
            .remote_names()
            .iter()
            .any(|configured| configured == name.as_str())
    {
        return Err(user_error(format!(
            "Remote {} already has local configuration",
            name.as_symbol(),
        )));
    }
    for remote in view.store_view().remote_connections.keys() {
        match jj_lib::view::remote_identity::resolve(view.store_view(), remote) {
            Ok(identity)
                if identity
                    .and_then(|identity| identity.scoped_name)
                    .map(|identity| &identity.project)
                    == project
                    && identity
                        .and_then(|identity| identity.scoped_name)
                        .map_or(remote.as_str(), |identity| identity.name.as_str())
                        == name =>
            {
                return Err(user_error(format!(
                    "Remote {} already exists in this scope",
                    name.as_symbol()
                )));
            }
            Err(error) => {
                // Unrelated unresolved projects must not fence healthy root work.
                let relevant = view.project_state().remote_names.values().any(|value| {
                    value
                        .iter()
                        .flatten()
                        .any(|identity| Some(&identity.project) == project && identity.name == name)
                });
                if relevant || remote == name {
                    return Err(user_error(error));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Management selects current logical identities, including disconnected ones,
/// but never resurrects a historical-only observation as a live remote.
fn resolve_management_remote(
    workspace: &WorkspaceCommandHelper,
    selector: &str,
    project: Option<&str>,
) -> Result<RemoteNameBuf, CommandError> {
    let (name, project) = management_remote_name(workspace, RemoteName::new(selector), project)?;
    let view = workspace.repo().view();
    let mut candidates: Vec<RemoteNameBuf> = git::get_git_repo(workspace.repo().store())?
        .remote_names()
        .into_iter()
        .filter_map(|name| String::from_utf8(name.into()).ok())
        .map(RemoteNameBuf::from)
        .collect();
    candidates.extend(
        view.store_view()
            .remote_connections
            .iter()
            .filter(|(_, owner)| owner.as_resolved() != Some(&None))
            .map(|(remote, _)| remote.clone()),
    );
    candidates.sort();
    candidates.dedup();
    let mut found = None;
    for remote in candidates {
        let identity = jj_lib::view::remote_identity::resolve(view.store_view(), &remote);
        let identity = match identity {
            Ok(identity) => identity,
            Err(error) => {
                let relevant = remote == name
                    || view
                        .store_view()
                        .remote_connections
                        .get(&remote)
                        .is_some_and(|owners| {
                            owners.iter().flatten().any(|owner| {
                                view.project_state()
                                    .remote_names
                                    .get(owner)
                                    .is_some_and(|names| {
                                        names.iter().flatten().any(|identity| {
                                            Some(&identity.project) == project.as_ref()
                                                && identity.name == name
                                        })
                                    })
                            })
                        });
                if relevant {
                    return Err(user_error(error));
                }
                continue;
            }
        };
        // Historical-only observations never become a live management alias.
        let scoped = identity
            .filter(|identity| {
                identity.source == jj_lib::view::remote_identity::IdentitySource::Logical
            })
            .and_then(|identity| identity.scoped_name);
        let local = scoped.map_or(remote.as_str(), |identity| identity.name.as_str());
        if scoped.map(|identity| &identity.project) != project.as_ref() || local != name {
            continue;
        }
        if found.is_some() {
            return Err(user_error(
                "Remote name is duplicated in the selected scope",
            ));
        }
        found = Some(remote);
    }
    found.ok_or_else(|| git::GitRemoteManagementError::NoSuchRemote(name).into())
}

/// Resolve only the scope's recorded identity, not its transport or project health.
fn management_remote_name(
    workspace: &WorkspaceCommandHelper,
    name: &RemoteName,
    project: Option<&str>,
) -> Result<(RemoteNameBuf, Option<ProjectId>), CommandError> {
    let state = workspace.repo().view().project_state();
    let mut scope = project
        .map(|name| {
            let mut matches = state
                .projects
                .iter()
                .filter(|(_, records)| records.iter().flatten().any(|record| record.name == name));
            let (id, _) = matches
                .next()
                .ok_or_else(|| user_error(format!("No project named {name:?}")))?;
            if matches.next().is_some() {
                return Err(user_error(format!("Project name {name:?} is conflicted")));
            }
            Ok(id.clone())
        })
        .transpose()?;
    let local = if let Some(local) = name.as_str().strip_suffix('#') {
        if scope.is_some() {
            return Err(user_error(
                "Root remote selector and selected project scopes disagree",
            ));
        }
        local
    } else if let Some((local, label)) = name.as_str().rsplit_once('#')
        && let Some(target) = state.labels.get(label)
    {
        let target = target
            .as_resolved()
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                user_error(format!(
                    "Project label {label:?} is unavailable or unresolved"
                ))
            })?;
        if scope.as_ref().is_some_and(|scope| scope != target) {
            return Err(user_error(
                "Remote selector and selected project scopes disagree",
            ));
        }
        scope = Some(target.clone());
        local
    } else {
        name.as_str()
    };
    let local = RemoteNameBuf::from(local);
    Ok((local, scope))
}

fn binding_scope(binding: &BindingRecord) -> Option<&ProjectId> {
    match &binding.target {
        BindingTarget::Project(project) => Some(project),
        BindingTarget::RepositoryView => None,
    }
}

fn project_config_labels(view: &jj_lib::view::View, project: &ProjectId) -> Vec<String> {
    view.project_state()
        .labels
        .iter()
        .filter(|(_, target)| target.as_resolved().and_then(Option::as_ref) == Some(project))
        .map(|(label, _)| label.clone())
        .collect()
}

fn scoped_remote_name(
    view: &jj_lib::view::View,
    local: &RemoteName,
    project: &ProjectId,
) -> Result<RemoteNameBuf, CommandError> {
    let labels = project_config_labels(view, project);
    let [label] = labels.as_slice() else {
        return Err(user_error(
            "Project remotes require exactly one registered stable label",
        ));
    };
    let remote = RemoteNameBuf::from(format!("{}#{label}", local.as_str()));
    git::validate_remote_name(&remote).map_err(user_error)?;
    Ok(remote)
}

fn prepare_binding(
    command: &CommandHelper,
    workspace: &WorkspaceCommandHelper,
    remote: &RemoteName,
    connection: &ConnectionId,
    args: &GitRemoteBindingArgs,
) -> Result<Option<BindingRecord>, CommandError> {
    if !args.is_managed() {
        return Ok(None);
    }
    if !crate::git_remote::capabilities(command).contains(&"jjosh-v1") {
        return Err(user_error(
            "Conversion arguments require capability jjosh-v1",
        ));
    }
    command
        .git_remote_extension()
        .unwrap()
        .prepare_binding(workspace, remote, connection, args)
        .map(Some)
}

async fn cmd_attach(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &GitRemoteAttachArgs,
) -> Result<(), CommandError> {
    let mut workspace = command.workspace_helper_no_snapshot(ui).await?;
    let (local_name, requested_scope) =
        new_remote_name(&workspace, &args.remote, args.binding.project.as_deref())?;
    let remote = resolve_management_remote(&workspace, local_name.as_str(), None)?;
    let mut binding_args = args.binding.clone();
    if let Some(project) = &requested_scope {
        binding_args.project = Some(
            workspace.repo().view().project_state().projects[project]
                .as_resolved()
                .and_then(Option::as_ref)
                .expect("validated project")
                .name
                .clone(),
        );
    }
    let git_lock = workspace.lock_git_import_export()?;
    let mut git_repo = git::get_git_repo(workspace.repo().store())?;
    git_repo.reload().map_err(user_error)?;
    let inspection = git::inspect_remote_management(workspace.repo().view(), &git_repo, &remote)?;
    if !inspection.has_config {
        return Err(git::GitRemoteManagementError::NoSuchRemote(local_name.clone()).into());
    }
    if !inspection.owns_config() {
        return Err(user_error(
            "Remote configuration is owned by another connection",
        ));
    }
    if git::remote_required_capability(&git_repo, &remote).is_some_and(|value| value != "jjosh-v1")
    {
        return Err(user_error(
            "Remote requires a different conversion capability",
        ));
    }
    let connection = git::remote_connection_id(&git_repo, &remote)
        .map_err(user_error)?
        .unwrap_or_else(ConnectionId::generate);
    git::check_remote_owner(workspace.repo().view(), &remote, Some(&connection))
        .map_err(user_error)?;
    if workspace
        .repo()
        .view()
        .project_state()
        .bindings
        .values()
        .any(|bindings| {
            bindings
                .iter()
                .flatten()
                .any(|binding| binding.connection_id == connection)
        })
    {
        return Err(user_error("Connection already has an active binding"));
    }
    if workspace
        .repo()
        .view()
        .all_remote_bookmarks()
        .any(|(symbol, _)| symbol.remote == remote)
        || workspace
            .repo()
            .view()
            .all_remote_tags()
            .any(|(symbol, _)| symbol.remote == remote)
        || workspace
            .repo()
            .view()
            .store_view()
            .project_observations
            .keys()
            .any(|key| key.remote == remote)
        || workspace
            .repo()
            .view()
            .store_view()
            .observed_remote_connections
            .contains_key(&remote)
    {
        return Err(user_error(
            "Attach requires an unused remote; deliberately remove and recreate the remote with \
             the desired representation, or select the representation matching its observations",
        ));
    }
    let binding = prepare_binding(command, &workspace, &remote, &connection, &binding_args)?
        .ok_or_else(|| {
            user_error(
                "Attach requires an explicit representation (--whole, --filter, --view, or --like)",
            )
        })?;
    let scope = binding_scope(&binding).cloned();
    if let Some(project) = &scope {
        ensure_available_remote_name(&workspace, &local_name, Some(project))?;
    }
    let old_remote = remote;
    let remote = match &scope {
        Some(project) => scoped_remote_name(workspace.repo().view(), &local_name, project)?,
        None => old_remote.clone(),
    };
    let options = git::GitRemoteManagementOptions {
        extra_config_keys: git::MANAGED_REMOTE_KEYS,
        expected_connection: Some(Some(connection.clone())),
        ..Default::default()
    };
    let repo_config = if let Some(project) = &scope {
        let labels = project_config_labels(workspace.repo().view(), project);
        let label = labels
            .first()
            .ok_or_else(|| user_error("Project has no registered stable label"))?;
        super::prepare_remote_settings_scope(
            command.raw_config(),
            &[(
                old_remote.clone(),
                Some(format!("{}#{label}", local_name.as_str())),
            )],
        )?
    } else {
        None
    };
    let mut tx = workspace.start_transaction();
    // Mark conversion before a physical rename: interruption must never expose
    // this connection as an ordinary raw transport.
    git::set_remote_config_keys(
        tx.repo().store(),
        &[
            (
                old_remote.clone(),
                "jjosh-connectionId".into(),
                Some(connection.hex()),
            ),
            (
                old_remote.clone(),
                "jjosh-requiredCapability".into(),
                Some("jjosh-v1".into()),
            ),
        ],
        &git_repo.config_snapshot(),
    )?;
    if remote != old_remote {
        git::rename_remote_with_options(tx.repo_mut(), &old_remote, &remote, &options)?;
    }
    if let Some(project) = scope {
        tx.repo_mut()
            .view_mut()
            .project_state_mut()
            .remote_names
            .insert(
                connection.clone(),
                Merge::resolved(Some(ScopedRemoteName {
                    project,
                    name: local_name,
                })),
            );
    }
    tx.repo_mut()
        .view_mut()
        .project_state_mut()
        .bindings
        .insert(BindingId::generate(), Merge::resolved(Some(binding)));
    tx.repo_mut()
        .view_mut()
        .store_view_mut()
        .remote_connections
        .insert(remote.clone(), Merge::resolved(Some(connection)));
    let display_name = tx.repo().view().remote_qualified_name(&remote);
    tx.finish_with_git_import_export_lock(
        ui,
        format!("attach git remote {display_name}"),
        &git_lock,
    )
    .await?;
    if command.should_commit_transaction()
        && let Some(updated) = repo_config
    {
        crate::git_remote::commit_repo_config_update(command.raw_config(), &updated).map_err(
            |err| err.hinted("Remote attached, but repository settings were not updated"),
        )?;
    }
    Ok(())
}
