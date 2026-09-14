mod fetch;
mod lifecycle;
mod push;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use gix::remote::Direction;
use jj_cli::cli_util::{CommandHelper, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::{GitPreparedPush, GitRemoteExtension, GitRemoteFetchOptions, GitRemotePushOptions, GitRemoteSession, RemoteFuture};
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::git::{GitFetchRefExpression, GitPushOptions, GitPushRefTargets, GitRemoteObservation, IgnoredRefspecs};
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{BindingId, BindingRecord, BindingTarget, ConnectionId, ConversionObservation, ProjectId, ProjectState, Representation};
use jj_lib::ref_name::{RefName, RefNameBuf, RemoteName, RemoteNameBuf};
use jj_lib::repo::{MutableRepo, Repo};
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::str_util::StringExpression;
use josh_core::filter::Filter;

pub(crate) struct Extension;

#[derive(Clone)]
pub(crate) struct Project {
    pub id: ProjectId,
    pub name: String,
    pub mount: RepoPathBuf,
    pub label: String,
}

#[derive(Clone)]
pub(crate) struct Session {
    pub name: RemoteNameBuf,
    pub git_path: PathBuf,
    pub project: Option<Project>,
    pub binding: Option<(BindingId, BindingRecord)>,
    pub connection: Option<ConnectionId>,
    pub state: ProjectState,
    view: jj_lib::view::View,
    filter: Option<Filter>,
}


pub(crate) fn config_string(repo: &gix::Repository, key: &str) -> Result<Option<String>> {
    repo.config_snapshot()
        .string(key)
        .map(|value| {
            std::str::from_utf8(&value)
                .map(str::to_owned)
                .map_err(Into::into)
        })
        .transpose()
}

pub(crate) fn raw_ref_prefix(repo: &gix::Repository, endpoint: &str) -> Result<String> {
    let key = gix_object::compute_hash(repo.object_hash(), gix_object::Kind::Blob, endpoint.as_bytes())?;
    Ok(format!("refs/jjosh/remote/{key}/"))
}

/// Endpoint identity is independent of aliases and conversion identity.
pub(crate) fn remote_endpoint(remote: &gix::Remote<'_>, direction: Direction, native: bool) -> Result<String> {
    let repo = remote.repo();
    let mut url = remote.url(direction).context("Remote has no selected endpoint")?.clone();
    url.canonicalize(repo.workdir().unwrap_or_else(|| repo.common_dir()))?;
    let native_workspace = native && url.scheme == gix::url::Scheme::File && gix::path::from_bstr(&url.path).join(".jj").is_dir();
    if !native_workspace {
        url = remote.sanitized_url_and_version(direction)?.0;
        url.canonicalize(repo.workdir().unwrap_or_else(|| repo.common_dir()))?;
    }
    if url.scheme == gix::url::Scheme::File { url.serialize_alternative_form = false; }
    String::from_utf8(url.to_bstring().into()).context("Remote endpoint is not UTF-8")
}

impl GitRemoteExtension for Extension {
    fn capabilities(&self) -> &'static [&'static str] { &["jjosh-v1"] }

    fn prepare_binding(
        &self,
        workspace: &WorkspaceCommandHelper,
        remote: &RemoteName,
        connection_id: &ConnectionId,
        args: &jj_cli::git_remote::GitRemoteBindingArgs,
    ) -> Result<BindingRecord, CommandError> {
        lifecycle::prepare_binding(workspace, remote, connection_id, args)
    }

    fn open(
        &self,
        _command: &CommandHelper,
        workspace: &WorkspaceCommandHelper,
        remote: &RemoteName,
    ) -> Result<Box<dyn GitRemoteSession>, CommandError> {
        let backend = jj_lib::git::get_git_backend(workspace.repo().store())?;
        Ok(Box::new(Session::open(
            &backend.git_repo(),
            backend.git_repo_path().to_owned(),
            workspace.repo().view(),
            remote,
        )?))
    }

}

impl Session {
    fn open(
        git: &gix::Repository,
        git_path: PathBuf,
        view: &jj_lib::view::View,
        name: &RemoteName,
    ) -> Result<Self, CommandError> {
        let state = view.project_state();
        view.remote_identity(name).map_err(user_error)?;
        jj_lib::git::check_obsolete_remote_config(git, name).map_err(user_error)?;
        git.find_remote(name.as_str()).map_err(user_error)?;
        let connection =
            config_string(git, &format!("remote.{}.jjosh-connectionId", name.as_str()))
                .map_err(user_error)?
                .map(|id| {
                    ConnectionId::try_from_hex(&id)
                        .ok_or_else(|| user_error("Invalid remote connection identity"))
                })
                .transpose()?;
        if let Some(id) = &connection {
            for other in git.remote_names() {
                let other = std::str::from_utf8(&other).map_err(user_error)?;
                if other != name.as_str() && config_string(git, &format!("remote.{other}.jjosh-connectionId")).map_err(user_error)?.as_deref() == Some(id.hex().as_str()) {
                    return Err(user_error("Multiple remote aliases claim the same connection identity"));
                }
            }
        }
        let binding = connection.as_ref().map(|id| state.binding_for_connection(id).map_err(user_error)).transpose()?.flatten().map(|(id, record)| (id, record.clone()));
        let required = config_string(git, &format!("remote.{}.jjosh-requiredCapability", name.as_str())).map_err(user_error)?;
        if required.as_deref().is_some_and(|capability| capability != "jjosh-v1") { return Err(user_error("Unknown required remote capability")); }
        if required.is_some() && binding.is_none() { return Err(user_error("Managed connection has no active binding in this operation")); }
        if binding.is_none() && (config_string(git, &format!("remote.{}.jjosh-project", name.as_str())).map_err(user_error)?.is_some() || git_path.join("josh/remotes").join(format!("{}.josh", name.as_str())).exists()) {
            return Err(user_error("Legacy remote conversion requires explicit `project migrate`"));
        }
        let mut project = None;
        let mut filter = None;
        if let Some((_, record)) = &binding {
            if git.object_hash() != gix::hash::Kind::Sha1 { return Err(user_error("Project conversion requires SHA-1")); }
            if let BindingTarget::Project(id) = &record.target {
                state.validate_project(id).map_err(user_error)?;
                let definition = state.projects.get(id).and_then(|value| value.as_resolved()).and_then(Option::as_ref).ok_or_else(|| user_error("Project definition is unavailable"))?;
                let labels: Vec<_> = state.labels.iter().filter_map(|(label, value)| (value.as_resolved().and_then(Option::as_ref) == Some(id)).then_some(label)).collect();
                let [label] = labels.as_slice() else { return Err(user_error("Project transport requires one unambiguous registered reference label")); };
                project = Some(Project { id: id.clone(), name: definition.name.clone(), mount: definition.canonical_root.clone(), label: (*label).clone() });
            }
            filter = if record.representation == Representation::Whole { None } else {
                Some(crate::binding_config::filter(&record.representation).map_err(user_error)?)
            };
            if let (Some(value), Some(project)) = (filter, &project) { filter = Some(value.prefix(project.mount.as_internal_file_string())); }
        }
        Ok(Self { name: name.to_owned(), git_path, project, binding, connection, state: state.clone(), view: view.clone(), filter })
    }

    fn push_scope(&self, git: &gix::Repository, project: Option<&ProjectId>, source: Option<&str>) -> Result<Self, CommandError> {
        let selected = if let Some(source) = source {
            let candidates = git.remote_names().iter()
                .map(|name| std::str::from_utf8(name).map(RemoteNameBuf::from).map_err(user_error))
                .collect::<Result<Vec<_>, _>>()?;
            let source = jj_cli::git_remote::resolve_remote_selector_in_view(
                &self.view, &candidates, source, project,
            )?;
            let selected = Self::open(git, self.git_path.clone(), &self.view, &source)?;
            if selected.binding.is_none() { return Err(user_error("--source must select an active conversion binding")); }
            if self.binding.is_some() && self.binding.as_ref().map(|(id, _)| id) != selected.binding.as_ref().map(|(id, _)| id) {
                return Err(user_error("Destination has its own immutable binding; --source cannot replace its representation"));
            }
            selected
        } else { self.clone() };
        if selected.project.as_ref().map(|value| &value.id) != project {
            return Err(user_error("Selected reference project does not match the destination/source binding"));
        }
        if project.is_some() && selected.binding.is_none() { return Err(user_error("Project publication requires a destination binding or explicit --source")); }
        Ok(selected)
    }

    pub fn whole(&self) -> bool { self.binding.as_ref().is_some_and(|(_, record)| record.representation == Representation::Whole) }
    pub fn binding_id(&self) -> &BindingId {
        &self
            .binding
            .as_ref()
            .expect("converted operation has binding")
            .0
    }

    /// Offline native imports carry reusable lossless evidence, not a project mode.
    async fn anchors(
        &self,
        repo: &dyn Repo,
        transaction: &josh_core::cache::Transaction,
    ) -> Result<HashMap<CommitId, CommitId>, CommandError> {
        let mut known = crate::native_project::anchors(repo, transaction, self.binding_id())
            .await
            .map_err(user_error)?;
        if let Some(project) = &self.project {
            for (id, value) in &self.state.bindings {
                let Some(record) = value.as_resolved().and_then(Option::as_ref) else { continue; };
                if id == self.binding_id() || record.target != BindingTarget::Project(project.id.clone()) || record.representation != Representation::Whole { continue; }
                if !crate::native_project::is_offline_binding(transaction, id).map_err(user_error)? { continue; }
                for (raw, canonical) in crate::native_project::anchors(repo, transaction, id).await.map_err(user_error)? {
                    if known.get(&raw).is_some_and(|previous| previous != &canonical) { return Err(user_error("Ambiguous offline native correspondence")); }
                    known.insert(raw, canonical);
                }
            }
        }
        Ok(known)
    }

    fn evidence(
        &self,
        connection: &ConnectionId,
        endpoint: &str,
        raw_ref: String,
        terms: Vec<jj_lib::project::ConversionTerm>,
        base: Option<String>,
        generation: Option<String>,
    ) -> ConversionObservation {
        let (id, binding) = self
            .binding
            .as_ref()
            .expect("converted observation has a binding");
        ConversionObservation {
            binding_id: id.clone(),
            binding: binding.clone(),
            connection_id: connection.clone(),
            endpoint: endpoint.to_owned(),
            raw_ref,
            terms,
            base,
            generation,
        }
    }

    pub fn remote<'repo>(
        &self,
        repo: &'repo gix::Repository,
        direction: Direction,
    ) -> Result<gix::Remote<'repo>> {
        let remote = repo.find_remote(self.name.as_str())?;
        ensure!(
            remote.urls(direction).count() == 1,
            "Remote {} must have exactly one selected endpoint",
            self.name.as_str()
        );
        Ok(remote)
    }
    pub fn endpoint_url(&self, repo: &gix::Repository, direction: Direction) -> Result<String> {
        remote_endpoint(&self.remote(repo, direction)?, direction, self.whole())
    }
    pub fn filter(&self) -> Option<Filter> {
        self.filter
    }
    pub fn raw_prefix(&self, repo: &gix::Repository, endpoint: &str) -> Result<String> {
        raw_ref_prefix(repo, endpoint)
    }
}

impl GitRemoteSession for Session {
    fn local_name(&self, source: &str) -> RefNameBuf { self.project.as_ref().map_or_else(|| source.into(), |project| crate::ref_names::local_name(&project.label, source).into()) }
    fn source_name<'a>(&self, local: &'a RefName) -> Option<&'a str> {
        match &self.project { Some(project) => crate::ref_names::unscoped_name(&project.label, local.as_str()), None => Some(local.as_str()) }
    }
    fn push_name<'a>(&self, local: &'a RefName) -> &'a str {
        local.as_str().rsplit_once('#').filter(|(_, label)| self.state.resolve_label(label).ok().flatten().is_some()).map_or(local.as_str(), |(name, _)| name)
    }
    fn default_fetch_bookmarks(&self) -> Result<(IgnoredRefspecs, StringExpression), CommandError> {
        let repo = gix::open(&self.git_path).map_err(user_error)?;
        jj_lib::git::load_default_fetch_bookmarks(&self.name, &repo).map_err(Into::into)
    }
    fn fetch<'a>(&'a self, ui: &'a mut Ui, command: &'a CommandHelper, repo: &'a mut MutableRepo, selection: GitFetchRefExpression, options: &'a GitRemoteFetchOptions) -> RemoteFuture<'a, Vec<GitRemoteObservation>> { Box::pin(fetch::run(self, ui, command, repo, selection, options)) }
    fn prepare_push<'a>(&'a self, _ui: &'a mut Ui, _command: &'a CommandHelper, repo: &'a mut MutableRepo, targets: &'a GitPushRefTargets, options: &'a GitPushOptions, preparation: &'a GitRemotePushOptions, dry_run: bool) -> RemoteFuture<'a, Box<dyn GitPreparedPush>> { Box::pin(push::prepare(self, repo, targets, options, preparation, dry_run)) }
}
