use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
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
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::store::Store;
use josh_core::cache::Expected;
use josh_core::cache::Transaction;
pub(crate) fn parse_project(name: &str) -> Result<String> {
    let name = jj_lib::revset::parse_symbol(name)
        .map_err(|err| anyhow::anyhow!("Invalid project name: {}", err.kind()))?;
    ensure!(
        !name.contains('#'),
        "Project names cannot contain '#'; it separates NAME#PROJECT"
    );
    ensure!(
        !name.contains('/'),
        "Project names cannot contain '/'; use --mount for the dest path"
    );
    gix_validate::reference::name_partial(name.as_bytes().into()).map_err(|err| {
        anyhow::anyhow!("Project name {name:?} is not a valid Git ref component: {err}")
    })?;
    Ok(name)
}

pub(crate) fn validate_project(name: &str) -> Result<()> {
    parse_project(name).map(|_| ())
}

pub(crate) fn project_ref_prefix(project: &str) -> String {
    format!("refs/jjosh/native/{project}/")
}

fn mount_ref_name(project: &str) -> String {
    format!("{}mount", project_ref_prefix(project))
}

/// Dest directory for a native project. Identity stays `NAME`; this path may be nested.
pub(crate) fn parse_mount(value: &str) -> Result<RepoPathBuf> {
    ensure!(
        !value.is_empty() && !value.contains('\\') && !value.contains('\0'),
        "Project mount {value:?} must be a repository-relative directory"
    );
    let path = RepoPathBuf::from_internal_string(value)?;
    ensure!(
        !path.is_root()
            && path.components().all(|component| {
                let name = component.as_internal_str();
                name != "." && name != ".."
            }),
        "Project mount {value:?} must stay within the repository and cannot be the root"
    );
    Ok(path)
}

pub(crate) fn default_mount(project: &str) -> Result<RepoPathBuf> {
    parse_mount(project)
}

fn mounts_overlap(left: &RepoPath, right: &RepoPath) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

pub(crate) fn check_mounts_disjoint<'a>(
    mounts: impl IntoIterator<Item = (&'a str, &'a RepoPath)>,
) -> Result<()> {
    let mounts: Vec<_> = mounts.into_iter().collect();
    for (i, (left_name, left)) in mounts.iter().enumerate() {
        for (right_name, right) in mounts.iter().skip(i + 1) {
            ensure!(
                !mounts_overlap(left, right),
                "Project mounts {} ({left_name}) and {} ({right_name}) overlap",
                left.as_internal_file_string(),
                right.as_internal_file_string()
            );
        }
    }
    Ok(())
}

fn list_native_projects(transaction: &Transaction) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    transaction.for_each_ref_prefixed("refs/jjosh/native/", |name, _| {
        let rest = name.strip_prefix("refs/jjosh/native/").unwrap_or(name);
        if let Some(project) = rest.split('/').next().filter(|name| !name.is_empty()) {
            names.insert(project.to_owned());
        }
        Ok(())
    })?;
    Ok(names.into_iter().collect())
}

pub(crate) fn load_mount(transaction: &Transaction, project: &str) -> Result<RepoPathBuf> {
    let Some(oid) = transaction.resolve_ref(&mount_ref_name(project))? else {
        return default_mount(project);
    };
    let bytes = josh_core::filter::tree::blob_bytes(transaction.odb(), oid)
        .ok_or_else(|| anyhow::anyhow!("Native mount ref for {project} is not a blob"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|err| anyhow::anyhow!("Native mount ref for {project} is not UTF-8: {err}"))?;
    parse_mount(text)
}

pub(crate) fn record_mount(
    transaction: &Transaction,
    project: &str,
    mount: &RepoPath,
) -> Result<()> {
    if let Some(other) = native_project_for_mount(transaction, mount)? {
        ensure!(
            other == project,
            "Mount {} is already used by native project {other}",
            mount.as_internal_file_string()
        );
    }
    let blob = josh_core::objects::write_blob(
        transaction.odb(),
        mount.as_internal_file_string().as_bytes(),
    )?;
    let name = mount_ref_name(project);
    let previous = transaction.resolve_ref(&name)?;
    if let Some(old) = previous {
        ensure!(
            old == blob,
            "Native project {project} is already mounted at {}",
            load_mount(transaction, project)?.as_internal_file_string()
        );
        return Ok(());
    }
    transaction.update_ref(&name, Expected::Absent, blob, "record native project mount")
}

pub(crate) fn native_project_for_mount(
    transaction: &Transaction,
    mount: &RepoPath,
) -> Result<Option<String>> {
    let mut matched = None;
    for project in list_native_projects(transaction)? {
        if load_mount(transaction, &project)?.as_ref() == mount {
            ensure!(
                matched.as_ref().is_none_or(|previous| previous == &project),
                "Mount {} is claimed by multiple native projects",
                mount.as_internal_file_string()
            );
            matched = Some(project);
        }
    }
    Ok(matched)
}

async fn value_at_path(
    store: &Store,
    tree_id: &TreeId,
    path: &RepoPath,
) -> Result<Option<TreeValue>> {
    ensure!(
        !path.is_root(),
        "Project mount cannot be the repository root"
    );
    let mut current = tree_id.clone();
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate() {
        let tree = store
            .backend()
            .read_tree(RepoPath::root(), &current)
            .await?;
        match tree.value(component) {
            None => return Ok(None),
            Some(value) if index + 1 == components.len() => return Ok(Some(value.clone())),
            Some(TreeValue::Tree(id)) => current = id.clone(),
            Some(value) => return Ok(Some(value.clone())),
        }
    }
    Ok(None)
}

pub(crate) async fn commit_path_occupied(commit: &Commit, path: &RepoPath) -> Result<bool> {
    let store = commit.store().as_ref();
    for id in commit.tree_ids().iter() {
        if value_at_path(store, id, path).await?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn commit_has_project_tree(commit: &Commit, mount: &RepoPath) -> Result<bool> {
    let store = commit.store().as_ref();
    let mut present = false;
    for id in commit.tree_ids().iter() {
        match value_at_path(store, id, mount).await? {
            Some(TreeValue::Tree(_)) => present = true,
            Some(_) => anyhow::bail!(
                "Project mount {} is not a directory in {}",
                mount.as_internal_file_string(),
                commit.id()
            ),
            None => {}
        }
    }
    Ok(present)
}

async fn tree_id_at_path(store: &Store, tree_id: &TreeId, path: &RepoPath) -> Result<TreeId> {
    match value_at_path(store, tree_id, path).await? {
        Some(TreeValue::Tree(id)) => Ok(id),
        None => Ok(store.empty_tree_id().clone()),
        Some(_) => anyhow::bail!(
            "Project mount {} is not a directory",
            path.as_internal_file_string()
        ),
    }
}

pub(crate) async fn prefix_tree(store: &Store, mount: &RepoPath, inner: &TreeId) -> Result<TreeId> {
    ensure!(
        !mount.is_root(),
        "Project mount cannot be the repository root"
    );
    if inner == store.empty_tree_id() {
        return Ok(inner.clone());
    }
    let mut current = inner.clone();
    for component in mount.components().rev() {
        let tree =
            Tree::from_sorted_entries(vec![(component.to_owned(), TreeValue::Tree(current))]);
        current = store.backend().write_tree(RepoPath::root(), &tree).await?;
    }
    Ok(current)
}

async fn without_path(store: &Store, tree_id: &TreeId, path: &RepoPath) -> Result<TreeId> {
    let empty = store.empty_tree_id();
    let components: Vec<_> = path.components().collect();
    ensure!(
        !components.is_empty(),
        "Project mount cannot be the repository root"
    );
    if tree_id == empty {
        return Ok(empty.clone());
    }
    let mut current = tree_id.clone();
    let mut stack = Vec::with_capacity(components.len());
    for (index, component) in components.iter().enumerate() {
        let tree = store
            .backend()
            .read_tree(RepoPath::root(), &current)
            .await?;
        let last = index + 1 == components.len();
        let child = match tree.value(component) {
            Some(TreeValue::Tree(id)) if !last => Some(id.clone()),
            Some(TreeValue::Tree(_)) if last => None,
            None if last => return Ok(tree_id.clone()),
            None => return Ok(tree_id.clone()),
            Some(_) if last => None,
            Some(_) => anyhow::bail!(
                "Project mount is not a directory at {}",
                component.as_internal_str()
            ),
        };
        stack.push((tree, *component, last));
        if let Some(id) = child {
            current = id;
        }
    }
    let mut rewritten: Option<TreeId> = None;
    while let Some((tree, component, last)) = stack.pop() {
        let mut entries = Vec::new();
        for entry in tree.entries() {
            if entry.name() != component {
                entries.push((entry.name().to_owned(), entry.value().clone()));
                continue;
            }
            if last {
                continue;
            }
            if let Some(child) = &rewritten {
                if child != empty {
                    entries.push((entry.name().to_owned(), TreeValue::Tree(child.clone())));
                }
            }
        }
        rewritten = Some(if entries.is_empty() {
            empty.clone()
        } else {
            store
                .backend()
                .write_tree(RepoPath::root(), &Tree::from_sorted_entries(entries))
                .await?
        });
    }
    Ok(rewritten.unwrap_or_else(|| tree_id.clone()))
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
        let mount = load_mount(transaction, project)?;
        for head in repo.view().heads() {
            let commit = repo.store().get_commit_async(head).await?;
            if commit_path_occupied(&commit, &mount).await? {
                present = true;
                break;
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
    mount: &RepoPath,
    source: &backend::Commit,
) -> Result<Option<CommitId>> {
    let Some(targets) = repo.resolve_change_id(&source.change_id).await? else {
        return Ok(None);
    };
    let mut matched: Option<CommitId> = None;
    for (id, state) in &targets.targets {
        if *state != ResolvedChangeState::Visible {
            continue;
        }
        let commit = repo.store().get_commit_async(id).await?;
        if !commit_has_project_tree(&commit, mount).await? {
            continue;
        }
        if commit.description() != source.description
            || commit.author() != &source.author
            || commit.committer() != &source.committer
        {
            continue;
        }
        let projected = project_tree(&commit, mount).await?;
        if projected.tree_ids() != &source.root_tree {
            continue;
        }
        if let Some(previous) = &matched {
            ensure!(
                previous == id,
                "Change {} has multiple matching {} versions {} and {}",
                source.change_id.hex(),
                mount.as_internal_file_string(),
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
    mount: &RepoPath,
    incoming: &backend::Commit,
) -> Result<MergedTree> {
    let store = repo.store();
    let mut parents = Vec::with_capacity(incoming.parents.len());
    for id in &incoming.parents {
        parents.push(store.get_commit_async(id).await?);
    }
    let parent_tree = jj_lib::rewrite::merge_commit_trees(repo, &parents).await?;
    let mut outside = HashMap::new();
    for id in parent_tree.tree_ids().iter() {
        if !outside.contains_key(id) {
            let id_out = without_path(store.as_ref(), id, mount).await?;
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

pub(crate) async fn project_tree(commit: &Commit, mount: &RepoPath) -> Result<MergedTree> {
    let store = commit.store();
    let mut terms = HashMap::new();
    for id in commit.tree_ids().iter() {
        if terms.contains_key(id) {
            continue;
        }
        terms.insert(
            id.clone(),
            tree_id_at_path(store.as_ref(), id, mount).await?,
        );
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

pub(crate) async fn export_project(
    repo: &dyn Repo,
    mount: &RepoPath,
    head: &Commit,
    known: &HashMap<CommitId, CommitId>,
) -> Result<(CommitId, Vec<(CommitId, CommitId)>)> {
    ensure!(
        !project_tree(head, mount).await?.has_conflict(),
        "Project mount {} has unresolved conflicts at the publication revision",
        mount.as_internal_file_string()
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
                            tree: project_tree(&commit, mount).await?.tree_ids().clone(),
                        },
                    );
                    continue;
                }
                let parents = commit.parent_ids().to_vec();
                pending.push(Visit::Write(commit));
                pending.extend(parents.into_iter().map(Visit::Read));
            }
            Visit::Write(commit) => {
                let tree = project_tree(&commit, mount).await?;
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
