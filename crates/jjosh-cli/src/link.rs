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
use jj_lib::rewrite::rebase_commit;

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
    Add(AddArgs),
    /// Fetch and materialize newer commits for one or all links.
    Update(UpdateArgs),
    /// Export a linked path, safely rewriting a previously pushed destination.
    ///
    /// After a successful push, history rewrites are allowed only while the remote
    /// still matches that push (force-with-lease). State is kept locally for each
    /// remote URL and destination branch, independently of jj's rewritten history.
    ///
    /// Without a recorded push (including pushes made by older jjosh versions),
    /// only branch creation or fast-forward updates are allowed. Use --force only
    /// after checking that replacing the destination will not discard remote work.
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
    /// Overwrite the destination even if it changed since the last successful push.
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

fn embedded_parent_oids(
    transaction: &josh_core::cache::Transaction,
    filtered_commit: gix_hash::ObjectId,
) -> Result<Vec<gix_hash::ObjectId>, CommandError> {
    let filtered_parents = josh_core::git::read_parent_ids(transaction.odb(), filtered_commit)
        .map_err(|err| user_error_with_message("Failed to read the embedded link parents", err))?;
    let (_, new_link_parents) = filtered_parents
        .split_first()
        .ok_or_else(|| user_error("The embedded link result has no aggregate-history parent"))?;
    if new_link_parents.is_empty() {
        return Err(user_error(
            "The embedded link result did not include linked history",
        ));
    }
    Ok(new_link_parents.to_vec())
}

fn changed_embedded_parent(
    transaction: &josh_core::cache::Transaction,
    composition_commit: gix_hash::ObjectId,
    path: &Path,
    pinned_commit: gix_hash::ObjectId,
) -> Result<Option<gix_hash::ObjectId>, CommandError> {
    let parents =
        josh_core::git::read_parent_ids(transaction.odb(), composition_commit).map_err(|err| {
            user_error_with_message("Failed to read the link composition parents", err)
        })?;
    let Some(first_parent) = parents.first() else {
        return Ok(None);
    };
    let extra_parents = &parents[1..];
    if extra_parents.is_empty() {
        return Ok(None);
    }
    let tree = josh_core::git::read_tree_id(transaction.odb(), composition_commit)
        .map_err(|err| user_error_with_message("Failed to read the link composition tree", err))?;
    let first_parent_tree = josh_core::git::read_tree_id(transaction.odb(), *first_parent)
        .map_err(|err| {
            user_error_with_message("Failed to read the previous link composition tree", err)
        })?;
    let links = josh_core::link::find_link_files(transaction.odb(), tree)
        .map_err(|err| user_error_with_message("Failed to inspect composed Josh links", err))?;
    let previous_links = josh_core::link::find_link_files(transaction.odb(), first_parent_tree)
        .map_err(|err| {
            user_error_with_message("Failed to inspect previous composed Josh links", err)
        })?;
    let changed_links = links
        .iter()
        .filter(|(current_path, link_file)| {
            let embedded = link_file
                .get_meta("mode")
                .is_some_and(|mode| mode == "embedded");
            let previous = previous_links
                .iter()
                .find(|(previous_path, _)| previous_path == current_path);
            embedded
                && previous.is_none_or(|(_, previous_link)| {
                    previous_link.get_meta("mode") != link_file.get_meta("mode")
                        || previous_link.get_meta("commit") != link_file.get_meta("commit")
                })
        })
        .collect::<Vec<_>>();
    if changed_links.len() != extra_parents.len() {
        return Ok(None);
    }
    let pinned_commit = pinned_commit.to_string();
    Ok(changed_links.into_iter().zip(extra_parents).find_map(
        |((current_path, link_file), parent)| {
            (current_path == path
                && link_file.get_meta("commit").as_deref() == Some(pinned_commit.as_str()))
            .then_some(*parent)
        },
    ))
}

fn find_embedded_parent(
    transaction: &josh_core::cache::Transaction,
    start_commit: gix_hash::ObjectId,
    path: &Path,
    pinned_commit: gix_hash::ObjectId,
) -> Result<Option<gix_hash::ObjectId>, CommandError> {
    let mut cursor = start_commit;
    loop {
        if let Some(parent) = changed_embedded_parent(transaction, cursor, path, pinned_commit)? {
            return Ok(Some(parent));
        }
        let parents =
            josh_core::git::read_parent_ids(transaction.odb(), cursor).map_err(|err| {
                user_error_with_message("Failed to walk existing link composition history", err)
            })?;
        let Some(first_parent) = parents.first() else {
            return Ok(None);
        };
        cursor = *first_parent;
    }
}

fn explicit_change_ids(
    transaction: &josh_core::cache::Transaction,
    tip: gix_hash::ObjectId,
) -> Result<HashMap<Vec<u8>, gix_hash::ObjectId>, CommandError> {
    let mut change_ids = HashMap::new();
    let mut pending = vec![tip];
    let mut visited = HashSet::new();
    while let Some(commit) = pending.pop() {
        if !visited.insert(commit) {
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
    commit: &Commit,
    composition_tree: MergedTree,
    overlay: Option<(MergedTree, String)>,
    embedded_parents: Option<Vec<gix_hash::ObjectId>>,
    transaction: &josh_core::cache::Transaction,
    git_lock: &jj_cli::cli_util::GitImportExportLock,
    commit_description: String,
    operation_description: String,
) -> Result<(), CommandError> {
    let child_ids: Vec<_> = workspace_command
        .resolve_revsets_ordered(
            ui,
            &[RevisionArg::from(format!(
                "children({})",
                commit.id().hex()
            ))],
        )
        .await?
        .into_iter()
        .collect();
    workspace_command.check_rewritable(child_ids.iter()).await?;
    let was_working_copy = workspace_command.get_wc_commit_id() == Some(commit.id());
    let overlay = overlay.filter(|(tree, _)| tree.tree_ids() != composition_tree.tree_ids());

    transaction.flush_mem_odb().map_err(|err| {
        user_error_with_message("Failed to persist objects produced by Josh", err)
    })?;
    let mut tx = workspace_command.start_transaction();
    let mut parent_ids = vec![commit.id().clone()];
    if let Some(parent_oids) = embedded_parents {
        parent_ids.extend(import_link_parents(tx.repo_mut(), parent_oids).await?);
    }
    let link_commit = tx
        .repo_mut()
        .new_commit(parent_ids, composition_tree)
        .set_description(commit_description)
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

    let mut num_rebased = 0;
    for child_id in child_ids {
        let child = tx.repo().store().get_commit_async(&child_id).await?;
        let new_parents = child
            .parent_ids()
            .iter()
            .map(|parent_id| {
                if parent_id == commit.id() {
                    insertion_tip.id().clone()
                } else {
                    parent_id.clone()
                }
            })
            .collect();
        rebase_commit(tx.repo_mut(), child, new_parents).await?;
        num_rebased += 1;
    }
    num_rebased += tx.repo_mut().rebase_descendants().await?;
    if was_working_copy {
        tx.edit(&insertion_tip)?;
    }

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
    workspace_command.check_rewritable([commit.id()]).await?;
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
    let filter = args.filter.as_deref().unwrap_or(":/");
    let existing_source = josh_link::export_link_source(&transaction, source_commit, &path, filter)
        .map_err(|err| user_error_with_message("Failed to export existing link contents", err))?;
    let had_existing_contents = existing_source.is_some();
    let fetched_source = if embedded || existing_source.is_none() {
        let fetch_url = args.fetch_url.as_deref().unwrap_or(&args.url);
        let fetch_target = args.at.as_deref().unwrap_or(&args.target);
        transaction
            .spawn_git(&["fetch", fetch_url, fetch_target], &[])
            .map_err(|err| user_error_with_message("Failed to fetch linked repository", err))?;
        Some(
            josh_core::git::resolve_fetch_head(&transaction).map_err(|err| {
                user_error_with_message("Failed to resolve the fetched link target", err)
            })?,
        )
    } else {
        None
    };
    let linked_commit = if embedded {
        fetched_source.expect("embedded links are always fetched")
    } else {
        existing_source
            .or(fetched_source)
            .expect("snapshot links have a local or fetched source")
    };

    let source_tree = josh_core::git::read_tree_id(transaction.odb(), source_commit)
        .map_err(|err| user_error_with_message("Failed to read the jj revision tree", err))?;
    let existing_link = if embedded {
        josh_core::link::find_link_files(transaction.odb(), source_tree)
            .map_err(|err| user_error_with_message("Failed to inspect the existing mount", err))?
            .into_iter()
            .any(|(existing_path, _)| existing_path == path)
    } else {
        false
    };
    let previous_materialized_tree_oid = if existing_link {
        let unlink_filter = josh_core::filter::parse(":unlink").map_err(|err| {
            user_error_with_message("Failed to parse the Josh unlink filter", err)
        })?;
        let canonical_commit = josh_core::filter_commit(&transaction, unlink_filter, source_commit)
            .map_err(|err| {
                user_error_with_message("Failed to canonicalize the existing link", err)
            })?;
        let link_filter = josh_core::filter::parse(":link")
            .map_err(|err| user_error_with_message("Failed to parse the Josh link filter", err))?;
        let materialized_commit =
            josh_core::filter_commit(&transaction, link_filter, canonical_commit).map_err(
                |err| {
                    user_error_with_message("Failed to materialize the existing link baseline", err)
                },
            )?;
        Some(
            josh_core::git::read_tree_id(transaction.odb(), materialized_commit).map_err(
                |err| user_error_with_message("Failed to read the existing link baseline", err),
            )?,
        )
    } else {
        None
    };
    let prepared_source_tree = if embedded {
        josh_core::filter::tree::insert_oid(
            transaction.odb(),
            source_tree,
            &path,
            gix_hash::ObjectId::empty_tree(gix_hash::Kind::Sha1),
            0o0040000,
        )
        .map_err(|err| user_error_with_message("Failed to isolate the link mount", err))?
    } else {
        source_tree
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
        prepared_source_tree,
        mode.clone(),
    )
    .map_err(|err| user_error_with_message("Failed to prepare the Josh link", err))?;
    let clean_marker_tree_oid = prepared.tree_oid();
    let local_marker_tree_oid = if embedded && had_existing_contents {
        Some(
            josh_link::prepare_link_add(
                &transaction,
                &path,
                &args.url,
                args.push_url.as_deref(),
                args.filter.as_deref(),
                &args.target,
                args.push_target.as_deref(),
                linked_commit,
                source_tree,
                mode,
            )
            .map_err(|err| {
                user_error_with_message("Failed to preserve the existing link contents", err)
            })?
            .into_tree_oid(),
        )
    } else {
        None
    };

    let (linked_tree_oid, embedded_parents) = if embedded || existing_source.is_none() {
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
        let tree_oid = josh_core::git::read_tree_id(transaction.odb(), materialized_commit)
            .map_err(|err| {
                user_error_with_message("Failed to read the materialized link tree", err)
            })?;
        let parents = embedded
            .then(|| embedded_parent_oids(&transaction, materialized_commit))
            .transpose()?;
        (tree_oid, parents)
    } else {
        (prepared.into_tree_oid(), None)
    };

    let store = workspace_command.repo().store().clone();
    let linked_tree = tree_from_josh_oid(store.clone(), linked_tree_oid);
    let overlay = if let Some(local_marker_tree_oid) = local_marker_tree_oid {
        transaction.flush_mem_odb().map_err(|err| {
            user_error_with_message("Failed to persist the prepared link trees", err)
        })?;
        let (overlay_base_oid, overlay_base_label) =
            if let Some(previous_tree_oid) = previous_materialized_tree_oid {
                (previous_tree_oid, "previous linked snapshot")
            } else {
                (clean_marker_tree_oid, "empty link mount")
            };
        let overlay_base_tree = tree_from_josh_oid(store.clone(), overlay_base_oid);
        let local_marker_tree = tree_from_josh_oid(store, local_marker_tree_oid);
        let merged_tree = MergedTree::merge(Merge::from_vec(vec![
            (linked_tree.clone(), "linked repository".to_owned()),
            (overlay_base_tree, overlay_base_label.to_owned()),
            (local_marker_tree, "pre-existing local contents".to_owned()),
        ]))
        .await?;
        Some((
            merged_tree,
            format!("Preserve local contents at {}", path.display()),
        ))
    } else {
        None
    };
    let mode_name = if embedded { "embedded" } else { "snapshot" };
    insert_link_commit(
        ui,
        &mut workspace_command,
        &commit,
        linked_tree,
        overlay,
        embedded_parents,
        &transaction,
        &git_lock,
        format!("Add {mode_name} Josh link {}", path.display()),
        format!("add {mode_name} Josh link {}", path.display()),
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
    let updates_embedded_history = selected_links.iter().any(|(_, link_file)| {
        link_file
            .get_meta("mode")
            .is_some_and(|mode| mode == "embedded")
    });

    let mut links_to_update = Vec::with_capacity(selected_links.len());
    let mut embedded_history_updates = Vec::new();
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
        let old_commit = link_file
            .get_meta("commit")
            .ok_or_else(|| {
                user_error(format!(
                    "Josh link at '{}' has no pinned commit metadata",
                    path.display()
                ))
            })?
            .parse::<gix_hash::ObjectId>()
            .map_err(|err| {
                user_error_with_message(
                    format!(
                        "Josh link at '{}' has an invalid pinned commit",
                        path.display()
                    ),
                    err,
                )
            })?;
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
            embedded_history_updates.push((path.clone(), old_commit, new_commit));
        }
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
    let embedded_parents = updates_embedded_history
        .then(|| embedded_parent_oids(&transaction, result.update.filtered_commit))
        .transpose()?;
    if embedded_parents.is_some() {
        for (path, old_commit, new_commit) in &embedded_history_updates {
            let old_parent = find_embedded_parent(&transaction, source_commit, path, *old_commit)?
                .ok_or_else(|| {
                    user_error(format!(
                        "Could not find the existing embedded history parent for '{}'",
                        path.display()
                    ))
                })?;
            let new_parent = changed_embedded_parent(
                &transaction,
                result.update.filtered_commit,
                path,
                *new_commit,
            )?
            .ok_or_else(|| {
                user_error(format!(
                    "Could not find the updated embedded history parent for '{}'",
                    path.display()
                ))
            })?;
            let materialized_fast_forward =
                josh_core::filter::is_ancestor_of(&transaction, old_parent, new_parent).map_err(
                    |err| {
                        user_error_with_message(
                            format!(
                                "Failed to compare embedded history parents at '{}'",
                                path.display()
                            ),
                            err,
                        )
                    },
                )?;
            if !materialized_fast_forward
                && let Some((change_id, old_change, new_change)) =
                    find_explicit_change_id_collision(&transaction, old_parent, new_parent)?
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
    }
    let previous_tree_oid =
        josh_core::git::read_tree_id(transaction.odb(), result.previous_materialized_commit)
            .map_err(|err| user_error_with_message("Failed to read the previous link tree", err))?;
    let linked_tree_oid =
        josh_core::git::read_tree_id(transaction.odb(), result.update.filtered_commit)
            .map_err(|err| user_error_with_message("Failed to read the updated link tree", err))?;
    transaction
        .flush_mem_odb()
        .map_err(|err| user_error_with_message("Failed to persist the updated link trees", err))?;
    let store = workspace_command.repo().store().clone();
    let previous_tree = tree_from_josh_oid(store.clone(), previous_tree_oid);
    let linked_tree = tree_from_josh_oid(store, linked_tree_oid);
    let merged_tree = MergedTree::merge(Merge::from_vec(vec![
        (linked_tree.clone(), "updated linked snapshot".to_owned()),
        (previous_tree, "previous linked snapshot".to_owned()),
        (commit.tree(), "local link changes".to_owned()),
    ]))
    .await?;

    insert_link_commit(
        ui,
        &mut workspace_command,
        &commit,
        linked_tree,
        Some((
            merged_tree,
            format!(
                "Preserve local changes while updating {} Josh link(s)",
                selected_links.len()
            ),
        )),
        embedded_parents,
        &transaction,
        &git_lock,
        format!("Update {} Josh link(s)", selected_links.len()),
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

// Keep publication state outside the rewritten commit graph. A destination shared by
// multiple links must have one lease, while different remotes/branches stay independent.
fn push_tracking_ref(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<String, CommandError> {
    let key = format!("{remote}\0{destination}");
    let id = josh_core::objects::write_blob(transaction.odb(), key.as_bytes()).map_err(|err| {
        user_error_with_message("Failed to identify the link push destination", err)
    })?;
    Ok(format!("refs/jjosh/link-push/{id}"))
}

async fn run_push(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: PushArgs,
) -> Result<(), CommandError> {
    // Only a successful push establishes the lease. In particular, querying the
    // remote immediately before pushing would silently authorize unseen changes.
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
    let tracking_ref = push_tracking_ref(&transaction, push_remote, &destination)?;
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
