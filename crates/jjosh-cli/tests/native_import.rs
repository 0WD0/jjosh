#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct NativeRepo {
    temp: tempfile::TempDir,
    path: PathBuf,
}

impl NativeRepo {
    fn new() -> Self {
        Self::with_colocation(false)
    }

    fn with_colocation(colocated: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        fs::create_dir(&path).unwrap();
        fs::create_dir(temp.path().join("home")).unwrap();
        fs::write(
            temp.path().join("config.toml"),
            "[revset-aliases]\n'immutable_heads()' = 'root()'\n",
        )
        .unwrap();
        let repo = Self { temp, path };
        if colocated {
            repo.jj(&["git", "init", "--colocate"]);
        } else {
            repo.jj(&["git", "init", "--no-colocate"]);
        }
        assert_eq!(repo.path.join(".git").exists(), colocated);
        repo
    }

    fn unchecked(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjosh"))
            .args([
                "--no-pager",
                "--color=never",
                "--config",
                "user.name=Native Import Test",
                "--config",
                "user.email=native@example.com",
            ])
            .args(args)
            .current_dir(&self.path)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.temp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("home"))
            .env("JJ_CONFIG", self.temp.path().join("config.toml"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LANG", "C.UTF-8")
            .output()
            .unwrap_or_else(|err| panic!("failed to run jjosh {args:?}: {err}"))
    }

    fn jj(&self, args: &[&str]) -> String {
        let output = self.unchecked(args);
        assert!(
            output.status.success(),
            "jjosh {args:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn log(&self, revision: &str, template: &str) -> String {
        self.jj(&[
            "--ignore-working-copy",
            "log",
            "--no-graph",
            "-r",
            revision,
            "-T",
            template,
        ])
    }

    fn change_id(&self, revision: &str) -> String {
        self.log(revision, "change_id")
    }

    fn operation_id(&self) -> String {
        self.jj(&[
            "--ignore-working-copy",
            "op",
            "log",
            "--no-graph",
            "--limit",
            "1",
            "-T",
            "id",
        ])
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn bookmark(&self, name: &str) {
        self.jj(&["bookmark", "set", name]);
    }

    fn graph(&self, revision: &str) -> Vec<String> {
        let output = self.log(
            revision,
            r#"separate("|", change_id, parents.map(|c| c.change_id()).join(","), author.name(), author.email(), author.timestamp(), committer.name(), committer.email(), committer.timestamp(), description.first_line(), empty) ++ "\n""#,
        );
        let mut rows: Vec<_> = output.lines().map(str::to_owned).collect();
        rows.sort();
        rows
    }

    fn state(&self) -> (String, Vec<String>, String, BTreeMap<PathBuf, Vec<u8>>) {
        fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if path == root && entry.file_name() == ".jj" {
                    continue;
                }
                let path = entry.path();
                // Unpublished immutable objects and their private keep refs
                // may remain after a failed transaction; user refs must not.
                let relative = path.strip_prefix(root).unwrap();
                if relative.starts_with(".git/objects")
                    || relative.starts_with(".git/refs/jj")
                    || relative.starts_with(".git/logs/refs/jj")
                {
                    continue;
                }
                if entry.file_type().unwrap().is_dir() {
                    collect(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        collect(&self.path, &self.path, &mut files);
        (
            self.operation_id(),
            self.graph("all()"),
            self.jj(&["--ignore-working-copy", "bookmark", "list", "--all-remotes"]),
            files,
        )
    }

    fn export(&self, path: &Path) {
        self.jj(&["native", "export", path.to_str().unwrap()]);
    }

    fn import(&self, a: &Path, b: &Path) {
        self.jj(&[
            "native",
            "import",
            "--source",
            &format!("a={}", a.display()),
            "--source",
            &format!("b={}", b.display()),
        ]);
    }
}

#[test]
fn imports_self_contained_native_graphs_without_checkout_or_source_snapshot() {
    let a = NativeRepo::new();
    a.write("base.txt", "base\n");
    a.jj(&["describe", "-m", "a base"]);
    a.bookmark("base");
    a.jj(&["new", "-m", "intentional empty"]);
    let empty_change = a.change_id("@");
    a.jj(&["new", "-m", "left"]);
    a.write("left.txt", "left\n");
    a.bookmark("left");
    a.jj(&["new", "base", "-m", "right"]);
    a.write("right.txt", "right\n");
    a.bookmark("right");
    // Deliberately reverse lexical order: parent order is native identity.
    a.jj(&["new", "right", "left", "-m", "ordered merge\n\nmerge body"]);
    a.write("merge.txt", "merge delta\n");
    a.bookmark("main");
    a.jj(&["status"]);

    let b = NativeRepo::new();
    b.write("file.txt", "independent root\n");
    b.jj(&["describe", "-m", "b base"]);
    b.bookmark("main");
    b.jj(&["status"]);
    let a_graph = a.graph("all() ~ root()");
    let b_graph = b.graph("all() ~ root()");
    let a_main = a.change_id("main");
    let b_main = b.change_id("main");
    let description = a.log("main", "description");
    let parents = format!("{},{}", a.change_id("right"), a.change_id("left"));
    // Export must use the recorded view, not implicitly snapshot this file.
    a.write("not-recorded.txt", "leave this in the source only\n");
    let a_before = a.state();
    let b_before = b.state();
    let packages = tempfile::tempdir().unwrap();
    let a_bundle = packages.path().join("a.bundle");
    let b_bundle = packages.path().join("b.bundle");
    a.export(&a_bundle);
    b.export(&b_bundle);
    assert_eq!(a.state(), a_before);
    assert_eq!(b.state(), b_before);
    fs::remove_dir_all(&a.path).unwrap();
    fs::remove_dir_all(&b.path).unwrap();

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        let target_before = target.state();
        target.jj(&["native", "inspect", a_bundle.to_str().unwrap()]);
        target.jj(&["native", "inspect", b_bundle.to_str().unwrap()]);
        assert_eq!(target.state(), target_before);
        let target_checkout = target.log("@", "commit_id");

        target.import(&a_bundle, &b_bundle);

        assert_eq!(target.log("@", "commit_id"), target_checkout);
        assert!(!target.path.join("a").exists());
        assert!(!target.path.join("b").exists());
        assert_eq!(target.graph("::a/workspace/default ~ root()"), a_graph);
        assert_eq!(target.graph("::b/workspace/default ~ root()"), b_graph);
        assert_eq!(target.change_id("a/main"), a_main);
        assert_eq!(target.change_id("b/main"), b_main);
        assert_eq!(
            target.change_id(&format!("{empty_change} & empty()")),
            empty_change
        );
        assert_eq!(target.log("a/main", "description"), description);
        assert_eq!(
            target.log("a/main", "parents.map(|c| c.change_id()).join(\",\")"),
            parents
        );
        assert_eq!(
            target.log("::a/main & ::b/main", "commit_id"),
            target.log("root()", "commit_id")
        );
        assert_eq!(
            target.jj(&["workspace", "list", "-T", "name ++ \"\\n\""]),
            "default\n"
        );
        assert!(target.jj(&["git", "remote", "list"]).is_empty());

        target.jj(&["new", "a/workspace/default", "b/workspace/default"]);
        for (path, contents) in [
            ("a/base.txt", "base\n"),
            ("a/left.txt", "left\n"),
            ("a/right.txt", "right\n"),
            ("a/merge.txt", "merge delta\n"),
            ("b/file.txt", "independent root\n"),
        ] {
            assert_eq!(
                fs::read_to_string(target.path.join(path)).unwrap(),
                contents
            );
        }
        assert!(!target.path.join("a/not-recorded.txt").exists());
        assert_eq!(target.path.join(".git").exists(), colocated);
        target.jj(&["status"]);
        assert_eq!(target.graph("::a/workspace/default ~ root()"), a_graph);
        assert_eq!(target.graph("::b/workspace/default ~ root()"), b_graph);
    }
}

#[test]
fn preserves_native_conflicts_and_current_divergence_not_evolution_history() {
    let a = NativeRepo::new();
    a.write("conflict.txt", "base\n");
    a.jj(&["describe", "-m", "conflict base"]);
    a.bookmark("base");
    a.jj(&["new", "base", "-m", "left"]);
    a.write("conflict.txt", "left version\n");
    a.bookmark("left");
    a.jj(&["new", "base", "-m", "right"]);
    a.write("conflict.txt", "right version\n");
    a.bookmark("right");
    a.jj(&["new", "left", "right", "-m", "unresolved merge"]);
    a.bookmark("main");
    a.jj(&["status"]);
    let conflict_change = a.change_id("@ & conflicts()");
    assert_eq!(conflict_change, a.change_id("@"));
    let conflict_text = a.jj(&["file", "show", "-r", "@", "conflict.txt"]);

    let b = NativeRepo::new();
    b.write("b.txt", "native versions\n");
    b.jj(&["describe", "-m", "obsolete evolution-only"]);
    let obsolete_commit = b.log("@", "commit_id");
    b.bookmark("main");
    b.jj(&["describe", "-m", "common predecessor"]);
    let fork = b.operation_id();
    let divergent_change = b.change_id("@");
    b.jj(&["describe", "-m", "current left"]);
    b.jj(&["--at-op", &fork, "describe", "-m", "current right"]);
    // Reconcile operation heads before export; both versions remain current.
    b.jj(&["status"]);
    let divergent_graph = b.graph("divergent()");
    assert_eq!(divergent_graph.len(), 2);
    let ref_template = r#"separate("|", conflict, removed_targets.map(|c| c.description().first_line()).join(","), added_targets.map(|c| c.description().first_line()).join(","))"#;
    let conflicted_ref = b.jj(&["bookmark", "list", "main", "-T", ref_template]);
    assert!(conflicted_ref.starts_with("true|"));
    let evolution_args = [
        "--ignore-working-copy",
        "evolog",
        "-r",
        "description(substring:\"current left\")",
        "--no-graph",
        "-T",
        "commit.commit_id() ++ \"\\n\"",
    ];
    assert!(
        b.jj(&evolution_args)
            .lines()
            .any(|id| id == obsolete_commit)
    );
    let a_before = a.state();
    let b_before = b.state();
    let packages = tempfile::tempdir().unwrap();
    let a_bundle = packages.path().join("a.bundle");
    let b_bundle = packages.path().join("b.bundle");
    a.export(&a_bundle);
    b.export(&b_bundle);
    assert_eq!(a.state(), a_before);
    assert_eq!(b.state(), b_before);
    fs::remove_dir_all(&a.path).unwrap();
    fs::remove_dir_all(&b.path).unwrap();

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        target.jj(&["native", "inspect", a_bundle.to_str().unwrap()]);
        target.jj(&["native", "inspect", b_bundle.to_str().unwrap()]);
        target.import(&a_bundle, &b_bundle);

        assert_eq!(target.graph("divergent()"), divergent_graph);
        assert_eq!(target.graph("bookmarks(b/main)"), divergent_graph);
        assert_eq!(
            target.jj(&["bookmark", "list", "b/main", "-T", ref_template]),
            conflicted_ref
        );
        assert!(
            target
                .log(
                    "description(substring:\"obsolete evolution-only\")",
                    "commit_id"
                )
                .is_empty()
        );
        assert!(
            !target
                .unchecked(&["--ignore-working-copy", "log", "-r", &obsolete_commit])
                .status
                .success()
        );
        assert_eq!(
            target.jj(&evolution_args),
            format!(
                "{}\n",
                target.log("description(substring:\"current left\")", "commit_id")
            )
        );
        assert_eq!(target.change_id("a/main & conflicts()"), conflict_change);
        assert_eq!(
            target.jj(&["file", "show", "-r", "a/main", "a/conflict.txt"]),
            conflict_text
        );

        target.jj(&["new", "a/workspace/default", "b/workspace/default"]);
        assert_eq!(
            target.log("@ & conflicts()", "commit_id"),
            target.log("@", "commit_id")
        );
        target.write("a/conflict.txt", "resolved in destination\n");
        target.jj(&["status"]);
        assert!(target.log("@ & conflicts()", "commit_id").is_empty());
        assert_eq!(target.change_id("a/main & conflicts()"), conflict_change);
        assert_eq!(
            target.jj(&["file", "show", "-r", "@", "a/conflict.txt"]),
            "resolved in destination\n"
        );
        assert_eq!(
            fs::read_to_string(target.path.join("b/b.txt")).unwrap(),
            "native versions\n"
        );
        target.jj(&[
            "bookmark",
            "set",
            "b/main",
            "-r",
            "description(substring:\"current left\")",
        ]);
        assert_eq!(target.change_id("b/main"), divergent_change);
        assert_eq!(
            target.log("b/main", "description.first_line()"),
            "current left"
        );
        assert_eq!(
            target.jj(&["bookmark", "list", "b/main", "-T", "conflict"]),
            "false"
        );
    }
}

#[test]
fn rejects_namespace_collisions_without_publishing_any_source() {
    let a = NativeRepo::new();
    a.write("a.txt", "a\n");
    a.jj(&["status"]);
    let b = NativeRepo::new();
    b.write("b.txt", "b\n");
    b.jj(&["status"]);
    // This source-local name would overwrite the generated workspace role.
    b.bookmark("workspace/default");
    let a_before = a.state();
    let b_before = b.state();
    let packages = tempfile::tempdir().unwrap();
    let a_bundle = packages.path().join("a.bundle");
    let b_bundle = packages.path().join("b.bundle");
    a.export(&a_bundle);
    b.export(&b_bundle);
    let a_source = format!("a={}", a_bundle.display());
    let b_source = format!("b={}", b_bundle.display());
    let repeated = format!("a={}", b_bundle.display());

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        let before = target.state();
        for second in [&b_source, &repeated] {
            let output = target.unchecked(&[
                "native", "import", "--source", &a_source, "--source", second,
            ]);
            assert!(!output.status.success(), "collision unexpectedly succeeded");
            assert_eq!(target.state(), before);
        }
    }
    assert_eq!(a.state(), a_before);
    assert_eq!(b.state(), b_before);
}

#[test]
fn rejects_invalid_bundles_atomically_and_never_overwrites_an_export() {
    let source = NativeRepo::new();
    source.write("file.txt", "recorded contents\n");
    source.jj(&["describe", "-m", "bundle validation"]);
    source.bookmark("main");
    source.write("unrecorded.txt", "must remain unrecorded\n");
    let source_before = source.state();
    let packages = tempfile::tempdir().unwrap();
    let valid = packages.path().join("valid.bundle");
    source.export(&valid);
    assert_eq!(source.state(), source_before);
    let original = fs::read(&valid).unwrap();

    assert!(
        !source
            .unchecked(&["native", "export", valid.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::read(&valid).unwrap(), original);
    assert_eq!(source.state(), source_before);

    let malformed = packages.path().join("malformed.bundle");
    fs::write(&malformed, b"not a native state package\n").unwrap();
    let unsupported = packages.path().join("unsupported.bundle");
    let truncated = packages.path().join("truncated.bundle");
    for (path, change_version) in [(&unsupported, true), (&truncated, false)] {
        let mut archive = tar::Archive::new(Cursor::new(&original));
        let mut writer = tar::Builder::new(fs::File::create(path).unwrap());
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().into_owned();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).unwrap();
            if change_version && name == Path::new("manifest.json") {
                let mut manifest: serde_json::Value = serde_json::from_slice(&contents).unwrap();
                manifest["version"] = serde_json::json!(1_000_000);
                contents = serde_json::to_vec(&manifest).unwrap();
            } else if !change_version && name == Path::new("objects.pack") {
                contents.truncate(contents.len() / 2);
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            writer
                .append_data(&mut header, name, Cursor::new(contents))
                .unwrap();
        }
        writer.finish().unwrap();
    }
    fs::remove_dir_all(&source.path).unwrap();

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        let before = target.state();
        let valid_source = format!("a={}", valid.display());
        for path in [&malformed, &unsupported, &truncated] {
            assert!(
                !target
                    .unchecked(&["native", "inspect", path.to_str().unwrap()])
                    .status
                    .success()
            );
            assert_eq!(target.state(), before);
            let invalid_source = format!("b={}", path.display());
            // A valid first source must not be published if the second fails.
            assert!(
                !target
                    .unchecked(&[
                        "native",
                        "import",
                        "--source",
                        &valid_source,
                        "--source",
                        &invalid_source,
                    ])
                    .status
                    .success()
            );
            assert_eq!(target.state(), before);
        }
    }
}
