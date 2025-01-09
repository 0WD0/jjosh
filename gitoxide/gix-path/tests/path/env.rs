#[test]
fn exe_invocation() {
    let actual = gix_path::env::exe_invocation();
    assert!(
        !actual.as_os_str().is_empty(),
        "it finds something as long as git is installed somewhere on the system (or a default location)"
    );
}

#[test]
fn login_shell() {
    // On CI, the $SHELL variable isn't necessarily set. Maybe other ways to get the login shell should be used then.
    if !gix_testtools::is_ci::cached() {
        assert!(gix_path::env::login_shell()
            .expect("There should always be the notion of a shell used by git")
            .exists());
    }
}

#[test]
fn installation_config() {
    assert_ne!(
        gix_path::env::installation_config().map(|p| p.components().count()),
        gix_path::env::installation_config_prefix().map(|p| p.components().count()),
        "the prefix is a bit shorter than the installation config path itself"
    );
}

#[test]
fn system_prefix() {
    assert_ne!(
        gix_path::env::system_prefix(),
        None,
        "git should be present when running tests"
    );
}

#[test]
fn home_dir() {
    assert_ne!(
        gix_path::env::home_dir(),
        None,
        "we find a home on every system these tests execute"
    );
}

mod xdg_config {
    use std::ffi::OsStr;

    #[test]
    fn prefers_xdg_config_bases() {
        let actual = gix_path::env::xdg_config("test", &mut |n| {
            (n == OsStr::new("XDG_CONFIG_HOME")).then(|| "marker".into())
        })
        .expect("set");
        #[cfg(unix)]
        assert_eq!(actual.to_str(), Some("marker/git/test"));
        #[cfg(windows)]
        assert_eq!(actual.to_str(), Some("marker\\git\\test"));
    }

    #[test]
    fn falls_back_to_home() {
        let actual = gix_path::env::xdg_config("test", &mut |n| (n == OsStr::new("HOME")).then(|| "marker".into()))
            .expect("set");
        #[cfg(unix)]
        assert_eq!(actual.to_str(), Some("marker/.config/git/test"));
        #[cfg(windows)]
        assert_eq!(actual.to_str(), Some("marker\\.config\\git\\test"));
    }
}
