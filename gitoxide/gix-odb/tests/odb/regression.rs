mod repo_with_small_packs {

    use gix_object::Find;

    use crate::{db_small_packs, hex_to_id};

    #[test]
    fn all_packed_objects_can_be_found() -> gix_testtools::TestResult {
        let store = db_small_packs();
        let mut buf = Vec::new();
        assert!(
            store
                .try_find(&hex_to_id("ecc68100297fff843a7eef8df0d0fb80c1c8bac5"), &mut buf)?
                .is_some(),
            "object is present and available"
        );
        Ok(())
    }
}
