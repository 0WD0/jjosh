use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::anyhow;
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

/// Write a compatible `.link.josh` marker without creating a commit or materializing contents.
pub(crate) fn prepare_link_add(
    transaction: &josh_core::cache::Transaction,
    path: &Path,
    name: &str,
    url: &str,
    push_url: Option<&str>,
    filter: Option<&str>,
    target: &str,
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
    let content = josh_core::filter::as_file(link_filter, 0);
    let blob = josh_core::objects::write_blob(odb, content.as_bytes())?;
    let marker = path.join(".link.josh");
    tree::insert_oid(odb, head_tree, &marker, blob, 0o0100644)
        .with_context(|| format!("Failed to insert link metadata '{}'", marker.display()))
}
/// The local-side projection used to compare link content with a fetched source.
pub(crate) fn local_link_filter(path: &Path) -> anyhow::Result<Filter> {
    let normalized_path = path
        .to_str()
        .ok_or_else(|| anyhow!("Link path is not valid UTF-8: '{}'", path.display()))?
        .trim_matches('/');
    if normalized_path.is_empty() {
        return Err(anyhow!("Path cannot be empty"));
    }
    Ok(Filter::new()
        .subdir(normalized_path)
        .exclude(Filter::new().file(".link.josh"))
        .prefix(normalized_path))
}
