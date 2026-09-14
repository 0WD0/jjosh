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
}
