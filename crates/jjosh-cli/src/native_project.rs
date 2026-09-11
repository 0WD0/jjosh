use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as _;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use jj_cli::cli_util::CommandHelper;
use jj_cli::cli_util::RevisionArg;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::backend::Tree;
use jj_lib::backend::TreeId;
use jj_lib::backend::TreeValue;
use jj_lib::backend::{self};
use jj_lib::commit::Commit;
use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::index::ResolvedChangeState;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::op_store::RemoteRef;
use jj_lib::op_store::RemoteRefState;
use jj_lib::op_store::View;
use jj_lib::ref_name::RefName;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::ref_name::RemoteRefSymbol;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use jj_lib::repo_path::RepoPathComponent;
use josh_core::cache::Expected;
use josh_core::cache::Transaction;

use crate::native_source::NativeSource;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct FetchArgs {
    /// Imported project name (its canonical top-level directory).
    project: String,
    /// Git URL or local jj workspace. A jj workspace is read without snapshotting.
    source: String,
    /// Source branch/bookmark to receive, including a contributor's topic branch.
    #[arg(long)]
    branch: String,
    /// Name for the observation, exposed as PROJECT/BRANCH@PROJECT-REMOTE.
    #[arg(long, default_value = "upstream")]
    remote: String,
}

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct PushArgs {
    /// Imported project to publish; other projects are not exported.
    project: String,
    /// Destination Git URL or configured Git remote name.
    #[arg(long)]
    remote: String,
    /// Destination branch, independent of any import or fetch source.
    #[arg(long)]
    branch: String,
    /// Exact monorepo revision to project.
    #[arg(short = 'r', long)]
    revision: RevisionArg,
    /// Prepare the export and check the push without publishing refs or anchors.
    #[arg(long)]
    dry_run: bool,
    /// Explicitly bypass the destination lease/non-fast-forward protection.
    #[arg(long)]
    force: bool,
}

pub(crate) fn validate_project(project: &str) -> Result<()> {
    ensure!(
        !project.is_empty()
            && project
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "Project names must contain only ASCII letters, digits, '-' or '_'"
    );
    Ok(())
}

pub(crate) fn project_ref_prefix(project: &str) -> String {
    format!("refs/jjosh/native/{project}/")
}

fn oid(id: &CommitId) -> Result<gix_hash::ObjectId> {
    Ok(gix_hash::ObjectId::try_from(id.as_bytes())?)
}

fn retain_original(transaction: &Transaction, project: &str, raw: &CommitId) -> Result<()> {
    if raw.as_bytes().iter().all(|byte| *byte == 0) {
        return Ok(());
    }
    // A ref in the private namespace keeps raw ancestry alive without exposing
    // it as a branch or importing it into jj's visible graph.
    let name = format!("refs/jjosh/native/{project}/{}", raw.hex());
    let value = oid(raw)?;
    let previous = transaction.resolve_ref(&name)?;
    ensure!(
        previous.is_none_or(|old| old == value),
        "Native object retention ref was modified: {name}"
    );
    transaction.update_ref(
        &name,
        previous.map_or(Expected::Absent, Expected::At),
        value,
        "retain native project source",
    )?;
    Ok(())
}

pub(crate) fn record_anchor(
    transaction: &Transaction,
    project: &str,
    kind: &str,
    raw: &CommitId,
    canonical: &CommitId,
) -> Result<()> {
    if raw.as_bytes().iter().all(|byte| *byte == 0) {
        return Ok(());
    }
    let name = format!("{}{kind}/{}", project_ref_prefix(project), raw.hex());
    let value = oid(canonical)?;
    let previous = transaction.resolve_ref(&name)?;
    ensure!(
        previous.is_none_or(|old| old == value),
        "Native correspondence {name} already identifies another monorepo revision"
    );
    retain_original(transaction, project, raw)?;
    transaction.update_ref(
        &name,
        previous.map_or(Expected::Absent, Expected::At),
        value,
        "record native history boundary",
    )?;
    Ok(())
}

pub(crate) fn record_imported_boundaries<'a>(
    transaction: &Transaction,
    project: &str,
    imported: &crate::native_import::Imported,
    tips: impl IntoIterator<Item = &'a CommitId>,
) -> Result<()> {
    let mut grafted = HashSet::new();
    for (raw, canonical) in &imported.grafts {
        record_anchor(transaction, project, "graft", raw, canonical)?;
        grafted.insert(raw.clone());
    }
    for raw in tips {
        if grafted.contains(raw) {
            continue;
        }
        let Some(canonical) = imported.ids.get(raw) else {
            continue;
        };
        record_anchor(transaction, project, "origin", raw, canonical)?;
    }
    Ok(())
}

/// Lossless imported ancestry can be recovered from pairs of graph roots.
/// Published projections are explicit boundaries: their parent graphs may have
/// pruned unrelated changes and must never be zipped with the canonical graph.
pub(crate) async fn anchors(
    repo: &dyn Repo,
    transaction: &Transaction,
    project: &str,
) -> Result<HashMap<CommitId, CommitId>> {
    ensure!(
        !repo
            .view()
            .store_view()
            .remote_views
            .keys()
            .any(|name| { name.as_str() == format!("jjosh-native-{project}") }),
        "Run `jjosh native migrate` to move legacy correspondence bookmarks to private refs"
    );
    let prefix = project_ref_prefix(project);
    let mut ids = HashMap::from([(
        repo.store().root_commit_id().clone(),
        repo.store().root_commit_id().clone(),
    )]);
    let mut pending = Vec::new();
    transaction.for_each_ref_prefixed(&prefix, |name, canonical| {
        let Some((kind, raw)) = name[prefix.len()..].split_once('/') else {
            return Ok(()); // Raw-object retention ref, not a correspondence.
        };
        let raw = CommitId::try_from_hex(raw).context("Invalid native correspondence commit ID")?;
        ensure!(
            raw.as_bytes().len() == 20,
            "Invalid native correspondence commit ID"
        );
        let canonical = CommitId::from_bytes(canonical.as_bytes());
        match kind {
            "origin" => pending.push((raw, canonical.clone())),
            "published" | "graft" => {
                ids.insert(raw, canonical.clone());
            }
            _ => anyhow::bail!("Unknown native correspondence kind: {kind}"),
        }
        Ok(())
    })?;
    let named = |name: &str| crate::ref_names::belongs_to_project(project, name);
    let mut present = !pending.is_empty()
        || ids.len() > 1
        || repo
            .view()
            .local_bookmarks()
            .any(|(name, _)| named(name.as_str()))
        || repo
            .view()
            .local_tags()
            .any(|(name, _)| named(name.as_str()))
        || repo
            .view()
            .all_remote_bookmarks()
            .any(|(symbol, _)| named(symbol.name.as_str()));
    if !present {
        let component = RepoPathComponent::new(project)?;
        'heads: for head in repo.view().heads() {
            let commit = repo.store().get_commit_async(head).await?;
            for term in commit.tree_ids().iter() {
                let tree = repo
                    .store()
                    .backend()
                    .read_tree(RepoPath::root(), term)
                    .await?;
                if tree.value(component).is_some() {
                    present = true;
                    break 'heads;
                }
            }
        }
    }
    ensure!(
        present,
        "Project {project:?} is not present; import or link it first"
    );
    while let Some((raw, canonical)) = pending.pop() {
        if let Some(previous) = ids.get(&raw) {
            ensure!(
                *previous == canonical,
                "Inconsistent native correspondence for {raw}"
            );
            continue;
        }
        let original = josh_core::objects::CommitData::read(transaction.odb(), oid(&raw)?)
            .with_context(|| format!("Original source object {raw} is unavailable"))?;
        let mut raw_parents: Vec<_> = original
            .parent_ids()
            .map(|id| CommitId::from_bytes(id.as_bytes()))
            .collect();
        if raw_parents.is_empty() {
            raw_parents.push(repo.store().root_commit_id().clone());
        }
        let mapped = repo.store().get_commit_async(&canonical).await?;
        ensure!(
            raw_parents.len() == mapped.parent_ids().len(),
            "Native source ancestry no longer corresponds at {raw}"
        );
        pending.extend(
            raw_parents
                .into_iter()
                .zip(mapped.parent_ids().iter().cloned()),
        );
        ids.insert(raw, canonical);
    }
    Ok(ids)
}

/// After prefix/filter, a dest commit is the same version iff the project tree
/// and preserved metadata match. Parents are remapped, so they are not compared.
pub(crate) async fn existing_same_project_version(
    repo: &dyn Repo,
    project: &str,
    source: &backend::Commit,
) -> Result<Option<CommitId>> {
    let Some(targets) = repo.resolve_change_id(&source.change_id).await? else {
        return Ok(None);
    };
    let component = RepoPathComponent::new(project)?;
    let mut matched: Option<CommitId> = None;
    for (id, state) in &targets.targets {
        if *state != ResolvedChangeState::Visible {
            continue;
        }
        let commit = repo.store().get_commit_async(id).await?;
        let mut has_project = false;
        for tree_id in commit.tree_ids().iter() {
            let tree = repo
                .store()
                .backend()
                .read_tree(RepoPath::root(), tree_id)
                .await?;
            match tree.value(component) {
                Some(TreeValue::Tree(_)) => has_project = true,
                Some(_) => {
                    anyhow::bail!("Project {project:?} is not a directory in {}", commit.id())
                }
                None => {}
            }
        }
        if !has_project {
            continue;
        }
        if commit.description() != source.description
            || commit.author() != &source.author
            || commit.committer() != &source.committer
        {
            continue;
        }
        let projected = project_tree(&commit, project).await?;
        if projected.tree_ids() != &source.root_tree {
            continue;
        }
        if let Some(previous) = &matched {
            ensure!(
                previous == id,
                "Change {} has multiple matching {project} versions {} and {}",
                source.change_id.hex(),
                previous.hex(),
                id.hex()
            );
        }
        matched = Some(id.clone());
    }
    Ok(matched)
}

pub(crate) async fn inherit_other_projects(
    repo: &dyn Repo,
    project: &str,
    incoming: &backend::Commit,
) -> Result<MergedTree> {
    let store = repo.store();
    let mut parents = Vec::with_capacity(incoming.parents.len());
    for id in &incoming.parents {
        parents.push(store.get_commit_async(id).await?);
    }
    let parent_tree = jj_lib::rewrite::merge_commit_trees(repo, &parents).await?;
    let component = RepoPathComponent::new(project)?;
    let mut outside = HashMap::new();
    for id in parent_tree.tree_ids().iter() {
        if !outside.contains_key(id) {
            let tree = store.backend().read_tree(RepoPath::root(), id).await?;
            let entries = tree
                .entries()
                .filter(|entry| entry.name() != component)
                .map(|entry| (entry.name().to_owned(), entry.value().clone()))
                .collect();
            let id_out = store
                .backend()
                .write_tree(RepoPath::root(), &Tree::from_sorted_entries(entries))
                .await?;
            outside.insert(id.clone(), id_out);
        }
    }
    let incoming = MergedTree::new(
        store.clone(),
        incoming.root_tree.clone(),
        ConflictLabels::from_merge(incoming.conflict_labels.clone()),
    );
    if outside.values().all(|id| id == store.empty_tree_id()) {
        return Ok(incoming);
    }
    let outside = MergedTree::new(
        store.clone(),
        parent_tree.tree_ids().map(|id| outside[id].clone()),
        parent_tree.labels().clone(),
    );
    let empty = MergedTree::resolved(store.clone(), store.empty_tree_id().clone());
    Ok(MergedTree::merge(Merge::from_vec(vec![
        (outside, "other projects at source boundary".to_owned()),
        (empty, String::new()),
        (incoming, "received project state".to_owned()),
    ]))
    .await?)
}

pub(crate) async fn project_tree(commit: &Commit, project: &str) -> Result<MergedTree> {
    let store = commit.store();
    let component = RepoPathComponent::new(project)?;
    let mut terms = HashMap::new();
    for id in commit.tree_ids().iter() {
        if terms.contains_key(id) {
            continue;
        }
        let tree = store.backend().read_tree(RepoPath::root(), id).await?;
        let selected = match tree.value(component) {
            Some(TreeValue::Tree(id)) => id.clone(),
            None => store.empty_tree_id().clone(),
            Some(_) => anyhow::bail!("Project {project:?} is not a directory in {}", commit.id()),
        };
        terms.insert(id.clone(), selected);
    }
    Ok(MergedTree::new(
        store.clone(),
        commit.tree_ids().map(|id| terms[id].clone()),
        commit.tree().labels().clone(),
    )
    .resolve()
    .await?)
}

struct Projected {
    raw: CommitId,
    tree: Merge<TreeId>,
}

enum Visit {
    Read(CommitId),
    Write(Commit),
}

async fn export_project(
    repo: &dyn Repo,
    project: &str,
    head: &Commit,
    known: &HashMap<CommitId, CommitId>,
) -> Result<(CommitId, Vec<(CommitId, CommitId)>)> {
    ensure!(
        !project_tree(head, project).await?.has_conflict(),
        "Project {project:?} has unresolved conflicts at the publication revision"
    );
    let mut reverse = HashMap::new();
    for (raw, canonical) in known {
        if let Some(previous) = reverse.insert(canonical.clone(), raw.clone()) {
            ensure!(
                previous == *raw,
                "Project has ambiguous reverse history correspondence at {canonical}"
            );
        }
    }
    let root = repo.store().root_commit_id();
    let mut mapped = HashMap::from([(
        root.clone(),
        Projected {
            raw: root.clone(),
            tree: Merge::resolved(repo.store().empty_tree_id().clone()),
        },
    )]);
    let mut pending = vec![Visit::Read(head.id().clone())];
    let mut publications = Vec::new();
    let mut published_ids = HashSet::new();
    while let Some(visit) = pending.pop() {
        match visit {
            Visit::Read(id) => {
                if mapped.contains_key(&id) {
                    continue;
                }
                let commit = repo.store().get_commit_async(&id).await?;
                if let Some(raw) = reverse.get(&id) {
                    mapped.insert(
                        id,
                        Projected {
                            raw: raw.clone(),
                            tree: project_tree(&commit, project).await?.tree_ids().clone(),
                        },
                    );
                    continue;
                }
                let parents = commit.parent_ids().to_vec();
                pending.push(Visit::Write(commit));
                pending.extend(parents.into_iter().map(Visit::Read));
            }
            Visit::Write(commit) => {
                let tree = project_tree(&commit, project).await?;
                let mut seen = HashSet::new();
                let mut parents: Vec<_> = commit
                    .parent_ids()
                    .iter()
                    .map(|id| mapped[id].raw.clone())
                    .filter(|id| seen.insert(id.clone()))
                    .collect();
                if parents.len() > 1 {
                    parents.retain(|id| id != root);
                }
                let same_parent = if parents.len() == 1 {
                    commit.parent_ids().iter().find(|id| {
                        mapped[*id].raw == parents[0] && mapped[*id].tree == *tree.tree_ids()
                    })
                } else {
                    None
                };
                if let Some(parent) = same_parent {
                    mapped.insert(
                        commit.id().clone(),
                        Projected {
                            raw: mapped[parent].raw.clone(),
                            tree: tree.tree_ids().clone(),
                        },
                    );
                    continue;
                }
                if parents.is_empty() {
                    parents.push(root.clone());
                }
                let mut contents = commit.store_commit().as_ref().clone();
                contents.parents = parents;
                contents.root_tree = tree.tree_ids().clone();
                contents.conflict_labels = tree.labels().as_merge().clone();
                contents.predecessors.clear();
                contents.secure_sig = None;
                let exported = repo.store().write_commit(contents.clone(), None).await?;
                ensure!(
                    exported.store_commit().as_ref() == &contents,
                    "Backend changed metadata while projecting {}",
                    commit.id()
                );
                if !known.contains_key(exported.id()) && published_ids.insert(exported.id().clone())
                {
                    publications.push((exported.id().clone(), commit.id().clone()));
                }
                mapped.insert(
                    commit.id().clone(),
                    Projected {
                        raw: exported.id().clone(),
                        tree: tree.tree_ids().clone(),
                    },
                );
            }
        }
    }
    let result = mapped.remove(head.id()).unwrap().raw;
    ensure!(
        &result != root,
        "Selected history contains no project content to publish"
    );
    Ok((result, publications))
}

fn check_branch(transaction: &Transaction, branch: &str) -> Result<String> {
    ensure!(!branch.starts_with('-'), "A branch cannot start with '-'");
    let branch = format!("refs/heads/{branch}");
    transaction.spawn_git(&["check-ref-format", &branch], &[])?;
    Ok(branch)
}

fn endpoint(
    command: &CommandHelper,
    workspace: &jj_cli::cli_util::WorkspaceCommandHelper,
    transaction: &Transaction,
    input: &str,
    push: bool,
) -> Result<String, CommandError> {
    let git_path = crate::interop::sha1_git_repo_path(workspace)?;
    let git = jj_lib::git::get_git_backend(workspace.repo().store())?.git_repo();
    if git
        .remote_names()
        .iter()
        .any(|name| &name[..] == input.as_bytes())
    {
        let mut args = vec!["remote", "get-url"];
        if push {
            args.push("--push");
        }
        args.extend(["--all", input]);
        let output = transaction
            .git_command(&args, &[])
            .map_err(user_error)?
            .with_stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(user_error)?;
        let text = std::str::from_utf8(&output.stdout).map_err(user_error)?;
        let urls: Vec<_> = text.lines().collect();
        let [url] = urls.as_slice() else {
            return Err(user_error(
                "Choose one explicit URL for a remote with multiple destinations",
            ));
        };
        // Bind leases to the resolved endpoint, not an alias whose URL can change.
        jj_cli::git_util::absolute_git_url(&josh_core::git::normalize_repo_path(&git_path), url)
    } else {
        jj_cli::git_util::absolute_git_url(command.cwd(), input)
    }
}

pub(crate) async fn fetch(
    ui: &mut Ui,
    command: &CommandHelper,
    args: FetchArgs,
) -> Result<(), CommandError> {
    validate_project(&args.project).map_err(user_error)?;
    validate_project(&args.remote).map_err(user_error)?;
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error("Native fetch requires the current operation"));
    }
    let mut workspace = command.workspace_helper(ui).await?;
    let git_lock = workspace.lock_git_import_export()?;
    let git_path = crate::interop::sha1_git_repo_path(&workspace)?;
    let transaction = crate::interop::open_josh_transaction(&git_path, false)?;
    let known = anchors(workspace.repo().as_ref(), &transaction, &args.project)
        .await
        .map_err(user_error)?;
    let branch = check_branch(&transaction, &args.branch).map_err(user_error)?;
    let local = command.cwd().join(&args.source);
    let (source, target, git_observation) = if local.join(".jj").is_dir() {
        let source_workspace = command.load_workspace_at(&local, workspace.settings())?;
        if std::fs::canonicalize(source_workspace.repo_path())?
            == std::fs::canonicalize(workspace.repo_path())?
        {
            return Err(user_error(
                "Native fetch requires an external source repository",
            ));
        }
        let source = NativeSource::read(source_workspace.repo_loader(), Some(&args.branch))
            .await
            .map_err(user_error)?;
        let target = source
            .view
            .local_bookmarks
            .get(RefName::new(&args.branch))
            .ok_or_else(|| user_error(format!("Source has no local bookmark {:?}", args.branch)))?
            .clone();
        (source, target, None)
    } else {
        let source_url = endpoint(command, &workspace, &transaction, &args.source, false)?;
        transaction
            .spawn_git(&["fetch", "--no-tags", "--", &source_url, &branch], &[])
            .map_err(user_error)?;
        let raw = josh_core::git::resolve_fetch_head(&transaction).map_err(user_error)?;
        let id = CommitId::from_bytes(raw.as_bytes());
        let backend = jj_lib::git::get_git_backend(workspace.repo().store())?;
        backend.import_head_commits([&id])?;
        let view = View::make_root(id.clone());
        let source = NativeSource::read_view(
            workspace.repo().store().clone(),
            workspace.repo().op_store().clone(),
            view,
            workspace.repo().op_id().hex(),
        )
        .await
        .map_err(user_error)?;
        (source, RefTarget::normal(id), Some((source_url, raw)))
    };
    let mut tx = workspace.start_transaction();
    let imported =
        crate::native_import::import_source(&source, tx.repo_mut(), &args.project, known)
            .await
            .map_err(user_error)?;
    let target_mapped = RefTarget::from_merge(
        target
            .as_merge()
            .map(|term| term.as_ref().map(|id| imported.ids[id].clone())),
    );
    let remote: RemoteNameBuf = format!("{}-{}", args.project, args.remote).into();
    if jj_lib::git::get_git_repo(tx.repo().store())?
        .remote_names()
        .iter()
        .any(|name| &name[..] == remote.as_str().as_bytes())
    {
        return Err(user_error(
            "Observation name collides with a configured Git remote",
        ));
    }
    let name = crate::ref_names::local_name(&args.project, &args.branch);
    let symbol = RemoteRefSymbol {
        name: RefName::new(&name),
        remote: &remote,
    };
    let previous = tx.repo().view().get_remote_bookmark(symbol).clone();
    if previous.state == RemoteRefState::Tracked {
        tx.repo_mut()
            .merge_local_bookmark(symbol.name, &previous.target, &target_mapped)
            .await?;
    }
    tx.repo_mut().set_remote_bookmark(
        symbol,
        RemoteRef {
            target: target_mapped.clone(),
            state: previous.state,
        },
    );
    record_imported_boundaries(
        &transaction,
        &args.project,
        &imported,
        target.as_merge().iter().flatten(),
    )
    .map_err(user_error)?;
    // Fetch records observations without silently changing local bookmarks,
    // selecting a revision, or imposing a rebase/merge policy.
    for id in target_mapped.as_merge().iter().flatten() {
        let commit = tx.repo().store().get_commit_async(id).await?;
        tx.repo_mut().add_head(&commit).await?;
    }
    transaction.flush_mem_odb().map_err(user_error)?;
    let stats = jj_lib::git::export_some_refs(tx.repo_mut(), |_, candidate| candidate == symbol)?;
    jj_cli::git_util::print_git_export_stats(ui, &stats)?;
    tx.into_inner()
        .commit(format!(
            "receive native project {} from {}",
            args.project, args.source
        ))
        .await?;
    if let Some((source_url, raw)) = git_observation {
        crate::link_refs::record_observation(&transaction, &source_url, &branch, raw)?;
    }
    drop(git_lock);
    writeln!(
        ui.status(),
        "Received {symbol}: {} new native commits. Integrate with jj new/rebase; working copy \
         unchanged.",
        imported.commits.len()
    )?;
    Ok(())
}

pub(crate) async fn push(
    ui: &mut Ui,
    command: &CommandHelper,
    args: PushArgs,
) -> Result<(), CommandError> {
    validate_project(&args.project).map_err(user_error)?;
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error("Native push requires the current operation"));
    }
    let workspace = command.workspace_helper(ui).await?;
    let head = workspace.resolve_single_rev(ui, &args.revision).await?;
    let git_lock = workspace.lock_git_import_export()?;
    let transaction = crate::interop::open_josh_transaction(
        &crate::interop::sha1_git_repo_path(&workspace)?,
        false,
    )?;
    let known = anchors(workspace.repo().as_ref(), &transaction, &args.project)
        .await
        .map_err(user_error)?;
    let branch = check_branch(&transaction, &args.branch).map_err(user_error)?;
    let remote_url = endpoint(command, &workspace, &transaction, &args.remote, true)?;
    let (exported, publications) =
        export_project(workspace.repo().as_ref(), &args.project, &head, &known)
            .await
            .map_err(user_error)?;
    let tracking = crate::link_refs::push_tracking_ref(&transaction, &remote_url, &branch)?;
    let expected = transaction.resolve_ref(&tracking).map_err(user_error)?;
    let lease = expected
        .filter(|_| !args.force)
        .map(|id| format!("--force-with-lease={branch}:{id}"));
    let refspec = format!(
        "{}{}:{branch}",
        if args.force { "+" } else { "" },
        exported.hex()
    );
    let mut push_args = vec!["push"];
    if let Some(lease) = &lease {
        push_args.push(lease);
    }
    if args.dry_run {
        push_args.push("--dry-run");
    }
    push_args.extend(["--", &remote_url, &refspec]);
    transaction.spawn_git(&push_args, &[]).map_err(user_error)?;
    if args.dry_run {
        writeln!(
            ui.status(),
            "Native project push preflight succeeded. No remote update or native correspondence \
             published."
        )?;
        return Ok(());
    }
    // Save correspondence only after successful publication. A returned partial
    // change then refers to the canonical cross-project change, not a second
    // same-change-ID commit that could incorrectly absorb its remaining work.
    let save_error = |err| {
        user_error_with_message(
            "Remote was updated, but native publication state could not be saved",
            err,
        )
    };
    for (raw, canonical) in &publications {
        record_anchor(&transaction, &args.project, "published", raw, canonical)
            .map_err(&save_error)?;
    }
    retain_original(&transaction, &args.project, &exported).map_err(&save_error)?;
    transaction
        .update_ref(
            &tracking,
            expected.map_or(Expected::Absent, Expected::At),
            oid(&exported).map_err(&save_error)?,
            "native project publication",
        )
        .map_err(&save_error)?;
    transaction.flush_mem_odb().map_err(&save_error)?;
    drop(git_lock);
    writeln!(
        ui.status(),
        "Published {} revision {} to {}:{branch}",
        args.project,
        head.id().hex(),
        args.remote
    )?;
    Ok(())
}
