use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::io::Write as _;

use anyhow::{Context as _, Result, ensure};
use gix::remote::Direction;
use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::ui::Ui;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{BindingId, BindingRecord, BindingTarget, ConnectionId, ProjectId, ProjectRecord, Representation};
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
    pub(crate) push_url: Option<String>,
    pub(crate) read_only: Option<bool>,
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
        let (name, value) = value.split_once('=').context("Mapping must be NAME=VALUE")?;
        ensure!(!name.is_empty() && !value.is_empty(), "Mapping name and value cannot be empty");
        ensure!(result.insert(name.to_owned(), value.to_owned()).is_none(), "Duplicate mapping for {name}");
    }
    Ok(result)
}

/// Explicit migration is the only reader of the previous configuration model.
/// Preparation is read-only; all physical mutations are covered by the core
/// journal before the operation commit makes the new semantic state authoritative.
pub(crate) async fn run(ui: &Ui, command: &CommandHelper, args: &Args) -> Result<(), CommandError> {
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error("Project migration requires the current integrated operation"));
    }
    let mut workspace = crate::project::recorded_workspace(ui, command).await?;
    let git_path = crate::interop::sha1_git_repo_path(&workspace)?;
    let git = jj_lib::git::get_git_backend(workspace.repo().store())?.git_repo();
    jj_lib::git::ensure_no_pending_remote_management(&git)?;
    let inventory = inspect(&git_path).map_err(user_error)?;
    let mut explicit = mappings(&args.representation).map_err(user_error)?;
    let mut native_sources = mappings(&args.native_source).map_err(user_error)?;
    let mut clear: BTreeSet<_> = args.clear_observations.iter().cloned().collect();
    let mut view = workspace.repo().view().store_view().clone();
    let store_upgrade = workspace.repo().op_store().downcast_ref::<jj_lib::simple_op_store::SimpleOpStore>()
        .map(|store| store.requires_project_store_upgrade()).transpose()?.unwrap_or(false);
    if store_upgrade {
        writeln!(ui.status(), "Storage requires an exclusive project-aware format upgrade; stop incompatible writers before --apply --exclusive")?;
    }
    let mut blockers = Vec::new();
    // These old policy diagnostics are superseded by explicit per-binding
    // definitions below. Structural corruption is never overridden by a mapping.
    for issue in &inventory.issues {
        if issue.severity == Severity::Error
            && issue.message != "Project has conflicting source filters"
            && !(issue.message == "Native project has a source-changing Josh filter" && explicit.contains_key(&issue.subject))
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
            Ok(_) => return Err(user_error(format!("Project {} already has a different canonical root", project.name))),
            Err(_) if view.project_state.projects.values().any(|value| value.adds().flatten().any(|record| record.name == project.name)) => {
                return Err(user_error(format!("Resolve existing project {} before migration", project.name)));
            }
            Err(_) => {
                let id = ProjectId::generate();
                view.project_state.projects.insert(id.clone(), Merge::resolved(Some(ProjectRecord {
                    name: project.name.clone(), canonical_root: mount.clone(),
                })));
                id
            }
        };
        writeln!(ui.status(), "Project {} -> {} (materialized canonical root)", project.name, mount.as_internal_file_string())?;
        projects.insert(project.name.clone(), id);
    }
    // Older native imports used remote-view records as explicit raw/canonical
    // pairs. Their suffix names are evidence, unlike ordinary remote observations.
    let mut legacy_pairs = Vec::new();
    let mut legacy_mirrors = Vec::new();
    // These reserved refs may not have been imported into a View yet. Read their
    // explicit correspondence records directly rather than routing them through
    // ordinary Git import, which correctly rejects unproven project mirrors.
    for reference in git.references().map_err(user_error)?
        .prefixed("refs/remotes/jjosh-native-").map_err(user_error)?
    {
        let reference = reference.map_err(user_error)?;
        let full_name = std::str::from_utf8(reference.name().as_bstr()).map_err(user_error)?;
        let (project, name) = full_name.strip_prefix("refs/remotes/jjosh-native-").unwrap()
            .split_once('/').ok_or_else(|| user_error("Invalid legacy native correspondence ref"))?;
        native_project::validate_project(project).map_err(user_error)?;
        let remote: RemoteNameBuf = format!("jjosh-native-{project}").into();
        let raw_target = reference.target();
        let id = raw_target.try_id().ok_or_else(|| user_error("Legacy native correspondence refs must be direct"))?;
        let target = jj_lib::op_store::RefTarget::normal(jj_lib::backend::CommitId::from_bytes(id.as_bytes()));
        let refs = view.remote_views.entry(remote).or_default();
        if let Some(existing) = refs.bookmarks.get(jj_lib::ref_name::RefName::new(name)) {
            if existing.target != target {
                return Err(user_error(format!("Legacy native ref {full_name} disagrees with its recorded observation")));
            }
        } else {
            refs.bookmarks.insert(name.into(), jj_lib::op_store::RemoteRef {
                target, state: jj_lib::op_store::RemoteRefState::New,
            });
        }
    }
    let legacy_remotes: Vec<_> = view.remote_views.keys().filter(|name| name.as_str().starts_with("jjosh-native-")).cloned().collect();
    for remote in &legacy_remotes {
        let project = remote.as_str().strip_prefix("jjosh-native-").unwrap();
        native_project::validate_project(project).map_err(user_error)?;
        if git.find_remote(remote.as_str()).is_ok() {
            blockers.push(format!("{} is a configured remote; rename it before migrating legacy native evidence", remote.as_str()));
            continue;
        }
        if view.remote_connections.get(remote).is_some_and(|owner| owner.iter().flatten().next().is_some())
            || view.project_observations.keys().any(|key| key.remote == *remote)
        {
            blockers.push(format!("{} has new-format connection ownership; do not reinterpret it as a legacy correspondence namespace", remote.as_str()));
            continue;
        }
        if !projects.contains_key(project) {
            let id = if let Ok((id, _)) = view.project_state.project_by_name(project) {
                id
            } else {
                let id = ProjectId::generate();
                let mount = native_project::default_mount(project).map_err(user_error)?;
                view.project_state.projects.insert(id.clone(), Merge::resolved(Some(ProjectRecord { name: project.to_owned(), canonical_root: mount })));
                writeln!(ui.status(), "Project {project} -> {project} (legacy native default)")?;
                id
            };
            projects.insert(project.to_owned(), id);
        }
        native_projects.insert(project.to_owned());
        let refs = &view.remote_views[remote];
        if !refs.tags.is_empty() {
            blockers.push(format!("{} has tags; resolve these non-correspondence records first", remote.as_str()));
        }
        for (name, reference) in &refs.bookmarks {
            let (kind, raw) = name.as_str().split_once('/').ok_or_else(|| user_error("Invalid legacy native correspondence name"))?;
            if !matches!(kind, "origin" | "published") || reference.state == jj_lib::op_store::RemoteRefState::Tracked {
                return Err(user_error(format!("Invalid or tracked native correspondence {}@{}; untrack it first", name.as_str(), remote.as_str())));
            }
            let raw = jj_lib::backend::CommitId::try_from_hex(raw).filter(|id| id.as_bytes().len() == 20)
                .ok_or_else(|| user_error("Invalid legacy native source commit ID"))?;
            let canonical = reference.target.as_normal().ok_or_else(|| user_error("Resolve conflicted native correspondences before migrating"))?;
            workspace.repo().store().get_commit(&raw)?;
            workspace.repo().store().get_commit(canonical)?;
            writeln!(ui.status(), "Adopt native {project}/{kind}: {} -> {}", raw.hex(), canonical.hex())?;
            legacy_pairs.push((project.to_owned(), kind.to_owned(), raw, canonical.clone()));
        }
        view.remote_views.remove(remote);
        let mirror: RemoteNameBuf = format!("{project}-git").into();
        // The retired native importer made this unconfigured synthetic mirror.
        // Only exact local-target duplicates are its disposable bookkeeping.
        if git.find_remote(mirror.as_str()).is_err()
            && !view.remote_connections.get(&mirror).is_some_and(|owner| owner.iter().flatten().next().is_some())
            && !view.project_observations.keys().any(|key| key.remote == mirror)
        {
            for (prefix, local) in [
                (format!("refs/remotes/{}/", mirror.as_str()), &view.local_bookmarks),
                (format!("{}{}/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE, mirror.as_str()), &view.local_tags),
            ] {
                for reference in git.references().map_err(user_error)?.prefixed(prefix.as_str()).map_err(user_error)? {
                    let reference = reference.map_err(user_error)?;
                    let full_name = std::str::from_utf8(reference.name().as_bstr()).map_err(user_error)?;
                    let name = jj_lib::ref_name::RefName::new(full_name.strip_prefix(&prefix).unwrap());
                    if let Some(id) = reference.target().try_id()
                        && local.get(name).and_then(jj_lib::op_store::RefTarget::as_normal)
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
            Some(existing) if existing != *id => return Err(user_error(format!("Label {name} belongs to another project"))),
            Some(_) => {}
            None => {
                let suffix = format!("#{name}");
                for reference in view.local_bookmarks.keys().chain(view.local_tags.keys()).filter(|reference| reference.as_str().ends_with(&suffix)) {
                    writeln!(ui.status(), "Adopt local reference {} as project label {name}", reference.as_str())?;
                }
                for (remote, refs) in &view.remote_views {
                    for (reference, target) in refs.bookmarks.iter().chain(refs.tags.iter()).filter(|(reference, _)| reference.as_str().ends_with(&suffix)) {
                        writeln!(ui.status(), "Adopt remote reference {}@{} ({:?}) as project label {name}", reference.as_str(), remote.as_str(), target.state)?;
                    }
                }
                view.project_state.labels.insert(name.clone(), Merge::resolved(Some(id.clone())));
            }
        }
    }
    for reference in &legacy_mirrors {
        writeln!(ui.status(), "Retire legacy native mirror {reference}; identical local target retained")?;
    }
    let mut updates = Vec::new();
    let mut sidecars = Vec::new();
    let mut remote_bindings = BTreeMap::new();
    let mut cleared = Vec::new();
    let mut legacy_representations = BTreeMap::new();
    let mut filtered_projects = BTreeSet::new();
    for remote in &inventory.remotes {
        let representation = if let Some(filter) = &remote.filter {
            // Older native peers explicitly accepted the identity sidecar as
            // whole-project, as well as the already-prefixed identity filter.
            if remote.project.as_ref().is_some_and(|project| native_projects.contains(project))
                && josh_core::filter::parse(filter).map_err(user_error)? == josh_core::filter::Filter::new()
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
        if representation.as_ref().is_ok_and(|value| value != &Representation::Whole)
            && let Some(project) = &remote.project {
                filtered_projects.insert(project.clone());
            }
        legacy_representations.insert(remote.name.clone(), representation);
    }
    for remote in &inventory.remotes {
        if remote.project.is_none() && remote.filter.is_none() && !explicit.contains_key(&remote.name) {
            continue;
        }
        let name: RemoteNameBuf = remote.name.clone().into();
        if git.find_remote(name.as_str()).is_err() {
            blockers.push(format!("Orphan sidecar {}: explicitly remove or configure its remote before migration", remote.name));
            continue;
        }
        let specified = explicit.remove(&remote.name);
        let representation = if let Some(value) = &specified {
            crate::binding_config::parse_mapping(value).map_err(user_error)?
        } else {
            match legacy_representations.remove(&remote.name).expect("inventory representation was prepared") {
                Ok(value) => value,
                Err(error) => { blockers.push(format!("{}: {error:#}", remote.name)); continue; }
            }
        };
        if specified.is_none() && remote.filter.is_none()
            && remote.project.as_ref().is_some_and(|project| filtered_projects.contains(project))
        {
            blockers.push(format!("Remote {} fetched whole-project but could publish using a filtered peer; choose --representation {}=whole or {}=filter:SOURCE explicitly", remote.name, remote.name, remote.name));
        }
        let target = remote.project.as_ref().map(|project| BindingTarget::Project(projects[project].clone())).unwrap_or(BindingTarget::RepositoryView);
        let connection = jj_lib::git::remote_connection_id(&git, &name).map_err(user_error)?
            .unwrap_or_else(ConnectionId::generate);
        if view.project_state.binding_for_connection(&connection).map_err(user_error)?.is_some() {
            return Err(user_error(format!("Remote {} is already bound; do not reinterpret it through legacy migration", remote.name)));
        }
        let base =
            crate::git_remote::config_string(&git, &format!("remote.{}.jjosh-base", remote.name))
                .map_err(user_error)?
                .as_deref()
                .map(crate::binding_config::parse_base)
                .transpose()
                .map_err(user_error)?;
        let binding = BindingRecord {
            target,
            connection_id: connection.clone(),
            representation,
            base,
        };
        let id = BindingId::generate();
        writeln!(
            ui.status(),
            "Bind {}: {:?}; base={:?}; read-only={:?}; endpoints/refspec/auth unchanged",
            remote.name,
            binding.representation,
            binding.base,
            remote.read_only
        )?;
        if let Some(refs) = view.remote_views.get(&name)
            && (!refs.bookmarks.is_empty() || !refs.tags.is_empty())
        {
            for (reference, target) in refs.bookmarks.iter().chain(refs.tags.iter()) {
                writeln!(
                    ui.status(),
                    "Legacy observation {}@{} ({:?}) has no immutable source evidence",
                    reference.as_str(),
                    name.as_str(),
                    target.state
                )?;
            }
            if !clear.contains(&remote.name) {
                blockers.push(format!(
                    "Remote {remote_name}: legacy observations cannot prove \
                     endpoint/raw/generation. Pass --clear-observations {remote_name} to \
                     explicitly forget its remote observations/tracking, retain local refs and \
                     leases, then fetch again",
                    remote_name = name.as_str()
                ));
            }
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
        view.remote_connections.insert(name.clone(), Merge::resolved(Some(connection.clone())));
        view.project_state.bindings.insert(id.clone(), Merge::resolved(Some(binding.clone())));
        remote_bindings.insert(remote.name.clone(), (id, binding));
        updates.push((name.clone(), "jjosh-connectionId".to_owned(), Some(connection.hex())));
        updates.push((name.clone(), "jjosh-requiredCapability".to_owned(), Some("jjosh-v1".to_owned())));
        for key in ["jjosh-project", "jjosh-mount", "jjosh-base"] {
            updates.push((name.clone(), key.to_owned(), None));
        }
        if remote.filter.is_some() {
            sidecars.push(git.common_dir().join("josh/remotes").join(format!("{}.josh", remote.name)));
        }
    }
    if !explicit.is_empty() || !clear.is_empty() {
        return Err(user_error(format!("Unknown or non-legacy remote mappings: representations={:?}, clear-observations={clear:?}", explicit.keys())));
    }
    let mut native_bindings = BTreeMap::new();
    let mut offline_bindings = Vec::new();
    for name in &native_projects {
        let source = native_sources.remove(name).unwrap_or_else(|| "detached".to_owned());
        let id = if source == "detached" {
            let id = BindingId::generate();
            let binding = BindingRecord { target: BindingTarget::Project(projects[name].clone()), connection_id: ConnectionId::generate(), representation: Representation::Whole, base: None };
            view.project_state.bindings.insert(id.clone(), Merge::resolved(Some(binding)));
            offline_bindings.push(id.clone());
            id
        } else {
            let (id, binding) = if let Some((id, binding)) = remote_bindings.get(&source) {
                (id.clone(), binding)
            } else {
                git.find_remote(source.as_str()).map_err(user_error)?;
                let connection = jj_lib::git::remote_connection_id(&git, jj_lib::ref_name::RemoteName::new(&source))
                    .map_err(user_error)?.ok_or_else(|| user_error(format!("Native source {source} has no connection identity")))?;
                view.project_state.binding_for_connection(&connection).map_err(user_error)?
                    .ok_or_else(|| user_error(format!("Native source {source} has no active binding")))?
            };
            if binding.target != BindingTarget::Project(projects[name].clone()) || binding.representation != Representation::Whole {
                return Err(user_error(format!("Native evidence for {name} requires a whole-project binding for that project")));
            }
            id
        };
        writeln!(ui.status(), "Preserve native correspondence {name} -> {source} (no inferred endpoint or lease)")?;
        native_bindings.insert(name.clone(), id);
    }
    if !native_sources.is_empty() {
        return Err(user_error(format!("Unknown native projects in --native-source: {:?}", native_sources.keys())));
    }
    for diagnostic in view.project_state.diagnostics() {
        if diagnostic.projects.iter().any(|id| projects.values().any(|project| project == id))
            || diagnostic.bindings.iter().any(|id| remote_bindings.values().any(|(binding, _)| binding == id) || native_bindings.values().any(|binding| binding == id))
        {
            blockers.push(diagnostic.to_string());
        }
    }
    // Match the core journal's ownership rule during read-only planning, before
    // any provenance copying or operation publication.
    let config = git.config_snapshot();
    for remote in remote_bindings.keys() {
        if config.sections_by_name("remote").into_iter().flatten()
            .filter(|section| section.header().subsection_name().is_some_and(|name| name == remote.as_str()))
            .any(|section| section.meta() != config.meta())
        {
            blockers.push(format!("Remote {remote} has include/global configuration; move all its effective settings into local Git config before migration"));
        }
    }
    let legacy_transaction = crate::interop::open_josh_transaction(&git_path, true)?;
    let mut retire_refs = Vec::new();
    for name in projects.keys() {
        let prefix = native_project::project_ref_prefix(name);
        legacy_transaction.for_each_ref_prefixed(&prefix, |reference, _| {
            let suffix = reference.strip_prefix(&prefix).unwrap();
            let correspondence = suffix.split_once('/').is_some_and(|(kind, _)| matches!(kind, "origin" | "published" | "graft"));
            if suffix == "mount" || (native_bindings.contains_key(name) && correspondence) {
                retire_refs.push(reference.to_owned());
            }
            Ok(())
        }).map_err(user_error)?;
    }
    // Logical retirement belongs to the new operation. Physical mirrors remain
    // intact until its semantic witness commits, then retire through the same
    // CAS-checked journal as the obsolete private authority.
    for remote in cleared.iter().chain(legacy_remotes.iter()) {
        for prefix in [format!("refs/remotes/{}/", remote.as_str()), format!("{}{}/", jj_lib::git::REMOTE_TAG_REF_NAMESPACE, remote.as_str())] {
            legacy_transaction.for_each_ref_prefixed(&prefix, |name, _| {
                retire_refs.push(name.to_owned());
                Ok(())
            }).map_err(user_error)?;
            view.git_refs.retain(|name, _| !name.as_str().starts_with(&prefix));
        }
    }
    for reference in &legacy_mirrors {
        if legacy_transaction.resolve_ref(reference).map_err(user_error)?.is_some() {
            retire_refs.push(reference.clone());
        }
        view.git_refs.remove(jj_lib::ref_name::GitRefName::new(reference));
    }
    retire_refs.sort();
    retire_refs.dedup();
    for reference in &retire_refs {
        writeln!(ui.status(), "Retire legacy authority {reference} after operation commit")?;
    }
    for path in &sidecars {
        writeln!(ui.status(), "Retire legacy sidecar {} after operation commit", path.display())?;
        if !std::fs::symlink_metadata(path).map_err(user_error)?.file_type().is_file() {
            blockers.push(format!("Legacy sidecar {} is not a regular file; resolve it before migration", path.display()));
        }
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
        return Err(user_error("Migration is blocked; resolve the listed issues and rerun project migrate --dry-run"));
    }
    if args.dry_run || !args.apply {
        writeln!(ui.status(), "Dry run: no changes written. Use --apply with the same explicit mappings to commit this plan.")?;
        return Ok(());
    }
    if store_upgrade && !args.exclusive {
        return Err(user_error("Stop incompatible writers, then rerun project migrate --apply --exclusive"));
    }
    let _git_lock = workspace.lock_git_import_export()?;
    let journal = jj_lib::git::begin_remote_management(workspace.repo().store(), &workspace.repo().operation().id().hex(), &sidecars)?;
    journal.register_retirements(&sidecars, &retire_refs)?;
    for (remote, (_, binding)) in &remote_bindings {
        journal.expect_remote(jj_lib::ref_name::RemoteName::new(remote), true, Some(&binding.connection_id), true)?;
    }
    if store_upgrade {
        workspace.repo().op_store().downcast_ref::<jj_lib::simple_op_store::SimpleOpStore>()
            .expect("store requiring upgrade is SimpleOpStore").upgrade_project_store_type()?;
    }
    let transaction = crate::interop::open_josh_transaction(&git_path, false)?;
    for binding in &offline_bindings {
        native_project::record_offline_binding(&transaction, binding).map_err(user_error)?;
    }
    for (project, binding) in &native_bindings {
        native_project::migrate_binding_anchors(&transaction, project, binding).map_err(user_error)?;
    }
    for (project, kind, raw, canonical) in &legacy_pairs {
        native_project::record_anchor(&transaction, &native_bindings[project], kind, raw, canonical).map_err(user_error)?;
    }
    transaction.flush_mem_odb().map_err(user_error)?;
    jj_lib::git::set_remote_config_keys(workspace.repo().store(), &updates)?;
    let mut tx = workspace.start_transaction();
    tx.repo_mut().set_view(view);
    journal.expect_operation(tx.repo().view())?;
    tx.into_inner().commit("migrate legacy projects and immutable source bindings").await?;
    // Completion and `git remote recover --accept` perform the same idempotent
    // post-commit retirement. The journal remains until every retirement succeeds.
    journal.complete()?;
    writeln!(ui.status(), "Migrated {} projects and {} remote bindings; local references, working files, raw objects, shallow state and publication leases preserved.", projects.len(), remote_bindings.len())?;
    Ok(())
}
