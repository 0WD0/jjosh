use gix_attributes::Source;

#[test]
fn system_attributes_with_mingw64() -> gix_testtools::Result {
    system_attributes("mingw64", false)
}

#[test]
fn system_attributes_with_ucrt64() -> gix_testtools::Result {
    system_attributes("ucrt64", false)
}

#[test]
fn system_attributes_with_mixed_prefixes_and_mingw64_active() -> gix_testtools::Result {
    system_attributes("mingw64", true)
}

#[test]
fn system_attributes_with_mixed_prefixes_and_ucrt64_active() -> gix_testtools::Result {
    system_attributes("ucrt64", true)
}

fn system_attributes(runtime: &str, mixed: bool) -> gix_testtools::Result {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    let installation = gix_testtools::tempfile::tempdir()?;
    if mixed {
        for directory in ["mingw64", "ucrt64"] {
            std::fs::create_dir(installation.path().join(directory))?;
        }
    } else {
        std::fs::create_dir(installation.path().join(runtime))?;
    }
    // GIT_EXEC_PATH selects the active runtime when EXEPATH is ambiguous.
    // Git for Windows builds ETC_GITATTRIBUTES as ../etc/gitattributes for both runtimes.
    let core_dir = installation.path().join(runtime).join("libexec/git-core");
    let _env = gix_testtools::Env::new()
        .set("EXEPATH", installation.path().to_str().expect("UTF-8 temporary path"))
        .set("GIT_EXEC_PATH", core_dir.to_str().expect("UTF-8 temporary path"));
    assert_eq!(
        Source::System.storage_location(&mut |_| None),
        Some(installation.path().join("etc/gitattributes")),
        "system attributes stay in the top-level etc directory with either prefix discovery strategy"
    );
    assert_eq!(
        Source::System.storage_location(&mut |name| (name == "GIT_ATTR_NOSYSTEM").then(|| "1".into())),
        None,
        "disabling system attributes still takes precedence over path discovery"
    );
    Ok(())
}
