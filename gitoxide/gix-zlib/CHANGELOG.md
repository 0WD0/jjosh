

## Unreleased

### New Features

 - <csr-id-519e2d88a91d33b1576eabc5df0c2eccd7722fa0/> add `gix-zlib` crate
   This merely moves the `zlib` module into its own crate.
   Previously, there were multiple backends, but these times
   are over for a while and there is only one implementation: zlib-rs.

### New Features (BREAKING)

 - <csr-id-6936e855adbce26c3acdc2ac2c3a6c6eef4e643e/> make zlib compression levels configurable
   Introduce a validated Compression type and require callers to choose a level when creating deflate streams. Optional serde support allows options in dependent crates to retain their serialization APIs.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 9 commits contributed to the release over the course of 13 calendar days.
 - 2 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Update changelogs prior to release ([`3e996df`](https://github.com/GitoxideLabs/gitoxide/commit/3e996dff31d52bc1338402f461a4985bb2f71b9e))
    - Merge pull request #2722 from GitoxideLabs/reasons ([`c16b5a1`](https://github.com/GitoxideLabs/gitoxide/commit/c16b5a1892704b7c72a253bdd74a6848dd61032a))
    - Replace lint allowances with expectations ([`43ff87a`](https://github.com/GitoxideLabs/gitoxide/commit/43ff87a73897b70313e3a58e7de82231be5b59ad))
    - Merge pull request #2695 from ameyypawar/fix/2024-compression-level ([`6e1c4a2`](https://github.com/GitoxideLabs/gitoxide/commit/6e1c4a24813d99ad0bbfb231618210ffe6a5cd6a))
    - Review ([`f1ac335`](https://github.com/GitoxideLabs/gitoxide/commit/f1ac3359c3d88f550219116f1f3e8cb107f5f86f))
    - Make zlib compression levels configurable ([`6936e85`](https://github.com/GitoxideLabs/gitoxide/commit/6936e855adbce26c3acdc2ac2c3a6c6eef4e643e))
    - Merge pull request #2707 from ameyypawar/fix/2703-inflate-error ([`6d95da6`](https://github.com/GitoxideLabs/gitoxide/commit/6d95da6e7082e19a03123ad765b3d5f117731621))
    - Adapt to changes in `gix-features`, use `gix-zlib` accordingly. ([`9c2977a`](https://github.com/GitoxideLabs/gitoxide/commit/9c2977a3b6d540690a1a263a037c8d54c316a020))
    - Add `gix-zlib` crate ([`519e2d8`](https://github.com/GitoxideLabs/gitoxide/commit/519e2d88a91d33b1576eabc5df0c2eccd7722fa0))
</details>

