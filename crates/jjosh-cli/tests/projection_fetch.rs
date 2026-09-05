#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(cwd: &Path, program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
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
    git(
        &client,
        &["remote", "add", "origin", upstream_bare.to_str().unwrap()],
    );
    git(
        &client,
        &[
            "config",
            "remote.origin.pushurl",
            upstream_bare.to_str().unwrap(),
        ],
    );

    jjosh(
        &client,
        &[
            "projection",
            "remote",
            "add",
            "origin",
            upstream_bare.to_str().unwrap(),
            ":/app",
        ],
    );
    assert!(client.join(".git/josh/remotes/origin.josh").is_file());

    jjosh(&client, &["projection", "fetch", "--remote", "origin"]);
    let initial_backing = git(&client, &["rev-parse", "refs/josh/remotes/origin/main"])
        .trim()
        .to_owned();
    let initial_projected = git(&client, &["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    assert_eq!(initial_backing, upstream_base);
    assert_ne!(initial_projected, initial_backing);
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
        &[
            "push",
            upstream_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );

    let outside_fetch = jjosh(&client, &["projection", "fetch", "--remote", "origin"]);
    assert_eq!(
        git(&client, &["rev-parse", "refs/josh/remotes/origin/main"]).trim(),
        outside_upstream
    );
    assert_eq!(
        git(&client, &["rev-parse", "refs/remotes/origin/main"]).trim(),
        initial_projected
    );
    assert_eq!(operation_id(&client), initial_operation);
    let outside_stderr = String::from_utf8(outside_fetch.stderr).unwrap();
    assert!(outside_stderr.contains("Nothing changed."));
    assert!(outside_stderr.contains("Fetched 0 projected ref update(s)"));

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

    jjosh(&client, &["projection", "fetch", "--remote", "origin"]);
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

    let remote_url = git(&client, &["remote", "get-url", "origin"]);
    assert_eq!(
        Path::new(remote_url.trim()).canonicalize().unwrap(),
        client.canonicalize().unwrap()
    );
    let remote_push_url = git(&client, &["remote", "get-url", "--push", "origin"]);
    assert_eq!(
        Path::new(remote_push_url.trim()).canonicalize().unwrap(),
        client.canonicalize().unwrap()
    );
    assert_eq!(
        git(&client, &["config", "--get", "remote.origin.uploadpack"]),
        "env GIT_NAMESPACE=josh-origin git upload-pack\n"
    );
    assert_eq!(
        git(&client, &["config", "--get", "remote.origin.receivepack"]),
        "false\n"
    );
    let direct_push = run(
        &client,
        Path::new("git"),
        &["push", "origin", "HEAD:refs/heads/direct-push-must-fail"],
    );
    assert!(!direct_push.status.success());
}
