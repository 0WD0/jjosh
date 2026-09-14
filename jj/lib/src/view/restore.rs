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

//! Operation restore composition and historical identity reconciliation.

use std::collections::{BTreeMap, BTreeSet};

use super::remote_observations::RemoteObservations;
use crate::merge::Merge;
use crate::op_store;
use crate::project::ConnectionId;
use crate::project::{BindingId, BindingRecord, ProjectState};

/// Compare immutable candidates, including negative terms, without interpreting
/// active state or validating the associated project's current health.
fn immutable_binding<'a>(
    state: &'a ProjectState,
    connection: &ConnectionId,
) -> Option<Option<(&'a BindingId, &'a BindingRecord)>> {
    let mut candidate = None;
    for (id, target) in &state.bindings {
        if !target
            .iter()
            .flatten()
            .any(|record| &record.connection_id == connection)
        {
            continue;
        }
        let mut records = target.iter().flatten();
        let record = records.next()?;
        if candidate.is_some() || records.any(|other| other != record) {
            return None;
        }
        candidate = Some((id, record));
    }
    Some(candidate)
}

/// Compose selected repository and remote-tracking sources while retaining the
/// current Git baseline. Logical membership comes exclusively from `repo_source`;
/// historical identities must never be inferred from a reused alias or endpoint.
pub fn restore_view(
    repo_source: &op_store::View,
    remote_source: &op_store::View,
    current_view: &op_store::View,
) -> Result<op_store::View, String> {
    let mut restored = op_store::View {
        head_ids: repo_source.head_ids.clone(),
        local_bookmarks: repo_source.local_bookmarks.clone(),
        local_tags: repo_source.local_tags.clone(),
        remote_views: BTreeMap::new(),
        git_refs: current_view.git_refs.clone(),
        git_heads: current_view.git_heads.clone(),
        wc_commit_ids: repo_source.wc_commit_ids.clone(),
        wc_sparse_patterns: repo_source.wc_sparse_patterns.clone(),
        project_state: repo_source.project_state.clone(),
        remote_connections: repo_source.remote_connections.clone(),
        observed_remote_connections: BTreeMap::new(),
        observed_remote_names: BTreeMap::new(),
        project_observations: BTreeMap::new(),
    };
    RemoteObservations::new(&mut restored).copy_from(remote_source);
    // A physical root rename retains its connection and binding. Ambiguous
    // aliases or changed bindings cannot establish an identity-preserving move.
    let mut current_keys = BTreeMap::new();
    for (remote, owners) in &repo_source.remote_connections {
        if let Some(Some(connection)) = owners.as_resolved()
            && let Some(binding) = immutable_binding(&repo_source.project_state, connection)
            && immutable_binding(&remote_source.project_state, connection) == Some(binding)
        {
            current_keys
                .entry(connection)
                .and_modify(|key| *key = None)
                .or_insert(Some(remote));
        }
    }
    let remotes: BTreeSet<_> = remote_source
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
        if !restored.observed_remote_connections.contains_key(&remote) {
            RemoteObservations::new(&mut restored).capture_identity_from(&remote, remote_source);
        }
        if !restored.observed_remote_connections.contains_key(&remote)
            && current_view.remote_connections.contains_key(&remote)
        {
            // Name-only snapshots predate connection metadata and cannot prove
            // ownership, even when restoring the entire historical repository.
            restored
                .observed_remote_connections
                .insert(remote.clone(), Merge::normal(ConnectionId::generate()));
            RemoteObservations::new(&mut restored).archive(&remote)?;
            continue;
        }
        if let Some(owner) = restored.observed_remote_connections.get(&remote)
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
            RemoteObservations::new(&mut restored).archive(&remote)?;
        }
    }
    // Archive every displaced alias first so swaps cannot overwrite either
    // connection's complete observation snapshot.
    let relocations: Vec<_> = restored
        .observed_remote_connections
        .iter()
        .filter_map(|(remote, owners)| {
            let connection = owners.as_resolved()?.as_ref()?;
            let current = current_keys.get(connection).copied().flatten()?;
            (remote != current).then(|| (remote.clone(), current.clone()))
        })
        .collect();
    for (old, new) in relocations {
        RemoteObservations::new(&mut restored).relocate(&old, &new)?;
    }
    for (remote, observed) in &restored.observed_remote_connections {
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
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use crate::backend::CommitId;
    use crate::op_store::{RefTarget, RemoteRef, RemoteRefState, View};
    use crate::ref_name::RemoteNameBuf;

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
        let restored = restore_view(&current, &old, &current).unwrap();
        assert_eq!(
            restored.remote_connections[crate::ref_name::RemoteName::new("origin")],
            Merge::normal(b)
        );
        assert_eq!(restored.git_refs, current.git_refs);
        let archived: RemoteNameBuf = format!("jjosh-observed-{a}").into();
        assert_eq!(
            restored.remote_views[&archived].bookmarks[crate::ref_name::RefName::new("main")],
            target
        );
        assert!(
            !restored
                .remote_views
                .contains_key(crate::ref_name::RemoteName::new("origin"))
        );
        let complete = restore_view(&old, &old, &current).unwrap();
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
        for repo_source in [&current, &old] {
            let restored = restore_view(repo_source, &old, &current).unwrap();
            assert!(
                !restored
                    .remote_views
                    .contains_key(crate::ref_name::RemoteName::new("origin"))
            );
            let (historical_name, historical_refs) = restored.remote_views.iter().next().unwrap();
            assert_eq!(
                historical_refs.bookmarks[crate::ref_name::RefName::new("main")],
                target
            );
            let historical_owner = &restored.observed_remote_connections[historical_name];
            assert!(historical_owner.as_resolved().unwrap().is_some());
            assert_ne!(historical_owner, &current_owner);
        }
    }

    #[test]
    fn restore_preserves_conflicted_definitions_and_repo_only_restore_repairs_them() {
        use crate::project::{BindingTarget, ProjectId, ProjectRecord, Representation, ScopedRemoteName};
        use crate::repo_path::RepoPathBuf;

        let mut healthy = View::make_root(CommitId::from_hex("00"));
        let project = ProjectId::generate();
        let binding = BindingId::generate();
        let connection = ConnectionId::generate();
        let definition = ProjectRecord {
            name: "lib".into(),
            canonical_root: RepoPathBuf::from_internal_string("lib").unwrap(),
        };
        healthy.project_state.projects.insert(project.clone(), Merge::normal(definition.clone()));
        healthy.project_state.labels.insert("lib".into(), Merge::normal(project.clone()));
        healthy.project_state.remote_names.insert(connection.clone(), Merge::normal(ScopedRemoteName {
            project: project.clone(),
            name: "origin".into(),
        }));
        let binding_record = BindingRecord {
            target: BindingTarget::Project(project.clone()),
            connection_id: connection.clone(),
            representation: Representation::Whole,
            base: None,
        };
        healthy.project_state.bindings.insert(binding.clone(), Merge::normal(binding_record.clone()));
        healthy.remote_connections.insert("origin".into(), Merge::normal(connection));
        healthy.remote_views.entry("origin".into()).or_default().bookmarks.insert(
            "main#lib".into(),
            RemoteRef {
                target: RefTarget::normal(CommitId::from_hex("11")),
                state: RemoteRefState::Tracked,
            },
        );
        RemoteObservations::new(&mut healthy).capture_identity("origin".as_ref());
        let mut conflicted = healthy.clone();
        conflicted.project_state.projects.insert(project, Merge::from_vec(vec![
            Some(ProjectRecord { name: "left".into(), ..definition.clone() }),
            Some(definition.clone()),
            Some(ProjectRecord { name: "right".into(), ..definition }),
        ]));
        // Signed active-state conflicts do not change the immutable binding.
        conflicted.project_state.bindings.insert(binding, Merge::from_vec(vec![
            None, Some(binding_record), None,
        ]));
        let restored = restore_view(&conflicted, &conflicted, &healthy).unwrap();
        assert_eq!(restored, conflicted);
        let repaired = restore_view(&healthy, &conflicted, &conflicted).unwrap();
        assert_eq!(repaired.project_state, healthy.project_state);
        assert_eq!(repaired.remote_views, conflicted.remote_views);
        assert_eq!(repaired.observed_remote_connections, conflicted.observed_remote_connections);
    }

    #[test]
    fn ambiguous_immutable_binding_preserves_observations_without_relocation() {
        use crate::project::{BindingTarget, Representation};

        let mut old = View::make_root(CommitId::from_hex("00"));
        let connection = ConnectionId::generate();
        let binding = BindingId::generate();
        let original = BindingRecord {
            target: BindingTarget::RepositoryView,
            connection_id: connection.clone(),
            representation: Representation::Whole,
            base: None,
        };
        old.project_state.bindings.insert(binding.clone(), Merge::normal(original.clone()));
        old.remote_connections.insert("origin".into(), Merge::normal(connection.clone()));
        old.remote_views.entry("origin".into()).or_default().bookmarks.insert(
            "main".into(),
            RemoteRef {
                target: RefTarget::normal(CommitId::from_hex("11")),
                state: RemoteRefState::Tracked,
            },
        );
        RemoteObservations::new(&mut old).capture_identity("origin".as_ref());
        let mut current = old.clone();
        current.remote_connections.clear();
        current.remote_connections.insert("renamed".into(), Merge::normal(connection));
        current.project_state.bindings.insert(binding, Merge::from_vec(vec![
            Some(original.clone()),
            None,
            Some(BindingRecord {
                representation: Representation::JoshFilter(":/lib".into()),
                ..original
            }),
        ]));
        let restored = restore_view(&current, &old, &current).unwrap();
        assert_eq!(restored.remote_views, old.remote_views);
        assert_eq!(restored.observed_remote_connections, old.observed_remote_connections);
        assert_eq!(restored.project_state, current.project_state);
    }
}
