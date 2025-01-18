# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Chore

 - <csr-id-17835bccb066bbc47cc137e8ec5d9fe7d5665af0/> bump `rust-version` to 1.70
   That way clippy will allow to use the fantastic `Option::is_some_and()`
   and friends.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 3 commits contributed to the release over the course of 26 calendar days.
 - 26 days passed between releases.
 - 1 commit was understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Merge pull request #1762 from GitoxideLabs/fix-1759 ([`7ec21bb`](https://github.com/GitoxideLabs/gitoxide/commit/7ec21bb96ce05b29dde74b2efdf22b6e43189aab))
    - Bump `rust-version` to 1.70 ([`17835bc`](https://github.com/GitoxideLabs/gitoxide/commit/17835bccb066bbc47cc137e8ec5d9fe7d5665af0))
    - Merge pull request #1739 from GitoxideLabs/new-release ([`d22937f`](https://github.com/GitoxideLabs/gitoxide/commit/d22937f91b8ecd0ece0930c4df9d580f3819b2fe))
</details>

## 0.1.0 (2024-12-22)

### New Features (BREAKING)

 - <csr-id-6367c7d0a796aff8ee8778916c1a1ddae68b654d/> Add `gix-shallow` crate and use it from `gix` and `gix-protocol`
   That way it's easier to reuse shallow-handling code from plumbing crates.
   
   Note that this is a breaking change as `gix-protocol` now uses the `gix-shallow::Update`
   type, which doesn't implement a formerly public `from_line()` method anymore.
   Now it is available as `fetch::response::shallow_update_from_line()`.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 6 commits contributed to the release over the course of 7 calendar days.
 - 1 commit was understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Release gix-dir v0.11.0, gix-revision v0.31.1, gix-merge v0.2.0, gix-pack v0.56.0, gix-odb v0.66.0, gix-shallow v0.1.0, gix-packetline v0.18.2, gix-transport v0.44.0, gix-protocol v0.47.0, gix-status v0.16.0, gix-worktree-state v0.16.0, gix v0.69.0, gitoxide-core v0.44.0, gitoxide v0.40.0 ([`beb0ea8`](https://github.com/GitoxideLabs/gitoxide/commit/beb0ea8c4ff94c64b7773772a9d388ccb403f3c1))
    - Release gix-date v0.9.3, gix-object v0.46.1, gix-command v0.4.0, gix-filter v0.16.0, gix-fs v0.12.1, gix-traverse v0.43.1, gix-worktree-stream v0.18.0, gix-archive v0.18.0, gix-ref v0.49.1, gix-prompt v0.9.0, gix-url v0.28.2, gix-credentials v0.26.0, gix-diff v0.49.0, gix-dir v0.11.0, gix-revision v0.31.1, gix-merge v0.2.0, gix-pack v0.56.0, gix-odb v0.66.0, gix-shallow v0.1.0, gix-packetline v0.18.2, gix-transport v0.44.0, gix-protocol v0.47.0, gix-status v0.16.0, gix-worktree-state v0.16.0, gix v0.69.0, gitoxide-core v0.44.0, gitoxide v0.40.0, safety bump 16 crates ([`c1ba571`](https://github.com/GitoxideLabs/gitoxide/commit/c1ba5719132227410abefeb54e3032b015233e94))
    - Update changelogs prior to release ([`7ea8582`](https://github.com/GitoxideLabs/gitoxide/commit/7ea85821c6999e3e6cf50a2a009904e9c38642a4))
    - Finalize gix-shallow crate ([`2cc65bb`](https://github.com/GitoxideLabs/gitoxide/commit/2cc65bbdeeeb04248aa2570530e21b9f1fdeadda))
    - Merge pull request #1634 from GitoxideLabs/remove-delegates ([`ddeb97f`](https://github.com/GitoxideLabs/gitoxide/commit/ddeb97f550bb95835648841b476d7647dd7c1dc0))
    - Add `gix-shallow` crate and use it from `gix` and `gix-protocol` ([`6367c7d`](https://github.com/GitoxideLabs/gitoxide/commit/6367c7d0a796aff8ee8778916c1a1ddae68b654d))
</details>

