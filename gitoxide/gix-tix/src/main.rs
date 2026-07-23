#![forbid(unsafe_code)]

use std::ffi::OsString;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let (revisions, quit_on_finish) = arguments(gix::env::args_os().skip(1));
    if revisions.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "Usage: tix [--quit-on-finish] [REVISION]...\n\nBrowse commits reachable from HEAD or the given revisions."
        );
        return Ok(());
    }

    let current_dir = std::env::current_dir().context("could not determine current directory")?;
    let repository = gix::ThreadSafeRepository::discover_with_environment_overrides(current_dir)
        .context("could not discover repository")?;
    gix_tix::run(repository, revisions, gix_tix::Options { quit_on_finish })
}

fn arguments(args: impl Iterator<Item = OsString>) -> (Vec<OsString>, bool) {
    let mut quit_on_finish = false;
    let revisions = args
        .filter(|arg| {
            let is_option = arg == "--quit-on-finish";
            quit_on_finish |= is_option;
            !is_option
        })
        .collect();
    (revisions, quit_on_finish)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn separates_options_from_revisions() {
        let (revisions, quit_on_finish) = arguments(["--quit-on-finish", "main"].into_iter().map(OsString::from));

        assert!(quit_on_finish);
        assert_eq!(revisions, ["main"], "only revisions remain");
    }
}
