use std::collections::HashMap;
use std::io::Seek as _;
use std::io::Write as _;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use jj_lib::backend::Backend as _;
use jj_lib::backend::CommitId;
use jj_lib::backend::{self};
use jj_lib::git_backend::GitBackend;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::OpStore;
use jj_lib::op_store::View;
use jj_lib::repo::RepoLoader;
use jj_lib::store::Store;

/// A recorded native state. The commit reader is authoritative even when the
/// content store came from a transport that cannot represent native metadata.
pub(crate) struct NativeSource {
    pub store: Arc<Store>,
    pub op_store: Arc<dyn OpStore>,
    pub view: View,
    pub commits: HashMap<CommitId, backend::Commit>,
    pub source_operation: String,
    pub _temp: Option<tempfile::TempDir>,
}

impl NativeSource {
    pub async fn read(loader: &RepoLoader, bookmark: Option<&str>) -> Result<Self> {
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
        if let Some(name) = bookmark {
            let target = view
                .local_bookmarks
                .get(jj_lib::ref_name::RefName::new(name))
                .with_context(|| format!("Source has no local bookmark {name:?}"))?
                .clone();
            view = View::make_root(loader.store().root_commit_id().clone());
            view.head_ids.extend(target.added_ids().cloned());
            view.local_bookmarks.insert(name.into(), target);
        }
        let source = Self::read_view(
            loader.store().clone(),
            loader.op_store().clone(),
            view,
            operation.id().hex(),
        )
        .await?;
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

    pub async fn read_view(
        store: Arc<Store>,
        op_store: Arc<dyn OpStore>,
        view: View,
        source_operation: String,
    ) -> Result<Self> {
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
        crate::native_bundle::validate_graph(&view, &commits, &root)?;
        crate::native_bundle::validate_trees(&store, &commits).await?;
        Ok(Self {
            store,
            op_store,
            view,
            commits,
            source_operation,
            _temp: None,
        })
    }

    /// Copy the immutable original objects as well as every native conflict
    /// term. No bundle or intermediate repository is required. Original Git
    /// ancestry is also used to recover native import correspondences later.
    pub fn copy_objects_to(&self, destination: &GitBackend) -> Result<()> {
        let source = jj_lib::git::get_git_backend(&self.store)?;
        if source.git_repo_path() == destination.git_repo_path() {
            return Ok(());
        }
        let roots = crate::native_bundle::object_roots(&self.commits);
        if roots.is_empty() {
            return Ok(());
        }
        let mut input = tempfile::tempfile()?;
        for root in roots.keys() {
            writeln!(input, "{root}")?;
        }
        input.rewind()?;
        let mut pack = tempfile::tempfile()?;
        crate::native_bundle::run_git(
            crate::native_bundle::git_command(source.git_repo_path())
                .args(["pack-objects", "--stdout", "--revs", "--no-reuse-delta"])
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(pack.try_clone()?)),
            "copying native source objects",
        )?;
        pack.rewind()?;
        crate::native_bundle::run_git(
            crate::native_bundle::git_command(destination.git_repo_path())
                .args(["index-pack", "--stdin", "--strict"])
                .stdin(Stdio::from(pack))
                .stdout(Stdio::null()),
            "installing native source objects",
        )
    }
}
