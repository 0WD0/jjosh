use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use jj_lib::project::Representation;
use josh_core::filter::Filter;

/// Normalize a declarative source context without resolving it against a peer.
pub(crate) fn parse_base(value: &str) -> Result<String> {
    if let Some(pin) = value.strip_prefix("pins/") {
        let oid =
            gix_hash::ObjectId::from_hex(pin.as_bytes()).context("Invalid pinned source base")?;
        return Ok(format!("pins/{oid}"));
    }
    if let Ok(oid) = gix_hash::ObjectId::from_hex(value.as_bytes()) {
        return Ok(format!("pins/{oid}"));
    }
    ensure!(!value.is_empty(), "Source base cannot be empty");
    let reference = if value.starts_with("refs/") {
        value.to_owned()
    } else {
        format!("refs/heads/{value}")
    };
    gix_validate::reference::name_partial(reference.as_bytes().into())
        .with_context(|| format!("Invalid source base {value:?}"))?;
    Ok(reference)
}

fn validated_filter(value: &str) -> Result<Filter> {
    let filter = josh_core::filter::parse(value).context("Invalid Josh source filter")?;
    for key in josh_changes::remote_config::TRANSPORT_META_KEYS {
        ensure!(
            filter.get_meta(key).is_none(),
            "Source filter cannot contain transport metadata {key}"
        );
    }
    Ok(filter)
}

pub(crate) fn parse_filter(value: &str) -> Result<Representation> {
    Ok(Representation::JoshFilter(josh_core::filter::spec(
        validated_filter(value)?,
    )))
}

pub(crate) fn parse_view(value: &str) -> Result<Representation> {
    ensure!(
        !value.is_empty() && !value.contains('\0'),
        "View paths must not be empty or contain NUL"
    );
    let mut path = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            _ => anyhow::bail!("View path must be repository-relative and cannot contain .."),
        }
    }
    // An empty normalized path selects the root workspace.josh, as in preview.
    Ok(Representation::JoshView(
        path.to_str().context("View path is not UTF-8")?.to_owned(),
    ))
}

pub(crate) fn filter(representation: &Representation) -> Result<Filter> {
    match representation {
        Representation::Whole => Ok(Filter::new()),
        Representation::JoshFilter(value) => validated_filter(value),
        Representation::JoshView(value) => {
            if value.is_empty() {
                return Ok(Filter::new().workspace(PathBuf::new()));
            }
            let Representation::JoshView(path) = parse_view(value)? else {
                unreachable!()
            };
            Ok(Filter::new().workspace(path))
        }
    }
}

