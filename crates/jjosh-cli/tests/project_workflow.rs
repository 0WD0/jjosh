// These workflow tests exercise local Git repositories and file URLs.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

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
            &[
                "--ignore-working-copy",
                "op",
                "log",
                "--limit",
                "1",
                "--no-graph",
                "-T",
                "id",
            ],
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

#[test]
fn slow_project_fetch_does_not_block_read_only_log() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_work, remote, _tip) = create_remote(root, "slow-fetch");
    let client = create_client(root, true);
    jjosh(&client, &["project", "add", "p", "--path", "p"]);
    fs::create_dir(client.join("p")).unwrap();
    fs::write(client.join("p/local.txt"), "local project edit\n").unwrap();
    let remote_url = format!("ssh://dummy{}", remote.display());
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "upstream",
            &remote_url,
            "--whole",
            "--project",
            "p",
            "--base",
            "main",
        ],
    );

    let gate = root.join("fetch-gate");
    fs::create_dir(&gate).unwrap();
    let ssh = root.join("fake-ssh");
    fs::write(
        &ssh,
        r#"#!/bin/sh
set -eu
case " $* " in *" -G "*) exit 0 ;; esac
: > "$GATE_DIR/ready"
while [ ! -e "$GATE_DIR/go" ]; do sleep 0.02; done
while [ "$#" -gt 0 ]; do
  case "$1" in
    "git-upload-pack")
      shift
      path=$1
      path=${path#\'}
      path=${path%\'}
      exec git-upload-pack "$path"
      ;;
    "git-upload-pack "*) exec sh -c "$1" ;;
  esac
  shift
done
exit 2
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&ssh).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ssh, permissions).unwrap();

    let program = Path::new(env!("CARGO_BIN_EXE_jjosh"));
    let mut fetch = Command::new(program)
        .args([
            "--no-pager",
            "--color=never",
            "git",
            "fetch",
            "--project",
            "p",
            "--remote",
            "upstream",
            "--branch",
            "main",
        ])
        .current_dir(&client)
        .env("GATE_DIR", &gate)
        .env("GIT_SSH_COMMAND", &ssh)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gate.join("ready").exists() {
        if let Some(status) = fetch.try_wait().unwrap() {
            panic!("fetch exited before reaching upload-pack gate: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "fetch did not reach upload-pack gate"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let mut log = Command::new(program)
        .args([
            "--no-pager",
            "--color=never",
            "log",
            "-r",
            "@",
            "--no-graph",
            "-T",
            "jjosh_log_compact",
        ])
        .current_dir(&client)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let log_status = loop {
        if let Some(status) = log.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            fs::write(gate.join("go"), b"").unwrap();
            let _ = fetch.wait();
            let _ = log.kill();
            panic!("jjosh log blocked behind network fetch");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        log_status.success(),
        "jjosh log failed while fetch was blocked"
    );
    assert!(
        fetch.try_wait().unwrap().is_none(),
        "fetch should still be blocked at upload-pack gate"
    );

    fs::write(gate.join("go"), b"").unwrap();
    let output = fetch.wait_with_output().unwrap();
    assert_success(
        &output,
        program,
        &[
            "git",
            "fetch",
            "--project",
            "p",
            "--remote",
            "upstream",
            "--branch",
            "main",
        ],
    );
    let log_output = log.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&log_output.stdout).contains("[p]"),
        "project summary was not rendered: {}",
        String::from_utf8_lossy(&log_output.stdout)
    );
}

#[test]
fn whole_parent_contains_filtered_child_without_recursively_unfiltering_it() {
    for colocated in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (_, child, _) = create_remote(temp.path(), "child");
        let parent = temp.path().join("parent.git");
        git(temp.path(), &["init", "--bare", parent.to_str().unwrap()]);
        let client = create_client(temp.path(), colocated);
        import_project(&client, "child", "bundle/child", &child, ":/src");
        jjosh(&client, &["bookmark", "track", "main#child@child-upstream"]);
        jjosh(&client, &["project", "add", "bundle", "--path", "bundle"]);
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "add",
                "origin",
                parent.to_str().unwrap(),
                "--whole",
                "--project",
                "bundle",
            ],
        );
        fs::write(client.join("bundle/glue.txt"), "parent glue\n").unwrap();
        fs::write(
            client.join("bundle/child/value.txt"),
            "filtered child edit\n",
        )
        .unwrap();
        jjosh(&client, &["describe", "-m", "parent and filtered child"]);
        let canonical = commit_id(&client, "@");
        jjosh(&client, &["bookmark", "set", "main#bundle", "main#child"]);
        let child_before = git(&child, &["rev-parse", "main"]);
        jjosh(
            &client,
            &[
                "git",
                "push",
                "--bookmark",
                "main#bundle",
                "--allow-empty-description",
            ],
        );
        assert_eq!(git(&child, &["rev-parse", "main"]), child_before);
        assert_eq!(
            git(&parent, &["ls-tree", "-r", "--name-only", "main"]),
            "child/value.txt\nglue.txt\n"
        );
        assert_eq!(
            git(&parent, &["show", "main:child/value.txt"]),
            "filtered child edit\n"
        );
        let parent_before = git(&parent, &["rev-parse", "main"]);
        jjosh(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "child-upstream",
                "--bookmark",
                "main#child",
                "--allow-empty-description",
            ],
        );
        assert_eq!(git(&parent, &["rev-parse", "main"]), parent_before);
        assert_eq!(
            git(&child, &["show", "main:src/value.txt"]),
            "filtered child edit\n"
        );
        assert_eq!(
            git(&child, &["show", "main:outside.txt"]),
            "child-outside\n"
        );
        assert_eq!(
            git(&child, &["ls-tree", "-r", "--name-only", "main"]),
            "outside.txt\nsrc/value.txt\n"
        );
        jjosh(
            &client,
            &["git", "fetch", "--project", "bundle", "--branch", "main"],
        );
        assert_eq!(commit_id(&client, "main#bundle@origin"), canonical);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--project",
                "child",
                "--remote",
                "child-upstream",
                "--branch",
                "main",
            ],
        );
        assert_eq!(commit_id(&client, "main#child@child-upstream"), canonical);
        jjosh(&client, &["project", "check"]);
    }
}

#[test]
fn project_push_validates_only_commits_that_survive_scope_projection() {
    let temp = tempfile::tempdir().unwrap();
    let remote = temp.path().join("project.git");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "p", "--path", "p"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            remote.to_str().unwrap(),
            "--whole",
            "--project",
            "p",
        ],
    );

    fs::create_dir(client.join("p")).unwrap();
    fs::write(client.join("p/value.txt"), "base\n").unwrap();
    jjosh(&client, &["describe", "-m", "project base"]);

    // This empty-description commit changes only the monorepo outside project p. It must be
    // pruned before push metadata validation, just as it is pruned from the exported history.
    jjosh(&client, &["new"]);
    fs::write(client.join("outside-only.txt"), "outside\n").unwrap();
    jjosh(&client, &["new", "-m", "project update"]);
    fs::write(client.join("p/value.txt"), "updated\n").unwrap();
    jjosh(&client, &["bookmark", "set", "main#p", "-r", "@"]);

    let dry_run = jjosh_unchecked(
        &client,
        &["git", "push", "--bookmark", "main#p", "--dry-run"],
    );
    assert_success(
        &dry_run,
        Path::new(env!("CARGO_BIN_EXE_jjosh")),
        &["git", "push", "--bookmark", "main#p", "--dry-run"],
    );

    // An empty description on a commit which actually changes the project still blocks push.
    jjosh(&client, &["new"]);
    fs::write(client.join("p/value.txt"), "missing description\n").unwrap();
    jjosh(&client, &["bookmark", "set", "main#p", "-r", "@"]);
    let rejected = jjosh_unchecked(
        &client,
        &["git", "push", "--bookmark", "main#p", "--dry-run"],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("has no description"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    // The same canonical history sent to an ordinary root remote keeps jj's full-history
    // validation. Project scoping must not weaken normal Git push safety checks.
    let root_remote = temp.path().join("root.git");
    git(
        temp.path(),
        &["init", "--bare", root_remote.to_str().unwrap()],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "root-origin",
            root_remote.to_str().unwrap(),
        ],
    );
    jjosh(&client, &["bookmark", "set", "root-main", "-r", "@"]);
    let root_rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "root-origin",
            "--bookmark",
            "root-main",
            "--dry-run",
        ],
    );
    assert!(!root_rejected.status.success());
    assert!(
        String::from_utf8_lossy(&root_rejected.stderr).contains("has no description"),
        "{}",
        String::from_utf8_lossy(&root_rejected.stderr)
    );
}

#[test]
fn project_validation_and_export_collapse_out_of_scope_merge_parents() {
    for colocated in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("project.git");
        git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
        let client = create_client(temp.path(), colocated);
        jjosh(&client, &["project", "add", "p", "--path", "p"]);
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "add",
                "origin",
                remote.to_str().unwrap(),
                "--whole",
                "--project",
                "p",
            ],
        );
        fs::create_dir(client.join("p")).unwrap();
        fs::write(client.join("p/value.txt"), "project\n").unwrap();
        jjosh(&client, &["describe", "-m", "project base"]);
        let base = commit_id(&client, "@");

        // This disjoint branch and the content-neutral merge have no descriptions. Both
        // disappear from the exported project graph, even though the merge has two parents.
        jjosh(&client, &["new", "root()"]);
        fs::write(client.join("unrelated.txt"), "outside\n").unwrap();
        jjosh(&client, &["new", &base, "@"]);
        jjosh(&client, &["bookmark", "set", "main#p"]);
        let before = operation_id(&client);
        jjosh(
            &client,
            &["git", "push", "--bookmark", "main#p", "--dry-run"],
        );
        assert_eq!(operation_id(&client), before);
        assert!(git(&remote, &["for-each-ref"]).is_empty());

        jjosh(&client, &["git", "push", "--bookmark", "main#p"]);
        assert_eq!(
            git(&remote, &["log", "main", "--format=%s"]),
            "project base\n"
        );
        assert_eq!(
            git(&remote, &["ls-tree", "-r", "--name-only", "main"]),
            "value.txt\n"
        );
    }
}

#[test]
fn project_validation_keeps_meaningful_merges_and_canonical_private_checks() {
    let temp = tempfile::tempdir().unwrap();
    let remote = temp.path().join("project.git");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    let client = create_client(temp.path(), false);
    // Tags normally make their targets immutable. Keep this fixture explicitly mutable so
    // it exercises validation rather than the pre-existing immutable-history exemption.
    jjosh(
        &client,
        &[
            "config",
            "set",
            "--repo",
            "revset-aliases.\"immutable_heads()\"",
            "root()",
        ],
    );
    jjosh(&client, &["project", "add", "p", "--path", "p"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            remote.to_str().unwrap(),
            "--whole",
            "--project",
            "p",
        ],
    );
    fs::create_dir(client.join("p")).unwrap();
    fs::write(client.join("p/base"), "base\n").unwrap();
    jjosh(&client, &["describe", "-m", "base"]);
    let base = commit_id(&client, "@");
    jjosh(&client, &["new", "-m", "left"]);
    fs::write(client.join("p/left"), "left\n").unwrap();
    let left = commit_id(&client, "@");
    jjosh(&client, &["new", &base, "-m", "right"]);
    fs::write(client.join("p/right"), "right\n").unwrap();
    jjosh(&client, &["new", &left, "@"]);
    jjosh(&client, &["bookmark", "set", "main#p"]);
    jjosh(&client, &["tag", "set", "release#p"]);

    for selector in [["--bookmark", "main#p"], ["--tag", "release#p"]] {
        let before = operation_id(&client);
        let rejected = jjosh_unchecked(
            &client,
            &["git", "push", selector[0], selector[1], "--dry-run"],
        );
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("has no description"));
        assert_eq!(operation_id(&client), before);
    }
    jjosh(&client, &["describe", "-m", "merge both project branches"]);
    let canonical = commit_id(&client, "@");
    let private = format!("git.private-commits={canonical}");
    let rejected = jjosh_unchecked(
        &client,
        &[
            "--config",
            &private,
            "git",
            "push",
            "--bookmark",
            "main#p",
            "--dry-run",
        ],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("is private"));
    assert!(git(&remote, &["for-each-ref"]).is_empty());
    jjosh(&client, &["git", "push", "--bookmark", "main#p"]);
    assert_eq!(
        git(&remote, &["show", "-s", "--format=%p", "main"])
            .split_whitespace()
            .count(),
        2
    );
}

fn import_project(client: &Path, project: &str, mount: &str, source: &Path, filter: &str) {
    let remote = format!("{project}-upstream");
    jjosh(client, &["project", "add", project, "--path", mount]);
    let args = [
        "git",
        "remote",
        "add",
        &remote,
        source.to_str().unwrap(),
        "--filter",
        filter,
        "--project",
        project,
        "--base",
        "main",
    ];
    jjosh(client, &args);
    jjosh(
        client,
        &[
            "git",
            "fetch",
            "--project",
            project,
            "--remote",
            &remote,
            "--branch",
            "main",
        ],
    );
    jjosh(client, &["new", "@", &format!("main#{project}@{remote}")]);
}

fn physical_remote(client: &Path, project: &str, alias: &str) -> String {
    let state: serde_json::Value =
        serde_json::from_slice(&jjosh(client, &["project", "show", project, "--json"]).stdout)
            .unwrap();
    let projects = state["projects"].as_array().unwrap();
    assert_eq!(
        projects.len(),
        1,
        "expected exactly one project named {project}"
    );
    let identities: Vec<_> = projects[0]["remotes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|remote| {
            remote["resolved"] == true
                && remote["candidates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|candidate| candidate["definition"]["name"] == alias)
        })
        .map(|remote| remote["connection"].as_str().unwrap())
        .collect();
    assert_eq!(
        identities.len(),
        1,
        "expected exactly one resolved alias {alias} in project {project}: {state}"
    );
    let identity = identities[0];
    let git_dir = String::from_utf8(jjosh(client, &["git", "root"]).stdout).unwrap();
    let git_dir = Path::new(git_dir.trim());
    let remotes = git(git_dir, &["remote"]);
    let physical: Vec<_> = remotes
        .lines()
        .filter(|name| {
            let configured = run(
                git_dir,
                Path::new("git"),
                &[
                    "config",
                    "--get",
                    &format!("remote.{name}.jjosh-connectionId"),
                ],
            );
            configured.status.success()
                && String::from_utf8(configured.stdout).unwrap().trim() == identity
        })
        .collect();
    assert_eq!(
        physical.len(),
        1,
        "expected exactly one physical remote for {alias} in project {project}"
    );
    physical[0].to_owned()
}

fn shared_alias_repositories() -> (tempfile::TempDir, PathBuf, Vec<(String, String, PathBuf)>) {
    let temp = tempfile::tempdir().unwrap();
    let client = create_client(temp.path(), false);
    let mut remotes = Vec::new();
    for scope in ["root", "alpha", "beta"] {
        let (_, source, _) = create_remote(temp.path(), scope);
        if scope != "root" {
            jjosh(&client, &["project", "add", scope, "--path", scope]);
        }
        for alias in ["origin", "upstream", "fork"] {
            let remote = temp.path().join(format!("{scope}-{alias}.git"));
            git(
                temp.path(),
                &[
                    "clone",
                    "--bare",
                    source.to_str().unwrap(),
                    remote.to_str().unwrap(),
                ],
            );
            for branch in ["pattern", "listed", "all-scoped"] {
                git(&remote, &["branch", branch, "main"]);
            }
            git(&remote, &["tag", "release", "main"]);
            let mut args = vec!["git", "remote", "add", alias, remote.to_str().unwrap()];
            if scope != "root" {
                args.extend(["--project", scope, "--filter", ":/src", "--base", "main"]);
            }
            jjosh(&client, &args);
            remotes.push((scope.to_owned(), alias.to_owned(), remote));
        }
        let mut args = vec!["git", "fetch", "--branch", "main"];
        if scope != "root" {
            args.extend(["--project", scope]);
        }
        jjosh(&client, &args);
        if scope != "root" {
            jjosh(&client, &["new", "@", &format!("main#{scope}@origin")]);
        }
    }
    (temp, client, remotes)
}

#[test]
fn shared_remote_aliases_isolate_defaults_fetch_patterns_and_reference_tracking() {
    let (_temp, client, _remotes) = shared_alias_repositories();
    for (scope, path, expected) in [
        ("", "src/value.txt", "root-v1\n"),
        ("#alpha", "alpha/value.txt", "alpha-v1\n"),
        ("#beta", "beta/value.txt", "beta-v1\n"),
    ] {
        assert_eq!(
            file_at_revision(&client, &format!("main{scope}@origin"), path),
            expected.as_bytes()
        );
        for alias in ["upstream", "fork"] {
            assert!(
                !jjosh_unchecked(&client, &["log", "-r", &format!("main{scope}@{alias}")])
                    .status
                    .success()
            );
        }
    }
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
    let actual: std::collections::BTreeSet<_> = names.lines().collect();
    assert_eq!(
        actual,
        ["main@origin", "main#alpha@origin", "main#beta@origin"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        jjosh(
            &client,
            &[
                "bookmark",
                "list",
                "--remote",
                "origin#alpha",
                "-T",
                r#"name ++ "@" ++ remote ++ "\n""#
            ]
        )
        .stdout,
        b"main#alpha@origin\n"
    );
    assert!(
        !jjosh_unchecked(&client, &["bookmark", "track", "main@origin#alpha"])
            .status
            .success()
    );
    let physical = physical_remote(&client, "alpha", "origin");
    assert_eq!(physical, "origin#alpha");
    jjosh(
        &client,
        &["git", "fetch", "--remote", &physical, "--branch", "main"],
    );
    jjosh(&client, &["bookmark", "track", "main#alpha@origin"]);
    assert_eq!(
        commit_id(
            &client,
            "tracked_remote_bookmarks(exact:main#alpha, exact:origin)"
        ),
        commit_id(&client, "main#alpha@origin")
    );
    assert_eq!(
        commit_id(
            &client,
            "tracked_remote_bookmarks(exact:main#beta, exact:origin)"
        ),
        ""
    );
    assert_eq!(
        commit_id(
            &client,
            "tracked_remote_bookmarks(exact:main, exact:origin)"
        ),
        ""
    );
    jjosh(&client, &["bookmark", "untrack", "main#alpha@origin"]);
    assert_eq!(
        commit_id(
            &client,
            "tracked_remote_bookmarks(exact:main#alpha, exact:origin)"
        ),
        ""
    );
    assert_eq!(
        file_at_revision(&client, "main#beta@origin", "beta/value.txt"),
        b"beta-v1\n"
    );

    // Explicit patterns and both configured native syntaxes stay inside the selected scope.
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "alpha",
            "--remote",
            "glob:up*",
            "--branch",
            "pattern",
        ],
    );
    jjosh(
        &client,
        &[
            "--config",
            "git.fetch=missing-root",
            "--config",
            "git.projects.alpha.fetch=[\"origin\",\"fork\"]",
            "git",
            "fetch",
            "--project",
            "alpha",
            "--branch",
            "listed",
        ],
    );
    jjosh(
        &client,
        &[
            "--config",
            "git.projects.beta.fetch=glob:*",
            "git",
            "fetch",
            "--project",
            "beta",
            "--branch",
            "pattern",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "alpha",
            "--all-remotes",
            "--branch",
            "all-scoped",
            "--tag",
            "release",
        ],
    );
    for alias in ["origin", "upstream", "fork"] {
        assert_eq!(
            file_at_revision(&client, &format!("pattern#beta@{alias}"), "beta/value.txt"),
            b"beta-v1\n"
        );
        assert_eq!(
            file_at_revision(
                &client,
                &format!("all-scoped#alpha@{alias}"),
                "alpha/value.txt"
            ),
            b"alpha-v1\n"
        );
        assert_eq!(
            file_at_revision(
                &client,
                &format!("release#alpha@{alias}"),
                "alpha/value.txt"
            ),
            b"alpha-v1\n"
        );
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("all-scoped#beta@{alias}")])
                .status
                .success()
        );
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("pattern@{alias}")])
                .status
                .success()
        );
    }
    assert_eq!(
        file_at_revision(&client, "pattern#alpha@upstream", "alpha/value.txt"),
        b"alpha-v1\n"
    );
    for alias in ["origin", "fork"] {
        assert_eq!(
            file_at_revision(&client, &format!("listed#alpha@{alias}"), "alpha/value.txt"),
            b"alpha-v1\n"
        );
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("pattern#alpha@{alias}")])
                .status
                .success()
        );
    }
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "listed#alpha@upstream"])
            .status
            .success()
    );
    assert_eq!(
        jjosh(
            &client,
            &[
                "tag",
                "list",
                "--remote",
                "origin#alpha",
                "-T",
                r#"if(remote, name ++ "@" ++ remote ++ "\n")"#
            ]
        )
        .stdout,
        b"release#alpha@origin\n"
    );
    let not_origin = String::from_utf8(
        jjosh(
            &client,
            &[
                "bookmark",
                "list",
                "--remote",
                "~origin",
                "-T",
                r#"if(remote && remote != "git", remote ++ "\n")"#,
            ],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(
        not_origin
            .lines()
            .collect::<std::collections::BTreeSet<_>>(),
        ["fork", "upstream"].into_iter().collect()
    );

    // Root patterns exclude all project connections even when aliases are identical.
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "glob:*",
            "--branch",
            "all-scoped",
        ],
    );
    for alias in ["origin", "upstream", "fork"] {
        assert_eq!(
            file_at_revision(&client, &format!("all-scoped@{alias}"), "src/value.txt"),
            b"root-v1\n"
        );
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("all-scoped#beta@{alias}")])
                .status
                .success()
        );
    }
    let refs = git(&client.join(".jj/repo/store/git"), &["for-each-ref"]);
    let operation = operation_id(&client);
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "fetch",
                "--project",
                "beta",
                "--remote",
                "origin#alpha"
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        git(&client.join(".jj/repo/store/git"), &["for-each-ref"]),
        refs
    );
    assert_eq!(operation_id(&client), operation);
}

#[test]
fn initial_multi_project_whole_fetch_loads_objects_published_by_previous_transfers() {
    let temp = tempfile::tempdir().unwrap();
    let client = create_client(temp.path(), false);
    for project in ["alpha", "beta"] {
        let (_, remote, _) = create_remote(temp.path(), project);
        jjosh(&client, &["project", "add", project, "--path", project]);
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "add",
                "origin",
                remote.to_str().unwrap(),
                "--project",
                project,
                "--whole",
            ],
        );
    }
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "alpha",
            "--project",
            "beta",
            "--branch",
            "main",
        ],
    );
    for project in ["alpha", "beta"] {
        assert_eq!(
            file_at_revision(
                &client,
                &format!("main#{project}@origin"),
                &format!("{project}/src/value.txt")
            ),
            format!("{project}-v1\n").as_bytes(),
        );
        assert_eq!(
            file_at_revision(
                &client,
                &format!("main#{project}@origin"),
                &format!("{project}/outside.txt")
            ),
            format!("{project}-outside\n").as_bytes(),
        );
    }
}

#[test]
fn shared_remote_aliases_fetch_multiple_projects_without_scope_leakage_or_partial_updates() {
    let (temp, client, remotes) = shared_alias_repositories();
    jjosh(
        &client,
        &[
            "--config",
            "git.fetch=missing-root",
            "--config",
            "git.projects.alpha.fetch=upstream",
            "--config",
            "git.projects.beta.fetch=fork",
            "git",
            "fetch",
            "--all-projects",
            "--branch",
            "listed",
        ],
    );
    for (project, selected) in [("alpha", "upstream"), ("beta", "fork")] {
        for alias in ["origin", "upstream", "fork"] {
            let revision = format!("listed#{project}@{alias}");
            if alias == selected {
                assert_eq!(
                    file_at_revision(&client, &revision, &format!("{project}/value.txt")),
                    format!("{project}-v1\n").as_bytes(),
                );
            } else {
                assert!(
                    !jjosh_unchecked(&client, &["log", "-r", &revision])
                        .status
                        .success()
                );
            }
        }
    }
    for alias in ["origin", "upstream", "fork"] {
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("listed@{alias}")])
                .status
                .success()
        );
    }

    // Repeating one selector still selects only that project, not its sibling or root.
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "alpha",
            "--project",
            "alpha",
            "--remote",
            "origin",
            "--branch",
            "pattern",
        ],
    );
    assert_eq!(
        file_at_revision(&client, "pattern#alpha@origin", "alpha/value.txt"),
        b"alpha-v1\n"
    );
    for scope in ["", "#beta"] {
        for alias in ["origin", "upstream", "fork"] {
            assert!(
                !jjosh_unchecked(&client, &["log", "-r", &format!("pattern{scope}@{alias}")])
                    .status
                    .success()
            );
        }
    }
    for alias in ["upstream", "fork"] {
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("pattern#alpha@{alias}")])
                .status
                .success()
        );
    }

    // The same explicit alias resolves separately in each selected project.
    jjosh(
        &client,
        &[
            "--config",
            "git.projects.alpha.fetch=missing-alpha",
            "--config",
            "git.projects.beta.fetch=missing-beta",
            "git",
            "fetch",
            "--project",
            "alpha",
            "--project",
            "beta",
            "--project",
            "alpha",
            "--remote",
            "origin",
            "--branch",
            "listed",
        ],
    );
    for project in ["alpha", "beta"] {
        assert_eq!(
            file_at_revision(
                &client,
                &format!("listed#{project}@origin"),
                &format!("{project}/value.txt")
            ),
            format!("{project}-v1\n").as_bytes(),
        );
    }
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "listed@origin"])
            .status
            .success()
    );

    jjosh(
        &client,
        &[
            "--config",
            "git.projects.alpha.fetch=missing-alpha",
            "--config",
            "git.projects.beta.fetch=missing-beta",
            "git",
            "fetch",
            "--all-projects",
            "--all-remotes",
            "--branch",
            "all-scoped",
        ],
    );
    for alias in ["origin", "upstream", "fork"] {
        for project in ["alpha", "beta"] {
            assert_eq!(
                file_at_revision(
                    &client,
                    &format!("all-scoped#{project}@{alias}"),
                    &format!("{project}/value.txt")
                ),
                format!("{project}-v1\n").as_bytes(),
            );
        }
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &format!("all-scoped@{alias}")])
                .status
                .success()
        );
    }

    // A later project's missing alias must prevent even raw Git refs changing in
    // the earlier project, although the alias still exists in root and alpha.
    jjosh(
        &client,
        &["git", "remote", "remove", "upstream", "--project", "beta"],
    );
    let alpha_work = temp.path().join("alpha-work");
    let (_, _, alpha_upstream) = remotes
        .iter()
        .find(|(project, alias, _)| project == "alpha" && alias == "upstream")
        .unwrap();
    fs::write(alpha_work.join("src/value.txt"), "alpha-v2\n").unwrap();
    git(&alpha_work, &["commit", "-am", "advance alpha upstream"]);
    git(
        &alpha_work,
        &["push", alpha_upstream.to_str().unwrap(), "HEAD:listed"],
    );
    let previous = commit_id(&client, "listed#alpha@upstream");
    let git_dir = client.join(".jj/repo/store/git");
    let refs = git(&git_dir, &["for-each-ref"]);
    let operation = operation_id(&client);
    for selection in [
        vec!["--all-projects"],
        vec!["--project", "alpha", "--project", "beta"],
    ] {
        let mut args = vec!["git", "fetch", "--remote", "upstream", "--branch", "listed"];
        args.extend(selection);
        assert!(!jjosh_unchecked(&client, &args).status.success());
        assert_eq!(git(&git_dir, &["for-each-ref"]), refs);
        assert_eq!(operation_id(&client), operation);
        assert_eq!(commit_id(&client, "listed#alpha@upstream"), previous);
    }
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "alpha",
            "--remote",
            "upstream",
            "--branch",
            "listed",
        ],
    );
    assert_eq!(
        file_at_revision(&client, "listed#alpha@upstream", "alpha/value.txt"),
        b"alpha-v2\n"
    );
    assert_ne!(commit_id(&client, "listed#alpha@upstream"), previous);
}

#[test]
fn shared_remote_aliases_route_default_and_explicit_wildcard_pushes_without_partial_publication() {
    let (_temp, client, remotes) = shared_alias_repositories();
    fs::write(client.join("alpha/value.txt"), "alpha published\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta published\n").unwrap();
    fs::write(client.join("root.txt"), "root published\n").unwrap();
    jjosh(&client, &["describe", "-m", "publish each namespace"]);
    // The same zero-configuration origin fallback works in both directions.
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--named",
            "default=@",
            "--named",
            "default#alpha=@",
            "--named",
            "default#beta=@",
            "--allow-empty-description",
        ],
    );
    for (scope, alias, remote) in &remotes {
        if alias == "origin" {
            let path = if scope == "root" {
                "root.txt"
            } else {
                "src/value.txt"
            };
            assert_eq!(
                git(remote, &["show", &format!("default:{path}")]),
                format!("{scope} published\n")
            );
        } else {
            assert_eq!(git(remote, &["for-each-ref", "refs/heads/default"]), "");
        }
    }
    jjosh(
        &client,
        &["bookmark", "create", "review#alpha", "review#beta"],
    );
    // Root origin/fork never rescue the selected project's missing fork.
    jjosh(
        &client,
        &["git", "remote", "remove", "fork", "--project", "beta"],
    );
    let before: Vec<_> = remotes
        .iter()
        .map(|(_, _, remote)| git(remote, &["show-ref"]))
        .collect();
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &[
            "--config",
            "git.push=origin",
            "git",
            "push",
            "--remote",
            "fork",
            "--bookmark",
            "review#*",
            "--allow-empty-description",
        ],
    );
    assert!(!rejected.status.success());
    for ((_, _, remote), refs) in remotes.iter().zip(&before) {
        assert_eq!(&git(remote, &["show-ref"]), refs);
    }
    assert_eq!(operation_id(&client), operation);
    let beta_fork = &remotes
        .iter()
        .find(|(scope, alias, _)| scope == "beta" && alias == "fork")
        .unwrap()
        .2;
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "fork",
            beta_fork.to_str().unwrap(),
            "--project",
            "beta",
            "--filter",
            ":/src",
            "--base",
            "main",
        ],
    );
    // Each filtered publication destination needs its own immutable base observation.
    for project in ["alpha", "beta"] {
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--project",
                project,
                "--remote",
                "fork",
                "--remote",
                "upstream",
                "--branch",
                "main",
            ],
        );
    }
    jjosh(
        &client,
        &[
            "--config",
            "git.push=missing-root",
            "git",
            "push",
            "--remote",
            "fork",
            "--bookmark",
            "review#*",
            "--allow-empty-description",
        ],
    );
    for (scope, alias, remote) in &remotes {
        if scope != "root" && alias == "fork" {
            assert_eq!(
                git(remote, &["show", "review:src/value.txt"]),
                format!("{scope} published\n")
            );
            assert_eq!(
                git(remote, &["show", "review:outside.txt"]),
                format!("{scope}-outside\n")
            );
            assert_eq!(
                git(remote, &["ls-tree", "-r", "--name-only", "review"]),
                "outside.txt\nsrc/value.txt\n"
            );
        } else {
            assert_eq!(git(remote, &["for-each-ref", "refs/heads/review"]), "");
        }
    }
    // Configured lists and patterns are multi-destination routes, not singleton aliases.
    jjosh(
        &client,
        &[
            "--config",
            "git.push=missing-root",
            "--config",
            "git.projects.alpha.push=[\"upstream\",\"fork\"]",
            "--config",
            "git.projects.beta.push=glob:*",
            "git",
            "push",
            "--named",
            "multiple#alpha=@",
            "--named",
            "multiple#beta=@",
            "--allow-empty-description",
        ],
    );
    for (scope, alias, remote) in &remotes {
        if scope == "beta" || (scope == "alpha" && alias != "origin") {
            assert_eq!(
                git(remote, &["show", "multiple:src/value.txt"]),
                format!("{scope} published\n")
            );
        } else {
            assert_eq!(git(remote, &["for-each-ref", "refs/heads/multiple"]), "");
        }
    }
}

#[test]
fn shared_alias_rename_updates_only_local_defaults_and_restores_without_changing_connection() {
    let (_temp, client, _remotes) = shared_alias_repositories();
    for scope in ["", ".projects.alpha", ".projects.beta"] {
        for direction in ["fetch", "push"] {
            jjosh(
                &client,
                &[
                    "config",
                    "set",
                    "--repo",
                    &format!("git{scope}.{direction}"),
                    "origin",
                ],
            );
        }
    }
    jjosh(&client, &["bookmark", "track", "main#alpha@origin"]);
    let physical = physical_remote(&client, "alpha", "origin");
    let git_dir = client.join(".jj/repo/store/git");
    let connection = git(
        &git_dir,
        &[
            "config",
            "--get",
            &format!("remote.{physical}.jjosh-connectionId"),
        ],
    );
    let url = git(
        &git_dir,
        &["config", "--get", &format!("remote.{physical}.url")],
    );
    let canonical = commit_id(&client, "main#alpha@origin");
    let before = operation_id(&client);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "rename",
            "origin",
            "primary",
            "--project",
            "alpha",
        ],
    );
    let renamed = physical_remote(&client, "alpha", "primary");
    assert_eq!(physical, "origin#alpha");
    assert_eq!(renamed, "primary#alpha");
    assert_eq!(
        git(
            &git_dir,
            &[
                "config",
                "--get",
                &format!("remote.{renamed}.jjosh-connectionId")
            ]
        ),
        connection
    );
    assert_eq!(
        git(
            &git_dir,
            &["config", "--get", &format!("remote.{renamed}.url")]
        ),
        url
    );
    assert!(
        !run(
            &git_dir,
            Path::new("git"),
            &["config", "--get", &format!("remote.{physical}.url")]
        )
        .status
        .success()
    );
    assert_eq!(
        commit_id(
            &client,
            "tracked_remote_bookmarks(exact:main#alpha, exact:primary)"
        ),
        canonical
    );
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "main#alpha@origin"])
            .status
            .success()
    );
    for direction in ["fetch", "push"] {
        for (scope, expected) in [
            ("", "origin\n"),
            (".projects.alpha", "primary\n"),
            (".projects.beta", "origin\n"),
        ] {
            assert_eq!(
                jjosh(
                    &client,
                    &["config", "get", &format!("git{scope}.{direction}")]
                )
                .stdout,
                expected.as_bytes()
            );
        }
    }
    let names = |project: Option<&str>| {
        let mut args = vec!["git", "remote", "list"];
        if let Some(project) = project {
            args.extend(["--project", project]);
        }
        String::from_utf8(jjosh(&client, &args).stdout)
            .unwrap()
            .lines()
            .map(|line| line.split_whitespace().next().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        names(Some("alpha")),
        ["fork", "primary", "upstream"]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );
    assert_eq!(
        names(None),
        [
            "fork",
            "origin",
            "upstream",
            "fork#alpha",
            "primary#alpha",
            "upstream#alpha",
            "fork#beta",
            "origin#beta",
            "upstream#beta",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect()
    );
    jjosh(
        &client,
        &["git", "fetch", "--project", "alpha", "--branch", "listed"],
    );
    jjosh(
        &client,
        &["git", "fetch", "--project", "beta", "--branch", "listed"],
    );
    jjosh(&client, &["git", "fetch", "--branch", "listed"]);
    assert_eq!(
        file_at_revision(&client, "listed#alpha@primary", "alpha/value.txt"),
        b"alpha-v1\n"
    );
    assert_eq!(
        file_at_revision(&client, "listed#beta@origin", "beta/value.txt"),
        b"beta-v1\n"
    );
    assert_eq!(
        file_at_revision(&client, "listed@origin", "src/value.txt"),
        b"root-v1\n"
    );
    jjosh(
        &client,
        &["op", "restore", before.trim(), "--what", "remote-tracking"],
    );
    assert_eq!(
        physical_remote(&client, "alpha", "primary"),
        "primary#alpha"
    );
    assert_eq!(commit_id(&client, "main#alpha@primary"), canonical);
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "main#alpha@origin"])
            .status
            .success()
    );
    assert_eq!(
        file_at_revision(&client, "main#beta@origin", "beta/value.txt"),
        b"beta-v1\n"
    );
    assert_eq!(
        file_at_revision(&client, "main@origin", "src/value.txt"),
        b"root-v1\n"
    );
    jjosh(&client, &["op", "restore", before.trim(), "--what", "repo"]);
    // Repo-only restore rewinds logical metadata, not external Git config.
    // The connection is therefore displayed again as origin, while its current
    // physical Git remote remains the readable name created by the rename.
    assert_eq!(physical_remote(&client, "alpha", "origin"), "primary#alpha");
    assert_eq!(commit_id(&client, "main#alpha@origin"), canonical);
}

fn file_at_revision(client: &Path, revision: &str, path: &str) -> Vec<u8> {
    jjosh(client, &["file", "show", "-r", revision, path]).stdout
}

#[test]
fn pure_project_changes_support_unpublished_and_concurrent_operations_without_snapshotting() {
    let temp = tempfile::tempdir().unwrap();
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "api", "--path", "api"]);
    fs::write(client.join("dirty.txt"), "unrecorded user data\n").unwrap();
    let base = operation_id(&client);
    let output = jjosh(
        &client,
        &[
            "--no-integrate-operation",
            "project",
            "rename",
            "api",
            "draft",
        ],
    );
    assert_eq!(operation_id(&client), base);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let draft = stderr
        .split_whitespace()
        .filter(|word| word.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .find(|operation| {
            jjosh_unchecked(
                &client,
                &["--at-op", operation, "project", "show", "draft", "--json"],
            )
            .status
            .success()
        })
        .expect("the reported unpublished operation must be loadable");
    let draft_state: serde_json::Value = serde_json::from_slice(
        &jjosh(
            &client,
            &["--at-op", draft, "project", "show", "draft", "--json"],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(
        draft_state["projects"][0]["candidates"][0]["definition"]["name"],
        "draft"
    );
    for name in ["left", "right"] {
        jjosh(
            &client,
            &["--at-op", base.trim(), "project", "rename", "api", name],
        );
    }
    let merged: serde_json::Value =
        serde_json::from_slice(&jjosh(&client, &["project", "list", "--json"]).stdout).unwrap();
    let names: std::collections::BTreeSet<_> = merged["projects"][0]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|candidate| candidate["definition"]["name"].as_str())
        .collect();
    assert_eq!(names, std::collections::BTreeSet::from(["left", "right"]));
    assert_eq!(
        fs::read(client.join("dirty.txt")).unwrap(),
        b"unrecorded user data\n"
    );
    let recorded =
        String::from_utf8(jjosh(&client, &["--ignore-working-copy", "file", "list"]).stdout)
            .unwrap();
    assert!(!recorded.lines().any(|path| path == "dirty.txt"));
}

#[test]
fn conflicted_project_metadata_can_be_restored_and_repaired_by_repo_only_restore() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "api", &source, ":/src");
    let healthy = operation_id(&client);
    let canonical = commit_id(&client, "main#api@api-upstream");
    jjosh(
        &client,
        &[
            "--at-op",
            healthy.trim(),
            "project",
            "rename",
            "api",
            "left",
        ],
    );
    jjosh(
        &client,
        &[
            "--at-op",
            healthy.trim(),
            "project",
            "rename",
            "api",
            "right",
        ],
    );
    // Loading the current repository merges both operation heads and retains
    // their signed project-definition conflict.
    let conflict: serde_json::Value =
        serde_json::from_slice(&jjosh(&client, &["project", "list", "--json"]).stdout).unwrap();
    assert!(!conflict["diagnostics"].as_array().unwrap().is_empty());
    let unresolved = jjosh_unchecked(&client, &["log", "-r", "main#api@api-upstream"]);
    assert!(!unresolved.status.success());
    assert!(
        String::from_utf8_lossy(&unresolved.stderr).contains("unavailable or unresolved"),
        "{}",
        String::from_utf8_lossy(&unresolved.stderr)
    );
    let conflicted = operation_id(&client);
    jjosh(
        &client,
        &["op", "restore", healthy.trim(), "--what", "repo"],
    );
    jjosh(&client, &["project", "check", "api"]);
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);

    jjosh(&client, &["op", "restore", conflicted.trim()]);
    let restored: serde_json::Value =
        serde_json::from_slice(&jjosh(&client, &["project", "list", "--json"]).stdout).unwrap();
    assert_eq!(restored["projects"], conflict["projects"]);
    assert!(!restored["diagnostics"].as_array().unwrap().is_empty());
    jjosh(
        &client,
        &["op", "restore", healthy.trim(), "--what", "repo"],
    );
    jjosh(&client, &["project", "check", "api"]);
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
}

#[test]
fn repo_only_rewind_keeps_retired_remote_history_only_in_its_operation() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, tip) = create_remote(temp.path(), "source");
    git(&source, &["update-ref", "refs/tags/v1", &tip]);
    let client = create_client(temp.path(), false);
    let before_project = operation_id(&client);
    import_project(&client, "api", "api", &source, ":/src");
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "api-upstream#api",
            "--tag",
            "v1",
        ],
    );
    let canonical = commit_id(&client, "main#api@api-upstream");
    let tag = commit_id(&client, "v1#api@api-upstream");
    let historical = operation_id(&client);
    let git_dir = client.join(".jj/repo/store/git");
    let config = fs::read(git_dir.join("config")).unwrap();
    jjosh(
        &client,
        &["op", "restore", before_project.trim(), "--what", "repo"],
    );
    let state: serde_json::Value =
        serde_json::from_slice(&jjosh(&client, &["project", "list", "--json"]).stdout).unwrap();
    assert!(state["projects"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), config);
    for (kind, name, expected) in [
        ("bookmark", "main#api", canonical.as_str()),
        ("tag", "v1#api", tag.as_str()),
    ] {
        let names = jjosh(
            &client,
            &[
                kind,
                "list",
                "--all-remotes",
                "-T",
                r#"if(remote && remote != "git", name ++ "@" ++ remote ++ "\n")"#,
            ],
        );
        assert!(
            names.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&names.stdout)
        );
        let symbol = format!("{name}@api-upstream");
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", &symbol])
                .status
                .success()
        );
        let old = jjosh(
            &client,
            &[
                "--at-op",
                historical.trim(),
                "log",
                "--no-graph",
                "-r",
                &symbol,
                "-T",
                "commit_id",
            ],
        );
        assert_eq!(String::from_utf8(old.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn forgetting_converted_remote_bookmarks_preserves_history_and_publication_leases() {
    for include_remotes in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (work, source, original) = create_remote(temp.path(), "source");
        let client = create_client(temp.path(), false);
        import_project(&client, "api", "api", &source, ":/src");
        jjosh(&client, &["bookmark", "track", "main#api@api-upstream"]);
        let canonical = commit_id(&client, "main#api@api-upstream");
        let observed = operation_id(&client);
        if include_remotes {
            jjosh(
                &client,
                &["bookmark", "forget", "main#api", "--include-remotes"],
            );
        } else {
            jjosh(&client, &["bookmark", "forget", "main#api@api-upstream"]);
            assert_eq!(commit_id(&client, "main#api"), canonical);
        }
        jjosh(&client, &["project", "check", "api"]);
        assert!(
            !jjosh_unchecked(&client, &["log", "-r", "main#api@api-upstream"])
                .status
                .success()
        );
        let historical = String::from_utf8(
            jjosh(
                &client,
                &[
                    "--at-op",
                    observed.trim(),
                    "log",
                    "--no-graph",
                    "-r",
                    "main#api@api-upstream",
                    "-T",
                    "commit_id",
                ],
            )
            .stdout,
        )
        .unwrap();
        assert_eq!(historical.trim(), canonical);

        jjosh(
            &client,
            &["new", &canonical, "-m", "publish after forgetting"],
        );
        fs::write(client.join("api/value.txt"), "republished\n").unwrap();
        jjosh(&client, &["bookmark", "set", "main#api"]);
        // A forget must not reset the independently remembered raw endpoint.
        fs::write(work.join("src/value.txt"), "external advance\n").unwrap();
        git(&work, &["commit", "-am", "external advance"]);
        git(&work, &["push", source.to_str().unwrap(), "HEAD:main"]);
        let advanced = git(&source, &["rev-parse", "main"]);
        let push = [
            "git",
            "push",
            "--remote",
            "api-upstream",
            "--bookmark",
            "main#api",
            "--base",
            original.as_str(),
        ];
        let missing_source = jjosh_unchecked(&client, &push);
        assert!(!missing_source.status.success());
        assert!(
            String::from_utf8_lossy(&missing_source.stderr)
                .contains("no immutable conversion observation"),
            "{}",
            String::from_utf8_lossy(&missing_source.stderr)
        );
        // Reacquire immutable source evidence without fetching the destination
        // branch. This must not refresh its independently retained publication
        // lease, which still names `original`.
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--project",
                "api",
                "--remote",
                "api-upstream",
                "--revision",
                &original,
            ],
        );
        let stale = jjosh_unchecked(&client, &push);
        assert!(!stale.status.success());
        assert!(
            String::from_utf8_lossy(&stale.stderr).contains("stale lease"),
            "{}",
            String::from_utf8_lossy(&stale.stderr)
        );
        assert_eq!(git(&source, &["rev-parse", "main"]), advanced);
        git(
            &source,
            &["update-ref", "refs/heads/main", &original, advanced.trim()],
        );
        // Once the destination again matches the retained lease, rebuild the
        // current tracking/mirror view before publishing. The stale-lease check
        // above already proved that forgetting did not reset publication
        // authorization; this fetch restores current conversion/tracking state.
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--project",
                "api",
                "--remote",
                "api-upstream",
                "--branch",
                "main",
            ],
        );
        jjosh(&client, &["bookmark", "track", "main#api@api-upstream"]);
        jjosh(&client, &push);
        assert_eq!(
            git(&source, &["show", "main:src/value.txt"]),
            "republished\n"
        );
        assert_eq!(
            git(&source, &["show", "main:outside.txt"]),
            "source-outside\n"
        );
    }
}

#[test]
fn project_registration_restores_without_snapshotting_or_claiming_literal_refs() {
    let temp = tempfile::tempdir().unwrap();
    let client = create_client(temp.path(), false);
    jjosh(&client, &["bookmark", "create", "topic#api"]);
    let before = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &["project", "add", "api", "--path", "packages/api"],
    );
    assert!(!rejected.status.success());
    assert_eq!(operation_id(&client), before);
    jjosh(&client, &["bookmark", "delete", "topic#api"]);
    let original_commit = commit_id(&client, "@");
    let before_registration = operation_id(&client);
    fs::write(client.join("unrecorded.txt"), "pending local work\n").unwrap();
    jjosh(
        &client,
        &["project", "add", "api", "--path", "packages/api"],
    );
    assert_ne!(operation_id(&client), before_registration);
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &[
                    "--ignore-working-copy",
                    "log",
                    "-r",
                    "@",
                    "--no-graph",
                    "-T",
                    "commit_id"
                ]
            )
            .stdout
        )
        .unwrap()
        .trim(),
        original_commit,
    );
    jjosh(&client, &["project", "show", "api"]);
    jjosh(
        &client,
        &[
            "--ignore-working-copy",
            "op",
            "restore",
            before_registration.trim(),
            "--what",
            "repo",
        ],
    );
    assert!(
        !jjosh_unchecked(&client, &["project", "show", "api"])
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(client.join("unrecorded.txt")).unwrap(),
        "pending local work\n"
    );
}

#[test]
fn raw_fetch_cannot_claim_registered_project_labels() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    git(
        &source,
        &["update-ref", "refs/heads/topic#api", "refs/heads/main"],
    );
    git(
        &source,
        &["update-ref", "refs/tags/v1#api", "refs/heads/main"],
    );
    git(
        &source,
        &["update-ref", "refs/heads/topic#literal", "refs/heads/main"],
    );
    let client = create_client(temp.path(), false);
    jjosh(
        &client,
        &["project", "add", "api", "--path", "packages/api"],
    );
    jjosh(
        &client,
        &["git", "remote", "add", "raw", source.to_str().unwrap()],
    );
    // A repository-wide filtered view also lacks this project's mapping.
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "view",
            source.to_str().unwrap(),
            "--filter",
            ":/src",
        ],
    );
    let git_dir = client.join(".jj/repo/store/git");
    for remote in ["raw", "view"] {
        let refs = git(&git_dir, &["for-each-ref"]);
        let operation = operation_id(&client);
        for selection in [
            vec!["--branch", "main", "--branch", "topic#api"],
            vec!["--tag", "v1#api"],
        ] {
            let mut args = vec!["git", "fetch", "--remote", remote];
            args.extend(selection);
            let result = jjosh_unchecked(&client, &args);
            assert!(
                !result.status.success(),
                "raw input claimed a project label"
            );
            assert_eq!(git(&git_dir, &["for-each-ref"]), refs);
            assert_eq!(operation_id(&client), operation);
        }
    }
    // Unselected collisions do not block ordinary names; unregistered suffixes
    // remain literal on both the wire and the local observation.
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "raw",
            "--branch",
            "topic#literal",
        ],
    );
    assert_eq!(
        file_at_revision(&client, "topic#literal@raw", "outside.txt"),
        b"source-outside\n"
    );
}

#[test]
fn project_defaults_survive_display_and_remote_renames_and_clear_on_removal() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "packages/api", &source, ":/src");
    import_project(&client, "beta", "beta", &beta, ":/src");
    let physical = physical_remote(&client, "api", "api-upstream");
    let git_dir = client.join(".jj/repo/store/git");
    let connection = git(
        &git_dir,
        &[
            "config",
            "--get",
            &format!("remote.{physical}.jjosh-connectionId"),
        ],
    );
    let bindings: serde_json::Value =
        serde_json::from_slice(&jjosh(&client, &["project", "show", "api", "--json"]).stdout)
            .unwrap();
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "peer",
            source.to_str().unwrap(),
            "--like",
            "api-upstream#api",
        ],
    );
    for (label, remote) in [("api", "api-upstream"), ("beta", "beta-upstream")] {
        for direction in ["fetch", "push"] {
            jjosh(
                &client,
                &[
                    "config",
                    "set",
                    "--repo",
                    &format!("git.projects.{label}.{direction}"),
                    remote,
                ],
            );
        }
    }
    jjosh(&client, &["project", "rename", "api", "service"]);
    jjosh(&client, &["project", "show", "service"]);
    assert!(
        !jjosh_unchecked(&client, &["project", "show", "api"])
            .status
            .success()
    );
    git(&source, &["branch", "incoming", "main"]);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "service",
            "--branch",
            "incoming",
        ],
    );
    assert_eq!(
        file_at_revision(
            &client,
            "incoming#api@api-upstream",
            "packages/api/value.txt"
        ),
        b"source-v1\n"
    );
    fs::write(
        client.join("packages/api/value.txt"),
        "renamed project edit\n",
    )
    .unwrap();
    jjosh(&client, &["describe", "-m", "edit renamed project"]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--named",
            "topic#api=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&source, &["show", "topic:src/value.txt"]),
        "renamed project edit\n"
    );
    assert_eq!(
        git(&source, &["show", "topic:outside.txt"]),
        "source-outside\n"
    );
    jjosh(
        &client,
        &["git", "remote", "rename", "api-upstream#api", "primary"],
    );
    let renamed_physical = physical_remote(&client, "service", "primary");
    assert_eq!(renamed_physical, "primary#api");
    assert_ne!(renamed_physical, physical);
    assert_eq!(
        git(
            &git_dir,
            &[
                "config",
                "--get",
                &format!("remote.{renamed_physical}.jjosh-connectionId")
            ]
        ),
        connection,
    );
    let renamed: serde_json::Value =
        serde_json::from_slice(&jjosh(&client, &["project", "show", "service", "--json"]).stdout)
            .unwrap();
    let binding_definitions = |state: &serde_json::Value| {
        state["projects"][0]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|binding| {
                (
                    binding["id"].clone(),
                    binding["candidates"][0]["definition"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    for binding in binding_definitions(&bindings) {
        assert!(binding_definitions(&renamed).contains(&binding));
    }
    assert_eq!(
        commit_id(&client, "topic#api@primary"),
        commit_id(&client, "topic#api")
    );
    for direction in ["fetch", "push"] {
        assert_eq!(
            jjosh(
                &client,
                &["config", "get", &format!("git.projects.api.{direction}")]
            )
            .stdout,
            b"primary\n"
        );
        assert_eq!(
            jjosh(
                &client,
                &["config", "get", &format!("git.projects.beta.{direction}")]
            )
            .stdout,
            b"beta-upstream\n"
        );
    }
    git(&source, &["branch", "after-rename", "main"]);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "service",
            "--branch",
            "after-rename",
        ],
    );
    assert_eq!(
        file_at_revision(
            &client,
            "after-rename#api@primary",
            "packages/api/value.txt"
        ),
        b"source-v1\n"
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--named",
            "renamed#api=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&source, &["show", "renamed:src/value.txt"]),
        "renamed project edit\n"
    );
    jjosh(&client, &["git", "remote", "remove", "primary#api"]);
    for direction in ["fetch", "push"] {
        assert!(
            !jjosh_unchecked(
                &client,
                &["config", "get", &format!("git.projects.api.{direction}")]
            )
            .status
            .success()
        );
        assert_eq!(
            jjosh(
                &client,
                &["config", "get", &format!("git.projects.beta.{direction}")]
            )
            .stdout,
            b"beta-upstream\n"
        );
    }
    // Removing the default leaves a usable sole peer, not a dangling pointer.
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--project",
            "service",
            "--branch",
            "incoming",
        ],
    );
    assert_eq!(
        file_at_revision(&client, "incoming#api@peer", "packages/api/value.txt"),
        b"source-v1\n"
    );
    git(&beta, &["branch", "untouched", "main"]);
    jjosh(
        &client,
        &["git", "fetch", "--project", "beta", "--branch", "untouched"],
    );
    assert_eq!(
        file_at_revision(&client, "untouched#beta@beta-upstream", "beta/value.txt"),
        b"beta-v1\n"
    );
}

#[test]
fn project_fetch_defaults_are_directional_and_validate_selected_bindings_before_transfer() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let (_, mirror, _) = create_remote(temp.path(), "mirror");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "api", &source, ":/src");
    import_project(&client, "beta", "beta", &beta, ":/src");
    jjosh(
        &client,
        &["git", "remote", "rename", "beta-upstream#beta", "origin"],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "mirror",
            mirror.to_str().unwrap(),
            "--like",
            "api-upstream#api",
        ],
    );
    for remote in [&source, &mirror, &beta] {
        for branch in [
            "incoming",
            "explicit",
            "global",
            "forbidden",
            "fallback",
            "ordinary",
        ] {
            git(remote, &["branch", branch, "main"]);
        }
    }
    jjosh(
        &client,
        &[
            "config",
            "set",
            "--repo",
            "git.projects.api.fetch",
            "mirror",
        ],
    );
    jjosh(
        &client,
        &[
            "config",
            "set",
            "--repo",
            "git.projects.api.push",
            "missing",
        ],
    );
    jjosh(
        &client,
        &[
            "config",
            "set",
            "--repo",
            "git.projects.beta.fetch",
            "missing",
        ],
    );

    // Neither the selected project's push setting nor another project's
    // invalid fetch setting participates in this fetch.
    jjosh(
        &client,
        &["git", "fetch", "--project", "api", "--branch", "incoming"],
    );
    assert_eq!(
        file_at_revision(&client, "incoming#api@mirror", "api/value.txt"),
        b"mirror-v1\n"
    );
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "incoming#api@api-upstream"])
            .status
            .success()
    );
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "incoming#beta@origin"])
            .status
            .success()
    );

    // Explicit selection wins even over a global setting outside the project.
    jjosh(
        &client,
        &[
            "--config",
            "git.fetch=origin",
            "--config",
            "git.projects.api.fetch=missing",
            "git",
            "fetch",
            "--project",
            "api",
            "--remote",
            "api-upstream",
            "--branch",
            "explicit",
        ],
    );
    assert_eq!(
        file_at_revision(&client, "explicit#api@api-upstream", "api/value.txt"),
        b"source-v1\n"
    );
    jjosh(
        &client,
        &[
            "--config",
            "git.fetch=[\"missing-root\"]",
            "--config",
            "git.projects.api.fetch=[\"api-upstream\"]",
            "git",
            "fetch",
            "--project",
            "api",
            "--branch",
            "global",
        ],
    );
    assert_eq!(
        file_at_revision(&client, "global#api@api-upstream", "api/value.txt"),
        b"source-v1\n"
    );

    let git_dir = client.join(".jj/repo/store/git");
    let refs = git(&git_dir, &["for-each-ref"]);
    let operation = operation_id(&client);
    for selection in [
        vec!["--config", "git.projects.api.fetch=missing"],
        vec!["--config", "git.projects.api.fetch=origin"],
        vec!["--remote", "origin"],
        vec!["--remote", "api-upstream", "--remote", "origin"],
    ] {
        let mut args = vec!["git", "fetch", "--project", "api", "--branch", "forbidden"];
        args.extend(selection);
        assert!(!jjosh_unchecked(&client, &args).status.success());
        assert_eq!(git(&git_dir, &["for-each-ref"]), refs);
        assert_eq!(operation_id(&client), operation);
    }

    // Unconfigured project selection cannot borrow another project's origin.
    jjosh(
        &client,
        &["config", "unset", "--repo", "git.projects.api.fetch"],
    );
    assert!(
        !jjosh_unchecked(
            &client,
            &["git", "fetch", "--project", "api", "--branch", "fallback"]
        )
        .status
        .success()
    );
    assert_eq!(git(&git_dir, &["for-each-ref"]), refs);
    assert_eq!(operation_id(&client), operation);
    jjosh(
        &client,
        &["git", "remote", "rename", "origin#beta", "beta-upstream"],
    );
    jjosh(
        &client,
        &["git", "remote", "rename", "api-upstream#api", "origin"],
    );
    jjosh(
        &client,
        &["git", "fetch", "--project", "api", "--branch", "fallback"],
    );
    assert_eq!(
        file_at_revision(&client, "fallback#api@origin", "api/value.txt"),
        b"source-v1\n"
    );

    // Plain fetch keeps native root origin selection, ignoring project settings.
    jjosh(
        &client,
        &[
            "config",
            "set",
            "--repo",
            "git.projects.api.fetch",
            "missing",
        ],
    );
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    jjosh(&client, &["git", "fetch", "--branch", "ordinary"]);
    assert_eq!(
        file_at_revision(&client, "ordinary@origin", "src/value.txt"),
        b"source-v1\n"
    );
    // Conversely, a bad fetch default cannot block this project's push.
    jjosh(
        &client,
        &["config", "set", "--repo", "git.projects.api.push", "origin"],
    );
    fs::write(client.join("api/value.txt"), "push independent of fetch\n").unwrap();
    jjosh(&client, &["describe", "-m", "directional default"]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--named",
            "directional#api=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&source, &["show", "directional:src/value.txt"]),
        "push independent of fetch\n"
    );
    assert_eq!(
        git(
            &mirror,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/directional"
            ]
        ),
        ""
    );
    assert_eq!(
        git(
            &beta,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/directional"
            ]
        ),
        ""
    );
}

#[test]
fn recreated_remote_does_not_inherit_restored_observations() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "packages/api", &source, ":/src");
    let original = physical_remote(&client, "api", "api-upstream");
    let git_dir = client.join(".jj/repo/store/git");
    let original_connection = git(
        &git_dir,
        &[
            "config",
            "--get",
            &format!("remote.{original}.jjosh-connectionId"),
        ],
    );
    let original_tip = commit_id(&client, "main#api@api-upstream");
    jjosh(&client, &["bookmark", "create", "local#api"]);
    let local = commit_id(&client, "local#api");
    let historical = operation_id(&client);
    jjosh(&client, &["git", "remote", "remove", "api-upstream#api"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "api-upstream",
            source.to_str().unwrap(),
            "--project",
            "api",
            "--whole",
        ],
    );
    let replacement = physical_remote(&client, "api", "api-upstream");
    let replacement_connection = git(
        &git_dir,
        &[
            "config",
            "--get",
            &format!("remote.{replacement}.jjosh-connectionId"),
        ],
    );
    assert_eq!(original, "api-upstream#api");
    assert_eq!(replacement, original);
    assert_ne!(replacement_connection, original_connection);
    let replacement_url = git(
        &git_dir,
        &["config", "--get", &format!("remote.{replacement}.url")],
    );
    jjosh(
        &client,
        &[
            "op",
            "restore",
            historical.trim(),
            "--what",
            "remote-tracking",
        ],
    );
    // Restoring knowledge must not replace the current connection or lend it
    // the old instance's filtered target/tracking.
    assert_eq!(commit_id(&client, "local#api"), local);
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "main#api@api-upstream"])
            .status
            .success()
    );
    assert_eq!(physical_remote(&client, "api", "api-upstream"), replacement);
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &[
                    "--at-op",
                    historical.trim(),
                    "log",
                    "-r",
                    "main#api@api-upstream",
                    "--no-graph",
                    "-T",
                    "commit_id",
                ],
            )
            .stdout,
        )
        .unwrap()
        .trim(),
        original_tip
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "api-upstream#api",
            "--branch",
            "main",
        ],
    );
    assert_eq!(
        file_at_revision(
            &client,
            "main#api@api-upstream",
            "packages/api/src/value.txt"
        ),
        b"source-v1\n"
    );
    jjosh(&client, &["op", "restore", historical.trim()]);
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "api-upstream#api",
                "--branch",
                "main"
            ]
        )
        .status
        .success()
    );
    // Full restore revives the old logical remote, not its local configuration.
    // Ordinary remove handles it offline and must leave B's configuration alone.
    jjosh(&client, &["git", "remote", "remove", "api-upstream#api"]);
    assert_eq!(commit_id(&client, "local#api"), local);
    assert_eq!(
        git(
            &client.join(".jj/repo/store/git"),
            &["config", "--get", &format!("remote.{replacement}.url")],
        ),
        replacement_url
    );

    jjosh(&client, &["op", "restore", historical.trim()]);
    let (_, alternate, _) = create_remote(temp.path(), "alternate");
    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "remote",
            "set-url",
            "api-upstream#api",
            alternate.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("owned by another connection"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(
        git(
            &client.join(".jj/repo/store/git"),
            &["config", "--get", &format!("remote.{replacement}.url")],
        ),
        replacement_url
    );
}

#[test]
fn root_remote_restore_preserves_new_connection_without_inheriting_old_refs() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, old_tip) = create_remote(temp.path(), "source");
    let (_, replacement, new_tip) = create_remote(temp.path(), "replacement");
    let client = create_client(temp.path(), false);
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "main"],
    );
    jjosh(&client, &["bookmark", "track", "main@origin"]);
    let historical = operation_id(&client);
    let original_connection = git(
        &client.join(".jj/repo/store/git"),
        &["config", "--get", "remote.origin.jjosh-connectionId"],
    );
    assert_eq!(commit_id(&client, "main@origin"), old_tip);
    jjosh(&client, &["git", "remote", "remove", "origin"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            replacement.to_str().unwrap(),
        ],
    );
    let replacement_connection = git(
        &client.join(".jj/repo/store/git"),
        &["config", "--get", "remote.origin.jjosh-connectionId"],
    );
    assert_ne!(replacement_connection, original_connection);
    jjosh(
        &client,
        &[
            "op",
            "restore",
            historical.trim(),
            "--what",
            "remote-tracking",
        ],
    );
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "main@origin"])
            .status
            .success()
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "main"],
    );
    assert_eq!(commit_id(&client, "main@origin"), new_tip);
    // Tracking belongs to the old instance: B's first fetch must not move main.
    assert_eq!(commit_id(&client, "main"), old_tip);
    assert_eq!(
        git(
            &client.join(".jj/repo/store/git"),
            &["config", "--get", "remote.origin.url"],
        )
        .trim(),
        replacement.to_str().unwrap()
    );
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &[
                    "--at-op",
                    historical.trim(),
                    "log",
                    "-r",
                    "main@origin",
                    "--no-graph",
                    "-T",
                    "commit_id"
                ],
            )
            .stdout
        )
        .unwrap()
        .trim(),
        old_tip
    );
}

#[test]
fn root_tracking_restore_follows_connection_through_rename_and_alias_swap() {
    let temp = tempfile::tempdir().unwrap();
    let (source_work, source, old_tip) = create_remote(temp.path(), "source");
    let (_, other, other_tip) = create_remote(temp.path(), "other");
    let client = create_client(temp.path(), false);
    for (name, path) in [("origin", &source), ("backup", &other)] {
        jjosh(
            &client,
            &["git", "remote", "add", name, path.to_str().unwrap()],
        );
        jjosh(
            &client,
            &["git", "fetch", "--remote", name, "--branch", "main"],
        );
    }
    jjosh(&client, &["bookmark", "track", "main@origin"]);
    let historical = operation_id(&client);
    jjosh(&client, &["git", "remote", "rename", "origin", "temporary"]);
    jjosh(
        &client,
        &[
            "op",
            "restore",
            historical.trim(),
            "--what",
            "remote-tracking",
        ],
    );
    assert_eq!(commit_id(&client, "main@temporary"), old_tip);
    assert!(
        !jjosh_unchecked(&client, &["log", "-r", "main@origin"])
            .status
            .success()
    );

    jjosh(&client, &["git", "remote", "rename", "backup", "origin"]);
    jjosh(&client, &["git", "remote", "rename", "temporary", "backup"]);
    jjosh(
        &client,
        &[
            "op",
            "restore",
            historical.trim(),
            "--what",
            "remote-tracking",
        ],
    );
    assert_eq!(commit_id(&client, "main@backup"), old_tip);
    assert_eq!(commit_id(&client, "main@origin"), other_tip);
    assert_eq!(commit_id(&client, "main"), old_tip);
    let git_dir = client.join(".jj/repo/store/git");
    assert_eq!(
        git(&git_dir, &["config", "--get", "remote.backup.url"]).trim(),
        source.to_str().unwrap()
    );
    assert_eq!(
        git(&git_dir, &["config", "--get", "remote.origin.url"]).trim(),
        other.to_str().unwrap()
    );

    fs::write(source_work.join("src/value.txt"), "updated source\n").unwrap();
    git(&source_work, &["add", "."]);
    git(&source_work, &["commit", "-m", "advance tracked source"]);
    git(&source_work, &["push", source.to_str().unwrap(), "main"]);
    let new_tip = git(&source_work, &["rev-parse", "HEAD"]);
    jjosh(
        &client,
        &["git", "fetch", "--remote", "backup", "--branch", "main"],
    );
    assert_eq!(commit_id(&client, "main"), new_tip.trim());
    assert_eq!(commit_id(&client, "main@origin"), other_tip);
}

#[test]
fn root_remote_without_url_can_be_renamed_reconnected_and_its_alias_reused() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, tip) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    let git_dir = client.join(".jj/repo/store/git");
    git(&git_dir, &["config", "--unset", "remote.origin.url"]);

    jjosh(&client, &["git", "remote", "rename", "origin", "upstream"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "set-url",
            "upstream",
            source.to_str().unwrap(),
        ],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "upstream", "--branch", "main"],
    );
    assert_eq!(commit_id(&client, "main@upstream"), tip);
    // The old section must not keep reserving origin or duplicate the owner.
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "main"],
    );
    assert_eq!(commit_id(&client, "main@origin"), tip);
}

#[test]
fn root_remote_without_url_can_be_removed_and_recreated() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, tip) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    git(
        &client.join(".jj/repo/store/git"),
        &["config", "--unset", "remote.origin.url"],
    );

    jjosh(&client, &["git", "remote", "remove", "origin"]);
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "main"],
    );
    assert_eq!(commit_id(&client, "main@origin"), tip);
}

#[test]
fn restored_root_remote_leaves_foreign_url_less_configuration_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    let old_operation = operation_id(&client);
    jjosh(&client, &["git", "remote", "remove", "origin"]);
    jjosh(
        &client,
        &["git", "remote", "add", "origin", source.to_str().unwrap()],
    );
    let git_dir = client.join(".jj/repo/store/git");
    git(&git_dir, &["config", "--unset", "remote.origin.url"]);
    let foreign_config = fs::read(git_dir.join("config")).unwrap();

    jjosh(
        &client,
        &["op", "restore", old_operation.trim(), "--what", "repo"],
    );
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "remote",
                "set-url",
                "origin",
                source.to_str().unwrap()
            ],
        )
        .status
        .success()
    );
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), foreign_config);
    jjosh(&client, &["git", "remote", "rename", "origin", "offline"]);
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), foreign_config);
    jjosh(
        &client,
        &["op", "restore", old_operation.trim(), "--what", "repo"],
    );
    jjosh(&client, &["git", "remote", "remove", "origin"]);
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), foreign_config);
}

#[test]
fn base_free_publication_uses_unique_ancestry_evidence_and_rejects_ambiguous_raw_contexts() {
    let temp = tempfile::tempdir().unwrap();
    let (work, source, original) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "api", "--path", "api"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "source",
            source.to_str().unwrap(),
            "--project",
            "api",
            "--filter",
            ":/src",
        ],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "source#api", "--branch", "main"],
    );
    jjosh(&client, &["new", "main#api@source", "-m", "new topic"]);
    fs::write(client.join("api/value.txt"), "topic edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "new topic"]);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "source",
            "--named",
            "topic#api=@",
        ],
    );
    assert_eq!(git(&source, &["rev-parse", "topic^"]).trim(), original);
    assert_eq!(
        git(&source, &["show", "topic:outside.txt"]),
        "source-outside\n"
    );

    fs::write(work.join("outside.txt"), "different raw context\n").unwrap();
    git(&work, &["commit", "-am", "outside-only branch"]);
    git(&work, &["push", source.to_str().unwrap(), "HEAD:variant"]);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "source#api",
            "--branch",
            "variant",
        ],
    );
    assert_eq!(
        commit_id(&client, "main#api@source"),
        commit_id(&client, "variant#api@source")
    );
    jjosh(
        &client,
        &["new", "main#api@source", "-m", "ambiguous sibling"],
    );
    fs::write(client.join("api/value.txt"), "sibling edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "ambiguous sibling"]);
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "source",
                "--named",
                "sibling#api=@"
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        git(
            &source,
            &["for-each-ref", "--format=%(refname)", "refs/heads/sibling"]
        ),
        ""
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "source",
            "--named",
            "sibling#api=@",
            "--base",
            "main",
        ],
    );
    assert_eq!(
        git(&source, &["show", "sibling:outside.txt"]),
        "source-outside\n"
    );
    assert_eq!(
        git(&source, &["show", "variant:outside.txt"]),
        "different raw context\n"
    );
}

#[test]
fn fetch_records_raw_advances_even_when_the_projected_target_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let (work, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "api", &source, ":/src");
    let canonical = commit_id(&client, "main#api@api-upstream");
    let observed = operation_id(&client);
    fs::write(work.join("outside.txt"), "new outside content\n").unwrap();
    git(&work, &["commit", "-am", "outside-only update"]);
    git(&work, &["push", source.to_str().unwrap(), "main"]);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "api-upstream#api",
            "--branch",
            "main",
        ],
    );
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
    assert_ne!(operation_id(&client), observed);
    jjosh(
        &client,
        &[
            "op",
            "restore",
            observed.trim(),
            "--what",
            "remote-tracking",
        ],
    );
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "api-upstream#api",
            "--branch",
            "main",
        ],
    );
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
    assert_eq!(
        git(&source, &["show", "main:outside.txt"]),
        "new outside content\n"
    );
}

#[test]
fn literal_source_base_retains_shallow_generation_after_deepening_and_restore() {
    let temp = tempfile::tempdir().unwrap();
    let (work, source, _) = create_remote(temp.path(), "source");
    fs::write(work.join("src/value.txt"), "source-v2\n").unwrap();
    git(&work, &["commit", "-am", "source-v2"]);
    git(&work, &["push", source.to_str().unwrap(), "main"]);
    let raw_tip = git(&source, &["rev-parse", "main"]).trim().to_owned();
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "api", "--path", "api"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "source",
            source.to_str().unwrap(),
            "--project",
            "api",
            "--filter",
            ":/src",
        ],
    );
    let fetched = jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "source#api",
            "--revision",
            &raw_tip,
            "--depth",
            "1",
        ],
    );
    let stderr = String::from_utf8(fetched.stderr).unwrap();
    let prefix = format!("Fetched revision {raw_tip} as ");
    let canonical = stderr
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap();
    jjosh(
        &client,
        &["new", canonical, "-m", "continue shallow source"],
    );
    fs::write(client.join("api/value.txt"), "local continuation\n").unwrap();
    jjosh(&client, &["describe", "-m", "continue shallow source"]);
    jjosh(&client, &["bookmark", "create", "topic#api"]);
    let original = commit_id(&client, "topic#api");
    let shallow_operation = operation_id(&client);
    jjosh(
        &client,
        &[
            "--config",
            "git.abandon-unreachable-commits=false",
            "git",
            "fetch",
            "--remote",
            "source#api",
            "--revision",
            &raw_tip,
            "--unshallow",
        ],
    );
    jjosh(&client, &["op", "restore", shallow_operation.trim()]);
    assert_eq!(commit_id(&client, "topic#api"), original);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "source",
            "--bookmark",
            "topic#api",
            "--base",
            &raw_tip,
        ],
    );
    assert_eq!(git(&source, &["rev-parse", "topic^"]).trim(), raw_tip);
    assert_eq!(git(&source, &["rev-list", "--count", "topic"]).trim(), "3");
    assert_eq!(
        git(&source, &["show", "topic:src/value.txt"]),
        "local continuation\n"
    );
    assert_eq!(
        git(&source, &["show", "topic:outside.txt"]),
        "source-outside\n"
    );
}

#[test]
fn duplicate_remote_add_preserves_effective_source_and_layout() {
    let temp = tempfile::tempdir().unwrap();
    let (_, original, _) = create_remote(temp.path(), "original");
    let (_, replacement, _) = create_remote(temp.path(), "replacement");
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "pkg", "--path", "vendor/pkg"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "source",
            original.to_str().unwrap(),
            "--filter",
            ":/src",
            "--project",
            "pkg",
        ],
    );
    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "remote",
            "add",
            "source",
            replacement.to_str().unwrap(),
            "--filter",
            ":/",
            "--project",
            "pkg",
        ],
    );
    assert!(!rejected.status.success());
    jjosh(
        &client,
        &["git", "fetch", "--remote", "source#pkg", "--branch", "main"],
    );
    assert_eq!(
        file_at_revision(&client, "main#pkg@source", "vendor/pkg/value.txt"),
        b"original-v1\n",
    );
}

#[test]
fn project_peers_use_their_own_external_layout_in_both_directions() {
    let temp = tempfile::tempdir().unwrap();
    let (_, upstream, _) = create_remote(temp.path(), "upstream");
    let flat_work = temp.path().join("flat-work");
    fs::create_dir(&flat_work).unwrap();
    git(&flat_work, &["init", "-b", "main"]);
    git(&flat_work, &["config", "user.name", "Smoke Test"]);
    git(&flat_work, &["config", "user.email", "smoke@example.com"]);
    fs::write(flat_work.join("value.txt"), "flat source\n").unwrap();
    git(&flat_work, &["add", "."]);
    git(&flat_work, &["commit", "-m", "standalone source"]);
    let flat = temp.path().join("flat.git");
    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            flat_work.to_str().unwrap(),
            flat.to_str().unwrap(),
        ],
    );
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "pkg", "--path", "vendor/pkg"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "upstream",
            upstream.to_str().unwrap(),
            "--project",
            "pkg",
            "--filter",
            ":/src",
            "--base",
            "main",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "flat",
            flat.to_str().unwrap(),
            "--project",
            "pkg",
            "--whole",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "upstream#pkg",
            "--branch",
            "main",
        ],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "flat#pkg", "--branch", "main"],
    );
    assert_eq!(
        file_at_revision(&client, "main#pkg@upstream", "vendor/pkg/value.txt"),
        b"upstream-v1\n"
    );
    assert_eq!(
        file_at_revision(&client, "main#pkg@flat", "vendor/pkg/value.txt"),
        b"flat source\n"
    );
    jjosh(
        &client,
        &["new", "main#pkg@flat", "-m", "edit standalone project"],
    );
    fs::write(client.join("vendor/pkg/value.txt"), "edited flat\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit standalone project"]);
    jjosh(&client, &["bookmark", "create", "topic#pkg"]);
    jjosh(
        &client,
        &["git", "push", "--remote", "flat", "--bookmark", "topic#pkg"],
    );
    assert_eq!(
        git(&flat, &["ls-tree", "-r", "--name-only", "topic"]),
        "value.txt\n"
    );
    assert_eq!(git(&flat, &["show", "topic:value.txt"]), "edited flat\n");
    assert_eq!(
        git(&upstream, &["show", "main:outside.txt"]),
        "upstream-outside\n"
    );

    jjosh(&client, &["git", "remote", "remove", "upstream#pkg"]);
    jjosh(&client, &["new", "-m", "continue standalone project"]);
    fs::write(client.join("vendor/pkg/value.txt"), "continued flat\n").unwrap();
    jjosh(&client, &["describe", "-m", "continue standalone project"]);
    jjosh(&client, &["bookmark", "set", "topic#pkg"]);
    jjosh(
        &client,
        &["git", "push", "--remote", "flat", "--bookmark", "topic#pkg"],
    );
    assert_eq!(
        git(&flat, &["ls-tree", "-r", "--name-only", "topic"]),
        "value.txt\n"
    );
    assert_eq!(git(&flat, &["show", "topic:value.txt"]), "continued flat\n");
}

#[test]
fn marker_free_reattachment_preserves_edited_tree_and_full_history() {
    let temp = tempfile::tempdir().unwrap();
    let (work, bare, _) = create_remote(temp.path(), "reattach");
    fs::write(work.join("src/value.txt"), "upstream-v2\n").unwrap();
    git(&work, &["commit", "-am", "upstream-v2"]);
    git(&work, &["push", bare.to_str().unwrap(), "main"]);
    let client = create_client(temp.path(), false);
    import_project(&client, "deps", "vendor/deps", &bare, ":/src");
    assert_eq!(
        file_at_revision(
            &client,
            "parents(main#deps@deps-upstream)",
            "vendor/deps/value.txt"
        ),
        b"reattach-v1\n",
    );
    fs::write(client.join("vendor/deps/value.txt"), "maintained edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "maintain imported history"]);
    jjosh(&client, &["new", "-m", "local descendant"]);
    fs::write(client.join("vendor/deps/local.txt"), "local descendant\n").unwrap();
    let local = commit_id(&client, "@");
    let history_args = [
        "log",
        "--no-graph",
        "-r",
        "::@",
        "-T",
        "commit_id ++ \"\\n\"",
    ];
    let history = jjosh(&client, &history_args).stdout;
    assert!(!client.join("vendor/deps/.link.josh").exists());

    // Removing the last remote must retain the project's independently recorded mount.
    jjosh(&client, &["git", "remote", "remove", "deps-upstream#deps"]);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "deps-peer",
            bare.to_str().unwrap(),
            "--filter",
            ":/src",
            "--project",
            "deps",
            "--base",
            "main",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "deps-peer#deps",
            "--branch",
            "main",
        ],
    );

    assert_eq!(commit_id(&client, "@"), local);
    assert_eq!(jjosh(&client, &history_args).stdout, history);
    assert_eq!(
        fs::read(client.join("vendor/deps/value.txt")).unwrap(),
        b"maintained edit\n"
    );
    assert_eq!(
        fs::read(client.join("vendor/deps/local.txt")).unwrap(),
        b"local descendant\n"
    );
    assert_eq!(fs::read(client.join("root.txt")).unwrap(), b"root\n");
    assert_eq!(
        file_at_revision(&client, "main#deps@deps-peer", "vendor/deps/value.txt"),
        b"upstream-v2\n",
    );
    assert!(!client.join("vendor/deps/.link.josh").exists());
}

#[test]
fn unrelated_malformed_historical_marker_does_not_block_remote_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let (_, bare, _) = create_remote(temp.path(), "unrelated");
    let client = create_client(temp.path(), false);
    fs::create_dir(client.join("legacy")).unwrap();
    let marker = ":link[mode=\"embedded\",commit=\"unterminated\n";
    fs::write(client.join("legacy/.link.josh"), marker).unwrap();
    jjosh(&client, &["describe", "-m", "historical malformed marker"]);
    jjosh(&client, &["new"]);
    let local = commit_id(&client, "@");
    jjosh(
        &client,
        &["project", "add", "deps", "--path", "vendor/deps"],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "deps-source",
            bare.to_str().unwrap(),
            "--filter",
            ":/src",
            "--project",
            "deps",
            "--base",
            "main",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "deps-source#deps",
            "--branch",
            "main",
        ],
    );
    assert_eq!(commit_id(&client, "@"), local);
    jjosh(&client, &["new", "@", "main#deps@deps-source"]);
    assert_eq!(
        fs::read(client.join("vendor/deps/value.txt")).unwrap(),
        b"unrelated-v1\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("legacy/.link.josh")).unwrap(),
        marker
    );
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
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
    // neither contributes a commit to this project's published history.
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published-v1\n").unwrap();
    jjosh(&client, &["status"]);
    let local_change = change_id(&client, "@");
    jjosh(&client, &["bookmark", "track", "main#deps@deps-upstream"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
        ],
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
        &[
            "bookmark",
            "set",
            "topic#deps",
            "-r",
            "@",
            "--allow-backwards",
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
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["status"]);
    let local_change = change_id(&client, "@");
    jjosh(&client, &["bookmark", "track", "main#deps@deps-upstream"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
        ],
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
fn project_registration_supports_sha256_but_josh_conversion_rejects_it() {
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
    jjosh(&client, &["project", "add", "deps", "--path", "deps"]);

    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "remote",
            "add",
            "deps-upstream",
            remote_bare.to_str().unwrap(),
            "--filter",
            ":/src",
            "--project",
            "deps",
            "--base",
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
    fs::write(client.join("deps/value.txt"), "published local change\n").unwrap();
    jjosh(&client, &["describe", "-m", "local change"]);
    jjosh(&client, &["bookmark", "track", "main#deps@deps-upstream"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
        ],
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
            "deps-upstream#deps",
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
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
        &[
            "bookmark",
            "set",
            "topic#deps",
            "-r",
            "@",
            "--allow-backwards",
        ],
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
    // Historical markers are ordinary tracked files, never remote configuration.
    let redirected_metadata = josh_core::filter::as_file(
        josh_core::filter::parse(":/src:prefix=deps")
            .unwrap()
            .with_meta("name", "deps".to_owned())
            .with_meta("remote", other_bare.to_str().unwrap().to_owned())
            .with_meta("push", other_bare.to_str().unwrap().to_owned())
            .with_meta("target", "redirected".to_owned())
            .with_meta(
                "commit",
                git(&other_bare, &["rev-parse", "main"]).trim().to_owned(),
            )
            .with_meta("mode", "embedded".to_owned()),
        0,
    );
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
                "deps-upstream#deps",
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
        assert_eq!(
            git(
                &remote_bare,
                &[
                    "ls-tree",
                    "-r",
                    "--name-only",
                    &format!("published-{index}")
                ]
            ),
            "outside.txt\nsrc/local.txt\nsrc/value.txt\n",
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
        import_project(&client, "deps", "deps", &bare, ":/src");
        fs::write(client.join("deps/local.txt"), "local work\n").unwrap();
        let local = commit_id(&client, "@");
        let initial = commit_id(&client, "main#deps@deps-upstream");
        fs::write(work.join("src/value.txt"), "upstream v2\n").unwrap();
        git(&work, &["commit", "-am", "v2"]);
        git(&work, &["push", bare.to_str().unwrap(), "main"]);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "deps-upstream#deps",
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
                "deps-upstream#deps",
                "--branch",
                "main",
                "--branch",
                "keep",
            ],
        );
        let updated = commit_id(&client, "main#deps@deps-upstream");
        assert_ne!(updated, initial);
        assert_eq!(commit_id(&client, "@"), local);
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
                "deps-upstream#deps",
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
                "deps-upstream#deps",
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
                "deps-upstream#deps",
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
            "project",
            "import",
            "--nested",
            &format!("app={}", work.display()),
        ],
    );
    jjosh(&client, &["new", "@", "main#app"]);
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
    let local_before_attachment = commit_id(&client, "@");
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "app-upstream",
            bare.to_str().unwrap(),
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "attach",
            "app-upstream",
            "--project",
            "app",
            "--whole",
            "--base",
            "main",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "app-upstream#app",
            "--branch",
            "main",
        ],
    );
    assert_eq!(commit_id(&client, "@"), local_before_attachment);
    assert_eq!(commit_id(&client, "main#app"), fork);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "app-upstream#app",
            "--tag",
            "v1",
        ],
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
            "app-upstream#app",
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
            "app-upstream#app",
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
    import_project(&client, "deps", "vendor/deps", &bare, ":/src");
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["describe", "-m", "published local change"]);
    let published = commit_id(&client, "@");
    let published_change = change_id(&client, "@");
    jjosh(&client, &["bookmark", "track", "main#deps@deps-upstream"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "deps-upstream#deps",
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src");
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["describe", "-m", "published local change"]);
    let published = commit_id(&client, "@");
    jjosh(&client, &["bookmark", "track", "main#deps@deps-upstream"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "set",
            "main#deps",
            "-r",
            "@",
            "--allow-backwards",
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
            "deps-upstream#deps",
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

#[test]
fn arbitrary_publication_requires_explicit_source_without_reinterpreting_literal_names() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let (_, publication, _) = create_remote(temp.path(), "publication");
    let client = create_client(temp.path(), false);
    for (name, source) in [("alpha", &alpha), ("beta", &beta)] {
        import_project(&client, name, name, source, ":/src");
    }
    fs::write(client.join("alpha/value.txt"), "alpha edit\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta edit\n").unwrap();
    fs::write(client.join("root.txt"), "never publish the root\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit both projects"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "create",
            "alpha-topic#alpha",
            "beta-topic#beta",
            "-r",
            "@",
        ],
    );
    // Publication is an ordinary endpoint with no project attachment.
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "review+origin",
            publication.to_str().unwrap(),
        ],
    );
    let before = git(&publication, &["show-ref"]);
    assert!(
        !jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "review+origin",
                "--bookmark",
                "alpha-topic#alpha",
                "--allow-empty-description"
            ]
        )
        .status
        .success()
    );
    assert_eq!(git(&publication, &["show-ref"]), before);
    for project in ["alpha", "beta"] {
        jjosh(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "review+origin",
                "--source",
                &format!("{project}-upstream#{project}"),
                "--bookmark",
                &format!("{project}-topic#{project}"),
                "--allow-empty-description",
            ],
        );
    }
    assert_eq!(
        git(&publication, &["show", "alpha-topic:src/value.txt"]),
        "alpha edit\n"
    );
    assert_eq!(
        git(&publication, &["show", "beta-topic:src/value.txt"]),
        "beta edit\n"
    );
    assert_eq!(
        git(&publication, &["show", "alpha-topic:outside.txt"]),
        "alpha-outside\n"
    );
    assert_eq!(
        git(&publication, &["show", "beta-topic:outside.txt"]),
        "beta-outside\n"
    );
    for branch in ["alpha-topic", "beta-topic"] {
        assert_eq!(
            git(&publication, &["ls-tree", "-r", "--name-only", branch]),
            "outside.txt\nsrc/value.txt\n"
        );
    }
    assert_eq!(
        commit_id(&client, "alpha-topic#alpha@review+origin#"),
        commit_id(&client, "alpha-topic#alpha"),
    );
    assert_eq!(
        commit_id(&client, "beta-topic#beta@review+origin#"),
        commit_id(&client, "beta-topic#beta"),
    );

    // One-shot conversion evidence cannot be reinterpreted by a later raw fetch.
    let projected = commit_id(&client, "alpha-topic#alpha@review+origin#");
    assert!(
        !jjosh_unchecked(&client, &["git", "fetch", "--remote", "review+origin"])
            .status
            .success()
    );
    assert_eq!(
        commit_id(&client, "alpha-topic#alpha@review+origin#"),
        projected
    );
    assert_eq!(commit_id(&client, "alpha-topic#alpha"), projected);
    // An unrelated selected wire ref is still a valid ordinary fetch.
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "review+origin",
            "--branch",
            "main",
        ],
    );
    assert_eq!(
        commit_id(&client, "alpha-topic#alpha@review+origin#"),
        projected
    );

    // One explicitly chosen source cannot silently interpret another project.
    jjosh(
        &client,
        &[
            "bookmark",
            "create",
            "collision#alpha",
            "collision#beta",
            "-r",
            "@",
        ],
    );
    let refs = git(&publication, &["show-ref"]);
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "review+origin",
            "--source",
            "alpha-upstream#alpha",
            "--bookmark",
            "collision#alpha",
            "--bookmark",
            "collision#beta",
            "--allow-empty-description",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(git(&publication, &["show-ref"]), refs);
    assert_eq!(operation_id(&client), operation);

    // An unregistered suffix remains a literal ordinary ref, including on wire.
    jjosh(
        &client,
        &["bookmark", "create", "unknown#missing", "-r", "@"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "review+origin",
            "--bookmark",
            "unknown#missing",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&publication, &["show", "unknown#missing:root.txt"]),
        "never publish the root\n"
    );
    assert_eq!(
        git(&publication, &["show", "unknown#missing:alpha/value.txt"]),
        "alpha edit\n"
    );
}

fn add_routed_link(client: &Path, scope: &str, source: &Path) {
    import_project(client, scope, scope, source, ":/src");
    jjosh(
        client,
        &[
            "bookmark",
            "track",
            &format!("main#{scope}@{scope}-upstream"),
        ],
    );
}

#[test]
fn project_push_defaults_route_a_wildcard_without_leaking_other_projects() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    add_routed_link(&client, "beta", &beta);
    let alpha_publication = temp.path().join("alpha-publication.git");
    let beta_publication = temp.path().join("beta-publication.git");
    for (scope, source, destination) in [
        ("alpha", &alpha, &alpha_publication),
        ("beta", &beta, &beta_publication),
    ] {
        git(
            temp.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        let remote = format!("{scope}-publication");
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "add",
                &remote,
                destination.to_str().unwrap(),
                "--like",
                &format!("{scope}-upstream#{scope}"),
            ],
        );
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--project",
                scope,
                "--remote",
                &remote,
                "--branch",
                "main",
            ],
        );
        jjosh(
            &client,
            &["bookmark", "track", &format!("main#{scope}@{remote}")],
        );
        jjosh(
            &client,
            &[
                "config",
                "set",
                "--repo",
                &format!("git.projects.{scope}.push"),
                &remote,
            ],
        );
        jjosh(
            &client,
            &[
                "config",
                "set",
                "--repo",
                &format!("git.projects.{scope}.fetch"),
                "missing",
            ],
        );
    }
    fs::write(client.join("alpha/value.txt"), "alpha edit\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta edit\n").unwrap();
    fs::write(client.join("root.txt"), "never publish the root\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit both projects"]);
    jjosh(
        &client,
        &["bookmark", "set", "main#alpha", "main#beta", "-r", "@"],
    );

    let alpha_refs = git(&alpha, &["show-ref"]);
    let beta_refs = git(&beta, &["show-ref"]);
    let operation = operation_id(&client);
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "main#*",
            "--allow-empty-description",
            "--dry-run",
        ],
    );
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(git(&alpha_publication, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta_publication, &["show-ref"]), beta_refs);
    assert_eq!(operation_id(&client), operation);

    jjosh(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "main#*",
            "--allow-empty-description",
        ],
    );
    for (scope, remote, source) in [
        ("alpha", "alpha-publication", &alpha_publication),
        ("beta", "beta-publication", &beta_publication),
    ] {
        assert_eq!(
            git(source, &["show", "main:src/value.txt"]),
            format!("{scope} edit\n")
        );
        assert_eq!(
            git(source, &["show", "main:outside.txt"]),
            format!("{scope}-outside\n")
        );
        assert_eq!(
            git(source, &["ls-tree", "-r", "--name-only", "main"]),
            "outside.txt\nsrc/value.txt\n"
        );
        assert_eq!(
            commit_id(&client, &format!("main#{scope}@{remote}")),
            commit_id(&client, &format!("main#{scope}"))
        );
    }
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
}

#[test]
fn automatic_push_rejects_selected_ambiguous_and_missing_routes_only() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let (_, fork, _) = create_remote(temp.path(), "fork");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    add_routed_link(&client, "beta", &beta);
    jjosh(
        &client,
        &["git", "remote", "rename", "alpha-upstream#alpha", "origin"],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "fork+origin",
            fork.to_str().unwrap(),
            "--like",
            "beta-upstream#beta",
            "--push-url",
            fork.to_str().unwrap(),
        ],
    );
    fs::write(client.join("alpha/value.txt"), "alpha edit\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit projects"]);
    jjosh(
        &client,
        &["bookmark", "set", "main#alpha", "main#beta", "-r", "@"],
    );
    let alpha_refs = git(&alpha, &["show-ref"]);
    let beta_refs = git(&beta, &["show-ref"]);
    let fork_refs = git(&fork, &["show-ref"]);
    let operation = operation_id(&client);
    let ambiguous = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "main#*",
            "--allow-empty-description",
        ],
    );
    assert!(!ambiguous.status.success());
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(git(&fork, &["show-ref"]), fork_refs);
    assert_eq!(operation_id(&client), operation);
    // A missing alias and another project's origin must both reject before
    // the healthy alpha route publishes anything.
    for default in ["missing", "origin"] {
        let rejected = jjosh_unchecked(
            &client,
            &[
                "--config",
                &format!("git.projects.beta.push={default}"),
                "git",
                "push",
                "--bookmark",
                "main#*",
                "--allow-empty-description",
            ],
        );
        assert!(!rejected.status.success());
        assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
        assert_eq!(git(&beta, &["show-ref"]), beta_refs);
        assert_eq!(git(&fork, &["show-ref"]), fork_refs);
        assert_eq!(operation_id(&client), operation);
    }

    jjosh(&client, &["project", "add", "missing", "--path", "missing"]);
    jjosh(&client, &["bookmark", "create", "main#missing", "-r", "@"]);
    let operation = operation_id(&client);
    let missing = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "main#alpha",
            "--bookmark",
            "main#missing",
            "--allow-empty-description",
        ],
    );
    assert!(!missing.status.success());
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(git(&fork, &["show-ref"]), fork_refs);
    assert_eq!(operation_id(&client), operation);

    // Invalid defaults and unknown unselected scopes do not block this batch.
    jjosh(
        &client,
        &[
            "--config",
            "git.projects.beta.push=missing",
            "git",
            "push",
            "--bookmark",
            "main#alpha",
            "--allow-empty-description",
        ],
    );
    assert_eq!(git(&alpha, &["show", "main:src/value.txt"]), "alpha edit\n");
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(git(&fork, &["show-ref"]), fork_refs);
}

#[test]
fn automatic_push_prepares_later_remote_leases_before_publishing_first() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (beta_work, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    add_routed_link(&client, "beta", &beta);
    fs::write(client.join("alpha/value.txt"), "alpha published\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta published\n").unwrap();
    jjosh(&client, &["describe", "-m", "publish both projects"]);
    jjosh(
        &client,
        &["bookmark", "set", "main#alpha", "main#beta", "-r", "@"],
    );
    jjosh(
        &client,
        &["git", "push", "--tracked", "--allow-empty-description"],
    );
    let alpha_refs = git(&alpha, &["show-ref"]);
    let alpha_observation = commit_id(&client, "main#alpha@alpha-upstream");
    let beta_observation = commit_id(&client, "main#beta@beta-upstream");

    git(&beta_work, &["fetch", beta.to_str().unwrap(), "main"]);
    git(&beta_work, &["switch", "--detach", "FETCH_HEAD"]);
    fs::write(beta_work.join("src/value.txt"), "external writer\n").unwrap();
    git(&beta_work, &["commit", "-am", "external advance"]);
    git(&beta_work, &["push", beta.to_str().unwrap(), "HEAD:main"]);
    let beta_refs = git(&beta, &["show-ref"]);

    jjosh(&client, &["new", "-m", "next local update"]);
    fs::write(client.join("alpha/value.txt"), "alpha must wait\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta stale\n").unwrap();
    jjosh(
        &client,
        &["bookmark", "set", "main#alpha", "main#beta", "-r", "@"],
    );
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &["git", "push", "--tracked", "--allow-empty-description"],
    );
    assert!(!rejected.status.success());
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(operation_id(&client), operation);
    assert_eq!(
        commit_id(&client, "main#alpha@alpha-upstream"),
        alpha_observation
    );
    assert_eq!(
        commit_id(&client, "main#beta@beta-upstream"),
        beta_observation
    );
}

#[test]
fn automatic_push_preserves_all_tracked_and_deleted_selection_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    add_routed_link(&client, "beta", &beta);
    fs::write(client.join("alpha/value.txt"), "alpha topic\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta topic\n").unwrap();
    jjosh(&client, &["describe", "-m", "new topics"]);
    jjosh(
        &client,
        &["bookmark", "create", "topic#alpha", "topic#beta", "-r", "@"],
    );
    let alpha_refs = git(&alpha, &["show-ref"]);
    let beta_refs = git(&beta, &["show-ref"]);
    jjosh(
        &client,
        &["git", "push", "--tracked", "--allow-empty-description"],
    );
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);

    jjosh(
        &client,
        &["git", "push", "--all", "--allow-empty-description"],
    );
    assert_eq!(
        git(&alpha, &["show", "topic:src/value.txt"]),
        "alpha topic\n"
    );
    assert_eq!(git(&beta, &["show", "topic:src/value.txt"]), "beta topic\n");
    assert_eq!(git(&alpha, &["show", "main:src/value.txt"]), "alpha-v1\n");
    assert_eq!(git(&beta, &["show", "main:src/value.txt"]), "beta-v1\n");
    let beta_refs = git(&beta, &["show-ref"]);
    jjosh(&client, &["new", "-m", "advance only alpha topic"]);
    fs::write(client.join("alpha/value.txt"), "alpha next\n").unwrap();
    jjosh(&client, &["bookmark", "set", "topic#alpha", "-r", "@"]);
    jjosh(
        &client,
        &["git", "push", "--tracked", "--allow-empty-description"],
    );
    assert_eq!(
        git(&alpha, &["show", "topic:src/value.txt"]),
        "alpha next\n"
    );
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);

    jjosh(&client, &["bookmark", "delete", "topic#alpha"]);
    jjosh(&client, &["git", "push", "--deleted"]);
    assert_eq!(
        git(
            &alpha,
            &["for-each-ref", "--format=%(refname)", "refs/heads"]
        ),
        "refs/heads/main\n"
    );
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
}

#[test]
fn project_push_uses_only_bound_origin_after_explicit_and_configured_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let (_, upstream, _) = create_remote(temp.path(), "upstream");
    let (_, fork, _) = create_remote(temp.path(), "fork");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "api", &upstream, ":/src");
    jjosh(
        &client,
        &["git", "remote", "rename", "api-upstream#api", "primary"],
    );
    jjosh(
        &client,
        &["git", "remote", "add", "fork", upstream.to_str().unwrap()],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "attach",
            "fork",
            "--project",
            "api",
            "--like",
            "primary",
        ],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "fork#api", "--branch", "main"],
    );
    fs::write(client.join("api/value.txt"), "project review\n").unwrap();
    fs::write(client.join("root.txt"), "private overlay\n").unwrap();
    jjosh(&client, &["describe", "-m", "project review"]);
    jjosh(&client, &["bookmark", "create", "review#api"]);
    let upstream_refs = git(&upstream, &["show-ref"]);
    let fork_refs = git(&fork, &["show-ref"]);

    // Identical endpoints do not disambiguate aliases without a bound origin.
    for destination in [&upstream, &fork] {
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "set-url",
                "fork#api",
                "--push",
                destination.to_str().unwrap(),
            ],
        );
        let operation = operation_id(&client);
        let rejected = jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--bookmark",
                "review#api",
                "--allow-empty-description",
            ],
        );
        assert!(!rejected.status.success());
        assert_eq!(git(&upstream, &["show-ref"]), upstream_refs);
        assert_eq!(git(&fork, &["show-ref"]), fork_refs);
        assert_eq!(operation_id(&client), operation);
    }
    jjosh(
        &client,
        &["git", "remote", "rename", "primary#api", "origin"],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "review#api",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&upstream, &["show", "review:src/value.txt"]),
        "project review\n"
    );
    assert_eq!(git(&fork, &["show-ref"]), fork_refs);
    jjosh(&client, &["bookmark", "create", "explicit#api"]);

    // Explicit remote selection overrides both global and project defaults.
    jjosh(
        &client,
        &[
            "--config",
            "git.push=fork",
            "--config",
            "git.projects.api.push=fork",
            "git",
            "push",
            "--remote",
            "origin",
            "--bookmark",
            "explicit#api",
            "--allow-empty-description",
        ],
    );
    assert_eq!(git(&fork, &["show-ref"]), fork_refs);
    assert_eq!(
        git(&upstream, &["show", "explicit:src/value.txt"]),
        "project review\n"
    );
    let upstream_refs = git(&upstream, &["show-ref"]);
    // A project default also wins over its own bound origin.
    jjosh(
        &client,
        &[
            "--config",
            "git.projects.api.push=fork",
            "git",
            "push",
            "--named",
            "default#api=@",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&fork, &["show", "default:src/value.txt"]),
        "project review\n"
    );
    assert_eq!(git(&upstream, &["show-ref"]), upstream_refs);

    // Root defaults cannot rescue an invalid selected project route.
    let fork_refs = git(&fork, &["show-ref"]);
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &[
            "--config",
            "git.push=fork",
            "--config",
            "git.projects.api.push=missing",
            "git",
            "push",
            "--bookmark",
            "review#api",
            "--allow-empty-description",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(git(&upstream, &["show-ref"]), upstream_refs);
    assert_eq!(git(&fork, &["show-ref"]), fork_refs);
    assert_eq!(operation_id(&client), operation);
}

#[test]
fn explicit_source_export_and_unscoped_refs_use_root_origin() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, publication, _) = create_remote(temp.path(), "publication");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            publication.to_str().unwrap(),
        ],
    );
    fs::write(client.join("alpha/value.txt"), "alpha review\n").unwrap();
    fs::write(client.join("root.txt"), "root review\n").unwrap();
    jjosh(&client, &["describe", "-m", "review"]);
    jjosh(
        &client,
        &[
            "bookmark",
            "create",
            "review#alpha",
            "whole-tree",
            "-r",
            "@",
        ],
    );
    let alpha_refs = git(&alpha, &["show-ref"]);
    jjosh(
        &client,
        &[
            "--config",
            "git.push=origin",
            "git",
            "push",
            "--remote",
            "origin",
            "--source",
            "alpha-upstream#alpha",
            "--bookmark",
            "review#alpha",
            "--allow-empty-description",
        ],
    );
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(
        git(&publication, &["show", "review:src/value.txt"]),
        "alpha review\n"
    );
    assert_eq!(
        git(&publication, &["ls-tree", "-r", "--name-only", "review"]),
        "outside.txt\nsrc/value.txt\n"
    );

    // With several remotes, the ordinary unscoped fallback still picks origin.
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "whole-tree",
            "--allow-empty-description",
        ],
    );
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(
        git(&publication, &["show", "whole-tree:root.txt"]),
        "root review\n"
    );
    assert_eq!(
        git(&publication, &["show", "whole-tree:alpha/value.txt"]),
        "alpha review\n"
    );
}

#[test]
fn automatic_push_rejects_wire_collisions_across_remote_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    add_routed_link(&client, "beta", &beta);
    fs::write(client.join("alpha/value.txt"), "alpha publication\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta publication\n").unwrap();
    jjosh(
        &client,
        &["describe", "-m", "two different scoped publications"],
    );
    jjosh(
        &client,
        &[
            "bookmark",
            "create",
            "collision#alpha",
            "collision#beta",
            "-r",
            "@",
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "set-url",
            "beta-upstream#beta",
            "--push",
            alpha.to_str().unwrap(),
        ],
    );
    let alpha_refs = git(&alpha, &["show-ref"]);
    let beta_refs = git(&beta, &["show-ref"]);
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "collision#*",
            "--allow-empty-description",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(operation_id(&client), operation);
}

fn add_tag_project(client: &Path, scope: &str, work: &Path, bare: &Path, native: bool) {
    if native {
        jjosh(work, &["git", "init", "--colocate"]);
        jjosh(
            client,
            &[
                "project",
                "import",
                "--nested",
                &format!("{scope}={}", work.display()),
            ],
        );
        let remote = format!("{scope}-upstream");
        jjosh(
            client,
            &["git", "remote", "add", &remote, bare.to_str().unwrap()],
        );
        jjosh(
            client,
            &[
                "git",
                "remote",
                "attach",
                &remote,
                "--project",
                scope,
                "--whole",
            ],
        );
        jjosh(
            client,
            &[
                "git",
                "fetch",
                "--project",
                scope,
                "--remote",
                &remote,
                "--branch",
                "main",
            ],
        );
    } else {
        add_routed_link(client, scope, bare);
    }
}

fn tag_object(git_dir: &Path, target: &str, kind: &str, name: &str, message: &str) -> String {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        file.path(),
        format!(
            "object {target}\ntype {kind}\ntag {name}\ntagger Release Author \
             <release@example.com> 1700000000 +0530\n\n{message}"
        ),
    )
    .unwrap();
    git(
        git_dir,
        &[
            "hash-object",
            "-t",
            "tag",
            "-w",
            file.path().to_str().unwrap(),
        ],
    )
    .trim()
    .to_owned()
}

#[test]
fn scoped_lightweight_tags_route_roundtrip_and_preserve_dry_run_state() {
    for native in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (alpha_work, alpha, _) = create_remote(temp.path(), "alpha");
        let (beta_work, beta, _) = create_remote(temp.path(), "beta");
        let client = create_client(temp.path(), false);
        add_tag_project(&client, "alpha", &alpha_work, &alpha, native);
        add_tag_project(&client, "beta", &beta_work, &beta, native);
        jjosh(
            &client,
            &["new", "main#alpha", "main#beta", "-m", "release both"],
        );
        let alpha_file = if native {
            "alpha/src/value.txt"
        } else {
            "alpha/value.txt"
        };
        let beta_file = if native {
            "beta/src/value.txt"
        } else {
            "beta/value.txt"
        };
        fs::write(client.join(alpha_file), "alpha release\n").unwrap();
        fs::write(client.join(beta_file), "beta release\n").unwrap();
        fs::write(client.join("root.txt"), "private overlay\n").unwrap();
        jjosh(&client, &["tag", "set", "v1.0#alpha", "v1.0#beta"]);
        let canonical = commit_id(&client, "tags(exact:\"v1.0#alpha\")");
        let git_dir = PathBuf::from(
            String::from_utf8(jjosh(&client, &["git", "root"]).stdout)
                .unwrap()
                .trim(),
        );
        let refs = git(&git_dir, &["show-ref"]);
        let operation = operation_id(&client);
        let alpha_refs = git(&alpha, &["show-ref"]);
        let beta_refs = git(&beta, &["show-ref"]);
        jjosh(&client, &["git", "push", "--tag", "v1.0#*", "--dry-run"]);
        assert_eq!(git(&git_dir, &["show-ref"]), refs);
        assert_eq!(operation_id(&client), operation);
        assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
        assert_eq!(git(&beta, &["show-ref"]), beta_refs);

        jjosh(&client, &["git", "push", "--tag", "v1.0#*"]);
        for (scope, bare, contents) in [
            ("alpha", &alpha, "alpha release\n"),
            ("beta", &beta, "beta release\n"),
        ] {
            assert_eq!(
                git(bare, &["cat-file", "-t", "refs/tags/v1.0"]).trim(),
                "commit"
            );
            assert_eq!(git(bare, &["show", "v1.0:src/value.txt"]), contents);
            assert_eq!(
                git(bare, &["ls-tree", "-r", "--name-only", "v1.0"]),
                "outside.txt\nsrc/value.txt\n"
            );
            assert!(
                !run(
                    bare,
                    Path::new("git"),
                    &["show-ref", "--verify", &format!("refs/tags/v1.0#{scope}")]
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
                    &format!("{scope}-upstream#{scope}"),
                    "--tag",
                    "v1.0",
                ],
            );
            assert_eq!(
                commit_id(
                    &client,
                    &format!("remote_tags(exact:\"v1.0#{scope}\", exact:\"{scope}-upstream\")")
                ),
                canonical
            );
        }

        jjosh(&client, &["new", "-m", "next alpha release"]);
        fs::write(client.join(alpha_file), "alpha replacement\n").unwrap();
        jjosh(&client, &["tag", "set", "--allow-move", "v1.0#alpha"]);
        let replacement = commit_id(&client, "tags(exact:\"v1.0#alpha\")");
        jjosh(&client, &["git", "push", "--tag", "v1.0#alpha"]);
        assert_eq!(
            git(&alpha, &["show", "v1.0:src/value.txt"]),
            "alpha replacement\n"
        );
        assert_eq!(
            git(&beta, &["show", "v1.0:src/value.txt"]),
            "beta release\n"
        );
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "alpha-upstream#alpha",
                "--tag",
                "v1.0",
            ],
        );
        assert_eq!(
            commit_id(
                &client,
                "remote_tags(exact:\"v1.0#alpha\", exact:\"alpha-upstream\")"
            ),
            replacement
        );
        jjosh(&client, &["tag", "delete", "v1.0#alpha"]);
        jjosh(&client, &["git", "push", "--tag", "v1.0#alpha"]);
        assert!(
            !run(
                &alpha,
                Path::new("git"),
                &["show-ref", "--verify", "refs/tags/v1.0"]
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
                "alpha-upstream#alpha",
                "--tag",
                "v1.0",
            ],
        );
        assert_eq!(
            commit_id(
                &client,
                "remote_tags(exact:\"v1.0#alpha\", exact:\"alpha-upstream\")"
            ),
            ""
        );
        // The confirmed deletion is a reusable absence lease.
        jjosh(&client, &["tag", "set", "v1.0#alpha", "-r", &replacement]);
        jjosh(&client, &["git", "push", "--tag", "v1.0#alpha"]);
        assert_eq!(
            git(&alpha, &["show", "v1.0:src/value.txt"]),
            "alpha replacement\n"
        );
        // A selected fetch must also refresh an externally deleted raw tag,
        // rather than leave the old object-ID lease authorizing replacements.
        git(&alpha, &["update-ref", "-d", "refs/tags/v1.0"]);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "alpha-upstream#alpha",
                "--tag",
                "v1.0",
            ],
        );
        assert_eq!(
            commit_id(
                &client,
                "remote_tags(exact:\"v1.0#alpha\", exact:\"alpha-upstream\")"
            ),
            ""
        );
        jjosh(&client, &["tag", "set", "v1.0#alpha", "-r", &replacement]);
        jjosh(&client, &["git", "push", "--tag", "v1.0#alpha"]);
        assert_eq!(
            git(&alpha, &["show", "v1.0:src/value.txt"]),
            "alpha replacement\n"
        );
    }
}

#[test]
fn fetched_unsigned_nested_tags_republish_annotations_and_exported_targets() {
    for native in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (work, source, tip) = create_remote(temp.path(), "alpha");
        let client = create_client(temp.path(), false);
        add_tag_project(&client, "alpha", &work, &source, native);
        let inner = tag_object(&source, &tip, "commit", "inner", "inner message\n");
        let outer = tag_object(
            &source,
            &inner,
            "tag",
            "v1.0",
            "release message\n\nrelease notes\n",
        );
        git(&source, &["update-ref", "refs/tags/v1.0", &outer]);
        jjosh(
            &client,
            &[
                "git",
                "fetch",
                "--remote",
                "alpha-upstream#alpha",
                "--tag",
                "v1.0",
            ],
        );
        jjosh(&client, &["tag", "track", "v1.0#alpha@alpha-upstream"]);
        jjosh(&client, &["git", "export"]);
        let canonical = commit_id(
            &client,
            "remote_tags(exact:\"v1.0#alpha\", exact:\"alpha-upstream\")",
        );
        let git_dir = PathBuf::from(
            String::from_utf8(jjosh(&client, &["git", "root"]).stdout)
                .unwrap()
                .trim(),
        );
        assert_eq!(
            git(&git_dir, &["cat-file", "-t", "refs/tags/v1.0#alpha"]).trim(),
            "tag"
        );
        assert_eq!(
            git(&git_dir, &["rev-parse", "refs/tags/v1.0#alpha^{}"]).trim(),
            canonical
        );

        let destination = temp.path().join("release.git");
        git(
            temp.path(),
            &["init", "--bare", destination.to_str().unwrap()],
        );
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "add",
                "release",
                destination.to_str().unwrap(),
            ],
        );
        // --all explicitly permits publishing a tag already tracked elsewhere.
        jjosh(
            &client,
            &[
                "git",
                "push",
                "--remote",
                "release",
                "--source",
                "alpha-upstream#alpha",
                "--all",
                "--allow-empty-description",
            ],
        );
        let published_outer = git(&destination, &["cat-file", "tag", "refs/tags/v1.0"]);
        let published_inner_id = published_outer
            .lines()
            .next()
            .unwrap()
            .strip_prefix("object ")
            .unwrap();
        let published_inner = git(&destination, &["cat-file", "tag", published_inner_id]);
        assert_eq!(
            published_outer.split_once("\ntype ").unwrap().1,
            git(&source, &["cat-file", "tag", &outer])
                .split_once("\ntype ")
                .unwrap()
                .1
        );
        assert_eq!(
            published_inner.split_once("\ntype ").unwrap().1,
            git(&source, &["cat-file", "tag", &inner])
                .split_once("\ntype ")
                .unwrap()
                .1
        );
        assert_eq!(
            git(&destination, &["show", "v1.0:src/value.txt"]),
            "alpha-v1\n"
        );
        assert_eq!(
            git(&destination, &["ls-tree", "-r", "--name-only", "v1.0"]),
            "outside.txt\nsrc/value.txt\n"
        );
        assert_ne!(
            git(&destination, &["rev-parse", "v1.0^{}"]).trim(),
            canonical
        );
        assert_eq!(git(&source, &["rev-parse", "refs/tags/v1.0"]).trim(), outer);
        if native {
            // A native JJ workspace source keeps annotation objects outside its
            // commit-target view, so exercise that source path as well.
            git(
                &work,
                &[
                    "fetch",
                    source.to_str().unwrap(),
                    "refs/tags/v1.0:refs/tags/v1.0",
                ],
            );
            jjosh(&work, &["git", "import"]);
            jjosh(
                &client,
                &[
                    "git",
                    "remote",
                    "add",
                    "native-source",
                    work.to_str().unwrap(),
                ],
            );
            jjosh(
                &client,
                &[
                    "git",
                    "remote",
                    "attach",
                    "native-source",
                    "--project",
                    "alpha",
                    "--whole",
                ],
            );
            jjosh(
                &client,
                &[
                    "git",
                    "fetch",
                    "--remote",
                    "native-source#alpha",
                    "--tag",
                    "v1.0",
                ],
            );
            let physical = physical_remote(&client, "alpha", "native-source");
            assert_eq!(
                git(
                    &git_dir,
                    &[
                        "cat-file",
                        "-t",
                        &format!("refs/jj/remote-tags/{physical}/v1.0#alpha")
                    ]
                )
                .trim(),
                "tag"
            );
            assert_eq!(
                git(
                    &git_dir,
                    &[
                        "rev-parse",
                        &format!("refs/jj/remote-tags/{physical}/v1.0#alpha^{{}}")
                    ]
                )
                .trim(),
                commit_id(
                    &client,
                    "remote_tags(exact:\"v1.0#alpha\", exact:\"native-source\")"
                )
            );
        }
    }
}

#[test]
fn signed_scoped_tag_preflight_prevents_sibling_branch_publication() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha);
    add_routed_link(&client, "beta", &beta);
    fs::write(client.join("alpha/value.txt"), "must not publish\n").unwrap();
    jjosh(&client, &["describe", "-m", "signed release"]);
    jjosh(&client, &["bookmark", "set", "main#alpha"]);
    let canonical = commit_id(&client, "@");
    let git_dir = PathBuf::from(
        String::from_utf8(jjosh(&client, &["git", "root"]).stdout)
            .unwrap()
            .trim(),
    );
    // Signature-bearing tags are refused without attempting cryptographic
    // validation. In particular, malformed armor must not become unsigned.
    let signed = tag_object(
        &git_dir,
        &canonical,
        "commit",
        "inner",
        "signed release\n-----BEGIN SSH SIGNATURE-----\ninvalid\n-----END SSH SIGNATURE-----\n",
    );
    let outer = tag_object(
        &git_dir,
        &signed,
        "tag",
        "v1.0#beta",
        "outer unsigned annotation\n",
    );
    git(&git_dir, &["update-ref", "refs/tags/v1.0#beta", &outer]);
    jjosh(&client, &["git", "import"]);
    let refs = git(&git_dir, &["show-ref"]);
    let operation = operation_id(&client);
    let alpha_refs = git(&alpha, &["show-ref"]);
    let beta_refs = git(&beta, &["show-ref"]);
    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "main#alpha",
            "--tag",
            "v1.0#beta",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(git(&git_dir, &["show-ref"]), refs);
    assert_eq!(operation_id(&client), operation);
}

#[test]
fn annotation_only_remote_replacement_requires_a_fresh_tag_lease() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, tip) = create_remote(temp.path(), "alpha");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &source);
    let original = tag_object(&source, &tip, "commit", "v1.0", "original annotation\n");
    git(&source, &["update-ref", "refs/tags/v1.0", &original]);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "alpha-upstream#alpha",
            "--tag",
            "v1.0",
        ],
    );
    let concurrent = tag_object(&source, &tip, "commit", "v1.0", "concurrent annotation\n");
    git(
        &source,
        &["update-ref", "refs/tags/v1.0", &concurrent, &original],
    );
    assert_eq!(git(&source, &["rev-parse", "v1.0^{}"]).trim(), tip);
    jjosh(&client, &["new", "-m", "replace release"]);
    fs::write(client.join("alpha/value.txt"), "new release\n").unwrap();
    jjosh(&client, &["tag", "set", "--allow-move", "v1.0#alpha"]);
    let git_dir = PathBuf::from(
        String::from_utf8(jjosh(&client, &["git", "root"]).stdout)
            .unwrap()
            .trim(),
    );
    let refs = git(&git_dir, &["show-ref"]);
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(&client, &["git", "push", "--tag", "v1.0#alpha"]);
    assert!(!rejected.status.success());
    assert_eq!(
        git(&source, &["rev-parse", "refs/tags/v1.0"]).trim(),
        concurrent
    );
    assert_eq!(git(&git_dir, &["show-ref"]), refs);
    assert_eq!(operation_id(&client), operation);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "alpha-upstream#alpha",
            "--tag",
            "v1.0",
        ],
    );
    jjosh(&client, &["git", "push", "--tag", "v1.0#alpha"]);
    assert_eq!(
        git(&source, &["show", "v1.0:src/value.txt"]),
        "new release\n"
    );
}

#[test]
fn signed_transformed_fetch_does_not_grant_sibling_observations_or_leases() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, tip) = create_remote(temp.path(), "alpha");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &source);
    let git_dir = PathBuf::from(
        String::from_utf8(jjosh(&client, &["git", "root"]).stdout)
            .unwrap()
            .trim(),
    );
    let signed = tag_object(
        &source,
        &tip,
        "commit",
        "v1.0",
        "signed\n-----BEGIN PGP SIGNATURE-----\ninvalid\n-----END PGP SIGNATURE-----\n",
    );
    git(&source, &["update-ref", "refs/tags/v1.0", &signed]);
    git(&source, &["update-ref", "refs/heads/sibling", &tip]);
    let refs = git(&git_dir, &["show-ref"]);
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "alpha-upstream#alpha",
            "--branch",
            "sibling",
            "--tag",
            "v1.0",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(git(&git_dir, &["show-ref"]), refs);
    assert_eq!(operation_id(&client), operation);
    assert_eq!(
        git(&source, &["rev-parse", "refs/tags/v1.0"]).trim(),
        signed
    );
    // The same signed object remains legal on an ordinary unscoped remote.
    jjosh(
        &client,
        &["git", "remote", "add", "ordinary", source.to_str().unwrap()],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "ordinary", "--tag", "v1.0"],
    );
    jjosh(&client, &["tag", "track", "v1.0@ordinary"]);
    jjosh(&client, &["git", "export"]);
    assert_eq!(
        git(&git_dir, &["rev-parse", "refs/tags/v1.0"]).trim(),
        signed
    );
}
