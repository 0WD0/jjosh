use std::io::Write;

use anyhow::{Context, anyhow};

/// Forge-specific behavior for a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Forge {
    Github,
    Gerrit,
}

impl std::fmt::Display for Forge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Forge::Github => f.write_str("github"),
            Forge::Gerrit => f.write_str("gerrit"),
        }
    }
}

/// How `josh changes publish` maps a stack onto Gerrit changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum GerritMode {
    /// Publish only dependency-free changes as independent reviews.
    #[default]
    Independent,
    /// Push the whole commit history once as a single Gerrit relation chain.
    Stack,
}

impl std::fmt::Display for GerritMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GerritMode::Independent => f.write_str("independent"),
            GerritMode::Stack => f.write_str("stack"),
        }
    }
}

/// Meta keys reserved for remote configuration rather than filter semantics.
pub const TRANSPORT_META_KEYS: &[&str] =
    &["url", "fetch", "push", "pushurl", "forge", "gerrit-mode"];

/// Resolved remote transport and filter configuration.
pub struct RemoteConfig {
    pub url: String,
    pub ref_spec: String,
    pub filter_with_meta: josh_core::filter::Filter,
    pub forge: Option<Forge>,
    /// Push destination for forks; `url` remains the fetch and review target.
    pub push_url: Option<String>,
    /// Stack mapping used only for Gerrit remotes.
    pub gerrit_mode: GerritMode,
}

impl RemoteConfig {
    /// Preserve history-affecting metadata while removing remote transport keys.
    pub fn semantic_filter(&self) -> josh_core::filter::Filter {
        self.filter_with_meta.without_meta_keys(TRANSPORT_META_KEYS)
    }
}

fn validate_remote_name(name: &str) -> anyhow::Result<()> {
    gix::remote::name::validated(name)
        .with_context(|| format!("Invalid Josh remote name '{name}'"))?;
    anyhow::ensure!(
        !name.contains('/'),
        "Josh remote name '{name}' must be a single configuration-file component"
    );
    Ok(())
}

fn config_string(repo: &gix::Repository, key: &str) -> anyhow::Result<Option<String>> {
    repo.config_snapshot()
        .string(key)
        .map(|value| {
            std::str::from_utf8(value.as_ref())
                .map(ToOwned::to_owned)
                .map_err(Into::into)
        })
        .transpose()
}

/// Read a configured Josh remote without modifying the repository.
pub fn read_remote_config(
    repo_path: &std::path::Path,
    remote_name: &str,
) -> anyhow::Result<RemoteConfig> {
    try_read_remote_config(repo_path, remote_name)?.with_context(|| {
        format!(
            "Josh remote '{}' is not configured; configure it first",
            remote_name
        )
    })
}

/// Read authoritative named-remote configuration without modifying the repository.
///
/// Returns `None` only when the common-directory `josh/remotes/<name>.josh`
/// file is absent. Invalid or unreadable configuration is an error, not an
/// ordinary Git remote fallback. Obsolete endpoint metadata is rejected.
pub fn try_read_remote_config(
    repo_path: &std::path::Path,
    remote_name: &str,
) -> anyhow::Result<Option<RemoteConfig>> {
    validate_remote_name(remote_name)?;
    let mut repo = gix::open(repo_path)
        .with_context(|| format!("Failed to open repository at {}", repo_path.display()))?;
    try_read_remote_config_from_repo(&mut repo, remote_name)
}

fn try_read_remote_config_from_repo(
    repo: &mut gix::Repository,
    remote_name: &str,
) -> anyhow::Result<Option<RemoteConfig>> {
    let remote_file = repo
        .common_dir()
        .join("josh")
        .join("remotes")
        .join(format!("{remote_name}.josh"));
    // Readers take only the per-remote publication lock. In particular, never
    // acquire the Git config writer lock after this one: writers take it first.
    let _sidecar_lock = match gix::lock::Marker::acquire_to_hold_resource(
        &remote_file,
        gix::lock::acquire::Fail::Immediately,
        None,
    ) {
        Ok(lock) => lock,
        // No remotes directory means there cannot yet be a published sidecar.
        Err(gix::lock::acquire::Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Failed to lock remote config file: {}", remote_file.display())
            });
        }
    };
    // Opening the repository to locate its common directory took a config
    // snapshot before the lock. A writer may have completed since then.
    repo.reload().context("Failed to refresh Git remote configuration")?;

    let content = match std::fs::read_to_string(&remote_file) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(e) => {
            return Err(anyhow!(
                "Failed to read remote config file: {}: {}",
                remote_file.display(),
                e
            ));
        }
    };

    let filter = josh_core::filter::parse(&content)
        .with_context(|| format!("Failed to parse filter from {}", remote_file.display()))?;

    for key in ["url", "fetch", "push", "pushurl"] {
        anyhow::ensure!(
            filter.get_meta(key).is_none(),
            "Obsolete '{}' endpoint metadata in {}; reconfigure remote '{}' to store endpoints in Git config",
            key,
            remote_file.display(),
            remote_name
        );
    }
    let url = config_string(repo, &format!("remote.{remote_name}.url"))?
        .with_context(|| format!("Missing Git remote.{remote_name}.url"))?;
    gix::url::parse(url.as_str()).context("Invalid Git remote URL")?;

    let forge = filter
        .get_meta("forge")
        .map(|f| {
            use clap::ValueEnum;
            Forge::from_str(&f, true)
        })
        .transpose()
        .map_err(|f| anyhow!("Unknown forge: {f}"))?;

    let push_url = config_string(repo, &format!("remote.{remote_name}.pushurl"))?;
    if let Some(push_url) = &push_url {
        gix::url::parse(push_url.as_str()).context("Invalid Git remote push URL")?;
    }

    let gerrit_mode = filter
        .get_meta("gerrit-mode")
        .map(|m| {
            use clap::ValueEnum;
            GerritMode::from_str(&m, true)
        })
        .transpose()
        .map_err(|m| anyhow!("Unknown gerrit-mode: {m}"))?
        .unwrap_or_default();

    Ok(Some(RemoteConfig {
        url,
        ref_spec: format!("+refs/heads/*:refs/josh/remotes/{remote_name}/*"),
        filter_with_meta: filter,
        forge,
        push_url,
        gerrit_mode,
    }))
}

/// Persist real Git endpoints and canonical selection, plus separate Josh metadata.
///
/// Extra settings are single value names relative to `remote.<name>`, and cannot
/// override endpoint or selection keys. Both files are staged before publication;
/// a failed Git config commit restores the previous sidecar under its lock.
/// This is failure-safe for returned errors, not a crash-atomic two-file transaction.
pub fn write_remote_config(
    repo_path: &std::path::Path,
    remote_name: &str,
    url: &str,
    filter: &str,
    forge: Option<Forge>,
    push_url: Option<&str>,
    gerrit_mode: Option<GerritMode>,
    extra_remote_settings: &[(&str, &str)],
) -> anyhow::Result<()> {
    validate_remote_name(remote_name)?;
    anyhow::ensure!(!url.contains('\0'), "Git remote URL must not contain NUL");
    gix::url::parse(url).context("Invalid Git remote URL")?;
    if let Some(push_url) = push_url {
        anyhow::ensure!(
            !push_url.contains('\0'),
            "Git remote push URL must not contain NUL"
        );
        gix::url::parse(push_url).context("Invalid Git remote push URL")?;
    }
    for (index, (key, value)) in extra_remote_settings.iter().enumerate() {
        anyhow::ensure!(
            key.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Invalid extra remote setting key '{key}': expected a single Git variable name"
        );
        anyhow::ensure!(
            !TRANSPORT_META_KEYS
                .iter()
                .any(|reserved| key.eq_ignore_ascii_case(reserved))
                && !key.eq_ignore_ascii_case("filter"),
            "Extra remote setting '{key}' is reserved for endpoint or selection configuration"
        );
        anyhow::ensure!(
            !extra_remote_settings[..index]
                .iter()
                .any(|(previous, _)| key.eq_ignore_ascii_case(previous)),
            "Duplicate extra remote setting '{key}'"
        );
        anyhow::ensure!(
            !value.contains('\0'),
            "Extra remote setting '{key}' must not contain NUL"
        );
    }

    let filter_obj = josh_core::filter::parse(filter)
        .with_context(|| format!("Failed to parse filter '{}'", filter))?;

    // Reject transport metadata instead of silently overwriting it.
    for key in TRANSPORT_META_KEYS {
        if filter_obj.get_meta(key).is_some() {
            return Err(anyhow!(
                "Filter must not set reserved meta key '{}': it is owned by the remote config",
                key
            ));
        }
    }

    let mut filter_with_meta = filter_obj;

    if let Some(forge) = forge {
        filter_with_meta = filter_with_meta.with_meta("forge", forge.to_string());
    }

    if let Some(gerrit_mode) = gerrit_mode {
        filter_with_meta = filter_with_meta.with_meta("gerrit-mode", gerrit_mode.to_string());
    }

    let content = josh_core::filter::as_file(filter_with_meta, 0);

    let repo = gix::open(repo_path)
        .with_context(|| format!("Failed to open repository at {}", repo_path.display()))?;
    let mut config = repo.config_file_mut(repo.config_path(gix::config::Source::Local)?)?;
    // Replace only endpoint/selection keys, preserving authentication and other
    // custom remote settings, including custom upload-pack/receive-pack commands.
    let fetch = format!("+refs/heads/*:refs/remotes/{remote_name}/*");
    for (key, value) in [
        ("url", Some(url)),
        ("pushurl", push_url),
        ("fetch", Some(fetch.as_str())),
    ] {
        let key = format!("remote.{remote_name}.{key}");
        if let Ok(mut values) = config.raw_values_mut(key.as_str()) {
            values.delete_all();
        }
        if let Some(value) = value {
            config.set_raw_value(key.as_str(), value)?;
        }
    }
    let generated_uploadpack = format!("env GIT_NAMESPACE=josh-{remote_name} git upload-pack");
    for (key, generated) in [
        ("uploadpack", generated_uploadpack.as_str()),
        ("receivepack", "false"),
    ] {
        if let Ok(mut values) =
            config.raw_values_mut(format!("remote.{remote_name}.{key}").as_str())
        {
            for (index, value) in values.get()?.iter().enumerate().rev() {
                if value.as_slice() == generated.as_bytes() {
                    values.delete(index);
                }
            }
        }
    }
    // Reconfiguration also retires the old split Git configuration authority.
    for key in ["url", "fetch", "filter"] {
        if let Ok(mut values) =
            config.raw_values_mut(format!("josh-remote.{remote_name}.{key}").as_str())
        {
            values.delete_all();
        }
    }
    for (key, value) in extra_remote_settings {
        let key = format!("remote.{remote_name}.{key}");
        if let Ok(mut values) = config.raw_values_mut(key.as_str()) {
            values.delete_all();
        }
        config.set_raw_value(key.as_str(), *value)?;
    }
    let remotes_dir = repo.common_dir().join("josh").join("remotes");
    std::fs::create_dir_all(&remotes_dir).with_context(|| {
        format!(
            "Failed to create remotes directory: {}",
            remotes_dir.display()
        )
    })?;
    let remote_file = remotes_dir.join(format!("{}.josh", remote_name));
    // Keep this lock through both publication and rollback. The Git transaction
    // is always acquired first, matching other remote configuration writers.
    let _sidecar_lock = gix::lock::Marker::acquire_to_hold_resource(
        &remote_file,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .with_context(|| {
        format!(
            "Failed to lock remote config file: {}",
            remote_file.display()
        )
    })?;
    let stage = || {
        gix::tempfile::new(
            &remotes_dir,
            gix::tempfile::ContainingDirectory::Exists,
            gix::tempfile::AutoRemove::Tempfile,
        )?
        .take()
        .context("Remote config staging file disappeared")
    };
    let mut staged = stage().context("Failed to stage remote config")?;
    staged
        .write_all(content.as_bytes())
        .context("Failed to stage remote config content")?;
    let previous = match std::fs::File::open(&remote_file) {
        Ok(mut file) => {
            let permissions = file
                .metadata()
                .context("Failed to read remote config permissions")?
                .permissions();
            staged.as_file().set_permissions(permissions.clone())?;
            let mut backup = stage().context("Failed to stage remote config rollback")?;
            backup.as_file().set_permissions(permissions)?;
            std::io::copy(&mut file, &mut backup)
                .context("Failed to copy previous remote config")?;
            Some(backup)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Failed to read previous remote config: {}",
                    remote_file.display()
                )
            });
        }
    };
    staged.persist(&remote_file).with_context(|| {
        format!(
            "Failed to publish remote config file: {}",
            remote_file.display()
        )
    })?;
    if let Err(error) = config.commit() {
        let rollback = match previous {
            Some(previous) => previous
                .persist(&remote_file)
                .map(|_| ())
                .map_err(|error| error.error),
            None => std::fs::remove_file(&remote_file),
        };
        return match rollback {
            Ok(()) => Err(error).context("Failed to write Git remote configuration; restored previous Josh remote configuration"),
            Err(rollback) => Err(error).with_context(|| {
                format!(
                    "Failed to write Git remote configuration; also failed to restore {}: {rollback}",
                    remote_file.display()
                )
            }),
        };
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_refreshes_endpoints_after_reconfiguration() {
        let dir = tempfile::tempdir().unwrap();
        gix::init_bare(dir.path()).unwrap();
        write_remote_config(
            dir.path(), "origin", "https://example.com/old", ":/old",
            None, Some("https://example.com/old-push"), None, &[],
        ).unwrap();
        let mut reader = gix::open(dir.path()).unwrap();
        write_remote_config(
            dir.path(), "origin", "https://example.com/new", ":/new",
            Some(Forge::Gerrit), Some("https://example.com/new-push"),
            Some(GerritMode::Stack), &[],
        ).unwrap();

        // This is a real stale gix snapshot, not a simulated config reader.
        assert_eq!(
            config_string(&reader, "remote.origin.url").unwrap().as_deref(),
            Some("https://example.com/old"),
        );
        let config = try_read_remote_config_from_repo(&mut reader, "origin")
            .unwrap().unwrap();
        assert_eq!(config.url, "https://example.com/new");
        assert_eq!(config.push_url.as_deref(), Some("https://example.com/new-push"));
        assert_eq!(josh_core::filter::spec(config.semantic_filter()), ":/new");
        assert_eq!(config.forge, Some(Forge::Gerrit));
        assert_eq!(config.gerrit_mode, GerritMode::Stack);
    }

    #[test]
    fn reader_does_not_observe_uncommitted_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let repo = gix::init_bare(dir.path()).unwrap();
        write_remote_config(
            dir.path(), "origin", "https://example.com/old", ":/old",
            None, None, None, &[],
        ).unwrap();
        // Pause the real publication protocol between its two file commits.
        let mut config = repo.config_file_mut(repo.config_path(gix::config::Source::Local).unwrap())
            .unwrap();
        config.set_raw_value("remote.origin.url", "https://example.com/new").unwrap();
        let remote_file = repo.common_dir().join("josh/remotes/origin.josh");
        let sidecar_lock = gix::lock::Marker::acquire_to_hold_resource(
            &remote_file, gix::lock::acquire::Fail::Immediately, None,
        ).unwrap();
        std::fs::write(&remote_file, ":/new").unwrap();

        assert!(try_read_remote_config(dir.path(), "origin").is_err());
        config.commit().unwrap();
        drop(sidecar_lock);
        let config = read_remote_config(dir.path(), "origin").unwrap();
        assert_eq!(config.url, "https://example.com/new");
        assert_eq!(josh_core::filter::spec(config.semantic_filter()), ":/new");
    }

    #[test]
    fn ordinary_git_repository_has_no_josh_remote() {
        let dir = tempfile::tempdir().unwrap();
        gix::init_bare(dir.path()).unwrap();
        assert!(try_read_remote_config(dir.path(), "origin").unwrap().is_none());
    }
}
