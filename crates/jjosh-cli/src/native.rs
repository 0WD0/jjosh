use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;
use jj_cli::ui::Ui;
use jj_lib::repo::Repo as _;

use crate::native_source::NativeSource;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum Command {
    /// Export recorded native state to an optional self-contained transport file.
    ///
    /// Does not snapshot or write the source repository. Old operation/evolution
    /// histories, user settings and credentials are not exported.
    Export(BundleArgs),
    /// Validate a native bundle without needing its source repository.
    Inspect(BundleArgs),
    /// Relocate a native change graph with explicit path and parent mappings.
    Transplant(crate::transplant::Args),
}

#[derive(clap::Args, Clone, Debug)]
struct BundleArgs {
    /// Native bundle file (export never overwrites an existing file).
    file: PathBuf,
}

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct ImportArgs {
    /// Native jj workspace or bundle and its identity (repeatable).
    #[arg(long, value_name = "NAME=PATH", required = true)]
    source: Vec<String>,
    /// Dest directory for an imported project (repeatable). Defaults to NAME/.
    #[arg(long, value_name = "NAME=DEST")]
    mount: Vec<String>,
}

fn require_current_operation(command: &CommandHelper) -> Result<(), CommandError> {
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error(
            "Native state changes require the current integrated operation",
        ));
    }
    Ok(())
}

pub(crate) async fn run(
    ui: &mut Ui,
    command: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Transplant(args) => crate::transplant::run(ui, command, args).await,
        Command::Inspect(args) => {
            let source =
                crate::native_bundle::load(&command.cwd().join(args.file), command.settings())
                    .await
                    .map_err(|err| user_error_with_message("Cannot inspect native bundle", err))?;
            writeln!(
                ui.stdout(),
                "Valid native bundle: {} commits, {} heads, source operation {}; {} project definitions, {} binding definitions, {} conversion observations (inert source metadata)",
                source.commits.len() - 1,
                source.view.head_ids.len(),
                source.source_operation,
                source.view.project_state.projects.len(),
                source.view.project_state.bindings.len(),
                source.view.project_observations.len(),
            )?;
            Ok(())
        }
        Command::Export(args) => {
            require_current_operation(command)?;
            let workspace = command.load_workspace()?;
            let source = NativeSource::read(workspace.repo_loader(), None)
                .await
                .map_err(|err| user_error_with_message("Cannot read native source", err))?;
            let count = crate::native_bundle::export(
                &source,
                &command.cwd().join(&args.file),
                command.settings(),
            )
            .await
            .map_err(|err| user_error_with_message("Cannot export native bundle", err))?;
            writeln!(
                ui.status(),
                "Exported {count} native commits to {}. Source state unchanged.",
                args.file.display()
            )?;
            Ok(())
        }
    }
}

pub(crate) async fn run_import(
    ui: &mut Ui,
    command: &CommandHelper,
    args: ImportArgs,
) -> Result<(), CommandError> {
    require_current_operation(command)?;
    let mut names = HashSet::new();
    let mut specifications = Vec::with_capacity(args.source.len());
    for value in args.source {
        let (name, path) = value
            .split_once('=')
            .ok_or_else(|| user_error("Expected --source NAME=PATH"))?;
        let name = crate::native_project::parse_project(name)
            .map_err(|err| user_error_with_message("Invalid native project name", err))?;
        if path.is_empty() || !names.insert(name.clone()) {
            return Err(user_error(
                "Source paths must not be empty and project names must be unique",
            ));
        }
        specifications.push((name, command.cwd().join(path)));
    }
    let mut mounts = HashMap::new();
    for value in args.mount {
        let (name, path) = value
            .split_once('=')
            .ok_or_else(|| user_error("Expected --mount NAME=DEST"))?;
        let name = crate::native_project::parse_project(name)
            .map_err(|err| user_error_with_message("Invalid native project name", err))?;
        if !names.contains(&name) {
            return Err(user_error(format!(
                "Mount {name:?} does not match a --source project"
            )));
        }
        let mount = crate::native_project::parse_mount(path).map_err(|err| {
            user_error_with_message(format!("Invalid mount for native project {name}"), err)
        })?;
        if mounts.insert(name.clone(), mount).is_some() {
            return Err(user_error(format!("Duplicate mount for project {name:?}")));
        }
    }
    let mut resolved = Vec::with_capacity(specifications.len());
    for (name, path) in specifications {
        let mount = match mounts.remove(&name) {
            Some(mount) => mount,
            None => crate::native_project::default_mount(&name).map_err(user_error)?,
        };
        resolved.push((name, path, mount));
    }
    crate::native_project::check_mounts_disjoint(
        resolved
            .iter()
            .map(|(name, _, mount)| (name.as_str(), mount.as_ref())),
    )
    .map_err(user_error)?;
    let mut workspace = crate::project::recorded_workspace(ui, command).await?;
    let git_lock = workspace.lock_git_import_export()?;
    let git_path = crate::interop::sha1_git_repo_path(&workspace)?;
    let transaction = crate::interop::open_josh_transaction(&git_path, false)?;
    let mut sources = Vec::with_capacity(resolved.len());
    let mut planned_view = workspace.repo().view().store_view().clone();
    let mut reserved_remotes = HashSet::new();
    for (name, path, mount) in resolved {
        workspace
            .repo()
            .view()
            .check_project_label_available(&name)
            .map_err(user_error)?;
        let project_id = crate::project_config::register(&mut planned_view, &name, &mount)
            .map_err(user_error)?;
        let binding_id = jj_lib::project::BindingId::generate();
        planned_view.project_state.bindings.insert(
            binding_id.clone(),
            jj_lib::merge::Merge::resolved(Some(jj_lib::project::BindingRecord {
                target: jj_lib::project::BindingTarget::Project(project_id.clone()),
                connection_id: jj_lib::project::ConnectionId::generate(),
                representation: jj_lib::project::Representation::Whole,
                base: None,
            })),
        );
        let view = workspace.repo().view().store_view();
        for head in &view.head_ids {
            let commit = workspace.repo().store().get_commit_async(head).await?;
            if crate::native_project::commit_path_occupied(&commit, &mount)
                .await
                .map_err(user_error)?
            {
                return Err(user_error(format!(
                    "Destination path {} is already occupied in visible history",
                    mount.as_internal_file_string()
                )));
            }
        }
        let source = if path.is_dir() {
            let source_workspace = command.load_workspace_at(&path, workspace.settings())?;
            if std::fs::canonicalize(source_workspace.repo_path())?
                == std::fs::canonicalize(workspace.repo_path())?
            {
                return Err(user_error(
                    "Cannot import a workspace from the destination's own repository",
                ));
            }
            NativeSource::read(source_workspace.repo_loader(), None).await
        } else {
            crate::native_bundle::load(&path, workspace.settings()).await
        }
        .map_err(|err| user_error_with_message(format!("Cannot read native source {name}"), err))?;
        let remote_plan =
            crate::native_import::plan_remotes(&source.view, &project_id).map_err(|err| {
                user_error_with_message(format!("Cannot map native source remotes for {name}"), err)
            })?;
        let source_view = jj_lib::view::View::new(source.view.clone(), false);
        for remote in &remote_plan {
            if !reserved_remotes.insert(remote.physical.clone()) {
                return Err(user_error(
                    "Imported sources have colliding remote connection identities",
                ));
            }
            if planned_view
                .remote_connections
                .contains_key(&remote.physical)
                || planned_view.remote_views.contains_key(&remote.physical)
                || jj_lib::git::get_git_repo(workspace.repo().store())?
                    .find_remote(remote.physical.as_str())
                    .is_ok()
            {
                return Err(user_error(
                    "Imported remote identity collides with an existing connection",
                ));
            }
            writeln!(
                ui.status(),
                "Import remote {} -> {}#{} (disconnected)",
                source_view.remote_qualified_name(&remote.source),
                remote.alias.name.as_str(),
                name
            )?;
        }
        sources.push((name, source, mount, binding_id, remote_plan));
    }
    let mut tx = workspace.start_transaction();
    let mut view = planned_view;
    let mut summaries = Vec::new();
    let mut roots = Vec::new();
    let mut remotes = HashSet::new();
    for (name, source, mount, binding_id, remote_plan) in &mut sources {
        let mut imported =
            crate::native_import::import_source(source, tx.repo_mut(), name, mount, HashMap::new())
                .await
                .map_err(|err| {
                    user_error_with_message(format!("Cannot import native source {name}"), err)
                })?;
        crate::native_import::install_remote_names(&mut imported.view, std::mem::take(remote_plan));
        for remote in imported.view.remote_views.keys() {
            if view.remote_views.contains_key(remote) || remotes.contains(remote) {
                return Err(user_error(format!(
                    "Imported remote namespace collision: {}",
                    remote.as_str()
                )));
            }
            remotes.insert(remote.clone());
        }
        let parents: HashSet<_> = source
            .commits
            .values()
            .flat_map(|commit| commit.parents.iter())
            .collect();
        for raw in source.commits.keys().filter(|id| !parents.contains(id)) {
            roots.push((binding_id.clone(), raw.clone(), imported.ids[raw].clone()));
        }
        view.head_ids.extend(imported.view.head_ids);
        view.local_bookmarks.extend(imported.view.local_bookmarks);
        view.local_tags.extend(imported.view.local_tags);
        view.remote_views.extend(imported.view.remote_views);
        view.remote_connections
            .extend(imported.view.remote_connections);
        view.project_state
            .remote_names
            .extend(imported.view.project_state.remote_names);
        summaries.push((
            name.clone(),
            imported.commits.len(),
            imported.stripped_signatures,
        ));
    }
    let git = jj_lib::git::get_git_repo(tx.repo().store())?;
    for configured in git.remote_names() {
        if remotes
            .iter()
            .any(|remote| remote.as_str().as_bytes() == &configured[..])
        {
            return Err(user_error(
                "Imported reference namespace collides with a configured Git remote",
            ));
        }
    }
    tx.repo_mut().set_view(view);
    for (_, _, _, binding_id, _) in &sources {
        crate::native_project::record_offline_binding(&transaction, binding_id).map_err(|err| {
            user_error_with_message("Cannot retain offline native binding provenance", err)
        })?;
    }
    for (binding_id, raw, mapped) in roots {
        crate::native_project::record_anchor(&transaction, &binding_id, "origin", &raw, &mapped)
            .map_err(|err| {
                user_error_with_message("Cannot record native source correspondence", err)
            })?;
    }
    transaction
        .flush_mem_odb()
        .map_err(|err| user_error_with_message("Cannot retain native source objects", err))?;
    let stats =
        jj_lib::git::export_some_refs(tx.repo_mut(), |_, symbol| remotes.contains(symbol.remote))?;
    jj_cli::git_util::print_git_export_stats(ui, tx.repo().view(), &stats)?;
    tx.into_inner()
        .commit("import native project states")
        .await?;
    drop(git_lock);
    for (name, count, signatures) in summaries {
        writeln!(
            ui.status(),
            "Imported {name}: {count} commits, {signatures} invalidated signatures removed."
        )?;
    }
    writeln!(
        ui.status(),
        "Native states imported in one transaction; working copy unchanged. Compose source \
         workspace bookmarks with jjosh new."
    )?;
    Ok(())
}
