use gix::prelude::ObjectIdExt;

use crate::{
    revision::spec::from_bytes::{parse_spec_no_baseline, repo},
    util::hex_to_id,
};

mod with_known_revision {
    use gix::revision::Spec;

    use super::*;
    use crate::revision::spec::from_bytes::parse_spec;

    #[test]
    #[cfg(not(feature = "revparse-regex"))]
    fn contained_string_matches_in_unanchored_regex_and_disambiguates_automatically() {
        let repo = repo("ambiguous_blob_tree_commit").unwrap();
        let expected = Spec::from_id(hex_to_id("0000000000e4f9fbd19cf1e932319e5ad0d1d00b").attach(&repo));

        assert_eq!(parse_spec("0000000000^{/x}", &repo).unwrap(), expected);
        assert_eq!(parse_spec("@^{/x}", &repo).unwrap(), expected, "ref names are resolved");

        assert_eq!(
            parse_spec_no_baseline("@^{/.*x}", &repo).unwrap_err().to_string(),
            "None of 1 commits from 0000000000e matched text \".*x\"",
            "regexes are not actually available for us, but git could do that"
        );
    }

    #[test]
    #[cfg(feature = "revparse-regex")]
    fn contained_string_matches_in_unanchored_regex_and_disambiguates_automatically() {
        let repo = repo("ambiguous_blob_tree_commit").unwrap();
        let expected = Spec::from_id(hex_to_id("0000000000e4f9fbd19cf1e932319e5ad0d1d00b").attach(&repo));

        assert_eq!(
            parse_spec("0000000000^{/x}", &repo).unwrap(),
            expected,
            "search is unanchored by default"
        );
        assert_eq!(
            parse_spec("@^{/x}", &repo).unwrap(),
            expected,
            "ref names are resolved as well"
        );

        assert_eq!(
            parse_spec("@^{/^.*x}", &repo).unwrap(),
            expected,
            "we can use real regexes here"
        );
        assert_eq!(
            parse_spec_no_baseline("@^{/^x}", &repo).unwrap_err().to_string(),
            "None of 1 commits from 0000000000e matched regex \"^x\"",
        );
    }
}

mod find_youngest_matching_commit {
    use gix::revision::Spec;

    use super::*;
    use crate::revision::spec::from_bytes::parse_spec;

    #[test]
    #[cfg(not(feature = "revparse-regex"))]
    fn contained_string_matches() {
        let repo = repo("complex_graph").unwrap();

        // See the comment on `skip_some_baselines` in the `regex_matches` test function below.
        let skip_some_baselines = !is_ci::cached()
            && std::env::var_os("GIX_TEST_IGNORE_ARCHIVES").is_some()
            && ((2, 47, 0)..(2, 48, 0)).contains(&gix_testtools::GIT_VERSION);

        if skip_some_baselines {
            assert_eq!(
                parse_spec_no_baseline(":/message", &repo).unwrap(),
                Spec::from_id(hex_to_id("ef80b4b77b167f326351c93284dc0eb00dd54ff4").attach(&repo))
            );
        } else {
            assert_eq!(
                parse_spec(":/message", &repo).unwrap(),
                Spec::from_id(hex_to_id("ef80b4b77b167f326351c93284dc0eb00dd54ff4").attach(&repo))
            );
        }

        assert_eq!(
            parse_spec("@^{/!-B}", &repo).unwrap(),
            Spec::from_id(hex_to_id("55e825ebe8fd2ff78cad3826afb696b96b576a7e").attach(&repo)),
            "negations work as well"
        );

        if skip_some_baselines {
            assert_eq!(
                parse_spec_no_baseline(":/!-message", &repo).unwrap(),
                Spec::from_id(hex_to_id("55e825ebe8fd2ff78cad3826afb696b96b576a7e").attach(&repo))
            );
        } else {
            assert_eq!(
                parse_spec(":/!-message", &repo).unwrap(),
                Spec::from_id(hex_to_id("55e825ebe8fd2ff78cad3826afb696b96b576a7e").attach(&repo))
            );
        }

        assert_eq!(
            parse_spec_no_baseline(":/messa.e", &repo).unwrap_err().to_string(),
            "None of 10 commits reached from all references matched text \"messa.e\"",
            "regex definitely don't work as it's not compiled in"
        );
    }

    #[test]
    #[cfg(feature = "revparse-regex")]
    fn regex_matches() {
        let repo = repo("complex_graph").unwrap();

        // Traversal order with `:/` was broken in Git 2.47.*, so some `parse_spec` assertions
        // fail. The fix is in Git 2.48.* but is not backported. This causes incorrect baselines to
        // be computed when `GIX_TEST_IGNORE_ARCHIVES` is set. If that is not set, then archived
        // baselines are used and there is no problem. On CI, we assume a sufficiently new version
        // of Git. Otherwise, if `GIX_TEST_IGNORE_ARCHIVES` is set and Git 2.47.* is used, we skip
        // the baseline check, to allow the rest of the test to proceed. This accommodates local
        // development environments with a system-provided Git 2.47.*, though archives generated on
        // such a system should not be committed, as they would still contain incorrect baselines.
        // Please note that this workaround may be removed in the future. For more details, see:
        //
        //  - https://lore.kernel.org/git/Z1LJSADiStlFicTL@pks.im/T/
        //  - https://lore.kernel.org/git/Z1LtS-8f8WZyobz3@pks.im/T/
        //  - https://github.com/git/git/blob/v2.48.0/Documentation/RelNotes/2.48.0.txt#L294-L296
        //  - https://github.com/GitoxideLabs/gitoxide/issues/1622
        let skip_some_baselines = !is_ci::cached()
            && std::env::var_os("GIX_TEST_IGNORE_ARCHIVES").is_some()
            && ((2, 47, 0)..(2, 48, 0)).contains(&gix_testtools::GIT_VERSION);

        if skip_some_baselines {
            assert_eq!(
                parse_spec_no_baseline(":/mes.age", &repo).unwrap(),
                Spec::from_id(hex_to_id("ef80b4b77b167f326351c93284dc0eb00dd54ff4").attach(&repo))
            );
        } else {
            assert_eq!(
                parse_spec(":/mes.age", &repo).unwrap(),
                Spec::from_id(hex_to_id("ef80b4b77b167f326351c93284dc0eb00dd54ff4").attach(&repo))
            );
        }

        assert_eq!(
            parse_spec(":/not there", &repo).unwrap_err().to_string(),
            "None of 10 commits reached from all references matched regex \"not there\""
        );

        if skip_some_baselines {
            assert_eq!(
                parse_spec_no_baseline(":/!-message", &repo).unwrap(),
                Spec::from_id(hex_to_id("55e825ebe8fd2ff78cad3826afb696b96b576a7e").attach(&repo))
            );
        } else {
            assert_eq!(
                parse_spec(":/!-message", &repo).unwrap(),
                Spec::from_id(hex_to_id("55e825ebe8fd2ff78cad3826afb696b96b576a7e").attach(&repo))
            );
        }

        assert_eq!(
            parse_spec("@^{/!-B}", &repo).unwrap(),
            Spec::from_id(hex_to_id("55e825ebe8fd2ff78cad3826afb696b96b576a7e").attach(&repo)),
            "negations work as well"
        );
    }
}
