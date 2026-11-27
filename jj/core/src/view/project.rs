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

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::merge::Merge;
use crate::project::{
    BindingId, BindingRecord, BindingTarget, ConnectionId, ProjectDiagnostic, ProjectId,
    ProjectRecord, ProjectState,
};

/// Derived lookup data for one unchanged project state. Never persisted or merged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProjectIndex {
    diagnostics: Vec<ProjectDiagnostic>,
    project_errors: BTreeMap<ProjectId, usize>,
    binding_errors: BTreeMap<BindingId, usize>,
    // None means multiple positive binding IDs claim this connection.
    connections: BTreeMap<ConnectionId, Option<BindingId>>,
}

impl ProjectIndex {
    pub(super) fn new(state: &ProjectState) -> Self {
        let diagnostics = state.diagnostics();
        let mut project_errors = BTreeMap::new();
        let mut binding_errors = BTreeMap::new();
        for (index, diagnostic) in diagnostics.iter().enumerate() {
            for id in &diagnostic.projects {
                project_errors.entry(id.clone()).or_insert(index);
            }
            for id in &diagnostic.bindings {
                binding_errors.entry(id.clone()).or_insert(index);
            }
        }
        let mut connections = BTreeMap::new();
        for (id, target) in &state.bindings {
            for record in target.adds().flatten() {
                match connections.entry(record.connection_id.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(Some(id.clone()));
                    }
                    Entry::Occupied(mut entry) => {
                        if entry.get().as_ref() != Some(id) {
                            entry.insert(None);
                        }
                    }
                }
            }
        }
        Self {
            diagnostics,
            project_errors,
            binding_errors,
            connections,
        }
    }
}

/// Immutable queries against the project topology of a selected operation.
///
/// Prefer [`super::View::projects`] for repository queries. [`Self::new`] also
/// supports planning against raw operation state; its borrow prevents that state
/// from changing while the derived index is in use.
pub struct ProjectView<'a> {
    state: &'a ProjectState,
    index: Cow<'a, ProjectIndex>,
}

impl<'a> ProjectView<'a> {
    /// Build a query view over raw, possibly conflicted operation state.
    pub fn new(state: &'a ProjectState) -> Self {
        Self {
            state,
            index: Cow::Owned(ProjectIndex::new(state)),
        }
    }

    pub(super) fn with_index(state: &'a ProjectState, index: &'a ProjectIndex) -> Self {
        Self {
            state,
            index: Cow::Borrowed(index),
        }
    }

    /// Report all project constraints, including unresolved positive candidates.
    pub fn diagnostics(&self) -> &[ProjectDiagnostic] {
        &self.index.diagnostics
    }

    /// Validate this project without rejecting unrelated conflicted projects.
    pub fn validate_project(&self, id: &ProjectId) -> Result<(), String> {
        if !self
            .state
            .projects
            .get(id)
            .and_then(Merge::as_resolved)
            .is_some_and(Option::is_some)
        {
            return Err(format!("Project {id} is unavailable or unresolved"));
        }
        if let Some(&index) = self.index.project_errors.get(id) {
            return Err(self.index.diagnostics[index].message.clone());
        }
        Ok(())
    }

    /// Resolve an unambiguous, usable project by its current display name.
    pub fn project_by_name(&self, name: &str) -> Result<(ProjectId, &'a ProjectRecord), String> {
        let mut candidates = self
            .state
            .projects
            .iter()
            .filter(|(_, target)| target.adds().flatten().any(|r| r.name == name));
        let (id, target) = candidates
            .next()
            .ok_or_else(|| format!("No project named {name:?}"))?;
        if candidates.next().is_some() {
            return Err(format!("Project name {name:?} is conflicted"));
        }
        self.validate_project(id)?;
        let record = target
            .as_resolved()
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("Project {id} is unresolved"))?;
        Ok((id.clone(), record))
    }

    /// Resolve a registered reference label, validating its project identity.
    pub fn resolve_label(&self, label: &str) -> Result<Option<ProjectId>, String> {
        let Some(target) = self.state.labels.get(label) else {
            return Ok(None);
        };
        let id = target
            .as_resolved()
            .ok_or_else(|| format!("Project label {label:?} is unresolved"))?;
        if let Some(id) = id {
            self.validate_project(id)?;
        }
        Ok(id.clone())
    }

    /// Resolve the unique usable active binding of a logical connection.
    pub fn binding_for_connection(
        &self,
        connection: &ConnectionId,
    ) -> Result<Option<(BindingId, &'a BindingRecord)>, String> {
        let Some(candidate) = self.index.connections.get(connection) else {
            return Ok(None);
        };
        let id = candidate
            .as_ref()
            .ok_or_else(|| format!("Connection {connection} has multiple bindings"))?;
        if let Some(&index) = self.index.binding_errors.get(id) {
            return Err(self.index.diagnostics[index].message.clone());
        }
        let record = self.state.bindings[id]
            .as_resolved()
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("Binding {id} is unresolved"))?;
        if let BindingTarget::Project(project) = &record.target {
            self.validate_project(project)?;
        }
        Ok(Some((id.clone(), record)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Representation;
    use crate::repo_path::RepoPathBuf;

    fn record(name: &str, root: &str) -> ProjectRecord {
        ProjectRecord {
            name: name.into(),
            canonical_root: RepoPathBuf::from_internal_string(root).unwrap(),
        }
    }

    #[test]
    fn every_project_in_overlapping_name_and_root_groups_is_rejected() {
        let ids: Vec<_> = (1..=4)
            .map(|value| ProjectId::new(vec![value; 16]))
            .collect();
        let mut state = ProjectState::default();
        for (id, (name, root)) in ids.iter().zip([
            ("same", "shared"),
            ("same", "other"),
            ("different", "shared"),
            ("same", "shared"),
        ]) {
            state
                .projects
                .insert(id.clone(), Merge::normal(record(name, root)));
        }
        let affected: Vec<_> = state
            .diagnostics()
            .into_iter()
            .map(|diagnostic| diagnostic.projects)
            .collect();
        assert_eq!(
            affected,
            [(0, 1), (0, 2), (0, 3), (1, 3), (2, 3)]
                .map(|(a, b)| vec![ids[a].clone(), ids[b].clone()])
        );
        let projects = ProjectView::new(&state);
        for id in &ids {
            assert!(projects.validate_project(id).is_err());
        }
    }

    #[test]
    fn conflicted_positive_candidates_reserve_roots_but_negative_terms_do_not() {
        let a = ProjectId::generate();
        let b = ProjectId::generate();
        let mut state = ProjectState::default();
        state.projects.insert(
            a.clone(),
            Merge::from_vec(vec![
                Some(record("renamed", "packages/a")),
                None,
                Some(record("other", "packages/a")),
            ]),
        );
        state
            .projects
            .insert(b.clone(), Merge::normal(record("b", "packages/a")));
        assert!(ProjectView::new(&state).validate_project(&b).is_err());
        state.projects.insert(
            a,
            Merge::from_vec(vec![None, Some(record("retired", "packages/a")), None]),
        );
        assert!(ProjectView::new(&state).validate_project(&b).is_ok());
    }

    #[test]
    fn nested_roots_are_independent_and_only_identical_roots_conflict() {
        let parent = ProjectId::generate();
        let child = ProjectId::generate();
        let mut state = ProjectState::default();
        state
            .projects
            .insert(parent.clone(), Merge::normal(record("outer", "pkg")));
        state
            .projects
            .insert(child.clone(), Merge::normal(record("inner", "pkg/inner")));
        assert!(state.diagnostics().is_empty());
        ProjectView::new(&state).validate_project(&parent).unwrap();
        ProjectView::new(&state).validate_project(&child).unwrap();

        // An unresolved ancestor does not make a healthy child's path ambiguous.
        state.projects.insert(
            parent.clone(),
            Merge::from_vec(vec![
                Some(record("left", "pkg")),
                Some(record("outer", "pkg")),
                Some(record("right", "pkg")),
            ]),
        );
        assert!(ProjectView::new(&state).validate_project(&parent).is_err());
        ProjectView::new(&state).validate_project(&child).unwrap();

        state.projects.insert(
            parent.clone(),
            Merge::from_vec(vec![None, Some(record("outer", "pkg")), None]),
        );
        ProjectView::new(&state).validate_project(&child).unwrap();

        state
            .projects
            .insert(parent.clone(), Merge::normal(record("other", "pkg/inner")));
        assert!(ProjectView::new(&state).validate_project(&parent).is_err());
        assert!(ProjectView::new(&state).validate_project(&child).is_err());
    }

    #[test]
    fn unrelated_project_remains_selectable_with_dangling_binding() {
        let good = ProjectId::generate();
        let missing = ProjectId::generate();
        let mut state = ProjectState::default();
        state
            .projects
            .insert(good.clone(), Merge::normal(record("good", "good")));
        state.bindings.insert(
            BindingId::generate(),
            Merge::normal(BindingRecord {
                target: BindingTarget::Project(missing),
                connection_id: ConnectionId::generate(),
                representation: Representation::Whole,
                base: None,
            }),
        );
        assert!(!state.diagnostics().is_empty());
        assert_eq!(
            ProjectView::new(&state).project_by_name("good").unwrap().0,
            good
        );
    }

    #[test]
    fn connection_lookup_distinguishes_live_and_removed_binding_candidates() {
        let connection = ConnectionId::new(vec![1; 16]);
        let active = BindingId::new(vec![2; 16]);
        let removed = BindingId::new(vec![3; 16]);
        let competing = BindingId::new(vec![4; 16]);
        let record = BindingRecord {
            target: BindingTarget::RepositoryView,
            connection_id: connection.clone(),
            representation: Representation::Whole,
            base: None,
        };
        let mut state = ProjectState::default();
        state
            .bindings
            .insert(active.clone(), Merge::normal(record.clone()));
        state.bindings.insert(
            removed,
            Merge::from_vec(vec![None, Some(record.clone()), None]),
        );
        assert_eq!(
            ProjectView::new(&state)
                .binding_for_connection(&connection)
                .unwrap(),
            Some((active.clone(), &record))
        );
        state
            .bindings
            .insert(competing.clone(), Merge::normal(record.clone()));
        assert!(
            ProjectView::new(&state)
                .binding_for_connection(&connection)
                .is_err()
        );
        state.bindings.remove(&active);
        assert_eq!(
            ProjectView::new(&state)
                .binding_for_connection(&connection)
                .unwrap(),
            Some((competing, &record))
        );
    }
}
