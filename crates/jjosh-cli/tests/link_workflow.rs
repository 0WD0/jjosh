// Josh's local link transport currently uses the Unix `env` command for namespaced Git access.
#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

fn git_with_input(cwd: &Path, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output, Path::new("git"), args);
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

fn change_id(cwd: &Path, revision: &str) -> String {
    String::from_utf8(
        jjosh(
            cwd,
            &["log", "-r", revision, "--no-graph", "-T", "change_id"],
        )
        .stdout,
    )
    .unwrap()
    .trim()
    .to_owned()
}

fn commit_id(cwd: &Path, revision: &str) -> String {
    String::from_utf8(
        jjosh(
            cwd,
            &["log", "-r", revision, "--no-graph", "-T", "commit_id"],
        )
        .stdout,
    )
    .unwrap()
    .trim()
    .to_owned()
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

fn assert_no_divergent_changes(cwd: &Path) {
    let output = jjosh(
        cwd,
        &["log", "-r", "divergent()", "--no-graph", "-T", "commit_id"],
    );
    assert!(
        output.stdout.is_empty(),
        "embedded histories introduced divergent changes:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn create_remote(root: &Path, name: &str) -> (PathBuf, PathBuf, String) {
    let work = root.join(format!("{name}-work"));
    let bare = root.join(format!("{name}.git"));
    fs::create_dir(&work).unwrap();
    git(&work, &["init", "-b", "main"]);
    git(&work, &["config", "user.name", "Smoke Test"]);
    git(&work, &["config", "user.email", "smoke@example.com"]);
    fs::create_dir(work.join("src")).unwrap();
    fs::write(work.join("src/value.txt"), format!("{name}-v1\n")).unwrap();
    fs::write(work.join("outside.txt"), format!("{name}-outside\n")).unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-m", &format!("{name}-v1")]);
    let tip = git(&work, &["rev-parse", "HEAD"]).trim().to_owned();
    git(
        root,
        &[
            "clone",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    (work, bare, tip)
}

#[test]
fn embedded_links_fast_forward_and_restack_local_changes() {
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
    let remote_v1 = git(&remote_work, &["rev-parse", "HEAD"]).trim().to_owned();

    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    git(&client, &["switch", "--detach"]);

    let base_change_id = change_id(&client, "@");
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
            "--push-url",
            remote_bare.to_str().unwrap(),
            "--push-target",
            "main",
            "--fetch-url",
            remote_work.to_str().unwrap(),
            "--at",
            "HEAD",
        ],
    );
    let composition_change_id = change_id(&client, "@");
    assert_ne!(composition_change_id, base_change_id);
    assert_eq!(change_id(&client, "first_parent(@)"), base_change_id);
    assert_eq!(
        fs::read_to_string(client.join("deps/lib.txt")).unwrap(),
        "linked-v1\n"
    );
    assert!(!client.join("deps/outside.txt").exists());
    let link_file = fs::read_to_string(client.join("deps/.link.josh")).unwrap();
    assert!(link_file.contains("mode=\"embedded\""));
    assert!(link_file.contains(&format!("push=\"{}\"", remote_bare.display())));
    assert!(link_file.contains("push-target=\"main\""));
    let composition_commit = commit_id(&client, "@");
    let composition_parents = git(&client, &["show", "-s", "--format=%P", &composition_commit]);
    assert_eq!(composition_parents.split_whitespace().count(), 2);
    let add_log =
        String::from_utf8(jjosh(&client, &["op", "log", "--limit", "1", "--no-graph"]).stdout)
            .unwrap();
    assert!(add_log.contains("add embedded Josh link deps"));

    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            remote_work.to_str().unwrap(),
            remote_bare.to_str().unwrap(),
        ],
    );
    let unchanged_commit = commit_id(&client, "@");
    let unchanged_change = change_id(&client, "@");
    let unchanged_operation = operation_id(&client);
    let unchanged_update = jjosh(&client, &["link", "update", "deps"]);
    assert!(
        String::from_utf8_lossy(&unchanged_update.stderr)
            .contains("Selected Josh links are already up to date")
    );
    assert_eq!(commit_id(&client, "@"), unchanged_commit);
    assert_eq!(change_id(&client, "@"), unchanged_change);
    assert_eq!(operation_id(&client), unchanged_operation);
    jjosh(&client, &["new"]);
    let local_change_id = change_id(&client, "@");
    fs::write(client.join("deps/local.txt"), "local-overlay\n").unwrap();
    jjosh(&client, &["status"]);
    jjosh(&client, &["bookmark", "set", "local-stack", "-r", "@"]);
    let old_local_commit = commit_id(&client, "@");

    fs::write(remote_work.join("src/lib.txt"), "linked-v2\n").unwrap();
    git(&remote_work, &["commit", "-am", "linked-v2"]);
    let remote_v2 = git(&remote_work, &["rev-parse", "HEAD"]).trim().to_owned();
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    let update = jjosh(&client, &["link", "update", "deps", "-r", "@-"]);
    assert!(String::from_utf8_lossy(&update.stderr).contains("Rebased 1 descendant commits"));
    assert_eq!(
        change_id(&client, "first_parent(@, 2)"),
        composition_change_id
    );
    assert_eq!(change_id(&client, "@"), local_change_id);
    assert_ne!(commit_id(&client, "@"), old_local_commit);
    assert_eq!(commit_id(&client, "local-stack"), commit_id(&client, "@"));
    assert_eq!(change_id(&client, "local-stack"), local_change_id);
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
    assert_no_divergent_changes(&client);

    fs::write(client.join("deps/lib.txt"), "local-v3\n").unwrap();
    jjosh(&client, &["status"]);
    let preflight_operation = operation_id(&client);
    let preflight = jjosh(&client, &["link", "push", "deps", "--dry-run"]);
    assert!(String::from_utf8_lossy(&preflight.stderr).contains("Link push preflight succeeded"));
    assert!(String::from_utf8_lossy(&preflight.stderr).contains("Remote updated: no"));
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        remote_v2
    );
    assert_eq!(operation_id(&client), preflight_operation);
    let unrelated_tree = git(&remote_work, &["rev-parse", "HEAD^{tree}"]);
    let unrelated_commit = git(
        &remote_work,
        &["commit-tree", unrelated_tree.trim(), "-m", "unrelated"],
    );
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            &format!("{}:refs/heads/blocked", unrelated_commit.trim()),
        ],
    );
    let rejected_preflight = jjosh_unchecked(
        &client,
        &["link", "push", "deps", "--dry-run", "--to", "blocked"],
    );
    assert!(!rejected_preflight.status.success());
    let rejected_stderr = String::from_utf8_lossy(&rejected_preflight.stderr);
    assert!(
        rejected_stderr.contains("non-fast-forward") || rejected_stderr.contains("fetch first"),
        "unexpected preflight rejection:\n{rejected_stderr}"
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/blocked"]).trim(),
        unrelated_commit.trim()
    );
    let forced_preflight = jjosh(
        &client,
        &[
            "link",
            "push",
            "deps",
            "--dry-run",
            "--force",
            "--to",
            "blocked",
        ],
    );
    assert!(
        String::from_utf8_lossy(&forced_preflight.stderr).contains("Link push preflight succeeded")
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/blocked"]).trim(),
        unrelated_commit.trim()
    );
    jjosh(&client, &["link", "push", "deps"]);
    let published_tip = git(&remote_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_ne!(published_tip, remote_v2);
    assert!(
        run(
            &remote_bare,
            Path::new("git"),
            &["merge-base", "--is-ancestor", &remote_v1, &published_tip],
        )
        .status
        .success()
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/lib.txt"]),
        "local-v3\n"
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/local.txt"]),
        "local-overlay\n"
    );

    git(
        &remote_work,
        &["fetch", remote_bare.to_str().unwrap(), "main"],
    );
    git(&remote_work, &["reset", "--hard", "FETCH_HEAD"]);
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
    jjosh(&client, &["link", "update", "deps", "-r", "@-"]);
    assert_eq!(change_id(&client, "@"), local_change_id);
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

#[test]
fn multiple_embedded_links_share_one_composite_graph_and_publish_independently() {
    let temp = tempfile::tempdir().unwrap();
    let (alpha_work, alpha_bare, alpha_v1) = create_remote(temp.path(), "alpha");
    let (beta_work, beta_bare, beta_v1) = create_remote(temp.path(), "beta");
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);

    jjosh(
        &client,
        &[
            "link",
            "add",
            "alpha",
            alpha_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
            "--push-url",
            alpha_bare.to_str().unwrap(),
        ],
    );
    let alpha_composition = commit_id(&client, "@");
    jjosh(
        &client,
        &[
            "link",
            "add",
            "beta",
            beta_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
            "--push-url",
            beta_bare.to_str().unwrap(),
        ],
    );
    let composition_commit = commit_id(&client, "@");
    let composition_parents = git(&client, &["show", "-s", "--format=%P", &composition_commit]);
    let composition_parents: Vec<_> = composition_parents.split_whitespace().collect();
    assert_eq!(composition_parents.len(), 2);
    assert_eq!(composition_parents[0], alpha_composition);
    assert_eq!(
        fs::read_to_string(client.join("alpha/value.txt")).unwrap(),
        "alpha-v1\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("beta/value.txt")).unwrap(),
        "beta-v1\n"
    );

    fs::write(alpha_work.join("src/value.txt"), "alpha-v2\n").unwrap();
    git(&alpha_work, &["commit", "-am", "alpha-v2"]);
    git(
        &alpha_work,
        &["push", alpha_bare.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    fs::write(beta_work.join("src/value.txt"), "beta-v2\n").unwrap();
    git(&beta_work, &["commit", "-am", "beta-v2"]);
    git(
        &beta_work,
        &["push", beta_bare.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    jjosh(&client, &["link", "update"]);
    assert_eq!(
        fs::read_to_string(client.join("alpha/value.txt")).unwrap(),
        "alpha-v2\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("beta/value.txt")).unwrap(),
        "beta-v2\n"
    );
    let updated_commit = commit_id(&client, "@");
    let updated_parents = git(&client, &["show", "-s", "--format=%P", &updated_commit]);
    let updated_parents: Vec<_> = updated_parents.split_whitespace().collect();
    assert_eq!(updated_parents.len(), 3);
    assert_eq!(updated_parents[0], composition_commit);
    assert_no_divergent_changes(&client);

    jjosh(&client, &["new"]);
    fs::write(client.join("alpha/value.txt"), "alpha-local\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta-local\n").unwrap();
    jjosh(&client, &["status"]);
    jjosh(&client, &["link", "push", "alpha", "--to", "main"]);
    jjosh(&client, &["link", "push", "beta", "--to", "main"]);

    let alpha_published = git(&alpha_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    let beta_published = git(&beta_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert!(
        run(
            &alpha_bare,
            Path::new("git"),
            &["merge-base", "--is-ancestor", &alpha_v1, &alpha_published],
        )
        .status
        .success()
    );
    assert!(
        run(
            &beta_bare,
            Path::new("git"),
            &["merge-base", "--is-ancestor", &beta_v1, &beta_published],
        )
        .status
        .success()
    );
    assert_eq!(
        git(&alpha_bare, &["show", "refs/heads/main:src/value.txt"]),
        "alpha-local\n"
    );
    assert_eq!(
        git(&beta_bare, &["show", "refs/heads/main:src/value.txt"]),
        "beta-local\n"
    );
    assert_eq!(
        git(&alpha_bare, &["show", "refs/heads/main:outside.txt"]),
        "alpha-outside\n"
    );
    assert_eq!(
        git(&beta_bare, &["show", "refs/heads/main:outside.txt"]),
        "beta-outside\n"
    );
}

#[test]
fn adding_embedded_link_to_nonempty_mount_preserves_local_content_above_clean_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let (_remote_work, remote_bare, _) = create_remote(temp.path(), "existing");
    let client = temp.path().join("client");
    fs::create_dir_all(client.join("deps")).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    fs::write(client.join("deps/local.txt"), "preserve-me\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root with local mount"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    let incomplete_push_config = jjosh_unchecked(
        &client,
        &[
            "link",
            "add",
            "invalid",
            remote_bare.to_str().unwrap(),
            ":/src",
            "--push-target",
            "main",
        ],
    );
    assert!(!incomplete_push_config.status.success());
    assert!(
        String::from_utf8_lossy(&incomplete_push_config.stderr)
            .contains("--push-target requires --push-url")
    );

    let base_change = change_id(&client, "@");
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
        ],
    );

    assert_eq!(
        fs::read_to_string(client.join("deps/value.txt")).unwrap(),
        "existing-v1\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("deps/local.txt")).unwrap(),
        "preserve-me\n"
    );
    let overlay_commit = commit_id(&client, "@");
    let overlay_parents = git(&client, &["show", "-s", "--format=%P", &overlay_commit]);
    assert_eq!(overlay_parents.split_whitespace().count(), 1);
    let composition_commit = commit_id(&client, "first_parent(@)");
    let composition_parents = git(&client, &["show", "-s", "--format=%P", &composition_commit]);
    assert_eq!(composition_parents.split_whitespace().count(), 2);
    assert_eq!(change_id(&client, "first_parent(@, 2)"), base_change);
    assert_eq!(
        git(
            &client,
            &["show", &format!("{composition_commit}:deps/value.txt")]
        ),
        "existing-v1\n"
    );
    let local_at_boundary = run(
        &client,
        Path::new("git"),
        &[
            "cat-file",
            "-e",
            &format!("{composition_commit}:deps/local.txt"),
        ],
    );
    assert!(!local_at_boundary.status.success());
    assert_no_divergent_changes(&client);
    let missing_push_remote = jjosh_unchecked(&client, &["link", "push", "deps"]);
    assert!(!missing_push_remote.status.success());
    assert!(String::from_utf8_lossy(&missing_push_remote.stderr).contains("has no push remote"));
}

#[test]
fn link_commands_reject_sha256_git_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let client = temp.path().join("sha256-client");
    let (_remote_work, remote_bare, _) = create_remote(temp.path(), "sha1-source");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "--object-format=sha256", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "sha256 root"]);
    jjosh(&client, &["git", "init", "--colocate"]);

    let rejected = jjosh_unchecked(
        &client,
        &[
            "link",
            "add",
            "deps",
            remote_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("jjosh currently supports only SHA-1 Git repositories")
    );
}

#[test]
fn snapshot_link_migrates_to_embedded_history_without_losing_local_changes() {
    let temp = tempfile::tempdir().unwrap();
    let (_remote_work, remote_bare, remote_v1) = create_remote(temp.path(), "migration");
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);
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
            "--mode",
            "snapshot",
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "local-migration-change\n").unwrap();
    jjosh(&client, &["status"]);

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
            "--push-url",
            remote_bare.to_str().unwrap(),
            "--push-target",
            "embedded",
        ],
    );

    assert_eq!(
        fs::read_to_string(client.join("deps/value.txt")).unwrap(),
        "local-migration-change\n"
    );
    let status = String::from_utf8(jjosh(&client, &["status"]).stdout).unwrap();
    assert!(!status.contains("conflict"));
    let composition_commit = commit_id(&client, "first_parent(@)");
    assert_eq!(
        git(
            &client,
            &["show", &format!("{composition_commit}:deps/value.txt")]
        ),
        "migration-v1\n"
    );
    assert_eq!(
        git(&client, &["show", "-s", "--format=%P", &composition_commit])
            .split_whitespace()
            .count(),
        2
    );
    jjosh(&client, &["link", "push", "deps"]);
    let published = git(&remote_bare, &["rev-parse", "refs/heads/embedded"]);
    assert!(
        run(
            &remote_bare,
            Path::new("git"),
            &["merge-base", "--is-ancestor", &remote_v1, published.trim()],
        )
        .status
        .success()
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/embedded:src/value.txt"]),
        "local-migration-change\n"
    );
    assert_no_divergent_changes(&client);
}

#[test]
fn embedded_update_rejects_non_fast_forward_source_history() {
    let temp = tempfile::tempdir().unwrap();
    let (remote_work, remote_bare, _) = create_remote(temp.path(), "rewritten");
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);
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
        ],
    );
    let old_commit = commit_id(&client, "@");
    let old_operation = operation_id(&client);

    let source_tree = git(&remote_work, &["rev-parse", "HEAD^{tree}"]);
    let rewritten_tip = git(
        &remote_work,
        &["commit-tree", source_tree.trim(), "-m", "rewritten root"],
    );
    git(
        &remote_work,
        &[
            "push",
            "--force",
            remote_bare.to_str().unwrap(),
            &format!("{}:refs/heads/main", rewritten_tip.trim()),
        ],
    );

    let rejected = jjosh_unchecked(&client, &["link", "update", "deps"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("did not advance by fast-forward"));
    assert_eq!(commit_id(&client, "@"), old_commit);
    assert_eq!(operation_id(&client), old_operation);
    assert_no_divergent_changes(&client);
}

#[test]
fn embedded_update_rejects_materialized_change_id_divergence() {
    let temp = tempfile::tempdir().unwrap();
    let (remote_work, remote_bare, _) = create_remote(temp.path(), "evolution");
    let (_second_work, second_bare, _) = create_remote(temp.path(), "second");
    let original_commit = git(&remote_work, &["cat-file", "-p", "HEAD"]);
    let (headers, message) = original_commit.split_once("\n\n").unwrap();
    let source_change_id = "ossootuzwvxsosqnzywrvoyrtknwszpo";
    let commit_with_change_id = format!("{headers}\nchange-id {source_change_id}\n\n{message}");
    let source_v1 = git_with_input(
        &remote_work,
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        commit_with_change_id.as_bytes(),
    );
    git(
        &remote_work,
        &["update-ref", "refs/heads/main", source_v1.trim()],
    );
    git(
        &remote_work,
        &[
            "push",
            "--force",
            remote_bare.to_str().unwrap(),
            &format!("{}:refs/heads/main", source_v1.trim()),
        ],
    );

    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);
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
        ],
    );
    jjosh(
        &client,
        &[
            "link",
            "add",
            "second",
            second_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
        ],
    );
    let old_commit = commit_id(&client, "@");
    let old_operation = operation_id(&client);

    fs::write(remote_work.join("src/value.txt"), "evolution-v2\n").unwrap();
    git(&remote_work, &["commit", "-am", "evolution-v2"]);
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );

    let rejected = jjosh_unchecked(&client, &["link", "update", "deps"]);
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("rewrites commits carrying change-id"));
    assert!(stderr.contains(source_change_id));
    assert_eq!(commit_id(&client, "@"), old_commit);
    assert_eq!(operation_id(&client), old_operation);
    assert_no_divergent_changes(&client);
}
