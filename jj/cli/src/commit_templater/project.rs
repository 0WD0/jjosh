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

//! Project topology of the loaded operation as template values.
//!
//! These are display values. Listing keeps working on unresolved metadata, and
//! nothing here selects a transport route. Endpoints come from the local Git
//! configuration, which may have no section for a recorded connection.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io;
use std::rc::Rc;

use jj_lib::object_id::ObjectId as _;
use jj_lib::project::ProjectId;
use jj_lib::project::Representation;
use jj_lib::ref_name::RemoteName;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::view::View;
use pollster::FutureExt as _;

use super::CommitRef;
use super::CommitTemplateBuildMethodFnMap;
use super::RefSymbolBuf;
use crate::templater;
use crate::templater::Template;
use crate::templater::TemplateFormatter;
use crate::templater::TemplatePropertyError;
use crate::templater::TemplatePropertyExt as _;

/// A registered project for templating.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProjectRef {
    id: String,
    /// Display name, or `None` while the definition is unresolved.
    name: Option<String>,
    /// Canonical root, or `None` while the definition is unresolved.
    path: Option<RepoPathBuf>,
    /// Reference labels claimed by this project.
    labels: Vec<String>,
    remotes: Vec<ProjectRemote>,
}

impl ProjectRef {
    /// Display name, or `None` while the definition is unresolved.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// One of a project's current logical remotes for templating.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProjectRemote {
    /// Name within the project.
    name: RefSymbolBuf,
    /// Selector that names this remote from the root scope.
    qualified_name: String,
    url: Option<String>,
    push_url: Option<String>,
    representation: Option<String>,
}

impl Template for Rc<ProjectRef> {
    fn format(&self, formatter: &mut TemplateFormatter) -> io::Result<()> {
        match &self.name {
            Some(name) => write!(formatter.labeled("name"), "{name}"),
            None => write!(formatter.labeled("id"), "{}", self.id),
        }
    }
}

impl Template for Vec<Rc<ProjectRef>> {
    fn format(&self, formatter: &mut TemplateFormatter) -> io::Result<()> {
        templater::format_joined(formatter, self, " ")
    }
}

impl Template for ProjectRemote {
    fn format(&self, formatter: &mut TemplateFormatter) -> io::Result<()> {
        write!(formatter.labeled("remote"), "{}", self.name)
    }
}

impl Template for Vec<ProjectRemote> {
    fn format(&self, formatter: &mut TemplateFormatter) -> io::Result<()> {
        templater::format_joined(formatter, self, " ")
    }
}

/// Every project with a positive definition in one loaded View.
#[derive(Debug, Default)]
pub struct ProjectIndex {
    projects: BTreeMap<ProjectId, Rc<ProjectRef>>,
}

impl ProjectIndex {
    pub fn load(repo: &dyn Repo) -> Self {
        let view = repo.view();
        let state = view.project_state();
        let mut remotes = project_remotes(repo);
        let projects = state
            .projects
            .iter()
            .filter(|(_, target)| target.adds().flatten().next().is_some())
            .map(|(id, target)| {
                let record = target.as_resolved().and_then(Option::as_ref);
                let labels = state
                    .labels
                    .iter()
                    .filter(|(_, owner)| owner.adds().flatten().any(|owner| owner == id))
                    .map(|(label, _)| label.clone())
                    .collect();
                let project = ProjectRef {
                    id: id.hex(),
                    name: record.map(|record| record.name.clone()),
                    path: record.map(|record| record.canonical_root.clone()),
                    labels,
                    remotes: remotes.remove(id).unwrap_or_default(),
                };
                (id.clone(), Rc::new(project))
            })
            .collect();
        Self { projects }
    }

    pub fn get(&self, id: &ProjectId) -> Option<&Rc<ProjectRef>> {
        self.projects.get(id)
    }

    /// Projects by display name, unresolved definitions last.
    pub fn sorted(&self) -> Vec<Rc<ProjectRef>> {
        let mut projects: Vec<_> = self.projects.values().cloned().collect();
        projects.sort_by(|a, b| {
            (a.name.is_none(), &a.name, &a.id).cmp(&(b.name.is_none(), &b.name, &b.id))
        });
        projects
    }
}

/// Current logical connections grouped by owning project, in name order.
fn project_remotes(repo: &dyn Repo) -> HashMap<ProjectId, Vec<ProjectRemote>> {
    let view = repo.view();
    #[cfg(feature = "git")]
    let git_repo = jj_lib::git::get_git_repo(repo.store()).ok();
    let mut remotes: HashMap<ProjectId, Vec<ProjectRemote>> = HashMap::new();
    for remote in view.store_view().remote_connections.keys() {
        // Broken ownership is reported by project diagnostics, not listed as a route.
        let Ok(Some(identity)) = view.remote_identity(remote) else {
            continue;
        };
        let Some(scoped) = identity.scoped_name else {
            continue;
        };
        let representation = view
            .projects()
            .binding_for_connection(identity.connection)
            .ok()
            .flatten()
            .map(|(_, binding)| match &binding.representation {
                Representation::Whole => "whole".to_owned(),
                Representation::JoshFilter(filter) => format!("filter {filter}"),
                Representation::JoshView(path) => format!("view {path}"),
            });
        #[cfg(feature = "git")]
        let (url, push_url) = git_repo
            .as_ref()
            .map_or((None, None), |git_repo| remote_urls(git_repo, remote));
        #[cfg(not(feature = "git"))]
        let (url, push_url) = (None, None);
        remotes
            .entry(scoped.project.clone())
            .or_default()
            .push(ProjectRemote {
                name: RefSymbolBuf(scoped.name.as_str().to_owned()),
                qualified_name: view.remote_qualified_name(remote),
                url,
                push_url,
                representation,
            });
    }
    for remotes in remotes.values_mut() {
        remotes.sort_by(|a, b| a.name.0.cmp(&b.name.0));
    }
    remotes
}

#[cfg(feature = "git")]
fn remote_urls(
    git_repo: &gix::Repository,
    remote: &RemoteName,
) -> (Option<String>, Option<String>) {
    let Ok(Some(remote)) = jj_lib::git::try_find_active_remote(git_repo, remote) else {
        return (None, None);
    };
    let url = |direction| {
        remote
            .url(direction)
            .map(|url| url.to_bstring().to_string())
    };
    (
        url(gix::remote::Direction::Fetch),
        url(gix::remote::Direction::Push),
    )
}

/// The project owning a reference name's registered label. Display keeps the
/// owner of an unresolved definition; selecting it for an operation does not.
fn ref_project(view: &View, commit_ref: &CommitRef) -> Option<ProjectId> {
    let (_, label) = commit_ref.name.0.rsplit_once('#')?;
    view.project_state()
        .labels
        .get(label)?
        .as_resolved()?
        .clone()
}

/// Whether a tracked remote ref and its local ref deliver the same content: the
/// same target, or the same tree within the reference's scope. A project ref is
/// compared at the project root, so rewriting unrelated repository history does
/// not make it differ. Equal content does not mean that a push has nothing to do.
fn tracking_content_synced(
    repo: &dyn Repo,
    commit_ref: &CommitRef,
) -> Result<bool, TemplatePropertyError> {
    let Some(tracking) = &commit_ref.tracking_ref else {
        return Ok(false);
    };
    if commit_ref.target == tracking.target {
        return Ok(true);
    }
    let (Some(remote_id), Some(local_id)) =
        (commit_ref.target.as_normal(), tracking.target.as_normal())
    else {
        return Ok(false);
    };
    let remote = repo.store().get_commit(remote_id)?;
    let local = repo.store().get_commit(local_id)?;
    let view = repo.view();
    let root = ref_project(view, commit_ref).and_then(|id| {
        let record = view.project_state().projects.get(&id)?.as_resolved()?;
        Some(record.as_ref()?.canonical_root.clone())
    });
    Ok(match root {
        Some(root) => {
            remote.tree().path_value(&root).block_on()?
                == local.tree().path_value(&root).block_on()?
        }
        None => remote.tree_ids() == local.tree_ids(),
    })
}

pub(super) fn add_commit_ref_methods<'repo>(
    map: &mut CommitTemplateBuildMethodFnMap<'repo, Rc<CommitRef>>,
) {
    map.insert(
        "project",
        |language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let repo = language.repo;
            let index = language.keyword_cache.projects(repo).clone();
            let out_property = self_property.map(move |commit_ref| {
                ref_project(repo.view(), &commit_ref).and_then(|id| index.get(&id).cloned())
            });
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "unscoped_name",
        |language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let repo = language.repo;
            let out_property = self_property.map(move |commit_ref| {
                let name = &commit_ref.name.0;
                match ref_project(repo.view(), &commit_ref) {
                    Some(_) => RefSymbolBuf(name.rsplit_once('#').unwrap().0.to_owned()),
                    None => commit_ref.name.clone(),
                }
            });
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "tracking_content_synced",
        |language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let repo = language.repo;
            let out_property = self_property
                .and_then(move |commit_ref| tracking_content_synced(repo, &commit_ref));
            Ok(out_property.into_dyn_wrapped())
        },
    );
}

pub(super) fn builtin_project_ref_methods<'repo>()
-> CommitTemplateBuildMethodFnMap<'repo, Rc<ProjectRef>> {
    let mut map = CommitTemplateBuildMethodFnMap::<Rc<ProjectRef>>::new();
    map.insert(
        "id",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|project| project.id.clone());
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "name",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property
                .and_then(|project| project.name.clone().ok_or_else(unresolved_definition));
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "path",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property
                .and_then(|project| project.path.clone().ok_or_else(unresolved_definition));
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "label",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.and_then(|project| match project.labels.as_slice() {
                [label] => Ok(label.clone()),
                _ => Err(TemplatePropertyError(
                    "Project has no unambiguous reference label".into(),
                )),
            });
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "conflict",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|project| project.name.is_none());
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "remotes",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|project| project.remotes.clone());
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map
}

fn unresolved_definition() -> TemplatePropertyError {
    TemplatePropertyError("Project definition is unresolved".into())
}

pub(super) fn builtin_project_remote_methods<'repo>()
-> CommitTemplateBuildMethodFnMap<'repo, ProjectRemote> {
    let mut map = CommitTemplateBuildMethodFnMap::<ProjectRemote>::new();
    map.insert(
        "name",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|remote| remote.name);
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "qualified_name",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|remote| remote.qualified_name);
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "url",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|remote| remote.url.unwrap_or_default());
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "push_url",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property = self_property.map(|remote| remote.push_url.unwrap_or_default());
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map.insert(
        "representation",
        |_language, _diagnostics, _build_ctx, self_property, function| {
            function.expect_no_arguments()?;
            let out_property =
                self_property.map(|remote| remote.representation.unwrap_or_default());
            Ok(out_property.into_dyn_wrapped())
        },
    );
    map
}
