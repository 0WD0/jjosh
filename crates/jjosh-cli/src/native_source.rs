use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Seek as _;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use jj_lib::backend::Backend as _;
use jj_lib::backend::CommitId;
use jj_lib::backend::TreeValue;
use jj_lib::backend::{self};
use jj_lib::git_backend::GitBackend;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::View;
use jj_lib::repo::RepoLoader;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::repo_path::RepoPathComponentBuf;
use jj_lib::store::Store;

/// A recorded local native state and its authoritative commit metadata.
pub(crate) struct NativeSource {
    pub store: Arc<Store>,
    pub view: View,
    pub commits: HashMap<CommitId, backend::Commit>,
}

impl NativeSource {
    pub async fn read(loader: &RepoLoader, bookmark: Option<&str>) -> Result<Self> {
        Self::read_with(loader, |view| {
            if let Some(name) = bookmark {
                let target = view
                    .local_bookmarks
                    .get(jj_lib::ref_name::RefName::new(name))
                    .with_context(|| format!("Source has no local bookmark {name:?}"))?
                    .clone();
                *view = View::make_root(loader.store().root_commit_id().clone());
                view.head_ids.extend(target.added_ids().cloned());
                view.local_bookmarks.insert(name.into(), target);
            }
            Ok(())
        })
        .await
    }

    /// Capture only selected recorded local refs and their ancestry. In particular,
    /// fetching a workspace never snapshots it or imports its working-copy roles,
    /// foreign remote observations, or unrelated visible heads.
    pub async fn read_selected(
        loader: &RepoLoader,
        selection: &jj_lib::git::GitFetchRefExpression,
    ) -> Result<Self> {
        let bookmarks = selection.bookmark.to_matcher();
        let tags = selection.tag.to_matcher();
        Self::read_with(loader, |view| {
            let mut selected = View::make_root(loader.store().root_commit_id().clone());
            selected.local_bookmarks = std::mem::take(&mut view.local_bookmarks)
                .into_iter()
                .filter(|(name, _)| bookmarks.is_match(name.as_str()))
                .collect();
            selected.local_tags = std::mem::take(&mut view.local_tags)
                .into_iter()
                .filter(|(name, _)| tags.is_match(name.as_str()))
                .collect();
            for target in selected
                .local_bookmarks
                .values()
                .chain(selected.local_tags.values())
            {
                selected.head_ids.extend(target.added_ids().cloned());
            }
            *view = selected;
            Ok(())
        })
        .await
    }

    async fn read_with(
        loader: &RepoLoader,
        select: impl FnOnce(&mut View) -> Result<()>,
    ) -> Result<Self> {
        let heads = jj_lib::op_walk::get_current_head_ops(
            loader.op_store(),
            loader.op_heads_store().as_ref(),
        )
        .await?;
        let [operation] = heads.as_slice() else {
            anyhow::bail!(
                "Native source has multiple operation heads; select/reconcile its state before \
                 importing"
            );
        };
        let backend = jj_lib::git::get_git_backend(loader.store())?;
        ensure!(
            backend.commit_id_length() == 20,
            "Native sources require Git SHA-1"
        );
        backend.disable_lazy_commit_imports();
        let mut view = operation.view().await?.store_view().clone();
        select(&mut view)?;
        let source = Self::read_view(loader.store().clone(), view).await?;
        let current = jj_lib::op_walk::get_current_head_ops(
            loader.op_store(),
            loader.op_heads_store().as_ref(),
        )
        .await?;
        ensure!(
            current.len() == 1 && current[0].id() == operation.id(),
            "Native source changed during capture; retry from a stable recorded operation"
        );
        Ok(source)
    }

    pub async fn read_view(store: Arc<Store>, view: View) -> Result<Self> {
        let native_view = jj_lib::view::View::new(view.clone(), false);
        let mut pending: Vec<_> = native_view.all_referenced_commit_ids().cloned().collect();
        pending.push(store.root_commit_id().clone());
        let mut commits = HashMap::new();
        while let Some(id) = pending.pop() {
            if commits.contains_key(&id) {
                continue;
            }
            let mut commit = store
                .backend()
                .read_commit(&id)
                .await
                .with_context(|| format!("Reading native source commit {id}"))?;
            commit.predecessors.clear();
            pending.extend(commit.parents.iter().cloned());
            commits.insert(id, commit);
        }
        let root = store.backend().read_commit(store.root_commit_id()).await?;
        validate_graph(&view, &commits, store.root_commit_id(), &root)?;
        validate_trees(&store, &commits).await?;
        Ok(Self {
            store,
            view,
            commits,
        })
    }

    /// Copy the immutable original objects as well as every native conflict
    /// term. Original Git ancestry is also used to recover native import
    /// correspondences later.
    pub fn copy_objects_to(&self, destination: &GitBackend) -> Result<()> {
        let source = jj_lib::git::get_git_backend(&self.store)?;
        if source.git_repo_path() == destination.git_repo_path() {
            return Ok(());
        }
        let roots = object_roots(&self.commits, self.store.root_commit_id());
        if roots.is_empty() {
            return Ok(());
        }
        let mut input = tempfile::tempfile()?;
        for root in &roots {
            writeln!(input, "{root}")?;
        }
        input.rewind()?;
        let mut pack = tempfile::tempfile()?;
        run_git(
            git_command(source.git_repo_path())
                .args(["pack-objects", "--stdout", "--revs", "--no-reuse-delta"])
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(pack.try_clone()?)),
            "copying native source objects",
        )?;
        pack.rewind()?;
        run_git(
            git_command(destination.git_repo_path())
                .args(["index-pack", "--stdin", "--strict"])
                .stdin(Stdio::from(pack))
                .stdout(Stdio::null()),
            "installing native source objects",
        )
    }
}

/// No inherited Git environment, global config, replace refs, lazy network
/// fetches, or hooks may influence object copying. The source Git directory is
/// read only; objects are installed only in the destination Git directory.
fn git_command(path: &Path) -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("--git-dir")
        .arg(path)
        .args(["-c", "core.hooksPath=/dev/null"]);
    command
}

fn run_git(command: &mut Command, action: &str) -> Result<()> {
    let output = command
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("{action}: running git"))?;
    ensure!(
        output.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn object_roots(
    commits: &HashMap<CommitId, backend::Commit>,
    root_id: &CommitId,
) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    for (id, commit) in commits {
        if id == root_id {
            // The root and its empty tree are native synthetic objects. Every
            // real commit's terms, including empty terms, are physical roots.
            continue;
        }
        roots.insert(id.hex());
        for tree in commit.root_tree.iter() {
            roots.insert(tree.hex());
        }
    }
    roots
}

/// Validate the entire current parent closure, including hidden ref terms and
/// Git observations. Reject dangling extras and cycles in the recorded source.
fn validate_graph(
    view: &View,
    commits: &HashMap<CommitId, backend::Commit>,
    root_id: &CommitId,
    root: &backend::Commit,
) -> Result<()> {
    ensure!(
        commits.get(root_id) == Some(root),
        "source must contain the canonical synthetic root commit {root_id}"
    );
    let native_view = jj_lib::view::View::new(view.clone(), false);
    let mut pending: Vec<_> = native_view
        .all_referenced_commit_ids()
        .map(|id| (id.clone(), false))
        .collect();
    pending.push((root_id.clone(), false));
    let mut active = HashSet::new();
    let mut done = HashSet::new();
    while let Some((id, exit)) = pending.pop() {
        if exit {
            active.remove(&id);
            done.insert(id);
            continue;
        }
        if done.contains(&id) {
            continue;
        }
        ensure!(
            active.insert(id.clone()),
            "cyclic native parent graph at {id}"
        );
        let commit = commits
            .get(&id)
            .with_context(|| format!("missing referenced native commit {id}"))?;
        if &id != root_id {
            ensure!(
                !commit.parents.is_empty(),
                "non-root commit {id} has no parents"
            );
            ensure!(
                commit.parents.len() == 1 || !commit.parents.contains(root_id),
                "commit {id} mixes synthetic root with other parents"
            );
        }
        pending.push((id, true));
        pending.extend(commit.parents.iter().rev().map(|id| (id.clone(), false)));
    }
    ensure!(
        done.len() == commits.len(),
        "source contains commits outside the recorded view's parent closure"
    );
    Ok(())
}

async fn validate_trees(
    store: &Arc<Store>,
    commits: &HashMap<CommitId, backend::Commit>,
) -> Result<()> {
    let backend = jj_lib::git::get_git_backend(store)?;
    let raw_repo = backend.git_repo();
    let mut pending: Vec<_> = commits
        .values()
        .flat_map(|commit| commit.root_tree.iter())
        .map(|id| (RepoPathBuf::root(), id.clone()))
        .collect();
    let mut seen = HashSet::new();
    let mut blobs = BTreeSet::new();
    while let Some((path, id)) = pending.pop() {
        if !seen.insert(id.clone()) || id == *backend.empty_tree_id() {
            continue;
        }
        // Validate components before the backend's infallible native conversion.
        let raw_tree = raw_repo
            .find_object(gix_hash::ObjectId::from_hex(id.hex().as_bytes())?)?
            .try_into_tree()?;
        let mut names = HashSet::new();
        for entry in raw_tree.iter() {
            let entry = entry?;
            let name = std::str::from_utf8(entry.filename())?;
            RepoPathComponentBuf::new(name)?;
            ensure!(
                names.insert(name.to_owned()),
                "duplicate tree entry at {path:?}"
            );
        }
        let tree = backend.read_tree(&path, &id).await?;
        for entry in tree.entries() {
            let entry_path = path.join(entry.name());
            match entry.value() {
                TreeValue::Tree(child) => pending.push((entry_path, child.clone())),
                TreeValue::File { id, copy_id, .. } => {
                    ensure!(
                        copy_id.as_bytes().is_empty(),
                        "tracked copy metadata at {entry_path:?} cannot be preserved"
                    );
                    if blobs.insert(id.hex()) {
                        let object = raw_repo
                            .find_object(gix_hash::ObjectId::from_hex(id.hex().as_bytes())?)?;
                        object
                            .try_into_blob()
                            .with_context(|| format!("file at {entry_path:?} is not a blob"))?;
                    }
                }
                TreeValue::Symlink(id) => {
                    // Native symlinks require UTF-8, unlike ordinary blob data.
                    backend.read_symlink(&entry_path, id).await?;
                }
                TreeValue::GitSubmodule(_) => {} // Foreign object, not a missing local commit.
            }
        }
    }
    Ok(())
}
