use std::io::Write as _;

use jj_cli::cli_util::{CommandHelper, RevisionArg, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error};
use jj_cli::ui::Ui;
use jj_lib::merge::Merge;
use jj_lib::object_id::ObjectId as _;
use jj_lib::project::{BindingId, ProjectId};
use jj_lib::repo::Repo as _;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum Command {
    /// Register a canonical directory offline, without changing files or commits.
    Add(AddArgs),
    /// List operation-owned projects, stable labels, bindings and diagnostics.
    List(OutputArgs),
    /// Show one project, including unresolved definition candidates.
    Show(ShowArgs),
    /// Diagnose metadata offline, optionally limited to one project.
    Check(CheckArgs),
    /// Change a display name without changing its ID, reference label or path.
    Rename(RenameArgs),
    /// Unregister an unused project without deleting content or history.
    Remove(NameArgs),
    /// Explicitly choose or delete unresolved metadata, or retire a disconnected binding.
    Resolve(ResolveArgs),
    /// Import recorded native sources as new outer projects in one operation.
    /// Nested project metadata is not activated; source URLs and permissions are never imported.
    Import(crate::native::ImportArgs),
    /// Inspect or explicitly apply migration of legacy project configuration.
    Migrate(crate::project_migration::Args),
}

#[derive(clap::Args, Clone, Debug)]
struct AddArgs {
    name: String,
    /// Directory relative to cwd, translated through the workspace's canonical layout.
    /// The directory may be absent in recorded @; files and symlinks are rejected.
    #[arg(long)]
    path: String,
}

#[derive(clap::Args, Clone, Debug)]
struct OutputArgs {
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args, Clone, Debug)]
struct ShowArgs {
    project: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(clap::Args, Clone, Debug)]
struct CheckArgs {
    project: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(clap::Args, Clone, Debug)]
struct NameArgs { name: String }

#[derive(clap::Args, Clone, Debug)]
struct RenameArgs { old: String, new: String }

#[derive(clap::Args, Clone, Debug)]
#[command(group(clap::ArgGroup::new("record").required(true).args(["id", "binding", "label"])))]
#[command(group(clap::ArgGroup::new("action").required(true).args(["candidate", "delete", "name"])))]
struct ResolveArgs {
    /// Exact ProjectId from project list/show.
    #[arg(long)]
    id: Option<String>,
    /// Exact BindingId; definitions cannot be rewritten.
    #[arg(long)]
    binding: Option<String>,
    /// Stable reference label to resolve.
    #[arg(long)]
    label: Option<String>,
    /// Choose a positive candidate by its one-based index in list/show output.
    #[arg(long)]
    candidate: Option<usize>,
    /// Explicitly delete a definition; dependencies must already be removed.
    #[arg(long)]
    delete: bool,
    /// Resolve a project's display name, retaining its unique immutable root.
    #[arg(long, requires = "id")]
    name: Option<String>,
}

pub(crate) fn require_current_operation(command: &CommandHelper) -> Result<(), CommandError> {
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error("Project changes require the current integrated operation"));
    }
    Ok(())
}

pub(crate) async fn recorded_workspace(ui: &Ui, command: &CommandHelper) -> Result<WorkspaceCommandHelper, CommandError> {
    let workspace = command.load_workspace()?;
    let loader = workspace.repo_loader();
    let operation = if let Some(op) = &command.global_args().at_operation {
        jj_lib::op_walk::resolve_op_for_load(loader, op).await?
    } else {
        let heads = jj_lib::op_walk::get_current_head_ops(loader.op_store(), loader.op_heads_store().as_ref()).await?;
        let [operation] = heads.as_slice() else {
            return Err(user_error("Project commands require one recorded operation head; reconcile operations first, or select --at-operation for inspection"));
        };
        operation.clone()
    };
    let repo = loader.load_at(&operation).await?;
    command.for_workable_repo(ui, workspace, repo)
}

pub(crate) async fn run(ui: &mut Ui, command: &CommandHelper, args: Args) -> Result<(), CommandError> {
    match args.command {
        Command::Import(args) => crate::native::run_import(ui, command, args).await,
        Command::Migrate(args) => crate::project_migration::run(ui, command, &args).await,
        Command::List(args) => inspect(ui, command, None, args.json, false).await,
        Command::Show(args) => inspect(ui, command, Some(args.project), args.output.json, false).await,
        Command::Check(args) => inspect(ui, command, args.project, args.output.json, true).await,
        mutation => {
            require_current_operation(command)?;
            let mut workspace = recorded_workspace(ui, command).await?;
            let mut view = workspace.repo().view().store_view().clone();
            let description = match mutation {
                Command::Add(args) => {
                    let name = crate::native_project::parse_project(&args.name).map_err(user_error)?;
                    workspace.repo().view().check_project_label_available(&name).map_err(user_error)?;
                    let root = workspace.parse_file_path(&args.path)?;
                    crate::project_config::validate_registration(&view, &name, &root).map_err(user_error)?;
                    let commit = workspace.resolve_single_rev(ui, &RevisionArg::AT).await?;
                    crate::native_project::project_tree(&commit, &root).await.map_err(user_error)?;
                    crate::project_config::register(&mut view, &name, &root).map_err(user_error)?;
                    format!("register project {name}")
                }
                Command::Rename(args) => {
                    let (id, record) = view.project_state.project_by_name(&args.old).map_err(user_error)?;
                    let mut record = record.clone();
                    record.name = crate::native_project::parse_project(&args.new).map_err(user_error)?;
                    view.project_state.projects.insert(id.clone(), Merge::resolved(Some(record)));
                    view.project_state.validate_project(&id).map_err(user_error)?;
                    format!("rename project {} to {}", args.old, args.new)
                }
                Command::Remove(args) => {
                    let (id, _) = view.project_state.project_by_name(&args.name).map_err(user_error)?;
                    remove_project(&mut view, &id)?;
                    format!("remove project {}", args.name)
                }
                Command::Resolve(args) => {
                    resolve(&mut view, args, workspace.repo().store())?;
                    "resolve project metadata".to_owned()
                }
                _ => unreachable!(),
            };
            let mut tx = workspace.start_transaction();
            tx.repo_mut().set_view(view);
            tx.into_inner().commit(&description).await?;
            writeln!(ui.status(), "{description}; working files unchanged.")?;
            Ok(())
        }
    }
}

fn remove_project(view: &mut jj_lib::op_store::View, id: &ProjectId) -> Result<(), CommandError> {
    let labels = crate::project_config::validate_removal(view, id).map_err(user_error)?;
    view.project_state.projects.remove(id);
    for label in labels { view.project_state.labels.remove(&label); }
    Ok(())
}

fn candidate<T: Clone>(
    target: &Merge<Option<T>>,
    index: Option<usize>,
) -> Result<Option<T>, CommandError> {
    let index = index.ok_or_else(|| user_error("Specify --candidate or --delete"))?;
    target.adds().nth(index.checked_sub(1).ok_or_else(|| user_error("Candidate indices start at 1"))?)
        .cloned().ok_or_else(|| user_error("Candidate index is outside the displayed positive candidates"))
}

fn resolve(
    view: &mut jj_lib::op_store::View,
    args: ResolveArgs,
    store: &jj_lib::store::Store,
) -> Result<(), CommandError> {
    if let Some(value) = args.id {
        let id = ProjectId::try_from_hex(&value).filter(|id| id.as_bytes().len() == 16).ok_or_else(|| user_error("Expected a 32-digit ProjectId"))?;
        let target = view.project_state.projects.get(&id).ok_or_else(|| user_error("Unknown ProjectId"))?;
        let mut roots = target.iter().flatten().map(|record| &record.canonical_root);
        if let Some(first) = roots.next()
            && roots.any(|root| root != first) {
                return Err(user_error("ProjectId has inconsistent immutable roots; repair the corrupt input instead of choosing a layout"));
            }
        let selected = if args.delete { None } else if let Some(name) = args.name {
            let mut record = target.adds().flatten().next().cloned().ok_or_else(|| user_error("Project has no positive definition"))?;
            record.name = crate::native_project::parse_project(&name).map_err(user_error)?;
            Some(record)
        } else { candidate(target, args.candidate)? };
        if let Some(record) = selected {
            view.project_state.projects.insert(id.clone(), Merge::resolved(Some(record)));
        } else { remove_project(view, &id)?; }
    } else if let Some(value) = args.binding {
        let id = BindingId::try_from_hex(&value).filter(|id| id.as_bytes().len() == 16).ok_or_else(|| user_error("Expected a 32-digit BindingId"))?;
        let target = view.project_state.bindings.get(&id).ok_or_else(|| user_error("Unknown BindingId"))?;
        let definitions: Vec<_> = target.iter().flatten().collect();
        if definitions.windows(2).any(|pair| pair[0] != pair[1]) {
            return Err(user_error("BindingId has inconsistent immutable definitions; repair the corrupt input instead of choosing one"));
        }
        if args.delete {
            if let Ok(git) = jj_lib::git::get_git_repo(store) {
                for name in git.remote_names() {
                    let Ok(name) = std::str::from_utf8(&name) else { continue; };
                    let connection = git.config_snapshot().string(&format!("remote.{name}.jjosh-connectionId"))
                        .and_then(|value| jj_lib::project::ConnectionId::try_from_hex(&value));
                    if definitions.iter().any(|record| connection.as_ref() == Some(&record.connection_id)) {
                        return Err(user_error(format!("Binding is connected to remote {name}; use git remote remove to retire it atomically")));
                    }
                }
            }
            if view.project_observations.values().any(|value| value.adds().flatten().any(|observation| observation.binding_id == id && observation.terms.iter().any(|term| term.canonical.is_some()))) {
                return Err(user_error("Binding still owns observations; explicitly clear its remote references/tracking before retiring it"));
            }
            view.project_state.bindings.remove(&id);
        } else {
            let selected = candidate(target, args.candidate)?;
            if selected.is_none() { return Err(user_error("Use --delete to retire a binding explicitly")); }
            view.project_state.bindings.insert(id, Merge::resolved(selected));
        }
    } else if let Some(label) = args.label {
        let target = view.project_state.labels.get(&label).ok_or_else(|| user_error("Unknown reference label"))?;
        let selected = if args.delete { None } else { candidate(target, args.candidate)? };
        let references = crate::project_config::label_references(view, &label);
        if selected.is_none() && !references.is_empty() {
            return Err(user_error(format!("Label {label:?} still has references: {}", references.join(", "))));
        }
        if let Some(id) = selected {
            if !view.project_state.projects.get(&id).is_some_and(|target| target.adds().flatten().next().is_some()) {
                return Err(user_error("Selected label candidate refers to an absent project; restore or resolve that project first"));
            }
            view.project_state.labels.insert(label, Merge::resolved(Some(id)));
        } else { view.project_state.labels.remove(&label); }
    }
    Ok(())
}

async fn inspect(ui: &mut Ui, command: &CommandHelper, selected: Option<String>, json: bool, check: bool) -> Result<(), CommandError> {
    let workspace = recorded_workspace(ui, command).await?;
    let state = workspace.repo().view().project_state();
    let selected_ids: Vec<_> = state.projects.iter().filter(|(id, target)| selected.as_ref().is_none_or(|name| id.hex() == *name || target.adds().flatten().any(|record| record.name == *name))).map(|(id, _)| id.clone()).collect();
    if selected.is_some() && selected_ids.is_empty() { return Err(user_error("Unknown project name or ProjectId")); }
    let mut diagnostics = workspace.repo().view().project_diagnostics();
    let (connections, offline_bindings) = local_diagnostics(workspace.repo().as_ref(), &mut diagnostics);
    diagnostics.retain(|diagnostic| selected.is_none() || diagnostic.projects.iter().any(|id| selected_ids.contains(id)));
    let projects: Vec<_> = selected_ids.iter().map(|id| {
        let target = &state.projects[id];
        let records: Vec<_> = target.adds().enumerate().map(|(index, record)| serde_json::json!({"candidate":index+1, "definition":record.as_ref().map(|record| serde_json::json!({"name":record.name,"path":record.canonical_root.as_internal_file_string()}))})).collect();
        let labels: Vec<_> = state.labels.iter().filter(|(_, target)| target.adds().flatten().any(|candidate| candidate == id)).map(|(label, target)| serde_json::json!({"label":label,"resolved":target.as_resolved().is_some(),"candidates":target.adds().enumerate().map(|(index, id)| serde_json::json!({"candidate":index+1,"project":id.as_ref().map(|id| id.hex())})).collect::<Vec<_>>()})).collect();
        let bindings: Vec<_> = state.bindings.iter().filter(|(_, target)| target.adds().flatten().any(|record| record.target == jj_lib::project::BindingTarget::Project(id.clone()))).map(|(id, target)| serde_json::json!({"id":id.hex(),"resolved":target.as_resolved().is_some(),"offline_provenance":offline_bindings.contains(id),"candidates":target.adds().enumerate().map(|(index, record)| serde_json::json!({"candidate":index+1,"definition":record,"connections":record.as_ref().and_then(|record| connections.get(&record.connection_id)).cloned().unwrap_or_default()})).collect::<Vec<_>>()})).collect();
        serde_json::json!({"id":id.hex(),"resolved":target.as_resolved().is_some(),"candidates":records,"labels":labels,"bindings":bindings})
    }).collect();
    let diagnostic_values: Vec<_> = diagnostics.iter().map(|diagnostic| serde_json::json!({"message":diagnostic.message,"projects":diagnostic.projects.iter().map(|id|id.hex()).collect::<Vec<_>>(),"bindings":diagnostic.bindings.iter().map(|id|id.hex()).collect::<Vec<_>>(),"labels":diagnostic.labels})).collect();
    if json {
        serde_json::to_writer_pretty(ui.stdout(), &serde_json::json!({"projects":projects,"diagnostics":diagnostic_values})).map_err(user_error)?;
        writeln!(ui.stdout())?;
    } else {
        for project in &projects {
            writeln!(ui.stdout(), "Project {}{}", project["id"].as_str().unwrap(), if project["resolved"].as_bool() == Some(true) { "" } else { " (unresolved)" })?;
            for record in project["candidates"].as_array().unwrap() {
                writeln!(ui.stdout(), "  candidate {}: {}", record["candidate"], record["definition"])?;
            }
            for label in project["labels"].as_array().unwrap() { writeln!(ui.stdout(), "  label: {label}")?; }
            for binding in project["bindings"].as_array().unwrap() { writeln!(ui.stdout(), "  binding: {binding}")?; }
        }
        for diagnostic in &diagnostics { writeln!(ui.stdout(), "Problem: {diagnostic}")?; }
        if projects.is_empty() && diagnostics.is_empty() { writeln!(ui.stdout(), "No projects registered.")?; }
        if check && diagnostics.is_empty() { writeln!(ui.stdout(), "Project metadata is consistent.")?; }
    }
    if check && !diagnostics.is_empty() { return Err(user_error("Project metadata has unresolved problems")); }
    Ok(())
}

fn local_diagnostics(
    repo: &dyn jj_lib::repo::Repo,
    diagnostics: &mut Vec<jj_lib::project::ProjectDiagnostic>,
) -> (
    std::collections::BTreeMap<jj_lib::project::ConnectionId, Vec<String>>,
    std::collections::BTreeSet<BindingId>,
) {
    use jj_lib::project::{BindingTarget, ProjectDiagnostic};
    let state = repo.view().project_state();
    let mut connections = std::collections::BTreeMap::<_, Vec<String>>::new();
    if let Ok(git) = jj_lib::git::get_git_repo(repo.store()) {
        for name in git.remote_names() {
            let Ok(name) = std::str::from_utf8(&name) else {
                diagnostics.push(ProjectDiagnostic { message: "Local remote name is not UTF-8".to_owned(), projects: vec![], bindings: vec![], labels: vec![] });
                continue;
            };
            let remote = jj_lib::ref_name::RemoteName::new(name);
            // Read identity without selecting a binding so conflicts themselves
            // remain inspectable and diagnostics can attach every dependent ID.
            let identity = crate::git_remote::config_string(&git, &format!("remote.{name}.jjosh-connectionId"))
                .ok().flatten().and_then(jj_lib::project::ConnectionId::try_from_hex);
            if let Some(id) = &identity { connections.entry(id.clone()).or_default().push(name.to_owned()); }
            let binding_ids: Vec<_> = state.bindings.iter().filter(|(_, target)| target.adds().flatten().any(|record| identity.as_ref() == Some(&record.connection_id))).map(|(id, _)| id.clone()).collect();
            let project_ids: Vec<_> = binding_ids.iter().flat_map(|id| state.bindings[id].adds().flatten()).filter_map(|record| match &record.target { BindingTarget::Project(id) => Some(id.clone()), BindingTarget::RepositoryView => None }).collect();
            let mut problems = Vec::new();
            if let Err(error) = jj_lib::git::remote_connection_id(&git, remote) { problems.push(error); }
            if let Err(error) = jj_lib::git::check_remote_owner(repo.view(), remote, identity.as_ref()) { problems.push(error); }
            if !binding_ids.is_empty() {
                if let Err(error) = crate::git_remote::remote_read_only(&git, remote) { problems.push(format!("{error:#}")); }
                if jj_lib::git::remote_required_capability(&git, remote).as_deref() != Some("jjosh-v1") {
                    problems.push(format!("Remote {name} has a binding but lacks the required jjosh-v1 capability marker"));
                }
                match git.find_remote(name) {
                    Ok(remote) if remote.urls(gix::remote::Direction::Fetch).count() == 1 => {}
                    Ok(_) => problems.push(format!("Remote {name} needs exactly one fetch URL")),
                    Err(error) => problems.push(format!("Cannot read remote {name}: {error}")),
                }
            } else if jj_lib::git::remote_required_capability(&git, remote).is_some() {
                problems.push(format!("Managed remote {name} has no active operation binding"));
            }
            for message in problems {
                diagnostics.push(ProjectDiagnostic { message, projects: project_ids.clone(), bindings: binding_ids.clone(), labels: vec![] });
            }
        }
    }
    let mut offline_bindings = std::collections::BTreeSet::new();
    let needs_provenance = state.bindings.values().any(|target| target.adds().flatten().any(|record| {
        record.representation == jj_lib::project::Representation::Whole
            && !connections.contains_key(&record.connection_id)
    }));
    let provenance = needs_provenance.then(|| {
        jj_lib::git::get_git_backend(repo.store()).ok().map(|backend| {
            crate::interop::open_josh_transaction(backend.git_repo_path(), true)
        })
    }).flatten();
    for (id, target) in &state.bindings {
        for record in target.adds().flatten() {
            if !connections.contains_key(&record.connection_id) {
                if record.representation == jj_lib::project::Representation::Whole {
                    let offline = match &provenance {
                        Some(Ok(transaction)) => crate::native_project::is_offline_binding(transaction, id)
                            .map_err(|error| error.to_string()),
                        Some(Err(error)) => Err(error.error.to_string()),
                        None => Ok(false),
                    };
                    match offline {
                        Ok(true) => {
                            offline_bindings.insert(id.clone());
                            continue;
                        }
                        Ok(false) => {}
                        Err(error) => diagnostics.push(ProjectDiagnostic {
                            message: format!("Cannot read offline provenance for binding {id}: {error}"),
                            projects: match &record.target { BindingTarget::Project(id) => vec![id.clone()], BindingTarget::RepositoryView => vec![] },
                            bindings: vec![id.clone()], labels: vec![],
                        }),
                    }
                }
                diagnostics.push(ProjectDiagnostic {
                    message: format!("Binding {id} is disconnected: its original local connection {} is unavailable", record.connection_id),
                    projects: match &record.target { BindingTarget::Project(id) => vec![id.clone()], BindingTarget::RepositoryView => vec![] },
                    bindings: vec![id.clone()], labels: vec![],
                });
            }
        }
    }
    (connections, offline_bindings)
}
