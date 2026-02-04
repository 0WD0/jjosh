use gix_testtools::{Creation, Result};

#[test]
fn rust_fixture_read_only_creates_and_caches_fixture() -> Result {
    // First call should create the fixture
    let dir = gix_testtools::rust_fixture_read_only("test_fixture_read_only", 1, |dir| {
        std::fs::write(dir.join("test_file.txt"), "test content")?;
        std::fs::create_dir(dir.join("subdir"))?;
        std::fs::write(dir.join("subdir/nested.txt"), "nested content")?;
        Ok(())
    })?;

    // Verify the fixture was created correctly
    assert!(dir.is_dir());
    assert!(dir.join("test_file.txt").exists());
    assert_eq!(std::fs::read_to_string(dir.join("test_file.txt"))?, "test content");
    assert!(dir.join("subdir").is_dir());
    assert!(dir.join("subdir/nested.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("subdir/nested.txt"))?,
        "nested content"
    );

    // Second call with same version should return cached result
    let dir2 = gix_testtools::rust_fixture_read_only("test_fixture_read_only", 1, |_dir| {
        // This closure should not be called because the fixture is cached
        panic!("Closure should not be called for cached fixture");
    })?;

    // Both should point to the same directory
    assert_eq!(dir, dir2);

    Ok(())
}

#[test]
fn rust_fixture_read_only_version_change_invalidates_cache() -> Result {
    // Create fixture with version 1
    let dir1 = gix_testtools::rust_fixture_read_only("test_fixture_version", 1, |dir| {
        std::fs::write(dir.join("version.txt"), "v1")?;
        Ok(())
    })?;

    // Version 2 should create a new fixture in a different directory
    let dir2 = gix_testtools::rust_fixture_read_only("test_fixture_version", 2, |dir| {
        std::fs::write(dir.join("version.txt"), "v2")?;
        Ok(())
    })?;

    // Directories should be different (different version subdirectories)
    assert_ne!(dir1, dir2);

    // Each should have its own content
    assert_eq!(std::fs::read_to_string(dir1.join("version.txt"))?, "v1");
    assert_eq!(std::fs::read_to_string(dir2.join("version.txt"))?, "v2");

    Ok(())
}

#[test]
fn rust_fixture_writable() -> Result {
    for creation in [Creation::CopyFromReadOnly, Creation::ExecuteScript] {
        let tmp = gix_testtools::rust_fixture_writable("test_fixture_writable_copy", 1, creation, |dir| {
            std::fs::write(dir.join("original.txt"), "original content")?;
            Ok(())
        })?;

        // Verify the fixture was created
        let original_path = tmp.path().join("original.txt");
        assert!(original_path.exists());
        assert_eq!(std::fs::read_to_string(&original_path)?, "original content");

        // Verify we can write to the directory (it's writable)
        let new_file = tmp.path().join("new_file.txt");
        std::fs::write(&new_file, "new content")?;
        assert!(new_file.exists());
        assert_eq!(std::fs::read_to_string(&new_file)?, "new content");
    }
    Ok(())
}

#[test]
fn rust_fixture_closure_error_propagates() {
    // Test that errors from the closure are properly propagated
    let result = gix_testtools::rust_fixture_read_only("test_fixture_error", 1, |_dir| Err("intentional error".into()));

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Rust fixture closure"),
        "Error message should mention 'Rust fixture closure', got: {err_msg}"
    );
    assert!(
        err_msg.contains("intentional error"),
        "Error message should contain the original error, got: {err_msg}"
    );
}

#[test]
fn rust_fixture_standalone_uses_fixtures_directory() -> Result {
    let dir = gix_testtools::rust_fixture_read_only_standalone("test_fixture_standalone", 1, |dir| {
        std::fs::write(dir.join("standalone.txt"), "standalone")?;
        Ok(())
    })?;

    // Standalone fixtures are stored in fixtures/generated-do-not-edit, not tests/fixtures/...
    let dir_str = dir.to_string_lossy();
    assert!(
        dir_str.contains("fixtures") && dir_str.contains("generated-do-not-edit"),
        "Standalone fixture should be in fixtures/generated-do-not-edit directory, got: {dir_str}"
    );
    assert!(
        !dir_str.contains("tests/fixtures"),
        "Standalone fixture should NOT be in tests/fixtures directory, got: {dir_str}"
    );

    assert!(dir.join("standalone.txt").exists());
    Ok(())
}
