use anyhow::Context;
use anyhow::anyhow;
use josh_core::filter::tree;

use std::collections::HashSet;
use std::path::PathBuf;

/// Prepared link addition, ready to be finalized
pub struct PreparedLinkAdd {
    tree_oid: gix_hash::ObjectId,
    path: PathBuf,
}

impl PreparedLinkAdd {
    pub fn into_commit(
        self,
        transaction: &josh_core::cache::Transaction,
        head_commit: gix_hash::ObjectId,
        signature: &gix_actor::Signature,
    ) -> anyhow::Result<gix_hash::ObjectId> {
        josh_core::objects::write_commit(
            transaction.odb(),
            self.tree_oid,
            &[head_commit],
            signature,
            signature,
            &format!("Add link: {}", self.path.display()),
        )
        .context("Failed to create commit")
    }

    /// Get the prepared tree OID without consuming the addition.
    pub fn tree_oid(&self) -> gix_hash::ObjectId {
        self.tree_oid
    }

    /// Get tree OID for custom commit creation
    ///
    /// This is used by josh-cq to add additional files before creating a commit
    pub fn into_tree_oid(self) -> gix_hash::ObjectId {
        self.tree_oid
    }
}

/// Result from updating links
pub struct UpdateLinksResult {
    /// Commit with updated .link.josh files
    pub commit_with_updates: gix_hash::ObjectId,
    /// Commit after applying :link filter
    pub filtered_commit: gix_hash::ObjectId,
}
/// An updated materialized link tree and the clean tree it replaces.
///
/// Callers can use the previous materialization as the merge base when preserving local
/// modifications on top of a newer linked snapshot.
pub struct MaterializedLinkUpdateResult {
    /// Commit produced by materializing the original link markers.
    pub previous_materialized_commit: gix_hash::ObjectId,
    /// Marker and materialized commits for the updated links.
    pub update: UpdateLinksResult,
}

/// A remote URL and commit SHA found in a `.link.josh` file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkRef {
    pub remote: String,
    pub commit: String,
}

/// Walk the entire commit history reachable from the given commit and collect
/// all (remote, commit) pairs found in any `.link.josh` file across all commits and trees.
pub fn collect_all_link_refs(
    transaction: &josh_core::cache::Transaction,
    commit: gix_hash::ObjectId,
) -> anyhow::Result<HashSet<LinkRef>> {
    // Apply a filter that keeps only .link.josh files. This prunes the history
    // to only commits that actually changed those files, so the revwalk below
    // visits far fewer commits on typical repositories.
    let link_file_filter =
        josh_core::filter::parse("::**/.link.josh").context("Failed to parse .link.josh filter")?;

    let filtered_commit = josh_core::filter_commit(transaction, link_file_filter, commit)
        .context("Failed to apply .link.josh filter")?;

    if filtered_commit == gix_hash::ObjectId::null(gix_hash::Kind::Sha1) {
        return Ok(HashSet::new());
    }

    let mut refs = HashSet::new();

    let odb = transaction.odb();
    let mut walk = josh_core::objects::RevWalk::new(odb);
    walk.push(filtered_commit)
        .context("Failed to push commit to revwalk")?;

    for oid in walk
        .into_topo_vec(|_| false)
        .context("Failed to walk history")?
    {
        let tree = josh_core::git::read_tree_id(odb, oid).context("Failed to get commit tree")?;

        let link_files =
            josh_core::link::find_link_files(odb, tree).context("Failed to find link files")?;

        for (_, filter) in link_files {
            if let (Some(remote), Some(commit)) =
                (filter.get_meta("remote"), filter.get_meta("commit"))
            {
                refs.insert(LinkRef { remote, commit });
            }
        }
    }

    Ok(refs)
}

pub fn make_signature(
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<gix_actor::Signature> {
    if let Ok(time) = std::env::var("JOSH_COMMIT_TIME") {
        Ok(gix_actor::Signature {
            name: "JOSH".into(),
            email: "josh@josh-project.dev".into(),
            time: gix_actor::date::Time {
                seconds: time.parse().context("Failed to parse JOSH_COMMIT_TIME")?,
                offset: 0,
            },
        })
    } else {
        transaction.signature().context("Failed to get signature")
    }
}

/// Prepare a link addition without creating a commit
pub fn prepare_link_add(
    transaction: &josh_core::cache::Transaction,
    path: &std::path::Path,
    url: &str,
    push_url: Option<&str>,
    filter: Option<&str>,
    target: &str,
    push_target: Option<&str>,
    fetched_commit: gix_hash::ObjectId,
    head_tree: gix_hash::ObjectId,
    mode: josh_core::filter::LinkMode,
) -> anyhow::Result<PreparedLinkAdd> {
    let odb = transaction.odb();

    // Strip leading slash if present (git tree paths are always relative)
    let path = path.strip_prefix("/").unwrap_or(path);
    let filter = filter.unwrap_or(":/");

    // Parse the filter
    let filter_obj = josh_core::filter::parse(filter)
        .with_context(|| format!("Failed to parse filter '{}'", filter))?;

    let filter_obj = filter_obj.prefix(&path);

    // Create a filter with metadata
    let mut link_filter = filter_obj
        .with_meta("remote", url.to_string())
        .with_meta("target", target.to_string())
        .with_meta("commit", fetched_commit.to_string())
        .with_meta("mode", mode.to_string());
    if let Some(push_url) = push_url {
        link_filter = link_filter.with_meta("push", push_url.to_string());
    }
    if let Some(push_target) = push_target {
        link_filter = link_filter.with_meta("push-target", push_target.to_string());
    }
    let link_content = josh_core::filter::as_file(link_filter, 0);

    let link_blob = josh_core::objects::write_blob(odb, link_content.as_bytes())?;
    let link_path = path.join(".link.josh");

    let new_tree = tree::insert_oid(odb, head_tree, &link_path, link_blob, 0o0100644)
        .context("Failed to insert link file into tree")?;

    Ok(PreparedLinkAdd {
        tree_oid: new_tree,
        path: path.to_path_buf(),
    })
}

pub fn update_links(
    transaction: &josh_core::cache::Transaction,
    head_commit: gix_hash::ObjectId,
    links_to_update: Vec<(PathBuf, gix_hash::ObjectId)>,
    signature: &gix_actor::Signature,
) -> anyhow::Result<Option<UpdateLinksResult>> {
    let odb = transaction.odb();
    let head_tree_id =
        josh_core::git::read_tree_id(odb, head_commit).context("Failed to get HEAD tree")?;

    // Find all link files to get their current metadata
    let link_files =
        josh_core::link::find_link_files(odb, head_tree_id).context("Failed to find link files")?;

    // Update the link files with new commit OIDs
    let mut updated_link_files: Vec<(PathBuf, josh_core::filter::Filter)> = Vec::new();
    for (path, new_oid) in &links_to_update {
        // Find the existing link file at this path
        let link_file = link_files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, lf)| lf)
            .ok_or_else(|| anyhow!("Link file not found at path '{}'", path.display()))?;

        // Update the link file with the new commit SHA
        let updated_link_file = link_file.with_meta("commit", new_oid.to_string());
        updated_link_files.push((path.clone(), updated_link_file));
    }

    // Create new tree with updated .link.josh files
    let mut new_tree = head_tree_id;
    for (path, link_file) in &updated_link_files {
        let link_content = josh_core::filter::as_file(*link_file, 0);
        let link_blob = josh_core::objects::write_blob(odb, link_content.as_bytes())?;
        let link_path = path.join(".link.josh");

        new_tree = tree::insert_oid(odb, new_tree, &link_path, link_blob, 0o0100644).with_context(
            || {
                format!(
                    "Failed to insert link file into tree at path '{}'",
                    path.display()
                )
            },
        )?;
    }

    if new_tree == head_tree_id {
        return Ok(None);
    }

    // Create a new commit with the updated tree
    let commit_with_updates = josh_core::objects::write_commit(
        odb,
        new_tree,
        &[head_commit],
        signature,
        signature,
        &format!(
            "Update links: {}",
            updated_link_files
                .iter()
                .map(|(p, _)| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .context("Failed to create commit")?;

    // Apply the :link filter to the new commit
    let link_filter = josh_core::filter::parse(":link").context("Failed to parse :link filter")?;

    let filtered_commit = josh_core::filter_commit(transaction, link_filter, commit_with_updates)
        .context("Failed to apply :link filter")?;

    Ok(Some(UpdateLinksResult {
        commit_with_updates,
        filtered_commit,
    }))
}

/// Update links in a tree where linked contents have already been materialized.
///
/// `update_links()` operates on the canonical marker-only representation. A native working copy
/// contains both each marker and its projected files, so remove those files with `:unlink` first
/// and then materialize both the old and updated markers. The old materialization is returned as
/// the merge base callers need to preserve local changes.
pub fn update_materialized_links(
    transaction: &josh_core::cache::Transaction,
    materialized_commit: gix_hash::ObjectId,
    links_to_update: Vec<(PathBuf, gix_hash::ObjectId)>,
    signature: &gix_actor::Signature,
) -> anyhow::Result<Option<MaterializedLinkUpdateResult>> {
    let unlink_filter =
        josh_core::filter::parse(":unlink").context("Failed to parse :unlink filter")?;
    let canonical_commit =
        josh_core::filter_commit(transaction, unlink_filter, materialized_commit)
            .context("Failed to canonicalize materialized links")?;
    let link_filter = josh_core::filter::parse(":link").context("Failed to parse :link filter")?;
    let previous_materialized_commit =
        josh_core::filter_commit(transaction, link_filter, canonical_commit)
            .context("Failed to materialize previous links")?;
    let update = update_links(transaction, canonical_commit, links_to_update, signature)?;
    Ok(update.map(|update| MaterializedLinkUpdateResult {
        previous_materialized_commit,
        update,
    }))
}

/// Export the current contents at `path` back through the inverse of `filter`.
///
/// Returns `None` when the path has no content in `head_commit`. Callers can then fetch the
/// configured remote instead. The exported commit is written to the transaction's object store
/// and is suitable for recording as a link's pinned commit.
pub fn export_link_source(
    transaction: &josh_core::cache::Transaction,
    head_commit: gix_hash::ObjectId,
    path: &std::path::Path,
    filter: &str,
) -> anyhow::Result<Option<gix_hash::ObjectId>> {
    let normalized_path = path
        .to_str()
        .ok_or_else(|| anyhow!("Link path is not valid UTF-8: '{}'", path.display()))?
        .trim_matches('/');
    if normalized_path.is_empty() {
        return Err(anyhow!("Path cannot be empty"));
    }

    let path_filter = josh_core::filter::Filter::new().subdir(normalized_path);
    let filter_obj = josh_core::filter::parse(filter)
        .with_context(|| format!("Failed to parse filter '{filter}'"))?;
    let combined_filter = path_filter.export()?.chain(
        josh_core::filter::invert(filter_obj)
            .with_context(|| format!("Filter '{filter}' has no inverse"))?,
    );
    let exported_commit = josh_core::filter_commit(transaction, combined_filter, head_commit)
        .context("Failed to export existing link contents")?;
    Ok(
        (exported_commit != gix_hash::ObjectId::null(gix_hash::Kind::Sha1))
            .then_some(exported_commit),
    )
}

/// A link export ready to push to its own remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLinkPush {
    pub remote: String,
    pub push_remote: Option<String>,
    pub configured_target: String,
    pub configured_push_target: Option<String>,
    pub exported_commit: gix_hash::ObjectId,
}

/// Resolve a link at `path` and export its visible contents into the linked repository's layout.
pub fn prepare_link_push(
    transaction: &josh_core::cache::Transaction,
    head_commit: gix_hash::ObjectId,
    path: &std::path::Path,
) -> anyhow::Result<PreparedLinkPush> {
    let normalized_path = path
        .to_str()
        .ok_or_else(|| anyhow!("Link path is not valid UTF-8: '{}'", path.display()))?
        .trim_matches('/');
    if normalized_path.is_empty() {
        return Err(anyhow!("Path cannot be empty"));
    }

    let head_tree = josh_core::git::read_tree_id(transaction.odb(), head_commit)
        .context("Failed to get commit tree")?;
    let link_path = PathBuf::from(normalized_path);
    let link_files = josh_core::link::find_link_files(transaction.odb(), head_tree)
        .context("Failed to find link files")?;
    let (_, link_file) = link_files
        .iter()
        .find(|(candidate, _)| candidate == &link_path)
        .ok_or_else(|| anyhow!("No link found at path '{}'", path.display()))?;
    let remote = link_file
        .get_meta("remote")
        .ok_or_else(|| anyhow!("Link file missing 'remote' metadata"))?;
    let configured_target = link_file
        .get_meta("target")
        .unwrap_or_else(|| "HEAD".to_string());
    let push_remote = link_file.get_meta("push");
    let configured_push_target = link_file.get_meta("push-target");
    let original_target = link_file
        .get_meta("commit")
        .ok_or_else(|| anyhow!("Link file missing 'commit' metadata"))?
        .parse::<gix_hash::ObjectId>()
        .context("Link file contains an invalid commit ID")?;
    let source_filter = link_file.peel();
    let old_filtered_commit = josh_core::filter_commit(transaction, source_filter, original_target)
        .context("Failed to filter the pinned link commit")?;
    let local_filter = josh_core::filter::Filter::new()
        .subdir(normalized_path)
        .exclude(josh_core::filter::Filter::new().file(".link.josh"))
        .prefix(normalized_path);
    let local_commit = josh_core::filter_commit(transaction, local_filter, head_commit)
        .context("Failed to isolate the local link history")?;
    if local_commit == gix_hash::ObjectId::null(gix_hash::Kind::Sha1) {
        return Err(anyhow!(
            "No content found at path '{}' to push",
            path.display()
        ));
    }
    let exported_commit = josh_core::history::unapply_filter(
        transaction,
        source_filter,
        original_target,
        old_filtered_commit,
        local_commit,
        josh_core::history::OrphansMode::Keep,
        None,
    )
    .context("Failed to reverse the linked history")?;

    Ok(PreparedLinkPush {
        remote,
        push_remote,
        configured_target,
        configured_push_target,
        exported_commit,
    })
}
