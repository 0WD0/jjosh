# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.2.0 (2025-04-25)

### Bug Fixes

 - <csr-id-b07f907ba2e01849744c72df35dac57b624f2f85/> Adapt to changes in gix-actor
   Use the committer date and author date that are now backed by bytes and
   interpret these bytes into a `gix_date::Time` on demand.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 9 commits contributed to the release.
 - 1 commit was understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Update changelogs prior to release ([`0bf84db`](https://github.com/GitoxideLabs/gitoxide/commit/0bf84dbc041f59efba06adcf422c60b5d6e350f0))
    - Merge pull request #1935 from pierrechevalier83/fix_1923 ([`3b1bef7`](https://github.com/GitoxideLabs/gitoxide/commit/3b1bef7cc40e16b61bcc117ca90ebae21df7c7b1))
    - J fmt ([`c3c6504`](https://github.com/GitoxideLabs/gitoxide/commit/c3c650448f92bcb27194ce0a51f7d604ce87920d))
    - Adapt to changes in gix-actor ([`b07f907`](https://github.com/GitoxideLabs/gitoxide/commit/b07f907ba2e01849744c72df35dac57b624f2f85))
    - Merge pull request #1949 from GitoxideLabs/dependabot/cargo/cargo-6893e2988a ([`b5e9059`](https://github.com/GitoxideLabs/gitoxide/commit/b5e905991155ace32ef21464e69a8369a773f02b))
    - Merge pull request #1945 from cruessler/replace-btreemap-by-smallvec ([`c75bc44`](https://github.com/GitoxideLabs/gitoxide/commit/c75bc44b4f9d3b1c8d48b9dfc42c94576088b8a6))
    - Bump the cargo group with 21 updates ([`68e6b2e`](https://github.com/GitoxideLabs/gitoxide/commit/68e6b2e54613fe788d645ea8c942c71a39c6ede1))
    - Replace BTreeMap by SmallVec ([`75b842b`](https://github.com/GitoxideLabs/gitoxide/commit/75b842b13cc4a17acfd3419263aa1520df10fb01))
    - Merge pull request #1919 from GitoxideLabs/release ([`420e730`](https://github.com/GitoxideLabs/gitoxide/commit/420e730f765b91e1d17daca6bb1f99bdb2e54fda))
</details>

## v0.1.0 (2025-04-04)

<csr-id-f7f136dbe4f86e7dee1d54835c420ec07c96cd78/>
<csr-id-533e887e80c5f7ede8392884562e1c5ba56fb9a8/>

### Chore

 - <csr-id-f7f136dbe4f86e7dee1d54835c420ec07c96cd78/> uniformize deny attributes
 - <csr-id-533e887e80c5f7ede8392884562e1c5ba56fb9a8/> remove default link to cargo doc everywhere

### Bug Fixes

 - <csr-id-e14dc7d475373d2c266e84ff8f1826c68a34ab92/> note that crates have been renamed from `git-*` to `gix-*`.
   This also means that the `git-*` prefixed crates of the `gitoxide` project
   are effectively unmaintained.
   Use the crates with the `gix-*` prefix instead.
   
   If you were using `git-repository`, then `gix` is its substitute.

### New Features (BREAKING)

 - <csr-id-3d8fa8fef9800b1576beab8a5bc39b821157a5ed/> upgrade edition to 2021 in most crates.
   MSRV for this is 1.56, and we are now at 1.60 so should be compatible.
   This isn't more than a patch release as it should break nobody
   who is adhering to the MSRV, but let's be careful and mark it
   breaking.
   
   Note that `git-features` and `git-pack` are still on edition 2018
   as they make use of a workaround to support (safe) mutable access
   to non-overlapping entries in a slice which doesn't work anymore
   in edition 2021.
 - <csr-id-e9a493c204979d1a155c198331277662d26aec58/> add `diff_algorithm` to `blame::file()`
 - <csr-id-e08cf8811e25c91ca410963703ce98db32be3681/> add `since` to `blame::file()`
 - <csr-id-1250df3f9c10f66e4b8e227809831f3088482960/> skip uninteresting commits for blame
   This is breaking because it takes a commitgraph cache as argument
   , and because it replaces the `traverse` by `suspect`.
   
   Switch to date order for traversing the commit history, as opposed to
   topo order. This is also what `git blame` does.
   
   Skip suspects that have no associated unblamed hunks
   
   Pass blame to parent in `process_change`. `git`’s algorithm only seems
   to keep the current suspect for unblamed hunks that were the direct
   result of splitting an existing unblamed hunk because it matched with a
   change. All other hunks appear to be blamed on the parent without
   further checks.
   
   Add assertion that lines always match.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 20 commits contributed to the release.
 - 3 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Thanks Clippy

<csr-read-only-do-not-edit/>

[Clippy](https://github.com/rust-lang/rust-clippy) helped 1 time to make code idiomatic. 

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Release gix-date v0.9.4, gix-utils v0.2.0, gix-actor v0.34.0, gix-features v0.41.0, gix-hash v0.17.0, gix-hashtable v0.8.0, gix-path v0.10.15, gix-validate v0.9.4, gix-object v0.48.0, gix-glob v0.19.0, gix-quote v0.5.0, gix-attributes v0.25.0, gix-command v0.5.0, gix-packetline-blocking v0.18.3, gix-filter v0.18.0, gix-fs v0.14.0, gix-commitgraph v0.27.0, gix-revwalk v0.19.0, gix-traverse v0.45.0, gix-worktree-stream v0.20.0, gix-archive v0.20.0, gix-tempfile v17.0.0, gix-lock v17.0.0, gix-index v0.39.0, gix-config-value v0.14.12, gix-pathspec v0.10.0, gix-ignore v0.14.0, gix-worktree v0.40.0, gix-diff v0.51.0, gix-blame v0.1.0, gix-ref v0.51.0, gix-config v0.44.0, gix-prompt v0.10.0, gix-url v0.30.0, gix-credentials v0.28.0, gix-discover v0.39.0, gix-dir v0.13.0, gix-mailmap v0.26.0, gix-revision v0.33.0, gix-merge v0.4.0, gix-negotiate v0.19.0, gix-pack v0.58.0, gix-odb v0.68.0, gix-refspec v0.29.0, gix-shallow v0.3.0, gix-packetline v0.18.4, gix-transport v0.46.0, gix-protocol v0.49.0, gix-status v0.18.0, gix-submodule v0.18.0, gix-worktree-state v0.18.0, gix v0.71.0, gix-fsck v0.10.0, gitoxide-core v0.46.0, gitoxide v0.42.0, safety bump 48 crates ([`b41312b`](https://github.com/GitoxideLabs/gitoxide/commit/b41312b478b0d19efb330970cf36dba45d0fbfbd))
    - Update changelogs prior to release ([`38dff41`](https://github.com/GitoxideLabs/gitoxide/commit/38dff41d09b6841ff52435464e77cd012dce7645))
    - Merge pull request #1910 from cruessler/add-tree-id-to-either ([`544cdaf`](https://github.com/GitoxideLabs/gitoxide/commit/544cdafbb58bb3e39bf19a19eb02d5296a7361aa))
    - Make use `gix_traverse::commit::Either::tree_id()` ([`3fad860`](https://github.com/GitoxideLabs/gitoxide/commit/3fad860aaffb53fd27b6d2b959ad8a8d1ab9ac63))
    - Merge pull request #1901 from cruessler/make-either-copy ([`85b060c`](https://github.com/GitoxideLabs/gitoxide/commit/85b060c777cb893c85d60168f9b748ce78c0f146))
    - Derive Clone and Copy for Either ([`3c1b1df`](https://github.com/GitoxideLabs/gitoxide/commit/3c1b1df9320c11e754931e292689c6075bddbfa9))
    - Merge pull request #1888 from cruessler/respect-diff-algorithm-in-blame ([`dce127e`](https://github.com/GitoxideLabs/gitoxide/commit/dce127e63f7788c5424e2da2cf4e3112f9c3b159))
    - Add `diff_algorithm` to `blame::file()` ([`e9a493c`](https://github.com/GitoxideLabs/gitoxide/commit/e9a493c204979d1a155c198331277662d26aec58))
    - Merge pull request #1858 from cruessler/add-git-blame-since ([`7059609`](https://github.com/GitoxideLabs/gitoxide/commit/70596096e35ff8a910dacd6fefdc31d162282b81))
    - Add `since` to `blame::file()` ([`e08cf88`](https://github.com/GitoxideLabs/gitoxide/commit/e08cf8811e25c91ca410963703ce98db32be3681))
    - Merge pull request #1854 from GitoxideLabs/montly-report ([`16a248b`](https://github.com/GitoxideLabs/gitoxide/commit/16a248beddbfbd21621f2bb57aaa82dca35acb19))
    - Thanks clippy ([`8e96ed3`](https://github.com/GitoxideLabs/gitoxide/commit/8e96ed37db680855d194c10673ba2dab28655d95))
    - Merge pull request #1824 from cruessler/replace-find-commit-by-find ([`8ab0a6b`](https://github.com/GitoxideLabs/gitoxide/commit/8ab0a6b458327d3dc057bec3d4e09bea04dee388))
    - Replace `odb.find_commit` by `gix_traverse::commit::find` ([`e09ec3e`](https://github.com/GitoxideLabs/gitoxide/commit/e09ec3e438b5503f21eb784c5781b52e0b1f8a1b))
    - Merge pull request #1743 from cruessler/skip-uninteresting-commits-for-blame ([`aa05ef0`](https://github.com/GitoxideLabs/gitoxide/commit/aa05ef0d143d7ca14272f6cd36a40d2ed839fe76))
    - Refactor ([`4428838`](https://github.com/GitoxideLabs/gitoxide/commit/442883800bc3abe63592ec36cb03b7c7e55c0f34))
    - Skip uninteresting commits for blame ([`1250df3`](https://github.com/GitoxideLabs/gitoxide/commit/1250df3f9c10f66e4b8e227809831f3088482960))
    - Merge pull request #1823 from cruessler/add-test-for-differing-date-and-topo-order ([`18e163e`](https://github.com/GitoxideLabs/gitoxide/commit/18e163e5df653f698a356b26da4f7e1c31fac9ad))
    - Add test for commits not ordered chronologically ([`a9de4f0`](https://github.com/GitoxideLabs/gitoxide/commit/a9de4f0898148eb45ca8a229c14e65f5dbf56906))
    - Merge pull request #1778 from GitoxideLabs/new-release ([`8df0db2`](https://github.com/GitoxideLabs/gitoxide/commit/8df0db2f8fe1832a5efd86d6aba6fb12c4c855de))
</details>

## v0.0.0 (2025-01-18)

<csr-id-17835bccb066bbc47cc137e8ec5d9fe7d5665af0/>
<csr-id-64ff0a77062d35add1a2dd422bb61075647d1a36/>

### New Features (BREAKING)

 - <csr-id-787cf6f5a838a96da49330c99a8530ac3206de50/> add `range` to `blame::file()`

### New Features

 - <csr-id-4ffe6eb8f7921c6a03db0aa6d796cc2e3cc328e0/> Add support for statistics and additional performance information.
 - <csr-id-25efbfb72e5a043ce8f7d196c1f7104ef93394df/> Add `blame` plumbing crate to the top-level.
   For now, it doesn't come with a simplified `gix` API though.
 - <csr-id-17835bccb066bbc47cc137e8ec5d9fe7d5665af0/> bump `rust-version` to 1.70
   That way clippy will allow to use the fantastic `Option::is_some_and()`
   and friends.
 - <csr-id-64ff0a77062d35add1a2dd422bb61075647d1a36/> Update gitoxide repository URLs

### Chore

 - <csr-id-17835bccb066bbc47cc137e8ec5d9fe7d5665af0/> bump `rust-version` to 1.70
   That way clippy will allow to use the fantastic `Option::is_some_and()`
   and friends.

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

 - 47 commits contributed to the release.
 - 5 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Release gix-utils v0.1.14, gix-actor v0.33.2, gix-hash v0.16.0, gix-trace v0.1.12, gix-features v0.40.0, gix-hashtable v0.7.0, gix-path v0.10.14, gix-validate v0.9.3, gix-object v0.47.0, gix-glob v0.18.0, gix-quote v0.4.15, gix-attributes v0.24.0, gix-command v0.4.1, gix-packetline-blocking v0.18.2, gix-filter v0.17.0, gix-fs v0.13.0, gix-chunk v0.4.11, gix-commitgraph v0.26.0, gix-revwalk v0.18.0, gix-traverse v0.44.0, gix-worktree-stream v0.19.0, gix-archive v0.19.0, gix-bitmap v0.2.14, gix-tempfile v16.0.0, gix-lock v16.0.0, gix-index v0.38.0, gix-config-value v0.14.11, gix-pathspec v0.9.0, gix-ignore v0.13.0, gix-worktree v0.39.0, gix-diff v0.50.0, gix-blame v0.0.0, gix-ref v0.50.0, gix-sec v0.10.11, gix-config v0.43.0, gix-prompt v0.9.1, gix-url v0.29.0, gix-credentials v0.27.0, gix-discover v0.38.0, gix-dir v0.12.0, gix-mailmap v0.25.2, gix-revision v0.32.0, gix-merge v0.3.0, gix-negotiate v0.18.0, gix-pack v0.57.0, gix-odb v0.67.0, gix-refspec v0.28.0, gix-shallow v0.2.0, gix-packetline v0.18.3, gix-transport v0.45.0, gix-protocol v0.48.0, gix-status v0.17.0, gix-submodule v0.17.0, gix-worktree-state v0.17.0, gix v0.70.0, gix-fsck v0.9.0, gitoxide-core v0.45.0, gitoxide v0.41.0, safety bump 42 crates ([`dea106a`](https://github.com/GitoxideLabs/gitoxide/commit/dea106a8c4fecc1f0a8f891a2691ad9c63964d25))
    - Don't specify version numbers in dev-dependencies ([`7570daa`](https://github.com/GitoxideLabs/gitoxide/commit/7570daa50a93a2b99e9cd5228cb274f20839865f))
    - Update all changelogs prior to release ([`1f6390c`](https://github.com/GitoxideLabs/gitoxide/commit/1f6390c53ba68ce203ae59eb3545e2631dd8a106))
    - Merge pull request #1766 from cruessler/add-range-to-gix-blame ([`90fef01`](https://github.com/GitoxideLabs/gitoxide/commit/90fef0148376167763a3ebeff91a1cf9c236cf8a))
    - Refactor ([`1500c08`](https://github.com/GitoxideLabs/gitoxide/commit/1500c08736069153aab33842d2d877f42ad01f37))
    - Add `range` to `blame::file()` ([`787cf6f`](https://github.com/GitoxideLabs/gitoxide/commit/787cf6f5a838a96da49330c99a8530ac3206de50))
    - Merge pull request #1762 from GitoxideLabs/fix-1759 ([`7ec21bb`](https://github.com/GitoxideLabs/gitoxide/commit/7ec21bb96ce05b29dde74b2efdf22b6e43189aab))
    - Bump `rust-version` to 1.70 ([`17835bc`](https://github.com/GitoxideLabs/gitoxide/commit/17835bccb066bbc47cc137e8ec5d9fe7d5665af0))
    - Merge pull request #1756 from cruessler/extract-object-ids-in-tests ([`f18a312`](https://github.com/GitoxideLabs/gitoxide/commit/f18a3129b11c53e7922295908a6930039b8203c3))
    - Extract hard-coded ObjectIds in tests ([`50ba3d6`](https://github.com/GitoxideLabs/gitoxide/commit/50ba3d6aa60a67cbacb2aa7411e3f20c3c6cf0c0))
    - Merge pull request #1755 from cruessler/shortcut-tree-diffing-minor-cleanups ([`25c2646`](https://github.com/GitoxideLabs/gitoxide/commit/25c2646f2c7f0430791fc14131a7e103f3c9cac7))
    - Prefix variant to disambiguate from continue ([`ec3cdf1`](https://github.com/GitoxideLabs/gitoxide/commit/ec3cdf1520837db9a94257db3b08099e34892baa))
    - Merge pull request #1754 from GitoxideLabs/fix-ci ([`34096a5`](https://github.com/GitoxideLabs/gitoxide/commit/34096a5796f03f76e8ed696b886fbd62eb09d2cc))
    - Fix clippy ([`6805beb`](https://github.com/GitoxideLabs/gitoxide/commit/6805beb31609bff9dad1807901d8901024ab1d3c))
    - Merge pull request #1753 from GitoxideLabs/wip-changes-against-more-than-one-parent ([`a22f13b`](https://github.com/GitoxideLabs/gitoxide/commit/a22f13bec0cdd580ee92390a98d5d522eb29978d))
    - Refactor ([`360bf38`](https://github.com/GitoxideLabs/gitoxide/commit/360bf383a3ebdeeda1db161d42bb057a05cdf32b))
    - Rework how blame is passed to parents ([`a3d92b4`](https://github.com/GitoxideLabs/gitoxide/commit/a3d92b4d1f129b18217d789273c4991964891de0))
    - Merge pull request #1747 from cruessler/shortcut-tree-diffing ([`59bd978`](https://github.com/GitoxideLabs/gitoxide/commit/59bd978ba560295ed4fcb86f1a629e3c728dd5dd))
    - Update doc-string ([`9ac36bd`](https://github.com/GitoxideLabs/gitoxide/commit/9ac36bdd0af860df24c303d0d4a789b324ab2c43))
    - Rename to FindChangeToPath and move to where it's used ([`f857ca8`](https://github.com/GitoxideLabs/gitoxide/commit/f857ca86f88b25dc1ce1ca7c90db05793828ddf0))
    - Simplify Recorder by wrapping gix_diff::tree::Recorder ([`7d1416a`](https://github.com/GitoxideLabs/gitoxide/commit/7d1416a9124c16e757a3e7cb3fd762c9e52973bb))
    - Don't ignore gix_diff::tree errors ([`f049b00`](https://github.com/GitoxideLabs/gitoxide/commit/f049b00b9d59b3eff4c9489557d9d709f96fdd67))
    - Cancel tree diffing early when matching path is found ([`74565bc`](https://github.com/GitoxideLabs/gitoxide/commit/74565bc2c5ab46348a0e9182e7b9d946dfbc0dd8))
    - Merge pull request #1453 from cruessler/gix-blame ([`6ed9976`](https://github.com/GitoxideLabs/gitoxide/commit/6ed9976abaa3915b50efa46c46b195f3a1fc4ff7))
    - For linear histories, avoid redoing path lookup work ([`8196a43`](https://github.com/GitoxideLabs/gitoxide/commit/8196a433ed08de6b09b5cb187f8ce53fc2ab09ca))
    - Don't panic when suspect isn't known when converting unblamed to blame-entry ([`667e626`](https://github.com/GitoxideLabs/gitoxide/commit/667e6262bcba1d95e32795faa79dc6b354da9a01))
    - Additional pass of refactoring, focus on the algorithm itself. ([`3ac8be1`](https://github.com/GitoxideLabs/gitoxide/commit/3ac8be1557de8a66ff32abe3d1c9ea83198d4a05))
    - Review and remove all TODOs where possible, update docs and comments ([`63ee0f9`](https://github.com/GitoxideLabs/gitoxide/commit/63ee0f9c34dc89ad51d5c9ab83e49cbc08e3ed69))
    - Swap blamed-file and original-file variable names. ([`b7f1468`](https://github.com/GitoxideLabs/gitoxide/commit/b7f1468f0fe38a50ad3414efb5efcf3ac0d2fddb))
    - Replace todos!() with assertions or remove them. ([`b736ace`](https://github.com/GitoxideLabs/gitoxide/commit/b736ace18e8996b410a597fb4f43bf28f422dfc5))
    - Add `Error` type ([`845d96a`](https://github.com/GitoxideLabs/gitoxide/commit/845d96a4ffff89703a8c3815ac52adc7f2b286f6))
    - Add support for statistics and additional performance information. ([`4ffe6eb`](https://github.com/GitoxideLabs/gitoxide/commit/4ffe6eb8f7921c6a03db0aa6d796cc2e3cc328e0))
    - Remove duplication and unnecessary parameter ([`a158d22`](https://github.com/GitoxideLabs/gitoxide/commit/a158d22703077d37b83e0434aa229baf12c342ed))
    - Unify how lines in blame results are accessed ([`f2790a9`](https://github.com/GitoxideLabs/gitoxide/commit/f2790a9db8cac3ce57003b512edf735e734383d1))
    - Modularlize `gix-blame/lib.rs` ([`26bfd2d`](https://github.com/GitoxideLabs/gitoxide/commit/26bfd2d73374e134aff24410fac44857b8128244))
    - First review round ([`983ec7d`](https://github.com/GitoxideLabs/gitoxide/commit/983ec7d776b459898b90927242582fc03a0e9056))
    - Add `blame` plumbing crate to the top-level. ([`25efbfb`](https://github.com/GitoxideLabs/gitoxide/commit/25efbfb72e5a043ce8f7d196c1f7104ef93394df))
    - Add initial implementation and tests for `gix-blame`. ([`d27adf7`](https://github.com/GitoxideLabs/gitoxide/commit/d27adf70b4e2f57d8431a0a553119322d7158f4b))
    - Merge pull request #1624 from EliahKagan/update-repo-url ([`795962b`](https://github.com/GitoxideLabs/gitoxide/commit/795962b107d86f58b1f7c75006da256d19cc80ad))
    - Update gitoxide repository URLs ([`64ff0a7`](https://github.com/GitoxideLabs/gitoxide/commit/64ff0a77062d35add1a2dd422bb61075647d1a36))
    - Merge pull request #1589 from EliahKagan/maintenance ([`7c2af44`](https://github.com/GitoxideLabs/gitoxide/commit/7c2af442748f7245734ec1f987b6d839f2a795bd))
    - Add missing executable bits ([`694ebad`](https://github.com/GitoxideLabs/gitoxide/commit/694ebadb2d11d25c5b1285c61cef5df03685701a))
    - Merge branch 'global-lints' ([`37ba461`](https://github.com/GitoxideLabs/gitoxide/commit/37ba4619396974ec9cc41d1e882ac5efaf3816db))
    - Workspace Clippy lint management ([`2e0ce50`](https://github.com/GitoxideLabs/gitoxide/commit/2e0ce506968c112b215ca0056bd2742e7235df48))
    - Merge branch 'gix-blame' ([`e6fbea9`](https://github.com/GitoxideLabs/gitoxide/commit/e6fbea9be2ef7ab4064dc57c8233dfe81fac3bb4))
    - Add sample fixture ([`6d71e0d`](https://github.com/GitoxideLabs/gitoxide/commit/6d71e0d291f2a3b11c635949712ec86cf57d7449))
    - Add new `gix-blame` crate ([`f5f616d`](https://github.com/GitoxideLabs/gitoxide/commit/f5f616d8345898effc79d587c139e249f1c85ab6))
</details>

