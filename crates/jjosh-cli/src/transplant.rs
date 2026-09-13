use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use jj_cli::cli_util::{CommandHelper, RevisionArg, WorkspaceCommandHelper};
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::backend::{CommitId, TreeId};
use jj_lib::commit::Commit;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;
use jj_lib::rewrite::{RewriteRefsOptions, merge_commit_trees};
use jj_lib::working_copy::WorkingCopyFreshness;
use josh_core::filter::{Filter, Op, Rewrite};

use crate::interop::{check_git_state, open_josh_transaction, sha1_git_repo_path};

/// Transplant a complete mutable commit graph with explicit path and parent mappings.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    /// Revisions to rewrite, including every affected descendant
    #[arg(short = 'r', long = "revision", required = true, value_name = "REVSET")]
    revisions: Vec<RevisionArg>,
    /// Move a source directory to a destination directory; . denotes the root
    #[arg(long = "map", required = true, value_name = "FROM=TO")]
    maps: Vec<String>,
    /// Replace an external parent edge (use OLD=OLD to retain a side parent)
    #[arg(long = "parent", value_name = "OLD=NEW")]
    parents: Vec<String>,
    /// Explicitly discard a source path; gitlinks must be excluded exactly
    #[arg(long = "exclude", value_name = "PATH")]
    excludes: Vec<String>,
    /// Validate graph mappings and source tree terms without applying the rewrite
    #[arg(long)]
    dry_run: bool,
}

struct PathMap {
    source: PathBuf,
    destination: PathBuf,
}

struct Projection {
    maps: Vec<PathMap>,
    excludes: Vec<PathBuf>,
    filter: Filter,
}

fn parse_pair<'a>(value: &'a str, option: &str) -> Result<(&'a str, &'a str), CommandError> {
    let (left, right) = value
        .split_once('=')
        .filter(|(left, right)| !left.is_empty() && !right.is_empty())
        .ok_or_else(|| {
            user_error(format!(
                "{option} requires a non-empty OLD=NEW pair: {value}"
            ))
        })?;
    Ok((left, right))
}

fn normalized_path(value: &str) -> Result<PathBuf, CommandError> {
    if value.is_empty() || value.contains('\0') {
        return Err(user_error(
            "Transplant paths must not be empty or contain NUL",
        ));
    }
    let mut result = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(name) => result.push(name),
            Component::CurDir => {}
            _ => {
                return Err(user_error(format!(
                    "Transplant path must be relative and cannot contain ..: {value}"
                )));
            }
        }
    }
    Ok(result)
}

fn display_path(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        path.display().to_string()
    }
}

impl Projection {
    fn new(args: &Args) -> Result<Self, CommandError> {
        let mut maps: Vec<PathMap> = Vec::with_capacity(args.maps.len());
        for value in &args.maps {
            let (source, destination) = parse_pair(value, "--map")?;
            let source = normalized_path(source)?;
            let destination = normalized_path(destination)?;
            for previous in &maps {
                if source == previous.source
                    || (!source.as_os_str().is_empty()
                        && !previous.source.as_os_str().is_empty()
                        && (source.starts_with(&previous.source)
                            || previous.source.starts_with(&source)))
                {
                    return Err(user_error(format!(
                        "Duplicate or overlapping source mappings: {} and {}",
                        display_path(&previous.source),
                        display_path(&source)
                    )));
                }
                if destination == previous.destination {
                    return Err(user_error(format!(
                        "Duplicate destination mapping: {}",
                        display_path(&destination)
                    )));
                }
            }
            maps.push(PathMap {
                source,
                destination,
            });
        }
        if maps.is_empty() {
            return Err(user_error("At least one --map is required"));
        }
        let excludes = args
            .excludes
            .iter()
            .map(|value| normalized_path(value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut unique_excludes = HashSet::new();
        for path in &excludes {
            if !unique_excludes.insert(path) {
                return Err(user_error(format!(
                    "Duplicate excluded path: {}",
                    display_path(path)
                )));
            }
        }
        // Construct only tree-level structural operations, never arbitrary filters or
        // history filters. File selects an exact entry, including directories/gitlinks.
        let mut retained = Filter::new();
        for path in &excludes {
            retained = if path.as_os_str().is_empty() {
                retained.empty()
            } else {
                retained.exclude(Filter::new().file(path))
            };
        }
        let mut branches = Vec::with_capacity(maps.len());
        for mapping in &maps {
            let mut branch = retained;
            if mapping.source.as_os_str().is_empty() {
                // The root is fallback ownership, not a second copy of named sources.
                for other in &maps {
                    if !other.source.as_os_str().is_empty() {
                        branch = branch.exclude(Filter::new().file(&other.source));
                    }
                }
            } else {
                branch = branch.subdir(&mapping.source);
            }
            if !mapping.destination.as_os_str().is_empty() {
                branch = branch.prefix(&mapping.destination);
            }
            branches.push(branch);
        }
        let filter = josh_core::filter::to_filter(Op::Compose(branches));
        Ok(Self {
            maps,
            excludes,
            filter,
        })
    }

    fn validate_tree(
        &self,
        transaction: &josh_core::cache::Transaction,
        tree: gix_hash::ObjectId,
        destinations: &mut BTreeMap<PathBuf, PathBuf>,
    ) -> Result<(), CommandError> {
        let mut pending = vec![(PathBuf::new(), tree)];
        while let Some((directory, id)) = pending.pop() {
            let bytes = transaction
                .read_tree_bytes(transaction.odb(), id)
                .map_err(|err| user_error_with_message("Failed to read source tree", err))?
                .ok_or_else(|| user_error(format!("Source object {id} is not a tree")))?;
            // TreeReader::entries() skips malformed entries. Preflight must not.
            let parsed = gix_object::TreeRef::from_bytes(&bytes, gix_hash::Kind::Sha1)
                .map_err(|err| user_error_with_message("Malformed source Git tree", err))?;
            let mut names = HashSet::new();
            for entry in &parsed.entries {
                let name = std::str::from_utf8(entry.filename.as_ref()).map_err(|err| {
                    user_error_with_message("Source tree paths must be valid UTF-8 for jj", err)
                })?;
                if name.is_empty()
                    || name == "."
                    || name == ".."
                    || name.contains('/')
                    || !names.insert(name)
                {
                    return Err(user_error(format!(
                        "Invalid or duplicate tree entry in {}",
                        display_path(&directory)
                    )));
                }
                let path = directory.join(name);
                if entry.mode.is_tree() {
                    // Traverse even an excluded directory: a nested gitlink requires
                    // its own exact exclusion instead of being silently discarded.
                    pending.push((path, entry.oid.to_owned()));
                    continue;
                }
                if entry.mode.value() == 0o160000 && !self.excludes.contains(&path) {
                    return Err(user_error(format!(
                        "Source gitlink {} requires an exact --exclude {}",
                        path.display(),
                        path.display()
                    )));
                }
                if self
                    .excludes
                    .iter()
                    .any(|excluded| path.starts_with(excluded))
                {
                    continue;
                }
                if self.maps.iter().any(|mapping| {
                    !mapping.source.as_os_str().is_empty() && mapping.source.starts_with(&path)
                }) {
                    return Err(user_error(format!(
                        "Source mapping requires a directory, but {} is a non-directory entry",
                        path.display()
                    )));
                }
                let owner = self
                    .maps
                    .iter()
                    .find(|mapping| {
                        !mapping.source.as_os_str().is_empty() && path.starts_with(&mapping.source)
                    })
                    .or_else(|| {
                        self.maps
                            .iter()
                            .find(|mapping| mapping.source.as_os_str().is_empty())
                    })
                    .ok_or_else(|| {
                        user_error(format!(
                            "Source path {} has no --map owner; map it or explicitly --exclude it",
                            path.display()
                        ))
                    })?;
                let destination = owner
                    .destination
                    .join(path.strip_prefix(&owner.source).unwrap());
                if let Some(previous) = destinations.insert(destination.clone(), path.clone())
                    && previous != path
                {
                    return Err(user_error(format!(
                        "Source paths {} and {} both map to {} across source tree terms",
                        previous.display(),
                        path.display(),
                        destination.display()
                    )));
                }
            }
        }
        let mut previous: Option<&PathBuf> = None;
        for destination in destinations.keys() {
            if let Some(parent) = previous
                && destination.starts_with(parent) {
                    return Err(user_error(format!(
                        "Mapped file {} conflicts with descendant {}",
                        parent.display(),
                        destination.display()
                    )));
                }
            previous = Some(destination);
        }
        Ok(())
    }

    fn transform(
        &self,
        transaction: &josh_core::cache::Transaction,
        tree: &MergedTree,
        cache: &mut HashMap<TreeId, TreeId>,
    ) -> Result<MergedTree, CommandError> {
        // A conflict is one logical tree. Distinct paths in different signed
        // terms must not alias: cancellation could silently resolve the conflict.
        // Do not carry ownership across commits; historical renames are valid.
        let conflicted = !tree.tree_ids().is_resolved();
        if conflicted {
            let mut destinations = BTreeMap::new();
            for id in tree.tree_ids().iter() {
                let oid = gix_hash::ObjectId::try_from(id.as_bytes())
                    .map_err(|err| user_error_with_message("Invalid source Git tree ID", err))?;
                self.validate_tree(transaction, oid, &mut destinations)?;
            }
        }
        let ids = tree.tree_ids().try_map(|id| {
            if let Some(mapped) = cache.get(id) {
                return Ok(mapped.clone());
            }
            let oid = gix_hash::ObjectId::try_from(id.as_bytes())
                .map_err(|err| user_error_with_message("Invalid source Git tree ID", err))?;
            if !conflicted {
                self.validate_tree(transaction, oid, &mut BTreeMap::new())?;
            }
            let mapped =
                josh_core::filter::apply(transaction, self.filter, Rewrite::from_tree(oid))
                    .map_err(|err| {
                        user_error_with_message("Failed to transform a source tree term", err)
                    })?;
            let result = TreeId::from_bytes(mapped.tree_id().as_bytes());
            cache.insert(id.clone(), result.clone());
            Ok::<_, CommandError>(result)
        })?;
        // Do not simplify this merge: preserve the signed terms and their labels.
        Ok(MergedTree::new(
            tree.store().clone(),
            ids,
            tree.labels().clone(),
        ))
    }
}

/// Inspect without saving the snapshot. Otherwise an unsnapshotted edit could be
/// overwritten by the final checkout, or become a separate preflight operation.
async fn check_clean_working_copy(
    ui: &Ui,
    workspace: &WorkspaceCommandHelper,
    selected: &HashSet<CommitId>,
) -> Result<(), CommandError> {
    let Some(id) = workspace
        .get_wc_commit_id()
        .filter(|id| selected.contains(*id))
    else {
        return Ok(());
    };
    workspace.check_working_copy_writable()?;
    let commit = workspace.repo().store().get_commit_async(id).await?;
    let matcher = workspace.auto_tracking_matcher(ui)?;
    let options = workspace.snapshot_options_with_start_tracking_matcher(matcher.as_ref())?;
    let mut locked = workspace.working_copy().start_mutation().await?;
    if !matches!(
        WorkingCopyFreshness::check_stale(locked.as_ref(), &commit, workspace.repo()).await?,
        WorkingCopyFreshness::Fresh
    ) || locked.old_tree().tree_ids_and_labels() != commit.tree().tree_ids_and_labels()
    {
        return Err(user_error(
            "The working copy is stale; update it before transplanting",
        ));
    }
    let (snapshot, stats) = locked
        .snapshot(&options)
        .await
        .map_err(|err| user_error_with_message("Failed to inspect the working copy", err))?;
    if snapshot.tree_ids_and_labels() != commit.tree().tree_ids_and_labels()
        || !stats.untracked_paths.is_empty()
        || !stats.invalid_utf8_paths.is_empty()
    {
        return Err(user_error(
            "The working copy has unsnapshotted changes; snapshot or move them before transplanting",
        ));
    }
    // Dropping the lock without finish leaves working-copy state unchanged.
    Ok(())
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    let projection = Projection::new(&args)?;
    if command_helper.global_args().ignore_immutable {
        return Err(user_error("Transplant does not allow --ignore-immutable"));
    }
    if !command_helper.is_at_head_operation() || command_helper.global_args().no_integrate_operation
    {
        return Err(user_error(
            "Transplant requires the current operation; use --dry-run instead of --at-operation or --no-integrate-operation",
        ));
    }
    // Even workspace_helper_no_snapshot() automatically reconciles divergent
    // operations. Resolve the head read-only instead, rejecting multiple heads.
    let loaded_workspace = command_helper.load_workspace()?;
    let operation_heads = jj_lib::op_walk::get_current_head_ops(
        loaded_workspace.repo_loader().op_store(),
        loaded_workspace.repo_loader().op_heads_store().as_ref(),
    )
    .await?;
    let [operation] = operation_heads.as_slice() else {
        return Err(user_error(
            "Transplant requires exactly one operation head; reconcile operations separately before retrying",
        ));
    };
    let loaded_repo = loaded_workspace.repo_loader().load_at(operation).await?;
    jj_lib::git::get_git_backend(loaded_repo.store())?.disable_lazy_commit_imports();
    let mut workspace = command_helper.for_workable_repo(ui, loaded_workspace, loaded_repo)?;
    let repo_path = sha1_git_repo_path(&workspace)?;
    let git_lock = workspace.lock_git_import_export()?;
    check_git_state(&workspace)?;
    let selected_ids = workspace.resolve_some_revsets(ui, &args.revisions).await?;
    let selected: HashSet<_> = selected_ids.iter().cloned().collect();
    if selected.contains(workspace.repo().store().root_commit_id()) {
        return Err(user_error("The root commit cannot be transplanted"));
    }
    workspace.check_rewritable(selected.iter()).await?;
    let expression = selected_ids
        .iter()
        .map(|id| id.hex())
        .collect::<Vec<_>>()
        .join(" | ");
    let descendants = workspace
        .resolve_revsets_ordered(
            ui,
            &[RevisionArg::from(format!("descendants({expression})"))],
        )
        .await?;
    if let Some(missing) = descendants.iter().find(|id| !selected.contains(*id)) {
        return Err(user_error(format!(
            "Selection must include all affected descendants; missing {}",
            missing.hex()
        )));
    }
    let mut commits = BTreeMap::new();
    let mut change_ids = HashSet::new();
    let mut boundary = HashSet::new();
    for id in selected_ids {
        let commit = workspace.repo().store().get_commit_async(&id).await?;
        if !change_ids.insert(commit.change_id().clone()) {
            return Err(user_error(format!(
                "Selected graph contains divergent versions of change {}; choose one version before transplanting",
                commit.change_id().hex()
            )));
        }
        boundary.extend(
            commit
                .parent_ids()
                .iter()
                .filter(|parent| !selected.contains(*parent))
                .cloned(),
        );
        commits.insert(id, commit);
    }
    let mut parent_map = HashMap::new();
    for value in &args.parents {
        let (old, new) = parse_pair(value, "--parent")?;
        let old = workspace
            .resolve_single_rev(ui, &RevisionArg::from(old.to_owned()))
            .await?;
        let new = workspace
            .resolve_single_rev(ui, &RevisionArg::from(new.to_owned()))
            .await?;
        if !boundary.contains(old.id()) {
            return Err(user_error(format!(
                "Unused --parent mapping: {} is not an external parent of the selected graph",
                old.id().hex()
            )));
        }
        if parent_map
            .insert(old.id().clone(), new.id().clone())
            .is_some()
        {
            return Err(user_error(format!(
                "Duplicate or ambiguous --parent mapping for {}",
                old.id().hex()
            )));
        }
    }
    for old in &boundary {
        if !parent_map.contains_key(old) {
            return Err(user_error(format!(
                "Missing --parent mapping for external parent {} (use OLD=OLD to retain it)",
                old.hex()
            )));
        }
    }
    // An explicitly named hidden descendant need not appear in the default
    // visible descendants revset. It is still an invalid destination.
    let external_targets = parent_map
        .values()
        .filter(|id| !selected.contains(*id))
        .map(|id| id.hex())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" | ");
    if !external_targets.is_empty() {
        let cyclic = workspace
            .resolve_revsets_ordered(
                ui,
                &[RevisionArg::from(format!(
                    "ancestors({external_targets}) & ({expression})"
                ))],
            )
            .await?;
        if !cyclic.is_empty() {
            return Err(user_error(
                "An external destination parent descends from the selected graph; include that descendant and choose acyclic parent mappings",
            ));
        }
    }
    // Topologically order the graph with the requested boundary substitutions.
    // This also permits NEW to be selected, provided the new graph is acyclic.
    let mut parents = HashMap::new();
    let mut children: HashMap<CommitId, Vec<CommitId>> = HashMap::new();
    let mut degree = HashMap::new();
    let mut ready = BTreeSet::new();
    for (id, commit) in &commits {
        let new_parents: Vec<_> = commit
            .parent_ids()
            .iter()
            .map(|parent| {
                if selected.contains(parent) {
                    parent.clone()
                } else {
                    parent_map[parent].clone()
                }
            })
            .collect();
        let mut unique = HashSet::new();
        if new_parents.iter().any(|parent| !unique.insert(parent)) {
            return Err(user_error(format!(
                "Parent mappings would collapse distinct parent edges of {}",
                id.hex()
            )));
        }
        if new_parents.len() > 1 && new_parents.contains(workspace.repo().store().root_commit_id())
        {
            return Err(user_error(format!(
                "Parent mappings would create a merge with root() as a side parent at {}; Git cannot represent it",
                id.hex()
            )));
        }
        let mut count = 0;
        for parent in &new_parents {
            if selected.contains(parent) {
                children.entry(parent.clone()).or_default().push(id.clone());
                count += 1;
            }
        }
        if count == 0 {
            ready.insert(id.clone());
        }
        degree.insert(id.clone(), count);
        parents.insert(id.clone(), new_parents);
    }
    let mut order = Vec::with_capacity(commits.len());
    while let Some(id) = ready.pop_first() {
        if let Some(next) = children.get(&id) {
            for child in next {
                let count = degree.get_mut(child).unwrap();
                *count -= 1;
                if *count == 0 {
                    ready.insert(child.clone());
                }
            }
        }
        order.push(id);
    }
    if order.len() != commits.len() {
        return Err(user_error(
            "Parent mappings create a cycle in the transplanted graph",
        ));
    }
    check_clean_working_copy(ui, &workspace, &selected).await?;
    // Keep transformed trees ephemeral during preflight. Only apply flushes
    // them so jj's independent backend can read the new tree terms.
    let transaction = open_josh_transaction(&repo_path, true)?;
    let mut tree_cache = HashMap::new();
    let mut transformed = HashMap::new();
    for (id, commit) in &commits {
        let old_parents = commit.parents().await?;
        let old_base = merge_commit_trees(workspace.repo().as_ref(), &old_parents).await?;
        let base = projection.transform(&transaction, &old_base, &mut tree_cache)?;
        let tree = projection.transform(&transaction, &commit.tree(), &mut tree_cache)?;
        transformed.insert(id.clone(), (base, tree));
    }
    writeln!(
        ui.status(),
        "Transplant plan: {} commits, {} boundary parents{}",
        commits.len(),
        boundary.len(),
        if args.dry_run {
            " (dry-run; not applied)"
        } else {
            ""
        }
    )?;
    for mapping in &projection.maps {
        writeln!(
            ui.status(),
            "  map {} -> {}",
            display_path(&mapping.source),
            display_path(&mapping.destination)
        )?;
    }
    for excluded in &projection.excludes {
        writeln!(ui.status(), "  exclude {}", display_path(excluded))?;
    }
    let mut boundary_plan: Vec<_> = parent_map.iter().collect();
    boundary_plan.sort_by_key(|(old, _)| *old);
    for (old, new) in boundary_plan {
        writeln!(ui.status(), "  parent {} -> {}", old.hex(), new.hex())?;
    }
    if args.dry_run {
        writeln!(
            ui.status(),
            "Preflight complete: no commits, operation, refs, view, or working-copy state updated. Resulting merge conflicts are evaluated when applying."
        )?;
        return Ok(());
    }
    transaction.flush_mem_odb().map_err(|err| {
        user_error_with_message("Failed to persist transformed tree terms for jj", err)
    })?;
    let mut tx = workspace.start_transaction();
    let mut rewritten: HashMap<CommitId, Commit> = HashMap::new();
    let mut mappings = Vec::with_capacity(order.len());
    for id in order {
        let old = &commits[&id];
        let mut new_parents = Vec::with_capacity(parents[&id].len());
        for parent in &parents[&id] {
            new_parents.push(if let Some(new) = rewritten.get(parent) {
                new.clone()
            } else {
                tx.repo().store().get_commit_async(parent).await?
            });
        }
        let destination = merge_commit_trees(tx.repo(), &new_parents).await?;
        let (base, tree) = transformed.remove(&id).unwrap();
        let rebased = MergedTree::merge(Merge::from_vec(vec![
            (
                destination,
                format!("{} (transplant destination)", old.conflict_label()),
            ),
            (
                base,
                format!("{} (mapped original parents)", old.conflict_label()),
            ),
            (
                tree,
                format!("{} (transplanted revision)", old.conflict_label()),
            ),
        ]))
        .await?;
        // rewrite_commit preserves change identity, author, description and
        // predecessor metadata. set_tree writes both jj:trees and Git's visible
        // conflict tree; no commit-filter pruning or rebase-empty policy applies.
        let new = tx
            .repo_mut()
            .rewrite_commit(old)
            .set_parents(
                new_parents
                    .iter()
                    .map(|parent| parent.id().clone())
                    .collect(),
            )
            .set_tree(rebased)
            .write()
            .await?;
        mappings.push((id.clone(), new.id().clone()));
        rewritten.insert(id, new);
    }
    tx.repo_mut()
        .update_rewritten_references(&RewriteRefsOptions::default())
        .await?;
    for (old, new) in &mappings {
        writeln!(
            ui.status(),
            "  {} -> {} (pending transaction)",
            old.hex(),
            new.hex()
        )?;
    }
    tx.finish_with_git_import_export_lock(ui, "transplant projected commit graph", &git_lock)
        .await?;
    writeln!(
        ui.status(),
        "Transplanted {} commits in one jj transaction.",
        mappings.len()
    )?;
    Ok(())
}
