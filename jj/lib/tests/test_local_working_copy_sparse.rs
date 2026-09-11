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

use futures::StreamExt as _;
use itertools::Itertools as _;
use jj_lib::fileset;
use jj_lib::fileset::FilePattern;
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::fileset::FilesetDiagnostics;
use jj_lib::fileset::FilesetExpression;
use jj_lib::fileset::FilesetParseContext;
use jj_lib::local_working_copy::LocalWorkingCopy;
use jj_lib::matchers::EverythingMatcher;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPath;
use jj_lib::ui_path::RepoPathUiConverter;
use jj_lib::working_copy::CheckoutStats;
use jj_lib::working_copy::WorkingCopy as _;
use jj_lib::working_copy_patterns::SparseExpression;
use jj_lib::working_copy_patterns::SparseRule;
use jj_lib::working_copy_patterns::WorkingCopyMapping;
use jj_lib::working_copy_patterns::WorkingCopyPathBuf;
use jj_lib::working_copy_patterns::WorkingCopyPatterns;
use pollster::FutureExt as _;
use prost::Message as _;
use testutils::TestResult;
use testutils::TestWorkspace;
use testutils::commit_with_tree;
use testutils::create_tree;
use testutils::repo_path;

fn paths_to_fileset(paths: &[&RepoPath]) -> WorkingCopyPatterns {
    FilesetExpression::union_all(
        paths
            .iter()
            .map(|&path| FilesetExpression::prefix_path(path.to_owned()))
            .collect(),
    )
    .into()
}

#[test]
fn test_sparse_checkout() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = &test_workspace.repo;
    let working_copy_path = test_workspace.workspace.workspace_root().to_owned();

    let root_file1_path = repo_path("file1");
    let root_file2_path = repo_path("file2");
    let dir1_path = repo_path("dir1");
    let dir1_file1_path = repo_path("dir1/file1");
    let dir1_file2_path = repo_path("dir1/file2");
    let dir1_subdir1_path = repo_path("dir1/subdir1");
    let dir1_subdir1_file1_path = repo_path("dir1/subdir1/file1");
    let dir2_path = repo_path("dir2");
    let dir2_file1_path = repo_path("dir2/file1");

    let tree = create_tree(
        repo,
        &[
            (root_file1_path, "contents"),
            (root_file2_path, "contents"),
            (dir1_file1_path, "contents"),
            (dir1_file2_path, "contents"),
            (dir1_subdir1_file1_path, "contents"),
            (dir2_file1_path, "contents"),
        ],
    );
    let commit = commit_with_tree(repo.store(), tree);

    test_workspace
        .workspace
        .check_out(repo.op_id().clone(), None, &commit)
        .block_on()?;
    let ws = &mut test_workspace.workspace;

    // Set sparse patterns to only dir1/
    let mut locked_ws = ws.start_working_copy_mutation().block_on()?;
    let sparse_patterns = paths_to_fileset(&[dir1_path]);
    let stats = locked_ws
        .locked_wc()
        .set_sparse_patterns(sparse_patterns.clone())
        .block_on()?;
    assert_eq!(
        stats,
        CheckoutStats {
            updated_files: 0,
            added_files: 0,
            removed_files: 3,
            skipped_files: 0,
        }
    );
    assert!(
        !root_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        !root_file2_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        dir1_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        dir1_file2_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        dir1_subdir1_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        !dir2_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );

    // Write the new state to disk
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    let wc: &LocalWorkingCopy = ws.working_copy().downcast_ref().unwrap();

    // Reload the state to check that it was persisted
    let wc = LocalWorkingCopy::load(
        repo.store().clone(),
        ws.workspace_root().to_path_buf(),
        wc.state_path().to_path_buf(),
        repo.settings(),
    )?;

    // Set sparse patterns to file2, dir1/subdir1/ and dir2/
    let mut locked_wc = wc.start_mutation().block_on()?;
    let sparse_patterns = paths_to_fileset(&[root_file1_path, dir1_subdir1_path, dir2_path]);
    let stats = locked_wc
        .set_sparse_patterns(sparse_patterns.clone())
        .block_on()?;
    assert_eq!(
        stats,
        CheckoutStats {
            updated_files: 0,
            added_files: 2,
            removed_files: 2,
            skipped_files: 0,
        }
    );
    assert!(
        root_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        !root_file2_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        !dir1_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        !dir1_file2_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        dir1_subdir1_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    assert!(
        dir2_file1_path
            .to_fs_path_unchecked(&working_copy_path)
            .exists()
    );
    locked_wc.finish(repo.op_id().clone()).block_on()?;
    Ok(())
}

/// Test that sparse patterns are respected on commit
#[test]
fn test_sparse_commit() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = &test_workspace.repo;
    let op_id = repo.op_id().clone();
    let working_copy_path = test_workspace.workspace.workspace_root().to_owned();

    let root_file1_path = repo_path("file1");
    let dir1_path = repo_path("dir1");
    let dir1_file1_path = repo_path("dir1/file1");
    let dir2_path = repo_path("dir2");
    let dir2_file1_path = repo_path("dir2/file1");

    let tree = create_tree(
        repo,
        &[
            (root_file1_path, "contents"),
            (dir1_file1_path, "contents"),
            (dir2_file1_path, "contents"),
        ],
    );

    let commit = commit_with_tree(repo.store(), tree.clone());
    test_workspace
        .workspace
        .check_out(repo.op_id().clone(), None, &commit)
        .block_on()?;

    // Set sparse patterns to only dir1/
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    let sparse_patterns = paths_to_fileset(&[dir1_path]);
    locked_ws
        .locked_wc()
        .set_sparse_patterns(sparse_patterns)
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;

    // Write modified version of all files, including files that are not in the
    // sparse patterns.
    std::fs::write(
        root_file1_path.to_fs_path_unchecked(&working_copy_path),
        "modified",
    )?;
    std::fs::write(
        dir1_file1_path.to_fs_path_unchecked(&working_copy_path),
        "modified",
    )?;
    std::fs::create_dir(dir2_path.to_fs_path_unchecked(&working_copy_path))?;
    std::fs::write(
        dir2_file1_path.to_fs_path_unchecked(&working_copy_path),
        "modified",
    )?;

    // Create a tree from the working copy. Only dir1/file1 should be updated in the
    // tree.
    let modified_tree = test_workspace.snapshot()?;
    let diff: Vec<_> = tree
        .diff_stream(&modified_tree, &EverythingMatcher)
        .collect()
        .block_on();
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].path.as_ref(), dir1_file1_path);

    // Set sparse patterns to also include dir2/
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    let sparse_patterns = paths_to_fileset(&[dir1_path, dir2_path]);
    locked_ws
        .locked_wc()
        .set_sparse_patterns(sparse_patterns)
        .block_on()?;
    locked_ws.finish(op_id).block_on()?;

    // Create a tree from the working copy. Only dir1/file1 and dir2/file1 should be
    // updated in the tree.
    let modified_tree = test_workspace.snapshot()?;
    let diff: Vec<_> = tree
        .diff_stream(&modified_tree, &EverythingMatcher)
        .collect()
        .block_on();
    assert_eq!(diff.len(), 2);
    assert_eq!(diff[0].path.as_ref(), dir1_file1_path);
    assert_eq!(diff[1].path.as_ref(), dir2_file1_path);
    Ok(())
}

#[test]
fn test_sparse_commit_gitignore() -> TestResult {
    // Test that (untracked) .gitignore files in parent directories are respected
    let mut test_workspace = TestWorkspace::init();
    let repo = &test_workspace.repo;
    let working_copy_path = test_workspace.workspace.workspace_root().to_owned();

    let dir1_path = repo_path("dir1");
    let dir1_file1_path = repo_path("dir1/file1");
    let dir1_file2_path = repo_path("dir1/file2");

    // Set sparse patterns to only dir1/
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    let sparse_patterns = paths_to_fileset(&[dir1_path]);
    locked_ws
        .locked_wc()
        .set_sparse_patterns(sparse_patterns)
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;

    // Write dir1/file1 and dir1/file2 and a .gitignore saying to ignore dir1/file1
    std::fs::write(working_copy_path.join(".gitignore"), "dir1/file1")?;
    std::fs::create_dir(dir1_path.to_fs_path_unchecked(&working_copy_path))?;
    std::fs::write(
        dir1_file1_path.to_fs_path_unchecked(&working_copy_path),
        "contents",
    )?;
    std::fs::write(
        dir1_file2_path.to_fs_path_unchecked(&working_copy_path),
        "contents",
    )?;

    // Create a tree from the working copy. Only dir1/file2 should be updated in the
    // tree because dir1/file1 is ignored.
    let modified_tree = test_workspace.snapshot()?;
    let entries = modified_tree.entries().collect_vec();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), dir1_file2_path);
    Ok(())
}

#[test]
fn test_sparse_expression_roundtrip_preserves_set_operations() -> TestResult {
    let aliases = FilesetAliasesMap::new();
    let converter = RepoPathUiConverter::Fs {
        cwd: "/workspace/sub".into(),
        base: "/workspace".into(),
    };
    let context = FilesetParseContext {
        aliases_map: &aliases,
        path_converter: &converter,
    };
    for text in [
        "all() ~ (root:a | root:b)",
        "(all() ~ root:a) | root:a/b",
        "all() ~ (root:a ~ root:a/b)",
        "root:a & (root:a/b | root:a/c)",
        "all() ~ (root:a & root:a/b)",
        r#"root-file:"a\"b" | root-file:"line\nbreak""#,
    ] {
        let original = fileset::parse(&mut FilesetDiagnostics::new(), text, &context)?;
        let expected = original.to_matcher();
        let mut current = original.clone();
        for _ in 0..3 {
            let formatted = fileset::format_expression(&current);
            current = fileset::parse(&mut FilesetDiagnostics::new(), &formatted, &context)?;
            let actual = current.to_matcher();
            for path in [
                "",
                "a",
                "a/b",
                "a/b/file",
                "a/c",
                "b",
                "c",
                "a\"b",
                "line\nbreak",
            ] {
                assert_eq!(
                    actual.matches(repo_path(path)),
                    expected.matches(repo_path(path)),
                    "{text} -> {formatted}, path {path:?}",
                );
            }
        }
    }
    Ok(())
}

#[test]
fn test_sparse_glob_roundtrip_preserves_literal_directory() -> TestResult {
    let mut directories = vec!["Dir", "a[1]", "a{b}", "a*b", "a?b", "a]b"];
    if cfg!(unix) {
        directories.extend(["a\\b", "a\"b", "a\nb"]);
    }
    for directory in directories {
        let base = std::path::PathBuf::from("/workspace");
        let converter = RepoPathUiConverter::Fs {
            cwd: base.join(directory),
            base,
        };
        let context = FilesetParseContext {
            aliases_map: &FilesetAliasesMap::new(),
            path_converter: &converter,
        };
        for kind in ["glob", "prefix-glob", "glob-i", "prefix-glob-i"] {
            for glob in ["*.RS", "**"] {
                let pattern = FilePattern::from_str_kind(&converter, glob, kind)?;
                let mut expression = FilesetExpression::pattern(pattern);
                let expected = expression.to_matcher();
                assert!(expected.matches(repo_path(&format!("{directory}/file.RS"))));
                for _ in 0..3 {
                    let formatted = fileset::format_expression(&expression);
                    expression =
                        fileset::parse(&mut FilesetDiagnostics::new(), &formatted, &context)?;
                    let actual = expression.to_matcher();
                    for candidate_dir in [
                        directory.to_owned(),
                        directory.to_lowercase(),
                        "ab".to_owned(),
                        "a1".to_owned(),
                    ] {
                        for suffix in [
                            "",
                            "/file.RS",
                            "/file.rs",
                            "/file.txt",
                            "/nested/file.RS",
                            "/file.RS/child",
                        ] {
                            let path = format!("{candidate_dir}{suffix}");
                            assert_eq!(
                                actual.matches(repo_path(&path)),
                                expected.matches(repo_path(&path)),
                                "{directory:?} {kind}:{glob} -> {formatted}, path {path:?}",
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn test_sparse_legacy_state_distinguishes_empty_from_missing() -> TestResult {
    use jj_lib::protos::local_working_copy::SparsePatterns;
    use jj_lib::protos::local_working_copy::TreeState;

    let test_workspace = TestWorkspace::init();
    let wc: &LocalWorkingCopy = test_workspace
        .workspace
        .working_copy()
        .downcast_ref()
        .unwrap();
    let state_path = wc.state_path().to_owned();
    let proto_path = state_path.join("tree_state");
    let original = TreeState::decode(std::fs::read(&proto_path)?.as_slice())?;
    for (patterns, included) in [
        (None, vec!["a", "a/file", "b"]),
        (Some(SparsePatterns::default()), vec![]),
        (
            Some(SparsePatterns {
                prefixes: vec!["a".to_owned()],
                ..Default::default()
            }),
            vec!["a", "a/file"],
        ),
        (
            Some(SparsePatterns {
                prefixes: vec![String::new()],
                ..Default::default()
            }),
            vec!["a", "a/file", "b"],
        ),
    ] {
        let mut proto = original.clone();
        proto.sparse_patterns = patterns;
        proto.working_copy_patterns = None;
        std::fs::write(&proto_path, proto.encode_to_vec())?;
        let loaded = LocalWorkingCopy::load(
            test_workspace.repo.store().clone(),
            test_workspace.workspace.workspace_root().to_owned(),
            state_path.clone(),
            test_workspace.repo.settings(),
        )?;
        let matcher = loaded.sparse_patterns()?.to_matcher();
        let actual = ["a", "a/file", "b"]
            .into_iter()
            .filter(|path| matcher.matches(repo_path(path)))
            .collect_vec();
        assert_eq!(actual, included);
    }
    let mut proto = original;
    proto.working_copy_patterns = None;
    proto.sparse_patterns = Some(SparsePatterns {
        prefixes: vec![String::new()],
        fileset_expression: "all() ~ (".to_owned(),
    });
    std::fs::write(&proto_path, proto.encode_to_vec())?;
    let loaded = LocalWorkingCopy::load(
        test_workspace.repo.store().clone(),
        test_workspace.workspace.workspace_root().to_owned(),
        state_path,
        test_workspace.repo.settings(),
    )?;
    assert!(loaded.sparse_patterns().is_err());
    Ok(())
}

#[test]
fn test_invalid_sparse_destination_is_rejected_before_materialization() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = &test_workspace.repo;
    let op_id = repo.op_id().clone();
    let tree = create_tree(
        repo,
        &[
            (repo_path("safe"), "safe"),
            (repo_path("blocked/.jj/file"), "reserved path"),
        ],
    );
    let commit = commit_with_tree(repo.store(), tree);
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .set_sparse_patterns(FilesetExpression::prefix_path(repo_path("safe").to_owned()).into())
        .block_on()?;
    locked_ws.locked_wc().check_out(&commit).block_on()?;
    locked_ws.finish(op_id).block_on()?;

    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    assert!(
        locked_ws
            .locked_wc()
            .set_sparse_patterns(WorkingCopyPatterns::all())
            .block_on()
            .is_err()
    );
    drop(locked_ws);
    // Preflight rejected the reserved path before changing files or recording
    // an interruption. Subsequent commands can still use the original layout.
    drop(
        test_workspace
            .workspace
            .start_working_copy_mutation()
            .block_on()?,
    );
    assert_eq!(
        std::fs::read_to_string(test_workspace.workspace.workspace_root().join("safe"))?,
        "safe",
    );
    Ok(())
}

fn mapping(source: &str, destination: &str) -> WorkingCopyMapping {
    WorkingCopyMapping {
        source: repo_path(source).to_owned(),
        destination: WorkingCopyPathBuf::from_repo_path(repo_path(destination).to_owned()),
        recursive: true,
    }
}

#[test]
fn test_mapped_snapshot_preserves_hidden_signed_conflicts_and_file_types() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = test_workspace.repo.clone();
    let mut builder = testutils::TestThreeWayMergeTreeBuilder::new(repo.store().clone());
    builder.base().file(repo_path("hidden/conflict"), "base");
    builder.parent1().file(repo_path("hidden/conflict"), "left");
    builder
        .parent2()
        .file(repo_path("hidden/conflict"), "right");
    fn populate(tree: &mut testutils::TestTreeBuilder) {
        tree.file(repo_path("app/edit"), "old");
        tree.file(repo_path("app/delete"), "delete");
        tree.file(repo_path("app/script"), "script")
            .executable(true);
        tree.symlink(repo_path("app/link"), "edit");
        tree.file(repo_path("shared/lib"), "library");
    }
    populate(builder.base());
    populate(builder.parent1());
    populate(builder.parent2());
    let tree = builder.write_merged_tree();
    let commit = commit_with_tree(repo.store(), tree.clone());
    let patterns = WorkingCopyPatterns {
        rules: vec![
            SparseRule {
                include: true,
                expression: SparseExpression::All,
            },
            SparseRule {
                include: false,
                expression: SparseExpression::PrefixPath(repo_path("app/deps").to_owned()),
            },
        ],
        mappings: vec![mapping("app", ""), mapping("shared", "deps")],
    };
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&commit, patterns.clone())
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    let root = test_workspace.workspace.workspace_root().to_owned();
    assert_eq!(std::fs::read_to_string(root.join("deps/lib"))?, "library");
    assert!(!root.join("hidden").exists());
    assert!(!root.join("app").exists());
    assert_eq!(
        test_workspace.snapshot()?.tree_ids_and_labels(),
        tree.tree_ids_and_labels()
    );

    std::fs::write(root.join("edit"), "edited")?;
    std::fs::remove_file(root.join("delete"))?;
    std::fs::write(root.join("new"), "new application file")?;
    std::fs::write(root.join("deps/new"), "new dependency file")?;
    let snapshot = test_workspace.snapshot()?;
    let expected = create_tree(
        &repo,
        &[
            (repo_path("app/edit"), "edited"),
            (repo_path("app/new"), "new application file"),
            (repo_path("shared/new"), "new dependency file"),
        ],
    );
    for path in ["app/edit", "app/new", "shared/new"] {
        assert_eq!(
            snapshot.path_value(repo_path(path)).block_on()?,
            expected.path_value(repo_path(path)).block_on()?
        );
    }
    assert!(
        snapshot
            .path_value(repo_path("app/delete"))
            .block_on()?
            .is_absent()
    );
    for path in ["hidden/conflict", "app/script", "app/link", "shared/lib"] {
        assert_eq!(
            snapshot.path_value(repo_path(path)).block_on()?,
            tree.path_value(repo_path(path)).block_on()?
        );
    }
    assert_eq!(snapshot.labels(), tree.labels());
    assert_eq!(snapshot.tree_ids().num_sides(), tree.tree_ids().num_sides());
    assert_eq!(
        test_workspace.workspace.working_copy().sparse_patterns()?,
        &patterns
    );
    assert_eq!(
        test_workspace.snapshot()?.tree_ids_and_labels(),
        snapshot.tree_ids_and_labels()
    );
    Ok(())
}

#[test]
fn test_layout_swap_preserves_snapshotted_edits_and_preflights_untracked_files() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = test_workspace.repo.clone();
    let tree = create_tree(
        &repo,
        &[(repo_path("a/file"), "A"), (repo_path("b/file"), "B")],
    );
    let commit = commit_with_tree(repo.store(), tree);
    let mut patterns = WorkingCopyPatterns::all();
    patterns.mappings = vec![mapping("a", "left"), mapping("b", "right")];
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&commit, patterns.clone())
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    let root = test_workspace.workspace.workspace_root().to_owned();
    std::fs::write(root.join("left/file"), "dirty A")?;
    let dirty_tree = test_workspace.snapshot()?;
    let dirty_commit = commit_with_tree(repo.store(), dirty_tree.clone());
    std::fs::create_dir(root.join("blocked"))?;
    std::fs::write(root.join("blocked/file"), "untracked")?;
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    let mut blocked = patterns.clone();
    blocked.mappings[0] = mapping("a", "blocked");
    assert!(
        locked_ws
            .locked_wc()
            .check_out_with_sparse_patterns(&dirty_commit, blocked)
            .block_on()
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(root.join("left/file"))?, "dirty A");
    assert_eq!(
        std::fs::read_to_string(root.join("blocked/file"))?,
        "untracked"
    );
    patterns.mappings = vec![mapping("a", "right"), mapping("b", "left")];
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&dirty_commit, patterns)
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    assert_eq!(std::fs::read_to_string(root.join("right/file"))?, "dirty A");
    assert_eq!(std::fs::read_to_string(root.join("left/file"))?, "B");
    assert_eq!(
        std::fs::read_to_string(root.join("blocked/file"))?,
        "untracked"
    );
    assert_eq!(
        test_workspace.snapshot()?.tree_ids_and_labels(),
        dirty_tree.tree_ids_and_labels()
    );
    Ok(())
}

#[test]
fn test_partial_layout_failure_keeps_interruption_guard() -> TestResult {
    use jj_lib::backend::CopyId;
    use jj_lib::backend::FileId;
    use jj_lib::backend::TreeValue;
    use jj_lib::merge::Merge;
    use jj_lib::merged_tree_builder::MergedTreeBuilder;

    let mut test_workspace = TestWorkspace::init();
    let repo = test_workspace.repo.clone();
    let tree = create_tree(&repo, &[(repo_path("safe"), "safe")]);
    let mut builder = MergedTreeBuilder::new(tree);
    builder.set_or_remove(
        repo_path("missing").to_owned(),
        Merge::normal(TreeValue::File {
            id: FileId::new(vec![0xab; 20]),
            executable: false,
            copy_id: CopyId::placeholder(),
        }),
    );
    let tree = builder.write_tree().block_on()?;
    let commit = commit_with_tree(repo.store(), tree);
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&commit, paths_to_fileset(&[repo_path("safe")]))
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    let root = test_workspace.workspace.workspace_root().to_owned();
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    assert!(
        locked_ws
            .locked_wc()
            .set_sparse_patterns(paths_to_fileset(&[repo_path("missing")]),)
            .block_on()
            .is_err()
    );
    assert!(!root.join("safe").exists());
    assert!(
        locked_ws
            .locked_wc()
            .snapshot(&testutils::empty_snapshot_options())
            .block_on()
            .is_err()
    );
    assert!(locked_ws.finish(repo.op_id().clone()).block_on().is_err());
    assert!(
        test_workspace
            .workspace
            .start_working_copy_mutation()
            .block_on()
            .is_err()
    );
    Ok(())
}

#[test]
fn test_commit_and_layout_transition_never_materializes_old_coordinates() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = test_workspace.repo.clone();
    let old_tree = create_tree(
        &repo,
        &[
            (repo_path("app/old"), "old application"),
            (repo_path("dep/file"), "old dependency"),
        ],
    );
    let old_commit = commit_with_tree(repo.store(), old_tree);
    let mut patterns = WorkingCopyPatterns::all();
    patterns.mappings = vec![mapping("app", "")];
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&old_commit, patterns.clone())
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    let root = test_workspace.workspace.workspace_root().to_owned();
    std::fs::write(root.join("old"), "dirty application")?;
    test_workspace.snapshot()?;
    // This tree cannot safely be materialized under the old layout. Its
    // metadata-looking path is canonical hidden content under the new layout.
    let new_tree = create_tree(
        &repo,
        &[
            (repo_path("app/old"), "dirty application"),
            (
                repo_path("app/.jj/should-stay-hidden"),
                "canonical hidden content",
            ),
            (repo_path("dep/file"), "new dependency"),
        ],
    );
    let new_commit = commit_with_tree(repo.store(), new_tree.clone());
    patterns.mappings = vec![mapping("dep", "")];
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&new_commit, patterns)
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    assert!(!root.join("old").exists());
    assert!(!root.join(".jj/should-stay-hidden").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("file"))?,
        "new dependency"
    );
    assert_eq!(
        test_workspace.snapshot()?.tree_ids_and_labels(),
        new_tree.tree_ids_and_labels()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_mapping_rejects_symlink_parent_before_removing_old_files() -> TestResult {
    let mut test_workspace = TestWorkspace::init();
    let repo = test_workspace.repo.clone();
    let tree = create_tree(&repo, &[(repo_path("src/tracked"), "tracked")]);
    let commit = commit_with_tree(repo.store(), tree);
    let mut patterns = WorkingCopyPatterns::all();
    patterns.mappings = vec![mapping("src", "")];
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    locked_ws
        .locked_wc()
        .check_out_with_sparse_patterns(&commit, patterns.clone())
        .block_on()?;
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    let root = test_workspace.workspace.workspace_root().to_owned();
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("tracked"), "outside")?;
    std::os::unix::fs::symlink(outside.path(), root.join("escape"))?;
    patterns.mappings = vec![mapping("src", "escape")];
    let mut locked_ws = test_workspace
        .workspace
        .start_working_copy_mutation()
        .block_on()?;
    assert!(
        locked_ws
            .locked_wc()
            .set_sparse_patterns(patterns)
            .block_on()
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(root.join("tracked"))?, "tracked");
    assert_eq!(
        std::fs::read_to_string(outside.path().join("tracked"))?,
        "outside"
    );
    locked_ws.finish(repo.op_id().clone()).block_on()?;
    Ok(())
}
