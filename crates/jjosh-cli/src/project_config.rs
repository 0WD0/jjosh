use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use gix::remote::Direction;
use jj_lib::repo_path::RepoPath;
use serde::Serialize;

use crate::native_project;

#[derive(Serialize)]
pub(crate) struct Inventory {
    pub(crate) projects: Vec<ProjectInfo>,
    pub(crate) remotes: Vec<RemoteInfo>,
    pub(crate) issues: Vec<Diagnostic>,
}

#[derive(Serialize)]
pub(crate) struct ProjectInfo {
    pub(crate) name: String,
    pub(crate) mount: Option<String>,
    pub(crate) native: bool,
    pub(crate) registered: bool,
    pub(crate) remotes: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct RemoteInfo {
    pub(crate) name: String,
    pub(crate) project: Option<String>,
    pub(crate) mount: Option<String>,
    pub(crate) filter: Option<String>,
    pub(crate) fetch_url: Option<String>,
    pub(crate) push_url: Option<String>,
    pub(crate) read_only: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct Diagnostic {
    pub(crate) subject: String,
    pub(crate) severity: Severity,
    pub(crate) message: String,
}

#[derive(PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Severity {
    Error,
    Warning,
}

struct ProjectState {
    info: ProjectInfo,
    mounts: BTreeSet<String>,
    filters: BTreeSet<String>,
}

impl ProjectState {
    fn new(name: &str) -> Self {
        Self {
            info: ProjectInfo {
                name: name.to_owned(),
                mount: None,
                native: false,
                registered: false,
                remotes: Vec::new(),
            },
            mounts: BTreeSet::new(),
            filters: BTreeSet::new(),
        }
    }
}

fn diagnose(
    issues: &mut Vec<Diagnostic>,
    subject: &str,
    severity: Severity,
    message: impl Into<String>,
) {
    issues.push(Diagnostic {
        subject: subject.to_owned(),
        severity,
        message: message.into(),
    });
}

fn collect<T>(issues: &mut Vec<Diagnostic>, subject: &str, result: Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            diagnose(issues, subject, Severity::Error, format!("{error:#}"));
            None
        }
    }
}

fn read_setting(
    git: &gix::Repository,
    remote: &str,
    setting: &str,
    issues: &mut Vec<Diagnostic>,
) -> Option<String> {
    let key = format!("remote.{remote}.{setting}");
    collect(
        issues,
        remote,
        crate::git_remote::config_string(git, &key).with_context(|| format!("Invalid {key}")),
    )
    .flatten()
}

/// Normalize an explicit source context without resolving or fetching it.
pub(crate) fn parse_base(value: &str) -> Result<String> {
    if let Ok(oid) = gix_hash::ObjectId::from_hex(value.as_bytes()) {
        return Ok(format!("pins/{oid}"));
    }
    let reference = if value.starts_with("refs/") {
        value.to_owned()
    } else {
        ensure!(!value.is_empty(), "Source base cannot be empty");
        format!("refs/heads/{value}")
    };
    gix_validate::reference::name_partial(reference.as_bytes().into())
        .with_context(|| format!("Invalid source base {value:?}"))?;
    Ok(reference)
}

/// Validate all recorded and remote-derived ownership without writing any state.
pub(crate) fn validate_registration(git_path: &Path, name: &str, mount: &RepoPath) -> Result<()> {
    native_project::validate_project(name)?;
    native_project::parse_mount(mount.as_internal_file_string())?;
    let git = gix::open(git_path)?;
    ensure!(
        git.object_hash() == gix::hash::Kind::Sha1,
        "Project registration requires SHA-1"
    );
    let transaction = crate::interop::open_josh_transaction(git_path, true)
        .map_err(|error| anyhow::anyhow!(error.error))?;
    let registered = native_project::list_registered_projects(&transaction)?;
    let native = native_project::list_native_projects(&transaction)?;
    let names: BTreeSet<_> = registered.into_iter().chain(native).collect();
    let mut mounts = BTreeMap::new();
    for existing in names {
        native_project::validate_project(&existing)?;
        mounts.insert(
            existing.clone(),
            native_project::load_mount(&transaction, &existing)?,
        );
    }
    let mut remote_projects = BTreeSet::new();
    for remote in git.remote_names() {
        let remote = std::str::from_utf8(&remote).context("Remote name is not UTF-8")?;
        let project =
            crate::git_remote::config_string(&git, &format!("remote.{remote}.jjosh-project"))?;
        let configured_mount =
            crate::git_remote::config_string(&git, &format!("remote.{remote}.jjosh-mount"))?;
        let Some(project) = project else {
            ensure!(
                configured_mount.is_none(),
                "Remote {remote} has a project mount without a project identity"
            );
            continue;
        };
        native_project::validate_project(&project)?;
        remote_projects.insert(project.clone());
        if let Some(value) = configured_mount {
            let existing = native_project::parse_mount(&value)?;
            if let Some(previous) = mounts.insert(project.clone(), existing.clone()) {
                ensure!(
                    previous == existing,
                    "Project {project} has conflicting registered or remote mounts"
                );
            }
        }
    }
    // Resolve omitted mounts only after collecting every explicit peer.
    for project in remote_projects {
        if let std::collections::btree_map::Entry::Vacant(entry) = mounts.entry(project) {
            let mount = native_project::default_mount(entry.key())?;
            entry.insert(mount);
        }
    }
    if let Some(existing) = mounts.get(name) {
        ensure!(
            existing.as_ref() == mount,
            "Project {name} is already mounted at {}",
            existing.as_internal_file_string()
        );
    } else {
        mounts.insert(name.to_owned(), mount.to_owned());
    }
    native_project::check_mounts_disjoint(
        mounts
            .iter()
            .map(|(project, mount)| (project.as_str(), mount.as_ref())),
    )?;
    Ok(())
}

/// Persist ownership after validation, independently of any remote configuration.
pub(crate) fn write_registration(git_path: &Path, name: &str, mount: &RepoPath) -> Result<()> {
    let transaction = crate::interop::open_josh_transaction(git_path, false)
        .map_err(|error| anyhow::anyhow!(error.error))?;
    native_project::record_mount(&transaction, name, mount)?;
    transaction.flush_mem_odb()?;
    Ok(())
}

/// Register ownership without creating a remote or selecting a conversion mode.
pub(crate) fn register(git_path: &Path, name: &str, mount: &RepoPath) -> Result<()> {
    validate_registration(git_path, name, mount)?;
    write_registration(git_path, name, mount)
}

/// Inspect local configuration only. Broken entries remain visible alongside
/// their diagnostics; they never silently become ordinary Git remotes.
pub(crate) fn inspect(git_path: &Path) -> Result<Inventory> {
    let git = gix::open(git_path)?;
    let transaction = crate::interop::open_josh_transaction(git_path, true)
        .map_err(|error| anyhow::anyhow!(error.error))?;
    let mut issues = Vec::new();
    let mut projects = BTreeMap::<String, ProjectState>::new();
    for name in native_project::list_registered_projects(&transaction)? {
        projects
            .entry(name.clone())
            .or_insert_with(|| ProjectState::new(&name))
            .info
            .registered = true;
    }
    for name in native_project::list_native_projects(&transaction)? {
        projects
            .entry(name.clone())
            .or_insert_with(|| ProjectState::new(&name))
            .info
            .native = true;
    }
    for (name, project) in &mut projects {
        collect(&mut issues, name, native_project::validate_project(name));
        if let Some(mount) = collect(
            &mut issues,
            name,
            native_project::load_mount(&transaction, name),
        ) {
            project
                .mounts
                .insert(mount.as_internal_file_string().to_owned());
        }
    }

    let mut configured = BTreeSet::new();
    for name in git.remote_names() {
        if let Some(name) = collect(
            &mut issues,
            "remotes",
            std::str::from_utf8(&name)
                .map(str::to_owned)
                .map_err(Into::into),
        ) {
            configured.insert(name);
        }
    }
    // Resolve omitted mounts from all explicit peers, not remote iteration order.
    // The main pass below reports malformed values rather than dropping them.
    for remote in &configured {
        let project =
            crate::git_remote::config_string(&git, &format!("remote.{remote}.jjosh-project"));
        let mount = crate::git_remote::config_string(&git, &format!("remote.{remote}.jjosh-mount"));
        if let (Ok(Some(project)), Ok(Some(mount))) = (project, mount)
            && native_project::parse_mount(&mount).is_ok()
        {
            projects
                .entry(project.clone())
                .or_insert_with(|| ProjectState::new(&project))
                .mounts
                .insert(mount);
        }
    }
    let mut sidecars = BTreeSet::new();
    let directory = git.common_dir().join("josh/remotes");
    match std::fs::read_dir(&directory) {
        Ok(entries) => {
            for entry in entries {
                let Some(entry) = collect(&mut issues, "josh/remotes", entry.map_err(Into::into))
                else {
                    continue;
                };
                let filename = entry.file_name();
                let Some(filename) = filename.to_str() else {
                    diagnose(
                        &mut issues,
                        "josh/remotes",
                        Severity::Error,
                        "Sidecar filename is not UTF-8",
                    );
                    continue;
                };
                if let Some(name) = filename.strip_suffix(".josh") {
                    sidecars.insert(name.to_owned());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => diagnose(
            &mut issues,
            "josh/remotes",
            Severity::Error,
            error.to_string(),
        ),
    }
    let names: BTreeSet<_> = configured.union(&sidecars).cloned().collect();
    let mut remotes = Vec::new();
    for name in names {
        let project = read_setting(&git, &name, "jjosh-project", &mut issues);
        let configured_mount = read_setting(&git, &name, "jjosh-mount", &mut issues);
        let base = read_setting(&git, &name, "jjosh-base", &mut issues);
        let read_only = collect(
            &mut issues,
            &name,
            crate::git_remote::remote_read_only(&git, jj_lib::ref_name::RemoteName::new(&name)),
        );
        let fetch_url = read_setting(&git, &name, "url", &mut issues);
        let push_url =
            read_setting(&git, &name, "pushurl", &mut issues).or_else(|| fetch_url.clone());
        let has_sidecar = sidecars.contains(&name);
        if has_sidecar && !configured.contains(&name) {
            diagnose(
                &mut issues,
                &name,
                Severity::Warning,
                "Stale Josh sidecar has no named Git remote",
            );
        }
        // Plain Git permits slash-containing aliases; only sidecar-backed
        // remotes must satisfy Josh's single-component naming rules.
        let config = if has_sidecar {
            collect(
                &mut issues,
                &name,
                josh_changes::remote_config::try_read_remote_config(git_path, &name),
            )
            .flatten()
        } else {
            None
        };
        let filter = config
            .as_ref()
            .map(|config| josh_core::filter::spec(config.semantic_filter()));
        let relevant =
            has_sidecar || project.is_some() || configured_mount.is_some() || base.is_some();
        if relevant && configured.contains(&name)
            && let Some(remote) = collect(
                &mut issues,
                &name,
                git.find_remote(name.as_str()).map_err(Into::into),
            ) {
                for (direction, label) in [(Direction::Fetch, "fetch"), (Direction::Push, "push")] {
                    if matches!(direction, Direction::Push) && read_only != Some(false) {
                        continue;
                    }
                    if remote.urls(direction).count() != 1 {
                        diagnose(
                            &mut issues,
                            &name,
                            Severity::Error,
                            format!("Remote must have exactly one selected {label} endpoint"),
                        );
                    }
                }
            }
        if project.is_none() && configured_mount.is_some() {
            diagnose(
                &mut issues,
                &name,
                Severity::Error,
                "Project mount has no project identity",
            );
        }
        if let Some(value) = &configured_mount {
            collect(&mut issues, &name, native_project::parse_mount(value));
        }
        let mut mount = configured_mount;
        if let Some(project_name) = &project {
            collect(
                &mut issues,
                &name,
                native_project::validate_project(project_name),
            );
            let state = projects
                .entry(project_name.clone())
                .or_insert_with(|| ProjectState::new(project_name));
            if mount.is_none() {
                mount = if state.info.registered || state.info.native {
                    collect(
                        &mut issues,
                        &name,
                        native_project::load_mount(&transaction, project_name),
                    )
                    .map(|path| path.as_internal_file_string().to_owned())
                } else if !state.mounts.is_empty() {
                    (state.mounts.len() == 1).then(|| state.mounts.first().unwrap().clone())
                } else {
                    collect(
                        &mut issues,
                        &name,
                        native_project::default_mount(project_name),
                    )
                    .map(|path| path.as_internal_file_string().to_owned())
                };
            }
            if let Some(mount) = &mount {
                if native_project::parse_mount(mount).is_ok() {
                    state.mounts.insert(mount.clone());
                }
                if state.info.native
                    && let Some(config) = &config
                {
                    let filter = config.semantic_filter();
                    if filter != josh_core::filter::Filter::new()
                        && filter != josh_core::filter::Filter::new().prefix(mount)
                    {
                        diagnose(
                            &mut issues,
                            &name,
                            Severity::Error,
                            "Native project has a source-changing Josh filter",
                        );
                    }
                }
            }
            if let Some(filter) = &filter {
                state.filters.insert(filter.clone());
            }
            state.info.remotes.push(name.clone());
        }
        remotes.push(RemoteInfo {
            name,
            project,
            mount,
            filter,
            fetch_url,
            push_url,
            read_only,
        });
    }

    let mut owners = BTreeMap::<String, Vec<String>>::new();
    for (name, project) in &mut projects {
        if project.mounts.len() > 1 {
            diagnose(
                &mut issues,
                name,
                Severity::Error,
                format!(
                    "Project has conflicting mounts: {}",
                    project
                        .mounts
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        } else {
            project.info.mount = project.mounts.first().cloned();
        }
        for mount in &project.mounts {
            owners.entry(mount.clone()).or_default().push(name.clone());
        }
        if project.filters.len() > 1 {
            diagnose(
                &mut issues,
                name,
                Severity::Error,
                "Project has conflicting source filters",
            );
        }
        let writable = remotes
            .iter()
            .filter(|remote| {
                remote.project.as_ref() == Some(name)
                    && remote.read_only == Some(false)
                    && remote.push_url.is_some()
            })
            .count();
        match writable {
            0 => diagnose(
                &mut issues,
                name,
                Severity::Warning,
                "Project has no writable publication route",
            ),
            1 => {}
            _ => diagnose(
                &mut issues,
                name,
                Severity::Warning,
                "Project has multiple writable publication routes; select a remote explicitly",
            ),
        }
    }
    for (mount, names) in owners {
        if names.len() > 1 {
            let message = format!(
                "Mount {mount} is claimed by multiple projects: {}",
                names.join(", ")
            );
            for name in names {
                diagnose(&mut issues, &name, Severity::Error, message.clone());
            }
        }
    }
    issues.sort_by(|left, right| {
        (&left.subject, &left.message).cmp(&(&right.subject, &right.message))
    });
    Ok(Inventory {
        projects: projects.into_values().map(|project| project.info).collect(),
        remotes,
        issues,
    })
}
