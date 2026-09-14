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
            &["--ignore-working-copy", "op", "log", "--limit", "1", "--no-graph", "-T", "id"],
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

fn import_project(
    client: &Path,
    project: &str,
    mount: &str,
    source: &Path,
    filter: &str,
    writable: bool,
) {
    let remote = format!("{project}-upstream");
    jjosh(client, &["project", "add", project, "--path", mount]);
    let mut args = vec![
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
    if writable {
        args.push("--writable");
    }
    jjosh(client, &args);
    jjosh(
        client,
        &["git", "fetch", "--remote", &remote, "--branch", "main"],
    );
    jjosh(client, &["new", "@", &format!("main#{project}@{remote}")]);
}

fn file_at_revision(client: &Path, revision: &str, path: &str) -> Vec<u8> {
    jjosh(client, &["file", "show", "-r", revision, path]).stdout
}

#[test]
fn project_registration_restores_without_snapshotting_or_claiming_literal_refs() {
    let temp = tempfile::tempdir().unwrap();
    let client = create_client(temp.path(), false);
    jjosh(&client, &["bookmark", "create", "topic#api"]);
    let before = operation_id(&client);
    let rejected = jjosh_unchecked(&client, &["project", "add", "api", "--path", "packages/api"]);
    assert!(!rejected.status.success());
    assert_eq!(operation_id(&client), before);
    jjosh(&client, &["bookmark", "delete", "topic#api"]);
    let original_commit = commit_id(&client, "@");
    let before_registration = operation_id(&client);
    fs::write(client.join("unrecorded.txt"), "pending local work\n").unwrap();
    jjosh(&client, &["project", "add", "api", "--path", "packages/api"]);
    assert_ne!(operation_id(&client), before_registration);
    assert_eq!(
        String::from_utf8(jjosh(&client, &["--ignore-working-copy", "log", "-r", "@",
            "--no-graph", "-T", "commit_id"]).stdout).unwrap().trim(),
        original_commit,
    );
    jjosh(&client, &["project", "show", "api"]);
    jjosh(&client, &["--ignore-working-copy", "op", "restore", before_registration.trim(), "--what", "repo"]);
    assert!(!jjosh_unchecked(&client, &["project", "show", "api"]).status.success());
    assert_eq!(fs::read_to_string(client.join("unrecorded.txt")).unwrap(), "pending local work\n");
}

#[test]
fn display_rename_preserves_project_publication_label() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "packages/api", &source, ":/src", true);
    jjosh(&client, &["project", "rename", "api", "service"]);
    jjosh(&client, &["project", "show", "service"]);
    assert!(!jjosh_unchecked(&client, &["project", "show", "api"]).status.success());
    fs::write(client.join("packages/api/value.txt"), "renamed project edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit renamed project"]);
    jjosh(&client, &["git", "push", "--remote", "api-upstream",
        "--named", "topic#api=@", "--allow-empty-description"]);
    assert_eq!(git(&source, &["show", "topic:src/value.txt"]), "renamed project edit\n");
    assert_eq!(git(&source, &["show", "topic:outside.txt"]), "source-outside\n");
}

#[test]
fn recreated_remote_rejects_restored_observations_from_its_previous_instance() {
    let temp = tempfile::tempdir().unwrap();
    let (_, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "packages/api", &source, ":/src", false);
    jjosh(&client, &["bookmark", "create", "local#api"]);
    let local = commit_id(&client, "local#api");
    let historical = operation_id(&client);
    jjosh(&client, &["git", "remote", "remove", "api-upstream"]);
    jjosh(&client, &["git", "remote", "add", "api-upstream", source.to_str().unwrap(),
        "--project", "api", "--whole"]);
    jjosh(&client, &["op", "restore", historical.trim(), "--what", "remote-tracking"]);
    let restored = operation_id(&client);
    assert!(!jjosh_unchecked(&client, &["git", "fetch", "--remote", "api-upstream",
        "--branch", "main"]).status.success());
    assert_eq!(operation_id(&client), restored);
    jjosh(&client, &["git", "remote", "forget-observations", "api-upstream"]);
    assert_eq!(commit_id(&client, "local#api"), local);
    jjosh(&client, &["git", "fetch", "--remote", "api-upstream", "--branch", "main"]);
    assert_eq!(file_at_revision(&client, "main#api@api-upstream", "packages/api/src/value.txt"), b"source-v1\n");
    jjosh(&client, &["op", "restore", historical.trim(), "--what", "repo"]);
    assert!(!jjosh_unchecked(&client, &["git", "fetch", "--remote", "api-upstream",
        "--branch", "main"]).status.success());
}

#[test]
fn base_free_publication_uses_unique_ancestry_evidence_and_rejects_ambiguous_raw_contexts() {
    let temp = tempfile::tempdir().unwrap();
    let (work, source, original) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "api", "--path", "api"]);
    jjosh(&client, &["git", "remote", "add", "source", source.to_str().unwrap(),
        "--project", "api", "--filter", ":/src", "--writable"]);
    jjosh(&client, &["git", "fetch", "--remote", "source", "--branch", "main"]);
    jjosh(&client, &["new", "main#api@source", "-m", "new topic"]);
    fs::write(client.join("api/value.txt"), "topic edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "new topic"]);
    jjosh(&client, &["git", "push", "--remote", "source", "--named", "topic#api=@"]);
    assert_eq!(git(&source, &["rev-parse", "topic^"]).trim(), original);
    assert_eq!(git(&source, &["show", "topic:outside.txt"]), "source-outside\n");

    fs::write(work.join("outside.txt"), "different raw context\n").unwrap();
    git(&work, &["commit", "-am", "outside-only branch"]);
    git(&work, &["push", source.to_str().unwrap(), "HEAD:variant"]);
    jjosh(&client, &["git", "fetch", "--remote", "source", "--branch", "variant"]);
    assert_eq!(commit_id(&client, "main#api@source"), commit_id(&client, "variant#api@source"));
    jjosh(&client, &["new", "main#api@source", "-m", "ambiguous sibling"]);
    fs::write(client.join("api/value.txt"), "sibling edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "ambiguous sibling"]);
    assert!(!jjosh_unchecked(&client, &["git", "push", "--remote", "source",
        "--named", "sibling#api=@"]).status.success());
    assert_eq!(git(&source, &["for-each-ref", "--format=%(refname)", "refs/heads/sibling"]), "");
    jjosh(&client, &["git", "push", "--remote", "source",
        "--named", "sibling#api=@", "--base", "main"]);
    assert_eq!(git(&source, &["show", "sibling:outside.txt"]), "source-outside\n");
    assert_eq!(git(&source, &["show", "variant:outside.txt"]), "different raw context\n");
}

#[test]
fn fetch_records_raw_advances_even_when_the_projected_target_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let (work, source, _) = create_remote(temp.path(), "source");
    let client = create_client(temp.path(), false);
    import_project(&client, "api", "api", &source, ":/src", false);
    let canonical = commit_id(&client, "main#api@api-upstream");
    let observed = operation_id(&client);
    fs::write(work.join("outside.txt"), "new outside content\n").unwrap();
    git(&work, &["commit", "-am", "outside-only update"]);
    git(&work, &["push", source.to_str().unwrap(), "main"]);
    jjosh(&client, &["git", "fetch", "--remote", "api-upstream", "--branch", "main"]);
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
    assert_ne!(operation_id(&client), observed);
    jjosh(&client, &["op", "restore", observed.trim(), "--what", "remote-tracking"]);
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
    jjosh(&client, &["git", "fetch", "--remote", "api-upstream", "--branch", "main"]);
    assert_eq!(commit_id(&client, "main#api@api-upstream"), canonical);
    assert_eq!(git(&source, &["show", "main:outside.txt"]), "new outside content\n");
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
    jjosh(&client, &["git", "remote", "add", "source", source.to_str().unwrap(),
        "--project", "api", "--filter", ":/src", "--writable"]);
    let fetched = jjosh(&client, &["git", "fetch", "--remote", "source", "--revision", &raw_tip, "--depth", "1"]);
    let stderr = String::from_utf8(fetched.stderr).unwrap();
    let prefix = format!("Fetched revision {raw_tip} as ");
    let canonical = stderr.lines().find_map(|line| line.strip_prefix(&prefix)).unwrap();
    jjosh(&client, &["new", canonical, "-m", "continue shallow source"]);
    fs::write(client.join("api/value.txt"), "local continuation\n").unwrap();
    jjosh(&client, &["describe", "-m", "continue shallow source"]);
    jjosh(&client, &["bookmark", "create", "topic#api"]);
    let original = commit_id(&client, "topic#api");
    let shallow_operation = operation_id(&client);
    jjosh(&client, &["--config", "git.abandon-unreachable-commits=false",
        "git", "fetch", "--remote", "source", "--revision", &raw_tip, "--unshallow"]);
    jjosh(&client, &["op", "restore", shallow_operation.trim()]);
    assert_eq!(commit_id(&client, "topic#api"), original);
    jjosh(&client, &["git", "push", "--remote", "source",
        "--bookmark", "topic#api", "--base", &raw_tip]);
    assert_eq!(git(&source, &["rev-parse", "topic^"]).trim(), raw_tip);
    assert_eq!(git(&source, &["rev-list", "--count", "topic"]).trim(), "3");
    assert_eq!(git(&source, &["show", "topic:src/value.txt"]), "local continuation\n");
    assert_eq!(git(&source, &["show", "topic:outside.txt"]), "source-outside\n");
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
        &["git", "fetch", "--remote", "source", "--branch", "main"],
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
    git(temp.path(), &["clone", "--bare", flat_work.to_str().unwrap(), flat.to_str().unwrap()]);
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "pkg", "--path", "vendor/pkg"]);
    jjosh(&client, &["git", "remote", "add", "upstream", upstream.to_str().unwrap(),
        "--project", "pkg", "--filter", ":/src", "--base", "main"]);
    jjosh(&client, &["git", "remote", "add", "flat", flat.to_str().unwrap(),
        "--project", "pkg", "--whole", "--writable"]);
    jjosh(&client, &["git", "fetch", "--remote", "upstream", "--branch", "main"]);
    jjosh(&client, &["git", "fetch", "--remote", "flat", "--branch", "main"]);
    assert_eq!(file_at_revision(&client, "main#pkg@upstream", "vendor/pkg/value.txt"), b"upstream-v1\n");
    assert_eq!(file_at_revision(&client, "main#pkg@flat", "vendor/pkg/value.txt"), b"flat source\n");
    jjosh(&client, &["new", "main#pkg@flat", "-m", "edit standalone project"]);
    fs::write(client.join("vendor/pkg/value.txt"), "edited flat\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit standalone project"]);
    jjosh(&client, &["bookmark", "create", "topic#pkg"]);
    jjosh(&client, &["git", "push", "--remote", "flat", "--bookmark", "topic#pkg"]);
    assert_eq!(git(&flat, &["ls-tree", "-r", "--name-only", "topic"]), "value.txt\n");
    assert_eq!(git(&flat, &["show", "topic:value.txt"]), "edited flat\n");
    assert_eq!(git(&upstream, &["show", "main:outside.txt"]), "upstream-outside\n");

    jjosh(&client, &["git", "remote", "remove", "upstream"]);
    jjosh(&client, &["new", "-m", "continue standalone project"]);
    fs::write(client.join("vendor/pkg/value.txt"), "continued flat\n").unwrap();
    jjosh(&client, &["describe", "-m", "continue standalone project"]);
    jjosh(&client, &["bookmark", "set", "topic#pkg"]);
    jjosh(&client, &["git", "push", "--remote", "flat", "--bookmark", "topic#pkg"]);
    assert_eq!(git(&flat, &["ls-tree", "-r", "--name-only", "topic"]), "value.txt\n");
    assert_eq!(git(&flat, &["show", "topic:value.txt"]), "continued flat\n");
}

#[test]
fn reattachment_does_not_turn_an_invalid_read_only_policy_into_write_access() {
    let temp = tempfile::tempdir().unwrap();
    let (_, original, _) = create_remote(temp.path(), "original");
    let client = create_client(temp.path(), false);
    jjosh(&client, &["project", "add", "pkg", "--path", "pkg"]);
    jjosh(
        &client,
        &["git", "remote", "add", "source", original.to_str().unwrap()],
    );
    jjosh(
        &client,
        &["git", "remote", "attach", "source", "--project", "pkg", "--whole"],
    );
    let git_path = String::from_utf8(jjosh(&client, &["git", "root"]).stdout).unwrap();
    let git_path = Path::new(git_path.trim());
    git(
        git_path,
        &["config", "remote.source.jjosh-readOnly", "invalid"],
    );
    let rejected = jjosh_unchecked(
        &client,
        &["git", "remote", "attach", "source", "--project", "pkg", "--whole"],
    );
    assert!(!rejected.status.success());
    let rejected_push = jjosh_unchecked(
        &client,
        &["git", "push", "--remote", "source", "--named", "topic#pkg=@"],
    );
    assert!(!rejected_push.status.success());
    assert_eq!(git(&original, &["for-each-ref", "--format=%(refname)", "refs/heads/topic"]), "");
}

#[test]
fn marker_free_reattachment_preserves_edited_tree_and_full_history() {
    let temp = tempfile::tempdir().unwrap();
    let (work, bare, _) = create_remote(temp.path(), "reattach");
    fs::write(work.join("src/value.txt"), "upstream-v2\n").unwrap();
    git(&work, &["commit", "-am", "upstream-v2"]);
    git(&work, &["push", bare.to_str().unwrap(), "main"]);
    let client = create_client(temp.path(), false);
    import_project(&client, "deps", "vendor/deps", &bare, ":/src", true);
    assert_eq!(
        file_at_revision(&client, "parents(main#deps@deps-upstream)", "vendor/deps/value.txt"),
        b"reattach-v1\n",
    );
    fs::write(client.join("vendor/deps/value.txt"), "maintained edit\n").unwrap();
    jjosh(&client, &["describe", "-m", "maintain imported history"]);
    jjosh(&client, &["new", "-m", "local descendant"]);
    fs::write(client.join("vendor/deps/local.txt"), "local descendant\n").unwrap();
    let local = commit_id(&client, "@");
    let history_args = ["log", "--no-graph", "-r", "::@", "-T", "commit_id ++ \"\\n\""];
    let history = jjosh(&client, &history_args).stdout;
    assert!(!client.join("vendor/deps/.link.josh").exists());

    // Removing the last remote must retain the project's independently recorded mount.
    jjosh(&client, &["git", "remote", "remove", "deps-upstream"]);
    jjosh(
        &client,
        &[
            "git", "remote", "add", "deps-peer", bare.to_str().unwrap(),
            "--filter", ":/src", "--project", "deps", "--base", "main",
        ],
    );
    jjosh(&client, &["git", "fetch", "--remote", "deps-peer", "--branch", "main"]);

    assert_eq!(commit_id(&client, "@"), local);
    assert_eq!(jjosh(&client, &history_args).stdout, history);
    assert_eq!(fs::read(client.join("vendor/deps/value.txt")).unwrap(), b"maintained edit\n");
    assert_eq!(fs::read(client.join("vendor/deps/local.txt")).unwrap(), b"local descendant\n");
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
    jjosh(&client, &["project", "add", "deps", "--path", "vendor/deps"]);
    jjosh(
        &client,
        &[
            "git", "remote", "add", "deps-source", bare.to_str().unwrap(),
            "--filter", ":/src", "--project", "deps", "--base", "main",
        ],
    );
    jjosh(&client, &["git", "fetch", "--remote", "deps-source", "--branch", "main"]);
    assert_eq!(commit_id(&client, "@"), local);
    jjosh(&client, &["new", "@", "main#deps@deps-source"]);
    assert_eq!(fs::read(client.join("vendor/deps/value.txt")).unwrap(), b"unrelated-v1\n");
    assert_eq!(fs::read_to_string(client.join("legacy/.link.josh")).unwrap(), marker);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
    // Historical markers are ordinary tracked files, never remote configuration.
    let redirected_metadata = josh_core::filter::as_file(
        josh_core::filter::parse(":/src:prefix=deps")
            .unwrap()
            .with_meta("name", "deps".to_owned())
            .with_meta("remote", other_bare.to_str().unwrap().to_owned())
            .with_meta("push", other_bare.to_str().unwrap().to_owned())
            .with_meta("target", "redirected".to_owned())
            .with_meta("commit", git(&other_bare, &["rev-parse", "main"]).trim().to_owned())
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
        assert_eq!(
            git(
                &remote_bare,
                &["ls-tree", "-r", "--name-only", &format!("published-{index}")]
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
        import_project(&client, "deps", "deps", &bare, ":/src", false);
        fs::write(client.join("deps/local.txt"), "local work\n").unwrap();
        let local = commit_id(&client, "@");
        let initial = commit_id(&client, "main#deps@deps-upstream");
        // A project without an explicit write policy remains read-only, even though
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
        &["project", "import", "--source",
        &format!("app={}", work.display()),],
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
    let local_before_attachment = commit_id(&client, "@");
    jjosh(
        &client,
        &["git", "remote", "add", "app-upstream", bare.to_str().unwrap()],
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
            "--read-only",
        ],
    );
    jjosh(
        &client,
        &["git", "fetch", "--remote", "app-upstream", "--branch", "main"],
    );
    assert_eq!(commit_id(&client, "@"), local_before_attachment);
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
    import_project(&client, "deps", "vendor/deps", &bare, ":/src", false);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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
    import_project(&client, "deps", "deps", &remote_bare, ":/src", true);
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

#[test]
fn arbitrary_publication_requires_explicit_source_without_reinterpreting_literal_names() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let (_, publication, _) = create_remote(temp.path(), "publication");
    let client = create_client(temp.path(), false);
    for (name, source) in [("alpha", &alpha), ("beta", &beta)] {
        import_project(&client, name, name, source, ":/src", false);
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
    assert!(!jjosh_unchecked(&client, &["git", "push", "--remote", "review+origin",
        "--bookmark", "alpha-topic#alpha", "--allow-empty-description"]).status.success());
    assert_eq!(git(&publication, &["show-ref"]), before);
    for project in ["alpha", "beta"] {
        jjosh(&client, &["git", "push", "--remote", "review+origin",
            "--source", &format!("{project}-upstream"),
            "--bookmark", &format!("{project}-topic#{project}"),
            "--allow-empty-description"]);
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
        commit_id(&client, "alpha-topic#alpha@review+origin"),
        commit_id(&client, "alpha-topic#alpha"),
    );
    assert_eq!(
        commit_id(&client, "beta-topic#beta@review+origin"),
        commit_id(&client, "beta-topic#beta"),
    );

    // One-shot conversion evidence cannot be reinterpreted by a later raw fetch.
    let projected = commit_id(&client, "alpha-topic#alpha@review+origin");
    assert!(!jjosh_unchecked(&client, &["git", "fetch", "--remote", "review+origin"]).status.success());
    assert_eq!(commit_id(&client, "alpha-topic#alpha@review+origin"), projected);
    assert_eq!(commit_id(&client, "alpha-topic#alpha"), projected);
    // An unrelated selected wire ref is still a valid ordinary fetch.
    jjosh(&client, &["git", "fetch", "--remote", "review+origin", "--branch", "main"]);
    assert_eq!(commit_id(&client, "alpha-topic#alpha@review+origin"), projected);

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
            "--source", "alpha-upstream",
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
    assert_eq!(git(&publication, &["show", "unknown#missing:root.txt"]), "never publish the root\n");
    assert_eq!(git(&publication, &["show", "unknown#missing:alpha/value.txt"]), "alpha edit\n");
}

fn add_routed_link(client: &Path, scope: &str, source: &Path, writable: bool) {
    import_project(client, scope, scope, source, ":/src", writable);
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
fn automatic_push_routes_a_wildcard_without_leaking_other_projects() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha, false);
    add_routed_link(&client, "beta", &beta, false);
    // Keep the read-only source filters and bases intact. The independently
    // attached writers have arbitrary names and win over the read-only sources.
    for (scope, remote, source) in [
        ("alpha", "source-one", &alpha),
        ("beta", "source-two", &beta),
    ] {
        jjosh(
            &client,
            &[
                "git",
                "remote",
                "add",
                remote,
                source.to_str().unwrap(),
                "--filter",
                ":/src",
                "--project",
                scope,
                "--push-url",
                source.to_str().unwrap(),
            ],
        );
        jjosh(
            &client,
            &["git", "fetch", "--remote", remote, "--branch", "main"],
        );
        jjosh(
            &client,
            &["bookmark", "track", &format!("main#{scope}@{remote}")],
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
    let preview = jjosh(
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
    let preview = String::from_utf8_lossy(&preview.stderr);
    for (scope, remote) in [("alpha", "source-one"), ("beta", "source-two")] {
        assert!(
            preview.lines().any(|line| {
                line.contains(&format!("main#{scope}"))
                    && line.contains(remote)
                    && line.contains("refs/heads/main")
            }),
            "missing routing for {scope} in:\n{preview}"
        );
    }
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
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
        ("alpha", "source-one", &alpha),
        ("beta", "source-two", &beta),
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
}

#[test]
fn automatic_push_allows_unchanged_readonly_refs_but_preflights_updates() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha, true);
    add_routed_link(&client, "beta", &beta, false);
    fs::write(client.join("alpha/value.txt"), "alpha published\n").unwrap();
    jjosh(&client, &["describe", "-m", "edit writable project"]);
    jjosh(&client, &["bookmark", "set", "main#alpha", "-r", "@"]);
    let beta_refs = git(&beta, &["show-ref"]);
    let operation = operation_id(&client);
    let preview = jjosh(
        &client,
        &[
            "git",
            "push",
            "--bookmark",
            "main#*",
            "--dry-run",
            "--allow-empty-description",
        ],
    );
    let preview = String::from_utf8_lossy(&preview.stderr);
    assert!(
        preview.lines().any(|line| {
            line.contains("main#beta")
                && line.contains("beta-upstream")
                && line.contains("refs/heads/main")
        }),
        "unchanged read-only destination missing:\n{preview}"
    );
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
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(
        git(&alpha, &["show", "main:src/value.txt"]),
        "alpha published\n"
    );

    jjosh(&client, &["new", "-m", "edit both projects"]);
    fs::write(client.join("alpha/value.txt"), "alpha blocked\n").unwrap();
    fs::write(client.join("beta/value.txt"), "beta blocked\n").unwrap();
    jjosh(
        &client,
        &["bookmark", "set", "main#alpha", "main#beta", "-r", "@"],
    );
    let alpha_refs = git(&alpha, &["show-ref"]);
    let operation = operation_id(&client);
    let rejected = jjosh_unchecked(
        &client,
        &[
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
    assert_eq!(operation_id(&client), operation);
}

#[test]
fn automatic_push_rejects_selected_ambiguous_and_missing_routes_only() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, beta, _) = create_remote(temp.path(), "beta");
    let (_, fork, _) = create_remote(temp.path(), "fork");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha, true);
    add_routed_link(&client, "beta", &beta, true);
    jjosh(
        &client,
        &[
            "git",
            "remote",
            "add",
            "second-writer",
            fork.to_str().unwrap(),
        ],
    );
    jjosh(
        &client,
        &["git", "remote", "attach", "second-writer", "--project", "beta",
            "--like", "beta-upstream", "--writable"],
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

    // Neither an ambiguous nor an unknown unselected scope blocks this batch.
    jjosh(
        &client,
        &[
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
    add_routed_link(&client, "alpha", &alpha, true);
    add_routed_link(&client, "beta", &beta, true);
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
    add_routed_link(&client, "alpha", &alpha, true);
    add_routed_link(&client, "beta", &beta, true);
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
fn configured_push_overrides_scope_routes_and_unscoped_refs_use_origin() {
    let temp = tempfile::tempdir().unwrap();
    let (_, alpha, _) = create_remote(temp.path(), "alpha");
    let (_, publication, _) = create_remote(temp.path(), "publication");
    let client = create_client(temp.path(), false);
    add_routed_link(&client, "alpha", &alpha, true);
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
            "--source", "alpha-upstream",
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
    add_routed_link(&client, "alpha", &alpha, true);
    add_routed_link(&client, "beta", &beta, true);
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
            "beta-upstream",
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
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("refs/heads/collision"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(git(&alpha, &["show-ref"]), alpha_refs);
    assert_eq!(git(&beta, &["show-ref"]), beta_refs);
    assert_eq!(operation_id(&client), operation);
}

fn add_tag_project(client: &Path, scope: &str, work: &Path, bare: &Path, native: bool) {
    if native {
        jjosh(work, &["git", "init", "--colocate"]);
        jjosh(
            client,
            &["project", "import", "--source",
            &format!("{scope}={}", work.display()),],
        );
        let remote = format!("{scope}-upstream");
        jjosh(
            client,
            &["git", "remote", "add", &remote, bare.to_str().unwrap()],
        );
        jjosh(client, &["git", "remote", "attach", &remote, "--project", scope,
            "--whole", "--writable"]);
        jjosh(
            client,
            &["git", "fetch", "--remote", &remote, "--branch", "main"],
        );
    } else {
        add_routed_link(client, scope, bare, true);
    }
}

fn tag_object(git_dir: &Path, target: &str, kind: &str, name: &str, message: &str) -> String {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        file.path(),
        format!(
            "object {target}\ntype {kind}\ntag {name}\ntagger Release Author <release@example.com> 1700000000 +0530\n\n{message}"
        ),
    ).unwrap();
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
                    &format!("{scope}-upstream"),
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
                "alpha-upstream",
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
                "alpha-upstream",
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
                "alpha-upstream",
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
                "alpha-upstream",
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
                "--source", "alpha-upstream",
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
                &["git", "remote", "attach", "native-source", "--project", "alpha", "--whole"],
            );
            jjosh(
                &client,
                &["git", "fetch", "--remote", "native-source", "--tag", "v1.0"],
            );
            assert_eq!(
                git(
                    &git_dir,
                    &[
                        "cat-file",
                        "-t",
                        "refs/jj/remote-tags/native-source/v1.0#alpha"
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
                        "refs/jj/remote-tags/native-source/v1.0#alpha^{}"
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
    add_routed_link(&client, "alpha", &alpha, true);
    add_routed_link(&client, "beta", &beta, true);
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
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .to_lowercase()
            .contains("signed")
    );
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
    add_routed_link(&client, "alpha", &source, true);
    let original = tag_object(&source, &tip, "commit", "v1.0", "original annotation\n");
    git(&source, &["update-ref", "refs/tags/v1.0", &original]);
    jjosh(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "alpha-upstream",
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
            "alpha-upstream",
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
    add_routed_link(&client, "alpha", &source, true);
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
            "alpha-upstream",
            "--branch",
            "sibling",
            "--tag",
            "v1.0",
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .to_lowercase()
            .contains("signed")
    );
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
