use gix::Repository;

mod config;
#[cfg(feature = "excludes")]
mod excludes;
#[cfg(feature = "attributes")]
mod filter;
#[cfg(feature = "merge")]
mod merge;
mod object;
mod open;
#[cfg(feature = "attributes")]
mod pathspec;
mod reference;
mod remote;
mod shallow;
mod state;
#[cfg(feature = "attributes")]
mod submodule;
mod worktree;

#[cfg(feature = "revision")]
mod revision {
    use crate::util::hex_to_id;

    #[test]
    fn date() -> crate::Result {
        let repo = crate::named_repo("make_rev_parse_repo.sh")?;
        let actual = repo
            .rev_parse_single("old@{10 years ago}")
            .expect("it returns the oldest possible rev when overshooting");
        assert_eq!(actual, hex_to_id("be2f093f0588eaeb71e1eff7451b18c2a9b1d765"));

        let actual = repo
            .rev_parse_single("old@{1 month ago}")
            .expect("it finds something in the middle");
        assert_eq!(actual, hex_to_id("b29405fe9147a3a366c4048fbe295ea04de40fa6"));
        Ok(())
    }
}

#[cfg(feature = "index")]
mod index {
    #[test]
    fn basics() -> crate::Result {
        let repo = crate::named_subrepo_opts("make_basic_repo.sh", "unborn", gix::open::Options::isolated())?;
        assert!(
            repo.index_or_load_from_head().is_err(),
            "can't read index if `HEAD^{{tree}}` can't be resolved"
        );
        assert!(
            repo.index_or_load_from_head_or_empty()?.entries().is_empty(),
            "an empty index is created on the fly"
        );
        Ok(())
    }
}

#[cfg(feature = "dirwalk")]
mod dirwalk {
    use gix_dir::entry::Kind::*;
    use gix_dir::walk::EmissionMode;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn basics() -> crate::Result {
        let repo = crate::named_repo("make_basic_repo.sh")?;
        let untracked_only = repo.dirwalk_options()?.emit_untracked(EmissionMode::CollapseDirectory);
        let mut collect = gix::dir::walk::delegate::Collect::default();
        let index = repo.index()?;
        repo.dirwalk(
            &index,
            None::<&str>,
            &AtomicBool::default(),
            untracked_only,
            &mut collect,
        )?;
        let expected = [
            ("all-untracked".to_string(), Repository),
            ("bare-repo-with-index.git".to_string(), Directory),
            ("bare.git".into(), Directory),
            ("empty-core-excludes".into(), Repository),
            ("non-bare-repo-without-index".into(), Repository),
            ("non-bare-without-worktree".into(), Directory),
            ("some".into(), Directory),
            ("unborn".into(), Repository),
        ];
        assert_eq!(
            collect
                .into_entries_by_path()
                .into_iter()
                .map(|e| (e.0.rela_path.to_string(), e.0.disk_kind.expect("kind is known")))
                .collect::<Vec<_>>(),
            expected,
            "note how bare repos are just directories by default"
        );
        let mut iter = repo.dirwalk_iter(index, None::<&str>, Default::default(), untracked_only)?;
        let mut actual: Vec<_> = iter
            .by_ref()
            .map(Result::unwrap)
            .map(|item| {
                (
                    item.entry.rela_path.to_string(),
                    item.entry.disk_kind.expect("kind is known"),
                )
            })
            .collect();
        actual.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(actual, expected, "the iterator works the same");
        let out = iter.into_outcome().expect("iteration done and no error");
        assert_eq!(
            out.dirwalk.returned_entries,
            expected.len(),
            "just a minor sanity check, assuming everything else works as well"
        );
        Ok(())
    }
}

#[test]
fn size_in_memory() {
    let actual_size = std::mem::size_of::<Repository>();
    let limit = 1200;
    assert!(
        actual_size <= limit,
        "size of Repository shouldn't change without us noticing, it's meant to be cloned: should have been below {limit:?}, was {actual_size} (bigger on windows)"
    );
}

#[test]
#[cfg(feature = "parallel")]
fn thread_safe_repository_is_sync() -> crate::Result {
    fn f<T: Send + Sync + Clone>(_t: T) {}
    f(crate::util::basic_repo()?.into_sync());
    Ok(())
}
