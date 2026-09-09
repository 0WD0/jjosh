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

    // Printed expressions remain usable from a different cwd, including after
    // the expression has accumulated nested set operations.
    let expression = work_dir.run_jj(["sparse", "list"]).success();
    work_dir.run_jj(["sparse", "reset"]).success();
    sub_dir
        .run_jj(["sparse", "set", expression.stdout.raw().trim()])
        .success();
    assert!(!work_dir.root().join("file1").exists());
    assert!(work_dir.root().join("file2").exists());
    assert!(!work_dir.root().join("file3").exists());

    // The editor accepts one expression across lines, not a union of lines.
    std::fs::write(
        &edit_script,
        "write\nJJ: comment\n(../file1 |\n ../file2 | ../file3)\n ~ ../file2\n",
    )
    .unwrap();
    sub_dir.run_jj(["sparse", "edit"]).success();
    assert!(work_dir.root().join("file1").exists());
    assert!(!work_dir.root().join("file2").exists());
    assert!(work_dir.root().join("file3").exists());

    // Parse failure must not change the selection or lose a dirty visible file.
    work_dir.write_file("file1", "edited");
    std::fs::write(&edit_script, "write\n(root:file1 |").unwrap();
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
