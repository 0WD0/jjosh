mod fuzzed {
    #[test]
    fn ewah_artifacts_run_fuzzer() {
        for path in artifact_paths("ewah") {
            let data = std::fs::read(path).expect("artifact is readable");
            let _ = gix_bitmap::ewah::decode(&data);
        }
    }

    #[test]
    fn runaway_run_length_is_rejected() {
        let (bitmap, rest) = gix_bitmap::ewah::decode(include_bytes!(
            "../fuzz/artifacts/ewah/slow-unit-ac817962d1a6c123d4d1f73860f5b779423ed171"
        ))
        .expect("fixture must decode");

        assert!(rest.is_empty(), "fixture should be fully consumed");
        assert_eq!(
            bitmap.for_each_set_bit(|_| Some(())),
            None,
            "impossible run lengths must be rejected instead of iterating unboundedly"
        );
    }

    #[test]
    fn non_zero_padding_bits_in_last_literal_word_are_rejected() {
        let mut data = Vec::new();
        // One logical bit, two compressed words, then RLW position 0.
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&2u32.to_be_bytes());
        // The only in-range bit is bit 0 and it is unset; bit 33 is set only in padding.
        data.extend_from_slice(&(1u64 << 33).to_be_bytes());
        data.extend_from_slice(&2u64.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());

        let (bitmap, rest) = gix_bitmap::ewah::decode(&data).expect("fixture must decode");

        assert!(rest.is_empty(), "fixture should be fully consumed");
        assert_eq!(
            bitmap.for_each_set_bit(|_| Some(())),
            None,
            "set bits outside the declared bit length must be rejected"
        );
    }

    fn artifact_paths(target: &str) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fuzz/artifacts")
                .join(target),
        )
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect()
    }
}
