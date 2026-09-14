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

use std::num::NonZeroU32;
use std::pin::Pin;

use jj_lib::git::GitFetchRefExpression;
use jj_lib::git::GitPushOptions;
use jj_lib::git::GitPushRefTargets;
use jj_lib::git::GitPushStats;
use jj_lib::git::GitRemoteObservation;
use jj_lib::git::IgnoredRefspecs;
use jj_lib::ref_name::GitRefNameBuf;
use jj_lib::ref_name::RefName;
use jj_lib::ref_name::RefNameBuf;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::MutableRepo;
use jj_lib::str_util::StringExpression;

use crate::cli_util::CommandHelper;
use crate::cli_util::WorkspaceCommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

pub type RemoteFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CommandError>> + 'a>>;

/// A logical ref's default destination remote and unqualified publication name.
#[derive(Clone, Debug)]
pub struct GitPushRoute {
    pub remote: RemoteNameBuf,
    pub name: RefNameBuf,
}

/// Resolves only selected refs; unrelated ambiguous or missing routes are harmless.
pub trait GitPushRouter {
    /// `None` delegates an unscoped ref to jj's ordinary default-remote rules.
    fn route(&self, name: &RefName) -> Result<Option<GitPushRoute>, CommandError>;
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
        Err(crate::command_error::user_error("Remote provider does not support bindings"))
    }

    /// Used only when neither `--remote` nor `git.push` selects a destination.
    fn default_push_router(
        &self,
        _workspace: &WorkspaceCommandHelper,
    ) -> Result<Option<Box<dyn GitPushRouter>>, CommandError> {
        Ok(None)
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

/// Fully converted and preflighted publication, with no remote writes yet.
pub trait GitPreparedPush {
    /// Normalized endpoint and wire names, for collisions across remote aliases.
    fn destinations(&self) -> (&str, &[GitRefNameBuf]);

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
    /// Install only canonical Git mirrors; return no raw objects as observations.
    fn fetch<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        selection: GitFetchRefExpression,
        options: &'a GitRemoteFetchOptions,
    ) -> RemoteFuture<'a, Vec<GitRemoteObservation>>;

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
    #[arg(long, conflicts_with = "writable")]
    pub read_only: bool,
    #[arg(long)]
    pub writable: bool,
}

impl GitRemoteBindingArgs {
    pub fn is_managed(&self) -> bool {
        self.project.is_some() || self.whole || self.filter.is_some()
            || self.view.is_some() || self.like_remote.is_some() || self.base.is_some()
            || self.read_only || self.writable
    }
}

pub fn capabilities(command: &CommandHelper) -> &'static [&'static str] {
    command.git_remote_extension().map_or(&[], |extension| extension.capabilities())
}

pub fn check_remote(command: &CommandHelper, workspace: &WorkspaceCommandHelper, remote: &RemoteName) -> Result<(), CommandError> {
    use jj_lib::repo::Repo as _;
    jj_lib::git::check_remote_capability(workspace.repo().store(), workspace.repo().view(), remote, capabilities(command))
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
    use crate::command_error::user_error;
    use jj_lib::project::BindingTarget;
    jj_lib::git::check_remote_capability(store, view, remote, capabilities).map_err(user_error)?;
    let project = name.as_str().rsplit_once('#')
        .map(|(_, label)| view.project_state().resolve_label(label)).transpose().map_err(user_error)?.flatten();
    let git_repo = jj_lib::git::get_git_repo(store)?;
    let connection = jj_lib::git::remote_connection_id(&git_repo, remote).map_err(user_error)?;
    let destination = connection.as_ref().map(|id| view.project_state().binding_for_connection(id)).transpose().map_err(user_error)?.flatten();
    let selected = if let Some(source) = source {
        let source = RemoteName::new(source);
        jj_lib::git::check_remote_capability(store, view, source, capabilities).map_err(user_error)?;
        let id = jj_lib::git::remote_connection_id(&git_repo, source).map_err(user_error)?
            .ok_or_else(|| user_error("--source must name a bound connection"))?;
        Some(view.project_state().binding_for_connection(&id).map_err(user_error)?
            .ok_or_else(|| user_error("--source has no active binding"))?)
    } else { None };
    if project.is_some() && !capabilities.contains(&"jjosh-v1") {
        return Err(user_error(format!("Selected project ref {} requires capability jjosh-v1", name.as_symbol())));
    }
    if let Some((destination_id, _)) = &destination {
        if selected.as_ref().is_some_and(|(source_id, _)| source_id != destination_id) {
            return Err(user_error("A bound destination must use its own immutable binding, not another --source"));
        }
    }
    let binding = destination.or(selected);
    if let Some(project) = project {
        view.project_state().validate_project(&project).map_err(user_error)?;
        if !binding.is_some_and(|(_, binding)| binding.target == BindingTarget::Project(project.clone())) {
            return Err(user_error(format!("Project ref {} needs a matching destination binding or explicit --source", name.as_symbol())));
        }
    } else if binding.is_some_and(|(_, binding)| matches!(binding.target, BindingTarget::Project(_))) {
        return Err(user_error(format!("Unscoped ref {} cannot be published through a project binding", name.as_symbol())));
    }
    Ok(())
}
