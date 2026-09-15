use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use gix::remote::Direction;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::ui::Ui;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::BindingId;
use jj_lib::project::BindingRecord;
use jj_lib::project::BindingTarget;
use jj_lib::project::ConnectionId;
use jj_lib::project::ProjectId;
use jj_lib::project::ProjectRecord;
use jj_lib::project::Representation;
use jj_lib::project::ScopedRemoteName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;

use crate::native_project;

pub(crate) struct Inventory {
    pub(crate) projects: Vec<ProjectInfo>,
    pub(crate) remotes: Vec<RemoteInfo>,
    pub(crate) issues: Vec<Diagnostic>,
}

pub(crate) struct ProjectInfo {
    pub(crate) name: String,
    pub(crate) mount: Option<String>,
    pub(crate) native: bool,
    pub(crate) registered: bool,
    pub(crate) remotes: Vec<String>,
}

pub(crate) struct RemoteInfo {
    pub(crate) name: String,
    pub(crate) project: Option<String>,
    pub(crate) mount: Option<String>,
    pub(crate) filter: Option<String>,
}

pub(crate) struct Diagnostic {
    pub(crate) subject: String,
    pub(crate) severity: Severity,
    pub(crate) message: String,
}

#[derive(PartialEq)]
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
        if relevant
            && configured.contains(&name)
            && let Some(remote) = collect(
                &mut issues,
                &name,
                git.find_remote(name.as_str()).map_err(Into::into),
            )
        {
            for (direction, label) in [(Direction::Fetch, "fetch"), (Direction::Push, "push")] {
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

fn rekey_view(
    view: &mut jj_lib::op_store::View,
    old: &jj_lib::ref_name::RemoteName,
    new: &jj_lib::ref_name::RemoteName,
) -> std::result::Result<(), String> {
    if old == new {
        return Ok(());
    }
    if view.remote_connections.contains_key(new) {
        return Err(format!("Remote {new:?} already has a logical connection"));
    }
    for namespace in ["refs/remotes/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE] {
        let prefix = format!("{namespace}{}/", new.as_str());
        if view
            .git_refs
            .keys()
            .any(|name| name.as_str().starts_with(&prefix))
        {
            return Err(format!("Remote {new:?} already has Git reference mirrors"));
        }
    }
    jj_lib::view::remote_observations::RemoteObservations::new(view).relocate(old, new)?;
    if let Some(owner) = view.remote_connections.remove(old) {
        view.remote_connections.insert(new.to_owned(), owner);
    }
    for namespace in ["refs/remotes/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE] {
        let old_prefix = format!("{namespace}{}/", old.as_str());
        let new_prefix = format!("{namespace}{}/", new.as_str());
        view.git_refs = std::mem::take(&mut view.git_refs)
            .into_iter()
            .map(|(name, target)| {
                if let Some(suffix) = name.as_str().strip_prefix(&old_prefix) {
                    (format!("{new_prefix}{suffix}").into(), target)
                } else {
                    (name, target)
                }
            })
            .collect();
    }
    Ok(())
}

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    /// Show the complete adoption and retirement plan without writing state (default).
    #[arg(long, conflicts_with = "apply")]
    dry_run: bool,
    /// Commit project definitions and bindings, then retire legacy authority.
    #[arg(long)]
    apply: bool,
    /// Acknowledge that all incompatible writers have stopped before upgrading legacy storage.
    #[arg(long, requires = "apply")]
    exclusive: bool,
    /// Explicit unmounted representation for a legacy remote; repeat as needed.
    #[arg(long, value_name = "REMOTE=whole|filter:EXPR|view:PATH")]
    representation: Vec<String>,
    /// Assign project-wide native evidence to a whole binding, or keep it disconnected.
    #[arg(long, value_name = "PROJECT=REMOTE|detached")]
    native_source: Vec<String>,
    /// Explicitly forget unverifiable legacy remote observations and tracking.
    /// Local bookmarks/tags, source objects, shallow state and leases are retained.
    #[arg(long, value_name = "REMOTE")]
    clear_observations: Vec<String>,
}

fn mappings(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for value in values {
        let (name, value) = value
            .split_once('=')
            .context("Mapping must be NAME=VALUE")?;
        ensure!(
            !name.is_empty() && !value.is_empty(),
            "Mapping name and value cannot be empty"
        );
        ensure!(
            result.insert(name.to_owned(), value.to_owned()).is_none(),
            "Duplicate mapping for {name}"
        );
    }
    Ok(result)
}

/// Capture only the mutable Git inputs interpreted by migration: remote
/// configuration, legacy project evidence, remote mirrors, and Josh sidecars.
/// Ordinary branches, tags, operation heads, and unrelated Git configuration
/// are not prerequisites for applying this plan.
fn migration_inputs(
    git: &gix::Repository,
) -> Result<(
    Vec<u8>,
    BTreeMap<gix::refs::FullName, gix::refs::Target>,
    BTreeMap<std::ffi::OsString, Vec<u8>>,
)> {
    let mut config = Vec::new();
    git.config_snapshot()
        .write_to_filter(&mut config, |section| {
            section.header().name().eq_ignore_ascii_case(b"remote")
        })?;
    let mut refs = BTreeMap::new();
    for prefix in [
        "refs/jjosh/native/",
        "refs/remotes/",
        jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
    ] {
        for reference in git.references()?.prefixed(prefix)? {
            let reference = reference.map_err(anyhow::Error::from_boxed)?;
            refs.insert(reference.name().to_owned(), reference.target().into_owned());
        }
    }
    let mut sidecars = BTreeMap::new();
    match std::fs::read_dir(git.common_dir().join("josh/remotes")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "josh")
                {
                    sidecars.insert(entry.file_name(), std::fs::read(entry.path())?);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok((config, refs, sidecars))
}

/// Reconcile only physical names belonging to an existing, resolved connection.
/// This is an in-memory plan: interrupted cutovers never publish an operation
/// merely to make their old physical name current again.
fn reconcile_remote_names(
    git: &gix::Repository,
    view: &mut jj_lib::op_store::View,
    inventory: &mut Inventory,
) -> Result<BTreeMap<String, String>, CommandError> {
    let mut aliases = BTreeMap::new();
    let configured: BTreeSet<_> = git
        .remote_names()
        .iter()
        .map(|name| std::str::from_utf8(name).map(str::to_owned))
        .collect::<std::result::Result<_, _>>()
        .map_err(user_error)?;
    let mut owners = BTreeMap::new();
    for name in &configured {
        let remote = jj_lib::ref_name::RemoteName::new(name);
        let connection = jj_lib::git::remote_connection_id(git, remote).map_err(user_error)?;
        let connection = connection
            .or_else(|| {
                view.remote_connections
                    .get(remote)
                    .and_then(Merge::as_resolved)
                    .and_then(Option::as_ref)
                    .cloned()
            })
            .or_else(|| {
                let mut matches =
                    view.project_state
                        .remote_names
                        .iter()
                        .filter_map(|(id, alias)| {
                            alias
                                .as_resolved()
                                .and_then(Option::as_ref)
                                .filter(|alias| alias.name == remote)
                                .map(|_| id.clone())
                        });
                let first = matches.next();
                first.filter(|_| matches.next().is_none())
            });
        let Some(connection) = connection else {
            continue;
        };
        if let Some(other) = owners.insert(connection.clone(), name.clone()) {
            return Err(user_error(format!(
                "Connection {} is configured at both {other} and {name}; resolve ownership before \
                 migration",
                connection.hex()
            )));
        }
        let logical: Vec<_> = view
            .remote_connections
            .iter()
            .filter(|(_, owner)| owner.as_resolved().and_then(Option::as_ref) == Some(&connection))
            .map(|(name, _)| name.clone())
            .collect();
        if logical.len() > 1 {
            return Err(user_error(
                "Migration connection has multiple logical physical owners",
            ));
        }
        if let Some(old) = logical.first()
            && old != remote
        {
            if configured.contains(old.as_str()) {
                return Err(user_error(format!(
                    "Remote {} still has independent configuration",
                    old.as_str()
                )));
            }
            if !view.project_state.remote_names.contains_key(&connection)
                && let Some((_, binding)) = view
                    .project_state
                    .binding_for_connection(&connection)
                    .map_err(user_error)?
                && let BindingTarget::Project(project) = &binding.target
            {
                view.project_state.remote_names.insert(
                    connection.clone(),
                    Merge::resolved(Some(ScopedRemoteName {
                        project: project.clone(),
                        name: old.clone(),
                    })),
                );
            }
            rekey_view(view, old, remote).map_err(user_error)?;
        }
        if let Some(alias) = view
            .project_state
            .remote_names
            .get(&connection)
            .and_then(Merge::as_resolved)
            .and_then(Option::as_ref)
        {
            aliases.insert(alias.name.as_str().to_owned(), name.clone());
        }
    }
    // Sidecars are deliberately retained under their original spelling until
    // every new reference is installed. A completed physical rename can leave
    // such a sidecar beside the new opaque handle.
    let mut moved = Vec::new();
    for (old, new) in &aliases {
        if old == new || configured.contains(old) {
            continue;
        }
        if let Some(index) = inventory
            .remotes
            .iter()
            .position(|remote| &remote.name == old)
        {
            moved.push((new.clone(), inventory.remotes.remove(index)));
        }
    }
    for (new, old) in moved {
        let remote = inventory
            .remotes
            .iter_mut()
            .find(|remote| remote.name == new)
            .ok_or_else(|| user_error("Migrated sidecar has no configured connection"))?;
        if remote.filter.is_some() && remote.filter != old.filter {
            return Err(user_error("Old and rekeyed remote sidecars disagree"));
        }
        remote.filter = old.filter;
    }
    for remote in &mut inventory.remotes {
        if remote.filter.is_none() {
            continue;
        }
        if remote.project.is_none() {
            let Some(connection) = jj_lib::git::remote_connection_id(
                git,
                jj_lib::ref_name::RemoteName::new(&remote.name),
            )
            .map_err(user_error)?
            else {
                continue;
            };
            let Some((_, binding)) = view
                .project_state
                .binding_for_connection(&connection)
                .map_err(user_error)?
            else {
                continue;
            };
            if let BindingTarget::Project(project) = &binding.target {
                let record = view
                    .project_state
                    .projects
                    .get(project)
                    .and_then(Merge::as_resolved)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| user_error("Resolve the migrated sidecar's project first"))?;
                remote.project = Some(record.name.clone());
                remote.mount = Some(record.canonical_root.as_internal_file_string().to_owned());
                if !inventory
                    .projects
                    .iter()
                    .any(|project| project.name == record.name)
                {
                    inventory.projects.push(ProjectInfo {
                        name: record.name.clone(),
                        mount: remote.mount.clone(),
                        native: false,
                        registered: false,
                        remotes: vec![remote.name.clone()],
                    });
                }
            }
        }
    }
    Ok(aliases)
}

fn retire_sidecar(path: &Path, expected: &[u8]) -> Result<(), CommandError> {
    let _lock = gix::lock::File::acquire_to_update_resource(
        path,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .map_err(user_error)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || std::fs::read(path)? != expected {
                return Err(user_error(format!(
                    "Legacy sidecar {} changed; retained for explicit resolution",
                    path.display()
                )));
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Finish a config-first native rename interrupted before its mirror ref edits.
/// Exact identity and expected targets are the proof; no saved transaction is needed.
fn finish_rekeyed_mirrors(
    git: &gix::Repository,
    view: &jj_lib::op_store::View,
) -> Result<(), CommandError> {
    use gix::refs::transaction::Change;
    use gix::refs::transaction::LogChange;
    use gix::refs::transaction::PreviousValue;
    use gix::refs::transaction::RefEdit;
    use gix::refs::transaction::RefLog;
    let _config_lock = gix::lock::Marker::acquire_to_hold_resource(
        git.common_dir().join("config"),
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .map_err(user_error)?;
    let git = gix::open(git.path()).map_err(user_error)?;
    for (connection, alias) in &view.project_state.remote_names {
        let Some(alias) = alias.as_resolved().and_then(Option::as_ref) else {
            continue;
        };
        let new: RemoteNameBuf = format!("jjosh-{}", connection.hex()).into();
        if alias.name == new
            || git.try_find_remote(alias.name.as_str()).is_some()
            || jj_lib::git::remote_connection_id(&git, &new)
                .map_err(user_error)?
                .as_ref()
                != Some(connection)
        {
            continue;
        }
        let mut edits = Vec::new();
        for namespace in ["refs/remotes/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE] {
            let old_prefix = format!("{namespace}{}/", alias.name.as_str());
            for reference in git
                .references()
                .map_err(user_error)?
                .prefixed(old_prefix.as_str())
                .map_err(user_error)?
            {
                let reference = reference.map_err(user_error)?;
                let target = reference.target().into_owned();
                let full_name =
                    std::str::from_utf8(reference.name().as_bstr()).map_err(user_error)?;
                let new_name = format!(
                    "{namespace}{}/{}",
                    new.as_str(),
                    &full_name[old_prefix.len()..]
                );
                let existing = git.try_find_reference(&new_name).map_err(user_error)?;
                if existing
                    .as_ref()
                    .is_some_and(|reference| reference.target().into_owned() != target)
                {
                    return Err(user_error(format!(
                        "Interrupted migration mirror {new_name} has changed; both targets \
                         retained"
                    )));
                }
                edits.push(RefEdit {
                    name: new_name.try_into().map_err(user_error)?,
                    change: Change::Update {
                        log: LogChange {
                            mode: RefLog::AndReference,
                            force_create_reflog: false,
                            message: "finish project migration rename".into(),
                        },
                        expected: existing.map_or(PreviousValue::MustNotExist, |_| {
                            PreviousValue::MustExistAndMatch(target.clone())
                        }),
                        new: target.clone(),
                    },
                    deref: false,
                });
                edits.push(RefEdit {
                    name: reference.name().to_owned(),
                    change: Change::Delete {
                        expected: PreviousValue::MustExistAndMatch(target),
                        log: RefLog::AndReference,
                    },
                    deref: false,
                });
            }
        }
        git.edit_references(edits).map_err(user_error)?;
    }
    Ok(())
}

/// Explicit migration is the only reader of the previous configuration model.
/// Planning is read-only. Stage additive evidence and inactive transport markers,
/// publish logical identities, then rekey and retire with resource-local checks.
pub(crate) async fn run(ui: &Ui, command: &CommandHelper, args: &Args) -> Result<(), CommandError> {
    if !command.is_at_head_operation()
        || (args.apply && command.global_args().no_integrate_operation)
    {
        return Err(user_error(
            "Project migration requires the current integrated operation",
        ));
    }
    let mut workspace = crate::project::recorded_workspace(ui, command).await?;
    let git_path = crate::interop::sha1_git_repo_path(&workspace)?;
    let git = jj_lib::git::get_git_backend(workspace.repo().store())?.git_repo();
    let planned_inputs = migration_inputs(&git).map_err(user_error)?;
    let mut inventory = inspect(&git_path).map_err(user_error)?;
    let mut explicit = mappings(&args.representation).map_err(user_error)?;
    let mut native_sources = mappings(&args.native_source).map_err(user_error)?;
    let mut clear: BTreeSet<_> = args.clear_observations.iter().cloned().collect();
    let mut view = workspace.repo().view().store_view().clone();
    let aliases = reconcile_remote_names(&git, &mut view, &mut inventory)?;
    for (old, new) in &aliases {
        if old == new {
            continue;
        }
        if let Some(value) = explicit.remove(old) {
            if explicit.insert(new.clone(), value).is_some() {
                return Err(user_error(
                    "Representation specified by both physical and scoped name",
                ));
            }
        }
        if clear.remove(old) {
            clear.insert(new.clone());
        }
        for source in native_sources.values_mut() {
            if source == old {
                *source = new.clone();
            }
        }
    }
    let store_upgrade = workspace
        .repo()
        .op_store()
        .downcast_ref::<jj_lib::simple_op_store::SimpleOpStore>()
        .map(|store| store.requires_project_store_upgrade())
        .transpose()?
        .unwrap_or(false);
    if store_upgrade {
        writeln!(
            ui.status(),
            "Storage requires an exclusive project-aware format upgrade; stop incompatible \
             writers before --apply --exclusive"
        )?;
    }
    let mut blockers = Vec::new();
    // Explicit adoption frees the root namespace by assigning an opaque Git
    // handle. Connection IDs, logical spelling, bindings and evidence survive.
    let mut alias_remotes = BTreeMap::new();
    let mut owners = view.remote_connections.clone();
    for name in git.remote_names() {
        let name: RemoteNameBuf = std::str::from_utf8(&name).map_err(user_error)?.into();
        if let Some(connection) =
            jj_lib::git::remote_connection_id(&git, &name).map_err(user_error)?
        {
            if let Some(owner) = owners.get(&name)
                && owner != &Merge::resolved(Some(connection.clone()))
            {
                blockers.push(format!(
                    "Remote {} has conflicting operation/config connection ownership",
                    name.as_str()
                ));
                continue;
            }
            owners.insert(name, Merge::resolved(Some(connection)));
        }
    }
    for (remote, owner) in &owners {
        let Some(connection) = owner.as_resolved().and_then(Option::as_ref) else {
            if !owner.is_resolved() {
                blockers.push(format!(
                    "Remote {} has unresolved connection ownership",
                    remote.as_str()
                ));
            }
            continue;
        };
        let Some((_, binding)) = view
            .project_state
            .binding_for_connection(connection)
            .map_err(user_error)?
        else {
            continue;
        };
        let BindingTarget::Project(project) = &binding.target else {
            continue;
        };
        if let Some(alias) = view.project_state.remote_names.get(connection) {
            if alias
                .as_resolved()
                .and_then(Option::as_ref)
                .is_none_or(|alias| &alias.project != project)
            {
                blockers.push(format!(
                    "Remote {} has conflicting scoped alias metadata",
                    remote.as_str()
                ));
            }
            if remote.as_str() != format!("jjosh-{}", connection.hex()) {
                alias_remotes.insert(remote.clone(), connection.clone());
            }
            continue;
        }
        if owners.iter().any(|(other, owner)| {
            other != remote && owner.iter().flatten().any(|id| id == connection)
        }) {
            blockers.push(format!(
                "Remote {} shares its connection identity with another physical remote",
                remote.as_str()
            ));
            continue;
        }
        let alias = ScopedRemoteName {
            project: project.clone(),
            name: remote.clone(),
        };
        writeln!(
            ui.status(),
            "Adopt scoped remote {}#{} (connection {}, bindings, evidence and tracking unchanged)",
            remote.as_str(),
            view.project_state.projects[project]
                .as_resolved()
                .and_then(Option::as_ref)
                .expect("validated project")
                .name,
            connection.hex()
        )?;
        view.project_state
            .remote_names
            .insert(connection.clone(), Merge::resolved(Some(alias)));
        view.remote_connections
            .insert(remote.clone(), Merge::resolved(Some(connection.clone())));
        alias_remotes.insert(remote.clone(), connection.clone());
    }
    // Legacy project-wide filter diagnostics are superseded by explicit
    // per-binding definitions below. Structural corruption is never overridden.
    for issue in &inventory.issues {
        if issue.severity == Severity::Error
            && issue.message != "Project has conflicting source filters"
            && !(issue.message == "Native project has a source-changing Josh filter"
                && explicit.contains_key(&issue.subject))
        {
            blockers.push(format!("{}: {}", issue.subject, issue.message));
        }
    }
    let mut projects = BTreeMap::new();
    let mut native_projects = BTreeSet::new();
    for project in &inventory.projects {
        if project.native {
            native_projects.insert(project.name.clone());
        }
        let mount = project.mount.as_deref().unwrap_or(&project.name);
        let mount = native_project::parse_mount(mount).map_err(user_error)?;
        let id = match view.project_state.project_by_name(&project.name) {
            Ok((id, record)) if record.canonical_root == mount => id,
            Ok(_) => {
                return Err(user_error(format!(
                    "Project {} already has a different canonical root",
                    project.name
                )));
            }
            Err(_)
                if view.project_state.projects.values().any(|value| {
                    value
                        .adds()
                        .flatten()
                        .any(|record| record.name == project.name)
                }) =>
            {
                return Err(user_error(format!(
                    "Resolve existing project {} before migration",
                    project.name
                )));
            }
            Err(_) => {
                let id = ProjectId::generate();
                view.project_state.projects.insert(
                    id.clone(),
                    Merge::resolved(Some(ProjectRecord {
                        name: project.name.clone(),
                        canonical_root: mount.clone(),
                    })),
                );
                id
            }
        };
        writeln!(
            ui.status(),
            "Project {} -> {} (materialized canonical root)",
            project.name,
            mount.as_internal_file_string()
        )?;
        projects.insert(project.name.clone(), id);
    }
    // Older native imports used remote-view records as explicit raw/canonical
    // pairs. Their suffix names are evidence, unlike ordinary remote observations.
    let mut legacy_pairs = Vec::new();
    let mut legacy_mirrors = Vec::new();
    // These reserved refs may not have been imported into a View yet. Read their
    // explicit correspondence records directly rather than routing them through
    // ordinary Git import, which correctly rejects unproven project mirrors.
    for reference in git
        .references()
        .map_err(user_error)?
        .prefixed("refs/remotes/jjosh-native-")
        .map_err(user_error)?
    {
        let reference = reference.map_err(user_error)?;
        let full_name = std::str::from_utf8(reference.name().as_bstr()).map_err(user_error)?;
        let (project, name) = full_name
            .strip_prefix("refs/remotes/jjosh-native-")
            .unwrap()
            .split_once('/')
            .ok_or_else(|| user_error("Invalid legacy native correspondence ref"))?;
        native_project::validate_project(project).map_err(user_error)?;
        let remote: RemoteNameBuf = format!("jjosh-native-{project}").into();
        let raw_target = reference.target();
        let id = raw_target
            .try_id()
            .ok_or_else(|| user_error("Legacy native correspondence refs must be direct"))?;
        let target = jj_lib::op_store::RefTarget::normal(jj_lib::backend::CommitId::from_bytes(
            id.as_bytes(),
        ));
        let refs = view.remote_views.entry(remote).or_default();
        if let Some(existing) = refs.bookmarks.get(jj_lib::ref_name::RefName::new(name)) {
            if existing.target != target {
                return Err(user_error(format!(
                    "Legacy native ref {full_name} disagrees with its recorded observation"
                )));
            }
        } else {
            refs.bookmarks.insert(
                name.into(),
                jj_lib::op_store::RemoteRef {
                    target,
                    state: jj_lib::op_store::RemoteRefState::New,
                },
            );
        }
    }
    let legacy_remotes: Vec<_> = view
        .remote_views
        .keys()
        .filter(|name| name.as_str().starts_with("jjosh-native-"))
        .cloned()
        .collect();
    for remote in &legacy_remotes {
        let project = remote.as_str().strip_prefix("jjosh-native-").unwrap();
        native_project::validate_project(project).map_err(user_error)?;
        if git.find_remote(remote.as_str()).is_ok() {
            blockers.push(format!(
                "{} is a configured remote; rename it before migrating legacy native evidence",
                remote.as_str()
            ));
            continue;
        }
        if view
            .remote_connections
            .get(remote)
            .is_some_and(|owner| owner.iter().flatten().next().is_some())
            || view
                .project_observations
                .keys()
                .any(|key| key.remote == *remote)
        {
            blockers.push(format!(
                "{} has new-format connection ownership; do not reinterpret it as a legacy \
                 correspondence namespace",
                remote.as_str()
            ));
            continue;
        }
        if !projects.contains_key(project) {
            let id = if let Ok((id, _)) = view.project_state.project_by_name(project) {
                id
            } else {
                let id = ProjectId::generate();
                let mount = native_project::default_mount(project).map_err(user_error)?;
                view.project_state.projects.insert(
                    id.clone(),
                    Merge::resolved(Some(ProjectRecord {
                        name: project.to_owned(),
                        canonical_root: mount,
                    })),
                );
                writeln!(
                    ui.status(),
                    "Project {project} -> {project} (legacy native default)"
                )?;
                id
            };
            projects.insert(project.to_owned(), id);
        }
        native_projects.insert(project.to_owned());
        let refs = &view.remote_views[remote];
        if !refs.tags.is_empty() {
            blockers.push(format!(
                "{} has tags; resolve these non-correspondence records first",
                remote.as_str()
            ));
        }
        for (name, reference) in &refs.bookmarks {
            let (kind, raw) = name
                .as_str()
                .split_once('/')
                .ok_or_else(|| user_error("Invalid legacy native correspondence name"))?;
            if !matches!(kind, "origin" | "published")
                || reference.state == jj_lib::op_store::RemoteRefState::Tracked
            {
                return Err(user_error(format!(
                    "Invalid or tracked native correspondence {}@{}; untrack it first",
                    name.as_str(),
                    remote.as_str()
                )));
            }
            let raw = jj_lib::backend::CommitId::try_from_hex(raw)
                .filter(|id| id.as_bytes().len() == 20)
                .ok_or_else(|| user_error("Invalid legacy native source commit ID"))?;
            let canonical = reference.target.as_normal().ok_or_else(|| {
                user_error("Resolve conflicted native correspondences before migrating")
            })?;
            workspace.repo().store().get_commit(&raw)?;
            workspace.repo().store().get_commit(canonical)?;
            writeln!(
                ui.status(),
                "Adopt native {project}/{kind}: {} -> {}",
                raw.hex(),
                canonical.hex()
            )?;
            legacy_pairs.push((project.to_owned(), kind.to_owned(), raw, canonical.clone()));
        }
        view.remote_views.remove(remote);
        let mirror: RemoteNameBuf = format!("{project}-git").into();
        // The retired native importer made this unconfigured synthetic mirror.
        // Only exact local-target duplicates are its disposable bookkeeping.
        if git.find_remote(mirror.as_str()).is_err()
            && !view
                .remote_connections
                .get(&mirror)
                .is_some_and(|owner| owner.iter().flatten().next().is_some())
            && !view
                .project_observations
                .keys()
                .any(|key| key.remote == mirror)
        {
            for (prefix, local) in [
                (
                    format!("refs/remotes/{}/", mirror.as_str()),
                    &view.local_bookmarks,
                ),
                (
                    format!(
                        "{}{}/",
                        jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
                        mirror.as_str()
                    ),
                    &view.local_tags,
                ),
            ] {
                for reference in git
                    .references()
                    .map_err(user_error)?
                    .prefixed(prefix.as_str())
                    .map_err(user_error)?
                {
                    let reference = reference.map_err(user_error)?;
                    let full_name =
                        std::str::from_utf8(reference.name().as_bstr()).map_err(user_error)?;
                    let name =
                        jj_lib::ref_name::RefName::new(full_name.strip_prefix(&prefix).unwrap());
                    if let Some(id) = reference.target().try_id()
                        && local
                            .get(name)
                            .and_then(jj_lib::op_store::RefTarget::as_normal)
                            .is_some_and(|canonical| canonical.as_bytes() == id.as_bytes())
                    {
                        legacy_mirrors.push(full_name.to_owned());
                    }
                }
            }
            if let Some(refs) = view.remote_views.get_mut(&mirror) {
                for (references, local) in [
                    (&mut refs.bookmarks, &view.local_bookmarks),
                    (&mut refs.tags, &view.local_tags),
                ] {
                    references.retain(|name, reference| {
                        if local.get(name) != Some(&reference.target) {
                            return true;
                        }
                        // Physical retirement was selected above from its current
                        // target, not this potentially stale observation.
                        false
                    });
                }
                if refs.bookmarks.is_empty() && refs.tags.is_empty() {
                    view.remote_views.remove(&mirror);
                }
            }
        }
    }
    for (name, id) in &projects {
        match view.project_state.resolve_label(name).map_err(user_error)? {
            Some(existing) if existing != *id => {
                return Err(user_error(format!(
                    "Label {name} belongs to another project"
                )));
            }
            Some(_) => {}
            None => {
                let suffix = format!("#{name}");
                for reference in view
                    .local_bookmarks
                    .keys()
                    .chain(view.local_tags.keys())
                    .filter(|reference| reference.as_str().ends_with(&suffix))
                {
                    writeln!(
                        ui.status(),
                        "Adopt local reference {} as project label {name}",
                        reference.as_str()
                    )?;
                }
                for (remote, refs) in &view.remote_views {
                    for (reference, target) in refs
                        .bookmarks
                        .iter()
                        .chain(refs.tags.iter())
                        .filter(|(reference, _)| reference.as_str().ends_with(&suffix))
                    {
                        writeln!(
                            ui.status(),
                            "Adopt remote reference {}@{} ({:?}) as project label {name}",
                            reference.as_str(),
                            remote.as_str(),
                            target.state
                        )?;
                    }
                }
                view.project_state
                    .labels
                    .insert(name.clone(), Merge::resolved(Some(id.clone())));
            }
        }
    }
    for reference in &legacy_mirrors {
        writeln!(
            ui.status(),
            "Retire legacy native mirror {reference}; identical local target retained"
        )?;
    }
    let mut updates = Vec::new();
    let config = git.config_snapshot();
    let mut retired_config_remotes = BTreeSet::new();
    for section in config.sections_by_name("remote").into_iter().flatten() {
        if section
            .value_names()
            .any(|name| name.eq_ignore_ascii_case("jjosh-readOnly"))
            && let Some(name) = section.header().subsection_name()
        {
            let name = std::str::from_utf8(name).map_err(user_error)?;
            retired_config_remotes.insert(RemoteNameBuf::from(name));
        }
    }
    // This is config retirement, not legacy source adoption. In particular,
    // operation-owned bindings and their observations must remain untouched.
    for remote in &retired_config_remotes {
        updates.push((remote.clone(), "jjosh-readOnly".to_owned(), None));
        writeln!(
            ui.status(),
            "Retire obsolete remote.{}.jjosh-readOnly; source definitions and observations \
             unchanged",
            remote.as_str()
        )?;
    }
    let mut sidecars = Vec::new();
    let mut remote_bindings = BTreeMap::new();
    let mut cleared = Vec::new();
    let mut legacy_representations = BTreeMap::new();
    let mut filtered_projects = BTreeSet::new();
    for remote in &inventory.remotes {
        let representation = if let Some(filter) = &remote.filter {
            // Older native peers explicitly accepted the identity sidecar as
            // whole-project, as well as the already-prefixed identity filter.
            if remote
                .project
                .as_ref()
                .is_some_and(|project| native_projects.contains(project))
                && josh_core::filter::parse(filter).map_err(user_error)?
                    == josh_core::filter::Filter::new()
            {
                Ok(Representation::Whole)
            } else if let Some(mount) = &remote.mount {
                crate::binding_config::unmount_legacy(filter, mount)
            } else {
                crate::binding_config::parse_filter(filter)
            }
        } else {
            Ok(Representation::Whole)
        };
        if representation
            .as_ref()
            .is_ok_and(|value| value != &Representation::Whole)
            && let Some(project) = &remote.project
        {
            filtered_projects.insert(project.clone());
        }
        legacy_representations.insert(remote.name.clone(), representation);
    }
    for remote in &inventory.remotes {
        if remote.project.is_none()
            && remote.filter.is_none()
            && !explicit.contains_key(&remote.name)
            && !clear.contains(&remote.name)
        {
            continue;
        }
        let name: RemoteNameBuf = remote.name.clone().into();
        if git.find_remote(name.as_str()).is_err() {
            blockers.push(format!(
                "Orphan sidecar {}: explicitly remove or configure its remote before migration",
                remote.name
            ));
            continue;
        }
        let specified = explicit.remove(&remote.name);
        let connection = jj_lib::git::remote_connection_id(&git, &name)
            .map_err(user_error)?
            .or_else(|| {
                view.remote_connections
                    .get(&name)
                    .and_then(Merge::as_resolved)
                    .and_then(Option::as_ref)
                    .cloned()
            })
            .unwrap_or_else(ConnectionId::generate);
        let existing_binding = view
            .project_state
            .binding_for_connection(&connection)
            .map_err(user_error)?
            .map(|(id, binding)| (id, binding.clone()));
        let representation = if let Some(value) = &specified {
            crate::binding_config::parse_mapping(value).map_err(user_error)?
        } else if remote.project.is_none()
            && remote.filter.is_none()
            && let Some((_, binding)) = &existing_binding
        {
            binding.representation.clone()
        } else {
            match legacy_representations
                .remove(&remote.name)
                .expect("inventory representation was prepared")
            {
                Ok(value) => value,
                Err(error) => {
                    blockers.push(format!("{}: {error:#}", remote.name));
                    continue;
                }
            }
        };
        if specified.is_none()
            && remote.filter.is_none()
            && remote
                .project
                .as_ref()
                .is_some_and(|project| filtered_projects.contains(project))
        {
            blockers.push(format!(
                "Remote {} fetched whole-project but could publish using a filtered peer; choose \
                 --representation {}=whole or {}=filter:SOURCE explicitly",
                remote.name, remote.name, remote.name
            ));
        }
        let target = remote
            .project
            .as_ref()
            .map(|project| BindingTarget::Project(projects[project].clone()))
            .or_else(|| {
                existing_binding
                    .as_ref()
                    .map(|(_, binding)| binding.target.clone())
            })
            .unwrap_or(BindingTarget::RepositoryView);
        let base =
            crate::git_remote::config_string(&git, &format!("remote.{}.jjosh-base", remote.name))
                .map_err(user_error)?
                .as_deref()
                .map(crate::binding_config::parse_base)
                .transpose()
                .map_err(user_error)?
                .or_else(|| {
                    existing_binding
                        .as_ref()
                        .and_then(|(_, binding)| binding.base.clone())
                });
        let binding = BindingRecord {
            target,
            connection_id: connection.clone(),
            representation,
            base,
        };
        let id = if let Some((id, existing)) = existing_binding {
            if existing != binding {
                return Err(user_error(format!(
                    "Remote {} legacy inputs disagree with its retained binding; resolve them \
                     explicitly before retrying",
                    remote.name
                )));
            }
            id
        } else {
            BindingId::generate()
        };
        writeln!(
            ui.status(),
            "Bind {}: {:?}; base={:?}; endpoints/refspec/auth unchanged",
            remote.name,
            binding.representation,
            binding.base
        )?;
        if let Some(refs) = view.remote_views.get(&name)
            && !clear.contains(&remote.name)
            && (!refs.bookmarks.is_empty() || !refs.tags.is_empty())
        {
            // A recorded reference and its tracking state are historical cache,
            // not proof of a raw source, endpoint, or conversion generation.
            // Retain them without fabricating project_observations or leases.
            // Publication must still satisfy its independent evidence checks.
            writeln!(
                ui.status(),
                "Preserve {} legacy bookmarks and {} tags for {} with their tracking state; no \
                 conversion evidence or publication lease inferred",
                refs.bookmarks.len(),
                refs.tags.len(),
                name.as_str(),
            )?;
        }
        if clear.remove(&remote.name) {
            view.remote_views.remove(&name);
            view.project_observations
                .retain(|key, _| key.remote != name);
            cleared.push(name.clone());
            writeln!(
                ui.status(),
                "Clear {} remote observations, tracking and canonical mirrors; retain all local \
                 references and source/lease state",
                name.as_str()
            )?;
        }
        view.remote_connections
            .insert(name.clone(), Merge::resolved(Some(connection.clone())));
        if let BindingTarget::Project(project) = &binding.target {
            view.project_state
                .remote_names
                .entry(connection.clone())
                .or_insert_with(|| {
                    Merge::resolved(Some(ScopedRemoteName {
                        project: project.clone(),
                        name: name.clone(),
                    }))
                });
            alias_remotes.insert(name.clone(), connection.clone());
            writeln!(
                ui.status(),
                "Adopt scoped remote {}#{}; existing name preserved verbatim",
                name.as_str(),
                view.project_state.projects[project]
                    .as_resolved()
                    .and_then(Option::as_ref)
                    .ok_or_else(|| user_error("Resolve the migrated binding's project first"))?
                    .name
            )?;
        }
        view.project_state
            .bindings
            .insert(id.clone(), Merge::resolved(Some(binding.clone())));
        remote_bindings.insert(remote.name.clone(), (id, binding));
        updates.push((
            name.clone(),
            "jjosh-connectionId".to_owned(),
            Some(connection.hex()),
        ));
        updates.push((
            name.clone(),
            "jjosh-requiredCapability".to_owned(),
            Some("jjosh-v1".to_owned()),
        ));
        for key in ["jjosh-project", "jjosh-mount", "jjosh-base"] {
            updates.push((name.clone(), key.to_owned(), None));
        }
        if remote.filter.is_some() {
            for sidecar in planned_inputs.2.keys() {
                let Some(stem) = Path::new(sidecar)
                    .file_stem()
                    .and_then(|name| name.to_str())
                else {
                    continue;
                };
                if stem == remote.name || aliases.get(stem) == Some(&remote.name) {
                    sidecars.push(git.common_dir().join("josh/remotes").join(sidecar));
                }
            }
        }
    }
    if !explicit.is_empty() || !clear.is_empty() {
        return Err(user_error(format!(
            "Unknown or non-legacy remote mappings: representations={:?}, \
             clear-observations={clear:?}",
            explicit.keys()
        )));
    }
    // Explicit arguments remain valid after the legacy roots have been retired,
    // but only if a retained native binding proves this was an adopted source.
    let retained_native = crate::interop::open_josh_transaction(&git_path, true)?;
    for name in native_sources.keys() {
        if native_projects.contains(name) {
            continue;
        }
        let Ok((project, _)) = view.project_state.project_by_name(name) else {
            continue;
        };
        let mut has_native = false;
        for (id, binding) in &view.project_state.bindings {
            if binding
                .as_resolved()
                .and_then(Option::as_ref)
                .is_some_and(|binding| binding.target == BindingTarget::Project(project.clone()))
            {
                retained_native
                    .for_each_ref_prefixed(&native_project::binding_ref_prefix(id), |_, _| {
                        has_native = true;
                        Ok(())
                    })
                    .map_err(user_error)?;
            }
        }
        if has_native {
            projects.insert(name.clone(), project);
            native_projects.insert(name.clone());
        }
    }
    let mut native_bindings = BTreeMap::new();
    let mut offline_bindings = Vec::new();
    for name in &native_projects {
        let source = native_sources
            .remove(name)
            .unwrap_or_else(|| "detached".to_owned());
        let id = if source == "detached" {
            let candidates: Vec<_> = view
                .project_state
                .bindings
                .iter()
                .filter_map(|(id, value)| {
                    let binding = value.as_resolved().and_then(Option::as_ref)?;
                    (binding.target == BindingTarget::Project(projects[name].clone())
                        && binding.representation == Representation::Whole
                        && binding.base.is_none()
                        && !view.remote_connections.values().any(|owner| {
                            owner
                                .iter()
                                .flatten()
                                .any(|connection| connection == &binding.connection_id)
                        }))
                    .then(|| id.clone())
                })
                .collect();
            let id = match candidates.as_slice() {
                [id] => id.clone(),
                [] => {
                    let id = BindingId::generate();
                    view.project_state.bindings.insert(
                        id.clone(),
                        Merge::resolved(Some(BindingRecord {
                            target: BindingTarget::Project(projects[name].clone()),
                            connection_id: ConnectionId::generate(),
                            representation: Representation::Whole,
                            base: None,
                        })),
                    );
                    id
                }
                _ => {
                    return Err(user_error(format!(
                        "Project {name} has multiple disconnected whole bindings; choose an \
                         explicit native source before retrying"
                    )));
                }
            };
            offline_bindings.push(id.clone());
            id
        } else {
            let (id, binding) = if let Some((id, binding)) = remote_bindings.get(&source) {
                (id.clone(), binding)
            } else {
                git.find_remote(source.as_str()).map_err(user_error)?;
                let connection = jj_lib::git::remote_connection_id(
                    &git,
                    jj_lib::ref_name::RemoteName::new(&source),
                )
                .map_err(user_error)?
                .ok_or_else(|| {
                    user_error(format!("Native source {source} has no connection identity"))
                })?;
                view.project_state
                    .binding_for_connection(&connection)
                    .map_err(user_error)?
                    .ok_or_else(|| {
                        user_error(format!("Native source {source} has no active binding"))
                    })?
            };
            if binding.target != BindingTarget::Project(projects[name].clone())
                || binding.representation != Representation::Whole
            {
                return Err(user_error(format!(
                    "Native evidence for {name} requires a whole-project binding for that project"
                )));
            }
            id
        };
        writeln!(
            ui.status(),
            "Preserve native correspondence {name} -> {source} (no inferred endpoint or lease)"
        )?;
        native_bindings.insert(name.clone(), id);
    }
    if !native_sources.is_empty() {
        return Err(user_error(format!(
            "Unknown native projects in --native-source: {:?}",
            native_sources.keys()
        )));
    }
    for diagnostic in view.project_state.diagnostics() {
        if diagnostic
            .projects
            .iter()
            .any(|id| projects.values().any(|project| project == id))
            || diagnostic.bindings.iter().any(|id| {
                remote_bindings.values().any(|(binding, _)| binding == id)
                    || native_bindings.values().any(|binding| binding == id)
            })
            || diagnostic.projects.iter().any(|project| {
                alias_remotes.values().any(|connection| {
                    view.project_state
                        .remote_names
                        .get(connection)
                        .and_then(Merge::as_resolved)
                        .and_then(Option::as_ref)
                        .is_some_and(|alias| &alias.project == project)
                })
            })
        {
            blockers.push(diagnostic.to_string());
        }
    }
    // Check ownership during read-only planning, before any operation publication.
    let updated_remotes: BTreeSet<_> = updates
        .iter()
        .map(|(remote, _, _)| remote)
        .chain(alias_remotes.keys())
        .collect();
    for remote in updated_remotes {
        if config
            .sections_by_name("remote")
            .into_iter()
            .flatten()
            .filter(|section| {
                section
                    .header()
                    .subsection_name()
                    .is_some_and(|name| name == remote.as_str())
            })
            .any(|section| section.meta() != config.meta())
        {
            blockers.push(format!(
                "Remote {} has include/global configuration; move all its effective settings into \
                 local Git config before migration",
                remote.as_symbol()
            ));
        }
    }
    let legacy_transaction = crate::interop::open_josh_transaction(&git_path, true)?;
    let mut retire_refs = Vec::new();
    for name in projects.keys() {
        let prefix = native_project::project_ref_prefix(name);
        legacy_transaction
            .for_each_ref_prefixed(&prefix, |reference, _| {
                let suffix = reference.strip_prefix(&prefix).unwrap();
                let correspondence = suffix
                    .split_once('/')
                    .is_some_and(|(kind, _)| matches!(kind, "origin" | "published" | "graft"));
                if suffix == "mount" || (native_bindings.contains_key(name) && correspondence) {
                    retire_refs.push(reference.to_owned());
                }
                Ok(())
            })
            .map_err(user_error)?;
    }
    // Logical retirement belongs to the new operation. Physical mirrors remain
    // until replacement native evidence is available, then retire by expected value.
    for remote in cleared.iter().chain(legacy_remotes.iter()) {
        for prefix in [
            format!("refs/remotes/{}/", remote.as_str()),
            format!(
                "{}{}/",
                jj_lib::git::REMOTE_TAG_REF_NAMESPACE,
                remote.as_str()
            ),
        ] {
            legacy_transaction
                .for_each_ref_prefixed(&prefix, |name, _| {
                    retire_refs.push(name.to_owned());
                    Ok(())
                })
                .map_err(user_error)?;
            view.git_refs
                .retain(|name, _| !name.as_str().starts_with(&prefix));
        }
    }
    for reference in &legacy_mirrors {
        if legacy_transaction
            .resolve_ref(reference)
            .map_err(user_error)?
            .is_some()
        {
            retire_refs.push(reference.clone());
        }
        view.git_refs
            .remove(jj_lib::ref_name::GitRefName::new(reference));
    }
    retire_refs.sort();
    retire_refs.dedup();
    for reference in &retire_refs {
        writeln!(
            ui.status(),
            "Retire legacy authority {reference} after installing replacement evidence"
        )?;
    }
    for path in &sidecars {
        writeln!(
            ui.status(),
            "Retire legacy sidecar {} after operation commit",
            path.display()
        )?;
        if !std::fs::symlink_metadata(path)
            .map_err(user_error)?
            .file_type()
            .is_file()
        {
            blockers.push(format!(
                "Legacy sidecar {} is not a regular file; resolve it before migration",
                path.display()
            ));
        }
    }
    let mut rekeys = Vec::new();
    let mut settings_aliases = Vec::new();
    for (connection, alias) in &view.project_state.remote_names {
        if let Some(alias) = alias.as_resolved().and_then(Option::as_ref)
            && let Some((label, _)) = view.project_state.labels.iter().find(|(_, target)| {
                target.as_resolved().and_then(Option::as_ref) == Some(&alias.project)
            })
        {
            settings_aliases.push((
                alias.name.clone(),
                Some(format!("{}#{label}", alias.name.as_str())),
            ));
            // Retry local settings after an already-completed physical rekey.
            let physical: RemoteNameBuf = format!("jjosh-{}", connection.hex()).into();
            if physical != alias.name {
                settings_aliases.push((physical, Some(format!("{}#{label}", alias.name.as_str()))));
            }
        }
    }
    let has_config = |remote: &str| {
        config
            .sections_by_name("remote")
            .into_iter()
            .flatten()
            .any(|section| {
                section
                    .header()
                    .subsection_name()
                    .is_some_and(|name| name == remote)
            })
    };
    for (old, connection) in &alias_remotes {
        let new: RemoteNameBuf = format!("jjosh-{}", connection.hex()).into();
        let qualified_name = jj_lib::view::remote_identity::resolve(&view, old)
            .ok()
            .flatten()
            .map_or_else(
                || old.as_str().to_owned(),
                |identity| identity.qualified_name(&view.project_state, old),
            );
        settings_aliases.push((old.clone(), Some(qualified_name.clone())));
        if old == &new {
            continue;
        }
        if has_config(new.as_str())
            || view.remote_connections.contains_key(&new)
            || jj_lib::view::remote_observations::RemoteObservations::new(&mut view).contains(&new)
        {
            blockers.push(format!(
                "Opaque remote handle {} is already occupied",
                new.as_str()
            ));
            continue;
        }
        for prefix in [
            format!("refs/remotes/{}/", new.as_str()),
            format!("{}{}/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE, new.as_str()),
        ] {
            if view
                .git_refs
                .keys()
                .any(|name| name.as_str().starts_with(&prefix))
                || git
                    .references()
                    .map_err(user_error)?
                    .prefixed(prefix.as_str())
                    .map_err(user_error)?
                    .next()
                    .is_some()
            {
                blockers.push(format!(
                    "Opaque remote handle {} has existing physical refs",
                    new.as_str()
                ));
            }
        }
        let exists = has_config(old.as_str());
        if exists {
            let remote = git
                .try_find_remote(old.as_str())
                .expect("configured remote has a section")
                .map_err(user_error)?;
            match (
                remote.refspecs(Direction::Fetch),
                remote.refspecs(Direction::Push),
            ) {
                ([fetch], [])
                    if fetch.to_ref().to_bstring().as_slice()
                        == format!("+refs/heads/*:refs/remotes/{}/*", old.as_str()).as_bytes() => {}
                ([], [])
                    if remote.url(Direction::Fetch).is_none()
                        && remote.url(Direction::Push).is_none() => {}
                _ => blockers.push(format!(
                    "Remote {} has nonstandard refspecs; normalize them before explicit scoped \
                     migration",
                    old.as_str()
                )),
            }
            for section in config
                .sections_by_name("remote")
                .into_iter()
                .flatten()
                .filter(|section| {
                    section
                        .header()
                        .subsection_name()
                        .is_some_and(|name| name == old.as_str())
                })
            {
                if section.value_names().any(|key| {
                    !["url", "pushurl", "fetch", "tagOpt"]
                        .iter()
                        .chain(jj_lib::git::MANAGED_REMOTE_KEYS)
                        .any(|known| key.eq_ignore_ascii_case(known))
                        && !updates.iter().any(|(remote, retired_key, value)| {
                            remote == old
                                && value.is_none()
                                && key.eq_ignore_ascii_case(retired_key)
                        })
                }) {
                    blockers.push(format!(
                        "Remote {} has unsupported custom Git settings; move them explicitly \
                         before scoped migration",
                        old.as_str()
                    ));
                }
            }
        }
        writeln!(
            ui.status(),
            "Rekey {} -> {} for scoped identity {}; preserve IDs, refs, observations and tracking",
            old.as_str(),
            new.as_str(),
            qualified_name
        )?;
        rekeys.push((old.clone(), new, connection.clone(), exists));
    }
    // Explicitly cleared mirrors must not be carried to the new physical key.
    // Delete each selected mirror only with its captured expected value.
    let mut clear_edits = Vec::new();
    for (old, _, _, exists) in &rekeys {
        if !*exists || !cleared.iter().any(|remote| remote == old) {
            continue;
        }
        for prefix in [
            format!("refs/remotes/{}/", old.as_str()),
            format!("{}{}/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE, old.as_str()),
        ] {
            for reference in git
                .references()
                .map_err(user_error)?
                .prefixed(prefix.as_str())
                .map_err(user_error)?
            {
                let reference = reference.map_err(user_error)?;
                clear_edits.push(gix::refs::transaction::RefEdit {
                    change: gix::refs::transaction::Change::Delete {
                        expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(
                            reference.target().into_owned(),
                        ),
                        log: gix::refs::transaction::RefLog::AndReference,
                    },
                    name: reference.name().to_owned(),
                    deref: false,
                });
            }
        }
    }
    retire_refs.retain(|reference| {
        !clear_edits
            .iter()
            .any(|edit| edit.name.as_bstr() == reference.as_bytes())
    });
    settings_aliases.sort();
    settings_aliases.dedup();
    let mut pending_settings = Vec::new();
    for mapping in settings_aliases {
        let mut present = false;
        for layer in command.raw_config().as_ref().layers() {
            present |= layer
                .look_up_item(["remotes", mapping.0.as_str()])
                .map_err(|_| user_error("Remote settings parent must be a table"))?
                .is_some();
        }
        if present {
            pending_settings.push(mapping);
        }
    }
    let settings_aliases = pending_settings;
    let repo_config =
        jj_cli::git_remote::prepare_remote_settings_scope(command.raw_config(), &settings_aliases)?;
    if repo_config.is_some() {
        for (old, qualified) in &settings_aliases {
            writeln!(
                ui.status(),
                "Scope repo-local remotes.{} settings as remotes.{}",
                old.as_str(),
                qualified
                    .as_deref()
                    .expect("scope mapping has a destination")
            )?;
        }
    }
    for (old, new, _, _) in &rekeys {
        rekey_view(&mut view, old, new).map_err(user_error)?;
    }
    if !projects.is_empty() {
        let commit = workspace
            .resolve_single_rev(ui, &jj_cli::cli_util::RevisionArg::AT)
            .await?;
        for name in projects.keys() {
            if let Ok((_, record)) = view.project_state.project_by_name(name)
                && let Err(error) =
                    native_project::project_tree(&commit, &record.canonical_root).await
            {
                blockers.push(format!("Project {name}: {error:#}"));
            }
        }
    }
    for blocker in &blockers {
        writeln!(ui.status(), "Blocked: {blocker}")?;
    }
    if !blockers.is_empty() {
        return Err(user_error(
            "Migration is blocked; resolve the listed issues and rerun project migrate --dry-run",
        ));
    }
    if args.dry_run || !args.apply {
        writeln!(
            ui.status(),
            "Dry run: no changes written. Use --apply with the same explicit mappings to commit \
             this plan."
        )?;
        return Ok(());
    }
    if store_upgrade && !args.exclusive {
        return Err(user_error(
            "Stop incompatible writers, then rerun project migrate --apply --exclusive",
        ));
    }
    jj_cli::git_remote::check_repo_config_unchanged(command.raw_config())?;
    let mut current_git = jj_lib::git::get_git_repo(workspace.repo().store())?;
    current_git.reload().map_err(user_error)?;
    if migration_inputs(&current_git).map_err(user_error)? != planned_inputs {
        return Err(user_error(
            "Migration's Git inputs changed while planning; retry",
        ));
    }
    if store_upgrade {
        workspace
            .repo()
            .op_store()
            .downcast_ref::<jj_lib::simple_op_store::SimpleOpStore>()
            .expect("store requiring upgrade is SimpleOpStore")
            .upgrade_project_store_type()?;
    }
    let staging = (|| -> Result<(), CommandError> {
        let transaction = crate::interop::open_josh_transaction(&git_path, false)?;
        for binding in &offline_bindings {
            native_project::record_offline_binding(&transaction, binding).map_err(user_error)?;
        }
        for (project, binding) in &native_bindings {
            native_project::migrate_binding_anchors(&transaction, project, binding)
                .map_err(user_error)?;
        }
        for (project, kind, raw, canonical) in &legacy_pairs {
            native_project::record_anchor(
                &transaction,
                &native_bindings[project],
                kind,
                raw,
                canonical,
            )
            .map_err(user_error)?;
        }
        transaction.flush_mem_odb().map_err(user_error)?;
        let mut markers: Vec<_> = updates
            .iter()
            .filter(|(_, _, value)| value.is_some())
            .cloned()
            .collect();
        for (remote, connection) in &alias_remotes {
            if git.try_find_remote(remote.as_str()).is_none() {
                continue;
            }
            for (key, value) in [
                ("jjosh-connectionId", connection.hex()),
                ("jjosh-requiredCapability", "jjosh-v1".to_owned()),
            ] {
                if !markers
                    .iter()
                    .any(|(name, existing, _)| name == remote && existing == key)
                {
                    markers.push((remote.clone(), key.to_owned(), Some(value)));
                }
            }
        }
        if !markers.is_empty() {
            jj_lib::git::set_remote_config_keys(workspace.repo().store(), &markers, &config)?;
        }
        Ok(())
    })();
    staging.map_err(|error| {
        jj_cli::command_error::user_error_with_message(
            "Migration staging stopped before publishing metadata. Legacy source material and any \
             completed additive native evidence or inactive connection markers were retained; \
             resolve the reported resource conflict and retry with the same mappings.",
            error.error,
        )
    })?;
    // Required evidence and inactive transport markers are already available.
    // Publish final logical identities before physical rekey/settings/retirement;
    // failures after this boundary never restore an older operation.
    let mut tx = workspace.start_transaction();
    tx.repo_mut().set_view(view.clone());
    let published = if tx.repo().view().store_view() == tx.base_repo().view().store_view() {
        tx.base_repo().clone()
    } else {
        command
            .maybe_commit_transaction(
                tx.into_inner(),
                "migrate projects, immutable source bindings and scoped remote names",
            )
            .await?
    };
    let cutover = (|| -> Result<(), CommandError> {
        let mut tx = published.start_transaction();
        if !clear_edits.is_empty() {
            let prepared = git
                .refs
                .transaction()
                .prepare(
                    clear_edits.clone(),
                    gix::lock::acquire::Fail::Immediately,
                    gix::lock::acquire::Fail::Immediately,
                )
                .map_err(user_error)?;
            prepared
                .commit(git.committer().transpose().map_err(user_error)?)
                .map_err(user_error)?;
        }
        // The published operation already uses the final namespace. A scratch
        // transaction stages old keys solely for the native lifecycle; it is never
        // published as another operation.
        for (old, new, _, exists) in &rekeys {
            if *exists {
                rekey_view(&mut view, new, old).map_err(user_error)?;
            }
        }
        tx.repo_mut().set_view(view);
        for (old, new, connection, exists) in &rekeys {
            if *exists {
                jj_lib::git::rename_remote_with_options(
                    tx.repo_mut(),
                    old,
                    new,
                    &jj_lib::git::GitRemoteManagementOptions {
                        extra_config_keys: &[
                            "jjosh-project",
                            "jjosh-mount",
                            "jjosh-base",
                            "jjosh-readOnly",
                        ],
                        expected_connection: Some(Some(connection.clone())),
                    },
                )?;
            }
        }
        current_git.reload().map_err(user_error)?;
        finish_rekeyed_mirrors(&current_git, published.view().store_view())?;
        if let Some(config) = &repo_config {
            jj_cli::git_remote::commit_repo_config_update(command.raw_config(), config)?;
        }
        // All new evidence and physical handles are available. Retire only the
        // values read during planning; a changed value remains for explicit review.
        let mut retirement_edits = Vec::new();
        for name in &retire_refs {
            let expected = planned_inputs
                .1
                .get(&gix::refs::FullName::try_from(name.as_str()).map_err(user_error)?)
                .ok_or_else(|| user_error(format!("Legacy reference {name} was not captured")))?;
            if let Some(reference) = current_git.try_find_reference(name).map_err(user_error)? {
                if reference.target().into_owned() != *expected {
                    return Err(user_error(format!(
                        "Legacy reference {name} changed; retained"
                    )));
                }
                retirement_edits.push(gix::refs::transaction::RefEdit {
                    name: reference.name().to_owned(),
                    change: gix::refs::transaction::Change::Delete {
                        expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(
                            expected.clone(),
                        ),
                        log: gix::refs::transaction::RefLog::AndReference,
                    },
                    deref: false,
                });
            }
        }
        let retirement_config = current_git.config_snapshot();
        let mut retire_config = Vec::new();
        for (old, key, value) in updates.into_iter().filter(|(_, _, value)| value.is_none()) {
            let remote = rekeys
                .iter()
                .find(|(source, _, _, exists)| *exists && source == &old)
                .map_or(old.clone(), |(_, new, _, _)| new.clone());
            let old_key = format!("remote.{}.{key}", old.as_str());
            let new_key = format!("remote.{}.{key}", remote.as_str());
            if config.raw_values(old_key.as_str()).unwrap_or_default()
                != retirement_config
                    .raw_values(new_key.as_str())
                    .unwrap_or_default()
            {
                return Err(user_error(format!(
                    "Legacy setting {new_key} changed during migration; retained"
                )));
            }
            let expected_owner = published
                .view()
                .store_view()
                .remote_connections
                .get(&remote)
                .and_then(Merge::as_resolved)
                .and_then(Option::as_ref)
                .cloned()
                .or(jj_lib::git::remote_connection_id(&git, &old).map_err(user_error)?);
            if jj_lib::git::remote_connection_id(&current_git, &remote).map_err(user_error)?
                != expected_owner
            {
                return Err(user_error(format!(
                    "Remote {} changed ownership during migration; configuration retained",
                    remote.as_str()
                )));
            }
            retire_config.push((remote, key, value));
        }
        if !retire_config.is_empty() {
            jj_lib::git::set_remote_config_keys(
                published.store(),
                &retire_config,
                &retirement_config,
            )?;
        }
        for path in &sidecars {
            let expected = planned_inputs
                .2
                .get(path.file_name().expect("sidecar has filename"))
                .ok_or_else(|| user_error("Legacy sidecar was not captured"))?;
            retire_sidecar(path, expected)?;
        }
        current_git
            .edit_references(retirement_edits)
            .map_err(user_error)?;
        Ok(())
    })();
    cutover.map_err(|error| {
        jj_cli::command_error::user_error_with_message(
            "Project migration metadata is published, but local cutover/retirement is incomplete. \
             Existing source material and completed native changes were retained; rerun project \
             migrate --apply with the same mappings to finish after resolving the reported \
             resource conflict.",
            error.error,
        )
    })?;
    writeln!(
        ui.status(),
        "Migrated {} projects and {} remote bindings; adopted {} existing scoped remote names; \
         retired obsolete configuration for {} remotes; local references, working files, raw \
         objects, shallow state and publication leases preserved.",
        projects.len(),
        remote_bindings.len(),
        alias_remotes.len(),
        retired_config_remotes.len()
    )?;
    Ok(())
}
