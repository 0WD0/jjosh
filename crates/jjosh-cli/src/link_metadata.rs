use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::anyhow;
use jj_lib::object_id::ObjectId as _;
use josh_core::filter::Filter;
use josh_core::filter::tree;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkMode {
    Embedded,
    Snapshot,
}

impl LinkMode {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "embedded" => Ok(Self::Embedded),
            "snapshot" => Ok(Self::Snapshot),
            _ => Err(anyhow!(
                "Unknown native link mode: {value:?}; expected embedded or snapshot"
            )),
        }
    }
}

impl std::fmt::Display for LinkMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Embedded => "embedded",
            Self::Snapshot => "snapshot",
        })
    }
}

/// Discover metadata without silently dropping links whose marker cannot be read or parsed.
pub(crate) fn find_link_files(
    odb: &impl gix_object::Find,
    tree: gix_hash::ObjectId,
) -> anyhow::Result<Vec<(PathBuf, Filter)>> {
    let mut links = Vec::new();
    let mut buffer = Vec::new();
    josh_core::objects::walk_tree_preorder(odb, tree, &mut |root, entry| {
        if &entry.filename[..] != b".link.josh" {
            return Ok(());
        }
        let path = PathBuf::from(root);
        let marker = path.join(".link.josh");
        let data = odb
            .try_find(entry.oid, &mut buffer)
            .map_err(|err| anyhow!("Failed to read '{}': {err}", marker.display()))?
            .ok_or_else(|| {
                anyhow!(
                    "Failed to read '{}': object {} not found",
                    marker.display(),
                    entry.oid
                )
            })?;
        if data.kind != gix_object::Kind::Blob {
            return Err(anyhow!(
                "Link metadata '{}' is not a blob",
                marker.display()
            ));
        }
        let content = std::str::from_utf8(data.data)
            .with_context(|| format!("Link metadata '{}' is not valid UTF-8", marker.display()))?;
        let filter = josh_core::filter::parse(content.trim())
            .with_context(|| format!("Failed to parse link metadata '{}'", marker.display()))?;
        links.push((path, filter));
        Ok(())
    })
    .context("Failed to discover link metadata")?;
    Ok(links)
}

/// Resolve link configuration independently of conflicts in ordinary project files.
pub(crate) fn find_native_link_files(
    odb: &impl gix_object::Find,
    commit: &jj_lib::commit::Commit,
) -> anyhow::Result<Vec<(PathBuf, Filter)>> {
    if let Some(id) = commit.tree_ids().as_resolved() {
        return find_link_files(odb, gix_hash::ObjectId::try_from(id.as_bytes())?);
    }
    let mut terms = HashMap::new();
    let mut paths = BTreeSet::new();
    for id in commit.tree_ids().iter() {
        if terms.contains_key(id) {
            continue;
        }
        let links: BTreeMap<_, _> =
            find_link_files(odb, gix_hash::ObjectId::try_from(id.as_bytes())?)?
                .into_iter()
                .collect();
        paths.extend(links.keys().cloned());
        terms.insert(id.clone(), links);
    }
    let mut links = Vec::new();
    for path in paths {
        let value = commit.tree_ids().map(|id| terms[id].get(&path).copied());
        let resolved = value
            .resolve_trivial(jj_lib::merge::SameChange::Accept)
            .ok_or_else(|| {
                anyhow!(
                    "Resolve conflicted link configuration at '{}'",
                    path.display()
                )
            })?;
        if let Some(filter) = resolved {
            links.push((path, *filter));
        }
    }
    Ok(links)
}

/// Write a compatible `.link.josh` marker without creating a commit or materializing contents.
pub(crate) fn prepare_link_add(
    transaction: &josh_core::cache::Transaction,
    path: &Path,
    name: &str,
    url: &str,
    push_url: Option<&str>,
    filter: Option<&str>,
    target: &str,
    push_target: Option<&str>,
    fetched_commit: gix_hash::ObjectId,
    head_tree: gix_hash::ObjectId,
    mode: LinkMode,
) -> anyhow::Result<gix_hash::ObjectId> {
    let odb = transaction.odb();
    let path = path.strip_prefix("/").unwrap_or(path);
    let filter = filter.unwrap_or(":/");
    let filter = josh_core::filter::parse(filter)
        .with_context(|| format!("Failed to parse filter '{filter}'"))?
        .prefix(path);
    let mut link_filter = filter
        .with_meta("name", name.to_string())
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
    let content = josh_core::filter::as_file(link_filter, 0);
    let blob = josh_core::objects::write_blob(odb, content.as_bytes())?;
    let marker = path.join(".link.josh");
    tree::insert_oid(odb, head_tree, &marker, blob, 0o0100644)
        .with_context(|| format!("Failed to insert link metadata '{}'", marker.display()))
}

/// A link export ready to push to its configured destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLinkPush {
    pub(crate) push_remote: Option<String>,
    pub(crate) configured_target: String,
    pub(crate) configured_push_target: Option<String>,
    pub(crate) exported_commit: gix_hash::ObjectId,
}

/// Export visible link history while preserving pinned ancestry and surviving merges.
pub(crate) fn prepare_link_push(
    transaction: &josh_core::cache::Transaction,
    head_commit: gix_hash::ObjectId,
    path: &Path,
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
    let link_files = find_link_files(transaction.odb(), head_tree)?;
    let (_, link_file) = link_files
        .iter()
        .find(|(candidate, _)| candidate == Path::new(normalized_path))
        .ok_or_else(|| anyhow!("No link found at path '{}'", path.display()))?;
    let source_remote = link_file
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
    // Fetch does not rewrite the versioned marker. Reverse filtering still
    // needs the latest explicitly observed source context to retain content
    // outside the projection, without querying the network during push.
    let original_target = if let Some(branch) = link_file
        .get_meta("source-branch")
        .filter(|branch| branch != "pinned")
    {
        let reference = crate::link_refs::push_tracking_ref(
            transaction,
            &source_remote,
            &format!("refs/heads/{branch}"),
        )
        .map_err(|err| anyhow!("Cannot identify source observation: {err:?}"))?;
        transaction
            .resolve_ref(&reference)?
            .unwrap_or(original_target)
    } else {
        original_target
    };
    let source_filter = link_file.peel();
    let old_filtered_commit = josh_core::filter_commit(transaction, source_filter, original_target)
        .context("Failed to filter the pinned link commit")?;
    let local_filter = Filter::new()
        .subdir(normalized_path)
        .exclude(Filter::new().file(".link.josh"))
        .prefix(normalized_path);
    let local_commit = josh_core::filter_commit(transaction, local_filter, head_commit)
        .context("Failed to isolate the local link history")?;
    if local_commit == gix_hash::ObjectId::null(gix_hash::Kind::Sha1) {
        return Err(anyhow!(
            "No content found at path '{}' to push",
            path.display()
        ));
    }
    // Snapshot roots can be unrelated even after embedding. Attach them to the pinned
    // source to preserve upstream ancestry and content outside the source filter.
    let exported_commit = josh_core::history::unapply_filter(
        transaction,
        source_filter,
        original_target,
        old_filtered_commit,
        local_commit,
        josh_core::history::UnapplyOptions {
            reparent_orphans: Some(original_target),
            prune_empty: true,
            ..Default::default()
        },
    )
    .context("Failed to reverse the linked history")?;

    Ok(PreparedLinkPush {
        push_remote,
        configured_target,
        configured_push_target,
        exported_commit,
    })
}
