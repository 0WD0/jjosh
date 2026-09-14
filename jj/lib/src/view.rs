// Copyright 2020 The Jujutsu Authors
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

#![expect(missing_docs)]

use std::collections::BTreeMap;
use std::collections::HashSet;

use itertools::Itertools as _;
use thiserror::Error;

use crate::backend::CommitId;
use crate::index::Index;
use crate::index::IndexResult;
use crate::merge::Merge;
use crate::op_store;
use crate::op_store::LocalRemoteRefTarget;
use crate::op_store::RefTarget;
use crate::op_store::RefTargetOptionExt as _;
use crate::op_store::RemoteRef;
use crate::op_store::RemoteView;
use crate::op_store::WorkingCopyPatternsId;
use crate::ref_name::GitRefName;
use crate::ref_name::GitRefNameBuf;
use crate::ref_name::RefName;
use crate::ref_name::RemoteName;
use crate::ref_name::RemoteRefSymbol;
use crate::ref_name::WorkspaceName;
use crate::ref_name::WorkspaceNameBuf;
use crate::refs;
use crate::refs::LocalAndRemoteRef;
use crate::str_util::StringMatcher;

/// A wrapper around [`op_store::View`] that defines additional methods.
#[derive(Eq, Debug, Clone)]
pub struct View {
    data: op_store::View,
    head_normalized: bool,
}

impl PartialEq for View {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl View {
    pub fn new(op_store_view: op_store::View, head_normalized: bool) -> Self {
        Self {
            data: op_store_view,
            head_normalized,
        }
    }

    pub fn wc_commit_ids(&self) -> &BTreeMap<WorkspaceNameBuf, CommitId> {
        &self.data.wc_commit_ids
    }

    pub fn get_wc_commit_id(&self, name: &WorkspaceName) -> Option<&CommitId> {
        self.data.wc_commit_ids.get(name)
    }

    pub fn project_state(&self) -> &crate::project::ProjectState {
        &self.data.project_state
    }

    pub fn project_state_mut(&mut self) -> &mut crate::project::ProjectState {
        &mut self.data.project_state
    }

    /// Reject adopting a pre-existing literal suffix as a project label.
    pub fn check_project_label_available(&self, label: &str) -> Result<(), String> {
        if self.data.project_state.labels.get(label).is_some_and(Merge::is_present) {
            return Err(format!("Project label {label:?} is already registered"));
        }
        let occupied = self.data.local_bookmarks.keys().chain(self.data.local_tags.keys())
            .chain(self.data.remote_views.values().flat_map(|v| v.bookmarks.keys().chain(v.tags.keys())))
            .chain(self.data.project_observations.keys().map(|key| &key.name))
            .any(|name| name.as_str().rsplit_once('#').is_some_and(|(_, suffix)| suffix == label));
        if occupied {
            return Err(format!("Project label {label:?} would adopt existing literal references; migrate them explicitly"));
        }
        Ok(())
    }

    /// Validate the relationship of current canonical targets and their immutable
    /// observations without rewriting either side or choosing a conflict term.
    pub fn validate_project_observation(&self, key: &crate::project::ObservationKey) -> Result<(), String> {
        use crate::project::{BindingTarget, ObservationKind};
        let Some(evidence) = self.data.project_observations.get(key) else { return Ok(()); };
        let owner = self.data.remote_connections.get(&key.remote)
            .and_then(Merge::as_resolved).and_then(Option::as_ref);
        for observation in evidence.adds().flatten() {
            if owner != Some(&observation.connection_id) {
                return Err(format!("Observation {}@{} belongs to an unavailable or different connection", key.name.as_str(), key.remote.as_str()));
            }
            let binding = self.data.project_state.bindings.get(&observation.binding_id)
                .and_then(Merge::as_resolved).and_then(Option::as_ref);
            if binding != Some(&observation.binding) {
                return Err(format!("Observation {}@{} requires unavailable binding {}", key.name.as_str(), key.remote.as_str(), observation.binding_id));
            }
            if let BindingTarget::Project(id) = &observation.binding.target {
                self.data.project_state.validate_project(id)?;
                if let Some((_, label)) = key.name.as_str().rsplit_once('#')
                    && self.data.project_state.resolve_label(label)?.as_ref() != Some(id) {
                    return Err(format!("Observation {}@{} has incompatible project label", key.name.as_str(), key.remote.as_str()));
                }
            }
            if key.kind == ObservationKind::Revision
                && (!observation.raw_ref.is_empty() || !observation.terms.iter().step_by(2).any(|term| term.raw.as_deref() == Some(key.name.as_str()))) {
                return Err(format!("Revision observation {}@{} does not explain its pinned raw OID", key.name.as_str(), key.remote.as_str()));
            }
        }
        let symbol = RemoteRefSymbol { remote: &key.remote, name: &key.name };
        let target = match key.kind {
            ObservationKind::Bookmark => &self.get_remote_bookmark(symbol).target,
            ObservationKind::Tag => &self.get_remote_tag(symbol).target,
            ObservationKind::Revision => return Ok(()),
        };
        // The evidence expression is independent of ancestry simplification of
        // RefTarget. Each surviving positive target must nevertheless have an
        // actual positive witness; do not zip term positions.
        for canonical in target.as_merge().adds() {
            if !evidence.adds().flatten().any(|observation|
                observation.terms.iter().step_by(2).any(|term| &term.canonical == canonical)) {
                return Err(format!("Observation {}@{} does not explain the current canonical target", key.name.as_str(), key.remote.as_str()));
            }
        }
        Ok(())
    }

    pub fn project_diagnostics(&self) -> Vec<crate::project::ProjectDiagnostic> {
        use crate::project::{BindingTarget, ProjectDiagnostic};
        let mut diagnostics = self.data.project_state.diagnostics();
        for (remote, owners) in &self.data.remote_connections {
            if owners.is_resolved() { continue; }
            let mut projects = Vec::new();
            let mut bindings = Vec::new();
            for (id, target) in &self.data.project_state.bindings {
                for record in target.adds().flatten() {
                    if owners.adds().flatten().any(|owner| owner == &record.connection_id) {
                        bindings.push(id.clone());
                        if let BindingTarget::Project(project) = &record.target { projects.push(project.clone()); }
                    }
                }
            }
            diagnostics.push(ProjectDiagnostic {
                message: format!("Remote {} has unresolved connection ownership", remote.as_str()),
                projects, bindings, labels: Vec::new(),
            });
        }
        for (key, evidence) in &self.data.project_observations {
            if let Err(message) = self.validate_project_observation(key) {
                diagnostics.push(ProjectDiagnostic {
                    message,
                    projects: evidence.adds().flatten().filter_map(|observation| match &observation.binding.target {
                        BindingTarget::Project(id) => Some(id.clone()), BindingTarget::RepositoryView => None,
                    }).collect(),
                    bindings: evidence.adds().flatten().map(|observation| observation.binding_id.clone()).collect(),
                    labels: key.name.as_str().rsplit_once('#').map(|(_, label)| label.to_owned()).into_iter().collect(),
                });
            }
        }
        diagnostics
    }

    pub fn wc_sparse_patterns(
        &self,
    ) -> &BTreeMap<WorkspaceNameBuf, Merge<Option<WorkingCopyPatternsId>>> {
        &self.data.wc_sparse_patterns
    }

    pub fn get_wc_sparse_patterns(
        &self,
        name: &WorkspaceName,
    ) -> Option<&Merge<Option<WorkingCopyPatternsId>>> {
        self.data
            .wc_sparse_patterns
            .get(name)
            .filter(|target| target.is_present())
    }

    pub fn set_wc_sparse_patterns(
        &mut self,
        name: WorkspaceNameBuf,
        target: Merge<Option<WorkingCopyPatternsId>>,
    ) {
        if target.is_absent() {
            self.data.wc_sparse_patterns.remove(&name);
        } else {
            self.data.wc_sparse_patterns.insert(name, target);
        }
    }

    pub fn workspaces_for_wc_commit_id(&self, commit_id: &CommitId) -> Vec<WorkspaceNameBuf> {
        let mut workspace_names = vec![];
        for (name, wc_commit_id) in &self.data.wc_commit_ids {
            if wc_commit_id == commit_id {
                workspace_names.push(name.clone());
            }
        }
        workspace_names
    }

    pub fn is_wc_commit_id(&self, commit_id: &CommitId) -> bool {
        self.data.wc_commit_ids.values().contains(commit_id)
    }

    pub fn heads(&self) -> &HashSet<CommitId> {
        &self.data.head_ids
    }

    /// Iterates pair of local and remote bookmarks by bookmark name.
    pub fn bookmarks(&self) -> impl Iterator<Item = (&RefName, LocalRemoteRefTarget<'_>)> {
        op_store::merge_join_ref_views(
            &self.data.local_bookmarks,
            &self.data.remote_views,
            |view| &view.bookmarks,
        )
    }

    /// Iterates pair of local and remote tags by tag name.
    pub fn tags(&self) -> impl Iterator<Item = (&RefName, LocalRemoteRefTarget<'_>)> {
        op_store::merge_join_ref_views(&self.data.local_tags, &self.data.remote_views, |view| {
            &view.tags
        })
    }

    pub fn git_refs(&self) -> &BTreeMap<GitRefNameBuf, RefTarget> {
        &self.data.git_refs
    }

    pub fn git_head(&self, workspace: &WorkspaceName) -> &RefTarget {
        self.data.git_heads.get(workspace).flatten()
    }

    pub fn all_git_heads(&self) -> &BTreeMap<WorkspaceNameBuf, RefTarget> {
        &self.data.git_heads
    }

    pub fn set_wc_commit(&mut self, name: WorkspaceNameBuf, commit_id: CommitId) {
        self.data.wc_commit_ids.insert(name, commit_id);
    }

    pub fn remove_workspace(&mut self, name: &WorkspaceName) {
        self.data.wc_commit_ids.remove(name);
        self.data.git_heads.remove(name);
        self.data.wc_sparse_patterns.remove(name);
    }

    pub fn rename_workspace(
        &mut self,
        old_name: &WorkspaceName,
        new_name: WorkspaceNameBuf,
    ) -> Result<(), RenameWorkspaceError> {
        if self.data.wc_commit_ids.contains_key(&new_name) {
            return Err(RenameWorkspaceError::WorkspaceAlreadyExists {
                name: new_name.clone(),
            });
        }
        let wc_commit_id = self.data.wc_commit_ids.remove(old_name).ok_or_else(|| {
            RenameWorkspaceError::WorkspaceDoesNotExist {
                name: old_name.to_owned(),
            }
        })?;
        self.data
            .wc_commit_ids
            .insert(new_name.clone(), wc_commit_id);
        if let Some(git_head) = self.data.git_heads.remove(old_name) {
            self.data.git_heads.insert(new_name.clone(), git_head);
        }
        if let Some(target) = self.data.wc_sparse_patterns.remove(old_name) {
            self.data.wc_sparse_patterns.insert(new_name, target);
        }
        Ok(())
    }

    pub fn add_head(&mut self, head_id: &CommitId) {
        self.data.head_ids.insert(head_id.clone());
        self.head_normalized = false;
    }

    pub fn remove_head(&mut self, head_id: &CommitId) {
        self.data.head_ids.remove(head_id);
        self.head_normalized = false;
    }

    /// Inserts and removes the provided head ids without flipping
    /// the `head_normalized` bit. This serves as an optimization
    /// for the common incremental update case. Normalization guarantees
    /// is up to callers.
    ///
    /// `add_head_id` must be a descendant of `remove_head_ids`.
    pub fn replace_heads(&mut self, add_head_id: CommitId, remove_head_ids: &[CommitId]) {
        self.data.head_ids.insert(add_head_id);
        for head_id in remove_head_ids {
            self.data.head_ids.remove(head_id);
        }
    }

    /// Iterates local bookmark `(name, target)`s in lexicographical order.
    pub fn local_bookmarks(&self) -> impl Iterator<Item = (&RefName, &RefTarget)> {
        self.data
            .local_bookmarks
            .iter()
            .map(|(name, target)| (name.as_ref(), target))
    }

    /// Iterates local bookmarks `(name, target)` in lexicographical order where
    /// the target adds `commit_id`.
    pub fn local_bookmarks_for_commit(
        &self,
        commit_id: &CommitId,
    ) -> impl Iterator<Item = (&RefName, &RefTarget)> {
        self.local_bookmarks()
            .filter(|(_, target)| target.added_ids().contains(commit_id))
    }

    /// Iterates local bookmark `(name, target)`s matching the given pattern.
    /// Entries are sorted by `name`.
    pub fn local_bookmarks_matching(
        &self,
        matcher: &StringMatcher,
    ) -> impl Iterator<Item = (&RefName, &RefTarget)> {
        matcher
            .filter_btree_map_as_deref(&self.data.local_bookmarks)
            .map(|(name, target)| (name.as_ref(), target))
    }

    pub fn get_local_bookmark(&self, name: &RefName) -> &RefTarget {
        self.data.local_bookmarks.get(name).flatten()
    }

    /// Sets local bookmark to point to the given target. If the target is
    /// absent, the local bookmark will be removed. If there are absent remote
    /// bookmarks tracked by the newly-absent local bookmark, they will also be
    /// removed.
    pub fn set_local_bookmark_target(&mut self, name: &RefName, target: RefTarget) {
        if target.is_present() {
            self.data.local_bookmarks.insert(name.to_owned(), target);
        } else {
            self.data.local_bookmarks.remove(name);
            for remote_view in self.data.remote_views.values_mut() {
                let remote_refs = &mut remote_view.bookmarks;
                if remote_refs.get(name).is_some_and(RemoteRef::is_absent) {
                    remote_refs.remove(name);
                }
            }
        }
    }

    /// Iterates over `(symbol, remote_ref)` for all remote bookmarks in
    /// lexicographical order.
    pub fn all_remote_bookmarks(&self) -> impl Iterator<Item = (RemoteRefSymbol<'_>, &RemoteRef)> {
        op_store::flatten_remote_refs(&self.data.remote_views, |view| &view.bookmarks)
    }

    /// Iterates over `(name, remote_ref)`s for all remote bookmarks of the
    /// specified remote in lexicographical order.
    pub fn remote_bookmarks(
        &self,
        remote_name: &RemoteName,
    ) -> impl Iterator<Item = (&RefName, &RemoteRef)> + use<'_> {
        let maybe_remote_view = self.data.remote_views.get(remote_name);
        maybe_remote_view
            .map(|remote_view| {
                remote_view
                    .bookmarks
                    .iter()
                    .map(|(name, remote_ref)| (name.as_ref(), remote_ref))
            })
            .into_iter()
            .flatten()
    }

    /// Iterates over `(symbol, remote_ref)`s for all remote bookmarks of the
    /// specified remote that match the given pattern.
    ///
    /// Entries are sorted by `symbol`, which is `(name, remote)`.
    pub fn remote_bookmarks_matching(
        &self,
        bookmark_matcher: &StringMatcher,
        remote_matcher: &StringMatcher,
    ) -> impl Iterator<Item = (RemoteRefSymbol<'_>, &RemoteRef)> {
        // Use kmerge instead of flat_map for consistency with all_remote_bookmarks().
        remote_matcher
            .filter_btree_map_as_deref(&self.data.remote_views)
            .map(|(remote, remote_view)| {
                bookmark_matcher
                    .filter_btree_map_as_deref(&remote_view.bookmarks)
                    .map(|(name, remote_ref)| (name.to_remote_symbol(remote), remote_ref))
            })
            .kmerge_by(|(symbol1, _), (symbol2, _)| symbol1 < symbol2)
    }

    pub fn get_remote_bookmark(&self, symbol: RemoteRefSymbol<'_>) -> &RemoteRef {
        if let Some(remote_view) = self.data.remote_views.get(symbol.remote) {
            remote_view.bookmarks.get(symbol.name).flatten()
        } else {
            RemoteRef::absent_ref()
        }
    }

    /// Sets remote-tracking bookmark to the given target and state. If the
    /// target is absent and if no tracking local bookmark exists, the bookmark
    /// will be removed.
    pub fn set_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>, remote_ref: RemoteRef) {
        if remote_ref.is_present()
            || (remote_ref.is_tracked() && self.get_local_bookmark(symbol.name).is_present())
        {
            let remote_view = self
                .data
                .remote_views
                .entry(symbol.remote.to_owned())
                .or_default();
            remote_view
                .bookmarks
                .insert(symbol.name.to_owned(), remote_ref);
        } else if let Some(remote_view) = self.data.remote_views.get_mut(symbol.remote) {
            remote_view.bookmarks.remove(symbol.name);
        }
    }

    /// Iterates over `(name, {local_ref, remote_ref})`s for every bookmark
    /// present locally and/or on the specified remote, in lexicographical
    /// order.
    ///
    /// Note that this does *not* take into account whether the local bookmark
    /// tracks the remote bookmark or not. Missing values are represented as
    /// RefTarget::absent_ref() or RemoteRef::absent_ref().
    pub fn local_remote_bookmarks(
        &self,
        remote_name: &RemoteName,
    ) -> impl Iterator<Item = (&RefName, LocalAndRemoteRef<'_>)> + use<'_> {
        refs::iter_named_local_remote_refs(
            self.local_bookmarks(),
            self.remote_bookmarks(remote_name),
        )
        .map(|(name, (local_target, remote_ref))| {
            let targets = LocalAndRemoteRef {
                local_target,
                remote_ref,
            };
            (name, targets)
        })
    }

    /// Iterates over `(name, TrackingRefPair {local_ref, remote_ref})`s for
    /// every bookmark with a name that matches the given pattern, and that is
    /// present locally and/or on the specified remote.
    ///
    /// Entries are sorted by `name`.
    ///
    /// Note that this does *not* take into account whether the local bookmark
    /// tracks the remote bookmark or not. Missing values are represented as
    /// RefTarget::absent_ref() or RemoteRef::absent_ref().
    pub fn local_remote_bookmarks_matching<'a, 'b>(
        &'a self,
        bookmark_matcher: &'b StringMatcher,
        remote_name: &RemoteName,
    ) -> impl Iterator<Item = (&'a RefName, LocalAndRemoteRef<'a>)> + use<'a, 'b> {
        // Change remote_name to StringMatcher if needed, but merge-join adapter won't
        // be usable.
        let maybe_remote_view = self.data.remote_views.get(remote_name);
        refs::iter_named_local_remote_refs(
            bookmark_matcher.filter_btree_map_as_deref(&self.data.local_bookmarks),
            maybe_remote_view
                .map(|remote_view| {
                    bookmark_matcher.filter_btree_map_as_deref(&remote_view.bookmarks)
                })
                .into_iter()
                .flatten(),
        )
        .map(|(name, (local_target, remote_ref))| {
            let targets = LocalAndRemoteRef {
                local_target,
                remote_ref,
            };
            (name.as_ref(), targets)
        })
    }

    /// Iterates remote `(name, view)`s in lexicographical order.
    pub fn remote_views(&self) -> impl Iterator<Item = (&RemoteName, &RemoteView)> {
        self.data
            .remote_views
            .iter()
            .map(|(name, view)| (name.as_ref(), view))
    }

    /// Iterates matching remote `(name, view)`s in lexicographical order.
    pub fn remote_views_matching(
        &self,
        matcher: &StringMatcher,
    ) -> impl Iterator<Item = (&RemoteName, &RemoteView)> {
        matcher
            .filter_btree_map_as_deref(&self.data.remote_views)
            .map(|(name, view)| (name.as_ref(), view))
    }

    /// Returns the remote view for `name`.
    pub fn get_remote_view(&self, name: &RemoteName) -> Option<&RemoteView> {
        self.data.remote_views.get(name)
    }

    /// Adds remote view if it doesn't exist.
    pub fn ensure_remote(&mut self, remote_name: &RemoteName) {
        if self.data.remote_views.contains_key(remote_name) {
            return;
        }
        self.data
            .remote_views
            .insert(remote_name.to_owned(), RemoteView::default());
    }

    pub fn remove_remote(&mut self, remote_name: &RemoteName) {
        self.data.remote_views.remove(remote_name);
    }

    pub fn rename_remote(&mut self, old: &RemoteName, new: &RemoteName) {
        if let Some(remote_view) = self.data.remote_views.remove(old) {
            self.data.remote_views.insert(new.to_owned(), remote_view);
        }
    }

    /// Iterates local tag `(name, target)`s in lexicographical order.
    pub fn local_tags(&self) -> impl Iterator<Item = (&RefName, &RefTarget)> {
        self.data
            .local_tags
            .iter()
            .map(|(name, target)| (name.as_ref(), target))
    }

    pub fn get_local_tag(&self, name: &RefName) -> &RefTarget {
        self.data.local_tags.get(name).flatten()
    }

    /// Iterates local tag `(name, target)`s matching the given pattern. Entries
    /// are sorted by `name`.
    pub fn local_tags_matching(
        &self,
        matcher: &StringMatcher,
    ) -> impl Iterator<Item = (&RefName, &RefTarget)> {
        matcher
            .filter_btree_map_as_deref(&self.data.local_tags)
            .map(|(name, target)| (name.as_ref(), target))
    }

    /// Sets local tag to point to the given target. If the target is absent,
    /// the local tag will be removed. If there are absent remote tags tracked
    /// by the newly-absent local tag, they will also be removed.
    pub fn set_local_tag_target(&mut self, name: &RefName, target: RefTarget) {
        if target.is_present() {
            self.data.local_tags.insert(name.to_owned(), target);
        } else {
            self.data.local_tags.remove(name);
            for remote_view in self.data.remote_views.values_mut() {
                let remote_refs = &mut remote_view.tags;
                if remote_refs.get(name).is_some_and(RemoteRef::is_absent) {
                    remote_refs.remove(name);
                }
            }
        }
    }

    /// Iterates over `(symbol, remote_ref)` for all remote tags in
    /// lexicographical order.
    pub fn all_remote_tags(&self) -> impl Iterator<Item = (RemoteRefSymbol<'_>, &RemoteRef)> {
        op_store::flatten_remote_refs(&self.data.remote_views, |view| &view.tags)
    }

    /// Iterates over `(name, remote_ref)`s for all remote tags of the specified
    /// remote in lexicographical order.
    pub fn remote_tags(
        &self,
        remote_name: &RemoteName,
    ) -> impl Iterator<Item = (&RefName, &RemoteRef)> + use<'_> {
        let maybe_remote_view = self.data.remote_views.get(remote_name);
        maybe_remote_view
            .map(|remote_view| {
                remote_view
                    .tags
                    .iter()
                    .map(|(name, remote_ref)| (name.as_ref(), remote_ref))
            })
            .into_iter()
            .flatten()
    }

    /// Iterates over `(symbol, remote_ref)`s for all remote tags of the
    /// specified remote that match the given pattern.
    ///
    /// Entries are sorted by `symbol`, which is `(name, remote)`.
    pub fn remote_tags_matching(
        &self,
        tag_matcher: &StringMatcher,
        remote_matcher: &StringMatcher,
    ) -> impl Iterator<Item = (RemoteRefSymbol<'_>, &RemoteRef)> {
        // Use kmerge instead of flat_map for consistency with all_remote_tags().
        remote_matcher
            .filter_btree_map_as_deref(&self.data.remote_views)
            .map(|(remote, remote_view)| {
                tag_matcher
                    .filter_btree_map_as_deref(&remote_view.tags)
                    .map(|(name, remote_ref)| (name.to_remote_symbol(remote), remote_ref))
            })
            .kmerge_by(|(symbol1, _), (symbol2, _)| symbol1 < symbol2)
    }

    /// Returns remote-tracking tag target and state specified by `symbol`.
    pub fn get_remote_tag(&self, symbol: RemoteRefSymbol<'_>) -> &RemoteRef {
        if let Some(remote_view) = self.data.remote_views.get(symbol.remote) {
            remote_view.tags.get(symbol.name).flatten()
        } else {
            RemoteRef::absent_ref()
        }
    }

    /// Sets remote-tracking tag to the given target and state. If the target is
    /// absent and if no tracking local tag exists, the tag will be removed.
    pub fn set_remote_tag(&mut self, symbol: RemoteRefSymbol<'_>, remote_ref: RemoteRef) {
        if remote_ref.is_present()
            || (remote_ref.is_tracked() && self.get_local_tag(symbol.name).is_present())
        {
            let remote_view = self
                .data
                .remote_views
                .entry(symbol.remote.to_owned())
                .or_default();
            remote_view.tags.insert(symbol.name.to_owned(), remote_ref);
        } else if let Some(remote_view) = self.data.remote_views.get_mut(symbol.remote) {
            remote_view.tags.remove(symbol.name);
        }
    }

    /// Iterates over `(name, {local_ref, remote_ref})`s for every tag present
    /// locally and/or on the specified remote, in lexicographical order.
    ///
    /// Note that this does *not* take into account whether the local tag tracks
    /// the remote tag or not. Missing values are represented as
    /// [`RefTarget::absent_ref()`] or [`RemoteRef::absent_ref()`].
    pub fn local_remote_tags(
        &self,
        remote_name: &RemoteName,
    ) -> impl Iterator<Item = (&RefName, LocalAndRemoteRef<'_>)> + use<'_> {
        refs::iter_named_local_remote_refs(self.local_tags(), self.remote_tags(remote_name)).map(
            |(name, (local_target, remote_ref))| {
                let targets = LocalAndRemoteRef {
                    local_target,
                    remote_ref,
                };
                (name, targets)
            },
        )
    }

    /// Iterates over `(name, TrackingRefPair {local_ref, remote_ref})`s for
    /// every tag with a name that matches the given pattern, and that is
    /// present locally and/or on the specified remote.
    ///
    /// Entries are sorted by `name`.
    ///
    /// Note that this does *not* take into account whether the local tag tracks
    /// the remote tag or not. Missing values are represented as
    /// RefTarget::absent_ref() or RemoteRef::absent_ref().
    pub fn local_remote_tags_matching<'a, 'b>(
        &'a self,
        tag_matcher: &'b StringMatcher,
        remote_name: &RemoteName,
    ) -> impl Iterator<Item = (&'a RefName, LocalAndRemoteRef<'a>)> + use<'a, 'b> {
        // Change remote_name to StringMatcher if needed, but merge-join adapter won't
        // be usable.
        let maybe_remote_view = self.data.remote_views.get(remote_name);
        refs::iter_named_local_remote_refs(
            tag_matcher.filter_btree_map_as_deref(&self.data.local_tags),
            maybe_remote_view
                .map(|remote_view| tag_matcher.filter_btree_map_as_deref(&remote_view.tags))
                .into_iter()
                .flatten(),
        )
        .map(|(name, (local_target, remote_ref))| {
            let targets = LocalAndRemoteRef {
                local_target,
                remote_ref,
            };
            (name.as_ref(), targets)
        })
    }

    pub fn get_git_ref(&self, name: &GitRefName) -> &RefTarget {
        self.data.git_refs.get(name).flatten()
    }

    /// Sets the last imported Git ref to point to the given target. If the
    /// target is absent, the reference will be removed.
    pub fn set_git_ref_target(&mut self, name: &GitRefName, target: RefTarget) {
        if target.is_present() {
            self.data.git_refs.insert(name.to_owned(), target);
        } else {
            self.data.git_refs.remove(name);
        }
    }

    /// Sets Git HEAD for the given workspace to point to the given target. If
    /// the target is absent, the entry will be removed.
    pub fn set_git_head_target(&mut self, workspace: &WorkspaceName, target: RefTarget) {
        if target.is_present() {
            self.data.git_heads.insert(workspace.to_owned(), target);
        } else {
            self.data.git_heads.remove(workspace);
        }
    }

    /// Iterates all commit ids referenced by this view.
    ///
    /// This can include hidden commits referenced by remote bookmarks, previous
    /// positions of conflicted bookmarks, etc. The ancestors of the returned
    /// commits should be considered reachable from the view. Use this to build
    /// commit index from scratch.
    ///
    /// The iteration order is unspecified, and may include duplicated entries.
    pub fn all_referenced_commit_ids(&self) -> impl Iterator<Item = &CommitId> {
        // Include both added/removed ids since ancestry information of old
        // references will be needed while merging views.
        fn ref_target_ids(target: &RefTarget) -> impl Iterator<Item = &CommitId> {
            target.as_merge().iter().flatten()
        }

        // Some of the fields (e.g. wc_commit_ids) would be redundant, but let's
        // not be smart here. Callers will build a larger set of commits anyway.
        let op_store::View {
            head_ids,
            local_bookmarks,
            local_tags,
            remote_views,
            git_refs,
            git_heads,
            wc_commit_ids,
            wc_sparse_patterns: _,
            project_state: _,
            remote_connections: _,
            project_observations,
        } = &self.data;
        itertools::chain!(
            head_ids,
            local_bookmarks.values().flat_map(ref_target_ids),
            local_tags.values().flat_map(ref_target_ids),
            remote_views.values().flat_map(|remote_view| {
                let op_store::RemoteView { bookmarks, tags } = remote_view;
                itertools::chain(bookmarks.values(), tags.values())
                    .flat_map(|remote_ref| ref_target_ids(&remote_ref.target))
            }),
            git_refs.values().flat_map(ref_target_ids),
            git_heads.values().flat_map(ref_target_ids),
            wc_commit_ids.values(),
            project_observations.values().flat_map(|target| target.iter().flatten())
                .flat_map(|observation| observation.terms.iter())
                .filter_map(|term| term.canonical.as_ref())
        )
    }

    pub fn set_view(&mut self, data: op_store::View, head_normalized: bool) {
        self.data = data;
        self.head_normalized = head_normalized;
    }

    pub fn store_view(&self) -> &op_store::View {
        &self.data
    }

    pub fn store_view_mut(&mut self) -> &mut op_store::View {
        &mut self.data
    }

    pub fn is_heads_normalized(&self) -> bool {
        self.head_normalized
    }

    pub async fn normalize_heads(
        &mut self,
        index: &dyn Index,
        root_commit_id: &CommitId,
    ) -> IndexResult<()> {
        if self.head_normalized {
            return Ok(());
        }
        let view = self.store_view_mut();
        if view.head_ids.is_empty() {
            view.head_ids.insert(root_commit_id.clone());
        } else if view.head_ids.len() > 1 {
            // An empty head_ids set is padded with the root_commit_id, but the
            // root id is unwanted during the heads resolution.
            view.head_ids.remove(root_commit_id);
            view.head_ids = index
                .heads(&mut view.head_ids.iter())
                .await?
                .into_iter()
                .collect();
        }
        assert!(!view.head_ids.is_empty());
        self.head_normalized = true;
        Ok(())
    }
}

/// Error from attempts to rename a workspace
#[derive(Debug, Error)]
pub enum RenameWorkspaceError {
    #[error("Workspace {} not found", name.as_symbol())]
    WorkspaceDoesNotExist { name: WorkspaceNameBuf },

    #[error("Workspace {} already exists", name.as_symbol())]
    WorkspaceAlreadyExists { name: WorkspaceNameBuf },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_store::RemoteRefState;

    fn remote_symbol<'a, N, M>(name: &'a N, remote: &'a M) -> RemoteRefSymbol<'a>
    where
        N: AsRef<RefName> + ?Sized,
        M: AsRef<RemoteName> + ?Sized,
    {
        RemoteRefSymbol {
            name: name.as_ref(),
            remote: remote.as_ref(),
        }
    }

    #[test]
    fn test_absent_tracked_bookmarks() {
        let mut view = View {
            data: op_store::View::make_root(CommitId::from_hex("000000")),
            head_normalized: true,
        };
        let absent_tracked_ref = RemoteRef {
            target: RefTarget::absent(),
            state: RemoteRefState::Tracked,
        };
        let present_tracked_ref = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("111111")),
            state: RemoteRefState::Tracked,
        };

        // Absent remote ref cannot be tracked by absent local ref
        view.set_remote_bookmark(remote_symbol("foo", "new"), absent_tracked_ref.clone());
        assert_eq!(
            view.get_remote_bookmark(remote_symbol("foo", "new")),
            RemoteRef::absent_ref()
        );

        // Present remote ref can be tracked by absent local ref
        view.set_remote_bookmark(remote_symbol("foo", "present"), present_tracked_ref.clone());
        assert_eq!(
            view.get_remote_bookmark(remote_symbol("foo", "present")),
            &present_tracked_ref
        );

        // Absent remote ref can be tracked by present local ref
        view.set_local_bookmark_target(
            "foo".as_ref(),
            RefTarget::normal(CommitId::from_hex("222222")),
        );
        view.set_remote_bookmark(remote_symbol("foo", "new"), absent_tracked_ref.clone());
        assert_eq!(
            view.get_remote_bookmark(remote_symbol("foo", "new")),
            &absent_tracked_ref
        );

        // Absent remote ref should be removed if local ref becomes absent
        view.set_local_bookmark_target("foo".as_ref(), RefTarget::absent());
        assert_eq!(
            view.get_remote_bookmark(remote_symbol("foo", "new")),
            RemoteRef::absent_ref()
        );
        assert_eq!(
            view.get_remote_bookmark(remote_symbol("foo", "present")),
            &present_tracked_ref
        );
    }

    #[test]
    fn test_absent_tracked_tags() {
        let mut view = View {
            data: op_store::View::make_root(CommitId::from_hex("000000")),
            head_normalized: true,
        };
        let absent_tracked_ref = RemoteRef {
            target: RefTarget::absent(),
            state: RemoteRefState::Tracked,
        };
        let present_tracked_ref = RemoteRef {
            target: RefTarget::normal(CommitId::from_hex("111111")),
            state: RemoteRefState::Tracked,
        };

        // Absent remote ref cannot be tracked by absent local ref
        view.set_remote_tag(remote_symbol("foo", "new"), absent_tracked_ref.clone());
        assert_eq!(
            view.get_remote_tag(remote_symbol("foo", "new")),
            RemoteRef::absent_ref()
        );

        // Present remote ref can be tracked by absent local ref
        view.set_remote_tag(remote_symbol("foo", "present"), present_tracked_ref.clone());
        assert_eq!(
            view.get_remote_tag(remote_symbol("foo", "present")),
            &present_tracked_ref
        );

        // Absent remote ref can be tracked by present local ref
        view.set_local_tag_target(
            "foo".as_ref(),
            RefTarget::normal(CommitId::from_hex("222222")),
        );
        view.set_remote_tag(remote_symbol("foo", "new"), absent_tracked_ref.clone());
        assert_eq!(
            view.get_remote_tag(remote_symbol("foo", "new")),
            &absent_tracked_ref
        );

        // Absent remote ref should be removed if local ref becomes absent
        view.set_local_tag_target("foo".as_ref(), RefTarget::absent());
        assert_eq!(
            view.get_remote_tag(remote_symbol("foo", "new")),
            RemoteRef::absent_ref()
        );
        assert_eq!(
            view.get_remote_tag(remote_symbol("foo", "present")),
            &present_tracked_ref
        );
    }
}
