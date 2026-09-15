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

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use indoc::indoc;
use testutils::TestResult;
use testutils::git;

use crate::common::TestEnvironment;

#[test]
fn test_git_remotes() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "add", "foo", "http://example.com/repo/foo"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "add", "bar", "http://example.com/repo/bar"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "baz",
        "http://example.com/repo/baz",
        "--push-url",
        "git@example.com:repo/baz",
    ]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    bar http://example.com/repo/bar
    baz http://example.com/repo/baz (push: git@example.com:repo/baz)
    foo http://example.com/repo/foo
    [EOF]
    ");
    let output = work_dir.run_jj(["git", "remote", "remove", "foo"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    bar http://example.com/repo/bar
    baz http://example.com/repo/baz (push: git@example.com:repo/baz)
    [EOF]
    ");
    work_dir.run_jj(["bookmark", "create", "main"]).success();
    work_dir
        .run_jj(["bookmark", "track", "main", "--remote=*"])
        .success();
    let remote_state = || {
        (
            fs::read(work_dir.root().join(".jj/repo/store/git/config")).unwrap(),
            work_dir
                .run_jj(["bookmark", "list", "--all-remotes"])
                .success()
                .stdout,
        )
    };
    let state_before = remote_state();
    let output = work_dir.run_jj(["git", "remote", "remove", "nonexistent"]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);

    // named remote that cannot be parsed
    work_dir.write_file(
        ".jj/repo/store/git/config",
        indoc! {r#"
            [remote "foo"]
                url = https://
        "#},
    );
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @r#"
    ------- stderr -------
    Error: Unexpected Git error when managing remotes
    Caused by:
    1: The fetch url under `remote.foo` was invalid
    2: The url at "remote.<name>.url=https://" could not be parsed
    3: URL "https://" can not be parsed as valid URL
    4: Scheme requires host
    [EOF]
    [exit status: 1]
    "#);
}

#[test]
fn test_git_remote_add() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir
        .run_jj(["git", "remote", "add", "foo", "http://example.com/repo/foo"])
        .success();
    work_dir.run_jj(["bookmark", "create", "main"]).success();
    work_dir
        .run_jj(["bookmark", "track", "main", "--remote=*"])
        .success();
    let remote_state = || {
        (
            fs::read(work_dir.root().join(".jj/repo/store/git/config")).unwrap(),
            work_dir
                .run_jj(["bookmark", "list", "--all-remotes"])
                .success()
                .stdout,
        )
    };
    let state_before = remote_state();
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "foo",
        "http://example.com/repo/foo2",
    ]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj(["git", "remote", "add", "git", "http://example.com/repo/git"]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    foo http://example.com/repo/foo
    [EOF]
    ");
}

#[test]
fn test_git_remote_add_duplicate_url_warning() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir
        .run_jj(["git", "remote", "add", "foo", "http://example.com/repo/foo"])
        .success();
    let output = work_dir.run_jj(["git", "remote", "add", "bar", "http://example.com/repo/foo"]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Warning: Remote foo already uses the same URL.
    Hint: If this was a mistake, run `jj git remote remove bar`.
    [EOF]
    ");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    bar http://example.com/repo/foo
    foo http://example.com/repo/foo
    [EOF]
    ");
}

#[test]
fn test_git_remote_add_duplicate_url_warning_with_url_rewrite() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    let mut config_file = fs::OpenOptions::new()
        .append(true)
        .open(work_dir.root().join(".jj/repo/store/git/config"))
        .unwrap();
    // The warning is about exact configured URL strings. Git rewrite rules can
    // make different strings resolve to the same URL, so they should not affect
    // whether this warning fires.
    writeln!(
        config_file,
        r#"[url "https://example.com/"]
	insteadOf = gh:"#
    )
    .unwrap();
    drop(config_file);

    work_dir
        .run_jj(["git", "remote", "add", "foo", "gh:org/repo"])
        .success();
    let output = work_dir.run_jj(["git", "remote", "add", "bar", "gh:org/repo"]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Warning: Remote foo already uses the same URL.
    Hint: If this was a mistake, run `jj git remote remove bar`.
    [EOF]
    ");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "baz",
        "https://example.com/org/repo",
    ]);
    insta::assert_snapshot!(output, @"");
}

#[test]
fn test_git_remote_add_duplicate_url_warning_omits_url() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    // Remote URLs can contain embedded credentials. The warning should identify
    // the duplicate remote without echoing secrets into stderr or logs.
    work_dir
        .run_jj([
            "git",
            "remote",
            "add",
            "foo",
            "https://user:token@example.com/repo",
        ])
        .success();
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "bar",
        "https://user:token@example.com/repo",
    ]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Warning: Remote foo already uses the same URL.
    Hint: If this was a mistake, run `jj git remote remove bar`.
    [EOF]
    ");
}

#[test]
fn test_git_remote_add_duplicate_url_warning_cross_direction() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir
        .run_jj([
            "git",
            "remote",
            "add",
            "foo",
            "http://example.com/repo/fetch",
            "--push-url",
            "http://example.com/repo/push",
        ])
        .success();

    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "bar",
        "http://example.com/repo/new-fetch",
        "--push-url",
        "http://example.com/repo/fetch",
    ]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Warning: Remote foo already uses the same URL.
    Hint: If this was a mistake, run `jj git remote remove bar`.
    [EOF]
    ");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "baz",
        "http://example.com/repo/push",
        "--push-url",
        "http://example.com/repo/new-push",
    ]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Warning: Remote foo already uses the same URL.
    Hint: If this was a mistake, run `jj git remote remove baz`.
    [EOF]
    ");
}

#[test]
fn test_git_remote_set_url() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir
        .run_jj(["git", "remote", "add", "foo", "http://example.com/repo/foo"])
        .success();
    work_dir.run_jj(["bookmark", "create", "main"]).success();
    work_dir
        .run_jj(["bookmark", "track", "main", "--remote=*"])
        .success();
    let remote_state = || {
        (
            fs::read(work_dir.root().join(".jj/repo/store/git/config")).unwrap(),
            work_dir
                .run_jj(["bookmark", "list", "--all-remotes"])
                .success()
                .stdout,
        )
    };
    let state_before = remote_state();
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "bar",
        "http://example.com/repo/bar",
    ]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "git",
        "http://example.com/repo/git",
    ]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "http://example.com/repo/bar",
    ]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    foo http://example.com/repo/bar
    [EOF]
    ");
    // explicitly set the push url to the same value as fetch works.
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "--push",
        "https://example.com/repo/bar",
    ]);
    insta::assert_snapshot!(output, @"");
    insta::assert_snapshot!(work_dir.run_jj(["git", "remote", "list"]), @"
    foo http://example.com/repo/bar (push: https://example.com/repo/bar)
    [EOF]
    ");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "--push",
        "git@example.com:repo/bar",
    ]);
    insta::assert_snapshot!(output, @"");
    insta::assert_snapshot!(work_dir.run_jj(["git", "remote", "list"]), @"
    foo http://example.com/repo/bar (push: git@example.com:repo/bar)
    [EOF]
    ");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "--fetch",
        "http://example.com/repo/bar2",
    ]);
    insta::assert_snapshot!(output, @"");
    insta::assert_snapshot!(work_dir.run_jj(["git", "remote", "list"]), @"
    foo http://example.com/repo/bar2 (push: git@example.com:repo/bar)
    [EOF]
    ");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "http://example.com/repo/bar",
    ]);
    insta::assert_snapshot!(output, @"");
    insta::assert_snapshot!(work_dir.run_jj(["git", "remote", "list"]), @"
    foo http://example.com/repo/bar (push: git@example.com:repo/bar)
    [EOF]
    ");
    let state_before = remote_state();
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "https://example.com/repo/baz",
        "--fetch",
        "https://example.com/repo/bar2",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "https://example.com/repo/baz",
        "--push",
        "git@example.com:/repo/baz",
    ]);
    insta::assert_snapshot!(output, @"");
    insta::assert_snapshot!(work_dir.run_jj(["git", "remote", "list"]), @"
    foo https://example.com/repo/baz (push: git@example.com:/repo/baz)
    [EOF]
    ");
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "--fetch",
        "https://example.com/repo/bar",
        "--push",
        "git@example.com:/repo/bar",
    ]);
    insta::assert_snapshot!(output, @"");
    insta::assert_snapshot!(work_dir.run_jj(["git", "remote", "list"]), @"
    foo https://example.com/repo/bar (push: git@example.com:/repo/bar)
    [EOF]
    ");
}

#[test]
fn test_git_remote_relative_path() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    // Relative path using OS-native separator
    let path = PathBuf::from_iter(["..", "native", "sep"]);
    work_dir
        .run_jj(["git", "remote", "add", "foo", path.to_str().unwrap()])
        .success();
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    foo $TEST_ENV/native/sep
    [EOF]
    ");

    // Relative path using UNIX separator
    test_env
        .run_jj_in(
            ".",
            ["-Rrepo", "git", "remote", "set-url", "foo", "unix/sep"],
        )
        .success();
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    foo $TEST_ENV/unix/sep
    [EOF]
    ");
}

#[test]
fn test_git_remote_rename() {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir
        .run_jj(["git", "remote", "add", "foo", "http://example.com/repo/foo"])
        .success();
    work_dir
        .run_jj(["git", "remote", "add", "baz", "http://example.com/repo/baz"])
        .success();
    work_dir.run_jj(["bookmark", "create", "main"]).success();
    work_dir
        .run_jj(["bookmark", "track", "main", "--remote=*"])
        .success();
    let remote_state = || {
        (
            fs::read(work_dir.root().join(".jj/repo/store/git/config")).unwrap(),
            work_dir
                .run_jj(["bookmark", "list", "--all-remotes"])
                .success()
                .stdout,
        )
    };
    let state_before = remote_state();
    let output = work_dir.run_jj(["git", "remote", "rename", "bar", "foo"]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj(["git", "remote", "rename", "foo", "baz"]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj(["git", "remote", "rename", "foo", "git"]);
    assert!(!output.status.success());
    assert_eq!(remote_state(), state_before);
    let output = work_dir.run_jj(["git", "remote", "rename", "foo", "bar"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    bar http://example.com/repo/foo
    baz http://example.com/repo/baz
    [EOF]
    ");
}

#[test]
fn test_git_remote_rename_updates_trunk() {
    // Verify trunk() resolves correctly after renaming the remote it references.
    let test_env = TestEnvironment::default();

    let remote_repo = git::init(test_env.env_root().join("remote"));
    git::add_commit(&remote_repo, "refs/heads/main", "file", b"", "init", &[]);
    git::set_symbolic_reference(&remote_repo, "HEAD", "refs/heads/main");
    test_env
        .run_jj_in(".", ["git", "clone", "--branch=main", "remote", "local"])
        .success();
    let local_dir = test_env.work_dir("local");

    let output = local_dir.run_jj(["git", "remote", "rename", "origin", "upstream"]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Updating the revset alias `trunk()` to `main@upstream`.
    [EOF]
    ");

    // trunk() should resolve to the correct commit after the rename
    let output = local_dir.run_jj(["log", "-r", "trunk()", "-T", "description"]);
    insta::assert_snapshot!(output, @"
    ◆  init
    │
    ~
    [EOF]
    ");
}

#[test]
fn test_git_remote_with_preset_config() {
    let test_env = TestEnvironment::default();
    // Add user-level config which shouldn't be renamed
    test_env.add_config(indoc! {r#"
        remotes.origin.fetch-bookmarks = "user-origin"
        remotes.foo.fetch-bookmarks = "user-foo"
        remotes.bar.fetch-tags = "user-bar"
    "#});

    // Set up default branch at remote
    let remote_repo = git::init(test_env.env_root().join("remote"));
    git::add_commit(&remote_repo, "refs/heads/main", "file", b"", "init", &[]);
    git::set_symbolic_reference(&remote_repo, "HEAD", "refs/heads/main");

    // Clone the repo, add another remote to ensure that only the target remote
    // settings will be updated
    test_env
        .run_jj_in(
            ".",
            [
                "git",
                "clone",
                "--branch=main",
                "--tag=~*",
                "remote",
                "local",
            ],
        )
        .success();
    let local_dir = test_env.work_dir("local");
    local_dir
        .run_jj(["git", "remote", "add", "bar", "../remote"])
        .success();
    local_dir
        .run_jj([
            "config",
            "set",
            "--repo",
            "remotes.bar.fetch-bookmarks",
            "repo-bar",
        ])
        .success();

    let list_remotes_config =
        || local_dir.run_jj(["config", "list", "--include-overridden", "remotes"]);
    let list_trunk_config = || local_dir.run_jj(["config", "list", "revset-aliases.'trunk()'"]);
    insta::assert_snapshot!(list_remotes_config(), @r#"
    # remotes.origin.fetch-bookmarks = "user-origin"
    remotes.foo.fetch-bookmarks = "user-foo"
    remotes.bar.fetch-tags = "user-bar"
    remotes.origin.fetch-bookmarks = "main"
    remotes.origin.fetch-tags = "~*"
    remotes.bar.fetch-bookmarks = "repo-bar"
    [EOF]
    "#);
    insta::assert_snapshot!(list_trunk_config(), @r#"
    revset-aliases.'trunk()' = "main@origin"
    [EOF]
    "#);

    // Preset repo-level config should be updated automatically
    let output = local_dir.run_jj(["git", "remote", "rename", "origin", "foo"]);
    output.success();
    insta::assert_snapshot!(list_remotes_config(), @r#"
    remotes.origin.fetch-bookmarks = "user-origin"
    # remotes.foo.fetch-bookmarks = "user-foo"
    remotes.bar.fetch-tags = "user-bar"
    remotes.foo.fetch-bookmarks = "main"
    remotes.foo.fetch-tags = "~*"
    remotes.bar.fetch-bookmarks = "repo-bar"
    [EOF]
    "#);
    insta::assert_snapshot!(list_trunk_config(), @r#"
    revset-aliases.'trunk()' = "main@foo"
    [EOF]
    "#);

    // Preset repo-level config should be removed automatically
    let output = local_dir.run_jj(["git", "remote", "remove", "foo"]);
    output.success();
    insta::assert_snapshot!(list_remotes_config(), @r#"
    remotes.origin.fetch-bookmarks = "user-origin"
    remotes.foo.fetch-bookmarks = "user-foo"
    remotes.bar.fetch-tags = "user-bar"
    remotes.bar.fetch-bookmarks = "repo-bar"
    [EOF]
    "#);
    assert!(list_trunk_config().success().stdout.is_empty());

    // Set trunk to non-default value, which shouldn't be updated automatically
    local_dir
        .run_jj([
            "config",
            "set",
            "--repo",
            "revset-aliases.'trunk()'",
            "main@custom-remote",
        ])
        .success();
    let output = local_dir.run_jj(["git", "remote", "rename", "bar", "foo"]);
    output.success();
    insta::assert_snapshot!(list_remotes_config(), @r#"
    remotes.origin.fetch-bookmarks = "user-origin"
    # remotes.foo.fetch-bookmarks = "user-foo"
    remotes.bar.fetch-tags = "user-bar"
    remotes.foo.fetch-bookmarks = "repo-bar"
    [EOF]
    "#);
    insta::assert_snapshot!(list_trunk_config(), @r#"
    revset-aliases.'trunk()' = "main@custom-remote"
    [EOF]
    "#);

    let output = local_dir.run_jj(["git", "remote", "remove", "foo"]);
    output.success();
    insta::assert_snapshot!(list_remotes_config(), @r#"
    remotes.origin.fetch-bookmarks = "user-origin"
    remotes.foo.fetch-bookmarks = "user-foo"
    remotes.bar.fetch-tags = "user-bar"
    [EOF]
    "#);
    insta::assert_snapshot!(list_trunk_config(), @r#"
    revset-aliases.'trunk()' = "main@custom-remote"
    [EOF]
    "#);
}

#[test]
fn test_git_remote_named_git() {
    let test_env = TestEnvironment::default();

    // Existing remote named 'git' shouldn't block the repo initialization.
    let work_dir = test_env.work_dir("repo");
    git::init(work_dir.root());
    git::add_remote(work_dir.root(), "git", "http://example.com/repo/repo");
    work_dir.run_jj(["git", "init", "--git-repo=."]).success();
    work_dir
        .run_jj(["bookmark", "create", "-r@", "main"])
        .success();

    // The remote can be renamed.
    let output = work_dir.run_jj(["git", "remote", "rename", "git", "bar"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    bar http://example.com/repo/repo
    [EOF]
    ------- stderr -------
    Done importing changes from the underlying Git repo.
    [EOF]
    ");
    // @git bookmark shouldn't be renamed.
    let output = work_dir.run_jj(["log", "-rmain@git", "-Tbookmarks"]);
    insta::assert_snapshot!(output, @"
    @  main
    │
    ~
    [EOF]
    ");

    // The remote cannot be renamed back by jj.
    let config_before = std::fs::read(work_dir.root().join(".git/config")).unwrap();
    let bookmark_before = work_dir
        .run_jj(["log", "--no-graph", "-r", "main@git", "-T", "commit_id"])
        .success()
        .stdout
        .to_string();
    let output = work_dir.run_jj(["git", "remote", "rename", "bar", "git"]);
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(work_dir.root().join(".git/config")).unwrap(),
        config_before
    );
    assert_eq!(
        work_dir
            .run_jj(["log", "--no-graph", "-r", "main@git", "-T", "commit_id"])
            .success()
            .stdout
            .to_string(),
        bookmark_before,
    );

    // Reinitialize the repo with remote named 'git'.
    work_dir.remove_dir_all(".jj");
    git::rename_remote(work_dir.root(), "bar", "git");
    work_dir.run_jj(["git", "init", "--git-repo=."]).success();

    // The remote can also be removed.
    let output = work_dir.run_jj(["git", "remote", "remove", "git"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"");
    // @git bookmark shouldn't be removed.
    let output = work_dir.run_jj(["log", "-rmain@git", "-Tbookmarks"]);
    insta::assert_snapshot!(output, @"
    ○  main
    │
    ~
    [EOF]
    ");
}

#[test]
fn test_git_remote_with_slashes() {
    let test_env = TestEnvironment::default();

    // Existing remote with slashes shouldn't block the repo initialization.
    let work_dir = test_env.work_dir("repo");
    git::init(work_dir.root());
    git::add_remote(
        work_dir.root(),
        "slash/origin",
        "http://example.com/repo/repo",
    );
    work_dir.run_jj(["git", "init", "--git-repo=."]).success();
    work_dir
        .run_jj(["bookmark", "create", "-r@", "main"])
        .success();

    // Cannot add remote with a slash via `jj`
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "another/origin",
        "http://examples.org/repo/repo",
    ]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Error: Git remotes with slashes are incompatible with jj: another/origin
    [EOF]
    [exit status: 1]
    ");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    slash/origin http://example.com/repo/repo
    [EOF]
    ");

    // The remote can be renamed.
    let output = work_dir.run_jj(["git", "remote", "rename", "slash/origin", "origin"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    origin http://example.com/repo/repo
    [EOF]
    ");

    // The remote cannot be renamed back by jj.
    let output = work_dir.run_jj(["git", "remote", "rename", "origin", "slash/origin"]);
    insta::assert_snapshot!(output, @"
    ------- stderr -------
    Error: Git remotes with slashes are incompatible with jj: slash/origin
    [EOF]
    [exit status: 1]
    ");

    // Reinitialize the repo with remote with slashes
    work_dir.remove_dir_all(".jj");
    git::rename_remote(work_dir.root(), "origin", "slash/origin");
    work_dir.run_jj(["git", "init", "--git-repo=."]).success();

    // The remote can also be removed.
    let output = work_dir.run_jj(["git", "remote", "remove", "slash/origin"]);
    insta::assert_snapshot!(output, @"");
    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"");
    // @git bookmark shouldn't be removed.
    let output = work_dir.run_jj(["log", "-rmain@git", "-Tbookmarks"]);
    insta::assert_snapshot!(output, @"
    ○  main
    │
    ~
    [EOF]
    ");
}

#[test]
fn test_git_remote_with_branch_config() -> TestResult {
    let test_env = TestEnvironment::default();

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["git", "remote", "add", "foo", "http://example.com/repo"]);
    insta::assert_snapshot!(output, @"");

    let mut config_file = fs::OpenOptions::new()
        .append(true)
        .open(work_dir.root().join(".jj/repo/store/git/config"))?;
    // `git clone` adds branch configuration like this.
    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    write!(config_file, "[branch \"test\"]{eol}")?;
    write!(config_file, "\tremote = foo{eol}")?;
    write!(config_file, "\tmerge = refs/heads/test{eol}")?;
    drop(config_file);

    let output = work_dir.run_jj(["git", "remote", "rename", "foo", "bar"]);
    insta::assert_snapshot!(output, @"");

    let git_repo = gix::open(work_dir.root().join(".jj/repo/store/git"))?;
    let config = git_repo.config_snapshot();
    assert_eq!(
        config.string("branch.test.remote").unwrap().to_string(),
        "bar"
    );
    assert_eq!(
        config.string("branch.test.merge").unwrap().to_string(),
        "refs/heads/test"
    );
    Ok(())
}

#[test]
fn test_git_remote_with_global_git_remote_config() {
    let mut test_env = TestEnvironment::default();
    test_env.work_dir("").write_file(
        "git-config",
        indoc! {r#"
            [remote "origin"]
                prune = true
            [remote "foo"]
                url = htps://example.com/repo/foo
                fetch = +refs/heads/*:refs/remotes/foo/*
        "#},
    );
    test_env.add_env_var("GIT_CONFIG_GLOBAL", test_env.env_root().join("git-config"));

    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["git", "remote", "list"]);
    // Complete remotes from the global configuration are listed.
    //
    // `git remote -v` lists all remotes from the global configuration,
    // even incomplete ones like `origin`. This is inconsistent with
    // the other `git remote` commands, which ignore the global
    // configuration (even `git remote get-url`).
    insta::assert_snapshot!(output, @"
    foo htps://example.com/repo/foo
    [EOF]
    ");
    let original_remotes = output.success().stdout;

    // Local lifecycle commands must not pretend they renamed an included/global remote.
    let output = work_dir.run_jj(["git", "remote", "rename", "foo", "bar"]);
    assert!(!output.status.success());
    let output = work_dir.run_jj(["git", "remote", "list"]);
    assert_eq!(output.success().stdout.raw(), original_remotes.raw());

    let output = work_dir.run_jj(["git", "remote", "remove", "foo"]);
    assert!(!output.status.success());
    let output = work_dir.run_jj([
        "git",
        "remote",
        "set-url",
        "foo",
        "https://example.com/replacement",
    ]);
    assert!(!output.status.success());

    // A new connection with no global section remains independently editable.
    work_dir
        .run_jj([
            "git",
            "remote",
            "add",
            "local",
            "http://example.com/local/1",
        ])
        .success();
    work_dir
        .run_jj([
            "git",
            "remote",
            "set-url",
            "local",
            "https://example.com/local/2",
        ])
        .success();

    let output = work_dir.run_jj(["git", "remote", "list"]);
    insta::assert_snapshot!(output, @"
    foo htps://example.com/repo/foo
    local https://example.com/local/2
    [EOF]
    ");
}

#[test]
fn test_disconnected_managed_remote_local_lifecycle_without_provider() -> TestResult {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");
    work_dir.run_jj([
        "git", "remote", "add", "origin", "https://example.com/original",
    ]).success();
    let git_path = work_dir.root().join(".jj/repo/store/git");
    let git_repo = gix::open(&git_path)?;
    let connection = git_repo.config_snapshot()
        .string("remote.origin.jjosh-connectionId").unwrap().to_string();
    // Simulate an owned conversion connection whose local endpoint was removed.
    // The ordinary jj CLI has no provider for this capability.
    fs::write(git_path.join("config"), format!(
        "[core]\n\tbare = true\n[remote \"origin\"]\n\tjjosh-connectionId = {connection}\n\tjjosh-requiredCapability = jjosh-v1\n",
    ))?;
    work_dir.run_jj(["git", "remote", "rename", "origin", "renamed"]).success();
    work_dir.run_jj([
        "git", "remote", "set-url", "renamed", "https://example.com/reconnected",
    ]).success();
    let git_repo = gix::open(&git_path)?;
    let config = git_repo.config_snapshot();
    assert_eq!(config.string("remote.renamed.url").unwrap().to_string(), "https://example.com/reconnected");
    assert_eq!(config.string("remote.renamed.jjosh-connectionId").unwrap().to_string(), connection);
    assert_eq!(config.string("remote.renamed.jjosh-requiredCapability").unwrap().to_string(), "jjosh-v1");
    work_dir.run_jj(["git", "remote", "remove", "renamed"]).success();
    assert!(gix::open(&git_path)?.remote_names().is_empty());
    Ok(())
}

#[test]
fn test_git_remote_name_validation() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    // Invalid remote name is rejected (detailed validation tested in jj-lib)
    let output = work_dir.run_jj([
        "git",
        "remote",
        "add",
        "my remote",
        "http://example.com/repo",
    ]);
    insta::assert_snapshot!(output, @r#"
    ------- stderr -------
    Error: Invalid Git remote name
    Caused by:
    1: remote names must be valid within refspecs for fetching: "my remote"
    2: Reference name contains invalid byte: " "
    [EOF]
    [exit status: 1]
    "#);
}
