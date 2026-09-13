mod fetch;
mod lifecycle;
mod push;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use gix::remote::Direction;
use jj_cli::cli_util::{CommandHelper, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::{
    GitPreparedPush, GitPushRoute, GitPushRouter, GitRemoteExtension, GitRemoteFetchOptions,
    GitRemotePushOptions, GitRemoteSession, RemoteFuture,
};
use jj_cli::ui::Ui;
use jj_lib::git::{
    GitFetchRefExpression, GitPushOptions, GitPushRefTargets, GitRemoteObservation, IgnoredRefspecs,
};
use jj_lib::ref_name::{RefName, RefNameBuf, RemoteName, RemoteNameBuf};
use jj_lib::repo::{MutableRepo, Repo as _};
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::str_util::StringExpression;
use josh_changes::remote_config::RemoteConfig;
use josh_core::filter::Filter;

pub(crate) struct Extension;

pub(crate) struct Project {
    pub name: String,
    pub mount: RepoPathBuf,
    pub native: bool,
}

pub(crate) struct Session {
    pub name: RemoteNameBuf,
    pub git_path: PathBuf,
    pub project: Option<Project>,
    pub josh: Option<RemoteConfig>,
}

struct PushCandidate {
    name: RemoteNameBuf,
    read_only: Result<bool, String>,
}

struct DefaultPushRouter {
    candidates: BTreeMap<String, Vec<PushCandidate>>,
}

impl GitPushRouter for DefaultPushRouter {
    fn route(&self, name: &RefName) -> Result<Option<GitPushRoute>, CommandError> {
        let Some((destination, scope)) = name.as_str().rsplit_once('#') else {
            return Ok(None);
        };
        crate::native_project::validate_project(scope).map_err(user_error)?;
        let candidates = self.candidates.get(scope).ok_or_else(|| {
            user_error(format!(
                "No default push remote for scope {scope}; attach a remote to this project or use --remote"
            ))
        })?;
        let mut writable = None;
        let mut writable_count = 0;
        for candidate in candidates {
            let read_only = candidate.read_only.as_ref().map_err(|error| {
                user_error(format!("Remote {}: {error}", candidate.name.as_str()))
            })?;
            if !read_only {
                writable = Some(&candidate.name);
                writable_count += 1;
            }
        }
        let remote = match writable_count {
            1 => writable.unwrap(),
            0 if candidates.len() == 1 => &candidates[0].name,
            _ => {
                let names = candidates
                    .iter()
                    .filter(|candidate| writable_count == 0 || candidate.read_only == Ok(false))
                    .map(|candidate| candidate.name.as_str())
                    .collect::<Vec<_>>();
                return Err(user_error(format!(
                    "Ambiguous default push remote for scope {scope}: {}; use --remote",
                    names.join(", ")
                )));
            }
        };
        Ok(Some(GitPushRoute {
            remote: remote.clone(),
            name: destination.into(),
        }))
    }
}

pub(crate) fn remote_read_only(repo: &gix::Repository, remote: &RemoteName) -> Result<bool> {
    repo.config_snapshot()
        .try_boolean(format!("remote.{}.jjosh-readOnly", remote.as_str()).as_str())
        .with_context(|| format!("Invalid read-only policy for remote {}", remote.as_str()))
        .map(|value| value.unwrap_or(false))
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

/// Resolve one project layout from native state and its configured named peers.
pub(crate) fn project_mount(
    git_path: &std::path::Path,
    project: &str,
) -> Result<RepoPathBuf, CommandError> {
    recorded_project_mount(git_path, project)?
        .map(Ok)
        .unwrap_or_else(|| crate::native_project::default_mount(project).map_err(user_error))
}

pub(crate) fn recorded_project_mount(
    git_path: &std::path::Path,
    project: &str,
) -> Result<Option<RepoPathBuf>, CommandError> {
    let transaction = crate::interop::open_josh_transaction(git_path, true)?;
    let mut recorded = false;
    transaction
        .for_each_ref_prefixed(
            &crate::native_project::project_ref_prefix(project),
            |_, _| {
                recorded = true;
                Ok(())
            },
        )
        .map_err(user_error)?;
    let mut mount = if recorded {
        Some(crate::native_project::load_mount(&transaction, project).map_err(user_error)?)
    } else {
        None
    };
    let git = gix::open(git_path).map_err(user_error)?;
    for remote in git.remote_names() {
        let Ok(name) = std::str::from_utf8(&remote) else {
            continue;
        };
        if config_string(&git, &format!("remote.{name}.jjosh-project"))
            .map_err(user_error)?
            .as_deref()
            != Some(project)
        {
            continue;
        }
        if let Some(path) =
            config_string(&git, &format!("remote.{name}.jjosh-mount")).map_err(user_error)?
        {
            let path = crate::native_project::parse_mount(&path).map_err(user_error)?;
            if mount.as_ref().is_some_and(|existing| existing != &path) {
                return Err(user_error(format!(
                    "Project {project} has inconsistent mounts in its named remotes"
                )));
            }
            mount = Some(path);
        }
    }
    Ok(mount)
}

/// Validate a complete proposed attachment before changing any configuration.
pub(crate) fn validate_attachment(
    repo_path: &std::path::Path,
    remote: &str,
    project: &str,
    mount: &jj_lib::repo_path::RepoPath,
    filter: Option<Filter>,
) -> Result<()> {
    crate::native_project::validate_project(project)?;
    let repo = gix::open(repo_path)?;
    ensure!(
        repo.object_hash() == gix::hash::Kind::Sha1,
        "Project attachment requires SHA-1"
    );
    let project_key = format!("remote.{remote}.jjosh-project");
    let mount_key = format!("remote.{remote}.jjosh-mount");
    if let Some(existing) = config_string(&repo, &project_key)? {
        ensure!(
            existing == project,
            "Remote {remote} is already attached to project {existing}"
        );
    }
    if let Some(existing) = config_string(&repo, &mount_key)? {
        ensure!(
            existing == mount.as_internal_file_string(),
            "Remote {remote} is already mounted at {existing}"
        );
    }
    if let Some(recorded) =
        recorded_project_mount(repo_path, project).map_err(|error| anyhow::anyhow!(error.error))?
    {
        ensure!(
            recorded.as_ref() == mount,
            "Project {project} is already mounted at {}",
            recorded.as_internal_file_string()
        );
    }
    let transaction = crate::interop::open_josh_transaction(repo_path, true)
        .map_err(|error| anyhow::anyhow!(error.error))?;
    if let Some(owner) = crate::native_project::project_for_mount(&transaction, mount)? {
        ensure!(
            owner == project,
            "Mount belongs to project {owner}, not {project}"
        );
    }
    if crate::native_project::native_project_for_mount(&transaction, mount)?.is_some() {
        ensure!(
            filter.is_none_or(|filter| {
                filter == Filter::new().prefix(mount.as_internal_file_string())
            }),
            "A native whole-project remote cannot apply a source-changing Josh filter"
        );
    }
    if let Some(filter) = filter {
        for name in repo.remote_names() {
            let Ok(name) = std::str::from_utf8(&name) else {
                continue;
            };
            if name == remote
                || config_string(&repo, &format!("remote.{name}.jjosh-project"))?.as_deref()
                    != Some(project)
            {
                continue;
            }
            if let Some(peer) =
                josh_changes::remote_config::try_read_remote_config(repo_path, name)?
            {
                ensure!(
                    peer.semantic_filter() == filter,
                    "Project {project} has a different source filter in remote {name}"
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn attachment_settings<'a>(
    project: &'a str,
    mount: &'a jj_lib::repo_path::RepoPath,
    read_only: bool,
    base: Option<&'a str>,
) -> Vec<(&'static str, &'a str)> {
    let mut settings = vec![
        ("jjosh-project", project),
        ("jjosh-mount", mount.as_internal_file_string()),
        ("jjosh-readOnly", if read_only { "true" } else { "false" }),
    ];
    if let Some(base) = base {
        settings.push(("jjosh-base", base));
    }
    settings
}

/// Bind layout identity to an existing named remote.
pub(crate) fn configure_attachment(
    repo_path: &std::path::Path,
    remote: &str,
    project: &str,
    mount: &jj_lib::repo_path::RepoPath,
    read_only: bool,
    base: Option<&str>,
) -> Result<()> {
    let repo = gix::open(repo_path)?;
    repo.find_remote(remote)?;
    let filter = josh_changes::remote_config::try_read_remote_config(repo_path, remote)?
        .map(|config| config.semantic_filter());
    validate_attachment(repo_path, remote, project, mount, filter)?;
    let mut config = repo.config_file_mut(repo.config_path(gix::config::Source::Local)?)?;
    for (key, value) in attachment_settings(project, mount, read_only, base) {
        config.set_raw_value(format!("remote.{remote}.{key}").as_str(), value)?;
    }
    config.commit()?;
    Ok(())
}

pub(crate) fn raw_ref_prefix(repo: &gix::Repository, endpoint: &str) -> Result<String> {
    let key = gix_object::compute_hash(
        repo.object_hash(),
        gix_object::Kind::Blob,
        endpoint.as_bytes(),
    )?;
    Ok(format!("refs/jjosh/remote/{key}/"))
}

/// Use one endpoint identity for initial context, later observations, and leases.
pub(crate) fn remote_endpoint(
    remote: &gix::Remote<'_>,
    direction: Direction,
    native: bool,
) -> Result<String> {
    let repo = remote.repo();
    let mut url = remote
        .url(direction)
        .context("Remote has no selected endpoint")?
        .clone();
    url.canonicalize(repo.workdir().unwrap_or_else(|| repo.common_dir()))?;
    let native_workspace = native
        && url.scheme == gix::url::Scheme::File
        && gix::path::from_bstr(&url.path).join(".jj").is_dir();
    if !native_workspace {
        // Git transport resolves worktrees to their Git directory. A native jj
        // workspace is instead opened by jj and need not contain a .git entry.
        url = remote.sanitized_url_and_version(direction)?.0;
        url.canonicalize(repo.workdir().unwrap_or_else(|| repo.common_dir()))?;
    }
    if url.scheme == gix::url::Scheme::File {
        url.serialize_alternative_form = false;
    }
    String::from_utf8(url.to_bstring().into()).context("Remote endpoint is not UTF-8")
}

impl GitRemoteExtension for Extension {
    fn prepare_remote_management(
        &self,
        workspace: &WorkspaceCommandHelper,
        old: &RemoteName,
        new: Option<&RemoteName>,
    ) -> Result<jj_lib::git::GitRemoteManagementOptions, CommandError> {
        lifecycle::prepare(workspace, old, new)
    }

    fn open(
        &self,
        command: &CommandHelper,
        workspace: &WorkspaceCommandHelper,
        remote: &RemoteName,
    ) -> Result<Box<dyn GitRemoteSession>, CommandError> {
        let backend = jj_lib::git::get_git_backend(workspace.repo().store())?;
        let git = backend.git_repo();
        let git_path = backend.git_repo_path().to_owned();
        let josh = josh_changes::remote_config::try_read_remote_config(&git_path, remote.as_str())
            .map_err(user_error)?;
        let project_name =
            config_string(&git, &format!("remote.{}.jjosh-project", remote.as_str()))
                .map_err(user_error)?;
        if (josh.is_some() || project_name.is_some()) && git.object_hash() != gix::hash::Kind::Sha1
        {
            return Err(user_error(
                "Josh and native project conversion require a SHA-1 repository",
            ));
        }
        let project = if let Some(name) = project_name {
            crate::native_project::validate_project(&name).map_err(user_error)?;
            let transaction = crate::interop::open_josh_transaction(&git_path, true)?;
            let mount = project_mount(&git_path, &name)?;
            if let Some(owner) = crate::native_project::project_for_mount(&transaction, &mount)
                .map_err(user_error)?
                && owner != name {
                    return Err(user_error(format!(
                        "Mount {} belongs to project {owner}, not {name}",
                        mount.as_internal_file_string(),
                    )));
                }
            let mut native = crate::native_project::native_project_for_mount(&transaction, &mount)
                .map_err(user_error)?
                .is_some();
            if !native {
                let url = git
                    .find_remote(remote.as_str())
                    .map_err(user_error)?
                    .url(Direction::Fetch)
                    .cloned()
                    .ok_or_else(|| user_error("Project remote has no fetch URL"))?;
                if url.scheme == gix::url::Scheme::File {
                    let path = gix::path::from_bstr(&url.path);
                    native = command.cwd().join(path).join(".jj").is_dir();
                }
            }
            if native && let Some(config) = &josh {
                let filter = config.semantic_filter();
                if filter != Filter::new()
                    && filter != Filter::new().prefix(mount.as_internal_file_string())
                {
                    return Err(user_error(
                        "A native whole-project remote cannot apply a source-changing Josh filter",
                    ));
                }
            }
            Some(Project {
                name,
                mount,
                native,
            })
        } else {
            None
        };
        Ok(Box::new(Session {
            name: remote.to_owned(),
            git_path,
            project,
            josh,
        }))
    }

    fn default_push_router(
        &self,
        workspace: &WorkspaceCommandHelper,
    ) -> Result<Option<Box<dyn GitPushRouter>>, CommandError> {
        let git = jj_lib::git::get_git_backend(workspace.repo().store())?.git_repo();
        let mut candidates: BTreeMap<String, Vec<PushCandidate>> = BTreeMap::new();
        for name in jj_lib::git::get_all_remote_names(workspace.repo().store())? {
            let Some(project) =
                config_string(&git, &format!("remote.{}.jjosh-project", name.as_str()))
                    .map_err(user_error)?
            else {
                continue;
            };
            let read_only = remote_read_only(&git, &name).map_err(|error| format!("{error:#}"));
            candidates
                .entry(project)
                .or_default()
                .push(PushCandidate { name, read_only });
        }
        Ok(Some(Box::new(DefaultPushRouter { candidates })))
    }
}

impl Session {
    /// Resolve project conversion from the selected reference, not the destination.
    fn push_scope(&self, git: &gix::Repository, name: &str) -> Result<Session, CommandError> {
        crate::native_project::validate_project(name).map_err(user_error)?;
        if git.object_hash() != gix::hash::Kind::Sha1 {
            return Err(user_error("Scoped project conversion requires SHA-1"));
        }
        let mount = project_mount(&self.git_path, name)?;
        let transaction = crate::interop::open_josh_transaction(&self.git_path, true)?;
        let native = crate::native_project::native_project_for_mount(&transaction, &mount)
            .map_err(user_error)?;
        if let Some(native) = native {
            if native != name {
                return Err(user_error(format!(
                    "Mount belongs to project {native}, not {name}"
                )));
            }
            return Ok(Session {
                name: self.name.clone(),
                git_path: self.git_path.clone(),
                project: Some(Project {
                    name: name.to_owned(),
                    mount,
                    native: true,
                }),
                josh: None,
            });
        }
        let mut candidates = Vec::new();
        let mut semantic_filter = None;
        for remote in git.remote_names() {
            let Ok(remote) = std::str::from_utf8(&remote) else {
                continue;
            };
            if config_string(git, &format!("remote.{remote}.jjosh-project"))
                .map_err(user_error)?
                .as_deref()
                != Some(name)
            {
                continue;
            }
            let config =
                josh_changes::remote_config::try_read_remote_config(&self.git_path, remote)
                    .map_err(user_error)?;
            if let Some(config) = &config {
                let filter = config.semantic_filter();
                if semantic_filter.is_some_and(|known| known != filter) {
                    return Err(user_error(format!(
                        "Project {name} has inconsistent source filters"
                    )));
                }
                semantic_filter = Some(filter);
            }
            let has_base = config_string(git, &format!("remote.{remote}.jjosh-base"))
                .map_err(user_error)?
                .is_some();
            candidates.push((remote.to_owned(), config, has_base));
        }
        if candidates.is_empty() {
            if crate::native_project::is_registered(&transaction, name).map_err(user_error)? {
                return Ok(Session {
                    name: self.name.clone(),
                    git_path: self.git_path.clone(),
                    project: Some(Project {
                        name: name.to_owned(),
                        mount,
                        native: false,
                    }),
                    josh: None,
                });
            }
            return Err(user_error(format!(
                "Unknown reference scope {name}: no recorded project conversion"
            )));
        }
        // Explicit source configurations carry reverse-filter context. A plain
        // publication endpoint does not override that context with its own layout.
        if semantic_filter.is_some() {
            candidates.retain(|(_, config, _)| config.is_some());
        }
        let selected = candidates
            .iter()
            .position(|(remote, _, _)| remote == self.name.as_str())
            .or_else(|| {
                let mut bases = candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, _, base))| *base);
                let (index, _) = bases.next()?;
                bases.next().is_none().then_some(index)
            })
            .or_else(|| (candidates.len() == 1).then_some(0))
            .ok_or_else(|| user_error(format!("Project {name} has ambiguous source contexts")))?;
        let (source, josh, _) = candidates.swap_remove(selected);
        Ok(Session {
            name: source.into(),
            git_path: self.git_path.clone(),
            project: Some(Project {
                name: name.to_owned(),
                mount,
                native: false,
            }),
            josh,
        })
    }

    /// Resolve endpoints and transport settings exclusively from the named Git remote.
    pub fn remote<'repo>(
        &self,
        repo: &'repo gix::Repository,
        direction: Direction,
    ) -> Result<gix::Remote<'repo>> {
        if matches!(direction, Direction::Push) {
            ensure!(
                !remote_read_only(repo, &self.name)?,
                "Remote {} has no publication endpoint; configure a writable named remote",
                self.name.as_str(),
            );
        }
        let remote = repo.find_remote(self.name.as_str())?;
        ensure!(
            remote.urls(direction).count() == 1,
            "Remote {} must have exactly one selected endpoint",
            self.name.as_str()
        );
        Ok(remote)
    }

    pub fn endpoint_url(&self, repo: &gix::Repository, direction: Direction) -> Result<String> {
        let remote = self.remote(repo, direction)?;
        remote_endpoint(
            &remote,
            direction,
            self.project.as_ref().is_some_and(|project| project.native),
        )
    }

    /// The complete source-to-canonical Git filter. Native conversion is separate.
    pub fn filter(&self) -> Option<Filter> {
        if self.project.as_ref().is_some_and(|project| project.native) {
            None
        } else if let Some(config) = &self.josh {
            Some(config.semantic_filter())
        } else {
            self.project
                .as_ref()
                .map(|project| Filter::new().prefix(project.mount.as_internal_file_string()))
        }
    }

    /// Endpoint provenance is independent of both remote aliases and canonical IDs.
    pub fn raw_prefix(&self, repo: &gix::Repository, endpoint: &str) -> Result<String> {
        raw_ref_prefix(repo, endpoint)
    }
}

impl GitRemoteSession for Session {
    fn local_name(&self, source: &str) -> RefNameBuf {
        self.project.as_ref().map_or_else(
            || source.into(),
            |project| crate::ref_names::local_name(&project.name, source).into(),
        )
    }

    fn source_name<'a>(&self, local: &'a RefName) -> Option<&'a str> {
        match &self.project {
            Some(project) => crate::ref_names::unscoped_name(&project.name, local.as_str()),
            None => Some(local.as_str()),
        }
    }

    fn push_name<'a>(&self, local: &'a RefName) -> &'a str {
        local
            .as_str()
            .rsplit_once('#')
            .map_or(local.as_str(), |(name, _)| name)
    }

    fn default_fetch_bookmarks(&self) -> Result<(IgnoredRefspecs, StringExpression), CommandError> {
        let repo = gix::open(&self.git_path).map_err(user_error)?;
        jj_lib::git::load_default_fetch_bookmarks(&self.name, &repo).map_err(Into::into)
    }

    fn fetch<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        selection: GitFetchRefExpression,
        options: &'a GitRemoteFetchOptions,
    ) -> RemoteFuture<'a, Vec<GitRemoteObservation>> {
        Box::pin(fetch::run(self, ui, command, repo, selection, options))
    }

    fn prepare_push<'a>(
        &'a self,
        _ui: &'a mut Ui,
        _command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        targets: &'a GitPushRefTargets,
        options: &'a GitPushOptions,
        preparation: &'a GitRemotePushOptions,
        dry_run: bool,
    ) -> RemoteFuture<'a, Box<dyn GitPreparedPush>> {
        Box::pin(push::prepare(
            self,
            repo,
            targets,
            options,
            preparation,
            dry_run,
        ))
    }
}
