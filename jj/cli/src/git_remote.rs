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

use std::pin::Pin;

use jj_lib::git::{
    GitFetchRefExpression, GitPushOptions, GitPushRefTargets, GitPushStats, GitRemoteObservation,
    IgnoredRefspecs,
};
use jj_lib::ref_name::{GitRefNameBuf, RefName, RefNameBuf, RemoteName, RemoteNameBuf};
use jj_lib::repo::MutableRepo;
use jj_lib::str_util::StringExpression;

use crate::cli_util::{CommandHelper, WorkspaceCommandHelper};
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
    fn open(
        &self,
        command: &CommandHelper,
        workspace: &WorkspaceCommandHelper,
        remote: &RemoteName,
    ) -> Result<Box<dyn GitRemoteSession>, CommandError>;

    /// Prepare name-keyed metadata and declare the remote keys owned by this
    /// extension. This must not mutate repository state.
    fn prepare_remote_management(
        &self,
        _workspace: &WorkspaceCommandHelper,
        _old: &RemoteName,
        _new: Option<&RemoteName>,
    ) -> Result<jj_lib::git::GitRemoteManagementOptions, CommandError> {
        Ok(Default::default())
    }

    /// Used only when neither `--remote` nor `git.push` selects a destination.
    fn default_push_router(
        &self,
        _workspace: &WorkspaceCommandHelper,
    ) -> Result<Option<Box<dyn GitPushRouter>>, CommandError> {
        Ok(None)
    }
}

/// Source-side context for a remote which reverses a history projection.
#[derive(Clone, Debug, Default)]
pub struct GitRemotePushOptions {
    pub base: Option<String>,
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
