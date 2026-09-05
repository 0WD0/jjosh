use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::config::{ConfigFile, ConfigLayer, ConfigSource};
use jj_lib::op_store::{RefTarget, RemoteRef};
use jj_lib::ref_name::{RefName, RemoteName};
use jj_lib::repo::{MutableRepo, Repo};

use crate::interop::commit_id_from_josh_oid;

pub struct SourceBookmark {
    pub path: PathBuf,
    pub branch: String,
    pub commit: gix_hash::ObjectId,
}

pub fn remote_name(path: &Path) -> Result<String, CommandError> {
    let path = path
        .to_str()
        .ok_or_else(|| user_error("Link path must be valid UTF-8"))?;
    let mut name = String::from("link-");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            name.push(char::from(byte));
        } else {
            write!(name, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    Ok(name)
}

pub fn trunk_id(repo: &dyn Repo) -> Option<CommitId> {
    let name: &RefName = "trunk".as_ref();
    repo.view()
        .get_remote_bookmark(name.to_remote_symbol("jjosh".as_ref()))
        .target
        .as_normal()
        .cloned()
}

// These are defaults, not repository/user overrides. A user-supplied trunk or
// immutable policy continues to win. Keep trunk singleton; source heads are a set.
pub fn default_config() -> ConfigLayer {
    ConfigLayer::parse(
        ConfigSource::Default,
        r#"
[revset-aliases]
'link_heads()' = 'remote_bookmarks(remote=glob:"link-*")'
'link_trunk()' = 'remote_bookmarks(exact:"trunk", exact:"jjosh")'
'link_fallback_trunk()' = '''latest(
    remote_bookmarks(exact:"main", exact:"origin") |
    remote_bookmarks(exact:"master", exact:"origin") |
    remote_bookmarks(exact:"trunk", exact:"origin") |
    remote_bookmarks(exact:"main", exact:"upstream") |
    remote_bookmarks(exact:"master", exact:"upstream") |
    remote_bookmarks(exact:"trunk", exact:"upstream") | root()
)'''
'trunk()' = 'coalesce(link_trunk(), link_fallback_trunk())'
'immutable_heads()' = 'builtin_immutable_heads() | link_heads() | link_trunk()'
"#,
    )
    .expect("built-in link configuration must parse")
}

fn ensure_marker_remote(
    transaction: &josh_core::cache::Transaction,
    name: &str,
) -> Result<(), CommandError> {
    let git_repo = transaction.repo();
    let config = git_repo.config_snapshot();
    let owner_key = format!("remote.{name}.jjosh-marker");
    let managed = config
        .string(owner_key.as_str())
        .is_some_and(|value| value.as_slice() == b"true");
    let url_key = format!("remote.{name}.url");
    if config.string(url_key.as_str()).is_some() && !managed {
        return Err(user_error(format!(
            "Git remote '{name}' already exists and is not managed by jjosh; rename it before creating link markers"
        )));
    }
    let local_url = git_repo.path().to_string_lossy();
    // Never configure the raw upstream URL here: these refs contain mapped IDs.
    // Both transports fail closed. Only `link` commands may refresh/publish them.
    for (key, value) in [
        (url_key, local_url.as_ref()),
        (format!("remote.{name}.pushurl"), local_url.as_ref()),
        (format!("remote.{name}.uploadpack"), "false"),
        (format!("remote.{name}.receivepack"), "false"),
        (owner_key, "true"),
    ] {
        transaction
            .spawn_git(&["config", "--local", "--replace-all", &key, value], &[])
            .map_err(|err| {
                user_error_with_message("Failed to configure the read-only link marker remote", err)
            })?;
    }
    Ok(())
}

async fn set_marker(
    repo: &mut MutableRepo,
    remote: &str,
    branch: &str,
    commit: CommitId,
) -> Result<(), CommandError> {
    let name: &RefName = branch.as_ref();
    let remote: &RemoteName = remote.as_ref();
    let symbol = name.to_remote_symbol(remote);
    let previous = repo.get_remote_bookmark(symbol).clone();
    if previous.target.has_conflict() {
        return Err(user_error(format!(
            "Link marker '{symbol}' is conflicted; resolve the concurrent repository operations first"
        )));
    }
    let target = RefTarget::normal(commit);
    // Respect explicit user tracking, but do not create a same-name local
    // bookmark merely because a link has an upstream branch named main.
    if previous.is_tracked() {
        repo.merge_local_bookmark(name, &previous.target, &target)
            .await?;
    }
    repo.set_remote_bookmark(
        symbol,
        RemoteRef {
            target,
            state: previous.state,
        },
    );
    Ok(())
}

fn export_markers(repo: &mut MutableRepo) -> Result<(), CommandError> {
    // Export even for non-colocated workspaces. A view-only remote bookmark is
    // otherwise interpreted as deleted by the next native Git import.
    let stats = jj_lib::git::export_some_refs(repo, |_, symbol| {
        symbol.remote.as_str() == "jjosh" || symbol.remote.as_str().starts_with("link-")
    })?;
    if !stats.failed_bookmarks.is_empty() {
        return Err(user_error(format!(
            "Failed to persist link marker references: {:?}",
            stats.failed_bookmarks
        )));
    }
    Ok(())
}

pub async fn publish(
    repo: &mut MutableRepo,
    transaction: &josh_core::cache::Transaction,
    sources: &[SourceBookmark],
    trunk: &CommitId,
) -> Result<(), CommandError> {
    let trunk_name: &RefName = "trunk".as_ref();
    if repo
        .get_remote_bookmark(trunk_name.to_remote_symbol("jjosh".as_ref()))
        .target
        .has_conflict()
    {
        return Err(user_error("The jjosh trunk marker is conflicted"));
    }
    transaction
        .flush_mem_odb()
        .map_err(|err| user_error_with_message("Failed to persist link history", err))?;
    let mut commits = Vec::with_capacity(sources.len());
    for source in sources {
        commits.push(
            repo.store()
                .get_commit_async(&commit_id_from_josh_oid(source.commit))
                .await?,
        );
    }
    repo.index_commits(&commits).await?;
    ensure_marker_remote(transaction, "jjosh")?;
    for source in sources {
        let remote = remote_name(&source.path)?;
        ensure_marker_remote(transaction, &remote)?;
        set_marker(
            repo,
            &remote,
            &source.branch,
            commit_id_from_josh_oid(source.commit),
        )
        .await?;
    }
    set_marker(repo, "jjosh", "trunk", trunk.clone()).await?;
    export_markers(repo)
}

// Persist the defaults needed by ordinary `jj` too. Never replace an explicit
// policy from any user/repository/command layer.
pub async fn install_config(ui: &Ui, command_helper: &CommandHelper) -> Result<(), CommandError> {
    let workspace = command_helper.workspace_helper(ui).await?;
    if trunk_id(workspace.repo().as_ref()).is_none() {
        return Ok(());
    }
    let Some(path) = command_helper.config_env().repo_config_path(ui)? else {
        return Ok(());
    };
    let mut file = ConfigFile::load_or_empty(ConfigSource::Repo, path)?;
    let defaults = default_config();
    let aliases = defaults
        .look_up_table("revset-aliases")
        .expect("built-in aliases are a table")
        .expect("built-in aliases exist");
    let mut changed = false;
    for (name, item) in aliases.iter() {
        let key = ["revset-aliases", name];
        let explicitly_set = workspace.settings().config().layers().iter().any(|layer| {
            layer.source != ConfigSource::Default && !matches!(layer.look_up_item(key), Ok(None))
        }) || !matches!(file.layer().look_up_item(key), Ok(None));
        if !explicitly_set {
            file.set_value(key, item.as_str().expect("built-in aliases are strings"))
                .map_err(|err| {
                    user_error_with_message("Failed to install native link aliases", err)
                })?;
            changed = true;
        }
    }
    if changed {
        file.save()?;
    }
    Ok(())
}

// Raw remote observations are deliberately separate from mapped source markers.
// Retain the existing key so leases established by previous jjosh pushes survive.
pub fn push_tracking_ref(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
) -> Result<String, CommandError> {
    let key = format!("{remote}\0{destination}");
    let id = josh_core::objects::write_blob(transaction.odb(), key.as_bytes()).map_err(|err| {
        user_error_with_message("Failed to identify the link push destination", err)
    })?;
    Ok(format!("refs/jjosh/link-push/{id}"))
}

pub fn record_observation(
    transaction: &josh_core::cache::Transaction,
    remote: &str,
    destination: &str,
    commit: gix_hash::ObjectId,
) -> Result<(), CommandError> {
    if !destination.starts_with("refs/heads/") {
        return Err(user_error(
            "A publication observation must identify a branch",
        ));
    }
    let reference = push_tracking_ref(transaction, remote, destination)?;
    let previous = transaction.resolve_ref(&reference).map_err(|err| {
        user_error_with_message("Failed to read the observed remote position", err)
    })?;
    transaction
        .update_ref(
            &reference,
            previous.map_or(
                josh_core::cache::Expected::Absent,
                josh_core::cache::Expected::At,
            ),
            commit,
            "jjosh link remote observation",
        )
        .and_then(|()| transaction.flush_mem_odb())
        .map_err(|err| user_error_with_message("Failed to save the observed remote position", err))
}
