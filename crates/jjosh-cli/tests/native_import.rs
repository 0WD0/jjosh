#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
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

    fn import(&self, a: &Path, b: &Path) {
        self.jj(&[
            "project",
            "import",
            "--nested",
            &format!("a={}", a.display()),
            "--nested",
            &format!("b={}", b.display()),
        ]);
    }

    fn add_project_remote(&self, name: &str, url: &Path, project: &str) {
        self.jj(&[
            "git",
            "remote",
            "add",
            name,
            url.to_str().unwrap(),
            "--project",
            project,
            "--whole",
        ]);
    }

    fn physical_remote(&self, project: &str, alias: &str) -> String {
        let state: serde_json::Value =
            serde_json::from_str(&self.jj(&["project", "show", project, "--json"])).unwrap();
        let identity = state["projects"][0]["remotes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|remote| remote["candidates"][0]["definition"]["name"] == alias)
            .unwrap()["connection"]
            .as_str()
            .unwrap();
        let git_dir = self.path.join(".jj/repo/store/git");
        native_git(&git_dir, &["remote"])
            .lines()
            .find(|name| {
                native_git(
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

    fn project(&self, name: &str) -> serde_json::Value {
        let state: serde_json::Value =
            serde_json::from_str(&self.jj(&["project", "show", name, "--json"])).unwrap();
        state["projects"][0].clone()
    }

    #[track_caller]
    fn assert_rejected_without_changes(&self, args: &[&str]) {
        let git_dir = PathBuf::from(self.jj(&["--ignore-working-copy", "git", "root"]).trim());
        let config = || {
            (
                fs::read(git_dir.join("config")).unwrap(),
                fs::read(self.path.join(".jj/repo/config.toml")).ok(),
            )
        };
        let refs = || {
            native_git(
                &git_dir,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            )
            .lines()
            .filter(|line| !line.starts_with("refs/jj/"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
        };
        let before = self.state();
        let config_before = config();
        let refs_before = refs();
        let output = self.unchecked(args);
        assert!(
            !output.status.success(),
            "jjosh {args:?} unexpectedly succeeded:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            self.state(),
            before,
            "rejected command changed repository state"
        );
        assert_eq!(
            config(),
            config_before,
            "rejected command changed configuration"
        );
        assert_eq!(
            refs(),
            refs_before,
            "rejected command changed Git references"
        );
    }
}

#[test]
fn mixed_import_recovers_failed_mirror_installation_before_retry() {
    let upstream = NativeRepo::new();
    upstream.write("value.txt", "upstream\n");
    upstream.jj(&["describe", "-m", "upstream"]);
    upstream.bookmark("main");
    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "alpha"]);
    source.add_project_remote("origin", &upstream.path, "alpha");
    source.jj(&["git", "fetch", "--remote", "origin#alpha"]);
    let physical = source.physical_remote("alpha", "origin");
    let source_before = source.state();
    upstream.jj(&["git", "export"]);
    let nested = NativeRepo::new();
    nested.jj(&[
        "git",
        "remote",
        "add",
        "origin",
        upstream.path.join(".jj/repo/store/git").to_str().unwrap(),
    ]);
    nested.jj(&["git", "fetch", "--remote", "origin", "--branch", "main"]);
    nested.jj(&["bookmark", "set", "main", "-r", "main@origin"]);
    nested.jj(&["bookmark", "track", "main@origin"]);
    let nested_before = nested.state();
    let nested_change = nested.change_id("main");
    let target = NativeRepo::new();
    target.jj(&[
        "git",
        "remote",
        "add",
        "existing",
        upstream.path.to_str().unwrap(),
    ]);
    native_git(
        &target.path.join(".jj/repo/store/git"),
        &["config", "remote.existing.tagOpt", "--no-tags"],
    );
    let git_dir = target.path.join(".jj/repo/store/git");
    let config_before = fs::read(git_dir.join("config")).unwrap();
    let repo_config_before = fs::read(target.path.join(".jj/repo/config.toml")).ok();
    let state_before = target.state();
    let refs = || {
        native_git(
            &git_dir,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
        .lines()
        .filter(|line| !line.starts_with("refs/jj/"))
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    let refs_before = refs();
    let lock = git_dir.join(format!("refs/remotes/{physical}/main#alpha.lock"));
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "held by another writer\n").unwrap();
    let specification = format!("archive={}", source.path.display());
    let nested_specification = format!("outer={}", nested.path.display());
    let args = [
        "project",
        "import",
        "--preserve",
        &specification,
        "--nested",
        &nested_specification,
    ];
    assert!(!target.unchecked(&args).status.success());
    assert_eq!(target.operation_id(), state_before.0);
    fs::remove_file(lock).unwrap();
    target.jj(&["git", "remote", "recover", "--rollback"]);
    assert_eq!(target.state(), state_before);
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), config_before);
    assert_eq!(
        fs::read(target.path.join(".jj/repo/config.toml")).ok(),
        repo_config_before
    );
    // Offline relation anchors must roll back along with preserved connections.
    assert_eq!(refs(), refs_before);
    assert_eq!(source.state(), source_before);
    assert_eq!(nested.state(), nested_before);

    target.jj(&args);
    assert_eq!(target.change_id("main#outer"), nested_change);
    assert_eq!(target.change_id("main#outer@origin"), nested_change);
    assert_eq!(
        target.jj(&["file", "show", "-r", "main#outer", "outer/value.txt"]),
        "upstream\n"
    );
    let imported = target.log("main#alpha@origin", "commit_id");
    target.jj(&["git", "fetch", "--remote", "origin#alpha"]);
    assert_eq!(target.log("main#alpha@origin", "commit_id"), imported);
    target.jj(&["git", "import"]);
    assert_eq!(target.change_id("main#outer@origin"), nested_change);
    assert_eq!(source.state(), source_before);
    assert_eq!(nested.state(), nested_before);
}

#[test]
fn outer_import_recovers_failed_mirror_installation_before_retry() {
    let upstream = NativeRepo::new();
    upstream.write("value.txt", "upstream\n");
    upstream.jj(&["describe", "-m", "upstream"]);
    upstream.bookmark("main");
    upstream.jj(&["git", "export"]);
    let endpoint = upstream.path.join(".jj/repo/store/git");
    let source = NativeRepo::new();
    source.jj(&["git", "remote", "add", "origin", endpoint.to_str().unwrap()]);
    source.jj(&["git", "fetch", "--remote", "origin", "--branch", "main"]);
    source.jj(&["bookmark", "set", "main", "-r", "main@origin"]);
    source.jj(&["bookmark", "track", "main@origin"]);
    let source_before = source.state();
    let source_change = source.change_id("main");

    let target = NativeRepo::new();
    target.write("keep.txt", "destination\n");
    target.jj(&["describe", "-m", "destination"]);
    target.bookmark("existing");
    target.jj(&["git", "export"]);
    target.jj(&[
        "git",
        "remote",
        "add",
        "existing",
        endpoint.to_str().unwrap(),
    ]);
    target.jj(&["git", "fetch", "--remote", "existing", "--branch", "main"]);
    let git_dir = target.path.join(".jj/repo/store/git");
    native_git(&git_dir, &["config", "remote.existing.tagOpt", "--no-tags"]);
    let config_before = fs::read(git_dir.join("config")).unwrap();
    let repo_config_before = fs::read(target.path.join(".jj/repo/config.toml")).ok();
    let state_before = target.state();
    let refs = || {
        native_git(
            &git_dir,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
        .lines()
        .filter(|line| !line.starts_with("refs/jj/"))
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    let refs_before = refs();
    // A real Git lock fails the local mirror export after private import
    // provenance has been installed, without predicting generated remote IDs.
    let lock = git_dir.join("refs/heads/main#archive.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "held by another writer\n").unwrap();
    let specification = format!("archive={}", source.path.display());
    let args = ["project", "import", "--nested", &specification];
    assert!(!target.unchecked(&args).status.success());
    assert_eq!(target.operation_id(), state_before.0);
    fs::remove_file(lock).unwrap();
    target.jj(&["git", "remote", "recover", "--rollback"]);
    assert_eq!(target.state(), state_before);
    assert_eq!(fs::read(git_dir.join("config")).unwrap(), config_before);
    assert_eq!(
        fs::read(target.path.join(".jj/repo/config.toml")).ok(),
        repo_config_before
    );
    // Includes offline relation anchors, excluding only immutable-object
    // keep refs that need not be removed by rollback.
    assert_eq!(refs(), refs_before);
    assert_eq!(source.state(), source_before);

    target.jj(&args);
    assert_eq!(target.change_id("main#archive"), source_change);
    let imported = target.log("main#archive", "commit_id");
    assert_eq!(target.log("main#archive@origin", "commit_id"), imported);
    assert_eq!(
        native_git(&git_dir, &["rev-parse", "refs/heads/main#archive"]).trim(),
        imported
    );
    assert_eq!(
        target.jj(&["file", "show", "-r", "main#archive", "archive/value.txt",]),
        "upstream\n"
    );
    target.jj(&["git", "import"]);
    assert_eq!(target.log("main#archive@origin", "commit_id"), imported);
    assert_eq!(source.state(), source_before);
}

#[test]
fn local_import_preserves_root_and_nested_scoped_aliases_without_activating_endpoints() {
    let seed = NativeRepo::new();
    seed.write("value.txt", "portable source\n");
    seed.jj(&["describe", "-m", "portable source"]);
    seed.bookmark("main");
    seed.jj(&["git", "export"]);
    let source = NativeRepo::new();
    source.jj(&[
        "git",
        "remote",
        "add",
        "origin",
        seed.path.join(".jj/repo/store/git").to_str().unwrap(),
    ]);
    source.jj(&["git", "fetch", "--remote", "origin", "--branch", "main"]);
    source.jj(&["project", "add", "inner", "--path", "inner"]);
    source.add_project_remote("origin", &seed.path, "inner");
    source.jj(&["git", "fetch", "--project", "inner", "--branch", "main"]);
    source.jj(&["bookmark", "track", "main#inner@origin"]);

    let target = NativeRepo::new();
    target.jj(&[
        "project",
        "import",
        "--nested",
        &format!("outer={}", source.path.display()),
    ]);
    assert_eq!(
        target.jj(&["file", "show", "-r", "main#outer@origin", "outer/value.txt"]),
        "portable source\n"
    );
    assert_eq!(
        target.jj(&[
            "file",
            "show",
            "-r",
            "main#inner#outer@\"origin@inner\"",
            "outer/inner/value.txt"
        ]),
        "portable source\n"
    );
    let state: serde_json::Value =
        serde_json::from_str(&target.jj(&["project", "show", "outer", "--json"])).unwrap();
    let aliases: std::collections::BTreeSet<_> = state["projects"][0]["remotes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|remote| {
            remote["candidates"][0]["definition"]["name"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert!(aliases.contains("origin"));
    assert!(aliases.contains("origin@inner"));
    assert!(target.jj(&["git", "remote", "list"]).is_empty());
    assert!(
        !target
            .unchecked(&["git", "fetch", "--project", "outer", "--remote", "origin"])
            .status
            .success()
    );
    target.jj(&["git", "export"]);
    target.jj(&["git", "import"]);
    assert_eq!(
        target.jj(&[
            "file",
            "show",
            "-r",
            "main#inner#outer@\"origin@inner\"",
            "outer/inner/value.txt"
        ]),
        "portable source\n"
    );
}

#[test]
fn imports_local_graphs_without_checkout_or_source_snapshot() {
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
    // Recorded source workspace selection must not become the destination
    // workspace's selection after namespacing.
    a.jj(&["sparse", "set", "file:left.txt"]);
    assert!(!a.path.join("base.txt").exists());
    a.jj(&["sparse", "map", "set", "left.txt=visible-left.txt"]);
    assert!(!a.path.join("left.txt").exists());
    assert_eq!(
        fs::read_to_string(a.path.join("visible-left.txt")).unwrap(),
        "left\n"
    );
    // Concurrent source layout choices must not affect the imported trees.
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
    b.jj(&["new", "-m", "unbookmarked source working copy"]);
    b.write("working-copy.txt", "recorded working copy\n");
    b.jj(&["status"]);
    let a_graph = a.graph("all() ~ root()");
    let b_graph = b.graph("all() ~ root()");
    let a_main = a.change_id("main");
    let b_main = b.change_id("main");
    let a_wc = a.change_id("@");
    let b_wc = b.change_id("@");
    let description = a.log("main", "description");
    let parents = format!("{},{}", a.change_id("right"), a.change_id("left"));
    // Import must use the recorded view, not implicitly snapshot this file.
    a.write("not-recorded.txt", "leave this in the source only\n");
    let a_before = a.state();
    let b_before = b.state();

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        let target_checkout = target.log("@", "commit_id");

        target.import(&a.path, &b.path);
        assert_eq!(a.state(), a_before);
        assert_eq!(b.state(), b_before);
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
        assert_eq!(target.graph(&format!("::{a_wc} ~ root()")), a_graph);
        assert_eq!(target.graph(&format!("::{b_wc} ~ root()")), b_graph);
        assert!(
            target
                .log("bookmarks(glob:\"workspace/*\")", "change_id")
                .is_empty()
        );
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

        target.jj(&["new", &a_wc, &b_wc]);
        for (path, contents) in [
            ("a/base.txt", "base\n"),
            ("a/left.txt", "left\n"),
            ("a/right.txt", "right\n"),
            ("a/merge.txt", "merge delta\n"),
            ("b/file.txt", "independent root\n"),
            ("b/working-copy.txt", "recorded working copy\n"),
        ] {
            assert_eq!(
                fs::read_to_string(target.path.join(path)).unwrap(),
                contents
            );
        }
        assert!(!target.path.join("a/not-recorded.txt").exists());
        assert_eq!(target.path.join(".git").exists(), colocated);
        target.jj(&["status"]);
        assert_eq!(target.graph(&format!("::{a_wc} ~ root()")), a_graph);
        assert_eq!(target.graph(&format!("::{b_wc} ~ root()")), b_graph);
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
    // Reconcile operation heads before import; both versions remain current.
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

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        target.import(&a.path, &b.path);
        assert_eq!(a.state(), a_before);
        assert_eq!(b.state(), b_before);

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

        target.jj(&["new", "main#a", "description(substring:\"current left\")"]);
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
fn mixed_import_preserves_project_identity_and_wraps_nested_history_in_one_operation() {
    let preserved = NativeRepo::new();
    preserved.jj(&["project", "add", "alpha", "--path", "lib"]);
    preserved.write("lib/value.txt", "preserved project\n");
    preserved.write("root.txt", "preserved root\n");
    preserved.jj(&["describe", "-m", "preserved source"]);
    preserved.bookmark("main#alpha");
    preserved.bookmark("root-main");
    preserved.jj(&["tag", "set", "v1#alpha"]);
    let preserved_project = preserved.project("alpha");
    let preserved_change = preserved.change_id("@");
    preserved.write("unrecorded.txt", "do not import or snapshot\n");
    let preserved_before = preserved.state();

    let nested = NativeRepo::new();
    nested.jj(&["project", "add", "beta", "--path", "pkg"]);
    nested.write("pkg/value.txt", "nested project\n");
    nested.jj(&["describe", "-m", "nested source"]);
    nested.bookmark("main");
    nested.bookmark("main#beta");
    nested.jj(&["tag", "set", "v1"]);
    let nested_project = nested.project("beta");
    let nested_change = nested.change_id("@");
    nested.write("unrecorded.txt", "leave nested source alone\n");
    let nested_before = nested.state();

    let target = NativeRepo::new();
    target.write("keep.txt", "destination checkout\n");
    target.jj(&["describe", "-m", "destination"]);
    let checkout = target.log("@", "commit_id");
    let checkout_files = target.state().3;
    let operations = || {
        target.jj(&[
            "--ignore-working-copy",
            "op",
            "log",
            "--no-graph",
            "-T",
            "id ++ \"\\n\"",
        ])
    };
    let operations_before = operations();
    target.jj(&[
        "project",
        "import",
        "--preserve",
        &format!("bundle={}", preserved.path.display()),
        "--nested",
        &format!("outer={}", nested.path.display()),
        "--mount",
        "bundle=vendor/preserved",
        "--mount",
        "outer=vendor/nested",
    ]);
    assert_eq!(
        operations().lines().skip(1).collect::<Vec<_>>(),
        operations_before.lines().collect::<Vec<_>>()
    );
    assert_eq!(target.log("@", "commit_id"), checkout);
    assert_eq!(target.state().3, checkout_files);
    assert_eq!(preserved.state(), preserved_before);
    assert_eq!(nested.state(), nested_before);

    let imported = target.project("alpha");
    assert_eq!(imported["id"], preserved_project["id"]);
    assert_eq!(
        imported["candidates"][0]["definition"]["path"],
        "vendor/preserved/lib"
    );
    let wrapper = target.project("outer");
    assert_ne!(wrapper["id"], nested_project["id"]);
    assert_ne!(wrapper["id"], preserved_project["id"]);
    assert_eq!(
        wrapper["candidates"][0]["definition"]["path"],
        "vendor/nested"
    );
    let projects: serde_json::Value =
        serde_json::from_str(&target.jj(&["project", "list", "--json"])).unwrap();
    assert_eq!(projects["projects"].as_array().unwrap().len(), 2);
    for reference in ["main#alpha", "bundle/root-main", "v1#alpha"] {
        assert_eq!(target.change_id(reference), preserved_change);
    }
    for reference in ["main#outer", "main#beta#outer", "v1#outer"] {
        assert_eq!(target.change_id(reference), nested_change);
    }
    for (revision, path, contents) in [
        (
            "main#alpha",
            "vendor/preserved/lib/value.txt",
            "preserved project\n",
        ),
        (
            "bundle/root-main",
            "vendor/preserved/root.txt",
            "preserved root\n",
        ),
        (
            "main#outer",
            "vendor/nested/pkg/value.txt",
            "nested project\n",
        ),
    ] {
        assert_eq!(target.jj(&["file", "show", "-r", revision, path]), contents);
    }
    for revision in ["bundle/root-main", "main#outer"] {
        assert!(
            !target
                .jj(&["file", "list", "-r", revision])
                .contains("unrecorded.txt")
        );
    }
}

#[test]
fn rejects_invalid_import_arguments_without_publishing_any_source() {
    let a = NativeRepo::new();
    a.write("a.txt", "a\n");
    a.jj(&["status"]);
    let b = NativeRepo::new();
    b.write("b.txt", "b\n");
    b.jj(&["status"]);
    let a_before = a.state();
    let b_before = b.state();
    let a_source = format!("a={}", a.path.display());
    let repeated = format!("a={}", b.path.display());

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        for (first, second) in [
            ("--nested", "--nested"),
            ("--preserve", "--preserve"),
            ("--nested", "--preserve"),
            ("--preserve", "--nested"),
        ] {
            target.assert_rejected_without_changes(&[
                "project", "import", first, &a_source, second, &repeated,
            ]);
        }
        target.assert_rejected_without_changes(&["project", "import"]);
        target.assert_rejected_without_changes(&[
            "project",
            "import",
            "--nested",
            &a_source,
            "--preserve",
            &format!("b={}", b.path.display()),
            "--mount",
            "missing=vendor/missing",
        ]);
    }
    assert_eq!(a.state(), a_before);
    assert_eq!(b.state(), b_before);
}

#[test]
fn imports_real_workspace_bookmark_without_synthesizing_working_copy_bookmarks() {
    let a = NativeRepo::new();
    a.write("a.txt", "unbookmarked source\n");
    a.jj(&["describe", "-m", "unbookmarked source"]);
    let a_wc = a.change_id("@");
    let b = NativeRepo::new();
    b.write("b.txt", "real bookmark\n");
    b.jj(&["describe", "-m", "real bookmark"]);
    b.bookmark("workspace/default");
    let b_bookmark = b.change_id("workspace/default");
    b.jj(&["new", "-m", "different working copy"]);
    b.write("wc.txt", "unbookmarked working copy\n");
    b.jj(&["status"]);
    let b_wc = b.change_id("@");
    let a_before = a.state();
    let b_before = b.state();

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        let checkout = target.log("@", "commit_id");
        target.import(&a.path, &b.path);
        assert_eq!(target.log("@", "commit_id"), checkout);
        assert_eq!(
            target.jj(&["bookmark", "list", "-T", "name ++ \"\\n\""]),
            "workspace/default#b\n"
        );
        assert_eq!(target.change_id("workspace/default#b"), b_bookmark);
        assert_eq!(
            target.jj(&["file", "show", "-r", "workspace/default#b", "b/b.txt"]),
            "real bookmark\n"
        );
        assert_eq!(target.change_id(&format!("{a_wc} & heads(all())")), a_wc);
        assert_eq!(target.change_id(&format!("{b_wc} & heads(all())")), b_wc);
        target.jj(&["new", &a_wc, &b_wc]);
        assert_eq!(
            fs::read_to_string(target.path.join("a/a.txt")).unwrap(),
            "unbookmarked source\n"
        );
        assert_eq!(
            fs::read_to_string(target.path.join("b/wc.txt")).unwrap(),
            "unbookmarked working copy\n"
        );
        assert_eq!(a.state(), a_before);
        assert_eq!(b.state(), b_before);
    }
}

#[test]
fn rejects_non_repository_sources_without_publishing_any_source() {
    let source = NativeRepo::new();
    source.write("file.txt", "recorded contents\n");
    source.jj(&["describe", "-m", "local source validation"]);
    source.bookmark("main");
    source.write("unrecorded.txt", "must remain unrecorded\n");
    let source_before = source.state();
    let invalid_sources = tempfile::tempdir().unwrap();
    let regular_file = invalid_sources.path().join("file");
    fs::write(&regular_file, b"not a repository\n").unwrap();
    let plain_directory = invalid_sources.path().join("directory");
    fs::create_dir(&plain_directory).unwrap();
    let missing = invalid_sources.path().join("missing");

    for colocated in [false, true] {
        let target = NativeRepo::with_colocation(colocated);
        let before = target.state();
        let valid_source = format!("a={}", source.path.display());
        for path in [&regular_file, &plain_directory, &missing] {
            let invalid_source = format!("b={}", path.display());
            // A valid first source must not be published if the second fails.
            assert!(
                !target
                    .unchecked(&[
                        "project",
                        "import",
                        "--nested",
                        &valid_source,
                        "--nested",
                        &invalid_source,
                    ])
                    .status
                    .success()
            );
            assert_eq!(target.state(), before);
            assert_eq!(source.state(), source_before);
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
        "project",
        "import",
        "--nested",
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
                "project",
                "import",
                "--nested",
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
        "project",
        "import",
        "--nested",
        &format!("alpha={}", source.path.display()),
    ]);
    let scoped = "main#alpha";
    assert_eq!(mono.change_id(scoped), source.change_id("main"));
    assert_eq!(
        mono.log("v1#alpha", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert!(
        mono.log("bookmarks(exact:\"workspace/default#alpha\")", "change_id")
            .is_empty()
    );
    assert_eq!(
        mono.log("main#alpha@origin", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert_eq!(
        mono.log("v1#alpha@origin", "commit_id"),
        mono.log(scoped, "commit_id")
    );
    assert_eq!(mono.log("main", "commit_id"), root_main);
    mono.jj(&["new", "@", scoped, "-m", "composition"]);

    mono.add_project_remote("upstream", &upstream, "alpha");
    mono.jj(&["git", "remote", "forget-observations", "origin#alpha"]);
    mono.add_project_remote("origin", &fork, "alpha");
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
        mono.jj(&[
            "git",
            "fetch",
            "--project",
            "alpha",
            "--remote",
            remote,
            "--branch",
            "main",
        ]);
    };
    mono.jj(&["bookmark", "track", "main#alpha@origin"]);
    for label in ["upstream", "origin"] {
        let remote = label.to_owned();
        fetch(&remote);
        mono.jj(&["bookmark", "track", &format!("{scoped}@{label}")]);
        publish(&remote);
        fetch(&remote);
        assert_eq!(
            mono.log(
                &format!("tracked_remote_bookmarks({scoped}, {label})"),
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
    fetch("upstream");
    assert_eq!(mono.log(scoped, "commit_id"), local);
    publish("origin");
    assert_eq!(
        native_git(&fork, &["show", "main:value.txt"]),
        "project change\n"
    );
    assert_eq!(
        native_git(&fork, &["ls-tree", "-r", "--name-only", "main"]),
        "value.txt\n"
    );
    assert_eq!(native_git(&upstream, &["show", "main:value.txt"]), "base\n");
    fetch("origin");
    assert_eq!(mono.log(&format!("{scoped}@origin"), "commit_id"), local);
    assert_eq!(mono.log(&format!("{scoped}@upstream"), "commit_id"), old);
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
        "project",
        "import",
        "--nested",
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
        "project",
        "import",
        "--nested",
        &format!("alpha={}", alpha.path.display()),
        "--nested",
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
        "alpha-upstream#alpha",
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
        "alpha-upstream#alpha",
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
        "alpha-upstream#alpha",
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
        "alpha-upstream#alpha",
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
            "project",
            "import",
            "--nested",
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
        let graph = mono.graph("all()");
        mono.jj(&[
            "project",
            "migrate",
            "--apply",
            "--native-source",
            "app=detached",
        ]);
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
            "app-upstream#app",
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
            "app-review#app",
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
fn native_migration_preserves_external_update_to_observed_mirror() {
    let mono = NativeRepo::new();
    mono.write("app/value.txt", "before\n");
    mono.jj(&["describe", "-m", "before"]);
    let external = mono.log("@", "commit_id");
    mono.jj(&["new"]);
    mono.write("app/value.txt", "canonical\n");
    mono.jj(&["describe", "-m", "canonical"]);
    mono.bookmark("main");
    mono.jj(&["git", "export"]);
    let canonical = mono.log("main", "commit_id");
    let git_dir = mono.path.join(".jj/repo/store/git");
    let git = |args: &[&str]| {
        let mut command = vec!["--git-dir", git_dir.to_str().unwrap()];
        command.extend_from_slice(args);
        native_git(&mono.path, &command)
    };
    let mirror = "refs/remotes/app-git/main";
    git(&["update-ref", mirror, &canonical]);
    mono.jj(&["git", "import"]);
    // The operation still records an exact local-target duplicate, but another
    // writer has since replaced the physical mirror with a different commit.
    git(&["update-ref", mirror, &external]);
    git(&[
        "update-ref",
        &format!("refs/remotes/jjosh-native-app/origin/{external}"),
        &canonical,
    ]);
    mono.jj(&["project", "migrate", "--apply"]);
    assert_eq!(git(&["rev-parse", mirror]).trim(), external);
    assert_eq!(mono.log("main", "commit_id"), canonical);
}

#[test]
fn obsolete_remote_key_retirement_preserves_current_bindings_and_publication() {
    let source = NativeRepo::new();
    source.write("value.txt", "source\n");
    source.jj(&["describe", "-m", "source"]);
    source.bookmark("main");
    source.jj(&["tag", "set", "v1"]);
    let upstream = source.temp.path().join("upstream.git");
    native_git(
        &source.path,
        &["init", "--bare", upstream.to_str().unwrap()],
    );
    source.jj(&["git", "remote", "add", "origin", upstream.to_str().unwrap()]);
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
    let publication = source.temp.path().join("publication.git");
    native_git(
        &source.path,
        &[
            "clone",
            "--bare",
            upstream.to_str().unwrap(),
            publication.to_str().unwrap(),
        ],
    );

    let mono = NativeRepo::new();
    mono.jj(&["project", "add", "app", "--path", "vendor/app"]);
    mono.jj(&[
        "git",
        "remote",
        "add",
        "source",
        upstream.to_str().unwrap(),
        "--push-url",
        publication.to_str().unwrap(),
        "--project",
        "app",
        "--whole",
    ]);
    mono.jj(&["git", "fetch", "--remote", "source#app"]);
    mono.jj(&["bookmark", "track", "main#app@source"]);
    mono.jj(&["new", "main#app@source", "-m", "local continuation"]);
    mono.write("vendor/app/value.txt", "published before migration\n");
    mono.jj(&["describe", "-m", "local continuation"]);
    mono.bookmark("main#app");
    // Establish publication evidence and a lease at the distinct push endpoint.
    mono.jj(&["git", "push", "--bookmark", "main#app"]);
    let git_dir = mono.path.join(".jj/repo/store/git");
    let git = |args: &[&str]| {
        let mut command = vec!["--git-dir", git_dir.to_str().unwrap()];
        command.extend_from_slice(args);
        native_git(&mono.path, &command)
    };
    let definitions: serde_json::Value =
        serde_json::from_str(&mono.jj(&["project", "show", "app", "--json"])).unwrap();
    let physical = mono.physical_remote("app", "source");
    let settings = git(&["config", "--local", "--null", "--list"]);
    let refs = || {
        git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
            "refs/jjosh",
        ])
    };
    let references = refs();
    let tags = mono.jj(&["tag", "list", "--all-remotes"]);

    // Different values, including malformed input, and duplicate remote sections
    // are all obsolete syntax, not permissions or new source definitions.
    let config_path = git_dir.join("config");
    let mut legacy_config = fs::read_to_string(&config_path).unwrap();
    legacy_config.push_str(&format!("\n[remote \"{physical}\"]\n\tjjosh-readOnly\n"));
    fs::write(&config_path, &legacy_config).unwrap();
    let state = mono.state();
    let rejected = mono.unchecked(&["git", "fetch", "--remote", "source#app", "--branch", "main"]);
    assert!(!rejected.status.success());
    assert_eq!(mono.state(), state);
    assert_eq!(refs(), references);
    legacy_config.push_str(&format!(
        "\n[remote \"{physical}\"]\n\tjjosh-readOnly = true\n\tjjosh-readOnly = false\n[remote \
         \"{physical}\"]\n\tjjosh-readOnly = malformed\n",
    ));
    fs::write(&config_path, &legacy_config).unwrap();
    let state = mono.state();
    mono.jj(&["project", "migrate", "--dry-run"]);
    assert_eq!(mono.state(), state);
    assert_eq!(fs::read_to_string(&config_path).unwrap(), legacy_config);
    assert_eq!(refs(), references);

    mono.jj(&["project", "migrate", "--apply"]);
    let migrated: serde_json::Value =
        serde_json::from_str(&mono.jj(&["project", "show", "app", "--json"])).unwrap();
    assert_eq!(migrated["projects"], definitions["projects"]);
    assert_eq!(git(&["config", "--local", "--null", "--list"]), settings);
    assert_eq!(refs(), references);
    assert_eq!(mono.jj(&["tag", "list", "--all-remotes"]), tags);
    let after = mono.state();
    assert_eq!(
        (&after.1, &after.2, &after.3),
        (&state.1, &state.2, &state.3)
    );
    mono.jj(&["project", "check", "app"]);

    // Publishing again must use the same source identity, tracking and lease,
    // without reattachment, re-fetching or clearing observations.
    mono.jj(&["new", "-m", "continued publication"]);
    mono.write("vendor/app/value.txt", "published after migration\n");
    mono.jj(&["describe", "-m", "continued publication"]);
    mono.bookmark("main#app");
    mono.jj(&["git", "push", "--bookmark", "main#app"]);
    assert_eq!(
        native_git(&publication, &["show", "main:value.txt"]),
        "published after migration\n"
    );
    assert_eq!(
        native_git(&upstream, &["show", "main:value.txt"]),
        "source\n"
    );
}

#[test]
fn obsolete_remote_key_retirement_preflights_included_remote_sections() {
    let mono = NativeRepo::new();
    let remote = mono.temp.path().join("remote.git");
    native_git(&mono.path, &["init", "--bare", remote.to_str().unwrap()]);
    mono.jj(&["project", "add", "app", "--path", "app"]);
    mono.add_project_remote("source", &remote, "app");
    let physical = mono.physical_remote("app", "source");
    let included_contents = format!("[remote \"{physical}\"]\n\tjjosh-readOnly = malformed\n");
    let git_dir = mono.path.join(".jj/repo/store/git");
    let config_path = git_dir.join("config");
    let included = mono.temp.path().join("included.gitconfig");
    fs::write(&included, &included_contents).unwrap();
    native_git(
        &git_dir,
        &["config", "include.path", included.to_str().unwrap()],
    );
    let config = fs::read(&config_path).unwrap();
    let state = mono.state();
    for mode in ["--dry-run", "--apply"] {
        let output = mono.unchecked(&["project", "migrate", mode]);
        assert!(!output.status.success());
        assert_eq!(mono.state(), state);
        assert_eq!(fs::read(&config_path).unwrap(), config);
        assert_eq!(fs::read_to_string(&included).unwrap(), included_contents);
    }
    // Correcting ownership is sufficient: preflight must not leave a journal
    // requiring recovery before the user can retry migration.
    native_git(&git_dir, &["config", "--unset", "include.path"]);
    native_git(
        &git_dir,
        &[
            "config",
            &format!("remote.{physical}.jjosh-readOnly"),
            "malformed",
        ],
    );
    mono.jj(&["project", "migrate", "--apply"]);
    mono.jj(&["project", "check", "app"]);
}

#[test]
fn legacy_source_migration_preserves_alias_verbatim_and_retires_malformed_remote_key() {
    let mono = NativeRepo::new();
    mono.write("app/value.txt", "local project\n");
    mono.jj(&["describe", "-m", "local project"]);
    mono.bookmark("main#app");
    let remote = mono.temp.path().join("remote.git");
    native_git(&mono.path, &["init", "--bare", remote.to_str().unwrap()]);
    let git_dir = mono.path.join(".jj/repo/store/git");
    native_git(
        &git_dir,
        &["remote", "add", "app-origin", remote.to_str().unwrap()],
    );
    native_git(
        &git_dir,
        &["config", "remote.app-origin.jjosh-project", "app"],
    );
    native_git(
        &git_dir,
        &["config", "remote.app-origin.jjosh-mount", "app"],
    );
    native_git(
        &git_dir,
        &["config", "remote.app-origin.jjosh-readOnly", "malformed"],
    );
    let connection = "0123456789abcdef0123456789abcdef";
    native_git(
        &git_dir,
        &["config", "remote.app-origin.jjosh-connectionId", connection],
    );
    // Planned obsolete-key retirement must not authorize dropping unrelated
    // configuration or begin a journal before preflight rejects it.
    native_git(
        &git_dir,
        &["config", "remote.app-origin.customSetting", "retain"],
    );
    let config_path = git_dir.join("config");
    let blocked_config = fs::read(&config_path).unwrap();
    let blocked_state = mono.state();
    for mode in ["--dry-run", "--apply"] {
        assert!(
            !mono
                .unchecked(&["project", "migrate", mode])
                .status
                .success()
        );
        assert_eq!(mono.state(), blocked_state);
        assert_eq!(fs::read(&config_path).unwrap(), blocked_config);
    }
    native_git(
        &git_dir,
        &["config", "--unset", "remote.app-origin.customSetting"],
    );
    let state = mono.state();
    let config = fs::read(&config_path).unwrap();
    mono.jj(&["project", "migrate", "--dry-run"]);
    assert_eq!(mono.state(), state);
    assert_eq!(fs::read(&config_path).unwrap(), config);
    mono.jj(&["project", "migrate", "--apply"]);
    let physical = mono.physical_remote("app", "app-origin");
    assert_eq!(physical, format!("jjosh-{connection}"));
    let keys = native_git(&git_dir, &["config", "--local", "--name-only", "--list"]);
    assert!(
        !keys
            .lines()
            .any(|key| key.to_ascii_lowercase().ends_with(".jjosh-readonly"))
    );
    assert_eq!(
        native_git(&git_dir, &["remote", "get-url", &physical]).trim(),
        remote.to_str().unwrap()
    );
    let after = mono.state();
    assert_eq!(
        (&after.1, &after.2, &after.3),
        (&state.1, &state.2, &state.3)
    );
    mono.jj(&["project", "check", "app"]);
    let aliases: serde_json::Value =
        serde_json::from_str(&mono.jj(&["project", "show", "app", "--json"])).unwrap();
    assert_eq!(
        aliases["projects"][0]["remotes"][0]["candidates"][0]["definition"]["name"],
        "app-origin"
    );
    assert!(
        !mono
            .unchecked(&["git", "fetch", "--remote", "app-origin"])
            .status
            .success()
    );
    assert!(
        !mono
            .unchecked(&["git", "fetch", "--project", "app", "--remote", "origin"])
            .status
            .success()
    );
    // Adoption frees the old root slot while preserving the project-local alias verbatim.
    mono.jj(&[
        "git",
        "remote",
        "add",
        "app-origin",
        remote.to_str().unwrap(),
    ]);
    assert_eq!(mono.physical_remote("app", "app-origin"), physical);
    mono.jj(&["git", "push", "--bookmark", "main#app"]);
    assert_eq!(
        native_git(&remote, &["show", "main:value.txt"]),
        "local project\n"
    );
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
    dest.jj(&["project", "add", "jj", "--path", "jj"]);
    dest.jj(&[
        "git",
        "remote",
        "add",
        "jj-upstream",
        git_dir.to_str().unwrap(),
        "--filter",
        ":/",
        "--project",
        "jj",
        "--base",
        "main",
    ]);
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "jj-upstream#jj",
        "--branch",
        "main",
    ]);
    dest.jj(&["new", "@", "main#jj@jj-upstream"]);
    let linked_main = dest.log("main#jj@jj-upstream", "commit_id");
    assert_eq!(
        dest.change_id("main#jj@jj-upstream"),
        source.change_id("main")
    );

    dest.add_project_remote("jj-local", &source.path, "jj");
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "jj-local#jj",
        "--branch",
        "topic",
    ]);
    let fetched = "topic#jj@jj-local";
    assert_eq!(dest.change_id(fetched), source.change_id("topic"));
    assert_eq!(dest.log(&format!("{fetched}-"), "commit_id"), linked_main);
    assert_eq!(
        dest.jj(&["file", "show", "-r", fetched, "jj/value.txt"]),
        "topic\n"
    );
    assert!(dest.log("divergent()", "commit_id").is_empty());

    let first = dest.log(fetched, "commit_id");
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "jj-local#jj",
        "--branch",
        "topic",
    ]);
    assert_eq!(dest.log(fetched, "commit_id"), first);

    source.write("value.txt", "amended\n");
    source.jj(&["describe", "-m", "amended topic"]);
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "jj-local#jj",
        "--branch",
        "topic",
    ]);
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
    dest.jj(&["project", "add", "jj", "--path", "jj"]);
    dest.jj(&[
        "git",
        "remote",
        "add",
        "jj-upstream",
        git_dir.to_str().unwrap(),
        "--filter",
        ":/",
        "--project",
        "jj",
        "--base",
        "main",
    ]);
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "jj-upstream#jj",
        "--branch",
        "main",
    ]);
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
    dest.jj(&[
        "git",
        "fetch",
        "--remote",
        "jj-local#jj",
        "--branch",
        "topic",
    ]);
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
        "project",
        "import",
        "--nested",
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
        "alpha-local#alpha",
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
        "--project",
        "alpha",
        "--whole",
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
        "project",
        "import",
        "--nested",
        &format!("alpha={}", source.path.display()),
        "--mount",
        "alpha=vendor/alpha",
    ]);
    assert!(!occupied.status.success());

    let overlap = NativeRepo::new();
    let overlapped = overlap.unchecked(&[
        "project",
        "import",
        "--nested",
        &format!("alpha={}", source.path.display()),
        "--nested",
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
        "project",
        "import",
        "--nested",
        &format!("foo.bar={}", dotted.path.display()),
        "--nested",
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
        "project",
        "import",
        "--nested",
        &format!("a#b={}", dotted.path.display()),
    ]);
    assert!(!hash.status.success());
}

#[test]
fn preserve_projects_import_retains_scopes_and_active_remote_identity() {
    let seed = NativeRepo::new();
    seed.write("value.txt", "upstream\n");
    seed.jj(&["describe", "-m", "upstream"]);
    seed.bookmark("main");
    let endpoint = seed.temp.path().join("upstream.git");
    native_git(&seed.path, &["init", "--bare", endpoint.to_str().unwrap()]);
    seed.jj(&["git", "remote", "add", "origin", endpoint.to_str().unwrap()]);
    seed.jj(&["git", "push", "--remote", "origin", "--bookmark", "main"]);
    let push_endpoint = seed.temp.path().join("publication.git");
    native_git(
        &seed.path,
        &[
            "clone",
            "--bare",
            endpoint.to_str().unwrap(),
            push_endpoint.to_str().unwrap(),
        ],
    );

    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "libs/alpha"]);
    source.jj(&["project", "add", "beta", "--path", "tools/beta"]);
    source.jj(&["project", "rename", "beta", "tooling"]);
    source.write("root.txt", "source root\n");
    source.write("tools/beta/tool.txt", "second project\n");
    source.jj(&["describe", "-m", "source root and tooling"]);
    source.add_project_remote("origin", &endpoint, "alpha");
    let physical = source.physical_remote("alpha", "origin");
    native_git(
        &source.path.join(".jj/repo/store/git"),
        &[
            "config",
            &format!("remote.{physical}.pushurl"),
            push_endpoint.to_str().unwrap(),
        ],
    );
    source.jj(&[
        "git",
        "fetch",
        "--remote",
        "origin#alpha",
        "--branch",
        "main",
    ]);
    source.jj(&["bookmark", "track", "main#alpha@origin"]);
    source.jj(&["tag", "set", "v1#alpha", "-r", "main#alpha"]);
    source.jj(&["new", "@", "main#alpha", "-m", "source composition"]);
    source.bookmark("main#beta");
    source.bookmark("root-main");
    source.jj(&["tag", "set", "root-release"]);
    let source_change = source.change_id("@");
    let source_alpha = source.project("alpha");
    let source_tooling = source.project("tooling");
    let source_main = source.change_id("main#alpha");
    // Preserve the recorded source operation without snapshotting later edits.
    source.write("not-recorded.txt", "leave in source\n");
    let source_before = source.state();

    let target = NativeRepo::new();
    target.write("keep.txt", "destination root\n");
    target.jj(&["describe", "-m", "destination"]);
    let checkout = target.log("@", "commit_id");
    target.jj(&[
        "project",
        "import",
        "--preserve",
        &format!("bundle={}", source.path.display()),
        "--mount",
        "bundle=vendor/source",
    ]);

    assert_eq!(source.state(), source_before);
    assert_eq!(target.log("@", "commit_id"), checkout);
    assert!(!target.path.join("vendor").exists());
    assert_eq!(
        target.jj(&["workspace", "list", "-T", "name ++ \"\\n\""]),
        "default\n"
    );
    let projects: serde_json::Value =
        serde_json::from_str(&target.jj(&["project", "list", "--json"])).unwrap();
    assert_eq!(projects["projects"].as_array().unwrap().len(), 2);
    for (name, original, path) in [
        ("alpha", &source_alpha, "vendor/source/libs/alpha"),
        ("tooling", &source_tooling, "vendor/source/tools/beta"),
    ] {
        let imported = target.project(name);
        assert_eq!(imported["id"], original["id"]);
        assert_eq!(imported["labels"], original["labels"]);
        assert_eq!(imported["remotes"], original["remotes"]);
        assert_eq!(imported["candidates"][0]["definition"]["name"], name);
        assert_eq!(imported["candidates"][0]["definition"]["path"], path);
        let bindings = |project: &serde_json::Value| {
            project["bindings"]
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
        assert_eq!(bindings(&imported), bindings(original));
    }
    let mut bookmarks: Vec<_> = target
        .jj(&["bookmark", "list", "-T", r#"if(!remote, name ++ "\n")"#])
        .lines()
        .map(str::to_owned)
        .collect();
    bookmarks.sort();
    assert_eq!(bookmarks, ["bundle/root-main", "main#alpha", "main#beta"]);
    assert_eq!(target.change_id("bundle/root-main"), source_change);
    assert_eq!(target.change_id("main#beta"), source_change);
    assert_eq!(target.change_id("bundle/root-release"), source_change);
    assert_eq!(target.change_id("v1#alpha"), source_main);
    assert_eq!(target.change_id("main#alpha@origin"), source_main);
    assert_eq!(
        target.log("tracked_remote_bookmarks(main#alpha, origin)", "commit_id"),
        target.log("main#alpha", "commit_id")
    );

    let imported_main = target.log("main#alpha@origin", "commit_id");
    assert_eq!(target.project("alpha")["remotes"], source_alpha["remotes"]);
    // The previously observed raw commit must resolve to the imported canonical
    // commit, not a second canonicalization with the same tree or change ID.
    target.jj(&[
        "git",
        "fetch",
        "--remote",
        "origin#alpha",
        "--branch",
        "main",
    ]);
    assert_eq!(target.log("main#alpha@origin", "commit_id"), imported_main);

    target.jj(&["new", "@", &source_change, "-m", "destination composition"]);
    for (path, contents) in [
        ("keep.txt", "destination root\n"),
        ("vendor/source/root.txt", "source root\n"),
        ("vendor/source/libs/alpha/value.txt", "upstream\n"),
        ("vendor/source/tools/beta/tool.txt", "second project\n"),
    ] {
        assert_eq!(
            fs::read_to_string(target.path.join(path)).unwrap(),
            contents
        );
    }
    assert!(!target.path.join("vendor/source/not-recorded.txt").exists());
    target.write(
        "vendor/source/libs/alpha/value.txt",
        "published from preserved project\n",
    );
    target.write(
        "vendor/source/tools/beta/tool.txt",
        "do not publish sibling\n",
    );
    target.jj(&["describe", "-m", "preserved project contribution"]);
    target.bookmark("main#alpha");
    let published = target.log("main#alpha", "commit_id");
    target.jj(&[
        "git",
        "push",
        "--remote",
        "origin#alpha",
        "--bookmark",
        "main#alpha",
    ]);
    assert_eq!(
        native_git(&push_endpoint, &["show", "main:value.txt"]),
        "published from preserved project\n"
    );
    assert_eq!(
        native_git(&push_endpoint, &["ls-tree", "-r", "--name-only", "main"]),
        "value.txt\n"
    );
    assert_eq!(
        native_git(&endpoint, &["show", "main:value.txt"]),
        "upstream\n"
    );
    // Model upstream accepting publication before fetching through the
    // independently preserved read endpoint.
    native_git(
        &push_endpoint,
        &["push", endpoint.to_str().unwrap(), "main"],
    );
    target.jj(&[
        "git",
        "fetch",
        "--remote",
        "origin#alpha",
        "--branch",
        "main",
    ]);
    assert_eq!(target.log("main#alpha@origin", "commit_id"), published);
    assert_eq!(target.project("alpha")["remotes"], source_alpha["remotes"]);
    assert_eq!(
        target.project("alpha")["bindings"][0]["id"],
        source_alpha["bindings"][0]["id"]
    );
    assert_eq!(source.state(), source_before);
}

#[test]
fn preserve_projects_retains_unindexed_legacy_lease_for_non_fast_forward_publication() {
    let seed = NativeRepo::new();
    seed.write("value.txt", "upstream\n");
    seed.jj(&["describe", "-m", "upstream main"]);
    seed.bookmark("main");
    let endpoint = seed.temp.path().join("upstream.git");
    native_git(&seed.path, &["init", "--bare", endpoint.to_str().unwrap()]);
    seed.jj(&["git", "remote", "add", "origin", endpoint.to_str().unwrap()]);
    seed.jj(&["git", "push", "--remote", "origin", "--bookmark", "main"]);
    let main_raw = native_git(&endpoint, &["rev-parse", "main"]);
    seed.jj(&["new", "-m", "retained branch"]);
    seed.write("retained-only.txt", "old branch contribution\n");
    seed.bookmark("retained");
    seed.jj(&[
        "git",
        "push",
        "--remote",
        "origin",
        "--bookmark",
        "retained",
    ]);

    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "libs/alpha"]);
    source.jj(&["project", "add", "beta", "--path", "tools/beta"]);
    source.write("root.txt", "source root\n");
    source.write("tools/beta/tool.txt", "sibling project\n");
    source.jj(&["describe", "-m", "source root and sibling"]);
    source.add_project_remote("origin", &endpoint, "alpha");
    source.jj(&[
        "git",
        "fetch",
        "--remote",
        "origin#alpha",
        "--branch",
        "main",
    ]);
    source.jj(&["bookmark", "track", "main#alpha@origin"]);
    source.jj(&["new", "@", "main#alpha", "-m", "source composition"]);
    source.bookmark("root-main");
    let source_change = source.change_id("@");

    // Archived sources can retain this supported legacy lease without a
    // corresponding conversion observation: only main was converted above.
    // The key is a real Git blob containing endpoint NUL destination, and the
    // legacy ref points directly at the unconverted upstream commit.
    let source_git = source.path.join(".jj/repo/store/git");
    let key_file = source.temp.path().join("legacy-publication-key");
    fs::write(
        &key_file,
        format!("file://{}\0refs/heads/retained", endpoint.display()),
    )
    .unwrap();
    let key = native_git(
        &source_git,
        &["hash-object", "-w", key_file.to_str().unwrap()],
    );
    native_git(
        &source_git,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            endpoint.to_str().unwrap(),
            &format!("refs/heads/retained:refs/jjosh/link-push/{}", key.trim()),
        ],
    );

    let target = NativeRepo::new();
    target.write("keep.txt", "destination root\n");
    target.jj(&["describe", "-m", "destination"]);
    target.jj(&[
        "project",
        "import",
        "--preserve",
        &format!("bundle={}", source.path.display()),
        "--mount",
        "bundle=vendor/source",
    ]);
    target.jj(&["new", "@", &source_change, "-m", "destination composition"]);
    target.write(
        "vendor/source/libs/alpha/value.txt",
        "published using retained lease\n",
    );
    target.write(
        "vendor/source/tools/beta/tool.txt",
        "do not publish sibling\n",
    );
    target.jj(&["describe", "-m", "project contribution"]);
    target.bookmark("retained#alpha");

    // This contribution descends from main, not the retained-only commit.
    // Naming the bookmark explicitly permits a new tracking relationship, but
    // an unknown wire lease still rejects the non-fast-forward update.
    // Do not fetch or reconnect after import: publication must use the lease.
    target.jj(&[
        "git",
        "push",
        "--remote",
        "origin#alpha",
        "--bookmark",
        "retained#alpha",
    ]);
    assert_eq!(
        native_git(&endpoint, &["show", "retained:value.txt"]),
        "published using retained lease\n"
    );
    assert_eq!(
        native_git(&endpoint, &["ls-tree", "-r", "--name-only", "retained"]),
        "value.txt\n"
    );
    assert_eq!(native_git(&endpoint, &["rev-parse", "main"]), main_raw);
}

#[test]
fn preserve_projects_rejects_configured_unbound_root_endpoint_without_publication() {
    let seed = NativeRepo::new();
    seed.write("value.txt", "read endpoint\n");
    seed.jj(&["describe", "-m", "read endpoint"]);
    seed.bookmark("main");
    seed.jj(&["git", "export"]);
    let endpoint = seed.path.join(".jj/repo/store/git");

    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "lib"]);
    source.write("lib/local.txt", "source project\n");
    source.jj(&["describe", "-m", "source project"]);
    let source_git = source.path.join(".jj/repo/store/git");
    // Configuration alone activates this endpoint, even without fetched
    // observations. It has no project binding to translate mounted history.
    native_git(
        &source_git,
        &["remote", "add", "origin", endpoint.to_str().unwrap()],
    );
    native_git(
        &source_git,
        &["config", "remote.origin.tagOpt", "--no-tags"],
    );
    native_git(
        &source_git,
        &[
            "config",
            "remote.origin.pushurl",
            seed.temp.path().join("publication.git").to_str().unwrap(),
        ],
    );
    let source_before = source.state();
    let source_config = fs::read(source_git.join("config")).unwrap();
    let source_refs = native_git(
        &source_git,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );

    let target = NativeRepo::new();
    target.write("keep.txt", "destination\n");
    target.jj(&["describe", "-m", "destination"]);
    target.bookmark("existing");
    target.jj(&[
        "git",
        "remote",
        "add",
        "existing",
        endpoint.to_str().unwrap(),
    ]);
    target.jj(&["git", "fetch", "--remote", "existing", "--branch", "main"]);
    target.assert_rejected_without_changes(&[
        "project",
        "import",
        "--preserve",
        &format!("archive={}", source.path.display()),
    ]);
    assert_eq!(source.state(), source_before);
    assert_eq!(fs::read(source_git.join("config")).unwrap(), source_config);
    assert_eq!(
        native_git(
            &source_git,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        source_refs
    );
}

#[test]
fn preserve_projects_rejects_duplicate_project_identity_without_publication() {
    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "lib"]);
    source.write("lib/value.txt", "source\n");
    source.jj(&["describe", "-m", "source"]);
    source.bookmark("main#alpha");
    let target = NativeRepo::new();
    target.jj(&[
        "project",
        "import",
        "--preserve",
        &format!("first={}", source.path.display()),
    ]);
    assert_eq!(
        target.project("alpha")["candidates"][0]["definition"]["path"],
        "first/lib"
    );
    assert_eq!(target.project("alpha")["id"], source.project("alpha")["id"]);
    target.assert_rejected_without_changes(&[
        "project",
        "import",
        "--preserve",
        &format!("second={}", source.path.display()),
    ]);
}

#[test]
fn mixed_import_preflights_names_labels_roots_and_reference_collisions() {
    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "lib"]);
    source.jj(&["project", "rename", "alpha", "service"]);
    source.write("lib/value.txt", "source\n");
    source.jj(&["describe", "-m", "source"]);
    source.bookmark("main#alpha");
    source.bookmark("root-main");
    let source_before = source.state();
    let valid = NativeRepo::new();
    valid.jj(&["project", "add", "independent", "--path", "pkg"]);
    valid.write("pkg/other.txt", "must not be partially imported\n");
    valid.jj(&["describe", "-m", "independent source"]);
    valid.bookmark("main#independent");
    let valid_before = valid.state();

    for collision in ["name", "label", "root", "reference"] {
        let target = NativeRepo::new();
        match collision {
            "name" => {
                target.jj(&["project", "add", "service", "--path", "existing"]);
            }
            "label" => {
                target.jj(&["project", "add", "alpha", "--path", "existing"]);
                target.jj(&["project", "rename", "alpha", "existing"]);
            }
            "root" => {
                target.jj(&["project", "add", "existing", "--path", "bundle/lib"]);
            }
            "reference" => target.bookmark("bundle/root-main"),
            _ => unreachable!(),
        }
        target.assert_rejected_without_changes(&[
            "project",
            "import",
            "--nested",
            &format!("accepted={}", valid.path.display()),
            "--preserve",
            &format!("bundle={}", source.path.display()),
        ]);
    }
    // Reservations from another input must reject collisions just like
    // projects already present in the target, regardless of option order.
    for (name, mount) in [
        ("service", "accepted"),
        ("alpha", "accepted"),
        ("accepted", "bundle/lib"),
    ] {
        let target = NativeRepo::new();
        target.assert_rejected_without_changes(&[
            "project",
            "import",
            "--preserve",
            &format!("bundle={}", source.path.display()),
            "--nested",
            &format!("{name}={}", valid.path.display()),
            "--mount",
            &format!("{name}={mount}"),
        ]);
    }
    assert_eq!(source.state(), source_before);
    assert_eq!(valid.state(), valid_before);
}

#[test]
fn preserve_projects_rejects_repository_view_bindings_before_publication() {
    let source = NativeRepo::new();
    source.jj(&["project", "add", "alpha", "--path", "lib"]);
    source.write("lib/value.txt", "source\n");
    source.jj(&["describe", "-m", "source with repository view"]);
    source.bookmark("main#alpha");
    let endpoint = source.temp.path().join("view.git");
    native_git(
        &source.path,
        &["init", "--bare", endpoint.to_str().unwrap()],
    );
    source.jj(&[
        "git",
        "remote",
        "add",
        "view",
        endpoint.to_str().unwrap(),
        "--filter",
        ":/lib",
    ]);
    let source_before = source.state();
    let target = NativeRepo::new();
    target.write("keep.txt", "destination\n");
    target.jj(&["describe", "-m", "destination"]);
    target.bookmark("keep");
    target.jj(&[
        "git",
        "remote",
        "add",
        "existing",
        endpoint.to_str().unwrap(),
    ]);
    target.jj(&["config", "set", "--repo", "git.fetch", "existing"]);
    target.jj(&["git", "export"]);
    target.assert_rejected_without_changes(&[
        "project",
        "import",
        "--preserve",
        &format!("bundle={}", source.path.display()),
    ]);
    assert_eq!(source.state(), source_before);
}

#[test]
fn preserve_projects_filtered_publication_survives_source_repository_relocation() {
    let seed = NativeRepo::new();
    seed.write("subdir/value.txt", "upstream project\n");
    seed.write("outside.txt", "upstream-only file\n");
    seed.jj(&["describe", "-m", "filtered upstream"]);
    seed.bookmark("main");
    let endpoint = seed.temp.path().join("upstream.git");
    native_git(&seed.path, &["init", "--bare", endpoint.to_str().unwrap()]);
    seed.jj(&["git", "remote", "add", "origin", endpoint.to_str().unwrap()]);
    seed.jj(&["git", "push", "--remote", "origin", "--bookmark", "main"]);
    let upstream_parent = native_git(&endpoint, &["rev-parse", "main"]);

    let source = NativeRepo::new();
    source.jj(&["project", "add", "api", "--path", "packages/api"]);
    source.jj(&[
        "git",
        "remote",
        "add",
        "origin",
        endpoint.to_str().unwrap(),
        "--project",
        "api",
        "--filter",
        ":/subdir",
        "--base",
        "main",
    ]);
    source.jj(&["git", "fetch", "--remote", "origin#api", "--branch", "main"]);
    source.jj(&["bookmark", "track", "main#api@origin"]);
    let source_change = source.change_id("main#api");
    let source_operation = source.operation_id();

    let target = NativeRepo::new();
    target.jj(&[
        "project",
        "import",
        "--preserve",
        &format!("archive={}", source.path.display()),
    ]);
    assert_eq!(source.operation_id(), source_operation);
    assert_eq!(target.change_id("main#api@origin"), source_change);
    fs::rename(&source.path, source.temp.path().join("relocated-source")).unwrap();
    assert!(!source.path.exists());
    // Push before fetching again: publication must use the imported observation
    // and raw generation, not rebuild them from the endpoint or old source path.
    target.jj(&[
        "new",
        "main#api",
        "-m",
        "continue imported filtered project",
    ]);
    assert_eq!(
        fs::read_to_string(target.path.join("archive/packages/api/value.txt")).unwrap(),
        "upstream project\n"
    );
    target.write("archive/packages/api/value.txt", "filtered contribution\n");
    target.write("local-only.txt", "never publish destination root\n");
    target.jj(&["describe", "-m", "filtered contribution"]);
    target.bookmark("main#api");
    let published = target.log("main#api", "commit_id");
    let published_change = target.change_id("main#api");
    target.jj(&[
        "git",
        "push",
        "--remote",
        "origin#api",
        "--bookmark",
        "main#api",
    ]);
    assert_eq!(
        native_git(&endpoint, &["rev-parse", "main^"]),
        upstream_parent
    );
    assert_eq!(
        native_git(&endpoint, &["show", "main:subdir/value.txt"]),
        "filtered contribution\n"
    );
    assert_eq!(
        native_git(&endpoint, &["show", "main:outside.txt"]),
        "upstream-only file\n"
    );
    assert_eq!(
        native_git(&endpoint, &["ls-tree", "-r", "--name-only", "main"]),
        "outside.txt\nsubdir/value.txt\n"
    );
    target.jj(&["git", "fetch", "--remote", "origin#api", "--branch", "main"]);
    assert_eq!(target.log("main#api@origin", "commit_id"), published);
    assert_eq!(target.change_id("main#api@origin"), published_change);
    assert_eq!(
        fs::read_to_string(target.path.join("local-only.txt")).unwrap(),
        "never publish destination root\n"
    );
}
