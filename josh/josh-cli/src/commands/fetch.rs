use anyhow::Context;

use josh_core::git::normalize_repo_path;

use crate::config::{RemoteConfig, read_remote_config};
use crate::porcelain::RefUpdate;
use crate::remote_ops;

#[derive(Debug, clap::Parser)]
pub struct FetchArgs {
    /// Remote name (or URL) to fetch from
    #[arg(short = 'r', long = "remote", default_value = "origin")]
    pub remote: String,

    /// Ref to fetch (branch, tag, or commit-ish)
    #[arg(short = 'R', long = "ref", default_value = "HEAD")]
    pub rref: String,
}

/// A completed backing fetch whose objects can be inspected before exposing
/// their filtered history through the namespace remote.
pub struct FetchedRemote {
    pub remote: String,
    pub filter: josh_core::filter::Filter,
    pub default_branch: String,
}

/// Fetch unfiltered refs, apply projection filtering, and fetch the filtered
/// refs through the namespace remote. Returns the ref updates reported by the
/// final (namespaced) fetch; does not integrate anything into local branches.
pub fn handle_fetch(
    args: &FetchArgs,
    transaction: &josh_core::cache::Transaction,
    distributed_cache: bool,
) -> anyhow::Result<Vec<RefUpdate>> {
    let fetched = fetch_unfiltered(args, transaction, distributed_cache)?;
    filter_fetched(&fetched, transaction)
}

/// Transfer the unfiltered history without publishing projected refs.
pub fn fetch_unfiltered(
    args: &FetchArgs,
    transaction: &josh_core::cache::Transaction,
    distributed_cache: bool,
) -> anyhow::Result<FetchedRemote> {
    let repo_path = normalize_repo_path(transaction.path());

    let config = read_remote_config(&repo_path, &args.remote)
        .with_context(|| format!("Failed to read remote config for '{}'", args.remote))?;
    let filter = config.semantic_filter();
    let RemoteConfig { url, ref_spec, .. } = config;

    // This backing fetch is the one that actually transfers objects from the
    // remote: --porcelain sends the internal ref-update listing to stdout
    // (captured and discarded) while transfer progress stays on stderr,
    // which is forwarded to the user.
    // Backing history belongs only to the configured private refspec. In
    // particular, auto-followed tags would expose unfiltered commits publicly.
    transaction
        .git_command(
            &[
                "fetch",
                "--prune",
                "--no-prune-tags",
                "--no-tags",
                "--refmap=",
                "--porcelain",
                &url,
                &ref_spec,
            ],
            &[],
        )?
        .with_stdout(std::process::Stdio::piped())
        .spawn()
        .context("git fetch to josh/remotes failed")?;

    if distributed_cache {
        if let Err(e) = crate::commands::cache::fetch_remote_cache(transaction, &url, filter) {
            eprintln!("Warning: could not fetch remote cache: {e}");
        }
    }

    // Resolve the default branch from the remote's HEAD symref.

    // ls-remote --symref output format: "ref: refs/heads/main\t<commit-hash>"
    let output = std::process::Command::new("git")
        .args(["ls-remote", "--symref", &url, "HEAD"])
        .current_dir(&repo_path)
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "Failed to determine default branch: git ls-remote --symref failed for '{}'",
            args.remote
        ));
    }

    let ls_output = String::from_utf8(output.stdout)?;
    let (default_branch, _) =
        remote_ops::try_parse_symref(&args.remote, &ls_output).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine default branch from remote '{}': \
                 no symref for HEAD in ls-remote output",
                args.remote
            )
        })?;

    Ok(FetchedRemote {
        remote: args.remote.clone(),
        filter,
        default_branch,
    })
}

/// Publish projections from a completed, optionally inspected backing fetch.
/// Local branch integration and checkout remain the caller's responsibility.
pub fn filter_fetched(
    fetched: &FetchedRemote,
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<Vec<RefUpdate>> {
    transaction.create_symref(
        &format!("refs/remotes/{}/HEAD", fetched.remote),
        &format!("refs/remotes/{}/{}", fetched.remote, fetched.default_branch),
        "josh remote HEAD",
    )?;
    transaction.create_symref(
        &format!("refs/namespaces/josh-{}/HEAD", fetched.remote),
        &format!("refs/heads/{}", fetched.default_branch),
        "josh remote HEAD",
    )?;
    remote_ops::apply_josh_filtering(
        transaction,
        fetched.filter,
        &fetched.remote,
        &fetched.default_branch,
    )
}
