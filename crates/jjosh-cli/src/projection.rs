use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::interop::{
    check_projectable_repo_history, commit_as_josh_oid, open_josh_transaction,
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
    /// Attach a named Git remote to a project without changing recorded history.
    Attach(RemoteAttachArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct RemoteAddArgs {
    /// Remote name exposed to Jujutsu, such as `origin`.
    name: String,
    /// Upstream Git URL containing the unprojected history.
    url: String,
    #[command(flatten)]
    projection: FilterArgs,
    /// Optional publication endpoint; otherwise use the source endpoint.
    #[arg(long)]
    push_url: Option<String>,
    /// Mount the projected history as this project.
    #[arg(long)]
    project: Option<String>,
    /// Project mount; defaults to its recorded native mount or project name.
    #[arg(long, requires = "project")]
    mount: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
struct RemoteAttachArgs {
    name: String,
    project: String,
    #[arg(long)]
    mount: Option<String>,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Status(args) => run_status(ui, command_helper, args).await,
        Command::Remote(args) => run_remote(ui, command_helper, args).await,
    }
}

fn require_current_operation(command_helper: &CommandHelper) -> Result<(), CommandError> {
    if !command_helper.is_at_head_operation() || command_helper.global_args().no_integrate_operation
    {
        return Err(user_error(
            "Projection configuration requires the current integrated operation",
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

    check_projectable_repo_history(workspace_command.repo().as_ref(), &commit).await?;

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
        RemoteCommand::Attach(args) => run_remote_attach(ui, command_helper, args).await,
    }
}

async fn run_remote_add(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: RemoteAddArgs,
) -> Result<(), CommandError> {
    require_current_operation(command_helper)?;
    let workspace = recorded_workspace(ui, command_helper).await?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let _git_lock = workspace.lock_git_import_export()?;
    let git = gix::open(&git_path).map_err(user_error)?;
    let project = args.project.or(
        crate::git_remote::config_string(&git, &format!("remote.{}.jjosh-project", args.name))
            .map_err(user_error)?,
    );
    let mut filter = args.projection.resolve()?;
    let attachment = if let Some(project) = project {
        let project = crate::native_project::parse_project(&project).map_err(user_error)?;
        let mount = args.mount.or(
            crate::git_remote::config_string(&git, &format!("remote.{}.jjosh-mount", args.name))
                .map_err(user_error)?,
        );
        let mount = attachment_mount(&git_path, &project, mount.as_deref())?;
        filter = filter.prefix(mount.as_internal_file_string());
        Some((project, mount))
    } else {
        None
    };
    josh_cli::remote_ops::configure_remote(
        &git_path,
        &args.name,
        &args.url,
        &josh_core::filter::spec(filter),
        None,
        args.push_url.as_deref(),
        None,
    )
    .map_err(|err| user_error_with_message("Failed to configure Josh projection remote", err))?;
    if let Some((project, mount)) = attachment {
        let read_only = args.push_url.is_none()
            && git.config_snapshot()
                .boolean(format!("remote.{}.jjosh-readOnly", args.name).as_str())
                .unwrap_or(false);
        crate::git_remote::configure_attachment(&git_path, &args.name, &project, &mount, read_only)
            .map_err(user_error)?;
    }
    writeln!(
        ui.status(),
        "Configured projection remote {}: {} through {}",
        args.name,
        args.url,
        josh_core::filter::spec(filter),
    )?;
    Ok(())
}

fn attachment_mount(
    git_path: &Path,
    project: &str,
    mount: Option<&str>,
) -> Result<jj_lib::repo_path::RepoPathBuf, CommandError> {
    if let Some(mount) = mount {
        crate::native_project::parse_mount(mount).map_err(user_error)
    } else {
        let transaction = open_josh_transaction(git_path, true)?;
        crate::native_project::load_mount(&transaction, project).map_err(user_error)
    }
}

async fn run_remote_attach(
    ui: &mut Ui,
    command: &CommandHelper,
    args: RemoteAttachArgs,
) -> Result<(), CommandError> {
    require_current_operation(command)?;
    let workspace = recorded_workspace(ui, command).await?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let _git_lock = workspace.lock_git_import_export()?;
    let project = crate::native_project::parse_project(&args.project).map_err(user_error)?;
    let mount = attachment_mount(&git_path, &project, args.mount.as_deref())?;
    let git = gix::open(&git_path).map_err(user_error)?;
    git.find_remote(args.name.as_str()).map_err(user_error)?;
    let existing = crate::git_remote::config_string(&git, &format!("remote.{}.jjosh-project", args.name))
        .map_err(user_error)?;
    if existing.is_none()
        && let Some(config) = josh_changes::remote_config::try_read_remote_config(&git_path, &args.name)
            .map_err(user_error)?
    {
        let filter = config.semantic_filter().prefix(mount.as_internal_file_string());
        josh_cli::remote_ops::configure_remote(
            &git_path, &args.name, &config.url, &josh_core::filter::spec(filter),
            config.forge, config.push_url.as_deref(), Some(config.gerrit_mode),
        ).map_err(user_error)?;
    }
    let read_only = git.config_snapshot()
        .boolean(format!("remote.{}.jjosh-readOnly", args.name).as_str())
        .unwrap_or(false);
    crate::git_remote::configure_attachment(&git_path, &args.name, &project, &mount, read_only)
        .map_err(user_error)?;
    writeln!(ui.status(), "Attached {} to {project} at {}", args.name, mount.as_internal_file_string())?;
    Ok(())
}

