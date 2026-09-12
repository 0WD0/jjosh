use anyhow::Context;

use josh_core::filter::{self, Filter, flatten_chain};

use crate::porcelain::RefUpdate;

/// Convert a filesystem remote URL relative to the caller's working directory.
pub fn to_absolute_remote_url(url: &str) -> anyhow::Result<String> {
    let parsed = gix::url::parse(url).context("Invalid Git remote URL")?;
    if parsed.scheme != gix::url::Scheme::File
        || url
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
    {
        Ok(url.to_owned())
    } else {
        // Git rejects Windows extended-length paths inside file:// URLs.
        #[cfg(windows)]
        let canonical = dunce::canonicalize(url);
        #[cfg(not(windows))]
        let canonical = std::fs::canonicalize(url);
        let canonical = canonical.with_context(|| format!("Failed to resolve path {}", url))?;

        let url = url::Url::from_file_path(&canonical).map_err(|_| {
            anyhow::anyhow!(
                "Path {} is not absolute or not convertible to a file URL",
                canonical.display()
            )
        })?;
        Ok(url.to_string())
    }
}

/// Add or update a Josh remote with real Git transport endpoints.
///
/// Source and push filesystem paths are resolved against the caller's working
/// directory. Ordinary Git commands bypass Josh's projection conversion.
pub fn configure_remote(
    repo_path: &std::path::Path,
    name: &str,
    url: &str,
    filter: &str,
    forge: Option<crate::config::Forge>,
    push_url: Option<&str>,
    gerrit_mode: Option<crate::config::GerritMode>,
) -> anyhow::Result<()> {
    let remote_url = to_absolute_remote_url(url)?;
    let push_url = push_url.map(to_absolute_remote_url).transpose()?;
    crate::config::write_remote_config(
        repo_path,
        name,
        &remote_url,
        filter,
        forge,
        push_url.as_deref(),
        gerrit_mode,
    )
    .context("Failed to configure Josh remote")?;
    Ok(())
}

/// Parse the symref output from `git ls-remote --symref` to extract the default branch.
/// Returns `(branch_name, full_ref)` e.g. `("master", "refs/remotes/origin/master")`.
pub fn try_parse_symref(remote: &str, output: &str) -> Option<(String, String)> {
    let line = output.lines().next()?;
    let symref_part = line.split('\t').next()?;

    let default_branch = symref_part.strip_prefix("ref: refs/heads/")?;
    let default_branch_ref = format!("refs/remotes/{}/{}", remote, default_branch);

    Some((default_branch.to_string(), default_branch_ref))
}

/// Query the remote's HEAD branch via `git ls-remote --symref`.
/// Falls back to `"master"` if the remote does not advertise a symref.
pub fn get_head_branch(
    url: &str,
    repo_path: &std::path::Path,
    remote_name: &str,
) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(["ls-remote", "--symref", url, "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git ls-remote")?;

    if output.status.success() {
        let text = String::from_utf8(output.stdout).context("Invalid ls-remote output")?;
        if let Some((branch, _)) = try_parse_symref(remote_name, &text) {
            return Ok(branch);
        }
    }

    Ok("master".to_string())
}

/// Resolve the default branch name from the locally stored
/// `refs/remotes/{remote_name}/HEAD` symref.
///
/// Returns an error if the symref cannot be resolved (e.g. the remote
/// has not been fetched yet or does not advertise HEAD).
pub fn resolve_default_branch(
    transaction: &josh_core::cache::Transaction,
    remote_name: &str,
) -> anyhow::Result<String> {
    let head_symref = format!("refs/remotes/{}/HEAD", remote_name);
    transaction
        .symref_target(&head_symref)
        .ok()
        .flatten()
        .and_then(|target| {
            target
                .strip_prefix(&format!("refs/remotes/{}/", remote_name))
                .map(|s| s.to_string())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not resolve default branch from '{}'. \
                 Has the remote been fetched?",
                head_symref
            )
        })
}

/// Return raw `(refname, oid)` pairs for all refs under
/// `refs/josh/remotes/{remote_name}/*`, suitable as input to
/// `josh_core::filter_refs`. An empty remote has no backing refs.
pub fn get_backing_refs(
    transaction: &josh_core::cache::Transaction,
    remote_name: &str,
) -> anyhow::Result<Vec<(String, gix_hash::ObjectId)>> {
    let mut input_refs = Vec::new();
    transaction.for_each_ref_prefixed(
        &format!("refs/josh/remotes/{}/", remote_name),
        |name, oid| {
            input_refs.push((name.to_string(), oid));
            Ok(())
        },
    )?;

    Ok(input_refs)
}

/// Build the ref-path prefix for step `step_idx` in a chain.
///
/// The path encodes the filter history newest-first so that each ref path
/// uniquely identifies both *what* was applied and *to what* it was applied:
///
/// - step 0 of [A, B, C] → `"{A_id}"`
/// - step 1 of [A, B, C] → `"{B_id}/{A_id}"`
/// - step 2 of [A, B, C] → `"{C_id}/{B_id}/{A_id}"`
pub fn step_ref_prefix(step_idx: usize, steps: &[Filter]) -> String {
    (0..=step_idx)
        .rev()
        .map(|i| steps[i].id().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Apply a Josh filter to backing refs and publish directly to
/// `refs/remotes/{remote_name}/*`, pruning branches no longer projected.
/// Also writes `refs/josh/filtered/` refs for the default branch and persists filter tree objects.
pub fn apply_josh_filtering(
    transaction: &josh_core::cache::Transaction,
    filter: josh_core::filter::Filter,
    remote_name: &str,
    default_branch: Option<&str>,
) -> anyhow::Result<Vec<RefUpdate>> {
    let prefix = format!("refs/josh/remotes/{}/", remote_name);

    let steps = flatten_chain(filter);

    // Seed with the raw backing refs
    let mut current_commits: Vec<(String, gix_hash::ObjectId)> =
        get_backing_refs(transaction, remote_name)?
            .into_iter()
            .map(|(refname, oid)| {
                let branch = refname
                    .strip_prefix(&prefix)
                    .unwrap_or(&refname)
                    .to_string();
                (branch, oid)
            })
            .collect();

    // Apply each step, writing filtered refs along the way
    for (step_idx, step_filter) in steps.iter().enumerate() {
        let (filtered, errors) =
            josh_core::filter_refs(transaction, *step_filter, &current_commits);

        if let Some(error) = errors.into_iter().next() {
            return Err(anyhow::anyhow!("josh filter error: {}", error.1));
        }

        // Persist the filter tree object to ODB so cache build can reconstruct it
        filter::as_tree(transaction, *step_filter)?;

        let prefix_path = step_ref_prefix(step_idx, &steps);
        let mut next_commits = Vec::new();

        for (branch_name, filtered_oid) in &filtered {
            if *filtered_oid == gix_hash::ObjectId::null(gix_hash::Kind::Sha1) {
                continue;
            }

            // Write refs/josh/filtered/ ref only for the default branch
            if Some(branch_name.as_str()) == default_branch {
                let filtered_ref =
                    format!("refs/josh/filtered/{}/heads/{}", prefix_path, branch_name);
                transaction
                    .update_ref(
                        &filtered_ref,
                        josh_core::cache::Expected::Any,
                        *filtered_oid,
                        "josh filter",
                    )
                    .with_context(|| format!("failed to write filtered ref '{}'", filtered_ref))?;
            }

            next_commits.push((branch_name.clone(), *filtered_oid));
        }

        current_commits = next_commits;
    }

    let canonical = format!("refs/remotes/{remote_name}/");
    let mut existing = std::collections::BTreeMap::new();
    // Symbolic refs are excluded by this iterator, preserving remote HEAD.
    transaction.for_each_ref_prefixed(&canonical, |name, id| {
        existing.insert(name.to_owned(), id);
        Ok(())
    })?;
    let mut updates = Vec::new();
    for (branch_name, new) in current_commits {
        let reference = format!("{canonical}{branch_name}");
        let old = existing.remove(&reference);
        let update = match old {
            Some(old) if old == new => continue,
            Some(old) if filter::is_ancestor_of(transaction, old, new)? => RefUpdate::FastForward {
                old,
                new,
                reference,
            },
            Some(old) => RefUpdate::Forced {
                old,
                new,
                reference,
            },
            None => RefUpdate::New { new, reference },
        };
        updates.push(update);
    }
    updates.extend(
        existing
            .into_iter()
            .map(|(reference, old)| RefUpdate::Deleted { old, reference }),
    );
    // Complete ancestry classification before staging any canonical ref edits.
    for update in &updates {
        use josh_core::cache::Expected;
        match update {
            RefUpdate::FastForward {
                old,
                new,
                reference,
            }
            | RefUpdate::Forced {
                old,
                new,
                reference,
            } => {
                transaction.update_ref(reference, Expected::At(*old), *new, "josh filter")?;
            }
            RefUpdate::New { new, reference } => {
                transaction.update_ref(reference, Expected::Absent, *new, "josh filter")?;
            }
            RefUpdate::Deleted { old, reference } => {
                transaction.delete_ref(reference, Expected::At(*old))?;
            }
            RefUpdate::Rejected { .. } => {
                unreachable!("local projection updates cannot be rejected")
            }
        }
    }
    Ok(updates)
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_remotes_are_not_resolved_as_local_paths() {
        for remote in [
            "git@example.invalid:org/repo.git",
            "git://example.invalid/repo.git",
        ] {
            assert_eq!(super::to_absolute_remote_url(remote).unwrap(), remote);
        }
    }
}
