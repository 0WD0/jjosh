//! Subtree preview in an ephemeral Josh object database, without writing JJ objects.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use jj_lib::backend::{CommitId, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use josh_core::cache::Transaction;

async fn subtree(commit: &Commit, mount: &RepoPath) -> Result<MergedTree> {
    let store = commit.store();
    let mut terms = HashMap::new();
    for id in commit.tree_ids().iter() {
        if terms.contains_key(id) {
            continue;
        }
        let tree = match super::value_at_path(store, id, mount).await? {
            Some(TreeValue::Tree(tree)) => tree,
            // Josh's subdir filter selects no content when the path is absent or a file.
            _ => store.empty_tree_id().clone(),
        };
        terms.insert(id.clone(), tree);
    }
    let projected = commit.tree_ids().map(|id| terms[id].clone());
    let tree = projected
        .resolve_trivial(store.merge_options().same_change)
        .with_context(|| {
            format!(
                "Revision {} contains a native conflict in projection path {}; resolve it before previewing this history",
                commit.id(),
                mount.as_internal_file_string()
            )
        })?;
    // Do not call MergedTree::resolve(): resolving a merge can persist trees and blobs.
    Ok(MergedTree::resolved(store.clone(), tree.clone()))
}

pub(crate) async fn project(
    repo: &dyn Repo,
    transaction: &Transaction,
    commit: &Commit,
    mount: &RepoPath,
) -> Result<gix_hash::ObjectId> {
    let root = repo.store().root_commit_id();
    let projected = super::history::project_with_tree(
        repo,
        std::slice::from_ref(commit.id()),
        |_| None,
        async |commit| subtree(commit, mount).await,
        |commit, tree, parents| {
            let source = gix_hash::ObjectId::try_from(commit.id().as_bytes())?;
            let source = josh_core::objects::CommitData::read(transaction.odb(), source)?;
            let tree = tree
                .tree_ids()
                .as_resolved()
                .expect("preview subtree is resolved");
            let tree = tree.hex();
            let parents: Vec<_> = parents
                .iter()
                .filter(|id| *id != root)
                .map(|id| id.hex())
                .collect();
            let mut contents = source.parsed()?;
            contents.tree = tree.as_bytes().into();
            contents.parents = parents.iter().map(|id| id.as_bytes().into()).collect();
            // Keep logical identity, but not signatures, old conflict trees or transport metadata.
            contents
                .extra_headers
                .retain(|(name, _)| *name == b"change-id");
            let id = gix_object::Write::write(transaction.odb(), &contents)?;
            Ok((CommitId::from_bytes(id.as_bytes()), None::<()>))
        },
    )
    .await?;
    let id = &projected.nodes[projected.heads[0]].id;
    if id == root {
        Ok(gix_hash::ObjectId::null(gix_hash::Kind::Sha1))
    } else {
        Ok(gix_hash::ObjectId::try_from(id.as_bytes())?)
    }
}
