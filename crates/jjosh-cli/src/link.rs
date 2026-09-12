use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use jj_cli::cli_util::CommandHelper;
use jj_cli::cli_util::RevisionArg;
use jj_cli::command_error::CommandError;
use jj_cli::command_error::user_error;
use jj_cli::command_error::user_error_with_message;
use jj_cli::ui::Ui;
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo::Repo as _;

use crate::interop::commit_as_josh_oid;
use crate::interop::commit_id_from_josh_oid;
use crate::interop::open_josh_transaction;
use crate::interop::sha1_git_repo_path;
use crate::interop::tree_from_josh_oid;

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: LinkCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum LinkCommand {
    /// Add an external project, or associate a source with an imported jj project.
    Add(AddArgs),
}

#[derive(clap::Args, Clone, Debug)]
struct AddArgs {
    /// Path where the linked repository is mounted.
    path: String,
    /// Project identity for NAME#PROJECT bookmarks and PROJECT-REMOTE observations.
    /// Defaults to the last path component.
    #[arg(long)]
    name: Option<String>,
    /// Linked repository URL.
    url: String,
    /// Josh filter applied before mounting the linked repository.
    filter: Option<String>,
    /// Source branch used for the initial import.
    #[arg(long, default_value = "HEAD")]
    target: String,
    /// Alternate URL used only to fetch the initial linked history.
    #[arg(long)]
    fetch_url: Option<String>,
    /// Separate remote URL used only for publication.
    #[arg(long)]
    push_url: Option<String>,
    /// Alternate branch, tag, or commit used only to select the initial linked history.
    #[arg(long)]
    at: Option<String>,
    /// Name of this source observation, such as upstream or origin.
    #[arg(long = "remote-name", default_value = "upstream")]
    source_remote: String,
    /// Link history mode: `embedded` for development or `snapshot` for vendoring.
    #[arg(long, default_value = "embedded")]
    mode: String,
    /// Jujutsu revision whose tree should receive the link.
    #[arg(short = 'r', long, default_value = "@")]
    revision: RevisionArg,
}

pub(crate) async fn run(
    ui: &mut Ui,
    command_helper: &CommandHelper,
    args: Args,
) -> Result<(), CommandError> {
    match args.command {
        LinkCommand::Add(args) => run_add(ui, command_helper, args).await,
    }
}

fn normalized_link_path(path: &str) -> Result<PathBuf, CommandError> {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        return Err(user_error("Link path cannot be empty"));
    }
    let path = PathBuf::from(normalized);
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(user_error("Link path must stay within the repository"));
    }
    Ok(path)
}

fn write_link_metadata(
    transaction: &josh_core::cache::Transaction,
    mut tree: gix_hash::ObjectId,
    links: &[(PathBuf, josh_core::filter::Filter)],
) -> Result<gix_hash::ObjectId, CommandError> {
    for (path, link) in links {
        let content = josh_core::filter::as_file(*link, 0);
        let blob = josh_core::objects::write_blob(transaction.odb(), content.as_bytes())
            .map_err(|err| user_error_with_message("Failed to write Josh link metadata", err))?;
        tree = josh_core::filter::tree::insert_oid(
            transaction.odb(),
            tree,
            &path.join(".link.josh"),
            blob,
            0o100644,
        )
        .map_err(|err| user_error_with_message("Failed to insert Josh link metadata", err))?;
    }
    Ok(tree)
}

fn fetch_initial(
    git_path: &Path,
    url: &str,
    target: &str,
) -> Result<(gix_hash::ObjectId, String, Vec<PathBuf>), CommandError> {
    let git = gix::open(git_path).map_err(user_error)?;
    let remote = git.remote_at(url).map_err(user_error)?;
    let interrupt = std::sync::atomic::AtomicBool::new(false);
    if let Ok(id) = gix_hash::ObjectId::from_hex(target.as_bytes()) {
        let keeps = crate::git_transport::fetch::receive_objects(remote, [id], &interrupt)
            .map_err(user_error)?;
        return Ok((id, "pinned".to_owned(), keeps));
    }
    let fetched = crate::git_transport::fetch::fetch(
        remote,
        |reference| {
            let name = reference.unpack().0;
            name == target.as_bytes()
                || target != "HEAD"
                    && (name.strip_prefix(b"refs/heads/") == Some(target.as_bytes())
                        || name.strip_prefix(b"refs/tags/") == Some(target.as_bytes()))
        },
        &interrupt,
    )
    .map_err(user_error)?;
    let [reference] = fetched.received.as_slice() else {
        return Err(user_error(format!(
            "Initial source {target:?} must select exactly one existing branch, tag, or HEAD"
        )));
    };
    let branch_name = match reference {
        gix::protocol::handshake::Ref::Symbolic { target, .. } => {
            target.strip_prefix(b"refs/heads/")
        }
        _ => reference.unpack().0.strip_prefix(b"refs/heads/"),
    };
    let branch = branch_name
        .map(std::str::from_utf8)
        .transpose()
        .map_err(user_error)?
        .unwrap_or("pinned")
        .to_owned();
    let id = reference
        .unpack()
        .1
        .ok_or_else(|| user_error("Initial source has no object"))?
        .to_owned();
    Ok((id, branch, fetched.keep_paths))
}

async fn run_add(ui: &mut Ui, command: &CommandHelper, args: AddArgs) -> Result<(), CommandError> {
    if !command.is_at_head_operation() || command.global_args().no_integrate_operation {
        return Err(user_error(
            "Link add requires the current integrated operation",
        ));
    }
    let mut workspace = command.workspace_helper(ui).await?;
    let commit = workspace.resolve_single_rev(ui, &args.revision).await?;
    let path = normalized_link_path(&args.path)?;
    let mount = path
        .to_str()
        .ok_or_else(|| user_error("Link path must be UTF-8"))?;
    crate::native_project::validate_project(&args.source_remote).map_err(user_error)?;
    let mode = crate::link_metadata::LinkMode::parse(&args.mode).map_err(user_error)?;
    let git_path = sha1_git_repo_path(&workspace)?;
    let git_lock = workspace.lock_git_import_export()?;
    let transaction = open_josh_transaction(&git_path, false)?;
    let source_tree = josh_core::git::read_tree_id(transaction.odb(), commit_as_josh_oid(&commit)?)
        .map_err(user_error)?;
    let existing_links = crate::link_metadata::find_link_files(transaction.odb(), source_tree)
        .map_err(user_error)?;
    let native_name = crate::native_project::native_project_for_mount(
        &transaction,
        &crate::native_project::parse_mount(mount).map_err(user_error)?,
    )
    .map_err(user_error)?;
    let native = native_name.is_some();
    let project = if let Some(name) = &args.name {
        let name = crate::native_project::parse_project(name).map_err(user_error)?;
        if let Some(native_name) = native_name.as_deref() {
            if native_name != name {
                return Err(user_error(format!(
                    "Imported project at {} is named {native_name}, not {name}",
                    path.display()
                )));
            }
        }
        name
    } else if let Some(native_name) = &native_name {
        native_name.clone()
    } else {
        crate::ref_names::project_from_path(&path).map_err(user_error)?
    };
    let remote_name =
        crate::ref_names::observation_remote(&project, &args.source_remote).map_err(user_error)?;
    let git = gix::open(&git_path).map_err(user_error)?;
    if git
        .remote_names()
        .iter()
        .any(|name| &name[..] == remote_name.as_bytes())
        && crate::git_remote::config_string(&git, &format!("remote.{remote_name}.jjosh-project"))
            .map_err(user_error)?
            .as_deref()
            != Some(project.as_str())
    {
        return Err(user_error(format!(
            "Remote {remote_name} already exists and is not attached to {project}"
        )));
    }
    for (existing_path, existing) in &existing_links {
        if existing_path == &path {
            continue;
        }
        let existing_name =
            crate::ref_names::project_from_link(existing_path, existing).map_err(user_error)?;
        if existing_name == project {
            return Err(user_error(format!(
                "Project {project:?} is already mounted at {}",
                existing_path.display()
            )));
        }
    }
    let filter =
        josh_core::filter::parse(args.filter.as_deref().unwrap_or(":/")).map_err(user_error)?;
    if native
        && (filter != josh_core::filter::Filter::new()
            || mode != crate::link_metadata::LinkMode::Embedded)
    {
        return Err(user_error(
            "Attach imported projects with their existing whole-repository layout",
        ));
    }
    let url = jj_cli::git_util::absolute_git_url(command.cwd(), &args.url)?;
    let fetch_url = jj_cli::git_util::absolute_git_url(
        command.cwd(),
        args.fetch_url.as_deref().unwrap_or(&args.url),
    )?;
    let fetch_target = args.at.as_deref().unwrap_or(&args.target);
    let (raw_object, branch, keeps) = fetch_initial(&git_path, &fetch_url, fetch_target)?;
    git.reference(
        format!("refs/jjosh/pins/{raw_object}"),
        raw_object,
        gix::refs::transaction::PreviousValue::Any,
        "retain initial linked source",
    )
    .map_err(user_error)?;
    for keep in keeps {
        std::fs::remove_file(keep)?;
    }
    let raw =
        josh_core::objects::peel_to_commit(transaction.odb(), raw_object).map_err(user_error)?;
    if !native {
        crate::interop::check_raw_projectable_history(&transaction, [raw])?;
    }
    let mut pin = raw;
    if native {
        let known =
            crate::native_project::anchors(workspace.repo().as_ref(), &transaction, &project)
                .await
                .map_err(user_error)?;
        let mut pending = vec![raw];
        let mut seen = HashSet::new();
        let mut found = None;
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if known.contains_key(&commit_id_from_josh_oid(id)) {
                found = Some(id);
                break;
            }
            pending.extend(
                josh_core::git::read_parent_ids(transaction.odb(), id).map_err(user_error)?,
            );
        }
        pin = found.ok_or_else(|| {
            user_error("The selected source has no known history in the imported project")
        })?;
    }
    let push_url = args
        .push_url
        .as_deref()
        .map(|url| jj_cli::git_util::absolute_git_url(command.cwd(), url))
        .transpose()?;
    let prepared = crate::link_metadata::prepare_link_add(
        &transaction,
        &path,
        &project,
        &url,
        push_url.as_deref(),
        args.filter.as_deref(),
        &args.target,
        pin,
        source_tree,
        mode.clone(),
    )
    .map_err(user_error)?;
    let mut links =
        crate::link_metadata::find_link_files(transaction.odb(), prepared).map_err(user_error)?;
    let (_, link) = links
        .iter_mut()
        .find(|(candidate, _)| candidate == &path)
        .unwrap();
    *link = link
        .with_meta("source-branch", branch.clone())
        .with_meta("source-remote", args.source_remote.clone());
    let link = *link;
    let metadata_tree = write_link_metadata(&transaction, prepared, &links)?;
    let was_working_copy = workspace.get_wc_commit_id() == Some(commit.id());
    let mut tx = workspace.start_transaction();
    let mut parents = vec![commit.id().clone()];
    let tree = if native
        || existing_links
            .iter()
            .any(|(candidate, _)| candidate == &path)
    {
        tree_from_josh_oid(tx.repo().store().clone(), metadata_tree)
    } else {
        let projected =
            josh_core::filter_commit(&transaction, link.peel(), raw).map_err(user_error)?;
        transaction.flush_mem_odb().map_err(user_error)?;
        if projected.is_null() {
            tree_from_josh_oid(tx.repo().store().clone(), metadata_tree)
        } else {
            let source_id = commit_id_from_josh_oid(projected);
            let source = tx.repo().store().get_commit_async(&source_id).await?;
            tx.repo_mut().add_head(&source).await?;
            if mode == crate::link_metadata::LinkMode::Embedded {
                parents.push(source_id.clone());
            }
            let remote: jj_lib::ref_name::RemoteNameBuf = remote_name.clone().into();
            if branch != "pinned" {
                let name: jj_lib::ref_name::RefNameBuf =
                    crate::ref_names::local_name(&project, &branch).into();
                tx.repo_mut().set_remote_bookmark(
                    name.to_remote_symbol(&remote),
                    jj_lib::op_store::RemoteRef {
                        target: jj_lib::op_store::RefTarget::normal(source_id),
                        state: jj_lib::op_store::RemoteRefState::New,
                    },
                );
                let stats = jj_lib::git::export_some_refs(tx.repo_mut(), |_, symbol| {
                    symbol.remote == remote
                })?;
                jj_cli::git_util::print_git_export_stats(ui, &stats)?;
            }
            MergedTree::merge(Merge::from_vec(vec![
                (
                    tree_from_josh_oid(tx.repo().store().clone(), metadata_tree),
                    "existing monorepo".to_owned(),
                ),
                (
                    tx.repo()
                        .store()
                        .get_commit_async(tx.repo().store().root_commit_id())
                        .await?
                        .tree(),
                    "empty mount".to_owned(),
                ),
                (source.tree(), "linked source".to_owned()),
            ]))
            .await?
        }
    };
    transaction.flush_mem_odb().map_err(user_error)?;
    let added = tx
        .repo_mut()
        .new_commit(parents, tree)
        .set_description(format!("Associate Josh link {}", path.display()))
        .write()
        .await?;
    if was_working_copy {
        tx.edit(&added)?;
    }
    josh_cli::remote_ops::configure_remote(
        &git_path,
        &remote_name,
        &url,
        &josh_core::filter::spec(filter.prefix(&path)),
        None,
        push_url.as_deref(),
        None,
    )
    .map_err(user_error)?;
    let project_mount = crate::native_project::parse_mount(mount).map_err(user_error)?;
    crate::git_remote::configure_attachment(
        &git_path,
        &remote_name,
        &project,
        &project_mount,
        push_url.is_none(),
    )
    .map_err(user_error)?;
    if !native {
        // Initial context is declared by link setup, not a publication lease.
        // Prefer the live source branch after later explicit fetches.
        let source_remote = git.remote_at(url.as_str()).map_err(user_error)?;
        let (mut endpoint, _) = source_remote
            .sanitized_url_and_version(gix::remote::Direction::Fetch).map_err(user_error)?;
        endpoint.canonicalize(git.workdir().unwrap_or_else(|| git.common_dir())).map_err(user_error)?;
        let endpoint = String::from_utf8(endpoint.to_bstring().into()).map_err(user_error)?;
        let prefix = crate::git_remote::raw_ref_prefix(&git, &endpoint).map_err(user_error)?;
        let initial = format!("{prefix}bases/{project}");
        let old = transaction.resolve_ref(&initial).map_err(user_error)?;
        transaction.update_ref(
            &initial,
            old.map_or(josh_core::cache::Expected::Absent, josh_core::cache::Expected::At),
            raw,
            "retain initial project context",
        ).map_err(user_error)?;
        let source = if branch == "pinned" {
            format!("bases/{project}")
        } else {
            format!("refs/heads/{branch}")
        };
        let mut config = git.config_file_mut(git.config_path(gix::config::Source::Local).map_err(user_error)?)
            .map_err(user_error)?;
        config.set_raw_value(format!("remote.{remote_name}.jjosh-base").as_str(), source.as_str())
            .map_err(user_error)?;
        config.commit().map_err(user_error)?;
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    if branch != "pinned" {
        let prefix = crate::git_remote::raw_ref_prefix(&git, &fetch_url).map_err(user_error)?;
        let raw_ref = format!("{prefix}refs/heads/{branch}");
        let old = transaction.resolve_ref(&raw_ref).map_err(user_error)?;
        transaction
            .update_ref(
                &raw_ref,
                old.map_or(
                    josh_core::cache::Expected::Absent,
                    josh_core::cache::Expected::At,
                ),
                raw,
                "observe initial linked source",
            )
            .map_err(user_error)?;
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    tx.finish_with_git_import_export_lock(
        ui,
        format!("add linked source {}", path.display()),
        &git_lock,
    )
    .await?;
    if branch != "pinned" {
        crate::link_refs::record_observation(
            &transaction,
            &fetch_url,
            &format!("refs/heads/{branch}"),
            raw,
        )?;
        transaction.flush_mem_odb().map_err(user_error)?;
    }
    Ok(())
}
