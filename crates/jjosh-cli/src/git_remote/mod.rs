mod fetch;
mod push;

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use gix::remote::Direction;
use jj_cli::cli_util::{CommandHelper, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::git_remote::{
    GitRemoteExtension, GitRemotePushOptions, GitRemoteSession, RemoteFuture,
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
    let transaction = crate::interop::open_josh_transaction(git_path, true)?;
    let mut recorded = false;
    transaction.for_each_ref_prefixed(
        &crate::native_project::project_ref_prefix(project),
        |_, _| { recorded = true; Ok(()) },
    ).map_err(user_error)?;
    let mut mount = if recorded {
        Some(crate::native_project::load_mount(&transaction, project).map_err(user_error)?)
    } else {
        None
    };
    let git = gix::open(git_path).map_err(user_error)?;
    for remote in git.remote_names() {
        let Ok(name) = std::str::from_utf8(&remote) else { continue };
        if config_string(&git, &format!("remote.{name}.jjosh-project")).map_err(user_error)?
            .as_deref() != Some(project)
        {
            continue;
        }
        if let Some(path) = config_string(&git, &format!("remote.{name}.jjosh-mount")).map_err(user_error)? {
            let path = crate::native_project::parse_mount(&path).map_err(user_error)?;
            if mount.as_ref().is_some_and(|existing| existing != &path) {
                return Err(user_error(format!("Project {project} has inconsistent mounts in its named remotes")));
            }
            mount = Some(path);
        }
    }
    mount.map(Ok).unwrap_or_else(|| crate::native_project::default_mount(project).map_err(user_error))
}

/// Bind layout identity to a named remote, never to a versioned transport marker.
pub(crate) fn configure_attachment(
    repo_path: &std::path::Path,
    remote: &str,
    project: &str,
    mount: &jj_lib::repo_path::RepoPath,
    read_only: bool,
) -> Result<()> {
    crate::native_project::validate_project(project)?;
    let repo = gix::open(repo_path)?;
    ensure!(
        repo.object_hash() == gix::hash::Kind::Sha1,
        "Project attachment requires SHA-1"
    );
    repo.find_remote(remote)?;
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
    let mut config = repo.config_file_mut(repo.config_path(gix::config::Source::Local)?)?;
    config.set_raw_value(project_key.as_str(), project)?;
    config.set_raw_value(mount_key.as_str(), mount.as_internal_file_string())?;
    config.set_raw_value(
        format!("remote.{remote}.jjosh-readOnly").as_str(),
        if read_only { "true" } else { "false" },
    )?;
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

impl GitRemoteExtension for Extension {
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
            let mount = if let Some(path) =
                config_string(&git, &format!("remote.{}.jjosh-mount", remote.as_str()))
                    .map_err(user_error)?
            {
                crate::native_project::parse_mount(&path).map_err(user_error)?
            } else {
                project_mount(&git_path, &name)?
            };
            let mut native = match crate::native_project::native_project_for_mount(&transaction, &mount)
                .map_err(user_error)?
            {
                Some(existing) if existing == name => true,
                Some(existing) => {
                    return Err(user_error(format!(
                        "Mount {} belongs to native project {existing}, not {name}",
                        mount.as_internal_file_string(),
                    )));
                }
                None => false,
            };
            if !native {
                let url = if let Some(config) = &josh {
                    gix::url::parse(config.url.as_str()).map_err(user_error)?
                } else {
                    git.find_remote(remote.as_str()).map_err(user_error)?
                        .url(Direction::Fetch).cloned()
                        .ok_or_else(|| user_error("Project remote has no fetch URL"))?
                };
                if url.scheme == gix::url::Scheme::File {
                    let path = gix::path::from_bstr(url.path.as_ref());
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
}

impl Session {
    /// Rebind the named handle to Josh's real endpoints without losing transport configuration.
    pub fn remote<'repo>(
        &self,
        repo: &'repo gix::Repository,
        direction: Direction,
    ) -> Result<gix::Remote<'repo>> {
        if matches!(direction, Direction::Push) {
            ensure!(
                !repo
                    .config_snapshot()
                    .boolean(format!("remote.{}.jjosh-readOnly", self.name.as_str()).as_str())
                    .unwrap_or(false),
                "Remote {} has no publication endpoint; configure a writable named remote",
                self.name.as_str(),
            );
        }
        let mut remote = repo.find_remote(self.name.as_str())?;
        if let Some(config) = &self.josh {
            remote = remote
                .with_url(config.url.as_str())?
                .with_push_url(config.push_url.as_deref().unwrap_or(&config.url))?;
        }
        ensure!(
            remote.urls(direction).count() == 1,
            "Remote {} must have exactly one selected endpoint",
            self.name.as_str()
        );
        Ok(remote)
    }

    pub fn endpoint_url(&self, repo: &gix::Repository, direction: Direction) -> Result<String> {
        let remote = self.remote(repo, direction)?;
        let (mut url, _) = remote.sanitized_url_and_version(direction)?;
        url.canonicalize(repo.workdir().unwrap_or_else(|| repo.common_dir()))?;
        String::from_utf8(url.to_bstring().into()).context("Remote endpoint is not UTF-8")
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
    ) -> RemoteFuture<'a, Vec<GitRemoteObservation>> {
        Box::pin(fetch::run(self, ui, command, repo, selection))
    }

    fn push<'a>(
        &'a self,
        ui: &'a mut Ui,
        command: &'a CommandHelper,
        repo: &'a mut MutableRepo,
        targets: &'a GitPushRefTargets,
        options: &'a GitPushOptions,
        preparation: &'a GitRemotePushOptions,
        dry_run: bool,
    ) -> RemoteFuture<'a, jj_cli::git_remote::GitRemotePushOutcome> {
        Box::pin(push::run(
            self,
            ui,
            command,
            repo,
            targets,
            options,
            preparation,
            dry_run,
        ))
    }
}
