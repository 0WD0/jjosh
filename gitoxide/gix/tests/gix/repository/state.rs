use crate::named_repo;

#[test]
fn apply_mailbox() -> gix_testtools::TestResult {
    let repo = named_repo("make_am_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::ApplyMailbox));
    Ok(())
}

#[test]
fn bisect() -> gix_testtools::TestResult {
    let repo = named_repo("make_bisect_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::Bisect));

    Ok(())
}

#[test]
fn cherry_pick() -> gix_testtools::TestResult {
    let repo = named_repo("make_cherry_pick_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::CherryPick));
    Ok(())
}

#[test]
fn cherry_pick_sequence() -> gix_testtools::TestResult {
    let repo = named_repo("make_cherry_pick_sequence_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::CherryPickSequence));

    Ok(())
}

#[test]
fn merge() -> gix_testtools::TestResult {
    let repo = named_repo("make_merge_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::Merge));

    Ok(())
}

#[test]
fn rebase_interactive() -> gix_testtools::TestResult {
    let repo = named_repo("make_rebase_i_repo.sh")?;

    assert!(repo.head()?.is_detached());
    assert_eq!(repo.state(), Some(gix::state::InProgress::RebaseInteractive));

    Ok(())
}

#[test]
fn revert() -> gix_testtools::TestResult {
    let repo = named_repo("make_revert_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::Revert));

    Ok(())
}

#[test]
fn revert_sequence() -> gix_testtools::TestResult {
    let repo = named_repo("make_revert_sequence_repo.sh")?;

    assert_eq!(repo.head_name()?.unwrap().shorten(), "main");
    assert_eq!(repo.state(), Some(gix::state::InProgress::RevertSequence));

    Ok(())
}
