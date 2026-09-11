//! Opt-in naming convention for the native workflow demo, not a jj namespace.
//! Keep the existing project-remote names and all conversion/transport logic.

use jj_lib::config::ConfigGetError;
use jj_lib::config::ConfigGetResultExt as _;
use jj_lib::settings::UserSettings;

pub(crate) fn use_scope_suffix(settings: &UserSettings) -> Result<bool, ConfigGetError> {
    Ok(settings
        .get_bool("jjosh.demo-scope-suffix")
        .optional()?
        .unwrap_or(false))
}

pub(crate) fn local_name(project: &str, name: &str, suffix: bool) -> String {
    if suffix {
        format!("{name}#{project}")
    } else {
        format!("{project}/{name}")
    }
}

pub(crate) fn belongs_to_project(project: &str, name: &str, suffix: bool) -> bool {
    // Native project names cannot contain '#' or '/'. The name itself is
    // opaque: preserve it, including slashes and any earlier '#' characters.
    if suffix {
        name.rsplit_once('#')
            .is_some_and(|(_, scope)| scope == project)
    } else {
        name.strip_prefix(project)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}
