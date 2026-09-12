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
    let repo = gix::open(repo_path)
        .with_context(|| format!("Failed to open repository at {}", repo_path.display()))?;
    let remote_file = repo
        .common_dir()
        .join("josh")
        .join("remotes")
        .join(format!("{remote_name}.josh"));

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
    let url = config_string(&repo, &format!("remote.{remote_name}.url"))?
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

    let push_url = config_string(&repo, &format!("remote.{remote_name}.pushurl"))?;
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
pub fn write_remote_config(
    repo_path: &std::path::Path,
    remote_name: &str,
    url: &str,
    filter: &str,
    forge: Option<Forge>,
    push_url: Option<&str>,
    gerrit_mode: Option<GerritMode>,
) -> anyhow::Result<()> {
    validate_remote_name(remote_name)?;
    gix::url::parse(url).context("Invalid Git remote URL")?;
    if let Some(push_url) = push_url {
        gix::url::parse(push_url).context("Invalid Git remote push URL")?;
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
    let remotes_dir = repo.common_dir().join("josh").join("remotes");
    std::fs::create_dir_all(&remotes_dir).with_context(|| {
        format!(
            "Failed to create remotes directory: {}",
            remotes_dir.display()
        )
    })?;
    let remote_file = remotes_dir.join(format!("{}.josh", remote_name));
    std::fs::write(&remote_file, content).with_context(|| {
        format!(
            "Failed to write remote config file: {}",
            remote_file.display()
        )
    })?;
    config
        .commit()
        .context("Failed to write Git remote configuration")?;

    Ok(())
}
