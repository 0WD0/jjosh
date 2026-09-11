use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use jj_cli::cli_util::CommandHelper;
use jj_cli::cli_util::RevisionArg;
use jj_cli::cli_util::WorkspaceCommandHelper;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;
use jj_cli::ui::Ui;
use jj_lib::commit::Commit;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;

use crate::interop::commit_as_josh_oid;
use crate::interop::commit_id_from_josh_oid;
use crate::interop::open_josh_transaction;
use crate::interop::sha1_git_repo_path;
use crate::interop::tree_from_josh_oid;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: LinkCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum LinkCommand {
    /// Add an external project, or associate a source with an imported jj project.
    Add(AddArgs),
    /// Fetch selected source branches and tags into monorepo coordinates.
    ///
    /// Updates jj reference observations without composing a new baseline or
    /// automatically rebasing local changes. Integrate with ordinary jj commands.
    Update(UpdateArgs),
    /// Export a linked path, safely rewriting a previously pushed destination.
    ///
    /// Rewrites are allowed only while the remote matches its last successful
    /// push or explicit source-branch observation (force-with-lease). State is
    /// kept locally per exact remote URL and destination branch.
    ///
    /// Without a recorded position (including old-version publications), only
    /// branch creation or fast-forward updates are allowed. Use --force only
    /// after checking that replacement will not discard remote work.
    /// Dry runs and failed pushes never change the recorded remote position.
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
    /// Alternate URL used only to fetch the initial linked history.
    #[arg(long)]
    fetch_url: Option<String>,
    /// Separate remote URL used only for publication.
    #[arg(long)]
    push_url: Option<String>,
    /// Alternate branch, tag, or commit used only to select the initial linked history.
    #[arg(long)]
    at: Option<String>,
    /// Default publication branch. Defaults to the source target when omitted.
    #[arg(long)]
    push_target: Option<String>,
    /// Name of this source observation, such as upstream or origin.
    #[arg(long = "remote-name", default_value = "upstream")]
    source_remote: String,
    /// Link history mode: `embedded` for development or `snapshot` for vendoring.
    #[arg(long, default_value = "embedded")]
    mode: String,
    /// Jujutsu revision whose tree should receive the link.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
}

#[derive(clap::Args, Clone, Debug)]
struct UpdateArgs {
    /// Linked path to update. Omit to update every link in the revision.
    path: Option<String>,
    /// Revision whose link configuration selects the sources to fetch.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
    /// Branch patterns to fetch (repeatable, using jj string-pattern syntax).
    #[arg(long = "branch", short = 'b', alias = "bookmark")]
    branches: Option<Vec<String>>,
    /// Tag patterns to fetch (repeatable, using jj string-pattern syntax).
    #[arg(long = "tag", short = 't')]
    tags: Option<Vec<String>>,
}

#[derive(clap::Args, Clone, Debug)]
struct PushArgs {
    /// Linked path to export and push.
    path: String,
    /// Jujutsu revision whose linked contents should be exported.
    /// Commits with no exported file changes are pruned regardless of their
    /// description or whether the revision was explicitly selected.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
    /// Destination branch. Required when the link target is not a branch.
    #[arg(long)]
    to: Option<String>,
    /// Overwrite the destination even if it changed since the last observation.
    #[arg(long, short)]
    force: bool,
    /// Validate the inverse export and remote update without changing the remote.
    /// Does not update the remembered remote position.
    #[arg(long)]
    dry_run: bool,
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

fn write_link_metadata(
    transaction: &josh_core::cache::Transaction,
    mut tree: gix_hash::ObjectId,
    links: &[(PathBuf, josh_core::filter::Filter)],
) -> Result<gix_hash::ObjectId, CommandError> {
    for (path, link) in links {
        let content = josh_core::filter::as_file(*link, 0);
        let blob = josh_core::objects::write_blob(transaction.odb(), content.as_bytes())
            .map_err(|err| user_error_with_message("Failed to write Josh link metadata", err))?;
        tree = josh_core::filter::tree::insert_oid(
            transaction.odb(),
            tree,
            &path.join(".link.josh"),
            blob,
            0o100644,
        )
        .map_err(|err| user_error_with_message("Failed to insert Josh link metadata", err))?;
    }
    Ok(tree)
}

fn fetched_source_branch(
    target: &str,
    remote: &str,
    repo_path: &Path,
) -> Result<String, CommandError> {
    let fetch_head = std::fs::read_to_string(repo_path.join("FETCH_HEAD"))
        .map_err(|err| user_error_with_message("Failed to read fetched source identity", err))?;
    if fetch_head
        .lines()
        .filter_map(|line| line.splitn(3, '\t').nth(2))
        .any(|description| description.starts_with("tag '"))
        || target.starts_with("refs/") && !target.starts_with("refs/heads/")
        || target.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Ok("pinned".to_owned());
    }
    Ok(destination_ref(target, None, remote, repo_path)?
        .trim_start_matches("refs/heads/")
        .to_owned())
}

async fn run_add(ui: &mut Ui, command: &CommandHelper, args: AddArgs) -> Result<(), CommandError> {
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error(
            "Link add requires the current integrated operation",
        ));
    }
    let mut workspace = command.workspace_helper(ui).await?;
    let commit = workspace.resolve_single_rev(ui, &args.revision).await?;
    check_link_commit(&workspace, &commit)?;
    let path = normalized_link_path(&args.path)?;
    let project = path.to_str().unwrap();
    crate::native_project::validate_project(&args.source_remote).map_err(user_error)?;
    let mode = crate::link_metadata::LinkMode::parse(&args.mode).map_err(user_error)?;
    if args.push_target.is_some() && args.push_url.is_none() {
        return Err(user_error("--push-target requires --push-url"));
    }
    let git_path = sha1_git_repo_path(&workspace)?;
    let git_lock = workspace.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_path, false)?;
    let source_tree = josh_core::git::read_tree_id(transaction.odb(), commit_as_josh_oid(&commit)?)
        .map_err(user_error)?;
    let existing_links = crate::link_metadata::find_link_files(transaction.odb(), source_tree)
        .map_err(user_error)?;
    let native = crate::link_fetch::has_native_history(&transaction, project)?;
    let filter =
        josh_core::filter::parse(args.filter.as_deref().unwrap_or(":/")).map_err(user_error)?;
    if native
        && (filter != josh_core::filter::Filter::new()
            || mode != crate::link_metadata::LinkMode::Embedded)
    {
        return Err(user_error(
            "Attach imported projects with their existing whole-repository layout",
        ));
    }
    let url = jj_cli::git_util::absolute_git_url(command.cwd(), &args.url)?;
    let fetch_url = jj_cli::git_util::absolute_git_url(
        command.cwd(),
        args.fetch_url.as_deref().unwrap_or(&args.url),
    )?;
    let fetch_target = args.at.as_deref().unwrap_or(&args.target);
    transaction
        .spawn_git(
            &[
                "fetch",
                "--no-tags",
                "--refmap=",
                "--",
                &fetch_url,
                fetch_target,
            ],
            &[],
        )
        .map_err(user_error)?;
    let raw = josh_core::git::resolve_fetch_head(&transaction).map_err(user_error)?;
    let branch = fetched_source_branch(fetch_target, &fetch_url, &git_path)?;
    let mut pin = raw;
    if native {
        let known =
            crate::native_project::anchors(workspace.repo().as_ref(), &transaction, project)
                .await
                .map_err(user_error)?;
        let mut pending = vec![raw];
        let mut seen = HashSet::new();
        let mut found = None;
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if known.contains_key(&commit_id_from_josh_oid(id)) {
                found = Some(id);
                break;
            }
            pending.extend(
                josh_core::git::read_parent_ids(transaction.odb(), id).map_err(user_error)?,
            );
        }
        pin = found.ok_or_else(|| {
            user_error("The selected source has no known history in the imported project")
        })?;
    }
    let push_url = args
        .push_url
        .as_deref()
        .map(|url| jj_cli::git_util::absolute_git_url(command.cwd(), url))
        .transpose()?;
    let prepared = crate::link_metadata::prepare_link_add(
        &transaction,
        &path,
        &url,
        push_url.as_deref(),
        args.filter.as_deref(),
        &args.target,
        args.push_target.as_deref(),
        pin,
        source_tree,
        mode.clone(),
    )
    .map_err(user_error)?;
    let mut links =
        crate::link_metadata::find_link_files(transaction.odb(), prepared).map_err(user_error)?;
    let (_, link) = links
        .iter_mut()
        .find(|(candidate, _)| candidate == &path)
        .unwrap();
    *link = link
        .with_meta("source-branch", branch.clone())
        .with_meta("source-remote", args.source_remote.clone());
    let link = *link;
    let metadata_tree = write_link_metadata(&transaction, prepared, &links)?;
    let was_working_copy = workspace.get_wc_commit_id() == Some(commit.id());
    let mut tx = workspace.start_transaction();
    let mut parents = vec![commit.id().clone()];
    let tree = if native
        || existing_links
            .iter()
            .any(|(candidate, _)| candidate == &path)
    {
        tree_from_josh_oid(tx.repo().store().clone(), metadata_tree)
    } else {
        let projected =
            josh_core::filter_commit(&transaction, link.peel(), raw).map_err(user_error)?;
        transaction.flush_mem_odb().map_err(user_error)?;
        if projected.is_null() {
            tree_from_josh_oid(tx.repo().store().clone(), metadata_tree)
        } else {
            let source_id = commit_id_from_josh_oid(projected);
            let source = tx.repo().store().get_commit_async(&source_id).await?;
            tx.repo_mut().add_head(&source).await?;
            if mode == crate::link_metadata::LinkMode::Embedded {
                parents.push(source_id.clone());
            }
            let remote = crate::link_fetch::remote_name(&path, &args.source_remote)?;
            if branch != "pinned" {
                let scope = crate::link_refs::source_name(&path)?.replace("%2F", "/");
                let suffix = crate::ref_names::use_scope_suffix(tx.settings())
                    .map_err(user_error)?;
                let name: jj_lib::ref_name::RefNameBuf =
                    crate::ref_names::local_name(&scope, &branch, suffix).into();
                tx.repo_mut().set_remote_bookmark(
                    name.to_remote_symbol(&remote),
                    jj_lib::op_store::RemoteRef {
                        target: jj_lib::op_store::RefTarget::normal(source_id),
                        state: jj_lib::op_store::RemoteRefState::New,
                    },
                );
                let stats = jj_lib::git::export_some_refs(tx.repo_mut(), |_, symbol| {
                    symbol.remote == remote
                })?;
                jj_cli::git_util::print_git_export_stats(ui, &stats)?;
            }
            MergedTree::merge(Merge::from_vec(vec![
                (
                    tree_from_josh_oid(tx.repo().store().clone(), metadata_tree),
                    "existing monorepo".to_owned(),
                ),
                (
                    tx.repo()
                        .store()
                        .get_commit_async(tx.repo().store().root_commit_id())
                        .await?
                        .tree(),
                    "empty mount".to_owned(),
                ),
                (source.tree(), "linked source".to_owned()),
            ]))
            .await?
        }
    };
    transaction.flush_mem_odb().map_err(user_error)?;
    let added = tx
        .repo_mut()
        .new_commit(parents, tree)
        .set_description(format!("Associate Josh link {}", path.display()))
        .write()
        .await?;
    if was_working_copy {
        tx.edit(&added)?;
    }
    tx.finish_with_git_import_export_lock(
        ui,
        format!("add linked source {}", path.display()),
        &git_lock,
    )
    .await?;
    if args.fetch_url.is_none() && args.at.is_none() && branch != "pinned" {
        crate::link_refs::record_observation(
            &transaction,
            &url,
            &format!("refs/heads/{branch}"),
            raw,
        )?;
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    Ok(())
}

async fn run_update(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: UpdateArgs,
) -> Result<(), CommandError> {
    if !command_helper.is_at_head_operation() || command_helper.global_args().no_integrate_operation
    {
        return Err(user_error(
            "Link fetch requires the current integrated operation",
        ));
    }
    let mut workspace = command_helper.workspace_helper(ui).await?;
    let commit = workspace.resolve_single_rev(ui, &args.revision).await?;
    let selected = args.path.as_deref().map(normalized_link_path).transpose()?;
    let git_lock = workspace.lock_git_import_export()?;
    let transaction = open_josh_transaction(&sha1_git_repo_path(&workspace)?, false)?;
    let links = crate::link_metadata::find_native_link_files(transaction.odb(), &commit)
        .map_err(user_error)?;
    let links: Vec<_> = links
        .into_iter()
        .filter(|(path, _)| selected.as_ref().is_none_or(|selected| selected == path))
        .collect();
    if links.is_empty() {
        return Err(user_error(
            "No matching Josh links in the selected revision",
        ));
    }
    let mut tx = workspace.start_transaction();
    let settings = jj_lib::git::GitSettings::from_settings(tx.settings())?;
    let remote_settings = tx.settings().remote_settings()?;
    let options = jj_cli::git_util::load_git_import_options(ui, &settings, &remote_settings)?;
    for (path, link) in links {
        crate::link_fetch::fetch(
            ui,
            command_helper,
            tx.repo_mut(),
            &transaction,
            &path,
            link,
            args.branches.as_deref(),
            args.tags.as_deref(),
            &options,
        )
        .await?;
    }
    tx.finish_with_git_import_export_lock(ui, "fetch linked source references", &git_lock)
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
    // Never refresh a lease by querying the remote during push. Only prior
    // successful publication or explicit source observation authorizes rewrites.
    let workspace_command = command_helper.workspace_helper(ui).await?;
    let commit = workspace_command
        .resolve_single_rev(ui, &args.revision)
        .await?;
    check_link_commit(&workspace_command, &commit)?;
    let path = normalized_link_path(&args.path)?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let _git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, args.dry_run)?;
    let source_commit = commit_as_josh_oid(&commit)?;
    let prepared = crate::link_metadata::prepare_link_push(&transaction, source_commit, &path)
        .map_err(|err| user_error_with_message("Failed to export the Josh link", err))?;
    let push_remote = prepared.push_remote.as_deref().ok_or_else(|| {
        user_error(format!(
            "Josh link at '{}' has no push remote; add push metadata with --push-url before \
             publishing",
            path.display()
        ))
    })?;
    let configured_destination = prepared
        .configured_push_target
        .as_deref()
        .unwrap_or(&prepared.configured_target);
    let normalized_repo_path = josh_core::git::normalize_repo_path(&git_repo_path);
    let destination = destination_ref(
        configured_destination,
        args.to.as_deref(),
        push_remote,
        &normalized_repo_path,
    )?;
    let tracking_ref =
        crate::link_refs::push_tracking_ref(&transaction, push_remote, &destination)?;
    let last_pushed = transaction.resolve_ref(&tracking_ref).map_err(|err| {
        user_error_with_message("Failed to read the last successful link push", err)
    })?;
    let refspec = format!(
        "{}{}:{}",
        if args.force { "+" } else { "" },
        prepared.exported_commit,
        destination
    );
    let lease = last_pushed
        .filter(|_| !args.force)
        .map(|expected| format!("--force-with-lease={destination}:{expected}"));
    let mut push_args = vec!["push"];
    if let Some(lease) = &lease {
        push_args.push(lease);
    }
    if args.dry_run {
        push_args.push("--dry-run");
    }
    push_args.extend(["--", push_remote, &refspec]);
    let failure_context = if args.dry_run {
        "Failed to preflight the Josh link push"
    } else {
        "Failed to push the Josh link"
    };
    transaction
        .spawn_git(&push_args, &[])
        .map_err(|err| user_error_with_message(failure_context, err))?;
    if args.dry_run {
        writeln!(
            ui.status(),
            "Link push preflight succeeded for {} to {}:{}\nExported commit: {}\nRemote updated: \
             no",
            path.display(),
            push_remote,
            destination,
            prepared.exported_commit
        )?;
    } else {
        transaction
            .update_ref(
                &tracking_ref,
                last_pushed.map_or(
                    josh_core::cache::Expected::Absent,
                    josh_core::cache::Expected::At,
                ),
                prepared.exported_commit,
                "jjosh link push",
            )
            .and_then(|()| transaction.flush_mem_odb())
            .map_err(|err| {
                user_error_with_message(
                    "Link was pushed, but its new remote position could not be saved",
                    err,
                )
            })?;
        writeln!(
            ui.status(),
            "Pushed link {} to {}:{}",
            path.display(),
            push_remote,
            destination
        )?;
    }
    Ok(())
}
