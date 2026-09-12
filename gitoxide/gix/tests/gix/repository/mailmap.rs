use crate::{Result, named_repo};

#[test]
fn empty_when_no_mailmap_present() -> Result {
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
fn reads_existing_mailmap_from_worktree_root() -> Result {
    let repo = named_repo("make_mailmap_repo.sh")?;
    let snapshot = repo.open_mailmap();
    assert_eq!(
        snapshot.entries().len(),
        1,
        "the single entry from the worktree .mailmap is loaded"
    );
    Ok(())
}
