use std::io::Write as _;
use std::path::{Path, PathBuf};

use jj_cli::cli_util::{CommandHelper, RevisionArg, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::commit::Commit;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;

use crate::interop::{
    commit_as_josh_oid, open_josh_transaction, sha1_git_repo_path, tree_from_josh_oid,
};

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: LinkCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum LinkCommand {
    /// Mount a linked repository snapshot at a path.
    Add(AddArgs),
    /// Fetch and materialize newer commits for one or all links.
    Update(UpdateArgs),
    /// Export one linked path and push it back to its remote.
    Push(PushArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct AddArgs {
    /// Path where the linked repository is mounted.
    path: String,
    /// Linked repository URL.
    url: String,
    /// Josh filter applied before mounting the linked repository.
    filter: Option<String>,
    /// Branch updated by `link update` and used by `link push`.
    #[arg(long, default_value = "HEAD")]
    target: String,
    /// Alternate URL used only to seed the initial snapshot.
    #[arg(long)]
    fetch_url: Option<String>,
    /// Alternate branch, tag, or commit used only to seed the initial snapshot.
    #[arg(long)]
    at: Option<String>,
    /// Link history mode. Native jjosh links currently require `snapshot`.
    #[arg(long, default_value = "snapshot")]
    mode: String,
    /// Jujutsu revision whose tree should receive the link.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
}

#[derive(clap::Args, Clone, Debug)]
struct UpdateArgs {
    /// Linked path to update. Omit to update every link in the revision.
    path: Option<String>,
    /// Jujutsu revision containing the link metadata.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
}

#[derive(clap::Args, Clone, Debug)]
struct PushArgs {
    /// Linked path to export and push.
    path: String,
    /// Jujutsu revision whose linked contents should be exported.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
    /// Destination branch. Required when the link target is not a branch.
    #[arg(long)]
    to: Option<String>,
    /// Allow a non-fast-forward update of the linked repository.
    #[arg(long, short)]
    force: bool,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        LinkCommand::Add(args) => run_add(ui, command_helper, args).await,
        LinkCommand::Update(args) => run_update(ui, command_helper, args).await,
        LinkCommand::Push(args) => run_push(ui, command_helper, args).await,
    }
}

fn normalized_link_path(path: &str) -> Result<PathBuf, CommandError> {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        return Err(user_error("Link path cannot be empty"));
    }
    let path = PathBuf::from(normalized);
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(user_error("Link path must stay within the repository"));
    }
    Ok(path)
}

fn check_link_commit(
    workspace_command: &WorkspaceCommandHelper,
    commit: &Commit,
) -> Result<(), CommandError> {
    if commit.id() == workspace_command.repo().store().root_commit_id() {
        return Err(user_error("The root commit cannot contain links"));
    }
    if commit.has_conflict() {
        return Err(user_error(format!(
            "Revision {} has unresolved conflicts and cannot be used for a link operation",
            commit.id().hex()
        )));
    }
    Ok(())
}

async fn rewrite_link_tree(
    ui: &mut Ui,
    workspace_command: &mut WorkspaceCommandHelper,
    commit: &Commit,
    tree: MergedTree,
    git_lock: &jj_cli::cli_util::GitImportExportLock,
    description: String,
) -> Result<(), CommandError> {
    let mut tx = workspace_command.start_transaction();
    let rewritten = tx
        .repo_mut()
        .rewrite_commit(commit)
        .set_tree(tree)
        .write()
        .await?;
    writeln!(
        ui.status(),
        "Updated link tree in revision {}",
        rewritten.id().hex()
    )?;
    tx.finish_with_git_import_export_lock(ui, description, git_lock)
        .await
}

async fn run_add(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: AddArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command_helper.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    check_link_commit(&workspace_command, &commit)?;
    workspace_command.check_rewritable([commit.id()]).await?;
    let path = normalized_link_path(&args.path)?;
    let mode = josh_core::filter::LinkMode::parse(&args.mode)
        .map_err(|err| user_error_with_message("Invalid Josh link mode", err))?;
    if mode != josh_core::filter::LinkMode::Snapshot {
        return Err(user_error(
            "Native jjosh links currently support only snapshot mode",
        ));
    }

    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let source_commit = commit_as_josh_oid(&commit)?;
    let filter = args.filter.as_deref().unwrap_or(":/");
    let existing_source = josh_link::export_link_source(&transaction, source_commit, &path, filter)
        .map_err(|err| user_error_with_message("Failed to export existing link contents", err))?;
    let linked_commit = if let Some(existing_source) = existing_source {
        existing_source
    } else {
        let fetch_url = args.fetch_url.as_deref().unwrap_or(&args.url);
        let fetch_target = args.at.as_deref().unwrap_or(&args.target);
        transaction
            .spawn_git(&["fetch", fetch_url, fetch_target], &[])
            .map_err(|err| user_error_with_message("Failed to fetch linked repository", err))?;
        josh_core::git::resolve_fetch_head(&transaction).map_err(|err| {
            user_error_with_message("Failed to resolve the fetched link target", err)
        })?
    };

    let source_tree = josh_core::git::read_tree_id(transaction.odb(), source_commit)
        .map_err(|err| user_error_with_message("Failed to read the jj revision tree", err))?;
    let prepared = josh_link::prepare_link_add(
        &transaction,
        &path,
        &args.url,
        args.filter.as_deref(),
        &args.target,
        linked_commit,
        source_tree,
        mode,
    )
    .map_err(|err| user_error_with_message("Failed to prepare the Josh link", err))?;

    let linked_tree = if existing_source.is_some() {
        prepared.into_tree_oid()
    } else {
        let signature = josh_link::make_signature(&transaction)
            .map_err(|err| user_error_with_message("Failed to create link signature", err))?;
        let marker_commit = prepared
            .into_commit(&transaction, source_commit, &signature)
            .map_err(|err| user_error_with_message("Failed to create link metadata commit", err))?;
        let link_filter = josh_core::filter::parse(":link")
            .map_err(|err| user_error_with_message("Failed to parse the Josh link filter", err))?;
        let materialized_commit =
            josh_core::filter_commit(&transaction, link_filter, marker_commit).map_err(|err| {
                user_error_with_message("Failed to materialize the linked repository", err)
            })?;
        josh_core::git::read_tree_id(transaction.odb(), materialized_commit).map_err(|err| {
            user_error_with_message("Failed to read the materialized link tree", err)
        })?
    };

    transaction.flush_mem_odb().map_err(|err| {
        user_error_with_message("Failed to persist objects produced by Josh", err)
    })?;
    let linked_tree = tree_from_josh_oid(workspace_command.repo().store().clone(), linked_tree);
    rewrite_link_tree(
        ui,
        &mut workspace_command,
        &commit,
        linked_tree,
        &git_lock,
        format!("add Josh link {}", path.display()),
    )
    .await
}

async fn run_update(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: UpdateArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command_helper.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    check_link_commit(&workspace_command, &commit)?;
    workspace_command.check_rewritable([commit.id()]).await?;
    let selected_path = args.path.as_deref().map(normalized_link_path).transpose()?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let source_commit = commit_as_josh_oid(&commit)?;
    let source_tree = josh_core::git::read_tree_id(transaction.odb(), source_commit)
        .map_err(|err| user_error_with_message("Failed to read the jj revision tree", err))?;
    let link_files = josh_core::link::find_link_files(transaction.odb(), source_tree)
        .map_err(|err| user_error_with_message("Failed to find Josh links", err))?;
    let selected_links: Vec<_> = link_files
        .into_iter()
        .filter(|(path, _)| {
            selected_path
                .as_ref()
                .is_none_or(|selected| path == selected)
        })
        .collect();
    if selected_links.is_empty() {
        return Err(match selected_path {
            Some(path) => user_error(format!("No Josh link found at '{}'", path.display())),
            None => user_error("No Josh links found in the selected revision"),
        });
    }

    let mut links_to_update = Vec::with_capacity(selected_links.len());
    for (path, link_file) in &selected_links {
        let remote = link_file.get_meta("remote").ok_or_else(|| {
            user_error(format!(
                "Josh link at '{}' has no remote metadata",
                path.display()
            ))
        })?;
        let target = link_file
            .get_meta("target")
            .unwrap_or_else(|| "HEAD".to_owned());
        transaction
            .spawn_git(&["fetch", &remote, &target], &[])
            .map_err(|err| {
                user_error_with_message(
                    format!("Failed to fetch Josh link '{}'", path.display()),
                    err,
                )
            })?;
        let new_commit = josh_core::git::resolve_fetch_head(&transaction).map_err(|err| {
            user_error_with_message(
                format!("Failed to resolve Josh link '{}'", path.display()),
                err,
            )
        })?;
        links_to_update.push((path.clone(), new_commit));
    }

    let signature = josh_link::make_signature(&transaction)
        .map_err(|err| user_error_with_message("Failed to create link signature", err))?;
    let Some(result) = josh_link::update_materialized_links(
        &transaction,
        source_commit,
        links_to_update,
        &signature,
    )
    .map_err(|err| user_error_with_message("Failed to update Josh links", err))?
    else {
        writeln!(ui.status(), "Selected Josh links are already up to date")?;
        return Ok(());
    };
    let previous_tree_oid =
        josh_core::git::read_tree_id(transaction.odb(), result.previous_materialized_commit)
            .map_err(|err| user_error_with_message("Failed to read the previous link tree", err))?;
    let linked_tree_oid =
        josh_core::git::read_tree_id(transaction.odb(), result.update.filtered_commit)
            .map_err(|err| user_error_with_message("Failed to read the updated link tree", err))?;
    transaction.flush_mem_odb().map_err(|err| {
        user_error_with_message("Failed to persist objects produced by Josh", err)
    })?;
    let store = workspace_command.repo().store().clone();
    let previous_tree = tree_from_josh_oid(store.clone(), previous_tree_oid);
    let linked_tree = tree_from_josh_oid(store, linked_tree_oid);
    let merged_tree = MergedTree::merge(Merge::from_vec(vec![
        (linked_tree, "updated linked snapshot".to_owned()),
        (previous_tree, "previous linked snapshot".to_owned()),
        (commit.tree(), "local link changes".to_owned()),
    ]))
    .await?;

    rewrite_link_tree(
        ui,
        &mut workspace_command,
        &commit,
        merged_tree,
        &git_lock,
        format!("update {} Josh link(s)", selected_links.len()),
    )
    .await
}

fn destination_ref(
    configured_target: &str,
    override_target: Option<&str>,
    remote: &str,
    repo_path: &Path,
) -> Result<String, CommandError> {
    let target = if let Some(target) = override_target {
        target.to_owned()
    } else if configured_target == "HEAD" {
        josh_cli::remote_ops::get_head_branch(remote, repo_path, "link").map_err(|err| {
            user_error_with_message("Failed to resolve the linked remote's default branch", err)
        })?
    } else {
        configured_target.to_owned()
    };
    if target.starts_with("refs/heads/") {
        Ok(target)
    } else if target.starts_with("refs/") || target.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Err(user_error(format!(
            "Link target '{target}' is not a branch; specify --to <branch>"
        )))
    } else {
        Ok(format!("refs/heads/{target}"))
    }
}

async fn run_push(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: PushArgs,
) -> Result<(), CommandError> {
    let workspace_command = command_helper.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    check_link_commit(&workspace_command, &commit)?;
    let path = normalized_link_path(&args.path)?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let _git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let source_commit = commit_as_josh_oid(&commit)?;
    let prepared = josh_link::prepare_link_push(&transaction, source_commit, &path)
        .map_err(|err| user_error_with_message("Failed to export the Josh link", err))?;
    let normalized_repo_path = josh_core::git::normalize_repo_path(&git_repo_path);
    let destination = destination_ref(
        &prepared.configured_target,
        args.to.as_deref(),
        &prepared.remote,
        &normalized_repo_path,
    )?;
    let refspec = format!(
        "{}{}:{}",
        if args.force { "+" } else { "" },
        prepared.exported_commit,
        destination
    );
    transaction
        .spawn_git(&["push", &prepared.remote, &refspec], &[])
        .map_err(|err| user_error_with_message("Failed to push the Josh link", err))?;
    writeln!(
        ui.status(),
        "Pushed link {} to {}:{}",
        path.display(),
        prepared.remote,
        destination
    )?;
    Ok(())
}
