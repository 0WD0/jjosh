

## 0.2.2 (2026-05-26)

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 6 commits contributed to the release over the course of 28 calendar days.
 - 28 days passed between releases.
 - 0 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Merge pull request #2568 from GitoxideLabs/dependabot/cargo/cargo-56d6b174d8 ([`ab2fee1`](https://github.com/GitoxideLabs/gitoxide/commit/ab2fee14651202fcb7b3d8178932090c73492014))
    - Update crates to Rust 2024 edition ([`2cb17b2`](https://github.com/GitoxideLabs/gitoxide/commit/2cb17b2e7f6009693a55af907614f705a29d8c29))
    - Raise MSRV for hash dependency updates ([`3675a8d`](https://github.com/GitoxideLabs/gitoxide/commit/3675a8d61b17845a783bc27912a3f52ac273a4af))
    - Bump the cargo group across 1 directory with 10 updates ([`4c77f81`](https://github.com/GitoxideLabs/gitoxide/commit/4c77f81e19b86979495abcf46401a4f226163177))
    - Merge pull request #2532 from cruessler/run-gix-diff-tests-with-sha-256 ([`7fbb9be`](https://github.com/GitoxideLabs/gitoxide/commit/7fbb9be28e1cf3dc3af874a02dc688374e109cf8))
    - Merge pull request #2546 from GitoxideLabs/fix-2545 ([`adb8328`](https://github.com/GitoxideLabs/gitoxide/commit/adb8328952478c443ead5f5a8c6851928b377b37))
</details>

## 0.2.1 (2026-04-28)

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 3 commits contributed to the release over the course of 2 calendar days.
 - 3 days passed between releases.
 - 0 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Release gix-error v0.2.3, gix-date v0.15.3, gix-actor v0.41.0, gix-path v0.12.0, gix-features v0.48.0, gix-hash v0.25.0, gix-hashtable v0.15.0, gix-object v0.60.0, gix-glob v0.26.0, gix-attributes v0.33.0, gix-command v0.9.0, gix-filter v0.30.0, gix-fs v0.21.0, gix-commitgraph v0.37.0, gix-revwalk v0.31.0, gix-traverse v0.57.0, gix-worktree-stream v0.32.0, gix-archive v0.32.0, gix-tempfile v23.0.0, gix-lock v23.0.0, gix-index v0.51.0, gix-config-value v0.18.0, gix-pathspec v0.18.0, gix-ignore v0.21.0, gix-worktree v0.52.0, gix-imara-diff v0.2.1, gix-diff v0.63.0, gix-blame v0.13.0, gix-ref v0.63.0, gix-sec v0.14.0, gix-config v0.56.0, gix-prompt v0.15.0, gix-url v0.36.0, gix-credentials v0.38.0, gix-discover v0.51.0, gix-dir v0.25.0, gix-mailmap v0.33.0, gix-revision v0.45.0, gix-merge v0.16.0, gix-negotiate v0.31.0, gix-pack v0.70.0, gix-odb v0.80.0, gix-refspec v0.41.0, gix-shallow v0.12.0, gix-transport v0.57.0, gix-protocol v0.61.0, gix-status v0.30.0, gix-submodule v0.30.0, gix-worktree-state v0.30.0, gix v0.83.0, gix-fsck v0.21.0, gitoxide-core v0.57.0, gitoxide v0.53.0, safety bump 48 crates ([`53f880c`](https://github.com/GitoxideLabs/gitoxide/commit/53f880c7604232c367870088176e42efd8a5b783))
    - Remove `memchr` dependency from `gix-imara-diff` ([`f267626`](https://github.com/GitoxideLabs/gitoxide/commit/f26762623b22314c7571ccf6668c12fb70e4941b))
    - Merge pull request #2540 from GitoxideLabs/reporting ([`4d5ba23`](https://github.com/GitoxideLabs/gitoxide/commit/4d5ba231685e8ff36195603c57193aa1cd21fa8e))
</details>

## 0.2.0 (2026-04-24)

### Bug Fixes

 - <csr-id-7a1b9cd0224956e86f9db4cf5098f879eea195a3/> non-terminating MyersMinimal split loop`
   The clusterfuzz testcase
   `clusterfuzz-testcase-minimized-gix-imara-diff-comprehensive_diff-6497314075377664`
   was timing out in the Myers implementation while running the new
   `comprehensive_diff` fuzz target.
   
   Root cause

### New Features (BREAKING)

 - <csr-id-8094f5dcd4f24f4d54f7fbe7f716f80f2974b586/> Use `imara-diff-v2` with git sliders processing
   The slider post-processing imrpoves the diff quality for about 8% slower diffs.
   Line-counts, however, will be 50% faster to compute.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 14 commits contributed to the release over the course of 11 calendar days.
 - 2 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Thanks Clippy

<csr-read-only-do-not-edit/>

[Clippy](https://github.com/rust-lang/rust-clippy) helped 1 time to make code idiomatic. 

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Update changelogs prior to release ([`f9fbcba`](https://github.com/GitoxideLabs/gitoxide/commit/f9fbcba28278f3fb2ad7969c2d00ac6765165724))
    - Merge pull request #2530 from GitoxideLabs/advisories ([`63b8419`](https://github.com/GitoxideLabs/gitoxide/commit/63b841907ce30b36bb50da5aae3a9e1a06eadf64))
    - Add fuzz tests for 10 more crates, and related fixes ([`0396152`](https://github.com/GitoxideLabs/gitoxide/commit/03961523d0208a12b7b480b14d57793049600283))
    - Remove profile.release section in gix-imara-diff ([`6945969`](https://github.com/GitoxideLabs/gitoxide/commit/6945969015741a54b324056e769501fd3f42d6c6))
    - Merge pull request #2524 from GitoxideLabs/reproduce-fuzz-diff-timeout ([`353940d`](https://github.com/GitoxideLabs/gitoxide/commit/353940dee9fdabe3301d3fb8132c84228b9e8d95))
    - Non-terminating MyersMinimal split loop` ([`7a1b9cd`](https://github.com/GitoxideLabs/gitoxide/commit/7a1b9cd0224956e86f9db4cf5098f879eea195a3))
    - Merge pull request #2513 from GitoxideLabs/v2-diff ([`2a5db88`](https://github.com/GitoxideLabs/gitoxide/commit/2a5db88d0330b0d125de4b6f3819f17a7f76f4b8))
    - Thanks clippy ([`e4f380e`](https://github.com/GitoxideLabs/gitoxide/commit/e4f380eff3b0440002f7e9b64a14ddcfbe63192a))
    - Last stretch to fix CI ([`1be2d4d`](https://github.com/GitoxideLabs/gitoxide/commit/1be2d4dff8a5000a147f4e36861a8d929f07cd91))
    - Optimise gix-imara-diff manifest. ([`3ec346b`](https://github.com/GitoxideLabs/gitoxide/commit/3ec346b41febc0b931c449b2e8703a8654b808cb))
    - Add license attributions to `gix-imara-diff` properly ([`e2d767d`](https://github.com/GitoxideLabs/gitoxide/commit/e2d767df8fa01d9977289fa009d7fced4e6df666))
    - Use `imara-diff-v2` with git sliders processing ([`8094f5d`](https://github.com/GitoxideLabs/gitoxide/commit/8094f5dcd4f24f4d54f7fbe7f716f80f2974b586))
    - Merge pull request #2506 from GitoxideLabs/vendor-imara-diff ([`8f091d1`](https://github.com/GitoxideLabs/gitoxide/commit/8f091d108cd75371be2ed9de6e81f785cda53d92))
    - Vendor `imara-diff` 0.1 and 0.2 ([`fd49295`](https://github.com/GitoxideLabs/gitoxide/commit/fd49295c5ed4a57bf5771e23c0f803435990ecfa))
</details>

