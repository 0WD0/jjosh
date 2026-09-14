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
    pub fn generate() -> Self { Self::new(rand::random::<[u8; 16]>().to_vec()) }
}
impl BindingId {
    pub fn generate() -> Self { Self::new(rand::random::<[u8; 16]>().to_vec()) }
}
impl ConnectionId {
    pub fn generate() -> Self { Self::new(rand::random::<[u8; 16]>().to_vec()) }
}

#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct ProjectRecord {
    pub name: String,
    pub canonical_root: RepoPathBuf,
}
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub enum BindingTarget { Project(ProjectId), RepositoryView }
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub enum Representation { Whole, JoshFilter(String), JoshView(String) }
#[derive(ContentHash, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct BindingRecord {
    pub target: BindingTarget,
    pub connection_id: ConnectionId,
    pub representation: Representation,
    pub base: Option<String>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct ProjectState {
    pub projects: BTreeMap<ProjectId, Merge<Option<ProjectRecord>>>,
    pub bindings: BTreeMap<BindingId, Merge<Option<BindingRecord>>>,
    pub labels: BTreeMap<String, Merge<Option<ProjectId>>>,
}
impl ContentHash for ProjectState {
    fn hash(&self, state: &mut impl crate::content_hash::DigestUpdate) {
        ContentHash::hash("projects", state);
        ContentHash::hash(&self.projects, state);
        ContentHash::hash("bindings", state);
        ContentHash::hash(&self.bindings, state);
        ContentHash::hash("labels", state);
        ContentHash::hash(&self.labels, state);
    }
}
#[derive(ContentHash, Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize)]
pub enum ObservationKind { Bookmark, Tag, Revision }
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}

impl ProjectState {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty() && self.bindings.is_empty() && self.labels.is_empty()
    }

    /// Derived constraints include every positive candidate of unresolved records.
    pub fn diagnostics(&self) -> Vec<ProjectDiagnostic> {
        let mut result = Vec::new();
        let mut report = |message: String, projects: Vec<ProjectId>, bindings: Vec<BindingId>, labels: Vec<String>| {
            result.push(ProjectDiagnostic { message, projects, bindings, labels });
        };
        let candidates: Vec<_> = self.projects.iter().flat_map(|(id, target)| {
            target.adds().flatten().map(move |record| (id, record))
        }).collect();
        for (id, target) in &self.projects {
            if !target.is_resolved() {
                report(format!("Project {id} has an unresolved definition"), vec![id.clone()], vec![], vec![]);
            }
            let mut roots = target.iter().flatten().map(|r| &r.canonical_root);
            if let Some(root) = roots.next() && roots.any(|other| other != root) {
                report(format!("Project {id} has inconsistent immutable roots"), vec![id.clone()], vec![], vec![]);
            }
        }
        for (i, (id, record)) in candidates.iter().enumerate() {
            if record.name.is_empty() || record.name.contains(['#', '/', '\\']) || record.name.chars().any(char::is_whitespace) || record.canonical_root.is_root() {
                report(format!("Project {id} has an invalid name or root"), vec![(*id).clone()], vec![], vec![]);
            }
            for (other_id, other) in &candidates[i + 1..] {
                if id == other_id { continue; }
                if record.name == other.name || record.canonical_root.starts_with(&other.canonical_root) || other.canonical_root.starts_with(&record.canonical_root) {
                    report(format!("Projects {id} and {other_id} have conflicting names or roots"), vec![(*id).clone(), (*other_id).clone()], vec![], vec![]);
                }
            }
        }
        let healthy_project = |id: &ProjectId| self.projects.get(id).and_then(Merge::as_resolved).is_some_and(Option::is_some);
        let mut connections: BTreeMap<&ConnectionId, BTreeSet<&BindingId>> = BTreeMap::new();
        for (id, target) in &self.bindings {
            let projects: Vec<_> = target.adds().flatten().filter_map(|record| match &record.target {
                BindingTarget::Project(id) => Some(id.clone()), BindingTarget::RepositoryView => None,
            }).collect();
            if !target.is_resolved() {
                report(format!("Binding {id} has an unresolved active state"), projects.clone(), vec![id.clone()], vec![]);
            }
            let mut records = target.iter().flatten();
            if let Some(record) = records.next() && records.any(|other| other != record) {
                report(format!("Binding {id} has inconsistent immutable definitions"), projects.clone(), vec![id.clone()], vec![]);
            }
            for record in target.adds().flatten() {
                connections.entry(&record.connection_id).or_default().insert(id);
                if let BindingTarget::Project(project) = &record.target && !healthy_project(project) {
                    report(format!("Binding {id} refers to unavailable project {project}"), vec![project.clone()], vec![id.clone()], vec![]);
                }
            }
        }
        for (connection, bindings) in connections {
            if bindings.len() > 1 {
                let projects = bindings.iter().flat_map(|id| self.bindings[*id].adds().flatten()).filter_map(|r| match &r.target {
                    BindingTarget::Project(id) => Some(id.clone()), BindingTarget::RepositoryView => None,
                }).collect();
                report(format!("Connection {connection} has multiple active bindings"), projects, bindings.into_iter().cloned().collect(), vec![]);
            }
        }
        for (label, target) in &self.labels {
            let projects: Vec<_> = target.adds().flatten().cloned().collect();
            if !target.is_resolved() || projects.iter().any(|id| !healthy_project(id)) {
                report(format!("Label {label:?} has an unresolved or unavailable project"), projects, vec![], vec![label.clone()]);
            }
        }
        result
    }

    pub fn validate_project(&self, id: &ProjectId) -> Result<(), String> {
        if !self.projects.get(id).and_then(Merge::as_resolved).is_some_and(Option::is_some) {
            return Err(format!("Project {id} is unavailable or unresolved"));
        }
        if let Some(diagnostic) = self.diagnostics().into_iter().find(|d| d.projects.contains(id)) {
            return Err(diagnostic.message);
        }
        Ok(())
    }

    pub fn project_by_name(&self, name: &str) -> Result<(ProjectId, &ProjectRecord), String> {
        let mut candidates = self.projects.iter().filter(|(_, target)| target.adds().flatten().any(|r| r.name == name));
        let (id, target) = candidates.next().ok_or_else(|| format!("No project named {name:?}"))?;
        if candidates.next().is_some() { return Err(format!("Project name {name:?} is conflicted")); }
        self.validate_project(id)?;
        let record = target.as_resolved().and_then(Option::as_ref).ok_or_else(|| format!("Project {id} is unresolved"))?;
        Ok((id.clone(), record))
    }

    pub fn resolve_label(&self, label: &str) -> Result<Option<ProjectId>, String> {
        let Some(target) = self.labels.get(label) else { return Ok(None); };
        let id = target.as_resolved().ok_or_else(|| format!("Project label {label:?} is unresolved"))?;
        if let Some(id) = id { self.validate_project(id)?; }
        Ok(id.clone())
    }

    pub fn binding_for_connection(&self, connection: &ConnectionId) -> Result<Option<(BindingId, &BindingRecord)>, String> {
        let mut candidates = self.bindings.iter().filter(|(_, target)| target.adds().flatten().any(|r| &r.connection_id == connection));
        let Some((id, target)) = candidates.next() else { return Ok(None); };
        if candidates.next().is_some() { return Err(format!("Connection {connection} has multiple bindings")); }
        if let Some(diagnostic) = self.diagnostics().into_iter().find(|d| d.bindings.contains(id)) { return Err(diagnostic.message); }
        let record = target.as_resolved().and_then(Option::as_ref).ok_or_else(|| format!("Binding {id} is unresolved"))?;
        if let BindingTarget::Project(project) = &record.target { self.validate_project(project)?; }
        Ok(Some((id.clone(), record)))
    }

    pub(crate) fn merge(&mut self, base: &Self, other: &Self) {
        merge_map(&mut self.projects, &base.projects, &other.projects);
        merge_map(&mut self.bindings, &base.bindings, &other.bindings);
        merge_map(&mut self.labels, &base.labels, &other.labels);
    }
}

/// Merge the signed expressions, not just the visible positive records.
pub(crate) fn merge_map<K: Ord + Clone, V: Eq + Hash + Clone>(
    current: &mut BTreeMap<K, Merge<Option<V>>>,
    base: &BTreeMap<K, Merge<Option<V>>>,
    other: &BTreeMap<K, Merge<Option<V>>>,
) {
    let keys: BTreeSet<_> = base.keys().chain(other.keys()).collect();
    let absent = Merge::absent();
    for key in keys {
        let base = base.get(key).unwrap_or(&absent);
        let other = other.get(key).unwrap_or(&absent);
        if base == other { continue; }
        let own = current.get(key).unwrap_or(&absent);
        let target = if let Some(&value) = trivial_merge(&[own, base, other], SameChange::Accept) {
            value.clone()
        } else {
            let value = Merge::from_vec(vec![own.clone(), base.clone(), other.clone()]).flatten().simplify();
            match value.resolve_trivial(SameChange::Accept) {
                Some(resolved) => Merge::resolved(resolved.clone()),
                None => value,
            }
        };
        if target.is_absent() { current.remove(key); } else { current.insert(key.clone(), target); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, root: &str) -> ProjectRecord {
        ProjectRecord { name: name.into(), canonical_root: RepoPathBuf::from_internal_string(root).unwrap() }
    }

    #[test]
    fn conflicted_positive_candidates_reserve_roots_but_negative_terms_do_not() {
        let a = ProjectId::generate();
        let b = ProjectId::generate();
        let mut state = ProjectState::default();
        state.projects.insert(a.clone(), Merge::from_vec(vec![
            Some(record("renamed", "packages/a")), None, Some(record("other", "packages/a")),
        ]));
        state.projects.insert(b.clone(), Merge::normal(record("b", "packages/a/nested")));
        assert!(state.validate_project(&b).is_err());
        state.projects.insert(a, Merge::from_vec(vec![None, Some(record("retired", "packages/a")), None]));
        assert!(state.validate_project(&b).is_ok());
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
        assert_eq!(own[&id], Merge::from_vec(vec![None, Some(original), Some(renamed.clone())]));
        let conflict = own.clone();
        let resolved = BTreeMap::from([(id.clone(), Merge::normal(renamed))]);
        merge_map(&mut own, &conflict, &resolved);
        assert_eq!(own, resolved);
    }

    #[test]
    fn unrelated_project_remains_selectable_with_dangling_binding() {
        let good = ProjectId::generate();
        let missing = ProjectId::generate();
        let mut state = ProjectState::default();
        state.projects.insert(good.clone(), Merge::normal(record("good", "good")));
        state.bindings.insert(BindingId::generate(), Merge::normal(BindingRecord {
            target: BindingTarget::Project(missing), connection_id: ConnectionId::generate(),
            representation: Representation::Whole, base: None,
        }));
        assert!(!state.diagnostics().is_empty());
        assert_eq!(state.project_by_name("good").unwrap().0, good);
    }
}
