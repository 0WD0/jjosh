use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::repo::{ReadonlyRepo, Repo as _, RepoLoader};

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum Command {
    /// Export the recorded current native state to a self-contained bundle.
    ///
    /// Sources are read-only: run jj status beforehand to record working files.
    /// The bundle retains native commits, tree conflicts and the current view,
    /// but not old operation/evolution histories, configuration or credentials.
    Export(BundleArgs),
    /// Validate a native bundle without needing its source repository.
    Inspect(BundleArgs),
    /// Import native bundles under separate directory and ref namespaces.
    ///
    /// The destination must be a fresh Git-backed jj repository, either
    /// colocated or non-colocated. Current heads, refs, change identities and
    /// parent histories are retained. Path relocation invalidates signatures,
    /// which are removed. Import does not check out or merge the imported heads.
    /// Use jj new with NAME/workspace/WORKSPACE bookmarks to compose them.
    Import(ImportArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct BundleArgs {
    /// Native bundle file (export never overwrites an existing file).
    file: PathBuf,
}

#[derive(clap::Args, Clone, Debug)]
struct ImportArgs {
    /// Bundle and destination directory/ref namespace (repeatable).
    #[arg(long, value_name = "NAME=FILE", required = true)]
    source: Vec<String>,
}

async fn load_current(loader: &RepoLoader) -> Result<Arc<ReadonlyRepo>, CommandError> {
    // load_at_head() can reconcile operations and write to the repository.
    let heads =
        jj_lib::op_walk::get_current_head_ops(loader.op_store(), loader.op_heads_store().as_ref())
            .await?;
    let [operation] = heads.as_slice() else {
        return Err(user_error(
            "Native export/import requires exactly one operation head; reconcile operations separately",
        ));
    };
    let backend = jj_lib::git::get_git_backend(loader.store())?;
    if backend.git_repo().object_hash().len_in_bytes() != 20 {
        return Err(user_error(
            "Native bundles currently require a SHA-1 Git backend",
        ));
    }
    // Never infer a missing native change identity from raw Git objects.
    backend.disable_lazy_commit_imports();
    Ok(loader.load_at(operation).await?)
}

async fn require_fresh_destination(repo: &ReadonlyRepo) -> Result<(), CommandError> {
    let view = repo.view().store_view();
    let fresh_error = || {
        user_error("Native import requires a fresh destination; initialize one with jjosh git init")
    };
    if view.wc_commit_ids.len() != 1 {
        return Err(fresh_error());
    }
    let wc = view.wc_commit_ids.values().next().unwrap();
    if view.head_ids.len() != 1
        || !view.head_ids.contains(wc)
        || !view.local_bookmarks.is_empty()
        || !view.local_tags.is_empty()
        || !view.remote_views.is_empty()
        || !view.git_refs.is_empty()
        || view.git_heads.values().any(|head| head.is_present())
    {
        return Err(fresh_error());
    }
    let commit = repo.store().get_commit_async(wc).await?;
    if commit.parent_ids() != std::slice::from_ref(repo.store().root_commit_id())
        || commit.store_commit().root_tree.as_resolved() != Some(repo.store().empty_tree_id())
    {
        return Err(fresh_error());
    }
    Ok(())
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Inspect(args) => {
            let bundle = crate::native_bundle::load(
                &command_helper.cwd().join(args.file),
                command_helper.settings(),
            )
            .await
            .map_err(|err| user_error_with_message("Cannot inspect native bundle", err))?;
            writeln!(
                ui.stdout(),
                "Valid native bundle: {} commits, {} heads, source operation {}",
                bundle.commits.len() - 1,
                bundle.view.head_ids.len(),
                bundle.source_operation
            )?;
            Ok(())
        }
        Command::Export(args) => {
            require_current_operation(command_helper)?;
            let workspace = command_helper.load_workspace()?;
            let repo = load_current(workspace.repo_loader()).await?;
            let count = crate::native_bundle::export(&repo, &command_helper.cwd().join(&args.file))
                .await
                .map_err(|err| user_error_with_message("Cannot export native bundle", err))?;
            writeln!(
                ui.status(),
                "Exported {count} native commits to {}. Source state unchanged.",
                args.file.display()
            )?;
            Ok(())
        }
        Command::Import(args) => run_import(ui, command_helper, args).await,
    }
}

fn require_current_operation(command_helper: &CommandHelper) -> Result<(), CommandError> {
    if !command_helper.is_at_head_operation() || command_helper.global_args().no_integrate_operation
    {
        return Err(user_error(
            "Native export/import requires the current integrated operation",
        ));
    }
    Ok(())
}

async fn run_import(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: ImportArgs,
) -> Result<(), CommandError> {
    require_current_operation(command_helper)?;
    let mut names = HashSet::new();
    let mut specifications = Vec::with_capacity(args.source.len());
    for value in args.source {
        let Some((name, path)) = value.split_once('=') else {
            return Err(user_error("Expected --source NAME=FILE"));
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || path.is_empty()
        {
            return Err(user_error(
                "Source names must contain only ASCII letters, digits, '-' or '_', and bundle paths must not be empty",
            ));
        }
        if !names.insert(name.to_owned()) {
            return Err(user_error(format!("Duplicate source namespace: {name}")));
        }
        specifications.push((name.to_owned(), command_helper.cwd().join(path)));
    }

    let loaded_workspace = command_helper.load_workspace()?;
    let destination = load_current(loaded_workspace.repo_loader()).await?;
    let mut workspace = command_helper.for_workable_repo(ui, loaded_workspace, destination)?;
    let git_lock = workspace.lock_git_import_export()?;
    crate::interop::check_git_state(&workspace)?;
    require_fresh_destination(workspace.repo()).await?;

    let mut sources = Vec::with_capacity(specifications.len());
    for (name, path) in specifications {
        let bundle = crate::native_bundle::load(&path, workspace.settings())
            .await
            .map_err(|err| user_error_with_message(format!("Cannot load bundle {name}"), err))?;
        sources.push((name, bundle));
    }
    let mut view = workspace.repo().view().store_view().clone();
    let mut commits = Vec::new();
    let mut summaries = Vec::new();
    for (name, bundle) in &sources {
        let imported = crate::native_import::import_bundle(bundle, workspace.repo(), name)
            .await
            .map_err(|err| user_error_with_message(format!("Cannot import bundle {name}"), err))?;
        // NAME-REMOTE can collide for distinct namespace/remote pairs.
        for remote in imported.view.remote_views.keys() {
            if view.remote_views.contains_key(remote) {
                return Err(user_error(format!(
                    "Imported remote namespace collision: {}; choose different source names",
                    remote.as_str()
                )));
            }
        }
        view.head_ids.extend(imported.view.head_ids);
        view.local_bookmarks.extend(imported.view.local_bookmarks);
        view.local_tags.extend(imported.view.local_tags);
        view.remote_views.extend(imported.view.remote_views);
        summaries.push((name, imported.commits.len(), imported.stripped_signatures));
        commits.extend(imported.commits);
    }
    let current = load_current(workspace.repo().loader()).await?;
    if current.op_id() != workspace.repo().op_id() {
        return Err(user_error(
            "Destination changed during import; retry in a fresh repository",
        ));
    }
    crate::interop::check_git_state(&workspace)?;

    // Publish only the native view. In colocated mode, leave HEAD/index and
    // Git-ref observations unchanged. A subsequent normal jj command handles
    // checkout/export under the usual native synchronization rules.
    let mut tx = workspace.start_transaction().into_inner();
    tx.repo_mut().index_commits(&commits).await?;
    tx.repo_mut().set_view(view);
    tx.commit("import native state bundles").await?;
    drop(git_lock);
    for (name, count, signatures) in summaries {
        writeln!(
            ui.status(),
            "Imported {name}: {count} commits, {signatures} invalidated signatures removed."
        )?;
    }
    writeln!(
        ui.status(),
        "Native states imported in one transaction; working copy unchanged. Compose source workspace bookmarks with jjosh new."
    )?;
    Ok(())
}
