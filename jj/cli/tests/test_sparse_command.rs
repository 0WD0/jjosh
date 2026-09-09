// Copyright 2022 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use testutils::TestResult;

use crate::common::TestEnvironment;

#[test]
fn test_sparse_manage_patterns() {
    let mut test_env = TestEnvironment::default();
    let edit_script = test_env.set_up_fake_editor();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    for name in ["file1", "file2", "file3"] {
        work_dir.write_file(name, "contents");
    }
    work_dir.run_jj(["sparse", "set", "--clear"]).success();
    for name in ["file1", "file2", "file3"] {
        assert!(!work_dir.root().join(name).exists());
    }
    assert_eq!(
        work_dir.run_jj(["file", "list"]).success().stdout.raw(),
        "file1\nfile2\nfile3\n",
    );

    let sub_dir = work_dir.create_dir("sub");
    sub_dir
        .run_jj(["sparse", "set", "--add", "../file2 | ../file3"])
        .success();
    assert!(!work_dir.root().join("file1").exists());
    assert!(work_dir.root().join("file2").exists());
    assert!(work_dir.root().join("file3").exists());
    sub_dir
        .run_jj([
            "sparse",
            "set",
            "--add",
            "../file1",
            "--remove",
            "../file2 | ../file3",
        ])
        .success();
    assert!(work_dir.root().join("file1").exists());
    assert!(!work_dir.root().join("file2").exists());
    assert!(!work_dir.root().join("file3").exists());

    // Exclusion from an ancestor, followed by inclusion, must preserve order.
    work_dir.run_jj(["sparse", "reset"]).success();
    work_dir
        .run_jj(["sparse", "set", "--remove", "."])
        .success();
    work_dir
        .run_jj(["sparse", "set", "--add", "file2"])
        .success();
    assert!(!work_dir.root().join("file1").exists());
    assert!(work_dir.root().join("file2").exists());
    assert!(!work_dir.root().join("file3").exists());

    // Printed ordered rules remain usable in the editor from a different cwd.
    let expression = work_dir.run_jj(["sparse", "list"]).success();
    work_dir.run_jj(["sparse", "reset"]).success();
    std::fs::write(&edit_script, format!("write\n{}", expression.stdout.raw())).unwrap();
    sub_dir.run_jj(["sparse", "edit"]).success();
    assert!(!work_dir.root().join("file1").exists());
    assert!(work_dir.root().join("file2").exists());
    assert!(!work_dir.root().join("file3").exists());

    // Each ordered rule accepts a compound expression in canonical coordinates.
    std::fs::write(
        &edit_script,
        "write\nJJ: comment\n+ (root:file1 | root:file2 | root:file3) ~ root:file2\n",
    )
    .unwrap();
    sub_dir.run_jj(["sparse", "edit"]).success();
    assert!(work_dir.root().join("file1").exists());
    assert!(!work_dir.root().join("file2").exists());
    assert!(work_dir.root().join("file3").exists());

    // Parse failure must not change the selection or lose a dirty visible file.
    work_dir.write_file("file1", "edited");
    std::fs::write(&edit_script, "write\n+ (root:file1 |").unwrap();
    assert!(!sub_dir.run_jj(["sparse", "edit"]).status.success());
    assert_eq!(work_dir.read_file("file1"), "edited");
    assert!(!work_dir.root().join("file2").exists());
    assert!(work_dir.root().join("file3").exists());

    std::fs::write(&edit_script, "write\nJJ: no selected paths\n\n").unwrap();
    sub_dir.run_jj(["sparse", "edit"]).success();
    for name in ["file1", "file2", "file3"] {
        assert!(!work_dir.root().join(name).exists());
    }
    sub_dir.run_jj(["sparse", "reset"]).success();
    assert_eq!(work_dir.read_file("file1"), "edited");
    assert_eq!(work_dir.read_file("file2"), "contents");
    assert_eq!(work_dir.read_file("file3"), "contents");
}

#[test]
fn test_sparse_glob_snapshot_preserves_hidden_files() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("src/main.rs", "original");
    work_dir.write_file("src/data.txt", "hidden");
    work_dir.write_file("README", "readme");
    work_dir
        .run_jj(["sparse", "set", r#"glob:"**/*.rs" ~ src/generated"#])
        .success();
    assert!(work_dir.root().join("src/main.rs").exists());
    assert!(!work_dir.root().join("src/data.txt").exists());
    assert!(!work_dir.root().join("README").exists());
    work_dir.write_file("src/main.rs", "changed");
    work_dir.write_file("src/new.rs", "new");
    work_dir.write_file("src/generated/ignored.rs", "outside selection");
    work_dir.run_jj(["status"]).success();
    assert_eq!(
        work_dir.run_jj(["file", "list"]).success().stdout.raw(),
        "README\nsrc/data.txt\nsrc/main.rs\nsrc/new.rs\n",
    );
    work_dir.run_jj(["sparse", "reset"]).success();
    assert_eq!(work_dir.read_file("src/data.txt"), "hidden");
    assert_eq!(work_dir.read_file("README"), "readme");
    assert_eq!(work_dir.read_file("src/main.rs"), "changed");
    assert_eq!(work_dir.read_file("src/new.rs"), "new");
}

#[test]
fn test_sparse_editor_avoids_unc() -> TestResult {
    use std::path::PathBuf;

    let mut test_env = TestEnvironment::default();
    let edit_script = test_env.set_up_fake_editor();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    std::fs::write(edit_script, "dump-path path")?;
    work_dir.run_jj(["sparse", "edit"]).success();

    let edited_path = PathBuf::from(std::fs::read_to_string(test_env.env_root().join("path"))?);
    // While `assert!(!edited_path.starts_with("//?/"))` could work here in most
    // cases, it fails when it is not safe to strip the prefix, such as paths
    // over 260 chars.
    assert_eq!(edited_path, dunce::simplified(&edited_path));
    Ok(())
}

#[test]
fn test_sparse_undo_redo_preserves_dirty_files() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("a", "original a");
    work_dir.write_file("b", "original b");
    work_dir.run_jj(["status"]).success();
    let before = work_dir
        .run_jj(["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .success();
    work_dir.run_jj(["sparse", "set", "a"]).success();
    assert!(work_dir.root().join("a").exists());
    assert!(!work_dir.root().join("b").exists());
    work_dir.run_jj(["undo"]).success();
    assert_eq!(work_dir.read_file("b"), "original b");
    work_dir.run_jj(["redo"]).success();
    assert!(!work_dir.root().join("b").exists());
    assert_eq!(
        work_dir
            .run_jj(["log", "-r", "@", "--no-graph", "-T", "commit_id"])
            .success()
            .stdout
            .raw(),
        before.stdout.raw(),
    );

    work_dir.write_file("a", "dirty a");
    work_dir.run_jj(["sparse", "set", "b"]).success();
    assert!(!work_dir.root().join("a").exists());
    assert_eq!(work_dir.read_file("b"), "original b");
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "a"])
            .success()
            .stdout
            .raw(),
        "dirty a",
    );
    work_dir.run_jj(["undo"]).success();
    assert_eq!(work_dir.read_file("a"), "dirty a");
    assert!(!work_dir.root().join("b").exists());
}

#[test]
fn test_sparse_deferred_materialization_and_historical_selection() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("a", "original a");
    work_dir.write_file("b", "original b");
    work_dir.run_jj(["sparse", "set", "a"]).success();
    let old_op = work_dir
        .run_jj(["op", "log", "--no-graph", "-n", "1", "-T", "id"])
        .success();
    work_dir.write_file("a", "unsnapshotted a");
    work_dir
        .run_jj(["--ignore-working-copy", "sparse", "set", "b"])
        .success();
    assert_eq!(work_dir.read_file("a"), "unsnapshotted a");
    assert!(!work_dir.root().join("b").exists());
    let historical = work_dir
        .run_jj(["--at-op", old_op.stdout.raw().trim(), "sparse", "list"])
        .success();
    // Querying history does not materialize the newly requested selection.
    assert!(work_dir.root().join("a").exists());
    assert!(!work_dir.root().join("b").exists());
    work_dir.run_jj(["status"]).success();
    assert!(!work_dir.root().join("a").exists());
    assert_eq!(work_dir.read_file("b"), "original b");
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "a"])
            .success()
            .stdout
            .raw(),
        "unsnapshotted a",
    );
    assert_eq!(
        work_dir
            .run_jj([
                "file",
                "list",
                historical.stdout.raw().trim().trim_start_matches("+ ")
            ])
            .success()
            .stdout
            .raw(),
        "a\n",
    );
}

#[test]
fn test_sparse_concurrent_selections_preserve_actual_checkout_until_resolved() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("a", "a");
    work_dir.write_file("b", "b");
    work_dir.run_jj(["sparse", "set", "a | b"]).success();
    let base = work_dir
        .run_jj(["op", "log", "--no-graph", "-n", "1", "-T", "id"])
        .success();
    for selection in ["a", "b"] {
        work_dir
            .run_jj([
                "--at-op",
                base.stdout.raw().trim(),
                "sparse",
                "set",
                selection,
            ])
            .success();
    }
    // Neither historical command is allowed to rewrite the actual checkout.
    assert!(work_dir.root().join("a").exists());
    assert!(work_dir.root().join("b").exists());
    work_dir.write_file("a", "edited a");
    assert!(!work_dir.run_jj(["sparse", "set"]).status.success());
    assert!(
        !work_dir
            .run_jj(["sparse", "set", "--add", "a"])
            .status
            .success()
    );
    work_dir.write_file("b", "edited b");
    work_dir.run_jj(["status"]).success();
    assert_eq!(work_dir.read_file("a"), "edited a");
    assert_eq!(work_dir.read_file("b"), "edited b");
    work_dir.run_jj(["sparse", "set", "a"]).success();
    assert_eq!(work_dir.read_file("a"), "edited a");
    assert!(!work_dir.root().join("b").exists());
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "b"])
            .success()
            .stdout
            .raw(),
        "edited b",
    );
}

#[test]
fn test_sparse_unregistered_history_does_not_guess_a_selection() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("a", "a");
    work_dir.write_file("b", "b");
    work_dir.run_jj(["status"]).success();
    let unregistered = work_dir
        .run_jj(["op", "log", "--no-graph", "-n", "1", "-T", "id"])
        .success();
    assert!(
        !work_dir
            .run_jj(["--ignore-working-copy", "sparse", "set", "a"])
            .status
            .success()
    );
    assert!(work_dir.root().join("a").exists());
    assert!(work_dir.root().join("b").exists());
    work_dir.run_jj(["sparse", "set", "a"]).success();
    assert!(
        !work_dir
            .run_jj([
                "--at-op",
                unregistered.stdout.raw().trim(),
                "sparse",
                "set",
                "b"
            ])
            .status
            .success()
    );
    work_dir
        .run_jj(["op", "restore", unregistered.stdout.raw().trim()])
        .success();
    // Older operations never recorded a choice: restoring one must not invent
    // a full checkout or erase the known physical selection.
    assert!(work_dir.root().join("a").exists());
    assert!(!work_dir.root().join("b").exists());
}

#[test]
fn test_sparse_mapping_sunshine_layout_and_canonical_paths() {
    let test_env = TestEnvironment::default();
    test_env
        .run_jj_in(".", ["git", "init", "--no-colocate", "repo"])
        .success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("projects/sunshine/main.rs", "sunshine");
    work_dir.write_file("projects/sunshine/deps/obsolete", "hidden original");
    work_dir.write_file("libraries/shared/lib.rs", "shared");
    work_dir.write_file("other/hidden", "unselected");
    work_dir
        .run_jj(["sparse", "set", "--remove", "root:projects/sunshine/deps"])
        .success();
    work_dir
        .run_jj([
            "sparse",
            "map",
            "set",
            "projects/sunshine=.",
            "libraries/shared=deps/shared",
        ])
        .success();
    assert_eq!(work_dir.read_file("main.rs"), "sunshine");
    assert_eq!(work_dir.read_file("deps/shared/lib.rs"), "shared");
    assert!(!work_dir.root().join("projects").exists());
    assert!(!work_dir.root().join("other").exists());
    work_dir.write_file("main.rs", "edited sunshine");
    work_dir.write_file("deps/shared/new.rs", "new dependency");
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "main.rs"])
            .success()
            .stdout
            .raw(),
        "edited sunshine"
    );
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "root:projects/sunshine/main.rs"])
            .success()
            .stdout
            .raw(),
        "edited sunshine"
    );
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "root:libraries/shared/new.rs"])
            .success()
            .stdout
            .raw(),
        "new dependency"
    );
    // Revision queries still see hidden canonical files, not just the checkout.
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "root:other/hidden"])
            .success()
            .stdout
            .raw(),
        "unselected"
    );
    assert!(
        !work_dir
            .run_jj(["file", "list", "glob:**"])
            .status
            .success()
    );
    assert!(!work_dir.run_jj(["file", "list", "."]).status.success());
    work_dir.run_jj(["file", "list", r#"root:"""#]).success();
    work_dir.run_jj(["sparse", "map", "reset"]).success();
    assert_eq!(
        work_dir.read_file("projects/sunshine/main.rs"),
        "edited sunshine"
    );
    assert_eq!(
        work_dir.read_file("libraries/shared/new.rs"),
        "new dependency"
    );
    assert!(
        !work_dir
            .root()
            .join("projects/sunshine/deps/obsolete")
            .exists()
    );
    work_dir.run_jj(["sparse", "reset"]).success();
    assert_eq!(
        work_dir.read_file("projects/sunshine/deps/obsolete"),
        "hidden original"
    );
}

#[test]
fn test_sparse_mapping_deferred_dirty_snapshot_and_undo() {
    let test_env = TestEnvironment::default();
    test_env
        .run_jj_in(".", ["git", "init", "--no-colocate", "repo"])
        .success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("canonical/a", "original");
    work_dir
        .run_jj(["sparse", "map", "set", "canonical=visible"])
        .success();
    let old_op = work_dir
        .run_jj(["op", "log", "--no-graph", "-n", "1", "-T", "id"])
        .success();
    work_dir.write_file("visible/a", "dirty");
    work_dir.write_file("visible/new", "new");
    work_dir
        .run_jj([
            "--ignore-working-copy",
            "sparse",
            "map",
            "set",
            "canonical=moved",
        ])
        .success();
    assert_eq!(work_dir.read_file("visible/a"), "dirty");
    assert!(!work_dir.root().join("moved").exists());
    work_dir
        .run_jj([
            "--at-op",
            old_op.stdout.raw().trim(),
            "sparse",
            "map",
            "list",
        ])
        .success();
    assert!(!work_dir.root().join("moved").exists());
    // Snapshot uses old physical ownership before applying desired mapping.
    work_dir.run_jj(["status"]).success();
    assert!(!work_dir.root().join("visible").exists());
    assert_eq!(work_dir.read_file("moved/a"), "dirty");
    assert_eq!(work_dir.read_file("moved/new"), "new");
    assert_eq!(
        work_dir
            .run_jj(["file", "show", "root:canonical/new"])
            .success()
            .stdout
            .raw(),
        "new"
    );
    // --at-op=@ retains the native writable-current-operation exception.
    work_dir
        .run_jj(["--at-op=@", "sparse", "map", "set", "canonical=third"])
        .success();
    assert_eq!(work_dir.read_file("third/a"), "dirty");
    work_dir.run_jj(["undo"]).success();
    assert_eq!(work_dir.read_file("moved/a"), "dirty");
    work_dir.run_jj(["redo"]).success();
    assert_eq!(work_dir.read_file("third/new"), "new");
}

#[test]
fn test_sparse_mapping_conflicts_keep_actual_layout() {
    let mut test_env = TestEnvironment::default();
    let editor = test_env.set_up_fake_editor();
    test_env
        .run_jj_in(".", ["git", "init", "--no-colocate", "repo"])
        .success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("canonical/a", "original");
    work_dir
        .run_jj(["sparse", "map", "set", "canonical=actual"])
        .success();
    let base = work_dir
        .run_jj(["op", "log", "--no-graph", "-n", "1", "-T", "id"])
        .success();
    for mapping in ["canonical=left", "canonical=right"] {
        work_dir
            .run_jj([
                "--at-op",
                base.stdout.raw().trim(),
                "sparse",
                "map",
                "set",
                mapping,
            ])
            .success();
    }
    work_dir.write_file("actual/a", "dirty conflict");
    work_dir.run_jj(["status"]).success();
    assert_eq!(work_dir.read_file("actual/a"), "dirty conflict");
    assert!(!work_dir.root().join("left").exists());
    assert!(!work_dir.root().join("right").exists());
    assert!(!work_dir.run_jj(["sparse", "map", "reset"]).status.success());
    assert!(
        !work_dir
            .run_jj(["sparse", "set", "root:canonical"])
            .status
            .success()
    );
    std::fs::write(editor, "write\n+ all()\nmap [\"canonical\",\"resolved\"]\n").unwrap();
    work_dir.run_jj(["sparse", "edit"]).success();
    assert_eq!(work_dir.read_file("resolved/a"), "dirty conflict");
    assert!(!work_dir.root().join("actual").exists());
}

#[test]
fn test_sparse_mapping_workspace_independence_and_colocation_rejection() {
    let test_env = TestEnvironment::default();
    test_env
        .run_jj_in(".", ["git", "init", "--no-colocate", "repo"])
        .success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("sunshine/a", "sunshine");
    work_dir.write_file("artemis/a", "artemis");
    work_dir.run_jj(["new"]).success();
    work_dir
        .run_jj(["sparse", "map", "set", "sunshine=."])
        .success();
    work_dir
        .run_jj(["workspace", "add", "--no-colocate", "../artemis"])
        .success();
    let artemis = test_env.work_dir("artemis");
    assert_eq!(artemis.read_file("a"), "sunshine");
    artemis
        .run_jj(["sparse", "map", "set", "artemis=."])
        .success();
    assert_eq!(artemis.read_file("a"), "artemis");
    assert_eq!(work_dir.read_file("a"), "sunshine");
    assert!(
        !work_dir
            .run_jj(["git", "colocation", "enable"])
            .status
            .success()
    );
    assert!(!work_dir.root().join(".git").exists());
    assert!(
        !work_dir
            .run_jj(["workspace", "add", "--colocate", "../rejected"])
            .status
            .success()
    );
    work_dir.run_jj(["sparse", "map", "reset"]).success();
    work_dir.run_jj(["git", "colocation", "enable"]).success();
    assert!(
        !work_dir
            .run_jj(["sparse", "map", "set", "sunshine=."])
            .status
            .success()
    );
    assert_eq!(work_dir.read_file("sunshine/a"), "sunshine");
}

#[test]
fn test_sparse_mapping_overlap_rejected_before_materialization() {
    let test_env = TestEnvironment::default();
    test_env
        .run_jj_in(".", ["git", "init", "--no-colocate", "repo"])
        .success();
    let work_dir = test_env.work_dir("repo");
    work_dir.write_file("project/a", "project");
    work_dir.write_file("dependency/a", "dependency");
    work_dir.run_jj(["status"]).success();
    // No dependency directory exists under project yet. Future ownership still
    // overlaps, so validation cannot merely inspect the current tree.
    assert!(
        !work_dir
            .run_jj(["sparse", "map", "set", "project=.", "dependency=deps"])
            .status
            .success()
    );
    assert_eq!(work_dir.read_file("project/a"), "project");
    assert_eq!(work_dir.read_file("dependency/a"), "dependency");
    assert!(!work_dir.root().join("a").exists());
}
