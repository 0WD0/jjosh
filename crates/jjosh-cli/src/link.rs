use std::collections::{HashMap, HashSet};
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
use jj_lib::rewrite::{MoveCommitsTarget, find_duplicate_divergent_commits};

use crate::interop::{
    commit_as_josh_oid, commit_id_from_josh_oid, open_josh_transaction, sha1_git_repo_path,
    tree_from_josh_oid,
};

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: LinkCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum LinkCommand {
    /// Compose a linked repository at a path.
    ///
    /// Embedded source history contains only the mounted path. The clean
    /// composition is marked by trunk@jjosh; local patches stay above it.
    /// Source bookmarks use <branch>@link-<encoded-path>. Their synthetic
    /// remotes reject direct Git transport; use link update and link push.
    Add(AddArgs),
    /// Fetch and materialize newer commits for one or all links.
    ///
    /// Advances the clean trunk and rebases local patches without freezing them.
    /// Successfully fetched source branches also refresh their raw publication
    /// leases, but never leases for a different remote URL or branch.
    /// Legacy Embed graphs require explicit migration before they can be updated.
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
    /// Revision selecting links to update; an older baseline uses the current trunk.
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
        LinkCommand::Add(args) => {
            run_add(ui, command_helper, args).await?;
            crate::link_refs::install_config(ui, command_helper).await
        }
        LinkCommand::Update(args) => {
            run_update(ui, command_helper, args).await?;
            crate::link_refs::install_config(ui, command_helper).await
        }
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

const NATIVE_BASELINE: &str = "jjosh native link baseline";

fn pinned_commit(
    path: &Path,
    link: josh_core::filter::Filter,
) -> Result<gix_hash::ObjectId, CommandError> {
    link.get_meta("commit")
        .ok_or_else(|| {
            user_error(format!(
                "Josh link at '{}' has no pinned commit",
                path.display()
            ))
        })?
        .parse()
        .map_err(|err| user_error_with_message("Invalid pinned Josh link commit", err))
}

fn scaffold_filter(links: &[(PathBuf, josh_core::filter::Filter)]) -> josh_core::filter::Filter {
    links
        .iter()
        .fold(josh_core::filter::Filter::new(), |filter, (path, _)| {
            filter.exclude(josh_core::filter::Filter::new().subdir(path).prefix(path))
        })
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

fn clean_composition(
    transaction: &josh_core::cache::Transaction,
    scaffold_source: gix_hash::ObjectId,
    links: &[(PathBuf, josh_core::filter::Filter)],
) -> Result<(gix_hash::ObjectId, Vec<gix_hash::ObjectId>), CommandError> {
    let scaffold = josh_core::filter_commit(transaction, scaffold_filter(links), scaffold_source)
        .map_err(|err| {
        user_error_with_message("Failed to isolate the root scaffold history", err)
    })?;
    let mut tree = if scaffold.is_null() {
        gix_hash::ObjectId::empty_tree(gix_hash::Kind::Sha1)
    } else {
        josh_core::git::read_tree_id(transaction.odb(), scaffold)
            .map_err(|err| user_error_with_message("Failed to read the root scaffold", err))?
    };
    let mut parents = vec![scaffold];
    for (path, link) in links {
        // Peel includes the mount prefix, but never the surrounding composite tree.
        let source =
            josh_core::filter_commit(transaction, link.peel(), pinned_commit(path, *link)?)
                .map_err(|err| {
                    user_error_with_message("Failed to project linked source history", err)
                })?;
        if !source.is_null() {
            let source_tree = josh_core::git::read_tree_id(transaction.odb(), source)
                .map_err(|err| user_error_with_message("Failed to read linked source tree", err))?;
            tree = josh_core::filter::tree::overlay(transaction, tree, source_tree).map_err(
                |err| user_error_with_message("Failed to compose linked source tree", err),
            )?;
        }
        if link.get_meta("mode").as_deref() == Some("embedded") {
            parents.push(source);
        }
    }
    Ok((write_link_metadata(transaction, tree, links)?, parents))
}

async fn existing_baseline(
    workspace_command: &WorkspaceCommandHelper,
    transaction: &josh_core::cache::Transaction,
    commit: &Commit,
    has_links: bool,
) -> Result<Option<Commit>, CommandError> {
    let repo = workspace_command.repo();
    if let Some(id) = crate::link_refs::trunk_id(repo.as_ref()) {
        if !repo.index().is_ancestor(&id, commit.id()).await?
            && !repo.index().is_ancestor(commit.id(), &id).await?
        {
            return Err(user_error(
                "The selected revision is not based on trunk@jjosh",
            ));
        }
        return Ok(Some(repo.store().get_commit_async(&id).await?));
    }
    if !has_links {
        return Ok(None);
    }
    // Recover missing native refs only from our explicitly identified, verifiably
    // clean graph. Legacy Embed histories require migration, even on a no-op.
    let mut cursor = commit.clone();
    loop {
        if cursor
            .description()
            .lines()
            .any(|line| line == NATIVE_BASELINE)
        {
            let mut pending = vec![cursor.clone()];
            let mut visited = HashSet::new();
            let mut valid = true;
            while let Some(candidate) = pending.pop() {
                if !visited.insert(candidate.id().clone()) {
                    continue;
                }
                let oid = commit_as_josh_oid(&candidate)?;
                let tree = josh_core::git::read_tree_id(transaction.odb(), oid).map_err(|err| {
                    user_error_with_message("Failed to inspect native baseline", err)
                })?;
                let links =
                    josh_core::link::find_link_files(transaction.odb(), tree).map_err(|err| {
                        user_error_with_message("Failed to inspect native links", err)
                    })?;
                let (clean_tree, expected_parents) = clean_composition(transaction, oid, &links)?;
                valid &= clean_tree == tree;
                for parent in candidate.parent_ids() {
                    let parent_oid = gix_hash::ObjectId::try_from(parent.as_bytes())
                        .map_err(|err| user_error_with_message("Invalid native parent ID", err))?;
                    if expected_parents
                        .iter()
                        .skip(1)
                        .any(|expected| expected == &parent_oid)
                        || parent_oid.is_null()
                        || josh_core::filter_commit(
                            transaction,
                            scaffold_filter(&links),
                            parent_oid,
                        )
                        .map_err(|err| {
                            user_error_with_message("Failed to verify scaffold ancestry", err)
                        })? == parent_oid
                    {
                        continue;
                    }
                    let parent = repo.store().get_commit_async(parent).await?;
                    if parent
                        .description()
                        .lines()
                        .any(|line| line == NATIVE_BASELINE)
                    {
                        pending.push(parent);
                    } else {
                        valid = false;
                    }
                }
                for expected in expected_parents.iter().skip(1) {
                    valid &= repo
                        .index()
                        .is_ancestor(&commit_id_from_josh_oid(*expected), candidate.id())
                        .await?;
                }
            }
            if valid {
                return Ok(Some(cursor));
            }
            break;
        }
        let Some(parent) = cursor.parent_ids().first() else {
            break;
        };
        if parent == repo.store().root_commit_id() {
            break;
        }
        cursor = repo.store().get_commit_async(parent).await?;
    }
    Err(user_error(
        "Existing Josh links have no verified native baseline; explicit legacy link migration is required",
    ))
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

fn source_bookmarks(
    transaction: &josh_core::cache::Transaction,
    links: &[(PathBuf, josh_core::filter::Filter)],
    repo_path: &Path,
) -> Result<Vec<crate::link_refs::SourceBookmark>, CommandError> {
    links
        .iter()
        .map(|(path, link)| {
            let target = link.get_meta("target").unwrap_or_else(|| "HEAD".to_owned());
            let remote = link
                .get_meta("remote")
                .ok_or_else(|| user_error("Josh link has no remote"))?;
            let branch = if let Some(branch) = link.get_meta("source-branch") {
                branch
            } else if target.starts_with("refs/") && !target.starts_with("refs/heads/")
                || target.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                "pinned".to_owned()
            } else {
                destination_ref(&target, None, &remote, repo_path)?
                    .trim_start_matches("refs/heads/")
                    .to_owned()
            };
            let source =
                josh_core::filter_commit(transaction, link.peel(), pinned_commit(path, *link)?)
                    .map_err(|err| {
                        user_error_with_message("Failed to project source bookmark", err)
                    })?;
            Ok(crate::link_refs::SourceBookmark {
                path: path.clone(),
                branch,
                commit: source,
            })
        })
        .collect()
}

fn explicit_change_ids(
    transaction: &josh_core::cache::Transaction,
    tip: gix_hash::ObjectId,
) -> Result<HashMap<Vec<u8>, gix_hash::ObjectId>, CommandError> {
    let mut change_ids = HashMap::new();
    let mut pending = vec![tip];
    let mut visited = HashSet::new();
    while let Some(commit) = pending.pop() {
        if commit.is_null() || !visited.insert(commit) {
            continue;
        }
        let data =
            josh_core::objects::CommitData::read(transaction.odb(), commit).map_err(|err| {
                user_error_with_message("Failed to inspect embedded commit metadata", err)
            })?;
        let parsed = data.parsed().map_err(|err| {
            user_error_with_message("Failed to parse embedded commit metadata", err)
        })?;
        if let Some(value) = parsed.extra_headers().find("change-id") {
            let value: &[u8] = value.as_ref();
            change_ids.entry(value.to_vec()).or_insert(commit);
        }
        pending.extend(
            josh_core::git::read_parent_ids(transaction.odb(), commit).map_err(|err| {
                user_error_with_message("Failed to walk embedded commit history", err)
            })?,
        );
    }
    Ok(change_ids)
}

fn find_explicit_change_id_collision(
    transaction: &josh_core::cache::Transaction,
    old_tip: gix_hash::ObjectId,
    new_tip: gix_hash::ObjectId,
) -> Result<Option<(Vec<u8>, gix_hash::ObjectId, gix_hash::ObjectId)>, CommandError> {
    let old_change_ids = explicit_change_ids(transaction, old_tip)?;
    let new_change_ids = explicit_change_ids(transaction, new_tip)?;
    Ok(new_change_ids
        .into_iter()
        .find_map(|(change_id, new_commit)| {
            old_change_ids
                .get(&change_id)
                .filter(|old_commit| **old_commit != new_commit)
                .map(|old_commit| (change_id, *old_commit, new_commit))
        }))
}
async fn import_link_parents(
    repo: &mut jj_lib::repo::MutableRepo,
    parent_oids: Vec<gix_hash::ObjectId>,
) -> Result<Vec<jj_lib::backend::CommitId>, CommandError> {
    let mut parents = Vec::with_capacity(parent_oids.len());
    for oid in parent_oids {
        let id = commit_id_from_josh_oid(oid);
        if parents.iter().any(|parent: &Commit| parent.id() == &id) {
            continue;
        }
        parents.push(repo.store().get_commit_async(&id).await?);
    }
    repo.index_commits(&parents).await?;

    let mut simplified = Vec::with_capacity(parents.len());
    for (index, parent) in parents.iter().enumerate() {
        let mut is_ancestor = false;
        for (other_index, other) in parents.iter().enumerate() {
            if index != other_index && repo.index().is_ancestor(parent.id(), other.id()).await? {
                is_ancestor = true;
                break;
            }
        }
        if !is_ancestor {
            simplified.push(parent.id().clone());
        }
    }
    if simplified.is_empty() {
        return Err(user_error("The embedded link result has no usable parents"));
    }
    Ok(simplified)
}

async fn insert_link_commit(
    ui: &mut Ui,
    workspace_command: &mut WorkspaceCommandHelper,
    rebase_base: &Commit,
    composition_tree: MergedTree,
    overlay: Option<(MergedTree, String)>,
    parent_oids: Vec<gix_hash::ObjectId>,
    sources: &[crate::link_refs::SourceBookmark],
    transaction: &josh_core::cache::Transaction,
    git_lock: &jj_cli::cli_util::GitImportExportLock,
    commit_description: String,
    operation_description: String,
) -> Result<(), CommandError> {
    let children = format!("children({})", rebase_base.id().hex());
    let child_ids: Vec<_> = workspace_command
        .resolve_revsets_ordered(ui, &[RevisionArg::from(children.clone())])
        .await?
        .into_iter()
        .collect();
    let descendants = workspace_command
        .resolve_revsets_ordered(ui, &[RevisionArg::from(format!("descendants({children})"))])
        .await?;
    // The clean baseline is not rewritten. Only actual local rebase targets
    // need to be mutable, including descendants beyond the immediate children.
    workspace_command
        .check_rewritable(descendants.iter())
        .await?;
    let was_working_copy = workspace_command.get_wc_commit_id() == Some(rebase_base.id());
    let overlay = overlay.filter(|(tree, _)| tree.tree_ids() != composition_tree.tree_ids());
    transaction.flush_mem_odb().map_err(|err| {
        user_error_with_message("Failed to persist objects produced by Josh", err)
    })?;
    let mut tx = workspace_command.start_transaction();
    let parent_ids = import_link_parents(tx.repo_mut(), parent_oids).await?;
    let link_commit = tx
        .repo_mut()
        .new_commit(parent_ids, composition_tree)
        .set_description(format!("{commit_description}\n\n{NATIVE_BASELINE}"))
        .write()
        .await?;
    let insertion_tip = if let Some((tree, description)) = overlay {
        tx.repo_mut()
            .new_commit(vec![link_commit.id().clone()], tree)
            .set_description(description)
            .write()
            .await?
    } else {
        link_commit.clone()
    };
    // A published local patch can return through its source branch. Use jj's
    // patch-equivalence check to absorb only changes fully present upstream;
    // keep residual changes and conflicts rather than silently dropping them.
    let duplicates: HashSet<_> = find_duplicate_divergent_commits(
        tx.repo(),
        std::slice::from_ref(insertion_tip.id()),
        &MoveCommitsTarget::Roots(child_ids.clone()),
    )
    .await?
    .into_iter()
    .map(|commit| commit.id().clone())
    .collect();
    let mut num_rebased = 0;
    tx.repo_mut()
        .transform_descendants(child_ids, async |mut rewriter| {
            rewriter.replace_parent(rebase_base.id(), [insertion_tip.id()]);
            if duplicates.contains(rewriter.old_commit().id()) {
                rewriter.abandon();
            } else {
                rewriter.rebase().await?.write().await?;
                num_rebased += 1;
            }
            Ok(())
        })
        .await?;
    if was_working_copy {
        if insertion_tip.id() == link_commit.id() {
            let working_copy = tx
                .repo_mut()
                .new_commit(vec![link_commit.id().clone()], link_commit.tree())
                .write()
                .await?;
            tx.edit(&working_copy)?;
        } else {
            tx.edit(&insertion_tip)?;
        }
    }
    crate::link_refs::publish(tx.repo_mut(), transaction, sources, link_commit.id()).await?;
    writeln!(
        ui.status(),
        "Created link composition revision {}",
        link_commit.id().hex()
    )?;
    if insertion_tip.id() != link_commit.id() {
        writeln!(
            ui.status(),
            "Preserved local link contents in revision {}",
            insertion_tip.id().hex()
        )?;
    }
    if num_rebased > 0 {
        writeln!(ui.status(), "Rebased {num_rebased} descendant commits.")?;
    }
    tx.finish_with_git_import_export_lock(ui, operation_description, git_lock)
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
    let path = normalized_link_path(&args.path)?;
    let mode = josh_core::filter::LinkMode::parse(&args.mode)
        .map_err(|err| user_error_with_message("Invalid Josh link mode", err))?;
    if mode == josh_core::filter::LinkMode::Pointer {
        return Err(user_error(
            "Native jjosh links support embedded and snapshot modes, not pointer mode",
        ));
    }
    let embedded = mode == josh_core::filter::LinkMode::Embedded;
    if args.push_target.is_some() && args.push_url.is_none() {
        return Err(user_error("--push-target requires --push-url"));
    }
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let source_commit = commit_as_josh_oid(&commit)?;
    let source_tree = josh_core::git::read_tree_id(transaction.odb(), source_commit)
        .map_err(|err| user_error_with_message("Failed to read the jj revision tree", err))?;
    let existing_links = josh_core::link::find_link_files(transaction.odb(), source_tree)
        .map_err(|err| user_error_with_message("Failed to inspect existing links", err))?;
    let baseline = existing_baseline(
        &workspace_command,
        &transaction,
        &commit,
        !existing_links.is_empty(),
    )
    .await?;
    let rebase_base = baseline.as_ref().unwrap_or(&commit);
    let base_oid = commit_as_josh_oid(rebase_base)?;
    let base_tree = josh_core::git::read_tree_id(transaction.odb(), base_oid)
        .map_err(|err| user_error_with_message("Failed to read the clean baseline tree", err))?;
    let filter = args.filter.as_deref().unwrap_or(":/");
    let existing_source = josh_link::export_link_source(&transaction, source_commit, &path, filter)
        .map_err(|err| user_error_with_message("Failed to export existing link contents", err))?;
    let fetched = embedded || existing_source.is_none();
    let fetch_url = args.fetch_url.as_deref().unwrap_or(&args.url);
    let fetch_target = args.at.as_deref().unwrap_or(&args.target);
    let linked_commit = if fetched {
        transaction
            .spawn_git(&["fetch", fetch_url, fetch_target], &[])
            .map_err(|err| user_error_with_message("Failed to fetch linked repository", err))?;
        josh_core::git::resolve_fetch_head(&transaction).map_err(|err| {
            user_error_with_message("Failed to resolve the fetched link target", err)
        })?
    } else {
        existing_source.expect("snapshot has existing contents")
    };
    let source_branch = if fetched {
        fetched_source_branch(fetch_target, fetch_url, &git_repo_path)?
    } else {
        "pinned".to_owned()
    };
    let prepared = josh_link::prepare_link_add(
        &transaction,
        &path,
        &args.url,
        args.push_url.as_deref(),
        args.filter.as_deref(),
        &args.target,
        args.push_target.as_deref(),
        linked_commit,
        base_tree,
        mode,
    )
    .map_err(|err| user_error_with_message("Failed to prepare the Josh link", err))?;
    let mut links = josh_core::link::find_link_files(transaction.odb(), prepared.tree_oid())
        .map_err(|err| user_error_with_message("Failed to inspect prepared links", err))?;
    for (link_path, link) in &mut links {
        if link_path == &path {
            *link = link.with_meta("source-branch", source_branch.clone());
        }
    }
    let (tree_oid, mut parents) = clean_composition(&transaction, base_oid, &links)?;
    if baseline.is_some() {
        parents[0] = base_oid;
    }
    let sources = source_bookmarks(&transaction, &links, &git_repo_path)?;
    let existing_link = josh_core::link::find_link_files(transaction.odb(), base_tree)
        .map_err(|err| user_error_with_message("Failed to inspect the baseline mount", err))?
        .iter()
        .any(|(existing_path, _)| existing_path == &path);
    // A newly mounted directory may already have scaffold content. Preserve it
    // above the clean baseline; subsequent local commits are rebased, not squashed.
    let overlay_trees = if embedded && !existing_link {
        let empty_mount_tree = josh_core::filter::tree::insert_oid(
            transaction.odb(),
            base_tree,
            &path,
            gix_hash::ObjectId::empty_tree(gix_hash::Kind::Sha1),
            0o40000,
        )
        .map_err(|err| user_error_with_message("Failed to isolate the new mount", err))?;
        Some((
            write_link_metadata(&transaction, empty_mount_tree, &links)?,
            write_link_metadata(&transaction, base_tree, &links)?,
        ))
    } else {
        None
    };
    transaction
        .flush_mem_odb()
        .map_err(|err| user_error_with_message("Failed to persist link trees", err))?;
    let store = workspace_command.repo().store().clone();
    let linked_tree = tree_from_josh_oid(store.clone(), tree_oid);
    let overlay = if let Some((base_tree, local_tree)) = overlay_trees {
        let merged = MergedTree::merge(Merge::from_vec(vec![
            (linked_tree.clone(), "linked repository".to_owned()),
            (
                tree_from_josh_oid(store.clone(), base_tree),
                "empty link mount".to_owned(),
            ),
            (
                tree_from_josh_oid(store, local_tree),
                "pre-existing local contents".to_owned(),
            ),
        ]))
        .await?;
        Some((
            merged,
            format!("Preserve local contents at {}", path.display()),
        ))
    } else {
        None
    };
    let mode_name = if embedded { "embedded" } else { "snapshot" };
    insert_link_commit(
        ui,
        &mut workspace_command,
        rebase_base,
        linked_tree,
        overlay,
        parents,
        &sources,
        &transaction,
        &git_lock,
        format!("Add {mode_name} Josh link {}", path.display()),
        format!("add {mode_name} Josh link {}", path.display()),
    )
    .await?;
    if fetched && args.fetch_url.is_none() && args.at.is_none() && source_branch != "pinned" {
        crate::link_refs::record_observation(
            &transaction,
            &args.url,
            &format!("refs/heads/{source_branch}"),
            linked_commit,
        )?;
    }
    Ok(())
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
    let selected_path = args.path.as_deref().map(normalized_link_path).transpose()?;
    let git_repo_path = sha1_git_repo_path(&workspace_command)?;
    let git_lock = workspace_command.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_repo_path, false)?;
    let source_commit = commit_as_josh_oid(&commit)?;
    let source_tree = josh_core::git::read_tree_id(transaction.odb(), source_commit)
        .map_err(|err| user_error_with_message("Failed to read the jj revision tree", err))?;
    let link_files = josh_core::link::find_link_files(transaction.odb(), source_tree)
        .map_err(|err| user_error_with_message("Failed to find Josh links", err))?;
    let baseline = existing_baseline(
        &workspace_command,
        &transaction,
        &commit,
        !link_files.is_empty(),
    )
    .await?
    .ok_or_else(|| user_error("No Josh links found in the selected revision"))?;
    let baseline_oid = commit_as_josh_oid(&baseline)?;
    let baseline_tree = josh_core::git::read_tree_id(transaction.odb(), baseline_oid)
        .map_err(|err| user_error_with_message("Failed to read the native baseline", err))?;
    let mut baseline_links = josh_core::link::find_link_files(transaction.odb(), baseline_tree)
        .map_err(|err| user_error_with_message("Failed to inspect baseline links", err))?;
    let selected_links: Vec<_> = link_files
        .iter()
        .cloned()
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
    let mut observations = Vec::with_capacity(selected_links.len());
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
        let (_, previous_link) = baseline_links
            .iter()
            .find(|(base_path, _)| base_path == path)
            .ok_or_else(|| {
                user_error("Selected link is absent from the current native baseline")
            })?;
        let old_commit = pinned_commit(path, *previous_link)?;
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
        let source_branch = fetched_source_branch(&target, &remote, &git_repo_path)?;
        if source_branch != "pinned" {
            observations.push((remote, format!("refs/heads/{source_branch}"), new_commit));
        }
        let embedded = link_file
            .get_meta("mode")
            .is_some_and(|mode| mode == "embedded");
        if embedded && old_commit != new_commit {
            let raw_fast_forward =
                josh_core::filter::is_ancestor_of(&transaction, old_commit, new_commit).map_err(
                    |err| {
                        user_error_with_message(
                            format!(
                                "Failed to compare Josh link history at '{}'",
                                path.display()
                            ),
                            err,
                        )
                    },
                )?;
            if !raw_fast_forward {
                return Err(user_error(format!(
                    "Josh link source at '{}' did not advance by fast-forward ({} -> {}); refusing to retain unrelated histories in the composite DAG",
                    path.display(),
                    old_commit,
                    new_commit
                )));
            }

            let source_filter = link_file.peel();
            let old_filtered = josh_core::filter_commit(&transaction, source_filter, old_commit)
                .map_err(|err| {
                    user_error_with_message(
                        format!(
                            "Failed to filter the old link history at '{}'",
                            path.display()
                        ),
                        err,
                    )
                })?;
            let new_filtered = josh_core::filter_commit(&transaction, source_filter, new_commit)
                .map_err(|err| {
                    user_error_with_message(
                        format!(
                            "Failed to filter the new link history at '{}'",
                            path.display()
                        ),
                        err,
                    )
                })?;
            let filtered_fast_forward =
                josh_core::filter::is_ancestor_of(&transaction, old_filtered, new_filtered)
                    .map_err(|err| {
                        user_error_with_message(
                            format!(
                                "Failed to compare filtered Josh link history at '{}'",
                                path.display()
                            ),
                            err,
                        )
                    })?;
            if !filtered_fast_forward {
                return Err(user_error(format!(
                    "Josh link source at '{}' rewrites its filtered history ({} -> {}); update rejected to avoid divergent jj Change IDs",
                    path.display(),
                    old_filtered,
                    new_filtered
                )));
            }
            if let Some((change_id, old_change, new_change)) =
                find_explicit_change_id_collision(&transaction, old_filtered, new_filtered)?
            {
                return Err(user_error(format!(
                    "Josh link source at '{}' rewrites commits carrying change-id '{}' ({} -> {}); update rejected to avoid divergent jj changes",
                    path.display(),
                    String::from_utf8_lossy(&change_id),
                    old_change,
                    new_change
                )));
            }
        }
        links_to_update.push((path.clone(), new_commit, source_branch));
    }

    for (path, new_commit, source_branch) in links_to_update {
        let (_, link) = baseline_links.iter_mut().find(|(base_path, _)| base_path == &path)
            .ok_or_else(|| user_error("Local link metadata is not present in the native baseline; explicit migration is required"))?;
        *link = link
            .with_meta("commit", new_commit.to_string())
            .with_meta("source-branch", source_branch);
    }
    let (tree_oid, mut parents) = clean_composition(&transaction, baseline_oid, &baseline_links)?;
    parents[0] = baseline_oid;
    let sources = source_bookmarks(&transaction, &baseline_links, &git_repo_path)?;
    if tree_oid == baseline_tree {
        transaction
            .flush_mem_odb()
            .map_err(|err| user_error_with_message("Failed to persist source markers", err))?;
        let mut tx = workspace_command.start_transaction();
        crate::link_refs::publish(tx.repo_mut(), &transaction, &sources, baseline.id()).await?;
        tx.finish_with_git_import_export_lock(ui, "restore native link bookmarks", &git_lock)
            .await?;
        writeln!(ui.status(), "Selected Josh links are already up to date")?;
    } else {
        let linked_tree = tree_from_josh_oid(workspace_command.repo().store().clone(), tree_oid);
        insert_link_commit(
            ui,
            &mut workspace_command,
            &baseline,
            linked_tree,
            None,
            parents,
            &sources,
            &transaction,
            &git_lock,
            format!("Update {} Josh link(s)", selected_links.len()),
            format!("update {} Josh link(s)", selected_links.len()),
        )
        .await?;
    }
    for (remote, destination, raw_commit) in observations {
        crate::link_refs::record_observation(&transaction, &remote, &destination, raw_commit)?;
    }
    Ok(())
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
    let prepared = josh_link::prepare_link_push(&transaction, source_commit, &path)
        .map_err(|err| user_error_with_message("Failed to export the Josh link", err))?;
    let push_remote = prepared.push_remote.as_deref().ok_or_else(|| {
        user_error(format!(
            "Josh link at '{}' has no push remote; add push metadata with --push-url before publishing",
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
            "Link push preflight succeeded for {} to {}:{}\nExported commit: {}\nRemote updated: no",
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
