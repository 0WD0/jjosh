use std::path::Path;

use gix_date::parse::TimeBuf;
use gix_lock::acquire::Fail;
use gix_ref::{
    Target,
    file::transaction::{PackedRefs, prepare::Error},
    transaction::{PreviousValue, RefEdit},
};

use crate::file::{
    odb_at, store_writable,
    transaction::prepare_and_commit::{committer, empty_store},
};

fn assert_locked(path: impl AsRef<Path>) {
    assert!(matches!(
        gix_lock::File::acquire_to_update_resource(path, Fail::Immediately, None),
        Err(gix_lock::acquire::Error::PermanentlyLocked { .. })
    ));
}

#[test]
fn absent_verification_holds_loose_and_packed_locks_until_commit_or_drop() -> crate::Result {
    for commit in [false, true] {
        let (dir, store) = empty_store()?;
        let name = "refs/heads/missing";
        let log_path = dir.path().join("logs").join(name);
        std::fs::create_dir_all(log_path.parent().unwrap())?;
        std::fs::write(&log_path, b"orphaned reflog must remain untouched\n")?;
        let transaction = store.transaction().prepare(
            [RefEdit::verify(name.try_into()?, PreviousValue::MustNotExist)],
            Fail::Immediately,
            Fail::Immediately,
        )?;
        assert_locked(dir.path().join(name));
        assert_locked(store.packed_refs_path());
        if commit {
            transaction.commit(None)?;
        } else {
            drop(transaction);
        }
        assert!(store.try_find(name)?.is_none());
        assert!(!dir.path().join(name).exists());
        assert!(!store.packed_refs_path().exists());
        assert_eq!(std::fs::read(&log_path)?, b"orphaned reflog must remain untouched\n");
        drop(gix_lock::File::acquire_to_update_resource(
            dir.path().join(name),
            Fail::Immediately,
            Some(dir.path().to_owned()),
        )?);
        drop(gix_lock::File::acquire_to_update_resource(
            store.packed_refs_path(),
            Fail::Immediately,
            None,
        )?);
    }
    Ok(())
}

#[test]
fn direct_verification_rejects_existing_or_mismatched_targets_and_preserves_ref_and_log() -> crate::Result {
    let (_keep, store) = store_writable("make_repo_for_reflog.sh")?;
    let reference = store.find("refs/heads/main")?;
    let path = store.git_dir().join(reference.name.as_bstr().to_string());
    let log_path = store.git_dir().join("logs/refs/heads/main");
    let contents = std::fs::read(&path)?;
    let log = std::fs::read(&log_path)?;
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify(reference.name.clone(), PreviousValue::MustNotExist)],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::VerifyMustNotExist { actual, .. }) if actual == reference.target
    ));
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify(
                reference.name.clone(),
                PreviousValue::MustExistAndMatch(Target::Symbolic("refs/heads/other".try_into()?)),
            )],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::ReferenceOutOfDate { actual, .. }) if actual == reference.target
    ));
    let transaction = store.transaction().prepare(
        [RefEdit::verify(
            reference.name.clone(),
            PreviousValue::MustExistAndMatch(reference.target.clone()),
        )],
        Fail::Immediately,
        Fail::Immediately,
    )?;
    assert_locked(&path);
    transaction.commit(None)?;
    assert_eq!(std::fs::read(&path)?, contents);
    assert_eq!(std::fs::read(&log_path)?, log);
    assert!(!path.with_extension("lock").exists());
    Ok(())
}

#[test]
fn packed_verification_checks_the_target_without_creating_a_loose_ref_or_rewriting_the_pack() -> crate::Result {
    let (_keep, store) = store_writable("make_packed_ref_repository.sh")?;
    let reference = store.find("refs/heads/main")?;
    let path = store.git_dir().join("refs/heads/main");
    let packed_before = std::fs::read(store.packed_refs_path())?;
    let log_path = store.git_dir().join("logs/refs/heads/main");
    let log_before = std::fs::read(&log_path)?;
    assert!(!path.exists());
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify(reference.name.clone(), PreviousValue::MustNotExist)],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::VerifyMustNotExist { actual, .. }) if actual == reference.target
    ));
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify(
                reference.name.clone(),
                PreviousValue::ExistingMustMatch(Target::Symbolic("refs/heads/other".try_into()?)),
            )],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::ReferenceOutOfDate { .. })
    ));
    let transaction = store.transaction().prepare(
        [RefEdit::verify(
            reference.name.clone(),
            PreviousValue::MustExistAndMatch(reference.target.clone()),
        )],
        Fail::Immediately,
        Fail::Immediately,
    )?;
    assert_locked(&path);
    assert_locked(store.packed_refs_path());
    transaction.commit(None)?;
    assert!(!path.exists());
    assert_eq!(std::fs::read(store.packed_refs_path())?, packed_before);
    assert_eq!(std::fs::read(&log_path)?, log_before);
    Ok(())
}

#[test]
fn verification_and_real_updates_can_share_a_packed_transaction() -> crate::Result {
    let (_keep, store) = store_writable("make_packed_ref_repository.sh")?;
    let reference = store.find("refs/heads/main")?;
    let odb = odb_at(store.git_dir().join("objects"))?;
    let transaction = store
        .transaction()
        .packed_refs(PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(
            Box::new(odb),
        ))
        .prepare(
            [
                RefEdit::verify(
                    reference.name.clone(),
                    PreviousValue::MustExistAndMatch(reference.target.clone()),
                ),
                RefEdit::update(
                    "refs/heads/new".try_into()?,
                    reference.target.clone(),
                    PreviousValue::MustNotExist,
                    "new branch",
                ),
            ],
            Fail::Immediately,
            Fail::Immediately,
        )?;
    assert_locked(store.git_dir().join("refs/heads/main"));
    transaction.commit(committer().to_ref(&mut TimeBuf::default()))?;
    assert_eq!(store.find("refs/heads/main")?.target, reference.target);
    assert_eq!(store.find("refs/heads/new")?.target, reference.target);
    assert!(store.try_find_loose("refs/heads/main")?.is_none());
    assert!(store.try_find_loose("refs/heads/new")?.is_none());
    Ok(())
}

#[test]
fn namespaced_packed_verification_uses_the_store_namespace() -> crate::Result {
    let (dir, mut store) = empty_store()?;
    let target = crate::hex_to_id("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    std::fs::write(
        store.packed_refs_path(),
        format!("{target} refs/namespaces/foo/refs/heads/main\n"),
    )?;
    store.namespace = gix_ref::namespace::expand("foo")?.into();
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify("refs/heads/main".try_into()?, PreviousValue::MustNotExist)],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::VerifyMustNotExist { actual, .. }) if actual == Target::Object(target)
    ));
    let transaction = store.transaction().prepare(
        [RefEdit::verify(
            "refs/heads/main".try_into()?,
            PreviousValue::MustExistAndMatch(Target::Object(target)),
        )],
        Fail::Immediately,
        Fail::Immediately,
    )?;
    assert_locked(dir.path().join("refs/namespaces/foo/refs/heads/main"));
    assert!(!dir.path().join("refs/heads/main.lock").exists());
    transaction.commit(None)?;
    assert_eq!(store.find("refs/heads/main")?.target, Target::Object(target));
    Ok(())
}

#[test]
fn dereferenced_verification_locks_the_symbolic_chain_without_writing_logs() -> crate::Result {
    let (_keep, store) = store_writable("make_repo_for_reflog.sh")?;
    let target = store.find("refs/heads/main")?.target;
    let head_path = store.git_dir().join("HEAD");
    let head = std::fs::read(&head_path)?;
    let log_path = store.git_dir().join("logs/HEAD");
    let log = std::fs::read(&log_path)?;
    let transaction = store.transaction().prepare(
        [RefEdit::verify("HEAD".try_into()?, PreviousValue::MustExistAndMatch(target)).with_deref(true)],
        Fail::Immediately,
        Fail::Immediately,
    )?;
    assert_locked(&head_path);
    assert_locked(store.git_dir().join("refs/heads/main"));
    transaction.commit(None)?;
    assert_eq!(std::fs::read(&head_path)?, head);
    assert_eq!(std::fs::read(&log_path)?, log);
    Ok(())
}

#[test]
fn missing_required_and_invalid_loose_refs_cannot_be_verified() -> crate::Result {
    let (dir, store) = empty_store()?;
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify("HEAD".try_into()?, PreviousValue::MustExist)],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::VerifyMustExist { .. })
    ));
    std::fs::write(dir.path().join("HEAD"), b"invalid reference\n")?;
    assert!(matches!(
        store.transaction().prepare(
            [RefEdit::verify("HEAD".try_into()?, PreviousValue::MustNotExist)],
            Fail::Immediately,
            Fail::Immediately,
        ),
        Err(Error::ReferenceDecode(_))
    ));
    assert!(!dir.path().join("HEAD.lock").exists());
    Ok(())
}

#[test]
fn locked_edits_excludes_released_no_op_updates_but_includes_verifications_and_changes() -> crate::Result {
    let (_keep, store) = store_writable("make_repo_for_reflog.sh")?;
    let target = store.find("refs/heads/main")?.target;
    let transaction = store.transaction().prepare(
        [
            RefEdit::update(
                "refs/heads/main".try_into()?,
                target.clone(),
                PreviousValue::MustExistAndMatch(target.clone()),
                "unchanged",
            ),
            RefEdit::verify("HEAD".try_into()?, PreviousValue::MustExist),
            RefEdit::update(
                "refs/heads/new".try_into()?,
                target,
                PreviousValue::MustNotExist,
                "new branch",
            ),
        ],
        Fail::Immediately,
        Fail::Immediately,
    )?;
    assert_eq!(
        transaction
            .locked_edits()
            .map(|edit| edit.name.to_string())
            .collect::<Vec<_>>(),
        ["HEAD", "refs/heads/new"]
    );
    drop(gix_lock::File::acquire_to_update_resource(
        store.git_dir().join("refs/heads/main"),
        Fail::Immediately,
        None,
    )?);
    assert_locked(store.git_dir().join("HEAD"));
    assert_locked(store.git_dir().join("refs/heads/new"));
    drop(transaction);
    Ok(())
}
