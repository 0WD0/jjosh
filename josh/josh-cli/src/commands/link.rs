use anyhow::{Context, anyhow};
use std::str::FromStr;

use josh_link::make_signature;

#[derive(Debug, clap::Parser)]
pub struct LinkArgs {
    /// Link subcommand
    #[command(subcommand)]
    pub command: LinkCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum LinkCommand {
    /// Add a link with optional filter and target branch
    Add(LinkAddArgs),
    /// Fetch all SHAs referenced in .link.josh files across history
    Fetch(LinkFetchArgs),
    /// Fetch the latest commit from each linked remote and update .link.josh files
    Update(LinkUpdateArgs),
    /// Push the linked repository to its remote using the :export filter
    Push(LinkPushArgs),
}

#[derive(Debug, clap::Parser)]
pub struct LinkAddArgs {
    /// Path where the link will be mounted
    #[arg()]
    pub path: String,

    /// Remote repository URL
    #[arg()]
    pub url: String,

    /// Optional filter to apply to the linked repository
    #[arg()]
    pub filter: Option<String>,

    /// Target branch to link (defaults to HEAD)
    #[arg(long = "target")]
    pub target: Option<String>,

    /// Link mode: embedded, snapshot, or pointer (defaults to snapshot)
    #[arg(long = "mode", default_value = "snapshot")]
    pub mode: String,
}

#[derive(Debug, clap::Parser)]
pub struct LinkFetchArgs {
    /// Josh filter selecting which links to consider (considers all if omitted)
    #[arg()]
    pub filter: Option<String>,
}

#[derive(Debug, clap::Parser)]
pub struct LinkUpdateArgs {
    /// Josh filter selecting which links to update (updates all if omitted)
    #[arg()]
    pub filter: Option<String>,
}

#[derive(Debug, clap::Parser)]
pub struct LinkPushArgs {
    /// Path of the link to push (e.g. /docs or docs)
    #[arg()]
    pub path: String,

    /// Force push, overwriting the remote branch
    #[arg(long, short = 'f')]
    pub force: bool,
}

pub fn handle_link(
    args: &LinkArgs,
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<()> {
    match &args.command {
        LinkCommand::Add(add_args) => handle_link_add(add_args, transaction),
        LinkCommand::Fetch(fetch_args) => handle_link_fetch(fetch_args, transaction),
        LinkCommand::Update(update_args) => handle_link_update(update_args, transaction),
        LinkCommand::Push(push_args) => handle_link_push(push_args, transaction),
    }
}

fn handle_link_add(
    args: &LinkAddArgs,
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<()> {
    // Validate the path (should not be empty and should be a valid path)
    if args.path.is_empty() {
        return Err(anyhow!("Path cannot be empty"));
    }

    // Get the filter (default to ":/" if not provided)
    let filter = args.filter.as_deref().unwrap_or(":/");

    // Get the target branch (default to "HEAD" if not provided)
    let target = args.target.as_deref().unwrap_or("HEAD");

    let head_commit = transaction.head().context("Failed to get HEAD")?.commit;

    let mode = josh_core::filter::LinkMode::parse(&args.mode)
        .with_context(|| format!("Invalid link mode: '{}'", args.mode))?;

    // Prefer the path's current contents, so adding a link to an existing directory preserves
    // local work. If the path is absent, seed it from the configured remote.
    let initial_oid = if let Some(export_oid) = josh_link::export_link_source(
        transaction,
        head_commit,
        std::path::Path::new(&args.path),
        filter,
    )? {
        eprintln!(
            "Using local content at '{}' ({})",
            args.path.trim_matches('/'),
            export_oid
        );
        export_oid
    } else {
        eprintln!(
            "No local content at '{}', fetching from remote...",
            args.path.trim_matches('/')
        );

        transaction
            .spawn_git(&["fetch", &args.url, target], &[])
            .context("Failed to execute git fetch")?;

        let fetched_oid = josh_core::git::resolve_fetch_head(transaction)
            .context("Failed to resolve FETCH_HEAD after fetch")?;

        eprintln!("Using fetched commit {}", fetched_oid);
        fetched_oid
    };

    // Create a new commit with the updated tree
    let signature = make_signature(transaction)?;

    let commit_oid = josh_link::prepare_link_add(
        transaction,
        std::path::Path::new(&args.path),
        &args.url,
        None,
        args.filter.as_deref(),
        target,
        None,
        initial_oid,
        josh_core::objects::CommitData::read(transaction.odb(), head_commit)?.tree_id()?,
        mode,
    )?
    .into_commit(transaction, head_commit, &signature)?;

    // Create the fixed branch name
    let branch_name = "refs/heads/josh-link";

    // Create or update the branch reference
    transaction
        .update_ref(
            branch_name,
            josh_core::cache::Expected::Any,
            commit_oid,
            "josh link add",
        )
        .with_context(|| format!("Failed to create branch '{}'", branch_name))?;

    eprintln!(
        "Added link '{}' with URL '{}', filter '{}', target '{}', and mode '{}'",
        args.path.trim_matches('/'),
        args.url,
        filter,
        target,
        args.mode
    );
    eprintln!("Created branch: {}", branch_name);

    Ok(())
}

fn handle_link_fetch(
    args: &LinkFetchArgs,
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<()> {
    let head_commit = transaction.head().context("Failed to get HEAD")?.commit;

    let commit_oid = if let Some(filter_str) = &args.filter {
        let filter = josh_core::filter::parse(filter_str)
            .with_context(|| format!("Failed to parse filter '{}'", filter_str))?;
        let roundtrip = filter.chain(
            josh_core::filter::invert(filter)
                .with_context(|| format!("Filter '{}' has no inverse", filter_str))?,
        );
        josh_core::filter_commit(transaction, roundtrip, head_commit)
            .context("Failed to apply filter")?
    } else {
        head_commit
    };

    let link_refs = josh_link::collect_all_link_refs(transaction, commit_oid)
        .context("Failed to collect link refs from history")?;

    if link_refs.is_empty() {
        eprintln!("No .link.josh references found in history");
        return Ok(());
    }

    eprintln!(
        "Found {} unique (remote, sha) pair(s) across history",
        link_refs.len()
    );

    let odb = transaction.odb();

    let mut fetched = 0;
    let mut skipped = 0;

    for link_ref in &link_refs {
        let oid = gix_hash::ObjectId::from_str(&link_ref.commit)
            .with_context(|| format!("Invalid commit SHA in link file: {}", link_ref.commit))?;

        if odb.contains(oid) {
            skipped += 1;
            continue;
        }

        // Fetch the specific SHA from the remote into a temporary ref so the
        // object is stored in the local ODB.
        let refspec = format!(
            "{}:refs/josh/link-shas/{}",
            link_ref.commit, link_ref.commit
        );

        eprintln!("Fetching {} from {}", link_ref.commit, link_ref.remote);

        transaction
            .spawn_git(&["fetch", &link_ref.remote, &refspec], &[])
            .with_context(|| {
                format!(
                    "git fetch of {} from {} failed",
                    link_ref.commit, link_ref.remote
                )
            })?;

        fetched += 1;
    }

    eprintln!(
        "Done: fetched {}, skipped {} (already present)",
        fetched, skipped
    );

    Ok(())
}

fn handle_link_update(
    args: &LinkUpdateArgs,
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<()> {
    let head_commit = transaction.head().context("Failed to get HEAD")?.commit;
    let head_tree = josh_core::objects::CommitData::read(transaction.odb(), head_commit)?
        .tree_id()
        .context("Failed to get HEAD tree")?;

    let link_files = if let Some(filter_str) = &args.filter {
        let filter = josh_core::filter::parse(filter_str)
            .with_context(|| format!("Failed to parse filter '{}'", filter_str))?;
        let roundtrip = filter.chain(
            josh_core::filter::invert(filter)
                .with_context(|| format!("Filter '{}' has no inverse", filter_str))?,
        );
        let filtered_oid = josh_core::filter_commit(transaction, roundtrip, head_commit)
            .context("Failed to apply filter")?;
        if filtered_oid == gix_hash::ObjectId::null(gix_hash::Kind::Sha1) {
            vec![]
        } else {
            let odb = transaction.odb();
            let filtered_tree = josh_core::objects::CommitData::read(odb, filtered_oid)
                .context("Failed to find filtered commit")?
                .tree_id()
                .context("Failed to get filtered tree")?;
            josh_core::link::find_link_files(odb, filtered_tree)
                .context("Failed to find link files in filtered tree")?
        }
    } else {
        josh_core::link::find_link_files(transaction.odb(), head_tree)
            .context("Failed to find link files")?
    };

    if link_files.is_empty() {
        return Err(anyhow!("No .link.josh files found"));
    }

    eprintln!("Found {} link file(s) to update", link_files.len());

    let mut links_to_update = Vec::new();
    for (path, link_file) in &link_files {
        let remote = link_file.get_meta("remote").ok_or_else(|| {
            anyhow!(
                "Link file missing 'remote' metadata at path '{}'",
                path.display()
            )
        })?;
        let branch = link_file
            .get_meta("target")
            .unwrap_or_else(|| "HEAD".to_string());

        eprintln!("Fetching {} from {}", branch, remote);

        transaction
            .spawn_git(&["fetch", &remote, &branch], &[])
            .with_context(|| format!("git fetch failed for '{}'", path.display()))?;

        let new_oid = josh_core::git::resolve_fetch_head(transaction)
            .context("Failed to resolve FETCH_HEAD")?;

        links_to_update.push((path.clone(), new_oid));
    }

    let signature = make_signature(transaction)?;
    let Some(result) =
        josh_link::update_links(transaction, head_commit, links_to_update, &signature)?
    else {
        eprintln!("All {} link file(s) already up to date", link_files.len());
        return Ok(());
    };

    let branch_name = "refs/heads/josh-link";
    transaction
        .update_ref(
            branch_name,
            josh_core::cache::Expected::Any,
            result.filtered_commit,
            "josh link update",
        )
        .with_context(|| format!("Failed to update branch '{}'", branch_name))?;

    eprintln!("Updated {} link file(s)", link_files.len());
    eprintln!("Updated branch: {}", branch_name);

    Ok(())
}

fn handle_link_push(
    args: &LinkPushArgs,
    transaction: &josh_core::cache::Transaction,
) -> anyhow::Result<()> {
    let head_commit = transaction.head().context("Failed to get HEAD")?.commit;
    let prepared =
        josh_link::prepare_link_push(transaction, head_commit, std::path::Path::new(&args.path))?;

    // Preserve the existing CLI behavior for links configured against HEAD. Native integrations
    // can resolve HEAD or supply an explicit destination before executing the prepared push.
    let push_ref = if prepared.configured_target == "HEAD" {
        "refs/heads/master"
    } else {
        &prepared.configured_target
    };
    let refspec = format!(
        "{}{}:{}",
        if args.force { "+" } else { "" },
        prepared.exported_commit,
        push_ref
    );

    transaction
        .spawn_git(&["push", &prepared.remote, &refspec], &[])
        .with_context(|| format!("Failed to push to '{}'", prepared.remote))?;

    Ok(())
}
