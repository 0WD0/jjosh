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

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::remote_observations::RemoteObservations;
use crate::op_store;
use crate::project::BindingId;
use crate::project::BindingRecord;
use crate::project::ConnectionId;
use crate::project::ProjectState;

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
    for remote in &remotes {
        if !restored.observed_remote_connections.contains_key(remote) {
            RemoteObservations::new(&mut restored)
                .capture_identity_from(remote.as_ref(), remote_source);
        }
    }

    // Reconcile the selected historical tracking snapshot onto the current
    // physical names by immutable connection identity. Current-view history is
    // intentionally not preserved under synthetic aliases: if a connection no
    // longer exists, its tracking state is dropped. If several historical
    // physical names claim the same connection, only an exact current-name
    // match is unambiguous; otherwise discard that connection's observations.
    let mut historical_by_connection: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (remote, owners) in &restored.observed_remote_connections {
        if let Some(Some(connection)) = owners.as_resolved() {
            historical_by_connection
                .entry(connection)
                .or_default()
                .push(remote);
        }
    }
    let mut remote_names = BTreeMap::new();
    for remote in &remotes {
        // @git is our own backend mirror, not an external connection. Legacy
        // name-only root snapshots also remain valid if no operation here has
        // assigned their name to an identified connection.
        #[cfg(feature = "git")]
        if remote == crate::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
            remote_names.insert(remote.clone(), remote.clone());
            continue;
        }
        match restored.observed_remote_connections.get(remote) {
            Some(owner)
                if !owner.is_absent()
                    && repo_source.remote_connections.get(remote) == Some(owner) =>
            {
                // Preserve signed conflicts at the same identity and name.
                // Restoration must not silently resolve them or lose evidence.
                remote_names.insert(remote.clone(), remote.clone());
            }
            None if !repo_source.remote_connections.contains_key(remote)
                && !current_view.remote_connections.contains_key(remote) =>
            {
                remote_names.insert(remote.clone(), remote.clone());
            }
            _ => {}
        }
    }
    for (connection, current) in current_keys {
        let Some(current) = current else {
            continue;
        };
        let Some(candidates) = historical_by_connection.get(connection) else {
            continue;
        };
        let source = candidates
            .iter()
            .copied()
            .find(|candidate| *candidate == current)
            .or_else(|| (candidates.len() == 1).then_some(candidates[0]));
        if let Some(source) = source {
            remote_names.insert(source.clone(), current.clone());
        }
    }
    RemoteObservations::new(&mut restored).remap(&remote_names, &BTreeMap::new())?;
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
    use super::*;
    use crate::backend::CommitId;
    use crate::merge::Merge;
    use crate::op_store::RefTarget;
    use crate::op_store::RemoteRef;
    use crate::op_store::RemoteRefState;
    use crate::op_store::View;
    #[test]
    fn tracking_restore_drops_old_root_owner_without_replacing_current_membership() {
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
        assert!(restored.remote_views.is_empty());
        assert!(restored.observed_remote_connections.is_empty());
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
            assert!(restored.remote_views.is_empty());
            assert!(restored.observed_remote_connections.is_empty());
        }
    }

    #[test]
    fn restore_preserves_conflicted_definitions_and_repo_only_restore_repairs_them() {
        use crate::project::BindingTarget;
        use crate::project::ProjectId;
        use crate::project::ProjectRecord;
        use crate::project::Representation;
        use crate::project::ScopedRemoteName;
        use crate::repo_path::RepoPathBuf;

        let mut healthy = View::make_root(CommitId::from_hex("00"));
        let project = ProjectId::generate();
        let binding = BindingId::generate();
        let connection = ConnectionId::generate();
        let definition = ProjectRecord {
            name: "lib".into(),
            canonical_root: RepoPathBuf::from_internal_string("lib").unwrap(),
        };
        healthy
            .project_state
            .projects
            .insert(project.clone(), Merge::normal(definition.clone()));
        healthy
            .project_state
            .labels
            .insert("lib".into(), Merge::normal(project.clone()));
        healthy.project_state.remote_names.insert(
            connection.clone(),
            Merge::normal(ScopedRemoteName {
                project: project.clone(),
                name: "origin".into(),
            }),
        );
        let binding_record = BindingRecord {
            target: BindingTarget::Project(project.clone()),
            connection_id: connection.clone(),
            representation: Representation::Whole,
            base: None,
        };
        healthy
            .project_state
            .bindings
            .insert(binding.clone(), Merge::normal(binding_record.clone()));
        healthy
            .remote_connections
            .insert("origin".into(), Merge::normal(connection));
        healthy
            .remote_views
            .entry("origin".into())
            .or_default()
            .bookmarks
            .insert(
                "main#lib".into(),
                RemoteRef {
                    target: RefTarget::normal(CommitId::from_hex("11")),
                    state: RemoteRefState::Tracked,
                },
            );
        RemoteObservations::new(&mut healthy).capture_identity("origin".as_ref());
        let mut conflicted = healthy.clone();
        conflicted.project_state.projects.insert(
            project,
            Merge::from_vec(vec![
                Some(ProjectRecord {
                    name: "left".into(),
                    ..definition.clone()
                }),
                Some(definition.clone()),
                Some(ProjectRecord {
                    name: "right".into(),
                    ..definition
                }),
            ]),
        );
        // Signed active-state conflicts do not change the immutable binding.
        conflicted.project_state.bindings.insert(
            binding,
            Merge::from_vec(vec![None, Some(binding_record), None]),
        );
        let restored = restore_view(&conflicted, &conflicted, &healthy).unwrap();
        assert_eq!(restored, conflicted);
        let repaired = restore_view(&healthy, &conflicted, &conflicted).unwrap();
        assert_eq!(repaired.project_state, healthy.project_state);
        assert_eq!(repaired.remote_views, conflicted.remote_views);
        assert_eq!(
            repaired.observed_remote_connections,
            conflicted.observed_remote_connections
        );
    }

    #[test]
    fn ambiguous_immutable_binding_does_not_relocate_observations_to_another_name() {
        use crate::project::BindingTarget;
        use crate::project::Representation;

        let mut old = View::make_root(CommitId::from_hex("00"));
        let connection = ConnectionId::generate();
        let binding = BindingId::generate();
        let original = BindingRecord {
            target: BindingTarget::RepositoryView,
            connection_id: connection.clone(),
            representation: Representation::Whole,
            base: None,
        };
        old.project_state
            .bindings
            .insert(binding.clone(), Merge::normal(original.clone()));
        old.remote_connections
            .insert("origin".into(), Merge::normal(connection.clone()));
        old.remote_views
            .entry("origin".into())
            .or_default()
            .bookmarks
            .insert(
                "main".into(),
                RemoteRef {
                    target: RefTarget::normal(CommitId::from_hex("11")),
                    state: RemoteRefState::Tracked,
                },
            );
        RemoteObservations::new(&mut old).capture_identity("origin".as_ref());
        let mut current = old.clone();
        current.remote_connections.clear();
        current
            .remote_connections
            .insert("renamed".into(), Merge::normal(connection));
        current.project_state.bindings.insert(
            binding,
            Merge::from_vec(vec![
                Some(original.clone()),
                None,
                Some(BindingRecord {
                    representation: Representation::JoshFilter(":/lib".into()),
                    ..original
                }),
            ]),
        );
        let restored = restore_view(&current, &old, &current).unwrap();
        assert!(restored.remote_views.is_empty());
        assert!(restored.observed_remote_connections.is_empty());
        assert_eq!(restored.project_state, current.project_state);
    }

    #[test]
    fn unchanged_legacy_and_local_git_tracking_survive_restore() {
        let mut old = View::make_root(CommitId::from_hex("00"));
        for remote in ["origin", "git"] {
            old.remote_views
                .entry(remote.into())
                .or_default()
                .bookmarks
                .insert(
                    "main".into(),
                    RemoteRef {
                        target: RefTarget::normal(CommitId::from_hex("11")),
                        state: RemoteRefState::Tracked,
                    },
                );
        }
        assert_eq!(restore_view(&old, &old, &old).unwrap(), old);
    }

    #[test]
    fn self_restore_removes_detached_observations_without_changing_live_state() {
        let mut view = View::make_root(CommitId::from_hex("00"));
        let live = ConnectionId::generate();
        let retired = ConnectionId::generate();
        let target = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("11")),
            state: RemoteRefState::Tracked,
        };
        view.local_bookmarks
            .insert("work".into(), target.target.clone());
        view.remote_connections
            .insert("origin".into(), Merge::normal(live.clone()));
        for (name, owner) in [("origin", live), ("retired-import", retired)] {
            view.observed_remote_connections
                .insert(name.into(), Merge::normal(owner));
            view.remote_views
                .entry(name.into())
                .or_default()
                .bookmarks
                .insert("work".into(), target.clone());
        }
        let restored = restore_view(&view, &view, &view).unwrap();
        assert_eq!(restored.head_ids, view.head_ids);
        assert_eq!(restored.local_bookmarks, view.local_bookmarks);
        assert_eq!(restored.remote_connections, view.remote_connections);
        assert_eq!(restored.project_state, view.project_state);
        assert_eq!(restored.git_refs, view.git_refs);
        assert_eq!(restored.remote_views.len(), 1);
        assert_eq!(
            restored.remote_views[crate::ref_name::RemoteName::new("origin")],
            view.remote_views[crate::ref_name::RemoteName::new("origin")]
        );
    }
}
