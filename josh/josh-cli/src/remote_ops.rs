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

fn validate_remote_name(name: &str) -> anyhow::Result<()> {
    // The name is both a single config-file component and an unquoted shell
    // argument in uploadpack. Git ref validation alone does not make it safe.
    anyhow::ensure!(
        !name.is_empty()
            && !name.starts_with('-')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
        "Invalid Josh remote name '{}': expected a single shell-safe name",
        name
    );
    // This validates the variable component shared by all generated ref paths.
    let reference = format!("refs/remotes/{name}/HEAD");
    gix::validate::reference::name(reference.as_str().into())
        .with_context(|| format!("Invalid Josh remote name '{}'", name))?;
    Ok(())
}

/// Add or update a Josh remote and its local, read-only namespace transport.
///
/// Source and push filesystem paths are resolved against the caller's working
/// directory. The Git remote exposes projected refs only; source publication
/// must go through Josh's reverse filtering rather than ordinary `git push`.
pub fn configure_remote(
    repo_path: &std::path::Path,
    name: &str,
    url: &str,
    filter: &str,
    forge: Option<crate::config::Forge>,
    push_url: Option<&str>,
    gerrit_mode: Option<crate::config::GerritMode>,
) -> anyhow::Result<()> {
    validate_remote_name(name)?;
    // The writer validates these too, but creates its directory first. Reject
    // invalid semantic input here before making any filesystem/config changes.
    let parsed_filter = josh_core::filter::parse(filter)
        .with_context(|| format!("Failed to parse filter '{}'", filter))?;
    for key in josh_changes::remote_config::TRANSPORT_META_KEYS {
        anyhow::ensure!(
            parsed_filter.get_meta(key).is_none(),
            "Filter must not set reserved meta key '{}': it is owned by the remote config",
            key
        );
    }
    let remote_url = to_absolute_remote_url(url)?;
    let push_url = push_url.map(to_absolute_remote_url).transpose()?;
    let repo = gix::open(repo_path).context("Failed to open repository")?;
    let workdir = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let repo_remote = to_absolute_remote_url(
        workdir
            .to_str()
            .context("Repository path is not valid UTF-8")?,
    )?;
    let source_refspec = format!("+refs/heads/*:refs/josh/remotes/{name}/*");
    crate::config::write_remote_config(
        repo_path,
        name,
        &remote_url,
        filter,
        &source_refspec,
        forge,
        push_url.as_deref(),
        gerrit_mode,
    )
    .context("Failed to write remote config file")?;

    let fetch_refspec = format!("+refs/heads/*:refs/remotes/{name}/*");
    let uploadpack = format!("env GIT_NAMESPACE=josh-{name} git upload-pack");
    for (key, value) in [
        ("receivepack", "false"),
        ("pushurl", repo_remote.as_str()),
        ("url", repo_remote.as_str()),
        ("fetch", fetch_refspec.as_str()),
        ("uploadpack", uploadpack.as_str()),
    ] {
        josh_core::git::GitCommand::new(
            repo.git_dir(),
            [
                "config",
                "--local",
                "--replace-all",
                &format!("remote.{name}.{key}"),
                value,
            ],
            std::iter::empty::<(&str, &str)>(),
        )
        .spawn()
        .with_context(|| format!("Failed to set remote {key}"))?;
    }
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
/// `josh_core::filter_refs`.  Errors if no refs are found.
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

    if input_refs.is_empty() {
        return Err(anyhow::anyhow!(
            "No remote references found for '{}'",
            remote_name
        ));
    }

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

/// Apply a josh filter to all refs under `refs/josh/remotes/{remote_name}/*` and write
/// the filtered commits to `refs/namespaces/josh-{remote_name}/refs/heads/*`.
/// Also writes `refs/josh/filtered/` refs for the default branch and persists filter tree objects.
/// Then runs `git fetch --porcelain {remote_name}` to expose them through the configured
/// remote, returning the ref updates reported by the fetch.
pub fn apply_josh_filtering(
    transaction: &josh_core::cache::Transaction,
    filter: josh_core::filter::Filter,
    remote_name: &str,
    default_branch: &str,
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
            if branch_name == default_branch {
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

    // A namespace is the current projection, not an append-only cache. Remove
    // branches that disappeared from the source or now project to no history.
    let namespace = format!("refs/namespaces/josh-{remote_name}/refs/heads/");
    let wanted: std::collections::HashSet<_> = current_commits
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let mut stale = Vec::new();
    transaction.for_each_ref_prefixed(&namespace, |name, id| {
        if let Some(branch) = name.strip_prefix(&namespace)
            && !wanted.contains(branch)
        {
            stale.push((name.to_owned(), id));
        }
        Ok(())
    })?;
    for (name, id) in stale {
        transaction.delete_ref(&name, josh_core::cache::Expected::At(id))?;
    }
    for (branch_name, filtered_oid) in &current_commits {
        let ns_ref = format!("{namespace}{branch_name}");
        transaction
            .update_ref(
                &ns_ref,
                josh_core::cache::Expected::Any,
                *filtered_oid,
                "josh filter",
            )
            .context("failed to create filtered reference")?;
    }

    // Ignore configured ref mappings and tag pruning: this fetch owns only the
    // selected remote's branch refs, even when global or remote pruneTags is set.
    let refspec = format!("+refs/heads/*:refs/remotes/{remote_name}/*");
    // Stdout is piped for parsing; stderr keeps the default handling
    // (inherited on a TTY, forwarded otherwise) so progress/errors reach the user.
    // git_command flushes staged namespace deletions and updates before upload-pack
    // reads them, so pruning observes the complete current projection.
    let output = transaction
        .git_command(
            &[
                "fetch",
                "--prune",
                "--no-prune-tags",
                "--no-tags",
                "--refmap=",
                "--porcelain",
                remote_name,
                &refspec,
            ],
            &[],
        )?
        .with_stdout(std::process::Stdio::piped())
        .spawn()
        .context("failed to fetch filtered refs")?;

    crate::porcelain::parse_fetch_porcelain(&String::from_utf8_lossy(&output.stdout))
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
