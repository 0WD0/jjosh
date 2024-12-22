# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### New Features (BREAKING)

 - <csr-id-6367c7d0a796aff8ee8778916c1a1ddae68b654d/> Add `gix-shallow` crate and use it from `gix` and `gix-protocol`
   That way it's easier to reuse shallow-handling code from plumbing crates.
   
   Note that this is a breaking change as `gix-protocol` now uses the `gix-shallow::Update`
   type, which doesn't implement a formerly public `from_line()` method anymore.
   Now it is available as `fetch::response::shallow_update_from_line()`.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 3 commits contributed to the release over the course of 7 calendar days.
 - 1 commit was understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Finalize gix-shallow crate ([`2cc65bb`](https://github.com/GitoxideLabs/gitoxide/commit/2cc65bbdeeeb04248aa2570530e21b9f1fdeadda))
    - Merge pull request #1634 from GitoxideLabs/remove-delegates ([`ddeb97f`](https://github.com/GitoxideLabs/gitoxide/commit/ddeb97f550bb95835648841b476d7647dd7c1dc0))
    - Add `gix-shallow` crate and use it from `gix` and `gix-protocol` ([`6367c7d`](https://github.com/GitoxideLabs/gitoxide/commit/6367c7d0a796aff8ee8778916c1a1ddae68b654d))
</details>

