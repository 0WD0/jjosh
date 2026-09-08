use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jj_cli::cli_util::CommandHelper;
use jj_cli::command_error::{CommandError, user_error, user_error_with_message};
use jj_cli::ui::Ui;
use jj_lib::backend::CommitId;
use jj_lib::config::{ConfigFile, ConfigLayer, ConfigSource};
use jj_lib::op_store::RefTarget;
use jj_lib::repo::{MutableRepo, Repo};

use crate::interop::commit_id_from_josh_oid;

pub struct SourceBookmark {
    pub path: PathBuf,
    pub commit: gix_hash::ObjectId,
}

pub fn source_name(path: &Path) -> Result<String, CommandError> {
    let path = path
        .to_str()
        .ok_or_else(|| user_error("Link path must be valid UTF-8"))?;
    let mut name = String::from("jjosh/source/");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            name.push(char::from(byte));
        } else {
            write!(name, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    Ok(name)
}

pub fn trunk_id(repo: &dyn Repo) -> Result<Option<CommitId>, CommandError> {
    let target = repo.view().get_local_bookmark("jjosh/trunk".as_ref());
    if target.has_conflict() {
        return Err(user_error(
            "The jjosh/trunk bookmark is conflicted; resolve the concurrent repository operations first",
        ));
    }
    Ok(target.as_normal().cloned())
}

// These are defaults, not repository/user overrides. A user-supplied trunk or
// immutable policy continues to win. Keep trunk singleton; source heads are a set.
pub fn default_config() -> ConfigLayer {
    ConfigLayer::parse(
        ConfigSource::Default,
        r#"
[revset-aliases]
'link_heads()' = 'bookmarks(glob:"jjosh/source/*")'
'link_trunk()' = 'bookmarks(exact:"jjosh/trunk")'
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

pub async fn publish(
    repo: &mut MutableRepo,
    transaction: &josh_core::cache::Transaction,
    sources: &[SourceBookmark],
    trunk: &CommitId,
) -> Result<(), CommandError> {
    let mut targets = BTreeMap::new();
    targets.insert("jjosh/trunk".to_owned(), trunk.clone());
    for source in sources {
        let name = source_name(&source.path)?;
        if targets
            .insert(name.clone(), commit_id_from_josh_oid(source.commit))
            .is_some()
        {
            return Err(user_error(format!(
                "Duplicate link source bookmark '{name}'"
            )));
        }
    }
    // Check every bookmark we will replace or delete before indexing or mutating
    // the view. A concurrent operation's conflict must never be overwritten.
    for name in targets.keys() {
        if repo
            .get_local_bookmark(name.as_str().as_ref())
            .has_conflict()
        {
            return Err(user_error(format!(
                "Link bookmark '{name}' is conflicted; resolve the concurrent repository operations first"
            )));
        }
    }
    let mut stale = Vec::new();
    for (name, target) in repo.view().local_bookmarks() {
        if name.as_str().starts_with("jjosh/source/") && !targets.contains_key(name.as_str()) {
            if target.has_conflict() {
                return Err(user_error(format!(
                    "Link bookmark '{}' is conflicted; resolve the concurrent repository operations first",
                    name.as_str(),
                )));
            }
            stale.push(name.to_owned());
        }
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
    for name in stale {
        repo.set_local_bookmark_target(&name, RefTarget::absent());
    }
    for (name, commit) in targets {
        let name = name.as_str().as_ref();
        // The setter also adds a visible head, even for an unchanged target.
        if repo.get_local_bookmark(name).as_normal() != Some(&commit) {
            repo.set_local_bookmark_target(name, RefTarget::normal(commit));
        }
    }
    Ok(())
}

/// Move a validated legacy native baseline into operation-owned bookmarks.
///
/// The caller must validate the old baseline and mapped source commits first,
/// hold the Git import/export lock, and finish the same jj transaction. Git
/// cleanup, like `jj git remote remove`, is not rolled back by a jj transaction.
pub async fn migrate_markers(
    repo: &mut MutableRepo,
    transaction: &josh_core::cache::Transaction,
    sources: &[SourceBookmark],
    trunk: &CommitId,
) -> Result<(), CommandError> {
    let mut targets = BTreeMap::new();
    targets.insert("jjosh/trunk".to_owned(), trunk.clone());
    for source in sources {
        let name = source_name(&source.path)?;
        if targets
            .insert(name.clone(), commit_id_from_josh_oid(source.commit))
            .is_some()
        {
            return Err(user_error(format!(
                "Duplicate link source bookmark '{name}'"
            )));
        }
    }
    // Unlike normal publication, migration must not take over an existing
    // local namespace. Identical targets permit safely repeating migration.
    for (name, target) in repo.view().local_bookmarks() {
        if (name.as_str() == "jjosh/trunk" || name.as_str().starts_with("jjosh/source/"))
            && (target.has_conflict() || target.as_normal() != targets.get(name.as_str()))
        {
            return Err(user_error(format!(
                "Local bookmark '{}' conflicts with the legacy link markers; move or resolve it before running link migrate",
                name.as_str(),
            )));
        }
    }
    let git_repo = jj_lib::git::get_git_repo(repo.store())?;
    let mut config = git_repo.config_snapshot().clone();
    let managed: Vec<String> = git_repo
        .remote_names()
        .into_iter()
        .filter_map(|name| String::from_utf8(name.into()).ok())
        .filter(|name| name == "jjosh" || name.starts_with("link-"))
        .filter(|name| {
            config
                .string(format!("remote.{name}.jjosh-marker").as_str())
                .is_some_and(|value| value.as_slice() == b"true")
        })
        .collect();
    // jj's remove_remote rejects our nonstandard transport/ownership keys and
    // can remove entire user branch sections. Mirror only its refs/view cleanup
    // and remove the owned remote sections, leaving local bookmarks untouched.
    let mut remote_sections = Vec::new();
    for section in config.sections_by_name("remote").into_iter().flatten() {
        if section
            .header()
            .subsection_name()
            .is_some_and(|name| managed.iter().any(|remote| name == remote.as_str()))
        {
            if section.meta() != config.meta() {
                return Err(user_error(
                    "Managed link remotes have configuration outside the repository's Git config; move it into the repository before running link migrate",
                ));
            }
            remote_sections.push(section.id());
        }
    }
    let mut branch_sections = Vec::new();
    for section in config.sections_by_name("branch").into_iter().flatten() {
        let remotes = section.values("remote");
        let push_remotes = section.values("pushRemote");
        let is_managed = |name: &[u8]| managed.iter().any(|remote| name == remote.as_bytes());
        let owns_remote = remotes.iter().any(|name| is_managed(name.as_slice()));
        let owns_push = push_remotes.iter().any(|name| is_managed(name.as_slice()));
        if (owns_remote && remotes.iter().any(|name| !is_managed(name.as_slice())))
            || (owns_push && push_remotes.iter().any(|name| !is_managed(name.as_slice())))
        {
            return Err(user_error(
                "A Git branch configuration mixes managed link and ordinary remotes; separate them before running link migrate",
            ));
        }
        if owns_remote || owns_push {
            if section.meta() != config.meta() {
                return Err(user_error(
                    "A Git branch tracks a managed link remote outside the repository's Git config; move that configuration into the repository before running link migrate",
                ));
            }
            branch_sections.push((section.id(), owns_remote, owns_push));
        }
    }
    // Gather references before making any changes. Reference::delete uses an
    // expected-target guard and does not follow symbolic refs.
    let mut references = Vec::new();
    for remote in &managed {
        for prefix in [
            format!("refs/remotes/{remote}/"),
            format!("refs/jj/remote-tags/{remote}/"),
        ] {
            let platform = git_repo.references().map_err(|err| {
                user_error_with_message("Failed to read legacy link references", err)
            })?;
            for reference in platform.prefixed(prefix.as_str()).map_err(|err| {
                user_error_with_message("Failed to read legacy link references", err)
            })? {
                references.push(reference.map_err(|err| {
                    user_error_with_message("Failed to read legacy link references", err)
                })?);
            }
        }
    }
    publish(repo, transaction, sources, trunk).await?;
    for reference in references {
        reference.delete().map_err(|err| {
            user_error_with_message("Failed to remove legacy link references", err)
        })?;
    }
    for remote in &managed {
        repo.remove_remote(remote.as_str().as_ref());
        let bookmark_prefix = format!("refs/remotes/{remote}/");
        let tag_prefix = format!("refs/jj/remote-tags/{remote}/");
        let git_refs: Vec<_> = repo
            .view()
            .git_refs()
            .keys()
            .filter(|name| {
                name.as_str().starts_with(&bookmark_prefix)
                    || name.as_str().starts_with(&tag_prefix)
            })
            .cloned()
            .collect();
        for name in git_refs {
            repo.set_git_ref_target(&name, RefTarget::absent());
        }
    }
    for (id, remote, push) in branch_sections {
        let mut section = config.section_mut_by_id(id).expect("section exists");
        if remote {
            while section.remove("remote").is_some() {}
            while section.remove("merge").is_some() {}
        }
        if push {
            while section.remove("pushRemote").is_some() {}
        }
    }
    for id in remote_sections {
        config.remove_section_by_id(id);
    }
    if !managed.is_empty() {
        jj_lib::git::save_git_config(&config).map_err(|err| {
            user_error_with_message("Failed to remove legacy link remote configuration", err)
        })?;
    }
    Ok(())
}

// Persist defaults for ordinary jj, upgrading only exact legacy built-in
// values. Any other explicit user/repository/command policy continues to win.
pub async fn install_config(ui: &Ui, command_helper: &CommandHelper) -> Result<(), CommandError> {
    let workspace = command_helper.workspace_helper(ui).await?;
    if trunk_id(workspace.repo().as_ref())?.is_none() {
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
        let legacy = match name {
            "link_heads()" => Some("remote_bookmarks(remote=glob:\"link-*\")"),
            "link_trunk()" => Some("remote_bookmarks(exact:\"trunk\", exact:\"jjosh\")"),
            _ => None,
        };
        let is_custom = |layer: &ConfigLayer| match layer.look_up_item(key) {
            Ok(None) => false,
            Ok(Some(value)) => legacy.is_none() || value.as_str() != legacy,
            Err(_) => true,
        };
        let explicitly_set = workspace
            .settings()
            .config()
            .layers()
            .iter()
            .any(|layer| layer.source != ConfigSource::Default && is_custom(layer))
            || is_custom(file.layer());
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
