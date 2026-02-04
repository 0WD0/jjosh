use std::path::Path;

use gix_testtools::{Creation, Result};
use serial_test::serial;

// Tests for Rust closure fixtures

#[test]
#[serial]
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
#[serial]
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
#[serial]
fn rust_fixture_writable_copy_from_read_only() -> Result {
    // Create a writable fixture by copying from read-only
    let temp_dir = gix_testtools::rust_fixture_writable(
        "test_fixture_writable_copy",
        1,
        Creation::CopyFromReadOnly,
        |dir| {
            std::fs::write(dir.join("original.txt"), "original content")?;
            Ok(())
        },
    )?;

    // Verify the fixture was created
    let original_path = temp_dir.path().join("original.txt");
    assert!(original_path.exists());
    assert_eq!(std::fs::read_to_string(&original_path)?, "original content");

    // Verify we can write to the directory (it's writable)
    let new_file = temp_dir.path().join("new_file.txt");
    std::fs::write(&new_file, "new content")?;
    assert!(new_file.exists());
    assert_eq!(std::fs::read_to_string(&new_file)?, "new content");

    Ok(())
}

#[test]
#[serial]
fn rust_fixture_writable_execute_closure() -> Result {
    // Create a writable fixture by executing the closure directly in temp dir
    let temp_dir = gix_testtools::rust_fixture_writable(
        "test_fixture_writable_exec",
        1,
        Creation::ExecuteScript,
        |dir| {
            std::fs::write(dir.join("executed.txt"), "executed content")?;
            Ok(())
        },
    )?;

    // Verify the fixture was created
    let executed_path = temp_dir.path().join("executed.txt");
    assert!(executed_path.exists());
    assert_eq!(std::fs::read_to_string(&executed_path)?, "executed content");

    // Verify we can write to the directory
    let new_file = temp_dir.path().join("modified.txt");
    std::fs::write(&new_file, "modified")?;
    assert!(new_file.exists());

    Ok(())
}

#[test]
#[serial]
fn rust_fixture_closure_error_propagates() {
    // Test that errors from the closure are properly propagated
    let result = gix_testtools::rust_fixture_read_only("test_fixture_error", 1, |_dir| {
        Err("intentional error".into())
    });

    assert!(result.is_err());
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
#[serial]
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

#[test]
#[serial]
fn rust_fixture_directory_passed_to_closure_exists() -> Result {
    let dir = gix_testtools::rust_fixture_read_only("test_fixture_dir_exists", 1, |dir| {
        // The directory should exist when the closure is called
        assert!(dir.is_dir(), "Directory should exist when closure is called");
        std::fs::write(dir.join("marker.txt"), "created")?;
        Ok(())
    })?;

    assert!(dir.join("marker.txt").exists());
    Ok(())
}

fn helper_create_fixture(dir: &Path) -> Result {
    std::fs::write(dir.join("helper_file.txt"), "from helper")?;
    Ok(())
}

#[test]
#[serial]
fn rust_fixture_with_function_reference() -> Result {
    // Test that we can pass a function reference instead of a closure
    let dir = gix_testtools::rust_fixture_read_only("test_fixture_fn_ref", 1, helper_create_fixture)?;

    assert!(dir.join("helper_file.txt").exists());
    assert_eq!(std::fs::read_to_string(dir.join("helper_file.txt"))?, "from helper");
    Ok(())
}
