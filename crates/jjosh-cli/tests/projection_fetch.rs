#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(cwd: &Path, program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", cwd)
        .env("XDG_CONFIG_HOME", cwd)
        .env("JJ_CONFIG", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00+00:00")
        .env("LANG", "C.UTF-8")
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

fn jjosh(cwd: &Path, args: &[&str]) -> Output {
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
    let output = run(cwd, program, &full_args);
    assert_success(&output, program, &full_args);
    output
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

#[test]
fn fetch_projects_refs_and_imports_only_visible_changes() {
    let temp = tempfile::tempdir().unwrap();
    let upstream_work = temp.path().join("upstream-work");
    let upstream_bare = temp.path().join("upstream.git");
    let client = temp.path().join("client");
    fs::create_dir(&upstream_work).unwrap();
    fs::create_dir(&client).unwrap();

    git(&upstream_work, &["init", "-b", "main"]);
    git(&upstream_work, &["config", "user.name", "Smoke Test"]);
    git(
        &upstream_work,
        &["config", "user.email", "smoke@example.com"],
    );
    fs::create_dir(upstream_work.join("app")).unwrap();
    fs::create_dir(upstream_work.join("other")).unwrap();
    fs::write(upstream_work.join("app/file.txt"), "app-v1\n").unwrap();
    fs::write(upstream_work.join("other/secret.txt"), "secret-v1\n").unwrap();
    git(&upstream_work, &["add", "."]);
    git(&upstream_work, &["commit", "-m", "upstream-base"]);
    let upstream_base = git(&upstream_work, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            upstream_work.to_str().unwrap(),
            upstream_bare.to_str().unwrap(),
        ],
    );

    git(&client, &["init", "-b", "local"]);
    git(&client, &["config", "user.name", "Smoke Test"]);
    git(&client, &["config", "user.email", "smoke@example.com"]);
    fs::write(client.join("local.txt"), "local\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "local-seed"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    git(&client, &["switch", "--detach"]);
    git(
        &client,
        &["remote", "add", "origin", upstream_bare.to_str().unwrap()],
    );
    git(
        &client,
        &[
            "config",
            "remote.origin.pushurl",
            upstream_bare.to_str().unwrap(),
        ],
    );

    jjosh(
        &client,
        &[
            "projection",
            "remote",
            "add",
            "origin",
            upstream_bare.to_str().unwrap(),
            ":/app",
        ],
    );

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    let initial_projected = git(&client, &["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    assert_ne!(initial_projected, upstream_base);
    assert_eq!(
        git(
            &client,
            &["ls-tree", "-r", "--name-only", &initial_projected]
        ),
        "file.txt\n"
    );
    assert_eq!(
        String::from_utf8(jjosh(&client, &["file", "list", "-r", "main@origin"]).stdout).unwrap(),
        "file.txt\n"
    );
    let initial_operation = operation_id(&client);

    fs::write(upstream_work.join("other/secret.txt"), "secret-v2\n").unwrap();
    git(&upstream_work, &["commit", "-am", "outside-only"]);
    let outside_upstream = git(&upstream_work, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    git(
        &upstream_work,
        &["tag", "-a", "outside-tag", "-m", "outside-only tag"],
    );
    git(
        &upstream_work,
        &[
            "push",
            upstream_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
            "refs/tags/outside-tag",
        ],
    );

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    assert_eq!(
        git(&upstream_bare, &["rev-parse", "main"]).trim(),
        outside_upstream
    );
    assert_eq!(
        git(&client, &["rev-parse", "refs/remotes/origin/main"]).trim(),
        initial_projected
    );
    assert_eq!(operation_id(&client), initial_operation);
    assert_eq!(git(&client, &["for-each-ref", "refs/tags/outside-tag"]), "");
    assert_eq!(
        String::from_utf8(jjosh(&client, &["tag", "list", "--all-remotes", "outside-tag"]).stdout,)
            .unwrap(),
        ""
    );

    fs::write(upstream_work.join("app/file.txt"), "app-v2\n").unwrap();
    git(&upstream_work, &["commit", "-am", "inside-change"]);
    git(
        &upstream_work,
        &[
            "push",
            upstream_bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    let final_projected = git(&client, &["rev-parse", "refs/remotes/origin/main"])
        .trim()
        .to_owned();
    assert_ne!(final_projected, initial_projected);
    assert_ne!(operation_id(&client), initial_operation);
    assert_eq!(
        String::from_utf8(
            jjosh(&client, &["file", "show", "-r", "main@origin", "file.txt"],).stdout,
        )
        .unwrap(),
        "app-v2\n"
    );

    let direct_push = run(
        &client,
        Path::new("git"),
        &["push", "origin", "HEAD:refs/heads/direct-push-must-fail"],
    );
    assert!(!direct_push.status.success());
    assert_eq!(
        git(
            &upstream_bare,
            &["for-each-ref", "refs/heads/direct-push-must-fail"]
        ),
        ""
    );
    let visible = String::from_utf8(
        jjosh(
            &client,
            &["log", "--no-graph", "-r", "all()", "-T", "description"],
        )
        .stdout,
    )
    .unwrap();
    assert!(visible.contains("local-seed"));
    assert!(visible.contains("inside-change"));
    assert!(!visible.contains("outside-only"));
}

fn projection_fetch_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let client = temp.path().join("client");
    for path in [&source, &client] {
        fs::create_dir(path).unwrap();
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.name", "Smoke Test"]);
        git(path, &["config", "user.email", "smoke@example.com"]);
    }
    fs::create_dir(source.join("app")).unwrap();
    fs::write(source.join("app/file.txt"), "projected\n").unwrap();
    fs::write(source.join("outside.txt"), "outside\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "source-base"]);
    fs::write(client.join("local.txt"), "local\n").unwrap();
    git(&client, &["add", "."]);
    git(&client, &["commit", "-m", "local-seed"]);
    jjosh(&client, &["git", "init", "--colocate"]);
    jjosh(
        &client,
        &[
            "projection",
            "remote",
            "add",
            "origin",
            source.to_str().unwrap(),
            ":/app",
        ],
    );
    (temp, source, client)
}

#[test]
fn fetch_prunes_only_selected_projection_branches_even_with_tag_pruning() {
    let (_temp, source, client) = projection_fetch_repo();
    git(&source, &["branch", "stale"]);
    git(&source, &["branch", "empty"]);
    let local = git(&client, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&client, &["tag", "local-keep", &local]);
    git(
        &client,
        &["remote", "add", "other", source.to_str().unwrap()],
    );
    let preserved = [
        "refs/tags/local-keep",
        "refs/heads/local-keep",
        "refs/remotes/other/keep",
    ];
    for name in preserved {
        git(&client, &["update-ref", name, &local]);
    }
    git(&client, &["config", "fetch.pruneTags", "true"]);
    git(&client, &["config", "remote.origin.pruneTags", "true"]);
    git(&client, &["config", "remote.origin.tagOpt", "--tags"]);
    // Additional user fetch mappings must not expand this selected fetch's
    // ownership, whether by importing tags or pruning another remote.
    for refspec in [
        "+refs/tags/*:refs/tags/*",
        "+refs/heads/*:refs/remotes/other/*",
    ] {
        git(
            &client,
            &["config", "--add", "remote.origin.fetch", refspec],
        );
    }
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    for branch in ["stale", "empty"] {
        assert_eq!(
            git(
                &client,
                &["rev-parse", &format!("refs/remotes/origin/{branch}")]
            ),
            git(&client, &["rev-parse", "refs/remotes/origin/main"])
        );
    }

    git(&source, &["branch", "-D", "stale", "empty"]);
    git(&source, &["checkout", "--orphan", "empty"]);
    git(&source, &["rm", "-rf", "."]);
    fs::write(source.join("outside.txt"), "only outside\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "empty-projection"]);
    git(&source, &["checkout", "main"]);
    // Exercise packed as well as loose canonical ref deletion.
    git(&client, &["pack-refs", "--all", "--prune"]);

    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    for branch in ["stale", "empty"] {
        assert_eq!(
            git(
                &client,
                &["for-each-ref", &format!("refs/remotes/origin/{branch}")]
            ),
            ""
        );
        assert_eq!(
            String::from_utf8(
                jjosh(
                    &client,
                    &[
                        "bookmark",
                        "list",
                        "--remote",
                        "origin",
                        &format!("exact:{branch}"),
                    ],
                )
                .stdout,
            )
            .unwrap(),
            ""
        );
    }
    // With every remaining source branch outside the view, no stale remote
    // bookmarks may remain visible.
    git(&source, &["reset", "--hard", "empty"]);
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    assert_eq!(git(&client, &["for-each-ref", "refs/remotes/origin"]), "");
    assert_eq!(
        String::from_utf8(jjosh(&client, &["bookmark", "list", "--remote", "origin"]).stdout,)
            .unwrap(),
        ""
    );
    for name in preserved {
        assert_eq!(git(&client, &["rev-parse", name]).trim(), local);
    }
    assert_eq!(
        String::from_utf8(jjosh(&client, &["tag", "list", "local-keep", "-T", "name"]).stdout,)
            .unwrap(),
        "local-keep"
    );
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &["log", "--no-graph", "-r", "keep@other", "-T", "commit_id"],
            )
            .stdout,
        )
        .unwrap(),
        local
    );
}

#[test]
fn fetch_synchronizes_external_git_checkout_and_unrecorded_files() {
    let (_temp, _source, client) = projection_fetch_repo();
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    jjosh(&client, &["new", "main@origin"]);
    git(
        &client,
        &["checkout", "-b", "external", "refs/remotes/origin/main"],
    );
    fs::write(client.join("external.txt"), "external commit\n").unwrap();
    git(&client, &["add", "external.txt"]);
    git(&client, &["commit", "-m", "external-git-commit"]);
    let external = git(&client, &["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(client.join("file.txt"), "unrecorded tracked edit\n").unwrap();
    fs::write(client.join("untracked.txt"), "unrecorded new file\n").unwrap();

    // No native command may synchronize the external checkout before fetch.
    jjosh(
        &client,
        &["git", "fetch", "--remote", "origin", "--branch", "*"],
    );
    assert_eq!(
        fs::read_to_string(client.join("external.txt")).unwrap(),
        "external commit\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("file.txt")).unwrap(),
        "unrecorded tracked edit\n"
    );
    assert_eq!(
        fs::read_to_string(client.join("untracked.txt")).unwrap(),
        "unrecorded new file\n"
    );
    assert_eq!(
        String::from_utf8(
            jjosh(
                &client,
                &["log", "--no-graph", "-r", "@-", "-T", "commit_id"]
            )
            .stdout,
        )
        .unwrap(),
        external
    );
    for (path, expected) in [
        ("external.txt", "external commit\n"),
        ("file.txt", "unrecorded tracked edit\n"),
        ("untracked.txt", "unrecorded new file\n"),
    ] {
        assert_eq!(
            String::from_utf8(jjosh(&client, &["file", "show", "-r", "@", path]).stdout).unwrap(),
            expected
        );
    }
}

struct TransplantRepo {
    temp: tempfile::TempDir,
    path: PathBuf,
}

impl TransplantRepo {
    fn new() -> Self {
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
        repo.jj(&["git", "init", "--colocate"]);
        repo
    }

    fn run(&self, program: &Path, args: &[&str]) -> Output {
        Command::new(program)
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
            .unwrap_or_else(|err| panic!("failed to run {program:?} {args:?}: {err}"))
    }

    fn jj_unchecked(&self, args: &[&str]) -> Output {
        let mut full_args = vec![
            "--no-pager",
            "--color=never",
            "--config",
            "user.name=Transplant Test",
            "--config",
            "user.email=transplant@example.com",
        ];
        full_args.extend_from_slice(args);
        self.run(Path::new(env!("CARGO_BIN_EXE_jjosh")), &full_args)
    }

    fn jj(&self, args: &[&str]) -> String {
        let output = self.jj_unchecked(args);
        assert_success(&output, Path::new(env!("CARGO_BIN_EXE_jjosh")), args);
        String::from_utf8(output.stdout).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.run(Path::new("git"), args);
        assert_success(&output, Path::new("git"), args);
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

    fn commit_id(&self, revision: &str) -> String {
        self.log(revision, "commit_id")
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn bookmark(&self, name: &str) {
        self.jj(&["bookmark", "set", name]);
    }

    fn bases(&self) {
        self.write("a/file.txt", "a-base\n");
        self.write("b/file.txt", "b-base\n");
        self.write("README", "base readme\n");
        self.jj(&["describe", "-m", "source base"]);
        self.bookmark("old-base");
        self.jj(&["new", "root()", "-m", "destination base"]);
        self.write("pkg/a/file.txt", "a-base\n");
        self.write("pkg/b/file.txt", "b-base\n");
        self.write("archive/README", "base readme\n");
        self.write("target-only.txt", "keep destination content\n");
        self.bookmark("target-base");
    }

    fn linear_changes(&self) {
        self.bases();
        self.jj(&["new", "old-base", "-m", "cross directory"]);
        self.write("a/file.txt", "a-cross\n");
        self.write("b/file.txt", "b-cross\n");
        self.bookmark("cross");
        self.jj(&["new", "cross", "-m", "descendant"]);
        self.write("a/child.txt", "child\n");
        self.bookmark("child");
    }

    fn state(&self) -> TransplantState {
        fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if path == root && (entry.file_name() == ".git" || entry.file_name() == ".jj") {
                    continue;
                }
                let path = entry.path();
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
        let mut worktree = BTreeMap::new();
        collect(&self.path, &self.path, &mut worktree);
        TransplantState {
            operation: self.jj(&[
                "--ignore-working-copy",
                "op",
                "log",
                "--no-graph",
                "--limit",
                "1",
                "-T",
                "id",
            ]),
            refs: self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            head: self.git(&["rev-parse", "HEAD", "HEAD^{tree}"]),
            workspace: self.commit_id("@"),
            view: self.log(
                "all()",
                "commit_id ++ \" \" ++ change_id ++ \" \" ++ bookmarks ++ \"\\n\"",
            ),
            worktree,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TransplantState {
    operation: String,
    refs: String,
    head: String,
    workspace: String,
    view: String,
    worktree: BTreeMap<PathBuf, Vec<u8>>,
}

#[test]
fn transplant_preserves_the_whole_change_graph_and_replays_merge_delta() {
    let repo = TransplantRepo::new();
    repo.bases();
    repo.jj(&["new", "old-base", "-m", "cross directory"]);
    repo.write("a/file.txt", "a-cross\n");
    repo.write("b/file.txt", "b-cross\n");
    repo.write("README", "cross readme\n");
    repo.bookmark("cross");
    repo.jj(&["new", "cross", "-m", "left branch"]);
    repo.write("a/left.txt", "left\n");
    repo.bookmark("left");
    repo.jj(&["new", "cross", "-m", "right branch"]);
    repo.write("b/right.txt", "right\n");
    repo.bookmark("right");
    repo.jj(&["new", "left", "right", "-m", "merge with its own delta"]);
    repo.write("a/merge-only.txt", "merge delta\n");
    repo.bookmark("merged");
    repo.jj(&["new", "merged", "-m", "intentional empty change"]);
    repo.bookmark("empty");
    repo.jj(&["new", "old-base", "-m", "independent side branch"]);
    repo.write("b/side.txt", "side\n");
    repo.bookmark("side");
    repo.jj(&["edit", "empty"]);

    let names = ["cross", "left", "right", "merged", "empty", "side"];
    let before: Vec<_> = names
        .iter()
        .map(|name| {
            let id = repo.commit_id(name);
            let identity = repo.log(name, "change_id");
            let author_and_description =
                repo.git(&["show", "-s", "--format=%an%n%ae%n%aI%n%B", &id]);
            let files: BTreeMap<_, _> = repo
                .git(&["ls-tree", "-r", "--name-only", &id])
                .lines()
                .map(|path| {
                    let destination = if let Some(suffix) = path.strip_prefix("a/") {
                        format!("pkg/a/{suffix}")
                    } else if let Some(suffix) = path.strip_prefix("b/") {
                        format!("pkg/b/{suffix}")
                    } else {
                        format!("archive/{path}")
                    };
                    (destination, repo.git(&["show", &format!("{id}:{path}")]))
                })
                .collect();
            (id, identity, author_and_description, files)
        })
        .collect();
    let target = repo.commit_id("target-base");
    repo.jj(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
    ]);

    let after: Vec<_> = names.iter().map(|name| repo.commit_id(name)).collect();
    let parents = [
        vec![target.as_str()],
        vec![after[0].as_str()],
        vec![after[0].as_str()],
        vec![after[1].as_str(), after[2].as_str()],
        vec![after[3].as_str()],
        vec![target.as_str()],
    ];
    for (index, name) in names.iter().enumerate() {
        let (old_id, identity, author_and_description, old_files) = &before[index];
        assert_ne!(&after[index], old_id);
        assert_eq!(repo.log(name, "change_id"), *identity);
        assert_eq!(
            repo.git(&["show", "-s", "--format=%an%n%ae%n%aI%n%B", &after[index]]),
            *author_and_description
        );
        assert_eq!(
            repo.git(&["show", "-s", "--format=%P", &after[index]])
                .trim(),
            parents[index].join(" ")
        );
        let mut expected_files = old_files.clone();
        expected_files.insert(
            "target-only.txt".to_owned(),
            "keep destination content\n".to_owned(),
        );
        let actual_paths = repo.jj(&["file", "list", "-r", name]);
        assert_eq!(
            actual_paths.lines().collect::<Vec<_>>(),
            expected_files
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            repo.git(&["ls-tree", "-r", "--name-only", &after[index]]),
            actual_paths
        );
        for (path, contents) in expected_files {
            assert_eq!(repo.jj(&["file", "show", "-r", name, &path]), contents);
            assert_eq!(
                repo.git(&["show", &format!("{}:{path}", after[index])]),
                contents
            );
        }
    }
    assert_eq!(repo.commit_id("@"), after[4]);
    assert_eq!(
        repo.git(&["rev-parse", &format!("{}^{{tree}}", after[3])]),
        repo.git(&["rev-parse", &format!("{}^{{tree}}", after[4])])
    );
    assert_eq!(
        repo.log("target-base:: ~ target-base", "change_id ++ \"\\n\"")
            .lines()
            .count(),
        names.len()
    );
    assert_eq!(
        fs::read_to_string(repo.path.join("pkg/a/merge-only.txt")).unwrap(),
        "merge delta\n"
    );
    assert!(!repo.path.join("a").exists());
}

#[test]
fn transplant_keeps_unresolved_conflicts_in_jj_and_git_and_allows_resolution() {
    let repo = TransplantRepo::new();
    repo.bases();
    repo.jj(&["new", "old-base", "-m", "left conflict"]);
    repo.write("a/file.txt", "left version\n");
    repo.bookmark("left");
    repo.jj(&["new", "old-base", "-m", "right conflict"]);
    repo.write("a/file.txt", "right version\n");
    repo.bookmark("right");
    repo.jj(&["new", "left", "right", "-m", "unresolved merge"]);
    repo.bookmark("conflicted");
    let change = repo.log("@", "change_id");
    assert_eq!(repo.commit_id("@ & conflicts()"), repo.commit_id("@"));

    repo.jj(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
    ]);

    let conflicted = repo.commit_id("conflicted");
    assert_eq!(repo.commit_id("@"), conflicted);
    assert_eq!(repo.log("@", "change_id"), change);
    assert_eq!(repo.commit_id("@ & conflicts()"), conflicted);
    let paths = repo.jj(&["file", "list", "-r", "@"]);
    assert_eq!(
        paths,
        "archive/README\npkg/a/file.txt\npkg/b/file.txt\ntarget-only.txt\n"
    );
    let materialized = repo.jj(&["file", "show", "-r", "@", "pkg/a/file.txt"]);
    assert!(materialized.contains("left version"));
    assert!(materialized.contains("right version"));
    assert_eq!(
        fs::read_to_string(repo.path.join("pkg/a/file.txt")).unwrap(),
        materialized
    );
    assert!(!repo.path.join("a/file.txt").exists());

    // Git exposes the first conflict side at the ordinary path, and all sides
    // through jj:trees. Neither representation may retain the old source paths.
    let header = repo.git(&["cat-file", "commit", &conflicted]);
    let trees = header
        .split("\n\n")
        .next()
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("jj:trees "))
        .expect("an unresolved commit must expose its conflict terms to Git");
    let mut versions = Vec::new();
    for tree in trees.split_whitespace() {
        assert_eq!(repo.git(&["ls-tree", "-r", "--name-only", tree]), paths);
        versions.push(repo.git(&["show", &format!("{tree}:pkg/a/file.txt")]));
    }
    versions.sort();
    versions.dedup();
    assert_eq!(
        versions,
        vec!["a-base\n", "left version\n", "right version\n"]
    );
    assert!(versions.contains(&repo.git(&["show", &format!("{conflicted}:pkg/a/file.txt")])));
    let git_paths = repo.git(&["ls-tree", "-r", "--name-only", &conflicted]);
    assert!(!git_paths.lines().any(|path| path == "a/file.txt"
        || path.ends_with("/a/file.txt")
            && path != "pkg/a/file.txt"
            && !path.ends_with("/pkg/a/file.txt")));

    repo.write("pkg/a/file.txt", "resolved version\n");
    repo.jj(&["status"]);
    assert_eq!(repo.log("@", "change_id"), change);
    assert_eq!(repo.commit_id("@ & conflicts()"), "");
    assert_eq!(
        repo.jj(&["file", "show", "-r", "@", "pkg/a/file.txt"]),
        "resolved version\n"
    );
    let resolved = repo.commit_id("@");
    assert_eq!(
        repo.git(&["show", &format!("{resolved}:pkg/a/file.txt")]),
        "resolved version\n"
    );
    assert_eq!(
        repo.git(&["ls-tree", "-r", "--name-only", &resolved]),
        paths
    );
    repo.jj(&["new", "-m", "continue after resolving"]);
    assert_eq!(repo.commit_id("@-"), resolved);
    assert_eq!(
        repo.jj(&["file", "show", "-r", "@", "pkg/a/file.txt"]),
        "resolved version\n"
    );
}

#[test]
fn transplant_rejects_incomplete_graphs_and_collisions_without_changing_state() {
    let repo = TransplantRepo::new();
    repo.linear_changes();
    let before = repo.state();
    let invalid_args: &[&[&str]] = &[
        &[
            "native",
            "transplant",
            "-r",
            "old-base:: ~ old-base",
            "--map",
            "a=pkg/a",
            "--map",
            "b=pkg/b",
            "--map",
            ".=archive",
        ],
        &[
            "native",
            "transplant",
            "-r",
            "cross",
            "--map",
            "a=pkg/a",
            "--map",
            "b=pkg/b",
            "--map",
            ".=archive",
            "--parent",
            "old-base=target-base",
        ],
        &[
            "native",
            "transplant",
            "-r",
            "old-base:: ~ old-base",
            "--map",
            "a=pkg",
            "--map",
            "b=pkg",
            "--map",
            ".=archive",
            "--parent",
            "old-base=target-base",
        ],
        &[
            "native",
            "transplant",
            "-r",
            "old-base:: ~ old-base",
            "--map",
            "a=pkg/b",
            "--map",
            ".=pkg",
            "--parent",
            "old-base=target-base",
        ],
    ];
    for args in invalid_args {
        let output = repo.jj_unchecked(args);
        assert!(
            !output.status.success(),
            "invalid transplant succeeded: {args:?}"
        );
        assert_eq!(
            repo.state(),
            before,
            "rejected transplant changed state: {args:?}"
        );
    }
}

#[test]
fn transplant_dry_run_preserves_operation_refs_view_and_worktree() {
    let repo = TransplantRepo::new();
    repo.linear_changes();
    let before = repo.state();
    repo.jj(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
        "--dry-run",
    ]);
    assert_eq!(repo.state(), before);
}

#[test]
fn transplant_replaces_each_boundary_parent_without_losing_the_side_parent() {
    let repo = TransplantRepo::new();
    repo.bases();
    repo.jj(&["new", "root()", "-m", "source side parent"]);
    repo.write("outside.txt", "side parent content\n");
    repo.bookmark("old-side");
    repo.jj(&["new", "root()", "-m", "target side parent"]);
    repo.write("archive/outside.txt", "side parent content\n");
    repo.bookmark("target-side");
    repo.jj(&["new", "old-base", "old-side", "-m", "boundary merge"]);
    repo.write("a/local.txt", "merge-local content\n");
    repo.bookmark("boundary");
    let change = repo.log("boundary", "change_id");
    let parents = [repo.commit_id("target-base"), repo.commit_id("target-side")];

    repo.jj(&[
        "native",
        "transplant",
        "-r",
        "boundary",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
        "--parent",
        "old-side=target-side",
    ]);

    let result = repo.commit_id("boundary");
    assert_eq!(repo.log("boundary", "change_id"), change);
    assert_eq!(
        repo.git(&["show", "-s", "--format=%P", &result]).trim(),
        parents.join(" ")
    );
    assert_eq!(
        repo.jj(&["file", "show", "archive/outside.txt"]),
        "side parent content\n"
    );
    assert_eq!(
        repo.jj(&["file", "show", "pkg/a/local.txt"]),
        "merge-local content\n"
    );
    assert_eq!(
        repo.jj(&["file", "show", "target-only.txt"]),
        "keep destination content\n"
    );
}

#[test]
fn transplant_does_not_discard_unsnapshotted_worktree_changes() {
    let repo = TransplantRepo::new();
    repo.linear_changes();
    repo.write("a/file.txt", "pending tracked edit\n");
    repo.write("pending.txt", "pending new file\n");
    let before = repo.state();
    let result = repo.jj_unchecked(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
    ]);
    assert!(!result.status.success());
    assert_eq!(repo.state(), before);
}

#[test]
fn transplant_normalizes_a_historical_directory_rename_without_splitting_changes() {
    let repo = TransplantRepo::new();
    repo.write("old-name/file.txt", "base\n");
    repo.bookmark("old-base");
    repo.jj(&["new", "root()", "-m", "destination base"]);
    repo.write("pkg/new-name/file.txt", "base\n");
    repo.bookmark("target-base");
    repo.jj(&["new", "old-base", "-m", "edit before directory rename"]);
    repo.write("old-name/file.txt", "local edit\n");
    repo.bookmark("edited");
    repo.jj(&["new", "edited", "-m", "rename source directory"]);
    fs::rename(repo.path.join("old-name"), repo.path.join("new-name")).unwrap();
    repo.bookmark("renamed");
    let edit_change = repo.log("edited", "change_id");
    let rename_change = repo.log("renamed", "change_id");

    repo.jj(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "old-name=pkg/new-name",
        "--map",
        ".=pkg",
        "--parent",
        "old-base=target-base",
    ]);

    assert_eq!(repo.log("edited", "change_id"), edit_change);
    assert_eq!(repo.log("renamed", "change_id"), rename_change);
    assert_eq!(repo.commit_id("renamed-"), repo.commit_id("edited"));
    assert_eq!(repo.jj(&["file", "list"]), "pkg/new-name/file.txt\n");
    assert_eq!(
        repo.jj(&["file", "show", "pkg/new-name/file.txt"]),
        "local edit\n"
    );
    assert_eq!(
        repo.git(&[
            "rev-parse",
            &format!("{}^{{tree}}", repo.commit_id("edited"))
        ]),
        repo.git(&[
            "rev-parse",
            &format!("{}^{{tree}}", repo.commit_id("renamed"))
        ])
    );
}

#[test]
fn transplant_rejects_collisions_between_conflict_terms_without_losing_conflicts() {
    let repo = TransplantRepo::new();
    repo.write("b/f", "A\n");
    repo.bookmark("pa");
    repo.jj(&["new", "root()"]);
    repo.write("b/f", "B\n");
    repo.bookmark("pb");
    repo.jj(&["new", "pa", "pb"]);
    repo.bookmark("p");
    for (name, value) in [("left", "A\n"), ("right", "B\n")] {
        repo.jj(&["new", "p"]);
        fs::remove_file(repo.path.join("b/f")).unwrap();
        repo.write("a/f", value);
        repo.bookmark(name);
    }
    repo.jj(&["new", "left", "right"]);
    repo.bookmark("conflict");
    repo.jj(&["new", "root()"]);
    repo.jj(&["restore", "--from", "conflict"]);
    repo.bookmark("specimen");
    assert_eq!(repo.jj(&["file", "list"]), "a/f\nb/f\n");
    assert_eq!(repo.commit_id("@ & conflicts()"), repo.commit_id("@"));
    // Move the checkout away so state inspection has an ordinary Git HEAD.
    repo.jj(&["new", "pa"]);
    let before = repo.state();
    let output = repo.jj_unchecked(&[
        "native",
        "transplant",
        "-r",
        "specimen",
        "--map",
        "a=pkg/b",
        "--map",
        ".=pkg",
        "--parent",
        "root()=root()",
    ]);
    assert!(
        !output.status.success(),
        "cross-term collision silently removed conflicts"
    );
    assert_eq!(repo.state(), before);
}

#[test]
fn transplant_rejects_a_git_checkout_not_yet_imported_into_jj() {
    let repo = TransplantRepo::new();
    repo.linear_changes();
    repo.jj(&["new"]);
    let current = repo.commit_id("@");
    repo.git(&["branch", "git-checkout", &current]);
    repo.jj(&["git", "import"]);
    // Same files, different HEAD: a tree-only check cannot detect this checkout.
    repo.git(&["switch", "git-checkout"]);
    let before = repo.state();
    let output = repo.jj_unchecked(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
    ]);
    assert!(
        !output.status.success(),
        "transplant overwrote the pending Git checkout"
    );
    assert_eq!(repo.state(), before);
    assert_eq!(
        repo.git(&["symbolic-ref", "--short", "HEAD"]).trim(),
        "git-checkout"
    );
}

#[test]
fn transplant_preflight_does_not_lazily_import_legacy_git_metadata() {
    let repo = TransplantRepo::new();
    repo.linear_changes();
    let current = repo.commit_id("@");
    // Exercise jj's supported compatibility path: indexed commits whose
    // supplemental metadata is absent, and whose keep ref may have been GC'd.
    jj_lib::stacked_table::TableStore::load(repo.path.join(".jj/repo/store/extra"), 20).reinit();
    let keep = format!("refs/jj/keep/{current}");
    repo.git(&["update-ref", "-d", &keep]);
    let before_refs = repo.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
    let operation_heads = || {
        let mut names: Vec<_> = fs::read_dir(repo.path.join(".jj/repo/op_heads"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before_ops = operation_heads();
    let output = repo.jj_unchecked(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
        "--dry-run",
    ]);
    assert!(
        !output.status.success(),
        "legacy metadata must be reconciled separately"
    );
    assert_eq!(
        repo.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
        before_refs
    );
    assert_eq!(operation_heads(), before_ops);
    // The ordinary compatibility path remains available outside transplant.
    repo.log(&current, "commit_id");
    assert_eq!(repo.git(&["rev-parse", &keep]).trim(), current);
}

#[test]
fn transplant_preflight_rejects_git_refs_missing_from_the_jj_view() {
    let repo = TransplantRepo::new();
    repo.linear_changes();
    repo.git(&["branch", "not-imported", &repo.commit_id("@")]);
    let before = repo.state();
    let output = repo.jj_unchecked(&[
        "native",
        "transplant",
        "-r",
        "old-base:: ~ old-base",
        "--map",
        "a=pkg/a",
        "--map",
        "b=pkg/b",
        "--map",
        ".=archive",
        "--parent",
        "old-base=target-base",
        "--dry-run",
    ]);
    assert!(
        !output.status.success(),
        "preflight used a stale Git ref view"
    );
    assert_eq!(repo.state(), before);
}
