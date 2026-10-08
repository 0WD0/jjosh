use crate::named_repo;

#[test]
fn empty_when_no_mailmap_present() -> gix_testtools::TestResult {
    let repo = named_repo("make_basic_repo.sh")?;
    let snapshot = repo.open_mailmap();
    assert!(
        snapshot.entries().is_empty(),
        "a repo without any .mailmap or mailmap.* config yields an empty snapshot"
    );

    let mut into = gix_mailmap::Snapshot::default();
    repo.open_mailmap_into(&mut into)?;
    assert!(
        into.entries().is_empty(),
        "open_mailmap_into mirrors open_mailmap when there are no sources"
    );
    Ok(())
}

#[test]
#[cfg(any(unix, windows))]
fn worktree_symlinks_are_rejected_but_configured_mailmaps_may_follow_them() -> gix_testtools::TestResult {
    let dir = gix_testtools::scripted_fixture_read_only("make_symlinked_mailmap_repo.sh")?;
    if !gix_testtools::fixture_has_symlinks(&dir)? {
        return Ok(());
    }
    let worktree = dir.join("repo");
    let mailmap = worktree.join(".mailmap");
    assert!(
        mailmap.symlink_metadata()?.file_type().is_symlink(),
        "the worktree .mailmap must be a symlink, not a copied regular file"
    );
    let mut repo = gix::open_opts(&worktree, crate::util::restricted())?;

    let mut snapshot = gix_mailmap::Snapshot::default();
    let err = repo
        .open_mailmap_into(&mut snapshot)
        .expect_err("worktree symlinks must be reported without reading their targets");
    assert!(
        err.to_string().contains("symlink"),
        "the error identifies the skipped symlink"
    );
    assert!(snapshot.entries().is_empty(), "the symlink target must not be loaded");
    assert!(
        repo.open_mailmap().entries().is_empty(),
        "ignoring the error must not expose the symlink target either"
    );

    repo.config_snapshot_mut()
        .set_raw_value("mailmap.file", gix::path::into_bstr(mailmap.as_path())?)?;
    assert!(
        repo.open_mailmap_into(&mut snapshot).is_err(),
        "the worktree symlink error is still reported while other sources are loaded"
    );
    assert_eq!(
        snapshot.entries().len(),
        1,
        "an explicitly configured mailmap may follow the same symlink"
    );
    Ok(())
}

#[test]
fn reads_existing_mailmap_from_worktree_root() -> gix_testtools::TestResult {
    let repo = named_repo("make_mailmap_repo.sh")?;
    let snapshot = repo.open_mailmap();
    assert_eq!(
        snapshot.entries().len(),
        1,
        "the single entry from the worktree .mailmap is loaded"
    );
    Ok(())
}
