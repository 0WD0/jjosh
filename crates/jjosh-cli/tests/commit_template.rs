// Isolated native repositories; no network or user configuration is needed.
#![cfg(unix)]

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

struct Fixture {
    temp: tempfile::TempDir,
    clock: Cell<u32>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("home")).unwrap();
        fs::create_dir(temp.path().join("repo")).unwrap();
        fs::write(
            temp.path().join("config.toml"),
            "[user]\nname = 'Template Test'\nemail = 'template@example.com'\n",
        )
        .unwrap();
        let fixture = Self {
            temp,
            clock: Cell::new(0),
        };
        fixture.jj(&["git", "init", "--no-colocate"]);
        fixture
    }

    fn path(&self) -> std::path::PathBuf {
        self.temp.path().join("repo")
    }

    fn jj_in(&self, cwd: &Path, args: &[&str]) -> String {
        let tick = self.clock.get();
        self.clock.set(tick + 1);
        let output = Command::new(env!("CARGO_BIN_EXE_jjosh"))
            .args(["--no-pager", "--color=never"])
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
            .env("LANG", "C.UTF-8")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn jj(&self, args: &[&str]) -> String {
        self.jj_in(&self.path(), args)
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn log(&self, revision: &str, template: &str) -> String {
        self.jj(&["log", "--no-graph", "-r", revision, "-T", template])
    }

    fn touched(&self, revision: &str) -> Vec<String> {
        serde_json::from_str(&self.log(revision, "json(touched_projects)")).unwrap()
    }

    fn two_projects(&self) {
        // Names deliberately sort in the opposite order to paths.
        self.jj(&["project", "add", "zeta", "--path", "libs/a"]);
        self.jj(&["project", "add", "alpha", "--path", "libs/b"]);
    }

    fn operation(&self) -> String {
        self.jj(&["op", "log", "--no-graph", "--limit", "1", "-T", "id"])
    }
}

#[test]
fn projects_follow_changed_paths_not_bookmarks_or_copy_sources() {
    let f = Fixture::new();
    f.two_projects();
    f.jj(&["project", "add", "absent", "--path", "libs/absent"]);
    f.write("libs/a/one", "one\n");
    f.write("libs/a/two", "two\n");
    f.write("libs/b/one", "one\n");
    f.write("outside", "outside\n");
    assert_eq!(f.touched("@"), ["alpha", "zeta"]);
    assert_eq!(
        f.log("@", "self.touched_projects().join(',')"),
        "alpha,zeta"
    );
    assert_eq!(
        f.jj_in(
            &f.path().join("libs/a"),
            &[
                "log",
                "-r",
                "@",
                "--no-graph",
                "-T",
                "touched_projects.join(',')"
            ],
        ),
        "alpha,zeta"
    );

    f.jj(&["new"]);
    fs::copy(f.path().join("libs/a/one"), f.path().join("libs/b/copy")).unwrap();
    f.jj(&["bookmark", "create", "misleading#zeta"]);
    assert_eq!(f.touched("@"), ["alpha"]);

    f.jj(&["new"]);
    fs::rename(f.path().join("libs/a/two"), f.path().join("libs/b/moved")).unwrap();
    assert_eq!(f.touched("@"), ["alpha", "zeta"]);

    f.jj(&["new"]);
    f.write("outside", "changed\n");
    f.write("libs/ab/not-a-project", "prefix lookalike\n");
    assert!(f.touched("@").is_empty());
    f.jj(&["new", "-m", "Only a description"]);
    assert!(f.touched("@").is_empty());
    assert!(f.touched("root()").is_empty());
}

#[test]
fn deletions_modes_symlinks_and_directory_replacements_are_changes() {
    let f = Fixture::new();
    f.two_projects();
    f.write("libs/a/run", "#!/bin/sh\n");
    f.write("libs/b/data", "data\n");
    f.jj(&["new"]);
    fs::set_permissions(
        f.path().join("libs/a/run"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(f.touched("@"), ["zeta"]);

    f.jj(&["new"]);
    fs::remove_dir_all(f.path().join("libs/b")).unwrap();
    assert_eq!(f.touched("@"), ["alpha"]);

    f.jj(&["new"]);
    std::os::unix::fs::symlink("a", f.path().join("libs/b")).unwrap();
    assert_eq!(f.touched("@"), ["alpha"]);

    f.jj(&["new"]);
    fs::remove_dir_all(f.path().join("libs/a")).unwrap();
    f.write("libs/a", "directory replaced by a file\n");
    assert_eq!(f.touched("@"), ["zeta"]);
}

#[test]
fn merged_parent_baseline_excludes_inherited_changes_and_includes_resolution() {
    let f = Fixture::new();
    f.two_projects();
    f.write("libs/a/file", "base\n");
    f.write("libs/b/file", "base\n");
    let base = f.log("@", "commit_id");
    f.jj(&["new", "-m", "Left"]);
    f.write("libs/a/file", "left\n");
    let left = f.log("@", "commit_id");
    f.jj(&["new", &base, "-m", "Right"]);
    f.write("libs/b/file", "right\n");
    let right = f.log("@", "commit_id");

    f.jj(&["new", &left, &right, "-m", "Pure merge"]);
    assert!(f.touched("@").is_empty());
    f.write("libs/b/file", "extra merge edit\n");
    assert_eq!(f.touched("@"), ["alpha"]);

    f.jj(&["new", &base, "-m", "Conflicting right"]);
    f.write("libs/a/file", "conflicting\n");
    let conflicting = f.log("@", "commit_id");
    f.jj(&["new", &left, &conflicting, "-m", "Resolution"]);
    assert_eq!(f.log("@", "conflict"), "true");
    assert!(f.touched("@").is_empty());
    f.write("libs/a/file", "resolved\n");
    assert_eq!(f.log("@", "conflict"), "false");
    assert_eq!(f.touched("@"), ["zeta"]);
}

#[test]
fn classification_uses_the_loaded_view_and_not_the_working_copy_layout() {
    let f = Fixture::new();
    f.write("libs/a/file", "a\n");
    f.write("libs/b/file", "b\n");
    let commit = f.log("@", "commit_id");
    assert!(f.touched(&commit).is_empty());
    f.two_projects();
    let operation = f.operation();
    assert_eq!(f.touched(&commit), ["alpha", "zeta"]);
    f.jj(&["project", "rename", "zeta", "renamed"]);
    assert_eq!(f.touched(&commit), ["alpha", "renamed"]);
    assert_eq!(
        f.jj(&[
            "--at-op",
            &operation,
            "log",
            "-r",
            &commit,
            "--no-graph",
            "-T",
            "touched_projects.join(',')",
        ]),
        "alpha,zeta"
    );
    f.jj(&["project", "remove", "renamed"]);
    assert_eq!(f.touched(&commit), ["alpha"]);
    f.jj(&["op", "restore", &operation, "--what", "repo"]);
    assert_eq!(f.touched(&commit), ["alpha", "zeta"]);

    f.jj(&["sparse", "set", "root:libs/a"]);
    f.jj(&["sparse", "map", "set", "libs/a=."]);
    assert!(!f.path().join("libs/b/file").exists());
    assert_eq!(fs::read_to_string(f.path().join("file")).unwrap(), "a\n");
    assert_eq!(f.touched(&commit), ["alpha", "zeta"]);
    f.jj(&["new"]);
    f.write("file", "edit through the physical root\n");
    assert_eq!(f.touched("@"), ["zeta"]);
}

#[test]
fn conflicted_project_definitions_remain_visible_without_guessing_names() {
    let f = Fixture::new();
    f.two_projects();
    f.write("libs/a/file", "a\n");
    f.write("libs/b/file", "b\n");
    let commit = f.log("@", "commit_id");
    let state: serde_json::Value =
        serde_json::from_str(&f.jj(&["project", "show", "zeta", "--json"])).unwrap();
    let id = state["projects"][0]["id"].as_str().unwrap();
    let operation = f.operation();
    for name in ["left", "right"] {
        f.jj(&["--at-op", &operation, "project", "rename", "zeta", name]);
    }
    // Loading the current head merges the two operation versions.
    f.jj(&["project", "list", "--json"]);
    let unresolved = format!("project:{}?", &id[..12]);
    assert_eq!(f.touched(&commit), ["alpha", unresolved.as_str()]);
    let output = f.jj(&["log", "-r", &commit]);
    assert!(
        output.contains(&format!("[alpha, {unresolved}]")),
        "{output}"
    );
    assert!(!output.contains("<Error"), "{output}");
}

#[test]
fn different_versions_of_one_change_have_separate_summaries() {
    let f = Fixture::new();
    f.two_projects();
    f.write("libs/a/file", "a\n");
    let old = f.log("@", "commit_id");
    let change = f.log("@", "change_id");
    fs::remove_file(f.path().join("libs/a/file")).unwrap();
    f.write("libs/b/file", "b\n");
    let new = f.log("@", "commit_id");
    assert_eq!(f.log("@", "change_id"), change);
    let output = f.log(
        &format!("{old} | {new}"),
        "commit_id ++ ':' ++ touched_projects.join(',') ++ ':' ++ self.touched_projects().join(',') ++ '\n'",
    );
    let lines: std::collections::BTreeSet<_> = output.lines().collect();
    assert_eq!(
        lines,
        std::collections::BTreeSet::from([
            format!("{old}:zeta:zeta").as_str(),
            format!("{new}:alpha:alpha").as_str(),
        ])
    );
}

#[test]
fn nested_summaries_include_every_touched_scope_without_hiding_parent_edits() {
    let f = Fixture::new();
    f.two_projects();
    f.jj(&["project", "add", "bundle", "--path", "libs"]);
    f.write("libs/a/file", "a\n");
    f.write("libs/b/file", "b\n");
    f.write("libs/glue", "glue\n");
    let base = f.log("@", "commit_id");
    assert_eq!(f.touched("@"), ["alpha", "bundle", "zeta"]);
    f.jj(&["new"]);
    f.write("libs/a/file", "child edit\n");
    assert_eq!(f.touched("@"), ["bundle", "zeta"]);
    f.write("libs/glue", "parent edit\n");
    assert_eq!(f.touched("@"), ["bundle", "zeta"]);
    let left = f.log("@", "commit_id");
    f.jj(&["new", &base]);
    f.write("libs/b/file", "sibling edit\n");
    let right = f.log("@", "commit_id");
    f.jj(&["new", &left, &right]);
    assert!(f.touched("@").is_empty());
    f.write("libs/glue", "merge edit\n");
    assert_eq!(f.touched("@"), ["bundle"]);
    f.jj(&["sparse", "set", "root:libs/a"]);
    f.jj(&["sparse", "map", "set", "libs/a=."]);
    assert_eq!(f.touched(&left), ["bundle", "zeta"]);

    let before = f.operation();
    f.jj(&["project", "remove", "bundle"]);
    assert_eq!(f.touched(&left), ["zeta"]);
    f.jj(&["op", "restore", &before, "--what", "repo"]);
    f.jj(&["project", "remove", "zeta"]);
    assert_eq!(f.touched(&left), ["bundle"]);
    f.jj(&["op", "restore", &before, "--what", "repo"]);
    f.jj(&["project", "rename", "bundle", "renamed"]);
    assert_eq!(f.touched(&left), ["renamed", "zeta"]);
    assert_eq!(
        f.jj(&[
            "--at-op",
            &before,
            "log",
            "-r",
            &left,
            "--no-graph",
            "-T",
            "touched_projects.join(',')"
        ]),
        "bundle,zeta"
    );
}

#[test]
fn default_log_adds_only_nonempty_summaries_and_respects_overrides() {
    let f = Fixture::new();
    let plain = f.jj(&["log", "-r", "@ | root()"]);
    assert_eq!(
        plain,
        f.jj(&["log", "-r", "@ | root()", "-T", "builtin_log_compact"])
    );
    f.two_projects();
    f.write("libs/a/file", "a\n");
    f.jj(&["describe", "-m", "Change a file"]);
    let output = f.jj(&["log", "-r", "@"]);
    assert!(
        output.lines().next().unwrap().ends_with("[zeta]"),
        "{output}"
    );
    assert_eq!(f.log("@", "description.first_line()"), "Change a file");
    assert!(!f.log("@", "builtin_log_compact").contains("[zeta]"));
    let custom = serde_json::to_string("description.first_line() ++ \"\\n\"").unwrap();
    f.jj(&["config", "set", "--repo", "templates.log", &custom]);
    assert_eq!(f.jj(&["log", "--no-graph", "-r", "@"]), "Change a file\n");
    // The field remains available even when a higher-priority config replaces the layout.
    assert_eq!(f.log("@", "format_touched_projects(self)"), "[zeta]");
}
