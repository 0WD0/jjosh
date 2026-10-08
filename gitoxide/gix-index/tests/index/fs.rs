mod metadata {
    use gix_index::fs::Metadata;
    use gix_testtools::TestResult;

    #[test]
    fn from_path_no_follow() -> TestResult {
        let root = crate::scripted_fixture_read_only("file_metadata.sh")?;

        // For now, don't assert on the values of the metadata as these depends on the filesystem,
        // which might truncate it, or fail entirely.
        for filename in ["future", "past"] {
            let meta = Metadata::from_path_no_follow(&root.join(filename))?;
            assert!(meta.created().is_some());
            assert!(meta.modified().is_some());
            assert_eq!(meta.len(), 0);
        }
        Ok(())
    }
}
