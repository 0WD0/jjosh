//! Native and link observations use ordinary jj names `NAME#PROJECT`.
//! Configured remote names identify endpoints independently of project scope.
//! PROJECT is a project identity, not a mount path.

use std::path::Path;

use anyhow::Context as _;

pub(crate) fn local_name(project: &str, name: &str) -> String {
    format!("{name}#{project}")
}

pub(crate) fn unscoped_name<'a>(project: &str, name: &'a str) -> Option<&'a str> {
    // Native project names cannot contain '#' or '/'. The name itself is
    // opaque: preserve it, including slashes and any earlier '#' characters.
    name.strip_suffix(&format!("#{project}"))
}

pub(crate) fn belongs_to_project(project: &str, name: &str) -> bool {
    unscoped_name(project, name).is_some()
}

pub(crate) fn observation_remote(project: &str, label: &str) -> anyhow::Result<String> {
    let project = crate::native_project::parse_project(project)?;
    let label = crate::native_project::parse_project(label)?;
    Ok(format!("{project}-{label}"))
}

/// Last path component, which must be a project identity.
pub(crate) fn project_from_path(path: &Path) -> anyhow::Result<String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Link path must end with a project name"))?;
    crate::native_project::parse_project(name)
        .with_context(|| format!("Link path {} is not a valid project name", path.display()))
}

pub(crate) fn project_from_link(
    path: &Path,
    link: &josh_core::filter::Filter,
) -> anyhow::Result<String> {
    if let Some(name) = link.get_meta("name") {
        return crate::native_project::parse_project(&name)
            .with_context(|| format!("Link {} has an invalid name", path.display()));
    }
    project_from_path(path)
}
