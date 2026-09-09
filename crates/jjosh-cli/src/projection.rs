use std::io::Write as _;

use crate::interop::{commit_as_josh_oid, open_josh_transaction, sha1_git_repo_path};
use jj_cli::cli_util::{CommandHelper, RevisionArg};
use jj_cli::command_error::{CommandError, cli_error, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::object_id::ObjectId as _;
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
    /// Relocate a local change graph while preserving jj identities and conflict trees.
    Transplant(crate::transplant::Args),
    /// Configure projection remotes.
    Remote(RemoteArgs),
    /// Fetch, project, and import remote history into Jujutsu.
    Fetch(FetchArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct StatusArgs {
    /// Josh filter expression, for example `:/services/api`.
    filter: String,

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
    /// Josh filter expression, for example `:/services/api`.
    filter: String,
}

#[derive(clap::Args, Clone, Debug)]
struct FetchArgs {
    /// Projection remote to fetch.
    #[arg(short, long, default_value = "origin")]
    remote: String,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        Command::Status(args) => run_status(ui, command_helper, args).await,
        Command::Transplant(args) => crate::transplant::run(ui, command_helper, args).await,
        Command::Remote(args) => run_remote(ui, command_helper, args).await,
        Command::Fetch(args) => run_fetch(ui, command_helper, args).await,
    }
}

async fn run_status(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: StatusArgs,
) -> Result<(), CommandError> {
    let workspace_command = command_helper.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;

    if commit.id() == workspace_command.repo().store().root_commit_id() {
        return Err(user_error("The root commit cannot be projected"));
    }
    if commit.has_conflict() {
        return Err(user_error(format!(
            "Revision {} has unresolved conflicts and cannot be projected",
            commit.id().hex()
        )));
    }

    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let filter = josh_core::filter::parse(&args.filter)
        .map_err(|err| user_error_with_message("Invalid Josh projection", err))?;
    let source_oid = commit_as_josh_oid(&commit)?;

    // Status is deliberately ephemeral: it proves that the selected jj revision can be read and
    // transformed by Josh, but does not publish the resulting objects or update either VCS's refs.
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
    writeln!(ui.stdout(), "Change ID: {}", commit.change_id())?;
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
    let workspace_command = command_helper.workspace_helper(ui).await?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let _git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;

    let filter = josh_core::filter::parse(&args.filter)
        .map_err(|err| user_error_with_message("Invalid Josh projection", err))?;
    let repo = transaction.repo();
    let local_repo = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let local_url = local_repo.to_string_lossy().into_owned();
    let remote_url_key = format!("remote.{}.url", args.name);
    let remote_push_url_key = format!("remote.{}.pushurl", args.name);
    let remote_fetch_key = format!("remote.{}.fetch", args.name);
    let remote_uploadpack_key = format!("remote.{}.uploadpack", args.name);
    let remote_receivepack_key = format!("remote.{}.receivepack", args.name);
    let projected_refspec = format!("+refs/heads/*:refs/remotes/{}/*", args.name);
    let backing_refspec = format!("+refs/heads/*:refs/josh/remotes/{}/*", args.name);
    let uploadpack = format!("env GIT_NAMESPACE=josh-{} git upload-pack", args.name);

    transaction
        .spawn_git(&["config", &remote_url_key, &local_url], &[])
        .map_err(|err| user_error_with_message("Failed to configure projection remote URL", err))?;
    transaction
        .spawn_git(
            &["config", "--replace-all", &remote_push_url_key, &local_url],
            &[],
        )
        .map_err(|err| {
            user_error_with_message("Failed to isolate projection remote pushes", err)
        })?;
    transaction
        .spawn_git(
            &[
                "config",
                "--replace-all",
                &remote_fetch_key,
                &projected_refspec,
            ],
            &[],
        )
        .map_err(|err| {
            user_error_with_message("Failed to configure projection remote refspec", err)
        })?;
    transaction
        .spawn_git(&["config", &remote_uploadpack_key, &uploadpack], &[])
        .map_err(|err| {
            user_error_with_message("Failed to configure projection remote namespace", err)
        })?;
    transaction
        .spawn_git(&["config", &remote_receivepack_key, "false"], &[])
        .map_err(|err| {
            user_error_with_message("Failed to disable direct projection remote pushes", err)
        })?;

    let repo_path = josh_core::git::normalize_repo_path(&git_repo_path);
    josh_cli::config::write_remote_config(
        &repo_path,
        &args.name,
        &args.url,
        &args.filter,
        &backing_refspec,
        None,
        None,
        None,
    )
    .map_err(|err| user_error_with_message("Failed to write Josh remote configuration", err))?;

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
    if command_helper.global_args().no_integrate_operation {
        return Err(cli_error("--no-integrate-operation is not respected"));
    }

    let mut workspace_command = command_helper.workspace_helper(ui).await?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let mut tx = workspace_command.start_transaction();
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let fetch_args = josh_cli::commands::fetch::FetchArgs {
        remote: args.remote.clone(),
        rref: "HEAD".to_owned(),
    };
    let updates = josh_cli::commands::fetch::handle_fetch(&fetch_args, &transaction, false)
        .map_err(|err| user_error_with_message("Failed to fetch the Josh projection", err))?;

    let git_settings = jj_lib::git::GitSettings::from_settings(tx.settings())?;
    let remote_settings = tx.settings().remote_settings()?;
    let import_options =
        jj_cli::git_util::load_git_import_options(ui, &git_settings, &remote_settings)?;
    let import_stats = jj_lib::git::import_refs(tx.repo_mut(), &import_options).await?;
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
