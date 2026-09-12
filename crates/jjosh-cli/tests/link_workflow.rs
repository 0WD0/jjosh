// These workflow tests exercise local Git repositories and file URLs.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

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
    String::from_utf8(
        jjosh(
            cwd,
            &["op", "log", "--limit", "1", "--no-graph", "-T", "id"],
        )
        .stdout,
    )
    .unwrap()
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

fn create_client(root: &Path, colocated: bool) -> PathBuf {
    let client = root.join("client");
    fs::create_dir(&client).unwrap();
    if colocated {
        git(&client, &["init", "-b", "main"]);
        git(&client, &["config", "user.name", "Smoke Test"]);
        git(&client, &["config", "user.email", "smoke@example.com"]);
        fs::write(client.join("root.txt"), "root\n").unwrap();
        git(&client, &["add", "."]);
        git(&client, &["commit", "-m", "root"]);
        jjosh(&client, &["git", "init", "--colocate"]);
    } else {
        jjosh(
            &client,
            &["git", "init", "--no-colocate", "--object-hash", "sha1"],
        );
        fs::write(client.join("root.txt"), "root\n").unwrap();
        jjosh(&client, &["new", "-m", "local overlay"]);
    }
    client
}

fn file_at_revision(client: &Path, revision: &str, path: &str) -> Vec<u8> {
    jjosh(client, &["file", "show", "-r", revision, path]).stdout
}

#[test]
fn link_push_prunes_empty_exported_commits_regardless_of_revision_or_description() {
    let temp = tempfile::tempdir().unwrap();
    let (remote_work, remote_bare, _) = create_remote(temp.path(), "empty-tip");
    git(
        &remote_work,
        &["commit", "--allow-empty", "--allow-empty-message", "-m", ""],
    );
    git(
        &remote_work,
        &["push", remote_bare.to_str().unwrap(), "main"],
    );
    let source_tip = git(&remote_bare, &["rev-parse", "refs/heads/main"]);
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    jjosh(
        &client,
        &["git", "init", "--no-colocate", "--object-hash", "sha1"],
    );
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
        ],
    );
    // Prune only new publication history, never rewrite the pinned upstream.
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "unchanged#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/unchanged"]),
        source_tip
    );
    fs::write(client.join("deps/value.txt"), "local change\n").unwrap();
    jjosh(&client, &["describe", "-m", "Actual change"]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "expected#deps=@",
            "--allow-empty-description",
        ],
    );
    let expected_tip = git(&remote_bare, &["rev-parse", "refs/heads/expected"])
        .trim()
        .to_owned();
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/expected^"]),
        source_tip
    );

    jjosh(&client, &["new"]);
    jjosh(&client, &["new"]);
    let empty_tip = commit_id(&client, "@");
    let before_operation = operation_id(&client);
    let before_refs = git(&remote_bare, &["show-ref"]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "topic#deps=@",
            "--allow-empty-description",
            "--dry-run",
        ],
    );
    assert_eq!(operation_id(&client), before_operation);
    assert_eq!(git(&remote_bare, &["show-ref"]), before_refs);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "topic#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/topic"]).trim(),
        expected_tip
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/topic:src/value.txt"]),
        "local change\n"
    );
    assert_eq!(commit_id(&client, "@"), empty_tip);

    // Explicit selection applies the same content-based pruning.
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "explicit#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/explicit"]).trim(),
        expected_tip
    );

    // A description cannot make an empty exported change meaningful.
    jjosh(&client, &["describe", "-m", "Release checkpoint"]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "checkpoint#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/checkpoint"]).trim(),
        expected_tip
    );

    // Out-of-scope changes and originally empty commits are equivalent:
    // neither contributes a commit to this link's published history.
    jjosh(&client, &["new", "-m", "Only change another project"]);
    fs::write(client.join("outside.txt"), "unrelated local content\n").unwrap();
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "outside#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/outside"]).trim(),
        expected_tip
    );
    jjosh(&client, &["new", "-m", "Next actual change"]);
    fs::write(client.join("deps/value.txt"), "next local change\n").unwrap();
    jjosh(&client, &["new"]);
    let local_tip = commit_id(&client, "@");
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "continued#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(
            &remote_bare,
            &["rev-list", "--count", "expected..continued"]
        )
        .trim(),
        "1"
    );
    assert_eq!(
        git(&remote_bare, &["show", "continued:src/value.txt"]),
        "next local change\n"
    );
    assert_eq!(commit_id(&client, "@"), local_tip);
}

#[test]
fn link_push_prunes_empty_branches_without_losing_meaningful_merges() {
    let temp = tempfile::tempdir().unwrap();
    let (_remote_work, remote_bare, _) = create_remote(temp.path(), "merge-tip");
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    jjosh(
        &client,
        &["git", "init", "--no-colocate", "--object-hash", "sha1"],
    );
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
        ],
    );
    fs::write(client.join("deps/left.txt"), "left\n").unwrap();
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "left#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(git(&remote_bare, &["show", "left:src/left.txt"]), "left\n");
    let left = commit_id(&client, "@");
    jjosh(
        &client,
        &["new", "main#deps@deps-upstream", "-m", "Right change"],
    );
    fs::write(client.join("deps/right.txt"), "right\n").unwrap();
    let right = commit_id(&client, "@");
    jjosh(&client, &["new", &left, &right]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "merged#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["show", "merged:src/left.txt"]),
        "left\n"
    );
    assert_eq!(
        git(&remote_bare, &["show", "merged:src/right.txt"]),
        "right\n"
    );
    assert_eq!(
        git(&remote_bare, &["show", "-s", "--format=%P", "merged"])
            .split_whitespace()
            .count(),
        2
    );

    // An empty side branch collapses to the source, so its merge with a
    // content-bearing branch must not create an empty merge commit either.
    jjosh(
        &client,
        &["new", "main#deps@deps-upstream", "-m", "Empty side branch"],
    );
    let empty_branch = commit_id(&client, "@");
    jjosh(&client, &["new", &left, &empty_branch]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "collapsed#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/collapsed"]),
        git(&remote_bare, &["rev-parse", "refs/heads/left"])
    );
}

#[test]
fn link_push_rewrites_published_changes_with_independent_destination_leases() {
    let temp = tempfile::tempdir().unwrap();
    let (_remote_work, remote_bare, _) = create_remote(temp.path(), "rewrite");
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
            "--push-url",
            remote_bare.to_str().unwrap(),
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published-v1\n").unwrap();
    jjosh(&client, &["status"]);
    let local_change = change_id(&client, "@");
    jjosh(
        &client,
        &["bookmark", "track", "main#deps@deps-upstream"],
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    let first_tip = git(&remote_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "published-v1\n"
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "topic#deps=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/topic"]).trim(),
        first_tip
    );

    fs::write(client.join("deps/value.txt"), "published-v2\n").unwrap();
    jjosh(&client, &["status"]);
    assert_eq!(change_id(&client, "@"), local_change);
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    let preflight_operation = operation_id(&client);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
            "--dry-run",
        ],
    );
    assert_eq!(operation_id(&client), preflight_operation);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        first_tip
    );
    // A dry-run must leave the lease at the actually published v1.
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    let second_tip = git(&remote_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "published-v2\n"
    );
    assert_eq!(
        run(
            &remote_bare,
            Path::new("git"),
            &["merge-base", "--is-ancestor", &first_tip, &second_tip],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/topic"]).trim(),
        first_tip
    );

    // Main remembers v2, while topic must still use its own v1 lease.
    fs::write(client.join("deps/value.txt"), "published-v3\n").unwrap();
    jjosh(&client, &["status"]);
    assert_eq!(change_id(&client, "@"), local_change);
    jjosh(
        &client,
        &["bookmark", "set", "topic#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "topic#deps",
            "--allow-empty-description",
        ],
    );
    let topic_tip = git(&remote_bare, &["rev-parse", "refs/heads/topic"])
        .trim()
        .to_owned();
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/topic:src/value.txt"]),
        "published-v3\n"
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        second_tip
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        topic_tip
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "published-v3\n"
    );
}

#[test]
fn link_push_rejects_external_advances_without_refreshing_its_lease() {
    let temp = tempfile::tempdir().unwrap();
    let (remote_work, remote_bare, _) = create_remote(temp.path(), "concurrent");
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
            "--push-url",
            remote_bare.to_str().unwrap(),
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["status"]);
    let local_change = change_id(&client, "@");
    jjosh(
        &client,
        &["bookmark", "track", "main#deps@deps-upstream"],
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    let published_tip = git(&remote_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "published\n"
    );

    git(
        &remote_work,
        &["fetch", remote_bare.to_str().unwrap(), "main"],
    );
    git(&remote_work, &["switch", "--detach", "FETCH_HEAD"]);
    fs::write(remote_work.join("src/value.txt"), "other-writer\n").unwrap();
    git(&remote_work, &["commit", "-am", "other writer"]);
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    let external_tip = git(&remote_work, &["rev-parse", "HEAD"]).trim().to_owned();

    fs::write(client.join("deps/value.txt"), "local-rewrite\n").unwrap();
    jjosh(&client, &["status"]);
    assert_eq!(change_id(&client, "@"), local_change);
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    let preflight_operation = operation_id(&client);
    let rejected_preflight = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
            "--dry-run",
        ],
    );
    assert!(!rejected_preflight.status.success());
    assert_eq!(operation_id(&client), preflight_operation);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        external_tip
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "other-writer\n"
    );
    let rejected_push = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    assert!(!rejected_push.status.success());
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        external_tip
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "other-writer\n"
    );
    // A rejected real push must not bless the other writer's tip either.
    let rejected_retry = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    assert!(!rejected_retry.status.success());
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        external_tip
    );

    // Once the other writer restores our last publication, the original lease
    // must still work: failed attempts must not remember the unpublished rewrite.
    git(
        &remote_work,
        &[
            "push",
            "--force",
            remote_bare.to_str().unwrap(),
            &format!("{published_tip}:refs/heads/main"),
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "local-rewrite\n"
    );
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
}

#[test]
fn source_update_refreshes_only_the_observed_publication_branch() {
    let temp = tempfile::tempdir().unwrap();
    let (remote_work, remote_bare, _) = create_remote(temp.path(), "observed");
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "scaffold\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "scaffold"]);
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
            "--push-url",
            remote_bare.to_str().unwrap(),
        ],
    );
    fs::write(client.join("deps/value.txt"), "published local change\n").unwrap();
    jjosh(&client, &["describe", "-m", "local change"]);
    jjosh(
        &client,
        &["bookmark", "track", "main#deps@deps-upstream"],
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--named",
            "topic#deps=@",
            "--allow-empty-description",
        ],
    );
    jjosh(&client, &["new"]);
    let local_change = change_id(&client, "@");

    git(
        &remote_work,
        &["fetch", remote_bare.to_str().unwrap(), "main"],
    );
    git(&remote_work, &["switch", "--detach", "FETCH_HEAD"]);
    fs::write(
        remote_work.join("outside.txt"),
        "new hidden upstream content\n",
    )
    .unwrap();
    git(
        &remote_work,
        &["commit", "-am", "upstream outside the projection"],
    );
    git(
        &remote_work,
        &[
            "push",
            remote_bare.to_str().unwrap(),
            "HEAD:main",
            "HEAD:topic",
        ],
    );
    let observed = git(&remote_work, &["rev-parse", "HEAD"]);
    fs::write(client.join("deps/pending.txt"), "new local change\n").unwrap();
    jjosh(&client, &["status"]);
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "deps-upstream",
                "--bookmark",
                "main#deps",
                "--allow-empty-description"
            ]
        )
        .status
        .success()
    );

    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "deps-upstream",
            "--branch",
            "main",
        ],
    );
    assert_eq!(change_id(&client, "@"), local_change);
    assert_eq!(
        fs::read_to_string(client.join("deps/pending.txt")).unwrap(),
        "new local change\n"
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&remote_bare, &["show", "main:src/pending.txt"]),
        "new local change\n"
    );
    assert_eq!(
        git(&remote_bare, &["show", "main:outside.txt"]),
        "new hidden upstream content\n"
    );
    // Fetching main must not authorize overwriting the independently changed topic.
    jjosh(
        &client,
        &["bookmark", "set", "topic#deps", "-r", "@", "--allow-backwards"],
    );
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "deps-upstream",
                "--bookmark",
                "topic#deps",
                "--allow-empty-description"
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "topic"]).trim(),
        observed.trim()
    );
}

#[test]
fn named_remote_sync_ignores_redirected_or_malformed_link_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let (remote_work, remote_bare, _) = create_remote(temp.path(), "malformed");
    let (_other_work, other_bare, _) = create_remote(temp.path(), "other");
    let client = create_client(temp.path(), true);
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
        ],
    );
    let valid_metadata = fs::read_to_string(client.join("deps/.link.josh")).unwrap();
    let redirected_metadata = valid_metadata
        .replace(remote_bare.to_str().unwrap(), other_bare.to_str().unwrap())
        .replace("\"main\"", "\"redirected\"");
    let other_refs = git(&other_bare, &["show-ref"]);
    fs::write(client.join("deps/local.txt"), "local publication\n").unwrap();
    jjosh(&client, &["describe", "-m", "local publication"]);
    fs::write(remote_work.join("src/value.txt"), "upstream-v2\n").unwrap();
    git(&remote_work, &["commit", "-am", "upstream-v2"]);
    git(
        &remote_work,
        &["push", remote_bare.to_str().unwrap(), "HEAD:main"],
    );
    for (index, marker) in [
        redirected_metadata.as_str(),
        ":link[mode=\"embedded\",commit=\"unterminated\n",
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(client.join("deps/.link.josh"), marker).unwrap();
        let local = commit_id(&client, "@");
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream",
                "--branch",
                "main",
            ],
        );
        assert_eq!(
            file_at_revision(&client, "main#deps@deps-upstream", "deps/value.txt"),
            b"upstream-v2\n"
        );
        jjosh(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "deps-upstream",
                "--named",
                &format!("published-{index}#deps=@"),
                "--allow-empty-description",
            ],
        );
        assert_eq!(
            git(
                &remote_bare,
                &["show", &format!("published-{index}:src/local.txt")]
            ),
            "local publication\n"
        );
        assert_eq!(git(&other_bare, &["show-ref"]), other_refs);
        assert_eq!(commit_id(&client, "@"), local);
        assert_eq!(
            fs::read_to_string(client.join("deps/.link.josh")).unwrap(),
            marker
        );
    }
}

#[test]
fn selected_link_fetch_preserves_work_and_handles_tags_rewrites_and_deletions() {
    for colocated in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (work, bare, _) = create_remote(temp.path(), "selection");
        git(&work, &["branch", "keep"]);
        git(&work, &["tag", "-a", "v1", "-m", "annotated release"]);
        git(
            &work,
            &["push", bare.to_str().unwrap(), "keep", "refs/tags/v1"],
        );
        let client = create_client(temp.path(), colocated);
        jjosh(
            &client,
            &[
                "link",
                "add",
                "deps",
                bare.to_str().unwrap(),
                ":/src",
                "--target",
                "main",
            ],
        );
        fs::write(client.join("deps/local.txt"), "local work\n").unwrap();
        let local = commit_id(&client, "@");
        let initial = commit_id(&client, "main#deps@deps-upstream");
        let metadata = fs::read(client.join("deps/.link.josh")).unwrap();
        // A link without an explicit push URL remains read-only, even though
        // its fetch endpoint happens to be a writable local repository.
        let readonly_refs = git(&bare, &["show-ref"]);
        let readonly_operation = operation_id(&client);
        let rejected = jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "deps-upstream",
                "--named",
                "readonly#deps=@",
                "--allow-empty-description",
            ],
        );
        assert!(!rejected.status.success());
        assert_eq!(git(&bare, &["show-ref"]), readonly_refs);
        assert_eq!(operation_id(&client), readonly_operation);
        assert_eq!(commit_id(&client, "@"), local);
        fs::write(work.join("src/value.txt"), "upstream v2\n").unwrap();
        git(&work, &["commit", "-am", "v2"]);
        git(&work, &["push", bare.to_str().unwrap(), "main"]);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream",
                "--tag",
                "glob:v*",
            ],
        );
        assert_eq!(commit_id(&client, "main#deps@deps-upstream"), initial);
        assert_eq!(
            file_at_revision(
                &client,
                "remote_tags(exact:\"v1#deps\", exact:\"deps-upstream\")",
                "deps/value.txt"
            ),
            b"selection-v1\n"
        );
        let before_fetch = operation_id(&client);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream",
                "--branch",
                "main",
                "--branch",
                "keep",
            ],
        );
        let updated = commit_id(&client, "main#deps@deps-upstream");
        assert_ne!(updated, initial);
        assert_eq!(commit_id(&client, "@"), local);
        assert_eq!(fs::read(client.join("deps/.link.josh")).unwrap(), metadata);
        assert_eq!(
            fs::read(client.join("deps/value.txt")).unwrap(),
            b"selection-v1\n"
        );
        assert_eq!(
            file_at_revision(&client, "main#deps@deps-upstream", "deps/value.txt"),
            b"upstream v2\n"
        );
        jjosh(&client, &["op", "restore", &before_fetch]);
        jjosh(&client, &["git", "export"]);
        jjosh(&client, &["git", "import"]);
        assert_eq!(commit_id(&client, "main#deps@deps-upstream"), initial);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream",
                "--branch",
                "main",
                "--branch",
                "keep",
            ],
        );
        assert_eq!(commit_id(&client, "main#deps@deps-upstream"), updated);
        fs::write(work.join("src/value.txt"), "rewritten upstream\n").unwrap();
        git(&work, &["commit", "--amend", "-am", "rewritten"]);
        git(&work, &["push", "--force", bare.to_str().unwrap(), "main"]);
        git(
            &work,
            &["push", bare.to_str().unwrap(), ":keep", ":refs/tags/v1"],
        );
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream",
                "--branch",
                "main",
            ],
        );
        assert_eq!(commit_id(&client, "@"), local);
        assert_eq!(
            file_at_revision(&client, "main#deps@deps-upstream", "deps/value.txt"),
            b"rewritten upstream\n"
        );
        assert_eq!(commit_id(&client, "keep#deps@deps-upstream"), initial);
        assert_eq!(
            file_at_revision(
                &client,
                "remote_tags(exact:\"v1#deps\", exact:\"deps-upstream\")",
                "deps/value.txt"
            ),
            b"selection-v1\n"
        );
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream",
                "--branch",
                "keep",
                "--tag",
                "v1",
            ],
        );
        assert_eq!(
            commit_id(
                &client,
                "remote_bookmarks(exact:\"keep#deps\", exact:\"deps-upstream\")"
            ),
            ""
        );
        assert_eq!(
            commit_id(
                &client,
                "remote_tags(exact:\"v1#deps\", exact:\"deps-upstream\")"
            ),
            ""
        );
        jjosh(&client, &["new", "@", "main#deps@deps-upstream"]);
        assert_eq!(
            fs::read(client.join("deps/value.txt")).unwrap(),
            b"rewritten upstream\n"
        );
        assert_eq!(
            fs::read(client.join("deps/local.txt")).unwrap(),
            b"local work\n"
        );
        assert_eq!(fs::read(client.join("deps/.link.josh")).unwrap(), metadata);
        assert_eq!(fs::read(client.join("root.txt")).unwrap(), b"root\n");
    }
}

#[test]
fn imported_soft_fork_attaches_upstream_without_reimporting_local_history() {
    let temp = tempfile::tempdir().unwrap();
    let (work, bare, _) = create_remote(temp.path(), "fork");
    let upstream = temp.path().join("upstream");
    git(
        temp.path(),
        &["clone", bare.to_str().unwrap(), upstream.to_str().unwrap()],
    );
    git(&upstream, &["config", "user.name", "Smoke Test"]);
    git(&upstream, &["config", "user.email", "smoke@example.com"]);
    jjosh(&work, &["git", "init", "--colocate"]);
    fs::write(work.join("src/local.txt"), "maintained fork\n").unwrap();
    jjosh(&work, &["describe", "-m", "local fork"]);
    jjosh(&work, &["bookmark", "set", "main"]);
    let client = create_client(temp.path(), false);
    git(&upstream, &["tag", "v1"]);
    git(&upstream, &["push", "origin", "refs/tags/v1"]);
    jjosh(
        &client,
        &[
            "native",
            "import",
            "--source",
            &format!("app={}", work.display()),
        ],
    );
    jjosh(&client, &["new", "@", "workspace/default#app"]);
    let fork = commit_id(&client, "main#app");
    let before = jjosh(
        &client,
        &[
            "log",
            "--no-graph",
            "-r",
            "::main#app",
            "-T",
            "commit_id ++ \"\\n\"",
        ],
    )
    .stdout;
    jjosh(
        &client,
        &[
            "link",
            "add",
            "app",
            bare.to_str().unwrap(),
            "--target",
            "main",
            "--remote-name",
            "upstream",
        ],
    );
    assert_eq!(commit_id(&client, "main#app"), fork);
    jjosh(
        &client,
        &["git", "fetch", "--remote", "app-upstream", "--tag", "v1"],
    );
    assert!(!commit_id(&client, "remote_tags(exact:v1#app, exact:app-upstream)").is_empty());
    assert_eq!(
        jjosh(
            &client,
            &[
                "log",
                "--no-graph",
                "-r",
                "::main#app",
                "-T",
                "commit_id ++ \"\\n\""
            ]
        )
        .stdout,
        before
    );
    fs::write(upstream.join("src/value.txt"), "upstream advancement\n").unwrap();
    git(&upstream, &["commit", "-am", "upstream advancement"]);
    git(&upstream, &["push", "origin", "main"]);
    let local = commit_id(&client, "@");
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "app-upstream",
            "--branch",
            "main",
        ],
    );
    assert_eq!(commit_id(&client, "@"), local);
    assert_eq!(commit_id(&client, "main#app"), fork);
    assert_eq!(
        commit_id(&client, "parents(main#app@app-upstream)"),
        commit_id(&client, "parents(main#app)")
    );
    jjosh(&client, &["new", "@", "main#app@app-upstream"]);
    assert_eq!(
        fs::read(client.join("app/src/local.txt")).unwrap(),
        b"maintained fork\n"
    );
    assert_eq!(
        fs::read(client.join("app/src/value.txt")).unwrap(),
        b"upstream advancement\n"
    );
    assert_eq!(fs::read(client.join("root.txt")).unwrap(), b"root\n");
    assert_no_divergent_changes(&client);
    let base = commit_id(&client, "@");
    jjosh(&client, &["new", &base]);
    fs::write(client.join("root.txt"), "left\n").unwrap();
    let left = commit_id(&client, "@");
    jjosh(&client, &["new", &base]);
    fs::write(client.join("root.txt"), "right\n").unwrap();
    let right = commit_id(&client, "@");
    jjosh(&client, &["new", &left, &right]);
    let conflicted = commit_id(&client, "@ & conflicts()");
    assert_eq!(conflicted, commit_id(&client, "@"));
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "app-upstream",
            "--branch",
            "main",
        ],
    );
    assert_eq!(commit_id(&client, "@ & conflicts()"), conflicted);
}

#[test]
fn link_add_names_observations_by_project_not_mount_path() {
    let temp = tempfile::tempdir().unwrap();
    let (_work, bare, _) = create_remote(temp.path(), "nested");
    let client = create_client(temp.path(), false);
    jjosh(
        &client,
        &[
            "link",
            "add",
            "vendor/deps",
            bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
        ],
    );
    let names = String::from_utf8(
        jjosh(
            &client,
            &[
                "bookmark",
                "list",
                "--all-remotes",
                "-T",
                r#"if(remote && remote != "git", name ++ "@" ++ remote ++ "\n")"#,
            ],
        )
        .stdout,
    )
    .unwrap();
    assert!(
        names.contains("main#deps@deps-upstream"),
        "expected project identity, got:\n{names}"
    );
    assert!(
        !names.contains("%2F") && !names.contains("vendor"),
        "mount path leaked into observation names:\n{names}"
    );
    jjosh(&client, &["new", "@", "main#deps@deps-upstream"]);
    assert_eq!(
        fs::read_to_string(client.join("vendor/deps/value.txt")).unwrap(),
        "nested-v1\n"
    );
}

#[test]
fn link_push_then_fetch_reuses_published_local_change() {
    let temp = tempfile::tempdir().unwrap();
    let (_work, remote_bare, _) = create_remote(temp.path(), "roundtrip");
    let client = create_client(temp.path(), false);
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
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["describe", "-m", "published local change"]);
    let published = commit_id(&client, "@");
    let published_change = change_id(&client, "@");
    jjosh(
        &client,
        &["bookmark", "track", "main#deps@deps-upstream"],
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "deps-upstream",
            "--branch",
            "main",
        ],
    );
    assert_no_divergent_changes(&client);
    assert_eq!(commit_id(&client, "main#deps@deps-upstream"), published);
    assert_eq!(
        change_id(&client, "main#deps@deps-upstream"),
        published_change
    );
}

#[test]
fn link_push_then_fetches_descendant_without_duplicate_published_change() {
    let temp = tempfile::tempdir().unwrap();
    let (work, remote_bare, _) = create_remote(temp.path(), "descendant");
    let client = create_client(temp.path(), false);
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
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["describe", "-m", "published local change"]);
    let published = commit_id(&client, "@");
    jjosh(
        &client,
        &["bookmark", "track", "main#deps@deps-upstream"],
    );
    jjosh(
        &client,
        &["bookmark", "set", "main#deps", "-r", "@", "--allow-backwards"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "deps-upstream",
            "--bookmark",
            "main#deps",
            "--allow-empty-description",
        ],
    );

    git(&work, &["fetch", remote_bare.to_str().unwrap(), "main"]);
    git(&work, &["reset", "--hard", "FETCH_HEAD"]);
    fs::write(work.join("src/value.txt"), "upstream descendant\n").unwrap();
    git(&work, &["add", "src/value.txt"]);
    git(&work, &["commit", "-m", "upstream descendant"]);
    git(&work, &["push", remote_bare.to_str().unwrap(), "HEAD:main"]);

    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "deps-upstream",
            "--branch",
            "main",
        ],
    );
    assert_no_divergent_changes(&client);
    assert_eq!(
        commit_id(&client, "parents(main#deps@deps-upstream)"),
        published
    );
    assert_eq!(
        file_at_revision(&client, "main#deps@deps-upstream", "deps/value.txt"),
        b"upstream descendant\n"
    );
}
