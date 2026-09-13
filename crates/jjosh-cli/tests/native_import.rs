#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

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
            .env("JOSH_EXPERIMENTAL_FEATURES", "1")
            .output()
            .unwrap_or_else(|err| panic!("failed to run jjosh {args:?}: {err}"))
    }

    #[track_caller]
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

    fn add_project_remote(&self, name: &str, url: &Path, project: &str) {
        self.jj(&["git", "remote", "add", name, url.to_str().unwrap()]);
        self.jj(&["projection", "remote", "attach", name, project]);
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
    // Recorded source workspace selection must travel in the bundle without
    // becoming the destination workspace's selection after namespacing.
    a.jj(&["sparse", "set", "file:left.txt"]);
    assert!(!a.path.join("base.txt").exists());
    a.jj(&["sparse", "map", "set", "left.txt=visible-left.txt"]);
    assert!(!a.path.join("left.txt").exists());
    assert_eq!(
        fs::read_to_string(a.path.join("visible-left.txt")).unwrap(),
        "left\n"
    );
    // Concurrent layout choices are native view conflicts, not commit-tree
    // conflicts. Export must retain every referenced configuration object.
    let layout_base = a.operation_id();
    a.jj(&["sparse", "map", "set", "left.txt=other-left.txt"]);
    a.jj(&[
        "--at-op",
        &layout_base,
        "sparse",
        "map",
        "set",
        "left.txt=third-left.txt",
    ]);
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
        let remote_names = || {
            target.jj(&[
                "bookmark",
                "list",
                "--all-remotes",
                "-T",
                r#"if(remote && remote != "git", name ++ "@" ++ remote ++ "\n")"#,
            ])
        };
        let before_sync = remote_names();
        target.jj(&["git", "export"]);
        target.jj(&["git", "import"]);
        assert_eq!(remote_names(), before_sync);

        assert_eq!(target.log("@", "commit_id"), target_checkout);
        assert!(!target.path.join("a").exists());
        assert!(!target.path.join("b").exists());
        assert_eq!(target.graph("::workspace/default#a ~ root()"), a_graph);
        assert_eq!(target.graph("::workspace/default#b ~ root()"), b_graph);
        assert_eq!(target.change_id("main#a"), a_main);
        assert_eq!(target.change_id("main#b"), b_main);
        assert_eq!(
            target.change_id(&format!("{empty_change} & empty()")),
            empty_change
        );
        assert_eq!(target.log("main#a", "description"), description);
        assert_eq!(
            target.log("main#a", "parents.map(|c| c.change_id()).join(\",\")"),
            parents
        );
        assert_eq!(
            target.log("::main#a & ::main#b", "commit_id"),
            target.log("root()", "commit_id")
        );
        assert_eq!(
            target.jj(&["workspace", "list", "-T", "name ++ \"\\n\""]),
            "default\n"
        );
        assert!(target.jj(&["git", "remote", "list"]).is_empty());

        target.jj(&["new", "workspace/default#a", "workspace/default#b"]);
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
        assert_eq!(target.graph("::workspace/default#a ~ root()"), a_graph);
        assert_eq!(target.graph("::workspace/default#b ~ root()"), b_graph);
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
        assert_eq!(target.graph("bookmarks(exact:\"main#b\")"), divergent_graph);
        assert_eq!(
            target.jj(&["bookmark", "list", "main#b", "-T", ref_template]),
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
        assert_eq!(target.change_id("main#a & conflicts()"), conflict_change);
        assert_eq!(
            target.jj(&["file", "show", "-r", "main#a", "a/conflict.txt"]),
            conflict_text
        );

        target.jj(&["new", "workspace/default#a", "workspace/default#b"]);
        assert_eq!(
            target.log("@ & conflicts()", "commit_id"),
            target.log("@", "commit_id")
        );
        target.write("a/conflict.txt", "resolved in destination\n");
        target.jj(&["status"]);
        assert!(target.log("@ & conflicts()", "commit_id").is_empty());
        assert_eq!(target.change_id("main#a & conflicts()"), conflict_change);
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
            "main#b",
            "-r",
            "description(substring:\"current left\")",
        ]);
        assert_eq!(target.change_id("main#b"), divergent_change);
        assert_eq!(
            target.log("main#b", "description.first_line()"),
            "current left"
        );
        assert_eq!(
            target.jj(&["bookmark", "list", "main#b", "-T", "conflict"]),
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
    source.jj(&["sparse", "set", "file:file.txt"]);
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
    let invalid_sparse = packages.path().join("invalid-sparse.bundle");
    let missing_sparse = packages.path().join("missing-sparse.bundle");
    let mismatched_sparse = packages.path().join("mismatched-sparse.bundle");
    let unknown_sparse = packages.path().join("unknown-sparse-field.bundle");
    for (path, corruption) in [
        (&unsupported, "version"),
        (&truncated, "pack"),
        (&invalid_sparse, "sparse"),
        (&missing_sparse, "missing-sparse"),
        (&mismatched_sparse, "mismatched-sparse"),
        (&unknown_sparse, "unknown-sparse"),
    ] {
        let mut archive = tar::Archive::new(Cursor::new(&original));
        let mut writer = tar::Builder::new(fs::File::create(path).unwrap());
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().into_owned();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).unwrap();
            if name == Path::new("manifest.json") && corruption != "pack" {
                let mut manifest: serde_json::Value = serde_json::from_slice(&contents).unwrap();
                match corruption {
                    "version" => manifest["version"] = serde_json::json!(1_000_000),
                    "sparse" => {
                        manifest["view"]["wc_sparse_patterns"] =
                            serde_json::json!({"default": ["not-an-object-id"]});
                    }
                    "missing-sparse" => {
                        manifest["working_copy_patterns"] = serde_json::json!({});
                    }
                    "mismatched-sparse" => {
                        let objects = manifest["working_copy_patterns"].as_object_mut().unwrap();
                        let id = objects.keys().next().unwrap().clone();
                        let object = objects.remove(&id).unwrap();
                        let wrong_id = "00".repeat(64);
                        objects.insert(wrong_id.clone(), object);
                        manifest["view"]["wc_sparse_patterns"] =
                            serde_json::json!({"default": [wrong_id]});
                    }
                    "unknown-sparse" => {
                        let objects = manifest["working_copy_patterns"].as_object_mut().unwrap();
                        objects.values_mut().next().unwrap()["unknown"] = serde_json::json!(true);
                    }
                    _ => unreachable!(),
                }
                contents = serde_json::to_vec(&manifest).unwrap();
            } else if corruption == "pack" && name == Path::new("objects.pack") {
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
        for path in [
            &malformed,
            &unsupported,
            &truncated,
            &invalid_sparse,
            &missing_sparse,
            &mismatched_sparse,
            &unknown_sparse,
        ] {
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

fn native_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Native Workflow Test",
            "-c",
            "user.email=native@example.invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn direct_native_import_preserves_recorded_state_without_rebuilding_source_index() {
    let source = NativeRepo::new();
    source.jj(&[
        "config",
        "set",
        "--repo",
        "git.write-change-id-header",
        "false",
    ]);
    source.write("value.txt", "recorded\n");
    source.jj(&["describe", "-m", "native identity without a Git header"]);
    source.bookmark("main");
    let graph = source.graph("root()..@");
    let operation = source.operation_id();
    let association = source.path.join(".jj/repo/index/op_links").join(&operation);
    fs::remove_file(&association).unwrap();
    source.write("value.txt", "unsnapshotted\n");
    let target = NativeRepo::new();
    target.write("existing.txt", "existing monorepo\n");
    target.jj(&["describe", "-m", "existing monorepo"]);
    target.jj(&[
        "native",
        "import",
        "--source",
        &format!("app={}", source.path.display()),
    ]);
    assert!(!association.exists(), "source index was rebuilt");
    assert_eq!(
        fs::read_to_string(source.path.join("value.txt")).unwrap(),
        "unsnapshotted\n"
    );
    assert_eq!(target.graph("root()..main#app"), graph);
    assert_eq!(target.jj(&["file", "list"]), "existing.txt\n");
    assert_eq!(
        target.jj(&["file", "show", "-r", "main#app", "app/value.txt"]),
        "recorded\n"
    );
    let imported = target.log("main#app", "commit_id");
    target.jj(&["git", "export"]);
    target.jj(&["git", "import"]);
    assert_eq!(target.log("main#app", "commit_id"), imported);
    assert!(
        !target
            .unchecked(&[
                "native",
                "import",
                "--source",
                &format!("app={}", source.path.display())
            ])
            .status
            .success()
    );
    assert!(!association.exists());
}

#[test]
fn scope_suffix_convention_uses_native_tracking_and_project_publication() {
    let source = NativeRepo::new();
    source.write("value.txt", "base\n");
    source.jj(&["describe", "-m", "source"]);
    source.bookmark("main");
    source.jj(&["tag", "set", "v1"]);
    let remotes = tempfile::tempdir().unwrap();
    let upstream = remotes.path().join("upstream.git");
    let fork = remotes.path().join("fork.git");
    for remote in [&upstream, &fork] {
        native_git(
            remotes.path(),
            &["init", "--bare", remote.to_str().unwrap()],
        );
    }
    source.jj(&["git", "remote", "add", "origin", fork.to_str().unwrap()]);
    source.jj(&[
        "git",
        "push",
        "--remote",
        "origin",
        "--bookmark",
        "main",
        "--tag",
        "v1",
    ]);
    let mono = NativeRepo::new();
    mono.write("root.txt", "root base\n");
    mono.jj(&["describe", "-m", "monorepo"]);
    mono.bookmark("main");
    let root_main = mono.log("main", "commit_id");
    let store_type = fs::read(mono.path.join(".jj/repo/op_store/type")).unwrap();
    mono.jj(&[
        "native",
        "import",
        "--source",
        &format!("alpha={}", source.path.display()),
    ]);
    let scoped = "main#alpha";
    assert_eq!(mono.change_id(scoped), source.change_id("main"));
    assert_eq!(
        mono.log("v1#alpha", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert_eq!(
        mono.log("workspace/default#alpha", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert_eq!(
        mono.log("main#alpha@alpha-origin", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert_eq!(
        mono.log("v1#alpha@alpha-origin", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert_eq!(mono.log("main", "commit_id"), root_main);
    mono.jj(&["new", "@", scoped, "-m", "composition"]);

    mono.add_project_remote("alpha-upstream", &upstream, "alpha");
    mono.add_project_remote("alpha-origin", &fork, "alpha");
    let publish = |remote: &str| {
        mono.jj(&[
            "git",
            "push",
            "--remote",
            remote,
            "--bookmark",
            scoped,
            "--allow-empty-description",
        ]);
    };
    let fetch = |remote: &str| {
        mono.jj(&["git", "fetch", "--remote", remote, "--branch", "main"]);
    };
    mono.jj(&["bookmark", "track", "main#alpha@alpha-origin"]);
    for label in ["upstream", "origin"] {
        let remote = format!("alpha-{label}");
        fetch(&remote);
        mono.jj(&["bookmark", "track", &format!("{scoped}@alpha-{label}")]);
        publish(&remote);
        fetch(&remote);
        assert_eq!(
            mono.log(
                &format!("tracked_remote_bookmarks({scoped}, alpha-{label})"),
                "commit_id"
            ),
            mono.log(scoped, "commit_id")
        );
    }
    let old = mono.log(scoped, "commit_id");
    mono.write("alpha/value.txt", "project change\n");
    mono.write("root.txt", "unpublished root change\n");
    mono.jj(&["describe", "-m", "project and monorepo changes"]);
    mono.bookmark(scoped);
    let local = mono.log(scoped, "commit_id");
    fetch("alpha-upstream");
    assert_eq!(mono.log(scoped, "commit_id"), local);
    publish("alpha-origin");
    assert_eq!(
        native_git(&fork, &["show", "main:value.txt"]),
        "project change\n"
    );
    assert_eq!(
        native_git(&fork, &["ls-tree", "-r", "--name-only", "main"]),
        "value.txt\n"
    );
    assert_eq!(native_git(&upstream, &["show", "main:value.txt"]), "base\n");
    fetch("alpha-origin");
    assert_eq!(
        mono.log(&format!("{scoped}@alpha-origin"), "commit_id"),
        local
    );
    assert_eq!(
        mono.log(&format!("{scoped}@alpha-upstream"), "commit_id"),
        old
    );
    assert_eq!(mono.log("main", "commit_id"), root_main);
    assert_eq!(
        fs::read_to_string(mono.path.join("root.txt")).unwrap(),
        "unpublished root change\n"
    );
    mono.jj(&["git", "export"]);
    mono.jj(&["git", "import"]);
    assert_eq!(mono.log(scoped, "commit_id"), local);
    assert_eq!(
        fs::read(mono.path.join(".jj/repo/op_store/type")).unwrap(),
        store_type
    );
}

#[test]
fn scope_suffix_convention_rejects_an_occupied_name_namespace() {
    let source = NativeRepo::new();
    let mono = NativeRepo::new();
    mono.bookmark("\"existing#alpha\"");
    let before = mono.operation_id();
    let output = mono.unchecked(&[
        "native",
        "import",
        "--source",
        &format!("alpha={}", source.path.display()),
    ]);
    assert!(!output.status.success());
    assert_eq!(mono.operation_id(), before);
}

#[test]
fn native_partial_publication_returns_to_canonical_change_and_accepts_contributions() {
    let alpha = NativeRepo::new();
    alpha.jj(&[
        "config",
        "set",
        "--repo",
        "git.write-change-id-header",
        "false",
    ]);
    alpha.write("value.txt", "alpha base\n");
    alpha.jj(&["describe", "-m", "alpha"]);
    alpha.bookmark("main");
    let beta = NativeRepo::new();
    beta.write("value.txt", "beta base\n");
    beta.jj(&["describe", "-m", "beta"]);
    beta.bookmark("main");
    let mono = NativeRepo::new();
    mono.write("root.txt", "monorepo root\n");
    mono.jj(&["describe", "-m", "monorepo root"]);
    mono.jj(&[
        "native",
        "import",
        "--source",
        &format!("alpha={}", alpha.path.display()),
        "--source",
        &format!("beta={}", beta.path.display()),
    ]);
    mono.jj(&["new", "@", "main#alpha", "main#beta", "-m", "composition"]);
    mono.jj(&["new", "-m", "one change across projects"]);
    mono.write("alpha/value.txt", "alpha local\n");
    mono.write("beta/value.txt", "beta local\n");
    mono.jj(&["status"]);
    let canonical = mono.log("@", "commit_id");
    let change = mono.change_id("@");
    let remotes = tempfile::tempdir().unwrap();
    let upstream = remotes.path().join("upstream.git");
    let fork = remotes.path().join("fork.git");
    let beta_remote = remotes.path().join("beta.git");
    for remote in [&upstream, &fork, &beta_remote] {
        native_git(
            remotes.path(),
            &["init", "--bare", remote.to_str().unwrap()],
        );
    }
    mono.add_project_remote("alpha-upstream", &upstream, "alpha");
    mono.add_project_remote("alpha-origin", &fork, "alpha");
    mono.add_project_remote("beta-origin", &beta_remote, "beta");
    let push_alpha = |remote: &str, branch: &str| {
        mono.jj(&[
            "git",
            "push",
            "--remote",
            remote,
            "--named",
            &format!("{branch}#alpha=@"),
            "--allow-empty-description",
        ]);
    };
    push_alpha("alpha-upstream", "topic");
    push_alpha("alpha-origin", "review");
    assert_eq!(
        native_git(
            remotes.path(),
            &[
                "--git-dir",
                upstream.to_str().unwrap(),
                "rev-parse",
                "topic"
            ]
        ),
        native_git(
            remotes.path(),
            &["--git-dir", fork.to_str().unwrap(), "rev-parse", "review"]
        ),
    );
    mono.jj(&[
        "git",
        "fetch",
        "--remote",
        "alpha-upstream",
        "--branch",
        "topic",
    ]);
    assert_eq!(
        mono.log("topic#alpha@alpha-upstream", "commit_id"),
        canonical
    );
    assert_eq!(mono.change_id("@"), change);
    assert!(mono.log("divergent()", "commit_id").is_empty());
    mono.jj(&["git", "export"]);
    mono.jj(&["git", "import"]);
    assert_eq!(
        mono.log("topic#alpha@alpha-upstream", "commit_id"),
        canonical
    );

    let contributor = remotes.path().join("contributor");
    native_git(
        remotes.path(),
        &[
            "clone",
            "--branch",
            "topic",
            upstream.to_str().unwrap(),
            contributor.to_str().unwrap(),
        ],
    );
    fs::write(
        contributor.join("contribution.txt"),
        "external contribution\n",
    )
    .unwrap();
    native_git(&contributor, &["add", "."]);
    native_git(&contributor, &["commit", "-m", "external contribution"]);
    native_git(&contributor, &["push", "origin", "HEAD:topic"]);
    let before_fetch = mono.operation_id();
    mono.jj(&[
        "git",
        "fetch",
        "--remote",
        "alpha-upstream",
        "--branch",
        "topic",
    ]);
    let received = mono.log("topic#alpha@alpha-upstream", "commit_id");
    assert_eq!(
        mono.log(
            "topic#alpha@alpha-upstream",
            "parents.map(|p| p.commit_id()).join(\",\")"
        ),
        canonical
    );
    assert_eq!(
        mono.jj(&["file", "show", "-r", &received, "beta/value.txt"]),
        "beta local\n"
    );
    assert_eq!(
        mono.jj(&["file", "show", "-r", &received, "root.txt"]),
        "monorepo root\n"
    );
    assert_eq!(
        mono.log("@", "commit_id"),
        canonical,
        "fetch must not choose an integration policy"
    );
    mono.jj(&["op", "restore", &before_fetch]);
    // In a non-colocated workspace, export the restored jj state before
    // importing Git refs again, just as with ordinary jj remote bookmarks.
    mono.jj(&["git", "export"]);
    mono.jj(&["git", "import"]);
    assert_eq!(
        mono.log("topic#alpha@alpha-upstream", "commit_id"),
        canonical
    );
    mono.jj(&[
        "git",
        "fetch",
        "--remote",
        "alpha-upstream",
        "--branch",
        "topic",
    ]);
    assert_eq!(
        mono.log("topic#alpha@alpha-upstream", "commit_id"),
        received
    );
    mono.jj(&["new", "topic#alpha@alpha-upstream", "-m", "local followup"]);
    mono.write("alpha/value.txt", "local conflicting edit\n");
    mono.jj(&["status"]);
    fs::write(contributor.join("value.txt"), "external conflicting edit\n").unwrap();
    native_git(
        &contributor,
        &["commit", "-am", "external conflicting edit"],
    );
    native_git(&contributor, &["push", "origin", "HEAD:topic"]);
    mono.jj(&[
        "git",
        "fetch",
        "--remote",
        "alpha-upstream",
        "--branch",
        "topic",
    ]);
    mono.jj(&[
        "new",
        "@",
        "topic#alpha@alpha-upstream",
        "-m",
        "native integration",
    ]);
    assert_eq!(mono.log("@", "conflict"), "true");
    assert_eq!(
        fs::read_to_string(mono.path.join("beta/value.txt")).unwrap(),
        "beta local\n"
    );
    // An unrelated project's conflict must not prevent publication of Beta.
    mono.jj(&[
        "git",
        "push",
        "--remote",
        "beta-origin",
        "--named",
        "feature/cross#beta=@",
        "--allow-empty-description",
        "--allow-conflicts",
    ]);
    assert_eq!(
        native_git(
            remotes.path(),
            &[
                "--git-dir",
                beta_remote.to_str().unwrap(),
                "ls-tree",
                "-r",
                "--name-only",
                "feature/cross"
            ]
        ),
        "value.txt\n"
    );
    assert_eq!(
        native_git(
            remotes.path(),
            &[
                "--git-dir",
                beta_remote.to_str().unwrap(),
                "show",
                "feature/cross:value.txt"
            ]
        ),
        "beta local\n"
    );
    assert!(
        !mono
            .unchecked(&[
                "git",
                "push",
                "--remote",
                "alpha-origin",
                "--named",
                "conflicted#alpha=@",
                "--allow-empty-description",
            ])
            .status
            .success()
    );
    let integration_change = mono.change_id("@");
    mono.write("alpha/value.txt", "resolved with jj\n");
    mono.jj(&["status"]);
    assert_eq!(mono.change_id("@"), integration_change);
    assert_eq!(mono.log("@", "conflict"), "false");
    push_alpha("alpha-origin", "resolved");
    mono.jj(&["util", "gc", "--expire", "now"]);
    mono.jj(&[
        "git",
        "push",
        "--remote",
        "alpha-origin",
        "--bookmark",
        "resolved#alpha",
        "--allow-empty-description",
        "--dry-run",
    ]);
}

#[test]
fn native_boundary_migration_preserves_rewrites_and_old_version_intake() {
    for colocated in [false, true] {
        let source = NativeRepo::with_colocation(colocated);
        source.write("value.txt", "original\n");
        source.jj(&["describe", "-m", "original"]);
        source.bookmark("main");
        source.jj(&["git", "export"]);
        let raw = source.log("@", "commit_id");
        let mono = NativeRepo::with_colocation(colocated);
        mono.jj(&[
            "native",
            "import",
            "--source",
            &format!("app={}", source.path.display()),
        ]);
        let original = mono.log("main#app", "commit_id");
        let git_dir = mono.path.join(if colocated {
            ".git"
        } else {
            ".jj/repo/store/git"
        });
        let git = |args: &[&str]| {
            let mut command = vec!["--git-dir", git_dir.to_str().unwrap()];
            command.extend_from_slice(args);
            native_git(&mono.path, &command)
        };
        // Recreate the old on-disk representation, including its duplicated
        // source-local Git observation. Migration must not change user branches.
        git(&[
            "update-ref",
            &format!("refs/remotes/jjosh-native-app/origin/{raw}"),
            &original,
        ]);
        git(&["update-ref", "refs/remotes/app-git/main#app", &original]);
        git(&[
            "update-ref",
            "-d",
            &format!("refs/jjosh/native/app/origin/{raw}"),
        ]);
        mono.jj(&["git", "import"]);
        let graph = mono.graph("all()");
        mono.jj(&["native", "migrate"]);
        assert_eq!(mono.graph("all()"), graph);
        let remote_names = || {
            mono.jj(&[
                "bookmark",
                "list",
                "--all-remotes",
                "-T",
                r#"if(remote && remote != "git", name ++ "@" ++ remote ++ "\n")"#,
            ])
        };
        assert_eq!(remote_names(), "");
        mono.jj(&["git", "import"]);
        assert_eq!(remote_names(), "");
        mono.jj(&["edit", "main#app"]);
        mono.write("app/value.txt", "rewritten locally\n");
        mono.jj(&["status"]);
        let rewritten = mono.log("@", "commit_id");
        assert_ne!(rewritten, original);
        assert_eq!(mono.log("main#app", "commit_id"), rewritten);
        mono.add_project_remote("app-upstream", &source.path, "app");
        mono.jj(&[
            "git",
            "fetch",
            "--remote",
            "app-upstream",
            "--branch",
            "main",
        ]);
        assert_eq!(mono.log("main#app@app-upstream", "commit_id"), original);
        assert_eq!(mono.log("main#app", "commit_id"), rewritten);
        assert_eq!(
            fs::read_to_string(mono.path.join("app/value.txt")).unwrap(),
            "rewritten locally\n"
        );
        let remote = mono.temp.path().join("publication.git");
        native_git(&mono.path, &["init", "--bare", remote.to_str().unwrap()]);
        mono.add_project_remote("app-review", &remote, "app");
        mono.jj(&[
            "git",
            "push",
            "--remote",
            "app-review",
            "--named",
            "topic#app=@",
            "--allow-empty-description",
        ]);
        mono.jj(&["util", "gc", "--expire", "now"]);
        mono.jj(&[
            "git",
            "fetch",
            "--remote",
            "app-review",
            "--branch",
            "topic",
        ]);
        assert_eq!(
            mono.jj(&[
                "file",
                "show",
                "-r",
                "topic#app@app-review",
                "app/value.txt"
            ]),
            "rewritten locally\n"
        );
        assert_eq!(
            remote_names(),
            "main#app@app-upstream\ntopic#app@app-review\n"
        );
    }
}

#[test]
fn native_fetch_grafts_by_change_id_onto_linked_suffix_history() {
    let source = NativeRepo::new();
    source.write("value.txt", "base\n");
    source.jj(&["describe", "-m", "base"]);
    source.bookmark("main");
    source.jj(&["new", "-m", "topic"]);
    source.write("value.txt", "topic\n");
    source.jj(&["describe", "-m", "topic"]);
    source.bookmark("topic");
    source.jj(&["git", "export"]);
    let git_dir = source.path.join(".jj/repo/store/git");

    let dest = NativeRepo::new();
    dest.jj(&[
        "projection",
        "remote",
        "add",
        "jj-upstream",
        git_dir.to_str().unwrap(),
        ":/",
        "--project",
        "jj",
        "--mount",
        "jj",
        "--base",
        "main",
    ]);
    dest.jj(&["git", "fetch", "--remote", "jj-upstream", "--branch", "main"]);
    dest.jj(&["new", "@", "main#jj@jj-upstream"]);
    let linked_main = dest.log("main#jj@jj-upstream", "commit_id");
    assert_eq!(
        dest.change_id("main#jj@jj-upstream"),
        source.change_id("main")
    );

    dest.add_project_remote("jj-local", &source.path, "jj");
    dest.jj(&["git", "fetch", "--remote", "jj-local", "--branch", "topic"]);
    let fetched = "topic#jj@jj-local";
    assert_eq!(dest.change_id(fetched), source.change_id("topic"));
    assert_eq!(dest.log(&format!("{fetched}-"), "commit_id"), linked_main);
    assert_eq!(
        dest.jj(&["file", "show", "-r", fetched, "jj/value.txt"]),
        "topic\n"
    );
    assert!(dest.log("divergent()", "commit_id").is_empty());

    let first = dest.log(fetched, "commit_id");
    dest.jj(&["git", "fetch", "--remote", "jj-local", "--branch", "topic"]);
    assert_eq!(dest.log(fetched, "commit_id"), first);

    source.write("value.txt", "amended\n");
    source.jj(&["describe", "-m", "amended topic"]);
    dest.jj(&["git", "fetch", "--remote", "jj-local", "--branch", "topic"]);
    assert_eq!(dest.change_id(fetched), source.change_id("topic"));
    assert_ne!(dest.log(fetched, "commit_id"), first);
    assert_eq!(dest.log(&format!("{fetched}-"), "commit_id"), linked_main);
    assert_eq!(
        dest.jj(&["file", "show", "-r", fetched, "jj/value.txt"]),
        "amended\n"
    );
}

#[test]
fn native_fetch_records_a_new_version_when_filtered_ancestor_differs() {
    let source = NativeRepo::new();
    source.write("value.txt", "base\n");
    source.jj(&["describe", "-m", "base"]);
    source.bookmark("main");
    source.jj(&["git", "export"]);
    let git_dir = source.path.join(".jj/repo/store/git");

    let dest = NativeRepo::new();
    dest.jj(&[
        "projection",
        "remote",
        "add",
        "jj-upstream",
        git_dir.to_str().unwrap(),
        ":/",
        "--project",
        "jj",
        "--mount",
        "jj",
        "--base",
        "main",
    ]);
    dest.jj(&["git", "fetch", "--remote", "jj-upstream", "--branch", "main"]);
    dest.jj(&["new", "@", "main#jj@jj-upstream"]);
    let linked_main = dest.log("main#jj@jj-upstream", "commit_id");
    let main_change = source.change_id("main");

    source.jj(&["new", "-r", "main", "-m", "tmp"]);
    source.write("value.txt", "drifted\n");
    source.jj(&["squash", "--into", "main", "--use-destination-message"]);
    source.jj(&["new", "-r", "main", "-m", "topic"]);
    source.write("value.txt", "topic\n");
    source.jj(&["describe", "-m", "topic"]);
    source.bookmark("topic");

    dest.add_project_remote("jj-local", &source.path, "jj");
    dest.jj(&["git", "fetch", "--remote", "jj-local", "--branch", "topic"]);
    let fetched = "topic#jj@jj-local";
    assert_eq!(dest.change_id(fetched), source.change_id("topic"));
    assert_eq!(dest.change_id(&format!("{fetched}-")), main_change);
    assert_ne!(dest.log(&format!("{fetched}-"), "commit_id"), linked_main);
    assert_eq!(dest.log("main#jj@jj-upstream", "commit_id"), linked_main);
    assert_eq!(
        dest.jj(&["file", "show", "-r", &format!("{fetched}-"), "jj/value.txt"]),
        "drifted\n"
    );
    assert_eq!(
        dest.jj(&["file", "show", "-r", fetched, "jj/value.txt"]),
        "topic\n"
    );
    assert!(
        dest.log("divergent()", "change_id")
            .contains(main_change.trim()),
        "source main and linked main are two versions of one change"
    );
}

#[test]
fn native_import_fetch_push_use_nested_mounts() {
    let source = NativeRepo::new();
    source.write("file.txt", "nested\n");
    source.jj(&["describe", "-m", "source"]);
    source.bookmark("main");

    let dest = NativeRepo::new();
    dest.write("keep.txt", "root\n");
    dest.jj(&["describe", "-m", "monorepo"]);
    dest.jj(&[
        "native",
        "import",
        "--source",
        &format!("alpha={}", source.path.display()),
        "--mount",
        "alpha=vendor/alpha",
    ]);
    assert!(!dest.path.join("vendor").exists());
    dest.jj(&["new", "@", "main#alpha"]);
    assert_eq!(
        fs::read_to_string(dest.path.join("vendor/alpha/file.txt")).unwrap(),
        "nested\n"
    );
    assert_eq!(
        fs::read_to_string(dest.path.join("keep.txt")).unwrap(),
        "root\n"
    );

    source.write("file.txt", "updated\n");
    source.jj(&["describe", "-m", "updated"]);
    dest.add_project_remote("alpha-local", &source.path, "alpha");
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "alpha-local",
        "--branch",
        "main",
    ]);
    let fetched = "main#alpha@alpha-local";
    assert_eq!(dest.change_id(fetched), source.change_id("main"));
    assert_eq!(
        dest.jj(&["file", "show", "-r", fetched, "vendor/alpha/file.txt"]),
        "updated\n"
    );

    let remotes = tempfile::tempdir().unwrap();
    let remote = remotes.path().join("alpha.git");
    native_git(
        remotes.path(),
        &["init", "--bare", remote.to_str().unwrap()],
    );
    dest.jj(&[
        "git",
        "remote",
        "add",
        "alpha-origin",
        remote.to_str().unwrap(),
    ]);
    dest.jj(&[
        "bookmark",
        "set",
        "main#alpha",
        "-r",
        fetched,
        "--allow-backwards",
    ]);
    dest.jj(&[
        "git",
        "push",
        "--remote",
        "alpha-origin",
        "--bookmark",
        "main#alpha",
        "--allow-empty-description",
    ]);
    let clone = remotes.path().join("clone");
    native_git(
        remotes.path(),
        &[
            "clone",
            "--branch",
            "main",
            remote.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read_to_string(clone.join("file.txt")).unwrap(),
        "updated\n"
    );
    assert!(!clone.join("vendor").exists());
    assert!(!clone.join("keep.txt").exists());
}

#[test]
fn native_import_rejects_occupied_or_overlapping_mounts() {
    let source = NativeRepo::new();
    source.write("file.txt", "src\n");
    source.jj(&["describe", "-m", "source"]);
    source.bookmark("main");
    let other = NativeRepo::new();
    other.write("file.txt", "other\n");
    other.jj(&["describe", "-m", "other"]);
    other.bookmark("main");

    let dest = NativeRepo::new();
    dest.write("vendor/alpha/blocked.txt", "occupied\n");
    dest.jj(&["describe", "-m", "occupied"]);
    let occupied = dest.unchecked(&[
        "native",
        "import",
        "--source",
        &format!("alpha={}", source.path.display()),
        "--mount",
        "alpha=vendor/alpha",
    ]);
    assert!(!occupied.status.success());

    let overlap = NativeRepo::new();
    let overlapped = overlap.unchecked(&[
        "native",
        "import",
        "--source",
        &format!("alpha={}", source.path.display()),
        "--source",
        &format!("beta={}", other.path.display()),
        "--mount",
        "alpha=vendor",
        "--mount",
        "beta=vendor/beta",
    ]);
    assert!(!overlapped.status.success());
}

#[test]
fn native_project_names_are_jj_symbols() {
    let dotted = NativeRepo::new();
    dotted.write("file.txt", "dot\n");
    dotted.jj(&["describe", "-m", "dotted"]);
    dotted.bookmark("main");
    let chinese = NativeRepo::new();
    chinese.write("file.txt", "han\n");
    chinese.jj(&["describe", "-m", "chinese"]);
    chinese.bookmark("main");

    let dest = NativeRepo::new();
    dest.jj(&[
        "native",
        "import",
        "--source",
        &format!("foo.bar={}", dotted.path.display()),
        "--source",
        &format!("项目={}", chinese.path.display()),
    ]);
    dest.jj(&["new", "main#foo.bar", "main#项目"]);
    assert_eq!(
        fs::read_to_string(dest.path.join("foo.bar/file.txt")).unwrap(),
        "dot\n"
    );
    assert_eq!(
        fs::read_to_string(dest.path.join("项目/file.txt")).unwrap(),
        "han\n"
    );

    let hash = dest.unchecked(&[
        "native",
        "import",
        "--source",
        &format!("a#b={}", dotted.path.display()),
    ]);
    assert!(!hash.status.success());
}
