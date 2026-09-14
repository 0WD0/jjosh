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

mod abandon;
mod diff;
mod integrate;
mod log;
mod restore;
mod revert;
mod show;

use abandon::OperationAbandonArgs;
use abandon::cmd_op_abandon;
use clap::Subcommand;
use diff::OperationDiffArgs;
use diff::cmd_op_diff;
use integrate::OperationIntegrateArgs;
use integrate::cmd_op_integrate;
use log::OperationLogArgs;
use log::cmd_op_log;
use restore::OperationRestoreArgs;
use restore::cmd_op_restore;
use revert::OperationRevertArgs;
use revert::cmd_op_revert;
use show::OperationShowArgs;
use show::cmd_op_show;

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Commands for working with the operation log
///
/// See the [operation log documentation] for more information.
///
/// [operation log documentation]:
///     https://docs.jj-vcs.dev/latest/operation-log/
#[derive(Subcommand, Clone, Debug)]
pub enum OperationCommand {
    Abandon(OperationAbandonArgs),
    Diff(OperationDiffArgs),
    Integrate(OperationIntegrateArgs),
    Log(OperationLogArgs),
    Restore(OperationRestoreArgs),
    Revert(OperationRevertArgs),
    Show(OperationShowArgs),
}

pub async fn cmd_operation(
    ui: &mut Ui,
    command: &CommandHelper,
    subcommand: &OperationCommand,
) -> Result<(), CommandError> {
    match subcommand {
        OperationCommand::Abandon(args) => cmd_op_abandon(ui, command, args).await,
        OperationCommand::Diff(args) => cmd_op_diff(ui, command, args).await,
        OperationCommand::Integrate(args) => cmd_op_integrate(ui, command, args).await,
        OperationCommand::Log(args) => cmd_op_log(ui, command, args).await,
        OperationCommand::Restore(args) => cmd_op_restore(ui, command, args).await,
        OperationCommand::Revert(args) => cmd_op_revert(ui, command, args).await,
        OperationCommand::Show(args) => cmd_op_show(ui, command, args).await,
    }
}

// pub for `jj undo`
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub(crate) enum RevertWhatToRestore {
    /// The jj repo state, local references, projects, bindings and logical remotes
    Repo,
    /// Observed remote references, tracking and historical identities, not the
    /// current logical remote namespace or local connection configuration.
    /// Do not restore these if you'd like to push after the undo
    RemoteTracking,
}

// pub for `jj undo`
pub(crate) const DEFAULT_REVERT_WHAT: [RevertWhatToRestore; 2] = [
    RevertWhatToRestore::Repo,
    RevertWhatToRestore::RemoteTracking,
];

/// Restore only the portions of the view specified by the `what` argument
pub(crate) fn view_with_desired_portions_restored(
    view_being_restored: &jj_lib::op_store::View,
    current_view: &jj_lib::op_store::View,
    what: &[RevertWhatToRestore],
) -> Result<jj_lib::op_store::View, String> {
    let repo_source = if what.contains(&RevertWhatToRestore::Repo) {
        view_being_restored
    } else {
        current_view
    };
    let remote_source = if what.contains(&RevertWhatToRestore::RemoteTracking) {
        view_being_restored
    } else {
        current_view
    };
    let mut history = jj_lib::view::View::new(remote_source.clone(), false);
    // A physical root remote can be renamed while retaining its connection.
    // Match identity (and the same binding), never an alias or endpoint.
    let mut current_keys = std::collections::BTreeMap::new();
    for (remote, owners) in &repo_source.remote_connections {
        if let Some(Some(connection)) = owners.as_resolved()
            && remote_source
                .project_state
                .binding_for_connection(connection)?
                == repo_source
                    .project_state
                    .binding_for_connection(connection)?
        {
            current_keys
                .entry(connection)
                .and_modify(|key| *key = None)
                .or_insert(Some(remote));
        }
    }
    let remotes: std::collections::BTreeSet<_> = remote_source
        .remote_views
        .keys()
        .chain(
            remote_source
                .project_observations
                .keys()
                .map(|key| &key.remote),
        )
        .chain(remote_source.observed_remote_connections.keys())
        .cloned()
        .collect();
    for remote in remotes {
        if !history
            .store_view()
            .observed_remote_connections
            .contains_key(&remote)
        {
            history.capture_remote_observation_identity(&remote);
        }
        if !history
            .store_view()
            .observed_remote_connections
            .contains_key(&remote)
            && current_view.remote_connections.contains_key(&remote)
        {
            // An old name-only snapshot cannot prove that it belongs to the
            // current incarnation, even when restoring the whole old repo.
            history.archive_remote_observations(&remote)?;
            continue;
        }
        if let Some(owner) = history
            .store_view()
            .observed_remote_connections
            .get(&remote)
            && (repo_source
                .remote_connections
                .get(&remote)
                .is_some_and(|current| current != owner)
                || owner
                    .as_resolved()
                    .and_then(Option::as_ref)
                    .and_then(|connection| current_keys.get(connection).copied().flatten())
                    .is_some_and(|current| current != &remote))
        {
            history.archive_remote_observations(&remote)?;
        }
    }
    // Stage moves through identity-backed keys first, so renamed/swapped root
    // aliases cannot overwrite another instance's observation snapshot.
    let relocations: Vec<_> = history
        .store_view()
        .observed_remote_connections
        .iter()
        .filter_map(|(remote, owners)| {
            let connection = owners.as_resolved()?.as_ref()?;
            let current = current_keys.get(connection).copied().flatten()?;
            (remote != current).then(|| (remote.clone(), current.clone()))
        })
        .collect();
    for (old, new) in relocations {
        history.relocate_remote_observations(&old, &new)?;
    }
    for (remote, observed) in &history.store_view().observed_remote_connections {
        if repo_source
            .remote_connections
            .get(remote)
            .is_some_and(|logical| logical != observed)
        {
            return Err(format!(
                "Historical remote {remote:?} collides with a different logical connection"
            ));
        }
    }
    let history = history.store_view_mut();
    Ok(jj_lib::op_store::View {
        head_ids: repo_source.head_ids.clone(),
        local_bookmarks: repo_source.local_bookmarks.clone(),
        local_tags: repo_source.local_tags.clone(),
        remote_views: std::mem::take(&mut history.remote_views),
        git_refs: current_view.git_refs.clone(),
        git_heads: current_view.git_heads.clone(),
        wc_commit_ids: repo_source.wc_commit_ids.clone(),
        wc_sparse_patterns: repo_source.wc_sparse_patterns.clone(),
        project_state: repo_source.project_state.clone(),
        remote_connections: repo_source.remote_connections.clone(),
        observed_remote_connections: std::mem::take(&mut history.observed_remote_connections),
        observed_remote_names: std::mem::take(&mut history.observed_remote_names),
        project_observations: std::mem::take(&mut history.project_observations),
    })
}

#[cfg(test)]
mod tests {
    use jj_lib::backend::CommitId;
    use jj_lib::merge::Merge;
    use jj_lib::op_store::{RefTarget, RemoteRef, RemoteRefState, View};
    use jj_lib::project::ConnectionId;
    use jj_lib::ref_name::RemoteNameBuf;

    use super::*;

    #[test]
    fn tracking_restore_archives_old_root_owner_without_replacing_current_membership() {
        let mut old = View::make_root(CommitId::from_hex("00"));
        let a = ConnectionId::generate();
        let b = ConnectionId::generate();
        old.remote_connections
            .insert("origin".into(), Merge::normal(a.clone()));
        let target = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("11")),
            state: RemoteRefState::Tracked,
        };
        old.remote_views
            .entry("origin".into())
            .or_default()
            .bookmarks
            .insert("main".into(), target.clone());
        let mut current = View::make_root(CommitId::from_hex("00"));
        current
            .remote_connections
            .insert("origin".into(), Merge::normal(b.clone()));
        current.git_refs.insert(
            "refs/remotes/origin/main".into(),
            RefTarget::normal(CommitId::from_hex("22")),
        );
        let restored = view_with_desired_portions_restored(
            &old,
            &current,
            &[RevertWhatToRestore::RemoteTracking],
        )
        .unwrap();
        assert_eq!(
            restored.remote_connections[jj_lib::ref_name::RemoteName::new("origin")],
            Merge::normal(b)
        );
        assert_eq!(restored.git_refs, current.git_refs);
        let archived: RemoteNameBuf = format!("jjosh-observed-{a}").into();
        assert_eq!(
            restored.remote_views[&archived].bookmarks[jj_lib::ref_name::RefName::new("main")],
            target
        );
        assert!(
            !restored
                .remote_views
                .contains_key(jj_lib::ref_name::RemoteName::new("origin"))
        );
        let complete =
            view_with_desired_portions_restored(&old, &current, &DEFAULT_REVERT_WHAT).unwrap();
        assert_eq!(complete.remote_connections, old.remote_connections);
        assert_eq!(complete.git_refs, current.git_refs);
        assert_eq!(complete.remote_views, old.remote_views);
    }

    #[test]
    fn restore_never_assigns_name_only_history_to_a_known_current_instance() {
        let mut old = View::make_root(CommitId::from_hex("00"));
        let target = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("11")),
            state: RemoteRefState::Tracked,
        };
        old.remote_views
            .entry("origin".into())
            .or_default()
            .bookmarks
            .insert("main".into(), target.clone());
        let mut current = View::make_root(CommitId::from_hex("00"));
        let current_owner = Merge::normal(ConnectionId::generate());
        current
            .remote_connections
            .insert("origin".into(), current_owner.clone());
        for what in [
            &[RevertWhatToRestore::RemoteTracking][..],
            &DEFAULT_REVERT_WHAT,
        ] {
            let restored = view_with_desired_portions_restored(&old, &current, what).unwrap();
            assert!(
                !restored
                    .remote_views
                    .contains_key(jj_lib::ref_name::RemoteName::new("origin"))
            );
            let (historical_name, historical_refs) = restored.remote_views.iter().next().unwrap();
            assert_eq!(
                historical_refs.bookmarks[jj_lib::ref_name::RefName::new("main")],
                target
            );
            let historical_owner = &restored.observed_remote_connections[historical_name];
            assert!(historical_owner.as_resolved().unwrap().is_some());
            assert_ne!(historical_owner, &current_owner);
        }
    }
}
