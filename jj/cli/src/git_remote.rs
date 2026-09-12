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

use jj_lib::git::{GitFetchRefExpression, GitPushOptions, GitPushRefTargets, GitPushStats, GitRemoteObservation, IgnoredRefspecs};
use jj_lib::ref_name::{RefName, RefNameBuf, RemoteName};
use jj_lib::repo::MutableRepo;
use jj_lib::str_util::StringExpression;

use crate::cli_util::{CommandHelper, WorkspaceCommandHelper};
use crate::command_error::CommandError;
use crate::ui::Ui;

pub type RemoteFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CommandError>> + 'a>>;

/// Resolves the authoritative configuration for a selected named remote.
pub trait GitRemoteExtension {
    fn open(
        &self,
        command: &CommandHelper,
        workspace: &WorkspaceCommandHelper,
        remote: &RemoteName,
    ) -> Result<Box<dyn GitRemoteSession>, CommandError>;
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

/// A configured remote, before any network mutation or logical observation update.
pub trait GitRemoteSession {
    /// Converts a source ref's short name to its name in the destination view.
    fn local_name(&self, source: &str) -> RefNameBuf;

    /// Returns no source name for a logical ref outside this remote's domain.
    fn source_name<'a>(&self, local: &'a RefName) -> Option<&'a str>;

    fn default_fetch_bookmarks(
        &self,
    ) -> Result<(IgnoredRefspecs, StringExpression), CommandError>;

    /// Receives and converts a complete selected snapshot, including unchanged refs.
    /// Install only canonical Git mirrors; return no raw objects as observations.
    fn fetch<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        selection: GitFetchRefExpression,
    ) -> RemoteFuture<'a, Vec<GitRemoteObservation>>;

    /// Publishes selected canonical targets and records only confirmed successes.
    /// A dry run must not mutate remote refs, observations, or correspondence state.
    fn push<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        targets: &'a GitPushRefTargets,
        options: &'a GitPushOptions,
        preparation: &'a GitRemotePushOptions,
        dry_run: bool,
    ) -> RemoteFuture<'a, GitRemotePushOutcome>;
}
