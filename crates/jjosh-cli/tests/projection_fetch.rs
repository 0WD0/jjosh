#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(cwd: &Path, program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", cwd)
        .env("XDG_CONFIG_HOME", cwd)
        .env("JJ_CONFIG", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00+00:00")
        .env("LANG", "C.UTF-8")
        .output()
        .unwrap_or_else(|err| panic!("failed to run {program:?} {args:?}: {err}"))
}

fn assert_success(output: &Output, program: &Path, args: &[&str]) {
    assert!(
        output.status.success(),
        "{program:?} {args:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let program = Path::new("git");
    let output = run(cwd, program, args);
    assert_success(&output, program, args);
    String::from_utf8(output.stdout).unwrap()
}

fn jjosh(cwd: &Path, args: &[&str]) -> Output {
    let program = Path::new(env!("CARGO_BIN_EXE_jjosh"));
    let mut full_args = vec![
        "--no-pager",
        "--color=never",
        "--config",
        "user.name=Smoke Test",
        "--config",
        "user.email=smoke@example.com",
    ];
    full_args.extend_from_slice(args);
    let output = run(cwd, program, &full_args);
    assert_success(&output, program, &full_args);
    output
}

fn operation_id(cwd: &Path) -> String {
    let output = jjosh(cwd, &["op", "log", "--limit", "1", "--no-graph"]);
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn fetch_projects_refs_and_imports_only_visible_changes() {
    let temp = tempfile::tempdir().unwrap();
    let upstream_work = temp.path().join("upstream-work");
    let upstream_bare = temp.path().join("upstream.git");
    let client = temp.path().join("client");
    fs::create_dir(&upstream_work).unwrap();
    fs::create_dir(&client).unwrap();

    git(&upstream_work, &["init", "-b", "main"]);
    git(&upstream_work, &["config", "user.name", "Smoke Test"]);
    git(
        &upstream_work,
        &["config", "user.email", "smoke@example.com"],
    );
    fs::create_dir(upstream_work.join("app")).unwrap();
    fs::create_dir(upstream_work.join("other")).unwrap();
    fs::write(upstream_work.join("app/file.txt"), "app-v1\n").unwrap();
    fs::write(upstream_work.join("other/secret.txt"), "secret-v1\n").unwrap();
    git(&upstream_work, &["add", "."]);
    git(&upstream_work, &["commit", "-m", "upstream-base"]);
    let upstream_base = git(&upstream_work, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            upstream_work.to_str().unwrap(),
            upstream_bare.to_str().unwrap(),
        ],
    );

    git(&client, &["init", "-b", "local"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("local.txt"), "local\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "local-seed"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    git(&client, &["switch", "--detach"]);

    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            upstream_bare.to_str().unwrap(),
            "--filter",
            ":/app",
        ],
    );

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    let initial_projected = git(&client, &["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    assert_ne!(initial_projected, upstream_base);
    assert_eq!(
        git(
            &client,
            &["ls-tree", "-r", "--name-only", &initial_projected]
        ),
        "file.txt\n"
    );
    assert_eq!(
        String::from_utf8(jjosh(&client, &["file", "list", "-r", "main@origin"]).stdout).unwrap(),
        "file.txt\n"
    );
    let initial_operation = operation_id(&client);

    fs::write(upstream_work.join("other/secret.txt"), "secret-v2\n").unwrap();
    git(&upstream_work, &["commit", "-am", "outside-only"]);
    let outside_upstream = git(&upstream_work, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    git(
        &upstream_work,
        &["tag", "-a", "outside-tag", "-m", "outside-only tag"],
    );
    git(
        &upstream_work,
        &[
            "push",
            upstream_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
            "refs/tags/outside-tag",
        ],
    );

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    assert_eq!(
        git(&upstream_bare, &["rev-parse", "main"]).trim(),
        outside_upstream
    );
    assert_eq!(
        git(&client, &["rev-parse", "refs/remotes/origin/main"]).trim(),
        initial_projected
    );
    assert_ne!(operation_id(&client), initial_operation);
    assert_eq!(git(&client, &["for-each-ref", "refs/tags/outside-tag"]), "");
    assert_eq!(
        String::from_utf8(jjosh(&client, &["tag", "list", "--all-remotes", "outside-tag"]).stdout,)
            .unwrap(),
        ""
    );

    fs::write(upstream_work.join("app/file.txt"), "app-v2\n").unwrap();
    git(&upstream_work, &["commit", "-am", "inside-change"]);
    git(
        &upstream_work,
        &[
            "push",
            upstream_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    let final_projected = git(&client, &["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    assert_ne!(final_projected, initial_projected);
    assert_ne!(operation_id(&client), initial_operation);
    assert_eq!(
        String::from_utf8(
            jjosh(&client, &["file", "show", "-r", "main@origin", "file.txt"],).stdout,
        )
        .unwrap(),
        "app-v2\n"
    );

    let visible = String::from_utf8(
        jjosh(
            &client,
            &["log", "--no-graph", "-r", "all()", "-T", "description"],
        )
        .stdout,
    )
    .unwrap();
    assert!(visible.contains("local-seed"));
    assert!(visible.contains("inside-change"));
    assert!(!visible.contains("outside-only"));
}

fn projection_fetch_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let client = temp.path().join("client");
    for path in [&source, &client] {
        fs::create_dir(path).unwrap();
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.name", "Smoke Test"]);
        git(path, &["config", "user.email", "smoke@example.com"]);
    }
    fs::create_dir(source.join("app")).unwrap();
    fs::write(source.join("app/file.txt"), "projected\n").unwrap();
    fs::write(source.join("outside.txt"), "outside\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "source-base"]);
    fs::write(client.join("local.txt"), "local\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "local-seed"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            source.to_str().unwrap(),
            "--filter",
            ":/app",
        ],
    );
    (temp, source, client)
}

#[test]
fn fetch_prunes_only_selected_projection_branches_even_with_tag_pruning() {
    let (_temp, source, client) = projection_fetch_repo();
    git(&source, &["branch", "stale"]);
    git(&source, &["branch", "empty"]);
    let local = git(&client, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&client, &["tag", "local-keep", &local]);
    git(
        &client,
        &["remote", "add", "other", source.to_str().unwrap()],
    );
    let preserved = [
        "refs/tags/local-keep",
        "refs/heads/local-keep",
        "refs/remotes/other/keep",
    ];
    for name in preserved {
        git(&client, &["update-ref", name, &local]);
    }
    git(&client, &["config", "fetch.pruneTags", "true"]);
    git(&client, &["config", "remote.origin.pruneTags", "true"]);
    git(&client, &["config", "remote.origin.tagOpt", "--tags"]);
    // Additional user fetch mappings must not expand this selected fetch's
    // ownership, whether by importing tags or pruning another remote.
    for refspec in [
        "+refs/tags/*:refs/tags/*",
        "+refs/heads/*:refs/remotes/other/*",
    ] {
        git(
            &client,
            &["config", "--add", "remote.origin.fetch", refspec],
        );
    }
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    for branch in ["stale", "empty"] {
        assert_eq!(
            git(
                &client,
                &["rev-parse", &format!("refs/remotes/origin/{branch}")]
            ),
            git(&client, &["rev-parse", "refs/remotes/origin/main"])
        );
    }

    git(&source, &["branch", "-D", "stale", "empty"]);
    git(&source, &["checkout", "--orphan", "empty"]);
    git(&source, &["rm", "-rf", "."]);
    fs::write(source.join("outside.txt"), "only outside\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "empty-projection"]);
    git(&source, &["checkout", "main"]);
    // Exercise packed as well as loose canonical ref deletion.
    git(&client, &["pack-refs", "--all", "--prune"]);

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    for branch in ["stale", "empty"] {
        assert_eq!(
            git(
                &client,
                &["for-each-ref", &format!("refs/remotes/origin/{branch}")]
            ),
            ""
        );
        assert_eq!(
            String::from_utf8(
                jjosh(
                    &client,
                    &[
                        "bookmark",
                        "list",
                        "--remote",
                        "origin",
                        &format!("exact:{branch}"),
                    ],
                )
                .stdout,
            )
            .unwrap(),
            ""
        );
    }
    // With every remaining source branch outside the view, no stale remote
    // bookmarks may remain visible.
    git(&source, &["reset", "--hard", "empty"]);
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    assert_eq!(git(&client, &["for-each-ref", "refs/remotes/origin"]), "");
    assert_eq!(
        String::from_utf8(jjosh(&client, &["bookmark", "list", "--remote", "origin"]).stdout,)
            .unwrap(),
        ""
    );
    for name in preserved {
        assert_eq!(git(&client, &["rev-parse", name]).trim(), local);
    }
    assert_eq!(
        String::from_utf8(jjosh(&client, &["tag", "list", "local-keep", "-T", "name"]).stdout,)
            .unwrap(),
        "local-keep"
    );
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &["log", "--no-graph", "-r", "keep@other", "-T", "commit_id"],
            )
            .stdout,
        )
        .unwrap(),
        local
    );
}

#[test]
fn fetch_synchronizes_external_git_checkout_and_unrecorded_files() {
    let (_temp, _source, client) = projection_fetch_repo();
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    jjosh(&client, &["new", "main@origin"]);
    git(
        &client,
        &["checkout", "-b", "external", "refs/remotes/origin/main"],
    );
    fs::write(client.join("external.txt"), "external commit\n").unwrap();
    git(&client, &["add", "external.txt"]);
    git(&client, &["commit", "-m", "external-git-commit"]);
    let external = git(&client, &["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(client.join("file.txt"), "unrecorded tracked edit\n").unwrap();
    fs::write(client.join("untracked.txt"), "unrecorded new file\n").unwrap();

    // No native command may synchronize the external checkout before fetch.
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    assert_eq!(
        fs::read_to_string(client.join("external.txt")).unwrap(),
        "external commit\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("file.txt")).unwrap(),
        "unrecorded tracked edit\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("untracked.txt")).unwrap(),
        "unrecorded new file\n"
    );
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &["log", "--no-graph", "-r", "@-", "-T", "commit_id"]
            )
            .stdout,
        )
        .unwrap(),
        external
    );
    for (path, expected) in [
        ("external.txt", "external commit\n"),
        ("file.txt", "unrecorded tracked edit\n"),
        ("untracked.txt", "unrecorded new file\n"),
    ] {
        assert_eq!(
            String::from_utf8(jjosh(&client, &["file", "show", "-r", "@", path]).stdout).unwrap(),
            expected
        );
    }
}
