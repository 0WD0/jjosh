// TODO: tests
use std::path::Path;

use crate::Capabilities;

#[cfg(windows)]
impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            precompose_unicode: false,
            ignore_case: true,
            executable_bit: false,
            symlink: false,
        }
    }
}

#[cfg(target_os = "macos")]
impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            precompose_unicode: true,
            ignore_case: true,
            executable_bit: true,
            symlink: true,
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            precompose_unicode: false,
            ignore_case: false,
            executable_bit: true,
            symlink: true,
        }
    }
}

impl Capabilities {
    /// try to determine all values in this context by probing them in the given `git_dir`, which
    /// should be on the file system the git repository is located on.
    /// `git_dir` is a typical git repository, expected to be populated with the typical files like `config`.
    ///
    /// All errors are ignored and interpreted on top of the default for the platform the binary is compiled for.
    pub fn probe(git_dir: &Path) -> Self {
        let ctx = Capabilities::default();
        Capabilities {
            symlink: Self::probe_symlink(git_dir).unwrap_or(ctx.symlink),
            ignore_case: Self::probe_ignore_case(git_dir).unwrap_or(ctx.ignore_case),
            precompose_unicode: Self::probe_precompose_unicode(git_dir).unwrap_or(ctx.precompose_unicode),
            executable_bit: Self::probe_file_mode(git_dir).unwrap_or(ctx.executable_bit),
        }
    }

    #[cfg(unix)]
    fn probe_file_mode(root: &Path) -> std::io::Result<bool> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        // First check that we can create an executable file, then check that we
        // can change the executable bit.
        // The equivalent test by git itself is here:
        // https://github.com/git/git/blob/f0ef5b6d9bcc258e4cbef93839d1b7465d5212b9/setup.c#L2367-L2379
        let rand = fastrand::usize(..);
        let test_path = root.join(format!("_test_executable_bit{rand}"));
        let res = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o777)
            .open(&test_path)
            .and_then(|file| {
                let old_mode = file.metadata()?.mode();
                let is_executable = old_mode & 0o100 == 0o100;
                Ok(is_executable && {
                    let new_mode = old_mode ^ 0o100;
                    match file.set_permissions(PermissionsExt::from_mode(new_mode)) {
                        Ok(()) => new_mode == file.metadata()?.mode(),
                        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => false,
                        Err(err) => return Err(err),
                    }
                })
            });
        std::fs::remove_file(test_path)?;
        res
    }

    #[cfg(not(unix))]
    fn probe_file_mode(_root: &Path) -> std::io::Result<bool> {
        Ok(false)
    }

    fn probe_ignore_case(git_dir: &Path) -> std::io::Result<bool> {
        std::fs::metadata(git_dir.join("cOnFiG")).map(|_| true).or_else(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(err)
            }
        })
    }

    fn probe_precompose_unicode(root: &Path) -> std::io::Result<bool> {
        let rand = fastrand::usize(..);
        let precomposed = format!("ä{rand}");
        let decomposed = format!("a\u{308}{rand}");

        let precomposed = root.join(precomposed);
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&precomposed)?;
        let res = root.join(decomposed).symlink_metadata().map(|_| true);
        std::fs::remove_file(precomposed)?;
        res
    }

    fn probe_symlink(root: &Path) -> std::io::Result<bool> {
        let rand = fastrand::usize(..);
        let link_path = root.join(format!("__file_link{rand}"));
        if crate::symlink::create("dangling".as_ref(), &link_path).is_err() {
            return Ok(false);
        }

        let res = std::fs::symlink_metadata(&link_path).map(|m| m.file_type().is_symlink());
        crate::symlink::remove(&link_path).or_else(|_| std::fs::remove_file(&link_path))?;
        res
    }
}
