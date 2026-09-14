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

//! Complete remote observation snapshots, independent of logical membership and Git mirrors.

use std::collections::{BTreeMap, BTreeSet};

use crate::merge::Merge;
use crate::op_store;
use crate::project::{ConnectionId, ConversionObservation, ObservationKey, ObservationKind};
use crate::ref_name::{RefNameBuf, RemoteName, RemoteNameBuf};

/// A mutable boundary over the four persisted observation maps. Construction
/// borrows the store view; it neither clones repository state nor changes hashes.
pub struct RemoteObservations<'a> {
    view: &'a mut op_store::View,
}

impl<'a> RemoteObservations<'a> {
    pub fn new(view: &'a mut op_store::View) -> Self {
        Self { view }
    }

    /// Copy only remote observation state, preserving all signed targets.
    pub fn copy_from(&mut self, source: &op_store::View) {
        self.view.remote_views.clone_from(&source.remote_views);
        self.view
            .observed_remote_connections
            .clone_from(&source.observed_remote_connections);
        self.view
            .observed_remote_names
            .clone_from(&source.observed_remote_names);
        self.view
            .project_observations
            .clone_from(&source.project_observations);
    }

    pub fn contains(&self, remote: &RemoteName) -> bool {
        contains(self.view, remote)
    }

    /// Check a relocation without changing any observation or logical state.
    pub fn check_relocation(&self, old: &RemoteName, new: &RemoteName) -> Result<(), String> {
        check_relocation(self.view, old, new)
    }

    pub fn remove(&mut self, remote: &RemoteName) {
        self.view.remote_views.remove(remote);
        self.view
            .project_observations
            .retain(|key, _| key.remote != remote);
        if let Some(owners) = self.view.observed_remote_connections.remove(remote) {
            for owner in owners.iter().flatten() {
                if !self
                    .view
                    .observed_remote_connections
                    .values()
                    .any(|target| target.iter().flatten().any(|other| other == owner))
                {
                    self.view.observed_remote_names.remove(owner);
                }
            }
        }
    }

    /// Record live conversion evidence and its identity snapshot together.
    /// Recording evidence never activates a logical connection.
    pub fn record(&mut self, key: ObservationKey, evidence: ConversionObservation) {
        self.view.observed_remote_connections.insert(
            key.remote.clone(),
            Merge::normal(evidence.connection_id.clone()),
        );
        if let Some(name) = self
            .view
            .project_state
            .remote_names
            .get(&evidence.connection_id)
        {
            self.view
                .observed_remote_names
                .insert(evidence.connection_id.clone(), name.clone());
        }
        self.view
            .project_observations
            .insert(key, Merge::normal(evidence));
    }

    /// Capture logical identity only when writing observations, never on a read.
    pub fn capture_identity(&mut self, remote: &RemoteName) {
        if let Some(owner) = self.view.remote_connections.get(remote) {
            self.view
                .observed_remote_connections
                .insert(remote.to_owned(), owner.clone());
            for connection in owner.iter().flatten() {
                if let Some(name) = self.view.project_state.remote_names.get(connection) {
                    self.view
                        .observed_remote_names
                        .insert(connection.clone(), name.clone());
                }
            }
        }
    }

    /// Capture identity from the operation that supplied historical observations,
    /// not from the selected logical namespace of the restored operation.
    pub(super) fn capture_identity_from(&mut self, remote: &RemoteName, source: &op_store::View) {
        if let Some(owner) = source.remote_connections.get(remote) {
            self.view
                .observed_remote_connections
                .insert(remote.to_owned(), owner.clone());
            for connection in owner.iter().flatten() {
                if let Some(name) = source.project_state.remote_names.get(connection) {
                    self.view
                        .observed_remote_names
                        .insert(connection.clone(), name.clone());
                }
            }
        }
    }

    /// Relocate all refs, signed ownership and conversion evidence. Historical
    /// names are connection-keyed, so retaining their keys preserves identity.
    /// Logical membership and backing Git mirrors are deliberately untouched.
    pub fn relocate(&mut self, old: &RemoteName, new: &RemoteName) -> Result<(), String> {
        self.check_relocation(old, new)?;
        if old == new {
            return Ok(());
        }
        if let Some(view) = self.view.remote_views.remove(old) {
            self.view.remote_views.insert(new.to_owned(), view);
        }
        if let Some(owner) = self.view.observed_remote_connections.remove(old) {
            self.view
                .observed_remote_connections
                .insert(new.to_owned(), owner);
        }
        let keys: Vec<_> = self
            .view
            .project_observations
            .keys()
            .filter(|key| key.remote == old)
            .cloned()
            .collect();
        for mut key in keys {
            let evidence = self.view.project_observations.remove(&key).unwrap();
            key.remote = new.to_owned();
            self.view.project_observations.insert(key, evidence);
        }
        Ok(())
    }

    /// Remap a complete imported snapshot. Unmapped remotes are omitted; absent
    /// reference mappings retain their names. Revision observation names are raw
    /// object IDs and are never renamed. All collisions are checked before writes.
    pub fn remap(
        &mut self,
        remote_names: &BTreeMap<RemoteNameBuf, RemoteNameBuf>,
        ref_names: &BTreeMap<RefNameBuf, RefNameBuf>,
    ) -> Result<(), String> {
        let mut destinations = BTreeSet::new();
        for remote in remote_names.values() {
            if !destinations.insert(remote) {
                return Err(format!("Imported remotes collide at {remote:?}"));
            }
        }
        for (remote, view) in &self.view.remote_views {
            if !remote_names.contains_key(remote) {
                continue;
            }
            for refs in [&view.bookmarks, &view.tags] {
                let mut names = BTreeSet::new();
                for name in refs.keys() {
                    let mapped = ref_names.get(name).unwrap_or(name);
                    if !names.insert(mapped) {
                        return Err(format!(
                            "Imported references collide at {mapped:?}@{remote:?}"
                        ));
                    }
                }
            }
        }
        let mut keys = BTreeSet::new();
        for key in self.view.project_observations.keys() {
            let Some(remote) = remote_names.get(&key.remote) else {
                continue;
            };
            let name = if key.kind == ObservationKind::Revision {
                &key.name
            } else {
                ref_names.get(&key.name).unwrap_or(&key.name)
            };
            if !keys.insert((remote, name, key.kind)) {
                return Err(format!(
                    "Imported conversion observations collide at {name:?}@{remote:?}"
                ));
            }
        }
        for (remote, mut view) in std::mem::take(&mut self.view.remote_views) {
            if let Some(mapped) = remote_names.get(&remote) {
                for refs in [&mut view.bookmarks, &mut view.tags] {
                    *refs = std::mem::take(refs)
                        .into_iter()
                        .map(|(name, target)| {
                            (ref_names.get(&name).cloned().unwrap_or(name), target)
                        })
                        .collect();
                }
                self.view.remote_views.insert(mapped.clone(), view);
            }
        }
        for (remote, owners) in std::mem::take(&mut self.view.observed_remote_connections) {
            if let Some(mapped) = remote_names.get(&remote) {
                self.view
                    .observed_remote_connections
                    .insert(mapped.clone(), owners);
            }
        }
        let owners: BTreeSet<_> = self
            .view
            .observed_remote_connections
            .values()
            .flat_map(|target| target.iter().flatten())
            .collect();
        self.view
            .observed_remote_names
            .retain(|connection, _| owners.contains(connection));
        for (mut key, evidence) in std::mem::take(&mut self.view.project_observations) {
            if let Some(remote) = remote_names.get(&key.remote) {
                key.remote = remote.clone();
                if key.kind != ObservationKind::Revision
                    && let Some(name) = ref_names.get(&key.name)
                {
                    key.name = name.clone();
                }
                self.view.project_observations.insert(key, evidence);
            }
        }
        Ok(())
    }

    /// Preserve a snapshot under an identity-backed historical key before alias
    /// reuse. All collision and ownership checks precede identity capture.
    pub fn archive(&mut self, remote: &RemoteName) -> Result<(), String> {
        #[cfg(feature = "git")]
        if remote == crate::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
            return Ok(());
        }
        if !self.contains(remote) {
            return Ok(());
        }
        let existing = self
            .view
            .observed_remote_connections
            .get(remote)
            .or_else(|| self.view.remote_connections.get(remote));
        let connection = match existing {
            Some(owner) => owner
                .as_resolved()
                .and_then(Option::as_ref)
                .cloned()
                .ok_or_else(|| format!("Remote {remote:?} has unresolved historical ownership"))?,
            // A name-only legacy snapshot must not inherit a later connection.
            None => ConnectionId::generate(),
        };
        let opaque = self.view.observed_remote_names.contains_key(&connection)
            || (!self.view.observed_remote_connections.contains_key(remote)
                && self.view.remote_connections.contains_key(remote)
                && self
                    .view
                    .project_state
                    .remote_names
                    .contains_key(&connection));
        let archived: RemoteNameBuf = if opaque && remote.as_str() == format!("jjosh-{connection}")
        {
            remote.to_owned()
        } else {
            format!("jjosh-observed-{connection}").into()
        };
        if archived != remote && self.view.remote_connections.contains_key(&archived) {
            return Err(format!(
                "Historical remote {archived:?} already exists; refusing to overwrite observations"
            ));
        }
        self.check_relocation(remote, &archived)?;
        if !self.view.observed_remote_connections.contains_key(remote) {
            self.capture_identity(remote);
            self.view
                .observed_remote_connections
                .entry(remote.to_owned())
                .or_insert_with(|| Merge::normal(connection));
        }
        self.relocate(remote, &archived)
    }
}

/// Whether any observation state occupies a physical remote key.
pub(super) fn contains(view: &op_store::View, remote: &RemoteName) -> bool {
    view.remote_views.contains_key(remote)
        || view.observed_remote_connections.contains_key(remote)
        || view
            .project_observations
            .keys()
            .any(|key| key.remote == remote)
}

pub(super) fn check_relocation(
    view: &op_store::View,
    old: &RemoteName,
    new: &RemoteName,
) -> Result<(), String> {
    if old != new && contains(view, new) {
        return Err(format!(
            "Historical remote {new:?} already exists; refusing to overwrite observations"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::backend::CommitId;
    use crate::op_store::{RefTarget, RemoteRef, RemoteRefState};
    use crate::project::{ProjectId, ScopedRemoteName};
    use crate::ref_name::RefName;

    use super::*;

    #[test]
    fn remap_preserves_signed_identity_and_revision_evidence() {
        let mut view = op_store::View::make_root(CommitId::from_hex("00"));
        let a = ConnectionId::generate();
        let b = ConnectionId::generate();
        let owners = Merge::from_vec(vec![Some(a.clone()), None, Some(b.clone())]);
        view.observed_remote_connections
            .insert("origin".into(), owners.clone());
        for connection in [&a, &b] {
            view.observed_remote_names.insert(
                connection.clone(),
                Merge::normal(ScopedRemoteName {
                    project: ProjectId::generate(),
                    name: "origin".into(),
                }),
            );
        }
        let names = view.observed_remote_names.clone();
        let target = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("11")),
            state: RemoteRefState::Tracked,
        };
        let refs = view.remote_views.entry("origin".into()).or_default();
        refs.bookmarks.insert("main".into(), target.clone());
        refs.tags.insert("main".into(), target.clone());
        view.remote_views.entry("git".into()).or_default();
        for kind in [
            ObservationKind::Bookmark,
            ObservationKind::Tag,
            ObservationKind::Revision,
        ] {
            view.project_observations.insert(
                ObservationKey {
                    remote: "origin".into(),
                    name: "main".into(),
                    kind,
                },
                Merge::absent(),
            );
        }
        RemoteObservations::new(&mut view)
            .remap(
                &BTreeMap::from([("origin".into(), "imported".into())]),
                &BTreeMap::from([("main".into(), "project/main".into())]),
            )
            .unwrap();
        let remote = RemoteName::new("imported");
        assert_eq!(view.observed_remote_connections[remote], owners);
        assert_eq!(view.observed_remote_names, names);
        assert_eq!(
            view.remote_views[remote].bookmarks[RefName::new("project/main")],
            target
        );
        assert_eq!(
            view.remote_views[remote].tags[RefName::new("project/main")],
            target
        );
        assert!(!view.remote_views.contains_key(RemoteName::new("git")));
        for kind in [
            ObservationKind::Bookmark,
            ObservationKind::Tag,
            ObservationKind::Revision,
        ] {
            let name = if kind == ObservationKind::Revision {
                "main"
            } else {
                "project/main"
            };
            assert_eq!(
                view.project_observations[&ObservationKey {
                    remote: remote.to_owned(),
                    name: name.into(),
                    kind,
                }],
                Merge::absent()
            );
        }
    }

    #[test]
    fn remap_reference_collision_leaves_the_snapshot_unchanged() {
        let mut view = op_store::View::make_root(CommitId::from_hex("00"));
        let target = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("11")),
            state: RemoteRefState::Tracked,
        };
        let refs = view.remote_views.entry("origin".into()).or_default();
        refs.bookmarks.insert("a".into(), target.clone());
        refs.bookmarks.insert("b".into(), target);
        let before = view.clone();
        assert!(
            RemoteObservations::new(&mut view)
                .remap(
                    &BTreeMap::from([("origin".into(), "imported".into())]),
                    &BTreeMap::from([("a".into(), "same".into()), ("b".into(), "same".into())]),
                )
                .is_err()
        );
        assert_eq!(view, before);
    }

    #[test]
    fn archive_collision_does_not_capture_identity_before_failing() {
        let mut view = op_store::View::make_root(CommitId::from_hex("00"));
        let connection = ConnectionId::generate();
        view.remote_connections
            .insert("origin".into(), Merge::normal(connection.clone()));
        view.remote_views.entry("origin".into()).or_default();
        view.remote_views
            .entry(format!("jjosh-observed-{connection}").into())
            .or_default();
        let before = view.clone();
        assert!(
            RemoteObservations::new(&mut view)
                .archive(RemoteName::new("origin"))
                .is_err()
        );
        assert_eq!(view, before);
    }
}
