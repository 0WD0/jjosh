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
