use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::interop::{
    check_projectable_history, check_projectable_remote, commit_as_josh_oid, open_josh_transaction,
    sha1_git_repo_path,
};
use jj_cli::cli_util::{CommandHelper, RevisionArg, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::repo::Repo as _;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum Command {
    /// Preview a projected commit without changing refs or the working copy.
    Status(StatusArgs),
    /// Configure projection remotes.
    Remote(RemoteArgs),
    /// Fetch, project, and import remote history into Jujutsu.
    Fetch(FetchArgs),
    /// Reverse-project a selected Jujutsu revision and push it to a source branch.
    Push(PushArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct FilterArgs {
    /// Josh filter expression, for example `:/services/api`.
    #[arg(required_unless_present = "view", conflicts_with = "view")]
    filter: Option<String>,

    /// Versioned projection view at this repository-relative path.
    ///
    /// Reads Josh's workspace.josh from each historical version of the path.
    /// This selects a history/layout view, not a Jujutsu working copy.
    #[arg(long, value_name = "REPO_PATH")]
    view: Option<String>,
}

impl FilterArgs {
    fn resolve(&self) -> Result<josh_core::filter::Filter, CommandError> {
        if let Some(value) = &self.view {
            if value.is_empty() || value.contains('\0') {
                return Err(user_error("View paths must not be empty or contain NUL"));
            }
            let mut path = PathBuf::new();
            for component in Path::new(value).components() {
                match component {
                    Component::Normal(name) => path.push(name),
                    Component::CurDir => {}
                    _ => {
                        return Err(user_error(
                            "View path must be repository-relative and cannot contain ..",
                        ));
                    }
                }
            }
            Ok(josh_core::filter::Filter::new().workspace(path))
        } else {
            josh_core::filter::parse(
                self.filter
                    .as_deref()
                    .expect("clap requires a filter or view"),
            )
            .map_err(|err| user_error_with_message("Invalid Josh projection", err))
        }
    }
}

#[derive(clap::Args, Clone, Debug)]
struct StatusArgs {
    #[command(flatten)]
    projection: FilterArgs,

    /// Revision to project.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
}

#[derive(clap::Args, Clone, Debug)]
struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum RemoteCommand {
    /// Add or replace a projection remote.
    Add(RemoteAddArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct RemoteAddArgs {
    /// Remote name exposed to Jujutsu, such as `origin`.
    name: String,
    /// Upstream Git URL containing the unprojected history.
    url: String,
    #[command(flatten)]
    projection: FilterArgs,
}

#[derive(clap::Args, Clone, Debug)]
struct FetchArgs {
    /// Projection remote to fetch.
    #[arg(short, long, default_value = "origin")]
    remote: String,
}

#[derive(clap::Args, Clone, Debug)]
struct PushArgs {
    /// Projection remote to push through.
    #[arg(long, default_value = "origin")]
    remote: String,

    /// Destination branch in the unprojected source repository.
    #[arg(long, required = true)]
    to: String,

    /// Exact revision or bookmark to publish; use @ for the current working commit.
    #[arg(short = 'r', long, required = true)]
    revision: RevisionArg,

    /// Source branch supplying context for a new branch or unrelated-history import.
    #[arg(long)]
    base: Option<String>,

    /// Merge the reverse-projected history with the source base.
    #[arg(long)]
    merge: bool,

    /// Prepare the push without updating the remote.
    #[arg(long)]
    dry_run: bool,

    /// Allow a non-fast-forward source branch update.
    #[arg(long)]
    force: bool,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Status(args) => run_status(ui, command_helper, args).await,
        Command::Remote(args) => run_remote(ui, command_helper, args).await,
        Command::Fetch(args) => run_fetch(ui, command_helper, args).await,
        Command::Push(args) => run_push(ui, command_helper, args).await,
    }
}

fn require_current_operation(command_helper: &CommandHelper) -> Result<(), CommandError> {
    if !command_helper.is_at_head_operation() || command_helper.global_args().no_integrate_operation
    {
        return Err(user_error(
            "Projection remote changes, fetch, and push require the current integrated operation",
        ));
    }
    Ok(())
}

async fn recorded_workspace(
    ui: &Ui,
    command_helper: &CommandHelper,
) -> Result<WorkspaceCommandHelper, CommandError> {
    // The ordinary no-snapshot helper can still merge divergent operation heads.
    // Preview/configuration need the recorded view, not a working-copy mutation.
    let workspace = command_helper.load_workspace()?;
    let loader = workspace.repo_loader();
    let operation = if let Some(op) = &command_helper.global_args().at_operation {
        jj_lib::op_walk::resolve_op_for_load(loader, op).await?
    } else {
        let heads = jj_lib::op_walk::get_current_head_ops(
            loader.op_store(),
            loader.op_heads_store().as_ref(),
        )
        .await?;
        let [operation] = heads.as_slice() else {
            return Err(user_error(
                "Projection requires one recorded operation head; reconcile operations separately or select --at-operation for status",
            ));
        };
        operation.clone()
    };
    let repo = loader.load_at(&operation).await?;
    command_helper.for_workable_repo(ui, workspace, repo)
}

async fn run_status(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: StatusArgs,
) -> Result<(), CommandError> {
    let workspace_command = recorded_workspace(ui, command_helper).await?;
    jj_lib::git::get_git_backend(workspace_command.repo().store())?.disable_lazy_commit_imports();
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;

    check_projectable_history(&workspace_command, &commit).await?;

    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let filter = args.projection.resolve()?;
    let source_oid = commit_as_josh_oid(&commit)?;

    // Filtering transforms history; the displayed change ID belongs to the source,
    // not a claim that the derived graph preserves every source commit.
    let transaction = open_josh_transaction(&git_repo_path, true)?;
    let projected_oid = josh_core::filter_commit(&transaction, filter, source_oid)
        .map_err(|err| user_error_with_message("Failed to apply the Josh projection", err))?;

    writeln!(
        ui.stdout(),
        "Projection: {}",
        josh_core::filter::spec(filter)
    )?;
    writeln!(ui.stdout(), "Source commit: {source_oid}")?;
    writeln!(ui.stdout(), "Projected commit: {projected_oid}")?;
    writeln!(ui.stdout(), "Source change ID: {}", commit.change_id())?;
    writeln!(ui.stdout(), "Objects persisted: no")?;
    Ok(())
}

async fn run_remote(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: RemoteArgs,
) -> Result<(), CommandError> {
    match args.command {
        RemoteCommand::Add(args) => run_remote_add(ui, command_helper, args).await,
    }
}

async fn run_remote_add(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: RemoteAddArgs,
) -> Result<(), CommandError> {
    require_current_operation(command_helper)?;
    let workspace_command = recorded_workspace(ui, command_helper).await?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let _git_lock = workspace_command.lock_git_import_export()?;

    let filter = args.projection.resolve()?;
    josh_cli::remote_ops::configure_remote(
        &git_repo_path,
        &args.name,
        &args.url,
        &josh_core::filter::spec(filter),
        None,
        None,
        None,
    )
    .map_err(|err| user_error_with_message("Failed to configure Josh projection remote", err))?;

    writeln!(
        ui.status(),
        "Configured projection remote {}: {} through {}",
        args.name,
        args.url,
        josh_core::filter::spec(filter)
    )?;
    Ok(())
}

async fn run_fetch(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: FetchArgs,
) -> Result<(), CommandError> {
    require_current_operation(command_helper)?;

    // Synchronize native/Git working-copy state before the eventual transaction
    // finish can update the colocated HEAD/index or rebase affected descendants.
    let mut workspace_command = command_helper.workspace_helper(ui).await?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let fetch_args = josh_cli::commands::fetch::FetchArgs {
        remote: args.remote.clone(),
        rref: "HEAD".to_owned(),
    };
    let fetched = josh_cli::commands::fetch::fetch_unfiltered(&fetch_args, &transaction, false)
        .map_err(|err| user_error_with_message("Failed to fetch the Josh source history", err))?;
    check_projectable_remote(&transaction, &args.remote)?;
    let updates = josh_cli::commands::fetch::filter_fetched(&fetched, &transaction)
        .map_err(|err| user_error_with_message("Failed to filter the Josh source history", err))?;

    let mut tx = workspace_command.start_transaction();
    let git_settings = jj_lib::git::GitSettings::from_settings(tx.settings())?;
    let remote_settings = tx.settings().remote_settings()?;
    let import_options =
        jj_cli::git_util::load_git_import_options(ui, &git_settings, &remote_settings)?;
    let import_stats =
        jj_lib::git::import_some_refs(tx.repo_mut(), &import_options, |kind, symbol| {
            kind == jj_lib::git::GitRefKind::Bookmark && symbol.remote.as_str() == args.remote
        })
        .await?;
    jj_cli::git_util::print_git_import_stats(ui, &tx, &import_stats)?;
    let description = format!("fetch Josh projection from {}", args.remote);
    tx.finish_with_git_import_export_lock(ui, description, &git_lock)
        .await?;
    writeln!(
        ui.status(),
        "Fetched {} projected ref update(s) from {}",
        updates.len(),
        args.remote
    )?;
    Ok(())
}

fn branch_ref(
    transaction: &josh_core::cache::Transaction,
    branch: &str,
) -> Result<String, CommandError> {
    if branch.starts_with('-') {
        return Err(user_error("A source branch name cannot start with '-'"));
    }
    let reference = format!("refs/heads/{branch}");
    // A qualified ref cannot be parsed as an option, and check-ref-format rejects
    // revision expressions, refspec delimiters, and invalid path components.
    transaction
        .spawn_git(&["check-ref-format", &reference], &[])
        .map_err(|err| user_error_with_message(format!("Invalid source branch {branch:?}"), err))?;
    Ok(reference)
}

async fn run_push(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: PushArgs,
) -> Result<(), CommandError> {
    require_current_operation(command_helper)?;
    let workspace_command = command_helper.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    check_projectable_history(&workspace_command, &commit).await?;

    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let _git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    check_projectable_remote(&transaction, &args.remote)?;
    let destination = branch_ref(&transaction, &args.to)?;
    if let Some(base) = &args.base {
        branch_ref(&transaction, base)?;
    }
    let source_oid = commit_as_josh_oid(&commit)?;
    let push_args = josh_cli::commands::push::PushArgs {
        remote: Some(args.remote),
        refspecs: vec![format!("{source_oid}:{destination}")],
        force: args.force,
        atomic: false,
        dry_run: args.dry_run,
        base: args.base,
        merge: args.merge,
    };
    // Josh owns source-aware reverse filtering and push status. No Git HEAD,
    // temporary ref, local revision rewrite, or post-push fetch is involved.
    josh_cli::commands::push::handle_push(&push_args, &transaction)
        .map_err(|err| user_error_with_message("Failed to push the Josh projection", err))
}
