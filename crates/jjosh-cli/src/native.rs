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
    /// Import recorded jj repositories into an existing or new monorepo.
    ///
    /// Each source retains its native graph and references under NAME. Trees are
    /// mounted at NAME/ by default, or at --mount NAME=DEST for a nested path.
    /// Sources are read without snapshotting; record working files in the source first.
    /// Bundles remain accepted as an optional offline source. Import records
    /// ancestry correspondences for subsequent native fetch/push. It does not
    /// merge or check out the imported heads: use normal jj new/rebase commands.
    Import(ImportArgs),
    /// Receive an upstream or contribution branch in native monorepo coordinates.
    Fetch(crate::native_project::FetchArgs),
    /// Publish a project revision to an explicitly chosen remote and branch.
    Push(crate::native_project::PushArgs),
    /// Move legacy native correspondence bookmarks into private Git refs.
    Migrate,
    /// Relocate a native change graph with explicit path and parent mappings.
    Transplant(crate::transplant::Args),
}

#[derive(clap::Args, Clone, Debug)]
struct BundleArgs {
    /// Native bundle file (export never overwrites an existing file).
    file: PathBuf,
}

#[derive(clap::Args, Clone, Debug)]
struct ImportArgs {
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
        Command::Fetch(args) => crate::native_project::fetch(ui, command, args).await,
        Command::Push(args) => crate::native_project::push(ui, command, args).await,
        Command::Migrate => run_migrate(ui, command).await,
        Command::Inspect(args) => {
            let source =
                crate::native_bundle::load(&command.cwd().join(args.file), command.settings())
                    .await
                    .map_err(|err| user_error_with_message("Cannot inspect native bundle", err))?;
            writeln!(
                ui.stdout(),
                "Valid native bundle: {} commits, {} heads, source operation {}",
                source.commits.len() - 1,
                source.view.head_ids.len(),
                source.source_operation
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
        Command::Import(args) => run_import(ui, command, args).await,
    }
}

async fn run_import(
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
    let mut workspace = command.workspace_helper(ui).await?;
    let git_lock = workspace.lock_git_import_export()?;
    let git_path = crate::interop::sha1_git_repo_path(&workspace)?;
    let transaction = crate::interop::open_josh_transaction(&git_path, false)?;
    let mut sources = Vec::with_capacity(resolved.len());
    for (name, path, mount) in resolved {
        let view = workspace.repo().view().store_view();
        let mut has_history = false;
        transaction
            .for_each_ref_prefixed(&crate::native_project::project_ref_prefix(&name), |_, _| {
                has_history = true;
                Ok(())
            })
            .map_err(user_error)?;
        if view
            .local_bookmarks
            .keys()
            .chain(view.local_tags.keys())
            .any(|key| crate::ref_names::belongs_to_project(&name, key.as_str()))
            || has_history
        {
            return Err(user_error(format!(
                "Project namespace {name:?} already exists; receive updates with native fetch"
            )));
        }
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
        sources.push((name, source, mount));
    }
    let mut tx = workspace.start_transaction();
    let mut view = tx.repo().view().store_view().clone();
    let mut summaries = Vec::new();
    let mut roots = Vec::new();
    let mut remotes = HashSet::new();
    for (name, source, mount) in &sources {
        let imported =
            crate::native_import::import_source(source, tx.repo_mut(), name, mount, HashMap::new())
                .await
                .map_err(|err| {
                    user_error_with_message(format!("Cannot import native source {name}"), err)
                })?;
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
            roots.push((name.clone(), raw.clone(), imported.ids[raw].clone()));
        }
        view.head_ids.extend(imported.view.head_ids);
        view.local_bookmarks.extend(imported.view.local_bookmarks);
        view.local_tags.extend(imported.view.local_tags);
        view.remote_views.extend(imported.view.remote_views);
        summaries.push((name, imported.commits.len(), imported.stripped_signatures));
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
    for (name, raw, mapped) in roots {
        crate::native_project::record_anchor(&transaction, &name, "origin", &raw, &mapped)
            .map_err(|err| {
                user_error_with_message("Cannot record native source correspondence", err)
            })?;
    }
    for (name, _, mount) in &sources {
        crate::native_project::record_mount(&transaction, name, mount)
            .map_err(|err| user_error_with_message("Cannot record native project mount", err))?;
    }
    transaction
        .flush_mem_odb()
        .map_err(|err| user_error_with_message("Cannot retain native source objects", err))?;
    let stats =
        jj_lib::git::export_some_refs(tx.repo_mut(), |_, symbol| remotes.contains(symbol.remote))?;
    jj_cli::git_util::print_git_export_stats(ui, &stats)?;
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

async fn run_migrate(ui: &mut Ui, command: &CommandHelper) -> Result<(), CommandError> {
    require_current_operation(command)?;
    let mut workspace = command.workspace_helper(ui).await?;
    let _git_lock = workspace.lock_git_import_export()?;
    let transaction = crate::interop::open_josh_transaction(
        &crate::interop::sha1_git_repo_path(&workspace)?,
        false,
    )?;
    let mut view = workspace.repo().view().store_view().clone();
    let legacy: Vec<_> = view
        .remote_views
        .keys()
        .filter(|name| name.as_str().starts_with("jjosh-native-"))
        .cloned()
        .collect();
    let mut changed = HashSet::new();
    for remote in &legacy {
        let project = remote.as_str().strip_prefix("jjosh-native-").unwrap();
        crate::native_project::validate_project(project).map_err(user_error)?;
        let git = jj_lib::git::get_git_repo(workspace.repo().store())?;
        if git
            .remote_names()
            .iter()
            .any(|name| &name[..] == remote.as_str().as_bytes())
        {
            return Err(user_error(
                "Legacy correspondence name is a configured Git remote; rename that remote first",
            ));
        }
        let refs = &view.remote_views[remote];
        if !refs.tags.is_empty() {
            return Err(user_error(
                "Legacy correspondence remote contains tags; resolve it before migration",
            ));
        }
        for (name, reference) in &refs.bookmarks {
            if reference.state == jj_lib::op_store::RemoteRefState::Tracked {
                return Err(user_error(
                    "Untrack legacy correspondence bookmarks before migration",
                ));
            }
            let canonical = reference.target.as_normal().ok_or_else(|| {
                user_error("Resolve conflicted legacy correspondences before migration")
            })?;
            let (kind, raw) = name
                .as_str()
                .split_once('/')
                .ok_or_else(|| user_error("Invalid legacy native correspondence"))?;
            if !matches!(kind, "origin" | "published") {
                return Err(user_error("Invalid legacy native correspondence kind"));
            }
            let raw = jj_lib::backend::CommitId::try_from_hex(raw)
                .filter(|id| jj_lib::object_id::ObjectId::as_bytes(id).len() == 20)
                .ok_or_else(|| user_error("Invalid legacy native commit ID"))?;
            crate::native_project::record_anchor(&transaction, project, kind, &raw, canonical)
                .map_err(user_error)?;
        }
        let git_remote: jj_lib::ref_name::RemoteNameBuf = format!("{project}-git").into();
        if let Some(git) = view.remote_views.get_mut(&git_remote) {
            git.bookmarks.retain(|name, reference| {
                view.local_bookmarks.get(name) != Some(&reference.target)
            });
            git.tags
                .retain(|name, reference| view.local_tags.get(name) != Some(&reference.target));
            if git.bookmarks.is_empty() && git.tags.is_empty() {
                view.remote_views.remove(&git_remote);
            }
            changed.insert(git_remote);
        }
        view.remote_views.remove(remote);
        changed.insert(remote.clone());
    }
    if legacy.is_empty() {
        writeln!(
            ui.status(),
            "No legacy native correspondence bookmarks to migrate."
        )?;
        return Ok(());
    }
    let mut tx = workspace.start_transaction();
    tx.repo_mut().set_view(view);
    for remote in &legacy {
        crate::native_project::anchors(
            tx.repo(),
            &transaction,
            remote.as_str().strip_prefix("jjosh-native-").unwrap(),
        )
        .await
        .map_err(user_error)?;
    }
    transaction.flush_mem_odb().map_err(user_error)?;
    let stats =
        jj_lib::git::export_some_refs(tx.repo_mut(), |_, symbol| changed.contains(symbol.remote))?;
    jj_cli::git_util::print_git_export_stats(ui, &stats)?;
    tx.into_inner()
        .commit("migrate native correspondence bookmarks to private refs")
        .await?;
    writeln!(
        ui.status(),
        "Migrated {} native projects; history and working files unchanged.",
        legacy.len()
    )?;
    Ok(())
}
