#![cfg(unix)]

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    temp: tempfile::TempDir,
    clock: Cell<u32>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("home")).unwrap();
        fs::write(
            temp.path().join("config.toml"),
            "[user]\nname = 'Projection Test'\nemail = 'projection@example.com'\n",
        )
        .unwrap();
        Self {
            temp,
            clock: Cell::new(0),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.temp.path().join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    fn run(&self, cwd: &Path, program: &Path, args: &[&str]) -> Output {
        let tick = self.clock.get();
        self.clock.set(tick + 1);
        Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.temp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("home"))
            .env("JJ_CONFIG", self.temp.path().join("config.toml"))
            .env("JJ_TIMESTAMP", "2001-01-01T00:00:00+00:00")
            .env("JJ_OP_TIMESTAMP", "2001-01-01T00:00:00+00:00")
            .env("JJ_RANDOMNESS_SEED", tick.to_string())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Projection Test")
            .env("GIT_AUTHOR_EMAIL", "projection@example.com")
            .env("GIT_COMMITTER_NAME", "Projection Test")
            .env("GIT_COMMITTER_EMAIL", "projection@example.com")
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00+00:00")
            .env("LANG", "C.UTF-8")
            .output()
            .unwrap_or_else(|err| panic!("failed to run {program:?} {args:?}: {err}"))
    }

    fn success(&self, output: Output, args: &[&str]) -> String {
        assert!(
            output.status.success(),
            "{args:?}: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.success(self.run(cwd, Path::new("git"), args), args)
    }

    fn jj_unchecked(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut full_args = vec!["--no-pager", "--color=never"];
        full_args.extend_from_slice(args);
        self.run(cwd, Path::new(env!("CARGO_BIN_EXE_jjosh")), &full_args)
    }

    fn jj(&self, cwd: &Path, args: &[&str]) -> String {
        self.success(self.jj_unchecked(cwd, args), args)
    }

    fn log(&self, cwd: &Path, revision: &str, template: &str) -> String {
        self.jj(
            cwd,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                revision,
                "-T",
                template,
            ],
        )
    }

    fn git_init(&self, name: &str) -> PathBuf {
        let path = self.dir(name);
        self.git(&path, &["init", "-b", "main"]);
        path
    }

    fn write(&self, cwd: &Path, path: &str, contents: &str) {
        let path = cwd.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn commit(&self, cwd: &Path, description: &str) {
        self.git(cwd, &["add", "."]);
        self.git(cwd, &["commit", "-m", description]);
    }

    fn bare(&self, source: &Path, name: &str) -> PathBuf {
        let bare = self.temp.path().join(name);
        self.git(
            self.temp.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        bare
    }

    fn init_client(&self, name: &str, colocated: bool) -> PathBuf {
        let client = self.dir(name);
        let args = if colocated {
            ["git", "init", "--colocate"]
        } else {
            ["git", "init", "--no-colocate"]
        };
        self.jj(&client, &args);
        client
    }

    fn client(&self, name: &str, colocated: bool, remote: &Path, filter: &str) -> PathBuf {
        let client = self.init_client(name, colocated);
        self.remote(&client, "origin", remote, filter);
        client
    }

    fn view_client(&self, name: &str, colocated: bool, remote: &Path, view: &str) -> PathBuf {
        let client = self.init_client(name, colocated);
        self.jj(
            &client,
            &[
                "git",
                "remote",
                "add",
                "origin",
                remote.to_str().unwrap(),
                "--view",
                view,
            ],
        );
        self.jj(&client, &["git", "fetch", "--remote", "origin"]);
        client
    }

    fn remote(&self, client: &Path, name: &str, remote: &Path, filter: &str) {
        self.jj(
            client,
            &[
                "git",
                "remote",
                "add",
                name,
                remote.to_str().unwrap(),
                "--filter",
                filter,
            ],
        );
        self.jj(client, &["git", "fetch", "--remote", name]);
    }

    fn physical_remote(&self, client: &Path, project: &str, alias: &str) -> String {
        let state: serde_json::Value =
            serde_json::from_str(&self.jj(client, &["project", "show", project, "--json"]))
                .unwrap();
        let identity = state["projects"][0]["remotes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|remote| remote["candidates"][0]["definition"]["name"] == alias)
            .unwrap()["connection"]
            .as_str()
            .unwrap();
        let git_dir = client.join(".jj/repo/store/git");
        self.git(&git_dir, &["remote"])
            .lines()
            .find(|name| {
                self.git(
                    &git_dir,
                    &[
                        "config",
                        "--get",
                        &format!("remote.{name}.jjosh-connectionId"),
                    ],
                )
                .trim()
                    == identity
            })
            .unwrap()
            .to_owned()
    }

    fn refs(&self, remote: &Path) -> String {
        self.git(
            remote,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    fn operation_id(&self, client: &Path) -> String {
        self.jj(
            client,
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
    }
}

#[test]
fn offline_project_registration_survives_remote_removal_and_exports_only_its_directory() {
    let f = Fixture::new();
    let client = f.init_client("client", false);
    f.write(&client, "packages/api/api.txt", "standalone API\n");
    f.write(&client, "outside.txt", "monorepo only\n");
    f.jj(&client, &["describe", "-m", "create API in monorepo"]);
    f.jj(
        &client,
        &["project", "add", "api", "--path", "packages/api"],
    );
    let bare = f.dir("api.git");
    f.git(&bare, &["init", "--bare"]);
    f.jj(
        &client,
        &[
            "git",
            "remote",
            "add",
            "publication",
            bare.to_str().unwrap(),
            "--project",
            "api",
            "--whole",
        ],
    );
    f.jj(&client, &["bookmark", "create", "main#api", "-r", "@"]);
    f.jj(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "publication",
            "--bookmark",
            "main#api",
        ],
    );
    assert_eq!(
        f.git(&bare, &["ls-tree", "-r", "--name-only", "main"]),
        "api.txt\n"
    );
    f.jj(&client, &["git", "remote", "remove", "publication#api"]);
    f.jj(
        &client,
        &[
            "git",
            "remote",
            "add",
            "replacement",
            bare.to_str().unwrap(),
        ],
    );
    f.jj(
        &client,
        &[
            "git",
            "remote",
            "attach",
            "replacement",
            "--project",
            "api",
            "--whole",
        ],
    );
    f.jj(
        &client,
        &[
            "git",
            "fetch",
            "--remote",
            "replacement#api",
            "--branch",
            "main",
        ],
    );
    assert_eq!(
        f.jj(
            &client,
            &[
                "file",
                "show",
                "-r",
                "main#api@replacement",
                "packages/api/api.txt"
            ]
        ),
        "standalone API\n",
    );
}

#[test]
fn registering_a_filtered_project_preserves_its_reverse_source_layout() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(&source, "src/value.txt", "original\n");
    f.write(&source, "outside.txt", "preserve upstream context\n");
    f.commit(&source, "source layout");
    let bare = f.bare(&source, "source.git");
    let client = f.init_client("client", false);
    f.jj(
        &client,
        &["project", "add", "api", "--path", "packages/api"],
    );
    f.jj(
        &client,
        &[
            "git",
            "remote",
            "add",
            "source",
            bare.to_str().unwrap(),
            "--filter",
            ":/src",
            "--project",
            "api",
        ],
    );
    f.jj(
        &client,
        &["git", "fetch", "--remote", "source#api", "--branch", "main"],
    );
    f.jj(
        &client,
        &["new", "main#api@source", "-m", "edit filtered project"],
    );
    f.write(&client, "packages/api/value.txt", "project edit\n");
    f.jj(&client, &["describe", "-m", "edit filtered project"]);
    f.jj(&client, &["bookmark", "create", "main#api", "-r", "@"]);
    f.jj(&client, &["bookmark", "track", "main#api@source"]);
    f.jj(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "source",
            "--bookmark",
            "main#api",
        ],
    );
    assert_eq!(
        f.git(&bare, &["show", "main:src/value.txt"]),
        "project edit\n"
    );
    assert_eq!(
        f.git(&bare, &["show", "main:outside.txt"]),
        "preserve upstream context\n"
    );
}

#[test]
fn projection_preview_uses_recorded_history_without_snapshotting_pending_files() {
    let f = Fixture::new();
    let client = f.init_client("client", false);
    f.write(&client, "outside.txt", "initial scaffold\n");
    f.jj(&client, &["describe", "-m", "monorepo scaffold"]);
    f.jj(&client, &["new", "-m", "create API"]);
    f.write(&client, "api/api.txt", "recorded API\n");
    f.jj(&client, &["describe", "-m", "create API"]);
    f.jj(&client, &["new", "-m", "unrelated monorepo change"]);
    f.write(&client, "outside.txt", "unrelated edit\n");
    f.jj(&client, &["describe", "-m", "unrelated monorepo change"]);
    let operation = f.operation_id(&client);
    let git_path = f.jj(&client, &["git", "root"]);
    let refs = f.refs(Path::new(git_path.trim()));
    f.write(&client, "api/pending.txt", "not recorded\n");
    let preview: serde_json::Value = serde_json::from_str(&f.jj(
        &client,
        &[
            "projection",
            "preview",
            ":/api",
            "--files",
            "--history",
            "--json",
        ],
    ))
    .unwrap();
    assert_eq!(preview["files"], serde_json::json!(["api.txt"]));
    let history = preview["history"].as_array().unwrap();
    assert_eq!(
        history
            .iter()
            .map(|commit| commit["description"].as_str().unwrap().trim())
            .collect::<Vec<_>>(),
        ["create API"]
    );
    assert_eq!(f.operation_id(&client), operation);
    assert_eq!(f.refs(Path::new(git_path.trim())), refs);
    assert_eq!(
        fs::read_to_string(client.join("api/pending.txt")).unwrap(),
        "not recorded\n"
    );
}

#[test]
fn project_check_isolates_unrelated_connection_failures() {
    let f = Fixture::new();
    let client = f.init_client("client", false);
    f.jj(&client, &["project", "add", "api", "--path", "api"]);
    f.jj(&client, &["project", "add", "other", "--path", "other"]);
    let bare = f.dir("source.git");
    f.git(&bare, &["init", "--bare"]);
    for (project, remote) in [("api", "publication"), ("other", "secondary")] {
        f.jj(
            &client,
            &[
                "git",
                "remote",
                "add",
                remote,
                bare.to_str().unwrap(),
                "--project",
                project,
                "--whole",
            ],
        );
    }
    let publication = f.physical_remote(&client, "api", "publication");
    let secondary = f.physical_remote(&client, "other", "secondary");
    let git_path = f.jj(&client, &["git", "root"]);
    let git_path = Path::new(git_path.trim());
    f.git(
        git_path,
        &[
            "config",
            &format!("remote.{publication}.jjosh-requiredCapability"),
            "missing-provider",
        ],
    );
    // A native Git edit cannot retire the operation-owned binding.
    f.git(git_path, &["remote", "remove", &secondary]);
    assert!(
        !f.jj_unchecked(&client, &["project", "check"])
            .status
            .success()
    );
    assert!(
        !f.jj_unchecked(&client, &["project", "check", "api"])
            .status
            .success()
    );
    assert!(
        !f.jj_unchecked(&client, &["project", "check", "other"])
            .status
            .success()
    );
    f.git(
        git_path,
        &[
            "config",
            &format!("remote.{publication}.jjosh-requiredCapability"),
            "jjosh-v1",
        ],
    );
    f.jj(&client, &["project", "check", "api"]);
    assert!(
        !f.jj_unchecked(&client, &["project", "check"])
            .status
            .success()
    );
    assert!(
        !f.jj_unchecked(&client, &["project", "check", "other"])
            .status
            .success()
    );
}

#[test]
fn native_working_copy_push_roundtrips_in_both_colocation_modes() {
    for colocated in [false, true] {
        let f = Fixture::new();
        let source = f.git_init("source");
        f.write(&source, "app/file.txt", "original\n");
        f.write(&source, "private/secret.txt", "outside view\n");
        f.commit(&source, "source base");
        let remote = f.bare(&source, "source.git");
        let client = f.client("client", colocated, &remote, ":/app");
        f.jj(&client, &["bookmark", "track", "main@origin"]);
        f.jj(&client, &["new", "main@origin", "-m", "native edit"]);
        f.write(&client, "file.txt", "native working copy\n");
        f.jj(&client, &["describe", "-m", "native edit"]);
        let selected = f.log(&client, "@", "commit_id");
        let change = f.log(&client, "@", "change_id");
        let git_dir = if colocated {
            client.join(".git")
        } else {
            client.join(".jj/repo/store/git")
        };
        let git_head = f.run(
            &git_dir,
            Path::new("git"),
            &["rev-parse", "--verify", "HEAD"],
        );
        if git_head.status.success() {
            assert_ne!(String::from_utf8(git_head.stdout).unwrap().trim(), selected);
        } else {
            assert!(
                !colocated,
                "colocated Git HEAD should point at the native parent"
            );
        }
        f.jj(&client, &["bookmark", "set", "main", "-r", "@"]);
        f.jj(
            &client,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        assert_eq!(f.log(&client, "main@origin", "commit_id"), selected);
        assert_eq!(
            f.git(&remote, &["show", "main:app/file.txt"]),
            "native working copy\n"
        );
        assert_eq!(
            f.git(&remote, &["show", "main:private/secret.txt"]),
            "outside view\n"
        );
        assert_eq!(
            f.git(&remote, &["ls-tree", "-r", "--name-only", "main"]),
            "app/file.txt\nprivate/secret.txt\n"
        );

        // A fresh cache must independently reproject the exact native snapshot and change ID.
        let reader = f.client("reader", !colocated, &remote, ":/app");
        assert_eq!(f.log(&reader, "main@origin", "commit_id"), selected);
        assert_eq!(f.log(&reader, "main@origin", "change_id"), change);
        let before = f.refs(&remote);
        f.jj(&client, &["git", "fetch", "--remote", "origin"]);
        f.jj(
            &client,
            &[
                "bookmark",
                "set",
                "main",
                "-r",
                &selected,
                "--allow-backwards",
            ],
        );
        f.jj(
            &client,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        assert_eq!(f.refs(&remote), before);

        // Explicit selection must still publish an intentional empty commit.
        f.jj(
            &client,
            &["new", "main@origin", "-m", "intentional empty revision"],
        );
        let empty = f.log(&client, "@", "commit_id");
        assert_eq!(f.log(&client, "@", "empty"), "true");
        f.jj(&client, &["bookmark", "set", "main", "-r", &empty]);
        f.jj(
            &client,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        assert_ne!(f.refs(&remote), before);
        f.jj(&reader, &["git", "fetch", "--remote", "origin"]);
        assert_eq!(f.log(&reader, "main@origin", "commit_id"), empty);
    }
}

#[test]
fn named_git_endpoints_control_projected_fetch_and_push() {
    for colocated in [false, true] {
        let f = Fixture::new();
        let source = f.git_init("source");
        f.write(&source, "app/file.txt", "original endpoint\n");
        f.write(&source, "private.txt", "original private content\n");
        f.commit(&source, "original source");
        let original = f.bare(&source, "original.git");
        let client = f.client("client", colocated, &original, ":/app");
        let initial = f.log(&client, "main@origin", "commit_id");
        let original_refs = f.refs(&original);

        f.write(&source, "app/file.txt", "replacement endpoint\n");
        f.write(&source, "private.txt", "replacement private content\n");
        f.commit(&source, "replacement source");
        let replacement = f.bare(&source, "replacement.git");
        let destination = f.bare(&source, "destination.git");
        let replacement_refs = f.refs(&replacement);
        let git_dir = if colocated {
            client.join(".git")
        } else {
            client.join(".jj/repo/store/git")
        };
        // Change only Git's endpoints, leaving the named Josh filter untouched.
        f.git(
            &git_dir,
            &["remote", "set-url", "origin", replacement.to_str().unwrap()],
        );
        f.git(
            &git_dir,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                destination.to_str().unwrap(),
            ],
        );
        f.jj(
            &client,
            &["git", "fetch", "--remote", "origin", "--branch", "main"],
        );
        assert_ne!(f.log(&client, "main@origin", "commit_id"), initial);
        assert_eq!(
            f.jj(&client, &["file", "show", "-r", "main@origin", "file.txt"]),
            "replacement endpoint\n"
        );
        assert_eq!(
            f.jj(&client, &["file", "list", "-r", "main@origin"]),
            "file.txt\n"
        );

        f.jj(&client, &["bookmark", "track", "main@origin"]);
        f.jj(
            &client,
            &["new", "main@origin", "-m", "publish to push endpoint"],
        );
        f.write(&client, "file.txt", "converted publication\n");
        f.jj(&client, &["describe", "-m", "publish to push endpoint"]);
        let selected = f.log(&client, "@", "commit_id");
        f.jj(&client, &["bookmark", "set", "main", "-r", "@"]);
        f.jj(
            &client,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        assert_eq!(
            f.git(&destination, &["show", "main:app/file.txt"]),
            "converted publication\n"
        );
        assert_eq!(
            f.git(&destination, &["show", "main:private.txt"]),
            "replacement private content\n"
        );
        assert_eq!(
            f.git(&destination, &["ls-tree", "-r", "--name-only", "main"]),
            "app/file.txt\nprivate.txt\n"
        );
        assert_eq!(f.refs(&replacement), replacement_refs);
        assert_eq!(f.refs(&original), original_refs);
        let reader = f.client("reader", !colocated, &destination, ":/app");
        assert_eq!(f.log(&reader, "main@origin", "commit_id"), selected);
    }
}

#[test]
fn unrelated_history_import_uses_base_with_optional_merge_and_reprojects_exactly() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(&source, "app/file.txt", "first\n");
    f.write(&source, "private.txt", "source-only\n");
    f.commit(&source, "unrelated source root");
    f.write(&source, "app/file.txt", "second\n");
    f.commit(&source, "unrelated source edit");
    let source_remote = f.bare(&source, "source.git");
    let client = f.client("client", false, &source_remote, ":/app");
    f.jj(
        &client,
        &["new", "main@origin", "-m", "native imported edit"],
    );
    f.write(&client, "new.txt", "native imported content\n");
    f.jj(&client, &["describe", "-m", "native imported edit"]);
    let projected = f.log(&client, "@", "commit_id");
    let original_refs = f.refs(&source_remote);

    for merge in [false, true] {
        let suffix = if merge { "merge" } else { "base" };
        let target_source = f.git_init(&format!("target-{suffix}"));
        f.write(&target_source, "keep.txt", "destination-only\n");
        f.commit(&target_source, "unrelated destination root");
        let base = f
            .git(&target_source, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        let target = f.bare(&target_source, &format!("target-{suffix}.git"));
        f.remote(&client, suffix, &target, ":/app");
        f.jj(
            &client,
            &[
                "bookmark",
                "set",
                "imported",
                "-r",
                &projected,
                "--allow-backwards",
            ],
        );
        f.jj(
            &client,
            &["bookmark", "track", &format!("imported@{suffix}")],
        );
        let working_copy_before_push = f.log(&client, "@", "commit_id");
        let mut args = vec![
            "git",
            "push",
            "--remote",
            suffix,
            "--bookmark",
            "imported",
            "--base",
            "main",
        ];
        if merge {
            args.push("--merge");
        }
        f.jj(&client, &args);
        assert_eq!(f.log(&client, "@", "commit_id"), working_copy_before_push);
        assert_eq!(f.git(&target, &["rev-parse", "main"]).trim(), base);
        f.git(&target, &["merge-base", "--is-ancestor", &base, "imported"]);
        assert_eq!(
            f.git(&target, &["show", "imported:keep.txt"]),
            "destination-only\n"
        );
        assert_eq!(
            f.git(&target, &["show", "imported:app/file.txt"]),
            "second\n"
        );
        assert_eq!(
            f.git(&target, &["show", "imported:app/new.txt"]),
            "native imported content\n"
        );
        assert_eq!(
            f.git(&target, &["ls-tree", "-r", "--name-only", "imported"]),
            "app/file.txt\napp/new.txt\nkeep.txt\n"
        );
        if merge {
            assert_eq!(
                f.git(&target, &["show", "-s", "--format=%P", "imported"])
                    .split_whitespace()
                    .count(),
                2
            );
        }
        let reader = f.client(&format!("reader-{suffix}"), true, &target, ":/app");
        assert_eq!(f.log(&reader, "imported@origin", "commit_id"), projected);
        let before = f.refs(&target);
        f.jj(&client, &["git", "fetch", "--remote", suffix]);
        f.jj(
            &client,
            &[
                "bookmark",
                "set",
                "imported",
                "-r",
                &projected,
                "--allow-backwards",
            ],
        );
        f.jj(
            &client,
            &["git", "push", "--remote", suffix, "--bookmark", "imported"],
        );
        assert_eq!(f.refs(&target), before);
    }
    assert_eq!(f.refs(&source_remote), original_refs);
}

#[test]
fn versioned_projection_views_splice_history_and_share_edits_across_consumer_paths() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(
        &source,
        "src/moonlight-common-c/lib.txt",
        "shared original\n",
    );
    f.write(&source, "src/msquic/quic.txt", "quic original\n");
    f.write(&source, "private.txt", "outside both views\n");
    f.commit(&source, "shared library history");
    f.write(&source, "src/Sunshine/local.txt", "Sunshine local\n");
    f.commit(&source, "Sunshine before mapping");
    f.write(&source, "src/Artemis/local.txt", "Artemis local\n");
    f.write(
        &source,
        "src/Artemis/workspace.josh",
        concat!(
            "app/src/main/jni/moonlight-core/moonlight-common-c = :/src/moonlight-common-c\n",
            "third-party/msquic = :/src/msquic\n",
        ),
    );
    f.commit(&source, "map libraries into Artemis view");
    assert_eq!(f.git(&source, &["rev-list", "--merges", "main"]), "");
    let remote = f.bare(&source, "source.git");
    let sunshine = f.view_client("sunshine", true, &remote, "src/Sunshine");
    f.jj(&sunshine, &["bookmark", "track", "main@origin"]);
    f.jj(
        &sunshine,
        &[
            "new",
            "main@origin",
            "-m",
            "splice libraries into Sunshine view",
        ],
    );
    f.write(
        &sunshine,
        "workspace.josh",
        concat!(
            "third-party/moonlight-common-c = :/src/moonlight-common-c\n",
            "third-party/msquic = :/src/msquic\n",
        ),
    );
    f.jj(
        &sunshine,
        &["describe", "-m", "splice libraries into Sunshine view"],
    );
    let mapping_change = f.log(&sunshine, "@", "change_id");
    let published = f.log(&sunshine, "@", "commit_id");
    f.jj(&sunshine, &["new", "-m", "unpublished follow-up"]);
    f.write(&sunshine, "pending.txt", "keep this local edit\n");
    f.jj(&sunshine, &["describe", "-m", "unpublished follow-up"]);
    let pending_change = f.log(&sunshine, "@", "change_id");
    f.jj(&sunshine, &["bookmark", "set", "main", "-r", &published]);
    f.jj(
        &sunshine,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    f.jj(&sunshine, &["git", "fetch", "--remote", "origin"]);
    assert_eq!(f.log(&sunshine, "main@origin", "change_id"), mapping_change);
    assert_eq!(f.git(&remote, &["rev-list", "--merges", "main"]), "");
    // Josh may canonicalize workspace.josh. The returned commit is the
    // published version, not a new base on which to replay that same mapping.
    // Rebase only the unpublished descendant, then retire the old version.
    f.jj(
        &sunshine,
        &["rebase", "-s", &pending_change, "-d", "main@origin"],
    );
    f.jj(&sunshine, &["abandon", &published]);
    assert_eq!(f.log(&sunshine, "@", "change_id"), pending_change);
    assert_eq!(f.log(&sunshine, "@ & conflicts()", "commit_id"), "");
    assert_eq!(
        fs::read_to_string(sunshine.join("pending.txt")).unwrap(),
        "keep this local edit\n"
    );
    assert_eq!(
        fs::read_to_string(sunshine.join("third-party/moonlight-common-c/lib.txt")).unwrap(),
        "shared original\n"
    );
    assert_eq!(
        fs::read_to_string(sunshine.join("third-party/msquic/quic.txt")).unwrap(),
        "quic original\n"
    );
    let splice_parents = f.log(&sunshine, "main@origin", "parents.len()");
    assert_eq!(
        splice_parents, "2",
        "adding mappings splices previously separate projected histories"
    );
    let descriptions = f.log(&sunshine, "::main@origin", "description");
    assert!(descriptions.contains("shared library history"));
    assert!(descriptions.contains("Sunshine before mapping"));
    f.write(
        &sunshine,
        "third-party/moonlight-common-c/lib.txt",
        "edited in Sunshine\n",
    );
    f.write(
        &sunshine,
        "third-party/msquic/quic.txt",
        "quic edited in Sunshine\n",
    );
    f.jj(&sunshine, &["describe", "-m", "edit via Sunshine view"]);
    f.jj(&sunshine, &["bookmark", "set", "main", "-r", "@"]);
    f.jj(
        &sunshine,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/moonlight-common-c/lib.txt"]),
        "edited in Sunshine\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/msquic/quic.txt"]),
        "quic edited in Sunshine\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/Sunshine/pending.txt"]),
        "keep this local edit\n"
    );

    let artemis = f.view_client("artemis", false, &remote, "src/Artemis");
    f.jj(&artemis, &["bookmark", "track", "main@origin"]);
    f.jj(
        &artemis,
        &["new", "main@origin", "-m", "edit via Artemis view"],
    );
    let artemis_library = "app/src/main/jni/moonlight-core/moonlight-common-c/lib.txt";
    assert_eq!(
        fs::read_to_string(artemis.join(artemis_library)).unwrap(),
        "edited in Sunshine\n"
    );
    assert_eq!(
        fs::read_to_string(artemis.join("third-party/msquic/quic.txt")).unwrap(),
        "quic edited in Sunshine\n"
    );
    f.write(&artemis, artemis_library, "edited in Artemis\n");
    f.write(
        &artemis,
        "third-party/msquic/quic.txt",
        "quic edited in Artemis\n",
    );
    f.jj(&artemis, &["describe", "-m", "edit via Artemis view"]);
    let artemis_change = f.log(&artemis, "@", "change_id");
    f.jj(&artemis, &["bookmark", "set", "main", "-r", "@"]);
    f.jj(
        &artemis,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/moonlight-common-c/lib.txt"]),
        "edited in Artemis\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/msquic/quic.txt"]),
        "quic edited in Artemis\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:private.txt"]),
        "outside both views\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/Sunshine/local.txt"]),
        "Sunshine local\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/Artemis/local.txt"]),
        "Artemis local\n"
    );
    let reader = f.view_client("sunshine-reader", false, &remote, "src/Sunshine");
    assert_eq!(
        f.jj(
            &reader,
            &[
                "file",
                "show",
                "-r",
                "main@origin",
                "third-party/moonlight-common-c/lib.txt"
            ]
        ),
        "edited in Artemis\n"
    );
    assert_eq!(
        f.jj(
            &reader,
            &[
                "file",
                "show",
                "-r",
                "main@origin",
                "third-party/msquic/quic.txt"
            ]
        ),
        "quic edited in Artemis\n"
    );
    assert_eq!(f.log(&reader, "main@origin", "change_id"), artemis_change);
}

#[test]
fn removing_view_mapping_can_detach_files_or_remove_them_without_deleting_shared_content() {
    for remove_files in [false, true] {
        let f = Fixture::new();
        let source = f.git_init("source");
        f.write(
            &source,
            "src/moonlight-common-c/lib.txt",
            "shared original\n",
        );
        f.write(&source, "src/msquic/quic.txt", "quic original\n");
        f.write(&source, "src/Sunshine/local.txt", "Sunshine local\n");
        f.write(&source, "private.txt", "outside the views\n");
        f.write(
            &source,
            "src/Sunshine/workspace.josh",
            concat!(
                "third-party/moonlight-common-c = :/src/moonlight-common-c\n",
                "third-party/msquic = :/src/msquic\n",
            ),
        );
        f.write(
            &source,
            "src/Artemis/workspace.josh",
            concat!(
                "app/src/main/jni/moonlight-core/moonlight-common-c = :/src/moonlight-common-c\n",
                "third-party/msquic = :/src/msquic\n",
            ),
        );
        f.commit(&source, "two views of canonical libraries");
        let remote = f.bare(&source, "source.git");
        let sunshine = f.view_client("sunshine", remove_files, &remote, "src/Sunshine");
        f.jj(&sunshine, &["bookmark", "track", "main@origin"]);
        f.jj(
            &sunshine,
            &["new", "main@origin", "-m", "remove moonlight mapping"],
        );
        let library = "third-party/moonlight-common-c/lib.txt";
        assert_eq!(
            fs::read_to_string(sunshine.join(library)).unwrap(),
            "shared original\n"
        );
        f.write(
            &sunshine,
            "workspace.josh",
            "third-party/msquic = :/src/msquic\n",
        );
        if remove_files {
            fs::remove_dir_all(sunshine.join("third-party/moonlight-common-c")).unwrap();
        }
        f.jj(&sunshine, &["describe", "-m", "remove moonlight mapping"]);
        f.jj(&sunshine, &["bookmark", "set", "main", "-r", "@"]);
        f.jj(
            &sunshine,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        assert_eq!(
            f.git(&remote, &["show", "main:src/moonlight-common-c/lib.txt"]),
            "shared original\n"
        );
        assert_eq!(
            f.git(&remote, &["show", "main:src/Sunshine/workspace.josh"]),
            "third-party/msquic = :/src/msquic\n"
        );
        let detached_path = f.git(
            &remote,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                "main",
                "--",
                "src/Sunshine/third-party/moonlight-common-c",
            ],
        );
        if remove_files {
            assert_eq!(detached_path, "");
        } else {
            assert_eq!(
                detached_path,
                "src/Sunshine/third-party/moonlight-common-c/lib.txt\n"
            );
            assert_eq!(
                f.git(
                    &remote,
                    &[
                        "show",
                        "main:src/Sunshine/third-party/moonlight-common-c/lib.txt"
                    ]
                ),
                "shared original\n"
            );
            // Without a mapping these are independent view-local files, not aliases.
            f.jj(&sunshine, &["git", "fetch", "--remote", "origin"]);
            f.jj(
                &sunshine,
                &["new", "main@origin", "-m", "edit detached library"],
            );
            f.write(&sunshine, library, "Sunshine independent copy\n");
            f.jj(&sunshine, &["bookmark", "set", "main", "-r", "@"]);
            f.jj(
                &sunshine,
                &["git", "push", "--remote", "origin", "--bookmark", "main"],
            );
            assert_eq!(
                f.git(&remote, &["show", "main:src/moonlight-common-c/lib.txt"]),
                "shared original\n"
            );
            assert_eq!(
                f.git(
                    &remote,
                    &[
                        "show",
                        "main:src/Sunshine/third-party/moonlight-common-c/lib.txt"
                    ]
                ),
                "Sunshine independent copy\n"
            );
        }

        // The other consumer still edits canonical content. A fresh projection
        // must neither recreate a removed path nor overwrite a detached copy.
        let artemis = f.view_client("artemis", !remove_files, &remote, "src/Artemis");
        f.jj(&artemis, &["bookmark", "track", "main@origin"]);
        f.jj(
            &artemis,
            &["new", "main@origin", "-m", "advance still-shared library"],
        );
        let artemis_library = "app/src/main/jni/moonlight-core/moonlight-common-c/lib.txt";
        assert_eq!(
            fs::read_to_string(artemis.join(artemis_library)).unwrap(),
            "shared original\n"
        );
        f.write(
            &artemis,
            artemis_library,
            "canonical advanced through Artemis\n",
        );
        f.jj(&artemis, &["bookmark", "set", "main", "-r", "@"]);
        f.jj(
            &artemis,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        assert_eq!(
            f.git(&remote, &["show", "main:src/moonlight-common-c/lib.txt"]),
            "canonical advanced through Artemis\n"
        );
        assert_eq!(
            f.git(&remote, &["show", "main:src/msquic/quic.txt"]),
            "quic original\n"
        );
        assert_eq!(
            f.git(&remote, &["show", "main:private.txt"]),
            "outside the views\n"
        );
        let reader = f.view_client("sunshine-reader", !remove_files, &remote, "src/Sunshine");
        f.jj(&reader, &["new", "main@origin"]);
        assert_eq!(
            fs::read_to_string(reader.join("local.txt")).unwrap(),
            "Sunshine local\n"
        );
        assert_eq!(
            fs::read_to_string(reader.join("third-party/msquic/quic.txt")).unwrap(),
            "quic original\n"
        );
        if remove_files {
            assert!(!reader.join("third-party/moonlight-common-c").exists());
        } else {
            assert_eq!(
                fs::read_to_string(reader.join(library)).unwrap(),
                "Sunshine independent copy\n"
            );
        }
    }
}

#[test]
fn view_mapping_can_publish_local_files_and_relocate_their_view_path() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(
        &source,
        "src/Sunshine/vendor/local/lib.txt",
        "local library\n",
    );
    f.write(&source, "outside.txt", "unrelated content\n");
    f.commit(&source, "library owned by application");
    let remote = f.bare(&source, "source.git");
    let client = f.view_client("sunshine", false, &remote, "src/Sunshine");
    f.jj(&client, &["bookmark", "track", "main@origin"]);
    f.jj(
        &client,
        &["new", "main@origin", "-m", "publish local library"],
    );
    f.write(
        &client,
        "workspace.josh",
        "vendor/local = :/src/new-library\n",
    );
    f.jj(&client, &["bookmark", "set", "main", "-r", "@"]);
    f.jj(
        &client,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/new-library/lib.txt"]),
        "local library\n"
    );
    assert_eq!(
        f.git(
            &remote,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                "main",
                "--",
                "src/Sunshine/vendor/local"
            ]
        ),
        ""
    );

    f.jj(&client, &["git", "fetch", "--remote", "origin"]);
    f.jj(&client, &["new", "main@origin", "-m", "relocate view path"]);
    fs::create_dir(client.join("deps")).unwrap();
    fs::rename(client.join("vendor/local"), client.join("deps/shared")).unwrap();
    f.write(
        &client,
        "workspace.josh",
        "deps/shared = :/src/new-library\n",
    );
    f.jj(&client, &["bookmark", "set", "main", "-r", "@"]);
    f.jj(
        &client,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/new-library/lib.txt"]),
        "local library\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:outside.txt"]),
        "unrelated content\n"
    );
    let reader = f.view_client("reader", true, &remote, "src/Sunshine");
    assert_eq!(
        f.jj(&reader, &["file", "list", "-r", "main@origin"]),
        "deps/shared/lib.txt\nworkspace.josh\n"
    );
    assert_eq!(
        f.jj(
            &reader,
            &["file", "show", "-r", "main@origin", "deps/shared/lib.txt"]
        ),
        "local library\n"
    );
}

#[test]
fn view_paths_are_literal_and_cannot_be_combined_with_filter_expressions() {
    let f = Fixture::new();
    let source = f.git_init("source");
    let view = "src/Sunshine [preview]:local";
    f.write(&source, &format!("{view}/local.txt"), "literal view\n");
    f.write(
        &source,
        "src/Sunshine/private.txt",
        "outside literal view\n",
    );
    f.commit(&source, "literal path and unrelated content");
    let remote = f.bare(&source, "source.git");
    let client = f.view_client("client", false, &remote, view);
    f.jj(&client, &["bookmark", "track", "main@origin"]);
    f.jj(&client, &["new", "main@origin", "-m", "edit literal view"]);
    assert_eq!(
        fs::read_to_string(client.join("local.txt")).unwrap(),
        "literal view\n"
    );
    assert_eq!(f.jj(&client, &["file", "list"]), "local.txt\n");
    let before = f.refs(&remote);
    let rejected = f.jj_unchecked(
        &client,
        &[
            "git",
            "remote",
            "add",
            "origin",
            remote.to_str().unwrap(),
            "--filter",
            ":/",
            "--view",
            view,
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.refs(&remote), before);
    f.jj(&client, &["git", "fetch", "--remote", "origin"]);
    assert_eq!(
        f.jj(&client, &["file", "show", "-r", "main@origin", "local.txt"]),
        "literal view\n"
    );
    assert_eq!(
        f.jj(&client, &["file", "list", "-r", "main@origin"]),
        "local.txt\n"
    );
    f.write(&client, "local.txt", "literal view edited\n");
    f.jj(&client, &["bookmark", "set", "main", "-r", "@"]);
    f.jj(
        &client,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    assert_eq!(
        f.git(&remote, &["show", &format!("main:{view}/local.txt")]),
        "literal view edited\n"
    );
    assert_eq!(
        f.git(&remote, &["show", "main:src/Sunshine/private.txt"]),
        "outside literal view\n"
    );

    let raw = f.client("raw-reader", true, &remote, ":/");
    let before = f.refs(&remote);
    let preview = f.jj(
        &raw,
        &["projection", "preview", "--view", view, "-r", "main@origin"],
    );
    assert_eq!(
        preview,
        f.jj(
            &raw,
            &[
                "projection",
                "preview",
                "--view",
                &format!("./{view}"),
                "-r",
                "main@origin",
            ]
        )
    );
    let rejected = f.jj_unchecked(
        &raw,
        &[
            "projection",
            "preview",
            ":/",
            "--view",
            view,
            "-r",
            "main@origin",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.refs(&remote), before);
}

#[test]
fn dry_run_non_fast_forward_and_native_conflict_ancestry_cannot_publish() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(&source, "app/file.txt", "base\n");
    f.commit(&source, "base");
    let remote = f.bare(&source, "source.git");
    let client = f.client("client", true, &remote, ":/app");
    f.jj(&client, &["bookmark", "track", "main@origin"]);
    f.jj(&client, &["new", "main@origin", "-m", "local edit"]);
    f.write(&client, "file.txt", "local\n");
    f.jj(&client, &["describe", "-m", "local edit"]);
    let selected = f.log(&client, "@", "commit_id");
    f.jj(&client, &["bookmark", "set", "main", "-r", "@"]);
    let before = f.refs(&remote);
    let local_refs = f.refs(&client);
    let operation = f.operation_id(&client);
    f.jj(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "origin",
            "--bookmark",
            "main",
            "--dry-run",
        ],
    );
    assert_eq!(f.refs(&remote), before);
    assert_eq!(f.log(&client, "@", "commit_id"), selected);
    assert_eq!(f.refs(&client), local_refs);
    assert_eq!(f.operation_id(&client), operation);
    f.write(&source, "app/file.txt", "remote advanced\n");
    f.commit(&source, "concurrent remote edit");
    f.git(&source, &["push", remote.to_str().unwrap(), "main"]);
    let advanced = f.refs(&remote);
    let rejected = f.jj_unchecked(
        &client,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.refs(&remote), advanced);
    assert_eq!(f.log(&client, "@", "commit_id"), selected);

    f.jj(&client, &["bookmark", "set", "left"]);
    f.jj(&client, &["new", "main@origin", "-m", "other side"]);
    f.write(&client, "file.txt", "right\n");
    f.jj(&client, &["bookmark", "set", "right"]);
    f.jj(&client, &["new", "left", "right", "-m", "native conflict"]);
    f.jj(&client, &["bookmark", "set", "conflicted"]);
    let conflict = f.log(&client, "@", "commit_id");
    assert_eq!(f.log(&client, "@ & conflicts()", "commit_id"), conflict);
    let rejected = f.jj_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "origin",
            "--named",
            "must-not-exist=conflicted",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.refs(&remote), advanced);
    assert_eq!(f.log(&client, "@ & conflicts()", "commit_id"), conflict);

    // Resolving only a child must not smuggle the parent's native transport into Git history.
    f.jj(&client, &["new", "conflicted", "-m", "clean descendant"]);
    f.write(&client, "file.txt", "resolved only in child\n");
    f.jj(&client, &["describe", "-m", "clean descendant"]);
    assert_eq!(f.log(&client, "@ & conflicts()", "commit_id"), "");
    assert_eq!(
        f.log(&client, "conflicted & conflicts()", "commit_id"),
        conflict
    );
    let clean = f.log(&client, "@", "commit_id");
    let rejected = f.jj_unchecked(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "origin",
            "--named",
            "must-not-exist=@",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.refs(&remote), advanced);
    assert_eq!(f.log(&client, "@", "commit_id"), clean);
    assert_eq!(
        fs::read_to_string(client.join("file.txt")).unwrap(),
        "resolved only in child\n"
    );

    // Fetch must inspect native transport before exposing projected Git refs;
    // a later ordinary jj command must not import corrupt projected conflicts.
    let provider = client.join(".git");
    f.git(&provider, &["update-ref", "refs/heads/provider", &clean]);
    f.git(&provider, &["symbolic-ref", "HEAD", "refs/heads/provider"]);
    let receiver = f.dir("receiver");
    f.jj(&receiver, &["git", "init", "--no-colocate"]);
    f.jj(
        &receiver,
        &[
            "git",
            "remote",
            "add",
            "origin",
            provider.to_str().unwrap(),
            "--filter",
            ":/",
        ],
    );
    let operation = f.jj(
        &receiver,
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
    );
    let rejected = f.jj_unchecked(&receiver, &["git", "fetch", "--remote", "origin"]);
    assert!(!rejected.status.success());
    f.jj(&receiver, &["status"]);
    assert_eq!(
        f.jj(
            &receiver,
            &[
                "--ignore-working-copy",
                "op",
                "log",
                "--limit",
                "1",
                "--no-graph",
                "-T",
                "id",
            ]
        ),
        operation
    );
    assert!(
        f.jj(&receiver, &["bookmark", "list", "--all-remotes"])
            .is_empty()
    );
}

#[test]
fn fetch_removes_deleted_branches_and_empty_projections() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(&source, "app/file.txt", "projected content\n");
    f.commit(&source, "source");
    let original = f.git(&source, &["rev-parse", "HEAD"]);
    f.git(&source, &["branch", "feature"]);
    let remote = f.bare(&source, "source.git");
    let client = f.client("client", false, &remote, ":/app");
    let projected = f.log(&client, "main@origin", "commit_id");
    assert_eq!(f.log(&client, "feature@origin", "commit_id"), projected);

    f.git(&remote, &["update-ref", "-d", "refs/heads/feature"]);
    f.jj(&client, &["git", "fetch", "--remote", "origin"]);
    assert!(
        !f.jj_unchecked(&client, &["log", "-r", "feature@origin"])
            .status
            .success()
    );
    assert_eq!(f.log(&client, "main@origin", "commit_id"), projected);

    // Rewrite the source to history that has never contained the selected subtree.
    f.git(&source, &["checkout", "--orphan", "empty-view"]);
    f.git(&source, &["rm", "-r", "-f", "app"]);
    f.write(&source, "outside.txt", "outside projection\n");
    f.commit(&source, "empty projected history");
    f.git(
        &source,
        &["push", "--force", remote.to_str().unwrap(), "HEAD:main"],
    );
    f.jj(&client, &["git", "fetch", "--remote", "origin"]);
    assert!(
        !f.jj_unchecked(&client, &["log", "-r", "main@origin"])
            .status
            .success()
    );
    assert!(
        f.jj(&client, &["bookmark", "list", "--all-remotes"])
            .is_empty()
    );

    f.git(&remote, &["update-ref", "refs/heads/main", original.trim()]);
    f.jj(&client, &["git", "fetch", "--remote", "origin"]);
    assert_eq!(f.log(&client, "main@origin", "commit_id"), projected);
    assert_eq!(
        f.jj(&client, &["file", "show", "-r", "main@origin", "file.txt"]),
        "projected content\n"
    );
}

#[test]
fn projected_remote_rename_retains_filter_and_remove_readd_is_unfiltered() {
    for colocated in [false, true] {
        let f = Fixture::new();
        let source = f.git_init("source");
        f.write(&source, "app/file.txt", "projected\n");
        f.write(&source, "private.txt", "private\n");
        f.commit(&source, "source");
        let remote = f.bare(&source, "source.git");
        let client = f.client("client", colocated, &remote, ":/app");
        let before = f.log(&client, "main@origin", "commit_id");
        f.jj(&client, &["git", "remote", "rename", "origin", "renamed"]);
        assert_eq!(f.log(&client, "main@renamed", "commit_id"), before);
        f.jj(&client, &["git", "fetch", "--remote", "renamed"]);
        assert_eq!(
            f.jj(&client, &["file", "list", "-r", "main@renamed"]),
            "file.txt\n",
        );
        assert_eq!(
            f.jj(&client, &["file", "show", "-r", "main@renamed", "file.txt"]),
            "projected\n",
        );
        f.jj(&client, &["git", "remote", "remove", "renamed"]);
        assert!(
            !f.jj_unchecked(&client, &["log", "-r", "main@renamed"])
                .status
                .success()
        );
        f.jj(
            &client,
            &["git", "remote", "add", "renamed", remote.to_str().unwrap()],
        );
        f.jj(&client, &["git", "fetch", "--remote", "renamed"]);
        assert_eq!(
            f.jj(&client, &["file", "list", "-r", "main@renamed"]),
            "app/file.txt\nprivate.txt\n",
        );
        // The original name was also retired by rename.
        f.jj(
            &client,
            &["git", "remote", "add", "origin", remote.to_str().unwrap()],
        );
        f.jj(&client, &["git", "fetch", "--remote", "origin"]);
        assert_eq!(
            f.jj(
                &client,
                &["file", "show", "-r", "main@origin", "private.txt"]
            ),
            "private\n",
        );
    }
}

#[test]
fn attached_remote_lifecycle_preserves_mount_push_endpoint_and_peer() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(&source, "file.txt", "project\n");
    f.commit(&source, "source");
    let remote = f.bare(&source, "source.git");
    let destination = f.bare(&source, "destination.git");
    let client = f.init_client("client", false);
    f.jj(&client, &["project", "add", "project", "--path", "vendor"]);
    let git_dir = client.join(".jj/repo/store/git");
    for name in ["origin", "peer"] {
        f.jj(
            &client,
            &["git", "remote", "add", name, remote.to_str().unwrap()],
        );
        f.jj(
            &client,
            &[
                "git",
                "remote",
                "attach",
                name,
                "--project",
                "project",
                "--whole",
            ],
        );
        f.jj(
            &client,
            &["git", "fetch", "--project", "project", "--remote", name],
        );
    }
    let physical = f.physical_remote(&client, "project", "origin");
    assert_eq!(physical, "origin#project");
    for spec in [
        format!("+refs/tags/*:refs/remotes/{physical}/tags/*"),
        "+refs/pull/*/head:refs/pullreqs/*".to_owned(),
        "^refs/heads/private/*".to_owned(),
    ] {
        f.git(
            &git_dir,
            &[
                "config",
                "--add",
                &format!("remote.{physical}.fetch"),
                &spec,
            ],
        );
    }
    let connection = f.git(
        &git_dir,
        &[
            "config",
            "--get",
            &format!("remote.{physical}.jjosh-connectionId"),
        ],
    );
    f.jj(
        &client,
        &["git", "remote", "add", "origin", remote.to_str().unwrap()],
    );
    f.jj(&client, &["git", "fetch", "--remote", "origin"]);
    let root_origin = f.log(&client, "main@origin", "commit_id");
    f.git(
        &git_dir,
        &[
            "config",
            &format!("remote.{physical}.pushurl"),
            destination.to_str().unwrap(),
        ],
    );
    let before = f.log(&client, "main#project@origin", "commit_id");
    f.jj(
        &client,
        &["git", "remote", "rename", "origin#project", "renamed"],
    );
    assert_eq!(f.log(&client, "main#project@renamed", "commit_id"), before);
    let renamed = f.physical_remote(&client, "project", "renamed");
    assert_eq!(renamed, "renamed#project");
    assert_eq!(
        f.git(
            &git_dir,
            &["config", "--get-all", &format!("remote.{renamed}.fetch")]
        ),
        format!(
            "+refs/heads/*:refs/remotes/{renamed}/*\n+refs/tags/*:refs/remotes/{renamed}/tags/*\n+refs/pull/*/head:refs/pullreqs/*\n^refs/heads/private/*\n"
        ),
    );
    assert_eq!(
        f.git(
            &git_dir,
            &[
                "config",
                "--get",
                &format!("remote.{renamed}.jjosh-connectionId")
            ]
        ),
        connection,
    );
    assert_eq!(f.log(&client, "main@origin", "commit_id"), root_origin);
    f.jj(&client, &["git", "fetch", "--remote", "renamed#project"]);
    assert_eq!(
        f.jj(
            &client,
            &[
                "file",
                "show",
                "-r",
                "main#project@renamed",
                "vendor/file.txt"
            ]
        ),
        "project\n",
    );
    f.jj(&client, &["bookmark", "track", "main#project@renamed"]);
    f.jj(&client, &["new", "main#project@renamed"]);
    f.write(&client, "vendor/file.txt", "published\n");
    f.jj(&client, &["describe", "-m", "publish"]);
    f.jj(&client, &["bookmark", "set", "main#project", "-r", "@"]);
    f.jj(
        &client,
        &[
            "git",
            "push",
            "--remote",
            "renamed",
            "--bookmark",
            "main#project",
        ],
    );
    assert_eq!(
        f.git(&destination, &["show", "main:file.txt"]),
        "published\n"
    );
    assert_eq!(f.git(&remote, &["show", "main:file.txt"]), "project\n");
    f.jj(&client, &["git", "remote", "remove", "renamed#project"]);
    assert!(
        !f.jj_unchecked(&client, &["log", "-r", "main#project@renamed"])
            .status
            .success()
    );
    f.jj(&client, &["git", "fetch", "--remote", "peer#project"]);
    assert_eq!(
        f.jj(
            &client,
            &["file", "show", "-r", "main#project@peer", "vendor/file.txt"]
        ),
        "project\n",
    );
    assert_eq!(f.log(&client, "main@origin", "commit_id"), root_origin);
}

#[test]
fn rejected_remote_lifecycle_preserves_bindings_and_observations() {
    let f = Fixture::new();
    let source = f.git_init("source");
    f.write(&source, "app/file.txt", "projected\n");
    f.write(&source, "private.txt", "private\n");
    f.commit(&source, "source");
    let remote = f.bare(&source, "source.git");
    let client = f.client("client", false, &remote, ":/app");
    let git_dir = client.join(".jj/repo/store/git");
    f.jj(
        &client,
        &[
            "git",
            "remote",
            "add",
            "destination",
            remote.to_str().unwrap(),
        ],
    );
    let config = fs::read(git_dir.join("config")).unwrap();
    let operation = f.operation_id(&client);
    let observation = f.log(&client, "main@origin", "commit_id");
    f.write(&client, "unrecorded.txt", "local edit\n");
    assert!(
        !f.jj_unchecked(
            &client,
            &["git", "remote", "rename", "origin", "destination"],
        )
        .status
        .success()
    );
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), config);
    assert_eq!(f.operation_id(&client), operation);
    assert_eq!(f.log(&client, "main@origin", "commit_id"), observation);
    f.jj(&client, &["git", "remote", "remove", "destination"]);

    let config = fs::read(git_dir.join("config")).unwrap();
    let operation = f.operation_id(&client);
    let ref_lock = git_dir.join("refs/remotes/origin/main.lock");
    fs::write(&ref_lock, "").unwrap();
    assert!(
        !f.jj_unchecked(&client, &["git", "remote", "remove", "origin"])
            .status
            .success()
    );
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), config);
    assert_eq!(f.operation_id(&client), operation);
    assert_eq!(f.log(&client, "main@origin", "commit_id"), observation);
    fs::remove_file(ref_lock).unwrap();

    f.jj(&client, &["git", "remote", "rename", "origin", "renamed"]);
    f.jj(&client, &["git", "fetch", "--remote", "renamed"]);
    assert_eq!(
        f.jj(&client, &["file", "list", "-r", "main@renamed"]),
        "file.txt\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("unrecorded.txt")).unwrap(),
        "local edit\n"
    );
}

#[test]
fn remote_rename_keeps_branch_fetch_and_push_destinations_independent() {
    let f = Fixture::new();
    let source = f.git_init("branch-source");
    f.write(&source, "file", "source\n");
    f.commit(&source, "source");
    let endpoint = f.bare(&source, "branch-source.git");
    let client = f.init_client("client", false);
    f.jj(
        &client,
        &["git", "remote", "add", "origin", endpoint.to_str().unwrap()],
    );
    let git_dir = client.join(".jj/repo/store/git");
    for (key, value) in [
        ("branch.fetch.remote", "origin"),
        ("branch.fetch.pushRemote", "other"),
        ("branch.fetch.description", "keep branch notes"),
        ("branch.push.remote", "other"),
        ("branch.push.pushRemote", "origin"),
        ("branch.only-push.pushRemote", "origin"),
    ] {
        f.git(&git_dir, &["config", key, value]);
    }
    f.jj(&client, &["git", "remote", "rename", "origin", "primary"]);
    for (key, expected) in [
        ("branch.fetch.remote", "primary"),
        ("branch.fetch.pushRemote", "other"),
        ("branch.fetch.description", "keep branch notes"),
        ("branch.push.remote", "other"),
        ("branch.push.pushRemote", "primary"),
        ("branch.only-push.pushRemote", "primary"),
    ] {
        assert_eq!(f.git(&git_dir, &["config", "--get", key]).trim(), expected);
    }
    assert_eq!(
        f.git(
            &git_dir,
            &["config", "--get-regexp", "^branch.only-push\\."]
        ),
        "branch.only-push.pushremote primary\n",
    );
}

#[test]
fn remote_rename_preserves_literal_helper_urls_and_rewrite_shorthands() {
    for (url, pushurl) in [
        (
            "rad://z4FUGM4AENYkjPk8TpvVqJVwtzvwJ",
            Some("rad://z4FUGM4AENYkjPk8TpvVqJVwtzvwJ/z6MkoyXnafaWcQ"),
        ),
        ("short:User/Repo.git", None),
    ] {
        let f = Fixture::new();
        let source = f.git_init("literal-source");
        f.write(&source, "file", "source\n");
        f.commit(&source, "source");
        let endpoint = f.bare(&source, "literal-source.git");
        let client = f.init_client("client", false);
        f.jj(
            &client,
            &["git", "remote", "add", "origin", endpoint.to_str().unwrap()],
        );
        let git_dir = client.join(".jj/repo/store/git");
        f.git(&git_dir, &["config", "remote.origin.url", url]);
        if let Some(pushurl) = pushurl {
            f.git(&git_dir, &["config", "remote.origin.pushurl", pushurl]);
        }
        f.git(
            &git_dir,
            &["config", "url.https://example.invalid/.insteadOf", "short:"],
        );
        f.jj(&client, &["git", "remote", "rename", "origin", "primary"]);
        assert_eq!(
            f.git(&git_dir, &["config", "--get", "remote.primary.url"])
                .trim(),
            url
        );
        if let Some(pushurl) = pushurl {
            assert_eq!(
                f.git(&git_dir, &["config", "--get", "remote.primary.pushurl"])
                    .trim(),
                pushurl
            );
        } else {
            assert_eq!(
                f.git(&git_dir, &["config", "--get-regexp", "^remote.primary\\."])
                    .lines()
                    .filter(|line| line.starts_with("remote.primary.pushurl "))
                    .count(),
                0,
            );
        }
    }
}
