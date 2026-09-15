// Copyright 2026 The Jujutsu Authors
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

//! Remote conversion and transport boundaries for embedders of the ordinary Git CLI.
//!
//! Argument selection, tracking, import, and operation commits remain owned by jj.

use std::io::Write as _;
use std::num::NonZeroU32;
use std::pin::Pin;

use jj_lib::config::ConfigGetResultExt as _;
use jj_lib::git::GitFetchRefExpression;
use jj_lib::git::GitPushOptions;
use jj_lib::git::GitPushRefTargets;
use jj_lib::git::GitPushStats;
use jj_lib::git::GitRemoteObservation;
use jj_lib::git::IgnoredRefspecs;
use jj_lib::project::ProjectId;
use jj_lib::ref_name::GitRefNameBuf;
use jj_lib::ref_name::RefName;
use jj_lib::ref_name::RefNameBuf;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::MutableRepo;
use jj_lib::repo::Repo as _;
use jj_lib::settings::UserSettings;
use jj_lib::str_util::StringExpression;
use jj_lib::view::View;

use crate::cli_util::CommandHelper;
use crate::cli_util::WorkspaceCommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::ui::Ui;

pub type RemoteFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CommandError>> + 'a>>;

/// Prepare repo-local settings for renamed/scoped aliases.
pub fn prepare_remote_settings_scope(
    config: &crate::config::RawConfig,
    aliases: &[(RemoteNameBuf, Option<String>)],
) -> Result<Option<jj_lib::config::ConfigFile>, CommandError> {
    crate::commands::git::prepare_remote_settings_scope(config, aliases)
}

/// Reject stale loaded repo-local settings before installing a prepared rewrite.
/// Call while holding the native settings-file lock.
pub fn check_repo_config_unchanged(config: &crate::config::RawConfig) -> Result<(), CommandError> {
    if let Some(file) = crate::config::existing_repo_config_file(config)
        && std::fs::read_to_string(file.path())? != file.layer().data.to_string()
    {
        return Err(user_error(
            "Repository configuration changed while preparing a settings update; retry",
        ));
    }
    Ok(())
}

/// Atomically replace the authorized repo settings, rejecting stale loaded data.
pub fn commit_repo_config_update(
    raw_config: &crate::config::RawConfig,
    updated: &jj_lib::config::ConfigFile,
) -> Result<(), CommandError> {
    let loaded = crate::config::existing_repo_config_file(raw_config)
        .ok_or_else(|| user_error("No loaded repository configuration authorizes this update"))?;
    if loaded.path() != updated.path() {
        return Err(user_error(
            "Repository configuration update targets an unauthorized file",
        ));
    }
    let _lock = gix::lock::Marker::acquire_to_hold_resource(
        updated.path(),
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .map_err(user_error)?;
    check_repo_config_unchanged(raw_config)?;
    let parent = updated
        .path()
        .parent()
        .expect("repository configuration parent");
    let mut replacement = tempfile::NamedTempFile::new_in(parent)?;
    replacement
        .as_file()
        .set_permissions(std::fs::metadata(updated.path())?.permissions())?;
    replacement.write_all(updated.layer().data.to_string().as_bytes())?;
    replacement.as_file().sync_all()?;
    replacement.persist(updated.path()).map_err(user_error)?;
    Ok(())
}

/// One destination in a logical reference's preflighted publication route set.
#[derive(Clone, Debug)]
pub struct GitPushRoute {
    pub remote: RemoteNameBuf,
    pub name: RefNameBuf,
}

/// Resolve a user-facing remote selector without exposing physical Git handles.
pub fn resolve_remote_selector(
    workspace: &WorkspaceCommandHelper,
    selector: &str,
    project: Option<&str>,
) -> Result<RemoteNameBuf, CommandError> {
    let view = workspace.repo().view();
    let project = project
        .map(|name| view.project_state().project_by_name(name).map(|(id, _)| id))
        .transpose()
        .map_err(user_error)?;
    let candidates = jj_lib::git::get_all_remote_names(workspace.repo().store())?;
    resolve_remote_selector_in_view(view, &candidates, selector, project.as_ref())
}

/// Resolve against actual Git candidates in an already selected reference scope.
pub fn resolve_remote_selector_in_view(
    view: &View,
    candidates: &[RemoteNameBuf],
    selector: &str,
    project: Option<&ProjectId>,
) -> Result<RemoteNameBuf, CommandError> {
    let (name, scope) = parse_remote_selector_scope(view, selector, project)?;
    view.resolve_remote_name(candidates, scope.as_ref(), RemoteName::new(name))
        .map_err(user_error)
}

/// Parse a local alias and optional registered scope without requiring it to exist.
pub fn parse_remote_selector_scope<'a>(
    view: &View,
    selector: &'a str,
    project: Option<&ProjectId>,
) -> Result<(&'a str, Option<ProjectId>), CommandError> {
    if let Some(name) = selector.strip_suffix('#') {
        if project.is_some() {
            return Err(user_error(
                "Root remote selector and selected project scopes disagree",
            ));
        }
        return Ok((name, None));
    }
    if let Some((name, label)) = selector.rsplit_once('#')
        && let Some(scope) = view
            .project_state()
            .resolve_label(label)
            .map_err(user_error)?
    {
        if project.is_some_and(|project| project != &scope) {
            return Err(user_error(
                "Remote selector and selected project scopes disagree",
            ));
        }
        Ok((name, Some(scope)))
    } else {
        Ok((selector, project.cloned()))
    }
}

fn expand_remote_expression(
    view: &View,
    candidates: &[RemoteNameBuf],
    project: Option<&ProjectId>,
    expression: &StringExpression,
    allow_qualified: bool,
    eligible: &mut std::collections::BTreeSet<RemoteNameBuf>,
    missing_root_names: &mut Vec<RemoteNameBuf>,
) -> Result<StringExpression, CommandError> {
    match expression {
        StringExpression::Pattern(pattern) => {
            let (local, scope) = parse_remote_selector_scope(view, pattern.as_str(), project)?;
            if !allow_qualified && scope.as_ref() != project {
                return Err(user_error("Configured remote belongs to another scope"));
            }
            if let Some(name) = pattern.as_exact() {
                let missing_root = scope.is_none()
                    && local == pattern.as_str()
                    && !candidates.iter().any(|remote| {
                        remote.as_str() == name
                            && !matches!(view.remote_in_scope(remote, None), Ok(false))
                    });
                if missing_root {
                    missing_root_names.push(name.into());
                } else {
                    resolve_remote_selector_in_view(view, candidates, name, project)?;
                }
            }
            let qualified = local != pattern.as_str();
            let matcher = pattern.to_matcher();
            let mut matching = Vec::new();
            for remote in candidates {
                if !view
                    .remote_in_scope(remote, scope.as_ref())
                    .unwrap_or(false)
                {
                    continue;
                }
                eligible.insert(remote.clone());
                let matches = if qualified {
                    if scope.is_none() {
                        matcher.is_match(&format!("{}#", view.remote_local_name(remote).as_str()))
                    } else {
                        matcher.is_match(&view.remote_qualified_name(remote))
                    }
                } else {
                    matcher.is_match(view.remote_local_name(remote).as_str())
                };
                if matches {
                    matching.push(StringExpression::exact(remote));
                }
            }
            Ok(StringExpression::union_all(matching))
        }
        StringExpression::NotIn(inner) => Ok(expand_remote_expression(
            view,
            candidates,
            project,
            inner,
            allow_qualified,
            eligible,
            missing_root_names,
        )?
        .negated()),
        StringExpression::Union(left, right) => {
            let left = expand_remote_expression(
                view,
                candidates,
                project,
                left,
                allow_qualified,
                eligible,
                missing_root_names,
            )?;
            let right = expand_remote_expression(
                view,
                candidates,
                project,
                right,
                allow_qualified,
                eligible,
                missing_root_names,
            )?;
            Ok(left.union(right))
        }
        StringExpression::Intersection(left, right) => {
            let left = expand_remote_expression(
                view,
                candidates,
                project,
                left,
                allow_qualified,
                eligible,
                missing_root_names,
            )?;
            let right = expand_remote_expression(
                view,
                candidates,
                project,
                right,
                allow_qualified,
                eligible,
                missing_root_names,
            )?;
            Ok(left.intersection(right))
        }
    }
}

/// Mutually exclusive ways to select remotes within a scope.
#[derive(Clone, Copy)]
pub enum RemoteSelection<'a> {
    Default,
    Explicit(&'a [String]),
    All,
}

/// Select local aliases using native string/list pattern syntax in one scope.
///
/// Only explicit qualified selectors may cross out of the root scope. Project
/// defaults never read the root git.fetch/git.push settings.
pub fn select_remote_names(
    ui: &Ui,
    view: &View,
    settings: &UserSettings,
    candidates: &[RemoteNameBuf],
    project: Option<&ProjectId>,
    selection: RemoteSelection<'_>,
    direction: gix::remote::Direction,
) -> Result<Vec<RemoteNameBuf>, CommandError> {
    let direction = match direction {
        gix::remote::Direction::Fetch => "fetch",
        gix::remote::Direction::Push => "push",
    };
    let mut scoped = Vec::new();
    for remote in candidates {
        // Invalid metadata belonging to another scope is not a selected route.
        if view.remote_in_scope(remote, project).unwrap_or(false) {
            scoped.push(remote.clone());
        }
    }
    let explicit = match selection {
        RemoteSelection::Default => None,
        RemoteSelection::Explicit(texts) => Some(texts),
        RemoteSelection::All => return Ok(scoped),
    };
    let configured;
    let texts = if let Some(explicit) = explicit {
        Some(explicit)
    } else {
        let label = if let Some(project) = project {
            view.project_state()
                .validate_project(project)
                .map_err(user_error)?;
            let labels: Vec<_> = view
                .project_state()
                .labels
                .iter()
                .filter_map(|(label, value)| {
                    (value.as_resolved().and_then(Option::as_ref) == Some(project)).then_some(label)
                })
                .collect();
            let [label] = labels.as_slice() else {
                return Err(user_error(
                    "Project transport requires one unambiguous registered reference label",
                ));
            };
            Some((*label).as_str())
        } else {
            None
        };
        let key = label.map_or_else(
            || vec!["git", direction],
            |label| vec!["git", "projects", label, direction],
        );
        configured = if let Ok(values) = settings.get::<Vec<String>>(key.as_slice()) {
            Some(values)
        } else {
            settings
                .get_string(key.as_slice())
                .optional()?
                .map(|value| vec![value])
        };
        configured.as_deref()
    };
    let default_names;
    let texts = if let Some(texts) = texts {
        texts
    } else {
        if let [remote] = scoped.as_slice() {
            if view.remote_local_name(remote).as_str() != "origin" {
                writeln!(
                    ui.hint_default(),
                    "{} the only existing remote: {}",
                    if direction == "fetch" {
                        "Fetching from"
                    } else {
                        "Pushing to"
                    },
                    view.remote_qualified_name(remote)
                )?;
            }
            return Ok(scoped);
        }
        if project.is_some() {
            return view
                .resolve_remote_name(candidates, project, RemoteName::new("origin"))
                .map(|remote| vec![remote])
                .map_err(user_error);
        }
        default_names = vec!["origin".to_owned()];
        &default_names
    };
    let expression = crate::revset_util::parse_union_name_patterns(ui, texts)?;
    let mut eligible = scoped.into_iter().collect();
    let mut missing_root_names = Vec::new();
    let physical_expression = expand_remote_expression(
        view,
        candidates,
        project,
        &expression,
        explicit.is_some(),
        &mut eligible,
        &mut missing_root_names,
    )?;
    if !missing_root_names.is_empty() {
        writeln!(
            ui.warning_default(),
            "No matching remotes for names: {}",
            missing_root_names
                .iter()
                .map(|name| name.as_symbol().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    let matcher = physical_expression.to_matcher();
    let selected: Vec<_> = candidates
        .iter()
        .filter(|remote| eligible.contains(*remote) && matcher.is_match(remote.as_str()))
        .cloned()
        .collect();
    if selected.is_empty() {
        return Err(user_error(if direction == "fetch" {
            "No git remotes to fetch from"
        } else {
            "No git remotes to push to"
        }));
    }
    Ok(selected)
}

/// Resolves the authoritative configuration for a selected named remote.
pub trait GitRemoteExtension {
    /// Capabilities this provider actually implements, not merely its presence.
    fn capabilities(&self) -> &'static [&'static str] {
        &[]
    }

    fn open(
        &self,
        command: &CommandHelper,
        workspace: &WorkspaceCommandHelper,
        remote: &RemoteName,
    ) -> Result<Box<dyn GitRemoteSession>, CommandError>;

    /// Validate an immutable binding without changing local or operation state.
    fn prepare_binding(
        &self,
        _workspace: &WorkspaceCommandHelper,
        _remote: &RemoteName,
        _connection_id: &jj_lib::project::ConnectionId,
        _args: &GitRemoteBindingArgs,
    ) -> Result<jj_lib::project::BindingRecord, CommandError> {
        Err(crate::command_error::user_error(
            "Remote provider does not support bindings",
        ))
    }
}

/// Source history and one-shot endpoint selection for a named remote fetch.
#[derive(Clone, Debug, Default)]
pub struct GitRemoteFetchOptions {
    pub revisions: Vec<String>,
    pub depth: Option<NonZeroU32>,
    pub deepen: Option<NonZeroU32>,
    pub unshallow: bool,
    pub fetch_url: Option<String>,
}

/// Source-side context for a remote which reverses a history projection.
#[derive(Clone, Debug, Default)]
pub struct GitRemotePushOptions {
    pub base: Option<String>,
    pub source: Option<String>,
    pub merge: bool,
}

/// Confirmed publication results survive a later transport or local-save error.
#[derive(Debug)]
pub struct GitRemotePushOutcome {
    pub stats: GitPushStats,
    pub error: Option<CommandError>,
}

/// A fully received and converted fetch whose shared Git mirrors are not yet visible.
pub trait GitPreparedFetch {
    /// Publish canonical Git mirrors and return the observations to import into the JJ view.
    fn publish<'a>(
        self: Box<Self>,
        repo: &'a mut MutableRepo,
    ) -> RemoteFuture<'a, Vec<GitRemoteObservation>>;
}

/// Fully converted and preflighted publication, with no remote writes yet.
pub trait GitPreparedPush {
    /// Normalized endpoint and wire names, for collisions across remote aliases.
    fn destinations(&self) -> (&str, &[GitRefNameBuf]);

    /// Describes the resolved conversion, endpoint, and exact publication lease.
    fn describe(&self, ui: &mut Ui) -> Result<(), CommandError>;

    /// Records only independently confirmed publication results.
    fn publish<'a>(
        self: Box<Self>,
        repo: &'a mut MutableRepo,
    ) -> RemoteFuture<'a, GitRemotePushOutcome>;
}

/// A configured remote, before any network mutation or logical observation update.
pub trait GitRemoteSession {
    /// Converts a source ref's short name to its name in the destination view.
    fn local_name(&self, source: &str) -> RefNameBuf;

    /// Returns no source name for a logical ref outside this remote's domain.
    fn source_name<'a>(&self, local: &'a RefName) -> Option<&'a str>;

    /// Maps a logical ref to its publication name, independently of fetch scope.
    fn push_name<'a>(&self, local: &'a RefName) -> &'a str {
        local.as_str()
    }

    fn default_fetch_bookmarks(&self) -> Result<(IgnoredRefspecs, StringExpression), CommandError>;

    /// Receives and converts a complete selected snapshot, including unchanged refs.
    /// Network I/O and private provenance writes happen here, but canonical Git mirrors must not
    /// become visible until the returned preparation is published under jj's import/export lock.
    fn prepare_fetch<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        selection: GitFetchRefExpression,
        options: &'a GitRemoteFetchOptions,
    ) -> RemoteFuture<'a, Box<dyn GitPreparedFetch>>;

    /// Completes conversion, object preparation, and transport preflight before any
    /// remote refs change. Dry runs must not update observations or correspondence.
    fn prepare_push<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        targets: &'a GitPushRefTargets,
        options: &'a GitPushOptions,
        preparation: &'a GitRemotePushOptions,
        dry_run: bool,
    ) -> RemoteFuture<'a, Box<dyn GitPreparedPush>>;
}

/// Declarative conversion configuration shared by add and explicit attach.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct GitRemoteBindingArgs {
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long, group = "representation")]
    pub whole: bool,
    #[arg(long, group = "representation")]
    pub filter: Option<String>,
    #[arg(long, group = "representation")]
    pub view: Option<String>,
    #[arg(long = "like", group = "representation")]
    pub like_remote: Option<String>,
    #[arg(long)]
    pub base: Option<String>,
}

impl GitRemoteBindingArgs {
    pub fn is_managed(&self) -> bool {
        self.project.is_some()
            || self.whole
            || self.filter.is_some()
            || self.view.is_some()
            || self.like_remote.is_some()
            || self.base.is_some()
    }
}

pub fn capabilities(command: &CommandHelper) -> &'static [&'static str] {
    command
        .git_remote_extension()
        .map_or(&[], |extension| extension.capabilities())
}

pub fn check_remote(
    command: &CommandHelper,
    workspace: &WorkspaceCommandHelper,
    remote: &RemoteName,
) -> Result<(), CommandError> {
    use jj_lib::repo::Repo as _;
    jj_lib::git::check_remote_capability(
        workspace.repo().store(),
        workspace.repo().view(),
        remote,
        capabilities(command),
    )
    .map_err(crate::command_error::user_error)
}

pub fn check_selected_ref(
    store: &jj_lib::store::Store,
    view: &jj_lib::view::View,
    remote: &RemoteName,
    name: &RefName,
    capabilities: &[&str],
    source: Option<&str>,
) -> Result<(), CommandError> {
    use jj_lib::project::BindingTarget;
    jj_lib::git::check_remote_capability(store, view, remote, capabilities).map_err(user_error)?;
    let project = name
        .as_str()
        .rsplit_once('#')
        .map(|(_, label)| view.project_state().resolve_label(label))
        .transpose()
        .map_err(user_error)?
        .flatten();
    let git_repo = jj_lib::git::get_git_repo(store)?;
    let connection = jj_lib::git::remote_connection_id(&git_repo, remote).map_err(user_error)?;
    let destination = connection
        .as_ref()
        .map(|id| view.project_state().binding_for_connection(id))
        .transpose()
        .map_err(user_error)?
        .flatten();
    let selected = if let Some(source) = source {
        let candidates = jj_lib::git::get_all_remote_names(store)?;
        let source = resolve_remote_selector_in_view(view, &candidates, source, project.as_ref())?;
        jj_lib::git::check_remote_capability(store, view, &source, capabilities)
            .map_err(user_error)?;
        let id = jj_lib::git::remote_connection_id(&git_repo, &source)
            .map_err(user_error)?
            .ok_or_else(|| user_error("--source must name a bound connection"))?;
        Some(
            view.project_state()
                .binding_for_connection(&id)
                .map_err(user_error)?
                .ok_or_else(|| user_error("--source has no active binding"))?,
        )
    } else {
        None
    };
    if project.is_some() && !capabilities.contains(&"jjosh-v1") {
        return Err(user_error(format!(
            "Selected project ref {} requires capability jjosh-v1",
            name.as_symbol()
        )));
    }
    if let Some((destination_id, _)) = &destination {
        if selected
            .as_ref()
            .is_some_and(|(source_id, _)| source_id != destination_id)
        {
            return Err(user_error(
                "A bound destination must use its own immutable binding, not another --source",
            ));
        }
    }
    let binding = destination.or(selected);
    if let Some(project) = project {
        view.project_state()
            .validate_project(&project)
            .map_err(user_error)?;
        if !binding
            .is_some_and(|(_, binding)| binding.target == BindingTarget::Project(project.clone()))
        {
            return Err(user_error(format!(
                "Project ref {} needs a matching destination binding or explicit --source",
                name.as_symbol()
            )));
        }
    } else if binding
        .is_some_and(|(_, binding)| matches!(binding.target, BindingTarget::Project(_)))
    {
        return Err(user_error(format!(
            "Unscoped ref {} cannot be published through a project binding",
            name.as_symbol()
        )));
    }
    Ok(())
}
