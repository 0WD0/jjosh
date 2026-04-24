

## Unreleased

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

 - 12 commits contributed to the release over the course of 11 calendar days.
 - 2 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Thanks Clippy

<csr-read-only-do-not-edit/>

[Clippy](https://github.com/rust-lang/rust-clippy) helped 1 time to make code idiomatic. 

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Merge pull request #2530 from GitoxideLabs/advisories ([`63b8419`](https://github.com/GitoxideLabs/gitoxide/commit/63b841907ce30b36bb50da5aae3a9e1a06eadf64))
    - Add fuzz tests for 10 more crates, and related fixes ([`0396152`](https://github.com/GitoxideLabs/gitoxide/commit/03961523d0208a12b7b480b14d57793049600283))
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

