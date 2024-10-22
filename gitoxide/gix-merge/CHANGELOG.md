# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.0.0 (2024-10-22)

<csr-id-64ff0a77062d35add1a2dd422bb61075647d1a36/>

### Other

 - <csr-id-64ff0a77062d35add1a2dd422bb61075647d1a36/> Update gitoxide repository URLs
   This updates `Byron/gitoxide` URLs to `GitoxideLabs/gitoxide` in:
   
   - Markdown documentation, except changelogs and other such files
     where such changes should not be made.
   
   - Documentation comments (in .rs files).
   
   - Manifest (.toml) files, for the value of the `repository` key.
   
   - The comments appearing at the top of a sample hook that contains
     a repository URL as an example.
   
   When making these changes, I also allowed my editor to remove
   trailing whitespace in any lines in files already being edited
   (since, in this case, there was no disadvantage to allowing this).
   
   The gitoxide repository URL changed when the repository was moved
   into the recently created GitHub organization `GitoxideLabs`, as
   detailed in #1406. Please note that, although I believe updating
   the URLs to their new canonical values is useful, this is not
   needed to fix any broken links, since `Byron/gitoxide` URLs
   redirect (and hopefully will always redirect) to the coresponding
   `GitoxideLabs/gitoxide` URLs.
   
   While this change should not break any URLs, some affected URLs
   were already broken. This updates them, but they are still broken.
   They will be fixed in a subsequent commit.
   
   This also does not update `Byron/gitoxide` URLs in test fixtures
   or test cases, nor in the `Makefile`. (It may make sense to change
   some of those too, but it is not really a documentation change.)

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 14 commits contributed to the release over the course of 26 calendar days.
 - 1 commit was understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Add new changelog for gix-merge ([`fa3e260`](https://github.com/Byron/gitoxide/commit/fa3e2600d7e39011f1d7f410249ebd0426a348a8))
    - Release gix-date v0.9.1, gix-utils v0.1.13, gix-actor v0.33.0, gix-hash v0.15.0, gix-trace v0.1.11, gix-features v0.39.0, gix-hashtable v0.6.0, gix-validate v0.9.1, gix-object v0.45.0, gix-path v0.10.12, gix-glob v0.17.0, gix-quote v0.4.13, gix-attributes v0.23.0, gix-command v0.3.10, gix-packetline-blocking v0.18.0, gix-filter v0.14.0, gix-fs v0.12.0, gix-chunk v0.4.9, gix-commitgraph v0.25.0, gix-revwalk v0.16.0, gix-traverse v0.42.0, gix-worktree-stream v0.16.0, gix-archive v0.16.0, gix-config-value v0.14.9, gix-tempfile v15.0.0, gix-lock v15.0.0, gix-ref v0.48.0, gix-sec v0.10.9, gix-config v0.41.0, gix-prompt v0.8.8, gix-url v0.28.0, gix-credentials v0.25.0, gix-ignore v0.12.0, gix-bitmap v0.2.12, gix-index v0.36.0, gix-worktree v0.37.0, gix-diff v0.47.0, gix-discover v0.36.0, gix-pathspec v0.8.0, gix-dir v0.9.0, gix-mailmap v0.25.0, gix-merge v0.0.0, gix-negotiate v0.16.0, gix-pack v0.54.0, gix-odb v0.64.0, gix-packetline v0.18.0, gix-transport v0.43.0, gix-protocol v0.46.0, gix-revision v0.30.0, gix-refspec v0.26.0, gix-status v0.14.0, gix-submodule v0.15.0, gix-worktree-state v0.14.0, gix v0.67.0, gix-fsck v0.7.0, gitoxide-core v0.42.0, gitoxide v0.38.0, safety bump 41 crates ([`3f7e8ee`](https://github.com/Byron/gitoxide/commit/3f7e8ee2c5107aec009eada1a05af7941da9cb4d))
    - Merge pull request #1624 from EliahKagan/update-repo-url ([`795962b`](https://github.com/Byron/gitoxide/commit/795962b107d86f58b1f7c75006da256d19cc80ad))
    - Update gitoxide repository URLs ([`64ff0a7`](https://github.com/Byron/gitoxide/commit/64ff0a77062d35add1a2dd422bb61075647d1a36))
    - Merge pull request #1612 from Byron/merge ([`37c1e4c`](https://github.com/Byron/gitoxide/commit/37c1e4c919382c9d213bd5ca299ed659d63ab45d))
    - Add some performance traces for blob-merges ([`b25fe4d`](https://github.com/Byron/gitoxide/commit/b25fe4d052250ddace9a118b2247537b2a7fa09e))
    - Merge pull request #1611 from Byron/merge ([`5ffccd2`](https://github.com/Byron/gitoxide/commit/5ffccd2f08d70576347e3ae17a66ca5a60f1d81c))
    - Make `blob::Platform::filter_mode` public. ([`b26eb26`](https://github.com/Byron/gitoxide/commit/b26eb2618c1764f699530e251ddef4e6cf456ddd))
    - Merge pull request #1585 from Byron/merge ([`2261de4`](https://github.com/Byron/gitoxide/commit/2261de470aeb77be080f9e423e1513bde85d9cc0))
    - Add platform tests and implementation ([`eb37dc3`](https://github.com/Byron/gitoxide/commit/eb37dc36d8c42f5a7714c641244ce4a13111b0a1))
    - Add all relevant tests for the merge processing pipeline ([`a6f3e30`](https://github.com/Byron/gitoxide/commit/a6f3e30017343c01ba61c49fe74ffc69e443a33c))
    - Implement `text` and `binary` merge algorithms, also with baseline tests for correctness. ([`0762846`](https://github.com/Byron/gitoxide/commit/07628465a0a3f047ec809d287c9a4567b4acd607))
    - Sketch the entire API surface to capture all parts of blob-merges ([`9efa09f`](https://github.com/Byron/gitoxide/commit/9efa09f10d042dc4d5db4edf4589594450a30b31))
    - Add the `gix-merge` crate for capturing merge algorithms ([`65efcb7`](https://github.com/Byron/gitoxide/commit/65efcb7624031ae478056fbb49a07ef176f0c96b))
</details>

