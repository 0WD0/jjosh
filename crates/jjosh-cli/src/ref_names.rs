//! Native and projected observations use ordinary jj names `NAME#PROJECT`.
//! Configured remote names identify endpoints independently of project scope.
//! PROJECT is a project identity, not a mount path.

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

