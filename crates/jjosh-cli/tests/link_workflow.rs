// Josh's local link transport currently uses the Unix `env` command for namespaced Git access.
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

fn jjosh_unchecked(cwd: &Path, args: &[&str]) -> Output {
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
    Command::new(program)
        .args(&full_args)
        .current_dir(cwd)
        .env("JOSH_EXPERIMENTAL_FEATURES", "1")
        .output()
        .unwrap_or_else(|err| panic!("failed to run {program:?} {full_args:?}: {err}"))
}

fn jjosh(cwd: &Path, args: &[&str]) -> Output {
    let program = Path::new(env!("CARGO_BIN_EXE_jjosh"));
    let output = jjosh_unchecked(cwd, args);
    assert_success(&output, program, args);
    output
}

fn change_id(cwd: &Path) -> String {
    String::from_utf8(jjosh(cwd, &["log", "-r", "@", "--no-graph", "-T", "change_id"]).stdout)
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn add_update_and_push_link_through_jj_transactions() {
    let temp = tempfile::tempdir().unwrap();
    let remote_work = temp.path().join("remote-work");
    let remote_bare = temp.path().join("remote.git");
    let client = temp.path().join("client");
    fs::create_dir(&remote_work).unwrap();
    fs::create_dir(&client).unwrap();

    git(&remote_work, &["init", "-b", "main"]);
    git(&remote_work, &["config", "user.name", "Smoke Test"]);
    git(&remote_work, &["config", "user.email", "smoke@example.com"]);
    fs::create_dir(remote_work.join("src")).unwrap();
    fs::write(remote_work.join("src/lib.txt"), "linked-v1\n").unwrap();
    fs::write(remote_work.join("outside.txt"), "hidden\n").unwrap();
    git(&remote_work, &["add", "."]);
    git(&remote_work, &["commit", "-m", "linked-v1"]);

    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    git(&client, &["switch", "--detach"]);

    let original_change_id = change_id(&client);
    jjosh(
        &client,
        &[
            "link",
            "add",
            "deps",
            remote_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
            "--fetch-url",
            remote_work.to_str().unwrap(),
            "--at",
            "HEAD",
        ],
    );
    assert_eq!(change_id(&client), original_change_id);
    assert_eq!(
        fs::read_to_string(client.join("deps/lib.txt")).unwrap(),
        "linked-v1\n"
    );
    assert!(!client.join("deps/outside.txt").exists());
    assert!(client.join("deps/.link.josh").is_file());
    let add_log =
        String::from_utf8(jjosh(&client, &["op", "log", "--limit", "1", "--no-graph"]).stdout)
            .unwrap();
    assert!(add_log.contains("add Josh link deps"));
    fs::write(client.join("deps/local.txt"), "local-overlay\n").unwrap();
    jjosh(&client, &["status"]);
    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            remote_work.to_str().unwrap(),
            remote_bare.to_str().unwrap(),
        ],
    );

    fs::write(remote_work.join("src/lib.txt"), "linked-v2\n").unwrap();
    git(&remote_work, &["commit", "-am", "linked-v2"]);
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    jjosh(&client, &["link", "update", "deps"]);
    assert_eq!(change_id(&client), original_change_id);
    assert_eq!(
        fs::read_to_string(client.join("deps/lib.txt")).unwrap(),
        "linked-v2\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("deps/local.txt")).unwrap(),
        "local-overlay\n"
    );
    let update_log =
        String::from_utf8(jjosh(&client, &["op", "log", "--limit", "1", "--no-graph"]).stdout)
            .unwrap();
    assert!(update_log.contains("update 1 Josh link(s)"));

    fs::write(client.join("deps/lib.txt"), "local-v3\n").unwrap();
    jjosh(&client, &["status"]);
    jjosh(&client, &["link", "push", "deps", "--to", "exported"]);
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/exported:src/lib.txt"]),
        "local-v3\n"
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/exported:src/local.txt"]),
        "local-overlay\n"
    );

    fs::write(remote_work.join("src/lib.txt"), "remote-v4\n").unwrap();
    git(&remote_work, &["commit", "-am", "remote-v4"]);
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    fs::write(client.join("deps/lib.txt"), "local-v4\n").unwrap();
    jjosh(&client, &["status"]);
    jjosh(&client, &["link", "update", "deps"]);
    let conflicted_status = String::from_utf8(jjosh(&client, &["status"]).stdout).unwrap();
    assert!(conflicted_status.contains("conflict"));

    let rejected_push = jjosh_unchecked(
        &client,
        &["link", "push", "deps", "--to", "rejected-conflict"],
    );
    assert!(!rejected_push.status.success());
    assert!(String::from_utf8_lossy(&rejected_push.stderr).contains("unresolved conflicts"));
    let rejected_ref = run(
        &remote_bare,
        Path::new("git"),
        &["show-ref", "--verify", "refs/heads/rejected-conflict"],
    );
    assert!(!rejected_ref.status.success());
}
