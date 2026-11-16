#![cfg(unix)]

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    temp: tempfile::TempDir,
    repo: PathBuf,
    tick: Cell<u32>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(temp.path().join("home")).unwrap();
        fs::create_dir(&repo).unwrap();
        fs::write(
            temp.path().join("config"),
            "[user]\nname='Context Test'\nemail='context@example.com'\n",
        )
        .unwrap();
        let f = Self {
            temp,
            repo,
            tick: Cell::new(0),
        };
        f.jj(&["git", "init", "--no-colocate"]);
        f.write("packages/api/a", "base api\n");
        f.write("other/b", "base other\n");
        f.jj(&["describe", "-m", "base"]);
        f.jj(&["bookmark", "create", "base"]);
        f.jj(&["project", "add", "api", "--path", "packages/api"]);
        f.jj(&["project", "add", "other", "--path", "other"]);
        f
    }

    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Output {
        let tick = self.tick.get();
        self.tick.set(tick + 1);
        Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.temp.path().join("home"))
            .env("JJ_CONFIG", self.temp.path().join("config"))
            .env("JJ_TIMESTAMP", "2001-01-01T00:00:00+00:00")
            .env("JJ_OP_TIMESTAMP", "2001-01-01T00:00:00+00:00")
            .env("JJ_RANDOMNESS_SEED", tick.to_string())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LANG", "C.UTF-8")
            .output()
            .unwrap()
    }

    fn unchecked(&self, args: &[&str]) -> Output {
        self.run(&self.repo, env!("CARGO_BIN_EXE_jjosh"), args)
    }
    fn success(output: Output) -> String {
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn jj(&self, args: &[&str]) -> String {
        Self::success(self.unchecked(args))
    }
    fn project(&self, args: &[&str]) -> String {
        self.jj(&[&["--in-project", "api"][..], args].concat())
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn op(&self) -> String {
        self.jj(&[
            "--ignore-working-copy",
            "op",
            "log",
            "--no-graph",
            "-n",
            "1",
            "-T",
            "id",
        ])
    }
    fn conflict(&self, path: &str) {
        for side in ["left", "right"] {
            self.jj(&["new", "base", "-m", side]);
            self.write(path, side);
            self.jj(&["bookmark", "create", side]);
        }
        self.jj(&["new", "left", "right", "-m", "merge"]);
    }
    fn remote(&self, project: &str) -> PathBuf {
        let path = self.temp.path().join(format!("{project}.git"));
        Self::success(self.run(
            self.temp.path(),
            "git",
            &["init", "--bare", path.to_str().unwrap()],
        ));
        self.jj(&[
            "git",
            "remote",
            "add",
            "origin",
            path.to_str().unwrap(),
            "--project",
            project,
            "--whole",
        ]);
        self.jj(&["bookmark", "create", &format!("main#{project}")]);
        path
    }
    fn git(&self, repo: &Path, args: &[&str]) -> String {
        Self::success(self.run(repo, "git", args))
    }
}

#[test]
fn projection_command_is_removed() {
    let f = Fixture::new();
    let help = f.jj(&["--help"]);
    assert!(!help.contains("  projection"));
    assert!(
        !f.unchecked(&["projection", "preview", ":/packages/api"])
            .status
            .success()
    );
}

#[test]
fn project_paths_and_templates_are_bounded_independently_of_cwd() {
    let f = Fixture::new();
    assert_eq!(f.project(&["file", "list"]), "a\n");
    assert_eq!(f.project(&["file", "list", "all()"]), "a\n");
    assert_eq!(f.project(&["file", "list", "~missing"]), "a\n");
    assert_eq!(f.project(&["file", "list", "glob:*"]), "a\n");
    assert_eq!(f.project(&["file", "list", "root:other"]), "");
    assert!(
        !f.unchecked(&["--in-project", "api", "file", "show", "../other/b"])
            .status
            .success()
    );
    assert_eq!(
        Fixture::success(f.run(
            &f.repo.join("other"),
            env!("CARGO_BIN_EXE_jjosh"),
            &["--in-project", "api", "file", "show", "a"]
        )),
        "base api\n"
    );
    f.jj(&["new", "-m", "mixed"]);
    f.write("packages/api/a", "api edit\n");
    f.write("other/b", "other edit\n");
    assert_eq!(f.project(&["diff", "--summary"]), "M a\n");
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            r#"touched_projects.join(",")"#
        ]),
        "api"
    );

    let show = f.project(&["show", "--summary"]);
    assert!(show.contains("M a"));
    assert!(!show.contains("other/b"));
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "files.map(|f| f.path().display()).join(\"\\n\")"
        ]),
        "a"
    );
    assert_eq!(
        f.project(&["log", "--no-graph", "-r", "@", "-T", "diff.summary()"]),
        "M a\n"
    );
    assert_eq!(
        f.project(&["log", "--no-graph", "-r", "files(a)", "-T", "description"]),
        "mixed\nbase\n"
    );
}

#[test]
fn operation_selects_project_definition_and_readonly_query_does_not_snapshot() {
    let f = Fixture::new();
    let old = f.op();
    f.jj(&["project", "rename", "api", "renamed"]);
    f.write("packages/api/a", "pending\n");
    let before = f.op();
    assert_eq!(
        f.jj(&[
            "--at-operation",
            old.trim(),
            "--in-project",
            "api",
            "file",
            "show",
            "a"
        ]),
        "base api\n"
    );
    assert_eq!(f.op(), before);
    assert!(
        !f.unchecked(&["--in-project", "api", "status"])
            .status
            .success()
    );
    assert_eq!(f.op(), before);
    assert_eq!(
        f.jj(&[
            "--in-project",
            "renamed",
            "--ignore-working-copy",
            "file",
            "show",
            "a"
        ]),
        "base api\n"
    );
}

#[test]
fn unsupported_commands_and_conflicting_selectors_fail_before_snapshot() {
    let f = Fixture::new();
    f.write("other/b", "pending outside\n");
    let op = f.op();
    for args in [
        vec!["describe", "-m", "wrong"],
        vec!["rebase", "-r", "@", "-o", "root()"],
        vec!["op", "restore", "@-"],
        vec!["git", "fetch", "--all-projects"],
        vec!["git", "fetch", "--project", "other"],
        vec!["git", "remote", "list", "--project", "other"],
        vec!["git", "push", "--change", "@"],
    ] {
        let result = f.unchecked(&[&["--in-project", "api"][..], &args].concat());
        assert!(!result.status.success(), "{args:?}");
        assert_eq!(f.op(), op, "{args:?}");
    }
}

#[test]
fn scope_does_not_partition_working_copy_snapshots() {
    let f = Fixture::new();
    f.jj(&["new", "-m", "pending"]);
    f.write("other/b", "saved outside\n");
    let status = f.project(&["status"]);
    assert!(status.contains("has no changes"));
    assert!(!status.contains("other/b"));
    assert_eq!(
        f.jj(&["--ignore-working-copy", "file", "show", "other/b"]),
        "saved outside\n"
    );
    assert_eq!(
        f.project(&["log", "--no-graph", "-r", "@", "-T", "empty"]),
        "true"
    );
    assert_eq!(
        f.jj(&["log", "--no-graph", "-r", "@", "-T", "empty"]),
        "false"
    );
}

#[test]
fn project_history_retains_merges_and_canonical_parent_selection() {
    let f = Fixture::new();
    for (side, path) in [
        ("api-left", "packages/api/l"),
        ("api-right", "packages/api/r"),
    ] {
        f.jj(&["new", "base", "-m", side]);
        f.write(path, side);
        f.jj(&["bookmark", "create", side]);
    }
    f.jj(&["new", "api-left", "api-right", "-m", "api merge"]);
    f.jj(&["new", "-m", "foreign change"]);
    f.write("other/b", "foreign\n");
    f.jj(&["describe", "-m", "foreign change"]);
    f.jj(&["new", "-m", "working"]);
    let history = f.project(&["log", "--no-graph", "-T", "description"]);
    assert!(history.contains("api merge\n"));
    assert!(history.contains("working\n"));
    assert!(!history.contains("foreign change\n"));
    assert_eq!(
        f.project(&["log", "--no-graph", "-r", "@-", "-T", "description"]),
        "foreign change\n"
    );
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "commit_id ++ change_id"
        ]),
        f.jj(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "commit_id ++ change_id"
        ])
    );
}

#[test]
fn conflicts_are_visible_and_diagnosed_only_in_the_selected_scope() {
    for (path, expected) in [("other/b", false), ("packages/api/a", true)] {
        let f = Fixture::new();
        f.conflict(path);
        let status = f.project(&["status"]);
        assert_eq!(
            status.contains("unresolved conflicts"),
            expected,
            "{status}"
        );
        assert_eq!(
            f.project(&["log", "--no-graph", "-r", "@", "-T", "conflict"]),
            expected.to_string()
        );
        let history = f.project(&["log", "--no-graph", "-T", "description"]);
        assert!(history.contains("merge"));
        let files = f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "conflicted_files.map(|f| f.path().display()).join(\"\\n\")",
        ]);
        assert_eq!(files, if expected { "a" } else { "" });
        if expected {
            assert!(f.project(&["file", "show", "a"]).contains("left"));
        }
    }
}

#[test]
fn references_use_project_labels_without_falling_back_to_root_names() {
    let f = Fixture::new();
    f.jj(&["bookmark", "create", "main#api"]);
    f.jj(&["new", "-m", "root main"]);
    f.jj(&["bookmark", "create", "main"]);
    f.jj(&["bookmark", "create", "main#other"]);
    assert_eq!(
        f.project(&["log", "--no-graph", "-r", "main", "-T", "description"]),
        "base\n"
    );
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "bookmarks()",
            "-T",
            "description"
        ]),
        "base\n"
    );
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "revset(\"main\").map(|c| c.description()).join(\"\")"
        ]),
        "base\n"
    );
    assert!(
        !f.unchecked(&["--in-project", "api", "show", "base"])
            .status
            .success()
    );
    assert!(
        !f.unchecked(&["--in-project", "api", "show", "main#other"])
            .status
            .success()
    );
    f.jj(&["project", "rename", "api", "renamed"]);
    assert_eq!(
        f.jj(&[
            "--in-project",
            "renamed",
            "log",
            "--no-graph",
            "-r",
            "main",
            "-T",
            "description"
        ]),
        "base\n"
    );
}

#[test]
fn project_fetch_and_push_all_use_only_the_selected_namespace() {
    let f = Fixture::new();
    let api = f.remote("api");
    let other = f.remote("other");
    f.jj(&["bookmark", "create", "main"]);
    f.jj(&["git", "remote", "add", "origin", "/missing-root-remote.git"]);
    let remotes = f.project(&["git", "remote", "list"]);
    assert_eq!(remotes.lines().count(), 1);
    assert!(remotes.starts_with("origin "));
    assert!(remotes.contains("api.git"));
    f.project(&["git", "push", "--all"]);
    assert_eq!(
        f.git(&api, &["ls-tree", "-r", "--name-only", "main"]),
        "a\n"
    );
    assert!(
        !f.run(&other, "git", &["rev-parse", "--verify", "main"])
            .status
            .success()
    );
    f.project(&["git", "fetch"]);
    assert_eq!(
        f.project(&["file", "show", "-r", "main@origin", "a"]),
        "base api\n"
    );
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "remote_bookmarks(exact:main, exact:origin)",
            "-T",
            "description"
        ]),
        "base\n"
    );
    assert!(
        !f.unchecked(&[
            "--in-project",
            "api",
            "git",
            "push",
            "--bookmark",
            "main#other"
        ])
        .status
        .success()
    );
    f.project(&["git", "push", "--bookmark", "main", "--dry-run"]);
}

#[test]
fn project_coordinates_survive_sparse_remapping_and_absent_checkout() {
    let f = Fixture::new();
    f.jj(&["sparse", "set", "root:packages/api"]);
    f.jj(&["sparse", "map", "set", "packages/api=."]);
    assert!(f.repo.join("a").exists());
    assert!(!f.repo.join("packages/api/a").exists());
    assert_eq!(f.project(&["file", "list", "glob:*"]), "a\n");
    assert_eq!(f.project(&["file", "show", "a"]), "base api\n");
    assert_eq!(
        f.project(&["file", "list", "-T", "path.absolute()"]),
        f.repo.join("a").to_str().unwrap()
    );
    assert_eq!(
        f.jj(&["--in-project", "other", "file", "show", "b"]),
        "base other\n"
    );
}

#[test]
fn bookmark_creation_and_movement_keep_root_and_other_projects_unchanged() {
    let f = Fixture::new();
    f.jj(&["bookmark", "create", "topic"]);
    f.jj(&["bookmark", "create", "topic#other"]);
    f.project(&["bookmark", "create", "topic"]);
    assert_eq!(
        f.project(&["bookmark", "list", "topic", "-T", "name"]),
        "topic#api"
    );
    f.jj(&["new", "-m", "new api"]);
    f.write("packages/api/a", "new\n");
    f.project(&["bookmark", "set", "topic"]);
    assert_eq!(
        f.project(&["log", "--no-graph", "-r", "topic", "-T", "description"]),
        "new api\n"
    );
    assert_eq!(
        f.jj(&[
            "log",
            "--no-graph",
            "-r",
            "topic | topic#other",
            "-T",
            "description"
        ]),
        "base\n"
    );
    assert!(
        !f.unchecked(&["--in-project", "api", "bookmark", "set", "topic#other"])
            .status
            .success()
    );
}

#[test]
fn content_revsets_and_templates_agree_on_project_scope() {
    let f = Fixture::new();
    f.jj(&["new", "-m", "foreign"]);
    f.write("other/b", "outside needle\n");
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@ & empty()",
            "-T",
            "description"
        ]),
        "foreign\n"
    );
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@ & diff_lines(needle)",
            "-T",
            "description"
        ]),
        ""
    );
    f.conflict("other/b");
    assert_eq!(
        f.project(&[
            "log",
            "--no-graph",
            "-r",
            "@ & conflicts()",
            "-T",
            "description"
        ]),
        ""
    );
    assert_eq!(
        f.jj(&[
            "log",
            "--no-graph",
            "-r",
            "@ & conflicts()",
            "-T",
            "description"
        ]),
        "merge\n"
    );
}

#[test]
fn neutral_merge_parent_cannot_hide_project_deletion() {
    let f = Fixture::new();
    f.jj(&["new", "root()", "-m", "foreign branch"]);
    f.write("foreign/file", "foreign\n");
    f.jj(&["bookmark", "create", "foreign"]);
    f.jj(&["new", "base", "foreign", "-m", "delete API in merge"]);
    fs::remove_dir_all(f.repo.join("packages/api")).unwrap();
    f.jj(&["new", "-m", "working"]);
    let history = f.project(&["log", "--no-graph", "-T", "description"]);
    assert!(history.contains("delete API in merge\n"), "{history}");
    assert!(!history.contains("foreign branch\n"), "{history}");
}

#[test]
fn project_context_preserves_repository_publication_policy() {
    let f = Fixture::new();
    let api = f.remote("api");
    f.jj(&["bookmark", "create", "private-root"]);
    let output = f.unchecked(&[
        "--in-project",
        "api",
        "--config",
        "git.private-commits='private-root'",
        "git",
        "push",
        "--bookmark",
        "main",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("private"));
    assert!(
        !f.run(&api, "git", &["rev-parse", "--verify", "main"])
            .status
            .success()
    );
}

#[test]
fn reference_templates_expose_scope_and_delivered_content() {
    let f = Fixture::new();
    let api = f.remote("api");
    f.project(&["git", "push", "--bookmark", "main"]);
    assert_eq!(
        f.jj(&["project", "list"]),
        format!(
            "api: packages/api (origin {})\nother: other\n",
            api.display()
        )
    );
    assert_eq!(
        f.jj(&[
            "project",
            "list",
            "-T",
            r#"self.label() ++ remotes.map(|r| " " ++ r.qualified_name() ++ " " ++ r.representation()).join("") ++ "\n""#,
        ]),
        "api origin#api whole\nother\n"
    );

    let origin = || {
        let rows = f.jj(&[
            "bookmark",
            "list",
            "--all-remotes",
            "-T",
            r#"separate("|", name, unscoped_name, remote, if(project, project.name(), "(root)"), synced, tracking_content_synced) ++ "\n""#,
        ]);
        assert!(rows.contains("base|base|(root)|"), "{rows}");
        rows.lines()
            .find(|row| row.contains("|origin|"))
            .unwrap()
            .to_owned()
    };
    assert_eq!(origin(), "main#api|main|origin|api|true|true");
    // Repository history outside the project moves the reference, not its content.
    f.jj(&["new"]);
    f.write("other/b", "outside\n");
    f.jj(&["bookmark", "set", "main#api", "-r", "@"]);
    assert_eq!(origin(), "main#api|main|origin|api|false|true");
    f.write("packages/api/a", "inside\n");
    assert_eq!(origin(), "main#api|main|origin|api|false|false");
}
