// Copyright 2026 The Jujutsu Authors
// SPDX-License-Identifier: Apache-2.0

//! Operation-owned project definitions and immutable conversion evidence.
#![expect(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hash;

use crate::backend::CommitId;
use crate::content_hash::ContentHash;
use crate::merge::{Merge, SameChange, trivial_merge};
use crate::object_id::ObjectId as _;
use crate::object_id::id_type;
use crate::ref_name::{RefNameBuf, RemoteNameBuf};
use crate::repo_path::RepoPathBuf;

id_type!(pub ProjectId { hex() });
id_type!(pub BindingId { hex() });
id_type!(pub ConnectionId { hex() });

impl ProjectId {
    pub fn generate() -> Self {
        Self::new(rand::random::<[u8; 16]>().to_vec())
    }
}
impl BindingId {
    pub fn generate() -> Self {
        Self::new(rand::random::<[u8; 16]>().to_vec())
    }
}
impl ConnectionId {
    pub fn generate() -> Self {
        Self::new(rand::random::<[u8; 16]>().to_vec())
    }
}

#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct ProjectRecord {
    pub name: String,
    /// Complete delivery subtree, including any independently registered descendants.
    pub canonical_root: RepoPathBuf,
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub enum BindingTarget {
    Project(ProjectId),
    RepositoryView,
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub enum Representation {
    Whole,
    JoshFilter(String),
    JoshView(String),
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct BindingRecord {
    pub target: BindingTarget,
    pub connection_id: ConnectionId,
    pub representation: Representation,
    pub base: Option<String>,
}
/// A mutable project-local identity, independent of the physical Git key and
/// immutable conversion binding.
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct ScopedRemoteName {
    pub project: ProjectId,
    #[serde(serialize_with = "serialize_remote_name")]
    pub name: RemoteNameBuf,
}
fn serialize_remote_name<S: serde::Serializer>(
    name: &RemoteNameBuf,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(name.as_str())
}
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct ProjectState {
    pub projects: BTreeMap<ProjectId, Merge<Option<ProjectRecord>>>,
    pub bindings: BTreeMap<BindingId, Merge<Option<BindingRecord>>>,
    pub labels: BTreeMap<String, Merge<Option<ProjectId>>>,
    pub remote_names: BTreeMap<ConnectionId, Merge<Option<ScopedRemoteName>>>,
}
impl ContentHash for ProjectState {
    fn hash(&self, state: &mut impl crate::content_hash::DigestUpdate) {
        ContentHash::hash("projects", state);
        ContentHash::hash(&self.projects, state);
        ContentHash::hash("bindings", state);
        ContentHash::hash(&self.bindings, state);
        ContentHash::hash("labels", state);
        ContentHash::hash(&self.labels, state);
        // Preserve historical hashes when the extension is absent.
        if !self.remote_names.is_empty() {
            ContentHash::hash("scoped-remote-names-v2", state);
            ContentHash::hash(&self.remote_names, state);
        }
    }
}
#[derive(
    ContentHash, Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize,
)]
pub enum ObservationKind {
    Bookmark,
    Tag,
    Revision,
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ObservationKey {
    pub remote: RemoteNameBuf,
    pub name: RefNameBuf,
    pub kind: ObservationKind,
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct ConversionTerm {
    pub canonical: Option<CommitId>,
    pub raw: Option<String>,
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct ConversionObservation {
    pub binding_id: BindingId,
    pub binding: BindingRecord,
    pub connection_id: ConnectionId,
    pub endpoint: String,
    pub raw_ref: String,
    pub terms: Vec<ConversionTerm>,
    pub base: Option<String>,
    pub generation: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ProjectDiagnostic {
    pub message: String,
    pub projects: Vec<ProjectId>,
    pub bindings: Vec<BindingId>,
    pub labels: Vec<String>,
}
impl std::fmt::Display for ProjectDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl ProjectState {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
            && self.bindings.is_empty()
            && self.labels.is_empty()
            && self.remote_names.is_empty()
    }

    /// Derived constraints include every positive candidate of unresolved records.
    pub fn diagnostics(&self) -> Vec<ProjectDiagnostic> {
        let mut result = Vec::new();
        let mut report = |message: String,
                          projects: Vec<ProjectId>,
                          bindings: Vec<BindingId>,
                          labels: Vec<String>| {
            result.push(ProjectDiagnostic {
                message,
                projects,
                bindings,
                labels,
            });
        };
        let candidates: Vec<_> = self
            .projects
            .iter()
            .flat_map(|(id, target)| target.adds().flatten().map(move |record| (id, record)))
            .collect();
        for (id, target) in &self.projects {
            if !target.is_resolved() {
                report(
                    format!("Project {id} has an unresolved definition"),
                    vec![id.clone()],
                    vec![],
                    vec![],
                );
            }
            let mut roots = target.iter().flatten().map(|r| &r.canonical_root);
            if let Some(root) = roots.next()
                && roots.any(|other| other != root)
            {
                report(
                    format!("Project {id} has inconsistent immutable roots"),
                    vec![id.clone()],
                    vec![],
                    vec![],
                );
            }
        }
        // Compare only equal-name/root groups, not every pair of projects. Keep
        // candidate order for diagnostics, including unresolved positive terms.
        let mut conflicts = BTreeSet::new();
        let mut add_conflicts = |group: &[usize]| {
            for (offset, &i) in group.iter().enumerate() {
                for &j in &group[offset + 1..] {
                    if candidates[i].0 != candidates[j].0 {
                        conflicts.insert((i.min(j), i.max(j)));
                    }
                }
            }
        };
        let mut order: Vec<_> = (0..candidates.len()).collect();
        order.sort_unstable_by_key(|&i| &candidates[i].1.name);
        for group in order.chunk_by(|&i, &j| candidates[i].1.name == candidates[j].1.name) {
            add_conflicts(group);
        }
        order.sort_unstable_by_key(|&i| &candidates[i].1.canonical_root);
        for group in order
            .chunk_by(|&i, &j| candidates[i].1.canonical_root == candidates[j].1.canonical_root)
        {
            add_conflicts(group);
        }
        let mut conflicts = conflicts.into_iter().peekable();
        for (i, (id, record)) in candidates.iter().enumerate() {
            if record.name.is_empty()
                || record.name.contains(['#', '/', '\\'])
                || record.name.chars().any(char::is_whitespace)
                || record.canonical_root.is_root()
            {
                report(
                    format!("Project {id} has an invalid name or root"),
                    vec![(*id).clone()],
                    vec![],
                    vec![],
                );
            }
            while conflicts.peek().is_some_and(|&(first, _)| first == i) {
                let (_, other) = conflicts.next().unwrap();
                let other_id = candidates[other].0;
                report(
                    format!(
                        "Projects {id} and {other_id} have conflicting names or identical roots"
                    ),
                    vec![(*id).clone(), other_id.clone()],
                    vec![],
                    vec![],
                );
            }
        }
        let healthy_project = |id: &ProjectId| {
            self.projects
                .get(id)
                .and_then(Merge::as_resolved)
                .is_some_and(Option::is_some)
        };
        let mut connections: BTreeMap<&ConnectionId, BTreeSet<&BindingId>> = BTreeMap::new();
        for (id, target) in &self.bindings {
            let projects = || {
                target
                    .adds()
                    .flatten()
                    .filter_map(|record| match &record.target {
                        BindingTarget::Project(id) => Some(id.clone()),
                        BindingTarget::RepositoryView => None,
                    })
                    .collect()
            };
            if !target.is_resolved() {
                report(
                    format!("Binding {id} has an unresolved active state"),
                    projects(),
                    vec![id.clone()],
                    vec![],
                );
            }
            let mut records = target.iter().flatten();
            if let Some(record) = records.next()
                && records.any(|other| other != record)
            {
                report(
                    format!("Binding {id} has inconsistent immutable definitions"),
                    projects(),
                    vec![id.clone()],
                    vec![],
                );
            }
            for record in target.adds().flatten() {
                connections
                    .entry(&record.connection_id)
                    .or_default()
                    .insert(id);
                if let BindingTarget::Project(project) = &record.target
                    && !healthy_project(project)
                {
                    report(
                        format!("Binding {id} refers to unavailable project {project}"),
                        vec![project.clone()],
                        vec![id.clone()],
                        vec![],
                    );
                }
            }
        }
        for (connection, bindings) in &connections {
            if bindings.len() > 1 {
                let projects = bindings
                    .iter()
                    .flat_map(|id| self.bindings[*id].adds().flatten())
                    .filter_map(|r| match &r.target {
                        BindingTarget::Project(id) => Some(id.clone()),
                        BindingTarget::RepositoryView => None,
                    })
                    .collect();
                report(
                    format!("Connection {connection} has multiple active bindings"),
                    projects,
                    bindings.iter().map(|id| (*id).clone()).collect(),
                    vec![],
                );
            }
        }
        for (label, target) in &self.labels {
            if !target.is_resolved() || target.adds().flatten().any(|id| !healthy_project(id)) {
                report(
                    format!("Label {label:?} has an unresolved or unavailable project"),
                    target.adds().flatten().cloned().collect(),
                    vec![],
                    vec![label.clone()],
                );
            }
        }
        let mut names = BTreeMap::new();
        for (connection, target) in &self.remote_names {
            if !target.is_resolved() {
                report(
                    format!("Connection {connection} has an unresolved scoped remote name"),
                    target
                        .adds()
                        .flatten()
                        .map(|identity| identity.project.clone())
                        .collect(),
                    vec![],
                    vec![],
                );
            }
            for identity in target.adds().flatten() {
                if identity.name.as_str().is_empty() || !healthy_project(&identity.project) {
                    report(
                        format!(
                            "Connection {connection} has an invalid name or unavailable project"
                        ),
                        vec![identity.project.clone()],
                        vec![],
                        vec![],
                    );
                }
                if let Some(previous) =
                    names.insert((&identity.project, &identity.name), connection)
                    && previous != connection
                {
                    report(
                        format!(
                            "Remote name {:?} is duplicated in project {}",
                            identity.name.as_str(),
                            identity.project
                        ),
                        vec![identity.project.clone()],
                        vec![],
                        vec![],
                    );
                }
                for binding_id in connections.get(connection).into_iter().flatten() {
                    for binding in self.bindings[*binding_id]
                        .adds()
                        .flatten()
                        .filter(|binding| &binding.connection_id == connection)
                    {
                        if !matches!(&binding.target,
                            BindingTarget::Project(project) if project == &identity.project)
                        {
                            let mut projects = vec![identity.project.clone()];
                            if let BindingTarget::Project(project) = &binding.target {
                                projects.push(project.clone());
                            }
                            report(
                                format!(
                                    "Connection {connection} has a scoped name incompatible with \
                                     binding {binding_id}"
                                ),
                                projects,
                                vec![(*binding_id).clone()],
                                vec![],
                            );
                        }
                    }
                }
            }
        }
        result
    }

    pub fn merge(&mut self, base: &Self, other: &Self) {
        merge_map(&mut self.projects, &base.projects, &other.projects);
        merge_map(&mut self.bindings, &base.bindings, &other.bindings);
        merge_map(&mut self.labels, &base.labels, &other.labels);
        merge_map(
            &mut self.remote_names,
            &base.remote_names,
            &other.remote_names,
        );
    }
}

/// Merge the signed expressions, not just the visible positive records.
pub fn merge_map<K: Ord + Clone, V: Eq + Hash + Clone>(
    current: &mut BTreeMap<K, Merge<Option<V>>>,
    base: &BTreeMap<K, Merge<Option<V>>>,
    other: &BTreeMap<K, Merge<Option<V>>>,
) {
    let keys: BTreeSet<_> = base.keys().chain(other.keys()).collect();
    let absent = Merge::absent();
    for key in keys {
        let base = base.get(key).unwrap_or(&absent);
        let other = other.get(key).unwrap_or(&absent);
        if base == other {
            continue;
        }
        let own = current.get(key).unwrap_or(&absent);
        let target = if let Some(&value) = trivial_merge(&[own, base, other], SameChange::Accept) {
            value.clone()
        } else {
            let value = Merge::from_vec(vec![own.clone(), base.clone(), other.clone()])
                .flatten()
                .simplify();
            match value.resolve_trivial(SameChange::Accept) {
                Some(resolved) => Merge::resolved(resolved.clone()),
                None => value,
            }
        };
        if target.is_absent() {
            current.remove(key);
        } else {
            current.insert(key.clone(), target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, root: &str) -> ProjectRecord {
        ProjectRecord {
            name: name.into(),
            canonical_root: RepoPathBuf::from_internal_string(root).unwrap(),
        }
    }


    #[test]
    fn recursive_merge_retains_absence_and_resolves_concurrent_rename() {
        let id = ProjectId::generate();
        let original = record("original", "a");
        let renamed = record("renamed", "a");
        let base = BTreeMap::from([(id.clone(), Merge::normal(original.clone()))]);
        let other = BTreeMap::from([(id.clone(), Merge::normal(renamed.clone()))]);
        let mut own = BTreeMap::new();
        merge_map(&mut own, &base, &other);
        assert_eq!(
            own[&id],
            Merge::from_vec(vec![None, Some(original), Some(renamed.clone())])
        );
        let conflict = own.clone();
        let resolved = BTreeMap::from([(id.clone(), Merge::normal(renamed))]);
        merge_map(&mut own, &conflict, &resolved);
        assert_eq!(own, resolved);
    }


    #[test]
    fn scoped_rename_delete_merge_retains_signed_identity_without_rebinding() {
        let connection = ConnectionId::generate();
        let project = ProjectId::generate();
        let binding = BindingId::generate();
        let old = ScopedRemoteName {
            project: project.clone(),
            name: "origin".into(),
        };
        let new = ScopedRemoteName {
            project: project.clone(),
            name: "upstream".into(),
        };
        let mut base = ProjectState::default();
        base.projects
            .insert(project.clone(), Merge::normal(record("parser", "parser")));
        base.bindings.insert(
            binding.clone(),
            Merge::normal(BindingRecord {
                target: BindingTarget::Project(project),
                connection_id: connection.clone(),
                representation: Representation::Whole,
                base: None,
            }),
        );
        base.remote_names
            .insert(connection.clone(), Merge::normal(old.clone()));
        let mut ours = base.clone();
        ours.remote_names.remove(&connection);
        let mut theirs = base.clone();
        theirs
            .remote_names
            .insert(connection.clone(), Merge::normal(new.clone()));
        ours.merge(&base, &theirs);
        assert_eq!(
            ours.remote_names[&connection],
            Merge::from_vec(vec![None, Some(old), Some(new)])
        );
        assert_eq!(ours.bindings[&binding], base.bindings[&binding]);
        let conflict = ours.clone();
        ours.merge(&conflict, &theirs);
        assert_eq!(ours.remote_names, theirs.remote_names);
    }
}
