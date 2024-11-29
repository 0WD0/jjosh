pub(super) mod function {
    use anyhow::{bail, Context};
    use gix::bstr::ByteSlice;
    use std::ffi::OsString;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    pub fn check_mode() -> anyhow::Result<()> {
        let root = find_root()?;
        let mut mismatch = false;

        let cmd = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "-sz", "--", "*.sh"])
            .stdout(Stdio::piped())
            .spawn()
            .context("Can't run `git` to list index")?;

        let stdout = cmd.stdout.expect("should have captured stdout");
        let reader = BufReader::new(stdout);
        for record in reader.split(b'\0') {
            // FIXME: Use the record, displaying messages and updating `mismatch`.
        }

        // FIXME: If `cmd` did not report successful completion, bail.
        // FIXME: If `mismatch` (any mismatches), bail.
        bail!("not yet implemented");
    }

    fn find_root() -> anyhow::Result<OsString> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("Can't run `git` to find worktree root")?;

        if !output.status.success() {
            bail!("`git` failed to find worktree root");
        }

        let root = output
            .stdout
            .strip_suffix(b"\n")
            .context("Can't parse worktree root")?
            .to_os_str()?
            .to_owned();

        Ok(root)
    }
}
