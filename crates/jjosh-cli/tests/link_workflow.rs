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

fn assert_path_only_source_history(client: &Path, revision: &str, mount: &str) {
    let source = commit_id(client, revision);
    assert!(!source.is_empty());
    assert_eq!(
        commit_id(
            client,
            &format!("({revision}) & untracked_remote_bookmarks()")
        ),
        source
    );
    let commits = git(client, &["rev-list", &source]);
    for commit in commits.lines() {
        assert_eq!(
            git(client, &["ls-tree", "-r", "--name-only", commit]),
            format!("{mount}/value.txt\n"),
            "source history {revision} contains unrelated paths at {commit}"
        );
    }
}

fn assert_native_marker(client: &Path, remote: &str, branch: &str, expected: &str) {
    let revision = format!("remote_bookmarks(\"{branch}\", remote=\"{remote}\")");
    assert_eq!(commit_id(client, &revision), expected);
    assert_eq!(
        git(
            client,
            &["rev-parse", &format!("refs/remotes/{remote}/{branch}")]
        )
        .trim(),
        expected
    );
    assert_eq!(
        commit_id(client, &format!("({revision}) & immutable()")),
        expected
    );
}

#[test]
fn native_link_histories_keep_clean_trunk_and_restack_patches_across_links() {
    let temp = tempfile::tempdir().unwrap();
    let (_alpha_work, alpha_bare, alpha_v1) = create_remote(temp.path(), "alpha");
    let (beta_work, beta_bare, _) = create_remote(temp.path(), "beta");
    let client = temp.path().join("client");
    fs::create_dir_all(client.join("alpha")).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root-scaffold\n").unwrap();
    // Put linked local content in the Git root, not just a later overlay: no
    // ancestor of either source or the clean trunk may inherit this path.
    fs::write(client.join("alpha/preexisting.txt"), "inherited-local\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root with local linked content"]);
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
        ],
    );
    let first_trunk = commit_id(&client, "trunk@jjosh");
    let alpha_source = commit_id(&client, "main@link-alpha");
    assert_ne!(alpha_source, alpha_v1);
    assert_path_only_source_history(&client, "main@link-alpha", "alpha");
    let inherited_change = change_id(&client, "@");
    jjosh(&client, &["bookmark", "set", "inherited-patch", "-r", "@"]);
    jjosh(&client, &["new"]);
    fs::write(client.join("alpha/local.txt"), "alpha-local-patch\n").unwrap();
    fs::write(client.join("root.txt"), "root-local-patch\n").unwrap();
    jjosh(&client, &["describe", "-m", "local alpha and root changes"]);
    let local_change = change_id(&client, "@");
    let old_local_commit = commit_id(&client, "@");
    jjosh(&client, &["bookmark", "set", "local-patch", "-r", "@"]);

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
        ],
    );
    let second_trunk = commit_id(&client, "trunk@jjosh");
    let beta_source = commit_id(&client, "main@link-beta");
    assert_ne!(second_trunk, first_trunk);
    assert_ne!(commit_id(&client, "@"), old_local_commit);
    assert_eq!(change_id(&client, "@"), local_change);
    assert_eq!(change_id(&client, "inherited-patch"), inherited_change);
    assert_eq!(change_id(&client, "local-patch"), local_change);
    assert_eq!(commit_id(&client, "main@link-alpha"), alpha_source);
    assert_path_only_source_history(&client, "main@link-beta", "beta");
    assert_eq!(
        commit_id(&client, "(inherited-patch | local-patch) & immutable()"),
        ""
    );
    assert_eq!(
        git(&client, &["ls-tree", "-r", "--name-only", &second_trunk]),
        "alpha/.link.josh\nalpha/value.txt\nbeta/.link.josh\nbeta/value.txt\nroot.txt\n"
    );
    assert_eq!(
        git(&client, &["show", &format!("{second_trunk}:root.txt")]),
        "root-scaffold\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("alpha/preexisting.txt")).unwrap(),
        "inherited-local\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("alpha/local.txt")).unwrap(),
        "alpha-local-patch\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("root.txt")).unwrap(),
        "root-local-patch\n"
    );

    fs::write(beta_work.join("src/value.txt"), "beta-v2\n").unwrap();
    git(&beta_work, &["commit", "-am", "beta-v2"]);
    git(
        &beta_work,
        &["push", beta_bare.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    let beta_v2 = git(&beta_work, &["rev-parse", "HEAD"]).trim().to_owned();
    let before_update = commit_id(&client, "@");
    jjosh(&client, &["link", "update", "beta"]);
    let trunk = commit_id(&client, "trunk@jjosh");
    let updated_beta_source = commit_id(&client, "main@link-beta");
    assert_ne!(trunk, second_trunk);
    assert_ne!(updated_beta_source, beta_source);
    assert_ne!(updated_beta_source, beta_v2);
    assert_ne!(commit_id(&client, "@"), before_update);
    assert_eq!(commit_id(&client, "main@link-alpha"), alpha_source);
    assert_eq!(change_id(&client, "@"), local_change);
    assert_eq!(change_id(&client, "inherited-patch"), inherited_change);
    assert_eq!(change_id(&client, "local-patch"), local_change);
    assert_eq!(commit_id(&client, "local-patch"), commit_id(&client, "@"));
    assert_eq!(
        fs::read_to_string(client.join("alpha/preexisting.txt")).unwrap(),
        "inherited-local\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("alpha/local.txt")).unwrap(),
        "alpha-local-patch\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("root.txt")).unwrap(),
        "root-local-patch\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("beta/value.txt")).unwrap(),
        "beta-v2\n"
    );
    assert_eq!(
        git(&client, &["ls-tree", "-r", "--name-only", &trunk]),
        "alpha/.link.josh\nalpha/value.txt\nbeta/.link.josh\nbeta/value.txt\nroot.txt\n"
    );
    assert_eq!(
        git(&client, &["show", &format!("{trunk}:alpha/value.txt")]),
        "alpha-v1\n"
    );
    assert_eq!(
        git(&client, &["show", &format!("{trunk}:beta/value.txt")]),
        "beta-v2\n"
    );
    assert_eq!(
        git(&client, &["show", &format!("{trunk}:root.txt")]),
        "root-scaffold\n"
    );
    assert!(git(&client, &["show", &format!("{trunk}:alpha/.link.josh")]).contains(&alpha_v1));
    assert!(git(&client, &["show", &format!("{trunk}:beta/.link.josh")]).contains(&beta_v2));
    assert_eq!(commit_id(&client, "remote_bookmarks(remote=jjosh)"), trunk);
    assert_eq!(commit_id(&client, "trunk()"), trunk);
    assert_eq!(commit_id(&client, "trunk@jjosh & ::@"), trunk);
    assert_eq!(
        commit_id(&client, "(inherited-patch | local-patch) & immutable()"),
        ""
    );
    assert_eq!(
        commit_id(&client, "inherited-patch & ::local-patch"),
        commit_id(&client, "inherited-patch")
    );
    for ancestor in git(&client, &["rev-list", &trunk]).lines() {
        let paths = git(&client, &["ls-tree", "-r", "--name-only", ancestor]);
        assert!(
            !paths
                .lines()
                .any(|path| matches!(path, "alpha/preexisting.txt" | "alpha/local.txt"))
        );
        if paths.lines().any(|path| path == "root.txt") {
            assert_eq!(
                git(&client, &["show", &format!("{ancestor}:root.txt")]),
                "root-scaffold\n"
            );
        }
    }
    assert_path_only_source_history(&client, "main@link-alpha", "alpha");
    assert_path_only_source_history(&client, "main@link-beta", "beta");
    assert_eq!(commit_id(&client, "main@link-alpha & ::main@link-beta"), "");

    // Both native view and Git backing refs must survive normal import and a
    // fresh process; source markers must never silently become tracked.
    jjosh(&client, &["status"]);
    jjosh(&client, &["git", "import"]);
    jjosh(&client, &["status"]);
    assert_native_marker(&client, "link-alpha", "main", &alpha_source);
    assert_native_marker(&client, "link-beta", "main", &updated_beta_source);
    assert_native_marker(&client, "jjosh", "trunk", &trunk);
    assert_eq!(
        commit_id(
            &client,
            "untracked_remote_bookmarks(main, remote=link-alpha)"
        ),
        alpha_source
    );
    assert_eq!(
        commit_id(
            &client,
            "untracked_remote_bookmarks(main, remote=link-beta)"
        ),
        updated_beta_source
    );
    jjosh(&client, &["bookmark", "track", "main@link-alpha"]);
    assert_eq!(
        commit_id(&client, "main@link-alpha & immutable()"),
        alpha_source
    );
    assert_no_divergent_changes(&client);
    assert!(
        !jjosh_unchecked(
            &client,
            &["describe", "-r", "trunk@jjosh", "-m", "forbidden"]
        )
        .status
        .success()
    );
    assert!(
        !jjosh_unchecked(
            &client,
            &["describe", "-r", "main@link-alpha", "-m", "forbidden"]
        )
        .status
        .success()
    );
    jjosh(
        &client,
        &["describe", "-r", "local-patch", "-m", "still editable"],
    );
    assert_eq!(change_id(&client, "local-patch"), local_change);
}

#[test]
fn native_marker_remotes_refuse_direct_git_transport() {
    let temp = tempfile::tempdir().unwrap();
    let (_source_work, source_bare, source_v1) = create_remote(temp.path(), "source");
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
            "vendor/deps",
            source_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
            "--push-url",
            source_bare.to_str().unwrap(),
        ],
    );
    let source_revision = "remote_bookmarks(main, remote=\"link-vendor%2Fdeps\")";
    let source = commit_id(&client, source_revision);
    let trunk = commit_id(&client, "trunk@jjosh");
    assert_path_only_source_history(&client, source_revision, "vendor/deps");
    assert_native_marker(&client, "link-vendor%2Fdeps", "main", &source);
    fs::write(client.join("vendor/deps/value.txt"), "local-composite\n").unwrap();
    jjosh(
        &client,
        &["describe", "-m", "composite must not be published directly"],
    );
    jjosh(&client, &["bookmark", "set", "composite", "-r", "@"]);
    let refs_before = git(&source_bare, &["show-ref"]);
    for remote in ["link-vendor%2Fdeps", "jjosh"] {
        let rejected = jjosh_unchecked(
            &client,
            &[
                "git",
                "push",
                "--remote",
                remote,
                "--bookmark",
                "composite",
                "--allow-empty-description",
            ],
        );
        assert!(
            !rejected.status.success(),
            "marker remote {remote} accepted a direct push"
        );
        assert_eq!(git(&source_bare, &["show-ref"]), refs_before);
        assert_eq!(
            git(&source_bare, &["rev-parse", "refs/heads/main"]).trim(),
            source_v1
        );
        let rejected_fetch = jjosh_unchecked(&client, &["git", "fetch", "--remote", remote]);
        assert!(
            !rejected_fetch.status.success(),
            "marker remote {remote} accepted a direct fetch"
        );
    }
    jjosh(&client, &["status"]);
    jjosh(&client, &["git", "import"]);
    assert_native_marker(&client, "link-vendor%2Fdeps", "main", &source);
    assert_native_marker(&client, "jjosh", "trunk", &trunk);
    assert_eq!(
        fs::read_to_string(client.join("vendor/deps/value.txt")).unwrap(),
        "local-composite\n"
    );
    // The same revision must be publishable to an ordinary local remote, so a
    // malformed push request cannot satisfy the transport-refusal assertions.
    let control_bare = temp.path().join("control.git");
    git(
        temp.path(),
        &["init", "--bare", control_bare.to_str().unwrap()],
    );
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "control",
            control_bare.to_str().unwrap(),
        ],
    );
    jjosh(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "control",
            "--bookmark",
            "composite",
            "--allow-empty-description",
        ],
    );
    assert_eq!(
        git(&control_bare, &["rev-parse", "refs/heads/composite"]).trim(),
        commit_id(&client, "composite")
    );
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
    let initial_trunk = commit_id(&client, "trunk@jjosh");
    assert_eq!(commit_id(&client, "trunk()"), initial_trunk);
    assert_eq!(
        fs::read_to_string(client.join("deps/lib.txt")).unwrap(),
        "linked-v1\n"
    );
    assert!(!client.join("deps/outside.txt").exists());
    let link_file = fs::read_to_string(client.join("deps/.link.josh")).unwrap();
    assert!(link_file.contains("mode=\"embedded\""));
    assert!(link_file.contains(&format!("push=\"{}\"", remote_bare.display())));
    assert!(link_file.contains("push-target=\"main\""));
    assert_eq!(
        git(&client, &["show", &format!("{initial_trunk}:deps/lib.txt")]),
        "linked-v1\n"
    );

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
    jjosh(&client, &["link", "update", "deps"]);
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
    jjosh(&client, &["link", "update", "deps", "-r", "@-"]);
    assert_ne!(commit_id(&client, "trunk@jjosh"), initial_trunk);
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
    assert_no_divergent_changes(&client);

    fs::write(client.join("deps/lib.txt"), "local-v3\n").unwrap();
    jjosh(&client, &["status"]);
    let preflight_operation = operation_id(&client);
    jjosh(&client, &["link", "push", "deps", "--dry-run"]);
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
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/blocked"]).trim(),
        unrelated_commit.trim()
    );
    jjosh(
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
    jjosh(&client, &["link", "update", "deps", "-r", "trunk@jjosh"]);
    assert_eq!(change_id(&client, "@"), local_change_id);
    let conflicted_status = String::from_utf8(jjosh(&client, &["status"]).stdout).unwrap();
    assert!(conflicted_status.contains("conflict"));

    let rejected_push = jjosh_unchecked(
        &client,
        &["link", "push", "deps", "--to", "rejected-conflict"],
    );
    assert!(!rejected_push.status.success());
    let rejected_ref = run(
        &remote_bare,
        Path::new("git"),
        &["show-ref", "--verify", "refs/heads/rejected-conflict"],
    );
    assert!(!rejected_ref.status.success());
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
    jjosh(&client, &["link", "push", "deps", "--to", "unchanged"]);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/unchanged"]),
        source_tip
    );
    fs::write(client.join("deps/value.txt"), "local change\n").unwrap();
    jjosh(&client, &["describe", "-m", "Actual change"]);
    jjosh(
        &client,
        &["link", "push", "deps", "-r", "@", "--to", "expected"],
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
        &["link", "push", "deps", "--to", "topic", "--dry-run"],
    );
    assert_eq!(git(&remote_bare, &["show-ref"]), before_refs);
    jjosh(&client, &["link", "push", "deps", "--to", "topic"]);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/topic"]).trim(),
        expected_tip
    );
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/topic:src/value.txt"]),
        "local change\n"
    );
    assert_eq!(commit_id(&client, "@"), empty_tip);
    assert_eq!(operation_id(&client), before_operation);

    // Explicit selection applies the same content-based pruning.
    jjosh(
        &client,
        &["link", "push", "deps", "-r", "@", "--to", "explicit"],
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/explicit"]).trim(),
        expected_tip
    );

    // A description cannot make an empty exported change meaningful.
    jjosh(&client, &["describe", "-m", "Release checkpoint"]);
    jjosh(&client, &["link", "push", "deps", "--to", "checkpoint"]);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/checkpoint"]).trim(),
        expected_tip
    );

    // Out-of-scope changes and originally empty commits are equivalent:
    // neither contributes a commit to this link's published history.
    jjosh(&client, &["new", "-m", "Only change another project"]);
    fs::write(client.join("outside.txt"), "unrelated local content\n").unwrap();
    jjosh(&client, &["link", "push", "deps", "--to", "outside"]);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/outside"]).trim(),
        expected_tip
    );
    jjosh(&client, &["new", "-m", "Next actual change"]);
    fs::write(client.join("deps/value.txt"), "next local change\n").unwrap();
    jjosh(&client, &["new"]);
    let local_tip = commit_id(&client, "@");
    let local_operation = operation_id(&client);
    jjosh(
        &client,
        &["link", "push", "deps", "-r", "@", "--to", "continued"],
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
    assert_eq!(operation_id(&client), local_operation);
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
    jjosh(&client, &["link", "push", "deps", "--to", "left"]);
    assert_eq!(git(&remote_bare, &["show", "left:src/left.txt"]), "left\n");
    assert_eq!(
        git(&remote_bare, &["show", "-s", "--format=%B", "left"]).trim(),
        ""
    );
    let left = commit_id(&client, "@");
    jjosh(&client, &["new", "trunk@jjosh", "-m", "Right change"]);
    fs::write(client.join("deps/right.txt"), "right\n").unwrap();
    let right = commit_id(&client, "@");
    jjosh(&client, &["new", &left, &right]);
    jjosh(&client, &["link", "push", "deps", "--to", "merged"]);
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
    jjosh(&client, &["new", "trunk@jjosh", "-m", "Empty side branch"]);
    let empty_branch = commit_id(&client, "@");
    jjosh(&client, &["new", &left, &empty_branch]);
    jjosh(&client, &["link", "push", "deps", "--to", "collapsed"]);
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
            "--push-target",
            "main",
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published-v1\n").unwrap();
    jjosh(&client, &["status"]);
    let local_change = change_id(&client, "@");
    jjosh(&client, &["link", "push", "deps"]);
    let first_tip = git(&remote_bare, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "published-v1\n"
    );
    jjosh(&client, &["link", "push", "deps", "--to", "topic"]);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/topic"]).trim(),
        first_tip
    );

    fs::write(client.join("deps/value.txt"), "published-v2\n").unwrap();
    jjosh(&client, &["status"]);
    assert_eq!(change_id(&client, "@"), local_change);
    let preflight_operation = operation_id(&client);
    jjosh(&client, &["link", "push", "deps", "--dry-run"]);
    assert_eq!(operation_id(&client), preflight_operation);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]).trim(),
        first_tip
    );
    // A dry-run must leave the lease at the actually published v1.
    jjosh(&client, &["link", "push", "deps"]);
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
    jjosh(&client, &["link", "push", "deps", "--to", "topic"]);
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
    jjosh(&client, &["link", "push", "deps"]);
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
            "--push-target",
            "main",
        ],
    );
    jjosh(&client, &["new"]);
    fs::write(client.join("deps/value.txt"), "published\n").unwrap();
    jjosh(&client, &["status"]);
    let local_change = change_id(&client, "@");
    jjosh(&client, &["link", "push", "deps"]);
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
    let preflight_operation = operation_id(&client);
    let rejected_preflight = jjosh_unchecked(&client, &["link", "push", "deps", "--dry-run"]);
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
    let rejected_push = jjosh_unchecked(&client, &["link", "push", "deps"]);
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
    let rejected_retry = jjosh_unchecked(&client, &["link", "push", "deps"]);
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
    jjosh(&client, &["link", "push", "deps"]);
    assert_eq!(
        git(&remote_bare, &["show", "refs/heads/main:src/value.txt"]),
        "local-rewrite\n"
    );
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
    let alpha_source = commit_id(&client, "main@link-alpha");
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
    let composition_commit = commit_id(&client, "trunk@jjosh");
    assert_eq!(commit_id(&client, "main@link-alpha"), alpha_source);
    assert_eq!(
        commit_id(&client, "remote_bookmarks(remote=jjosh)"),
        composition_commit
    );
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
    let updated_commit = commit_id(&client, "trunk@jjosh");
    assert_ne!(updated_commit, composition_commit);
    assert_eq!(
        git(
            &client,
            &["show", &format!("{updated_commit}:alpha/value.txt")]
        ),
        "alpha-v2\n"
    );
    assert_eq!(
        git(
            &client,
            &["show", &format!("{updated_commit}:beta/value.txt")]
        ),
        "beta-v2\n"
    );
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
    let composition_commit = commit_id(&client, "trunk@jjosh");
    assert_eq!(commit_id(&client, "@ & mutable()"), commit_id(&client, "@"));
    assert_eq!(commit_id(&client, "trunk@jjosh & ::@"), composition_commit);
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
fn legacy_embed_graph_requires_migration_before_adding_link() {
    let temp = tempfile::tempdir().unwrap();
    let (_alpha_work, alpha_bare, _) = create_remote(temp.path(), "legacy-alpha");
    let (_beta_work, beta_bare, _) = create_remote(temp.path(), "legacy-beta");
    let client = temp.path().join("client");
    fs::create_dir(&client).unwrap();
    git(&client, &["init", "-b", "main"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("root.txt"), "root\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "root"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    let root = commit_id(&client, "@");
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
        ],
    );

    // Recreate the old Embed commit shape: the materialized tree is unchanged,
    // but the clean commit has the scaffold and source as ordinary parents and
    // no native baseline marker.
    let native = commit_id(&client, "trunk@jjosh");
    let source = commit_id(&client, "main@link-alpha");
    let tree = git(&client, &["rev-parse", &format!("{native}^{{tree}}")])
        .trim()
        .to_owned();
    let legacy = git_with_input(
        &client,
        &["commit-tree", &tree, "-p", &root, "-p", &source],
        b"Add embedded Josh link alpha\n",
    )
    .trim()
    .to_owned();
    git(&client, &["update-ref", "refs/heads/legacy", &legacy]);
    jjosh(&client, &["git", "import"]);
    jjosh(&client, &["edit", &legacy]);
    jjosh(
        &client,
        &["bookmark", "forget", "--include-remotes", "trunk", "main"],
    );

    jjosh(&client, &["new", "-m", "legacy local patch"]);
    fs::write(client.join("alpha/local.txt"), "keep this patch\n").unwrap();
    let rejected = jjosh_unchecked(
        &client,
        &[
            "link",
            "add",
            "beta",
            beta_bare.to_str().unwrap(),
            ":/src",
            "--target",
            "main",
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("run `jjosh link migrate` before changing them"),
        "unexpected link-add error:\n{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    jjosh(&client, &["link", "migrate"]);
    let trunk = commit_id(&client, "trunk@jjosh");
    assert_eq!(
        fs::read_to_string(client.join("alpha/local.txt")).unwrap(),
        "keep this patch\n"
    );
    assert!(
        !git(&client, &["ls-tree", "-r", "--name-only", &trunk])
            .lines()
            .any(|path| path == "alpha/local.txt")
    );

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
        ],
    );
    assert!(!commit_id(&client, "trunk@jjosh").is_empty());
    assert!(!commit_id(&client, "main@link-beta").is_empty());
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
    let composition_commit = commit_id(&client, "trunk@jjosh");
    assert_eq!(
        git(
            &client,
            &["show", &format!("{composition_commit}:deps/value.txt")]
        ),
        "migration-v1\n"
    );
    assert_eq!(commit_id(&client, "@ & mutable()"), commit_id(&client, "@"));
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
fn embedded_update_preserves_source_change_ids_across_other_link_additions() {
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
    let source_root = commit_id(&client, "main@link-deps");
    assert_eq!(change_id(&client, "main@link-deps"), source_change_id);
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
    assert_eq!(commit_id(&client, "main@link-deps"), source_root);
    let local_change = change_id(&client, "@");

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

    jjosh(&client, &["link", "update", "deps"]);
    assert_eq!(change_id(&client, "@"), local_change);
    assert_eq!(commit_id(&client, source_change_id), source_root);
    assert_eq!(
        commit_id(&client, &format!("{source_change_id} & ::main@link-deps")),
        source_root
    );
    assert_eq!(
        fs::read_to_string(client.join("deps/value.txt")).unwrap(),
        "evolution-v2\n"
    );
    assert_path_only_source_history(&client, "main@link-deps", "deps");
    assert_no_divergent_changes(&client);
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
    jjosh(&client, &["link", "push", "deps"]);
    jjosh(&client, &["link", "push", "deps", "--to", "topic"]);
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
    assert!(
        !jjosh_unchecked(&client, &["link", "push", "deps"])
            .status
            .success()
    );

    jjosh(&client, &["link", "update", "deps"]);
    assert_eq!(change_id(&client, "@"), local_change);
    assert_eq!(
        fs::read_to_string(client.join("deps/pending.txt")).unwrap(),
        "new local change\n"
    );
    jjosh(&client, &["link", "push", "deps"]);
    assert_eq!(
        git(&remote_bare, &["show", "main:src/pending.txt"]),
        "new local change\n"
    );
    assert_eq!(
        git(&remote_bare, &["show", "main:outside.txt"]),
        "new hidden upstream content\n"
    );
    // Fetching main must not authorize overwriting the independently changed topic.
    assert!(
        !jjosh_unchecked(&client, &["link", "push", "deps", "--to", "topic"])
            .status
            .success()
    );
    assert_eq!(
        git(&remote_bare, &["rev-parse", "topic"]).trim(),
        observed.trim()
    );
    assert_no_divergent_changes(&client);
}
