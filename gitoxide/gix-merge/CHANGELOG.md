# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### New Features

 - <csr-id-3ee8b62dd025d6fdb0d9929dec7a561fa576f545/> provide a way to record and apply index changes.
   These changes will then be applicable to an index that is created
   from the written tree editor.
 - <csr-id-09213bc1b2aa725af1571dff040415772e844c3a/> when blob-merging, clarify if something would have conflicted.
 - <csr-id-9e106c4ab7ceea091cd8baef99a480739bd53c9d/> add `Conflict::is_unresolved()` as utility to see if multiple of them are considered unresolved.
 - <csr-id-bd91d6ae97d1981a2366136040407590f64fdad4/> respect the `conflict-marker-size` attribute as well.
 - <csr-id-4b1764ca9148e08ae9f11bca68e0689b12bc8c80/> add `tree()` and `commit()` merge support, en par with `merge-ORT` as far as tests go.
   Note that this judgement of quality is based on a limited amount of partially complex
   test, but it's likely that in practice there will be deviations of sorts.
   
   Also, given the complexity of the implementation it is definitely under-tested,
   but with that it's mostly en par with Git, unfortunatly.
   
   On the bright side, some of the tests are very taxing and I'd hope this
   means something for real-world quality.
 - <csr-id-dd99991ec2bfb07ef571769abc32f1b35122d5ca/> add `blob::PlatformRef::id_by_pick()` to more efficiently pick merge results.
   This works by either selecting a possibly unchanged and not even loaded resource,
   instead of always loading it to provide a buffer, in case the user doesn't
   actually want a buffer.
   
   Note that this also alters `buffer_by_pick()` to enforce handling of the 'buffer-too-large'
   option.

### Other

 - <csr-id-2fdbcfe17cdcc480e320582d7c6b48f8b615bf3b/> Fix code fences in gix-merge `ConflictStyle` and `Driver`
   They are not Rust code (they are text with conflict markers and a
   shell command, respectively) and they are not intended as doctests,
   but the absence of anything on the opening line caused them to be
   taken as doctests, so `cargo test --workspace --doc` would fail
   with parsing errors.
   
   (Doctests for all crates have not always been run automatically on
   CI, so this was not caught when these documentation comments were
   introduced in #1585.)

### New Features (BREAKING)

 - <csr-id-aff76f291a52fc6806944d72d249a8bd1b804c39/> Add more modes for checking for unresolved conflicts.
   They aim at making it possible to know if a conflict happened that was
   automatically resolved.
 - <csr-id-9d43b753a225482645b22e4151bf7dc192c8c082/> add `commit::virtual_merge_base()` to produce the single merge-base to use.
   This allows more flexibility in conjunction with tree-merging, as
   commits as input aren't required.
   
   This is breaking as it changes the return value of `commit()`.
 - <csr-id-1d2262f2ca416e3c22f9601e7eab11f3372b2128/> `Repository::merge_trees()` now has a fully-wrapped outcome.
   That way, more attached types are used for greater convenience.
 - <csr-id-c1cf08cc47f98abda017544b1791ab3b4463cc77/> Don't fail on big files during blob-merge, but turn them into binary merges.
   Binary merges are mere choices of which side to pick, which works well for big files
   as well. Git doesn't define this well during its own merges, so there is some room here.

### Bug Fixes (BREAKING)

 - <csr-id-de1cfb6a9caf5ac086c6411824835c75e888e2d7/> Adjust blob-merge baseline to also test the reverse of each operation
   This also fixes an issue with blob merge computations.
   
   It's breaking because the marker-size was reduced to `u8`.
 - <csr-id-78a535572643a0657348aea3c2fed0123505f7fe/> prefer to receive borrowed `gix_command::Context` when it's just passed on.
   That way, the clone occours only when needed, without forcing the caller
   to pre-emptively clone each time it's called.

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 26 commits contributed to the release.
 - 13 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Merge pull request #1661 from GitoxideLabs/merge ([`0b7abfb`](https://github.com/GitoxideLabs/gitoxide/commit/0b7abfbdebe8c5ab30b89499a70dd7727de41184))
    - Provide a way to record and apply index changes. ([`3ee8b62`](https://github.com/GitoxideLabs/gitoxide/commit/3ee8b62dd025d6fdb0d9929dec7a561fa576f545))
    - Add more modes for checking for unresolved conflicts. ([`aff76f2`](https://github.com/GitoxideLabs/gitoxide/commit/aff76f291a52fc6806944d72d249a8bd1b804c39))
    - When blob-merging, clarify if something would have conflicted. ([`09213bc`](https://github.com/GitoxideLabs/gitoxide/commit/09213bc1b2aa725af1571dff040415772e844c3a))
    - Merge pull request #1662 from paolobarbolini/thiserror-v2 ([`7a40648`](https://github.com/GitoxideLabs/gitoxide/commit/7a406481b072728cec089d7c05364f9dbba335a2))
    - Upgrade thiserror to v2.0.0 ([`0f0e4fe`](https://github.com/GitoxideLabs/gitoxide/commit/0f0e4fe121932a8a6302cf950b3caa4c8608fb61))
    - Merge pull request #1658 from GitoxideLabs/merge ([`905e5b4`](https://github.com/GitoxideLabs/gitoxide/commit/905e5b42a6163f92edef8fab82d97aeb6f17cf06))
    - Add `commit::virtual_merge_base()` to produce the single merge-base to use. ([`9d43b75`](https://github.com/GitoxideLabs/gitoxide/commit/9d43b753a225482645b22e4151bf7dc192c8c082))
    - Merge pull request #1654 from EliahKagan/doctest-workspace ([`1411289`](https://github.com/GitoxideLabs/gitoxide/commit/141128942c26bd63fc6855e5137b98f8da814446))
    - Fix code fences in gix-merge `ConflictStyle` and `Driver` ([`2fdbcfe`](https://github.com/GitoxideLabs/gitoxide/commit/2fdbcfe17cdcc480e320582d7c6b48f8b615bf3b))
    - Merge pull request #1651 from GitoxideLabs/merge ([`a876533`](https://github.com/GitoxideLabs/gitoxide/commit/a8765330fc16997dee275866b18a128dec1c5d55))
    - `Repository::merge_trees()` now has a fully-wrapped outcome. ([`1d2262f`](https://github.com/GitoxideLabs/gitoxide/commit/1d2262f2ca416e3c22f9601e7eab11f3372b2128))
    - Add `Conflict::is_unresolved()` as utility to see if multiple of them are considered unresolved. ([`9e106c4`](https://github.com/GitoxideLabs/gitoxide/commit/9e106c4ab7ceea091cd8baef99a480739bd53c9d))
    - Remove a TODO that turned out to be unnecessary. ([`5b428a9`](https://github.com/GitoxideLabs/gitoxide/commit/5b428a9e931b19622ae76c25bfb1fe882744cd1f))
    - Merge pull request #1652 from EliahKagan/run-ci/chmod ([`8e99eba`](https://github.com/GitoxideLabs/gitoxide/commit/8e99eba2a284b35b5e9bcb97e47bfbbafc3df5d1))
    - Update tree-baseline archive ([`ab45415`](https://github.com/GitoxideLabs/gitoxide/commit/ab45415a0119cbdcbd2fcbe8c7b6e2580432f705))
    - Set +x in index in added-file-changed-content-and-mode ([`6faf11a`](https://github.com/GitoxideLabs/gitoxide/commit/6faf11a0e0916fdf03e4532d28e40674c369bf9b))
    - Set +x in index in same-rename-different-mode baseline ([`041bdde`](https://github.com/GitoxideLabs/gitoxide/commit/041bddecacb23b4925bcd9bcdc4a86aec2ea719d))
    - Merge pull request #1618 from GitoxideLabs/merge ([`3fb989b`](https://github.com/GitoxideLabs/gitoxide/commit/3fb989be21c739bbfeac93953c1685e7c6cd2106))
    - Respect the `conflict-marker-size` attribute as well. ([`bd91d6a`](https://github.com/GitoxideLabs/gitoxide/commit/bd91d6ae97d1981a2366136040407590f64fdad4))
    - Add `tree()` and `commit()` merge support, en par with `merge-ORT` as far as tests go. ([`4b1764c`](https://github.com/GitoxideLabs/gitoxide/commit/4b1764ca9148e08ae9f11bca68e0689b12bc8c80))
    - Adjust blob-merge baseline to also test the reverse of each operation ([`de1cfb6`](https://github.com/GitoxideLabs/gitoxide/commit/de1cfb6a9caf5ac086c6411824835c75e888e2d7))
    - Add `blob::PlatformRef::id_by_pick()` to more efficiently pick merge results. ([`dd99991`](https://github.com/GitoxideLabs/gitoxide/commit/dd99991ec2bfb07ef571769abc32f1b35122d5ca))
    - Don't fail on big files during blob-merge, but turn them into binary merges. ([`c1cf08c`](https://github.com/GitoxideLabs/gitoxide/commit/c1cf08cc47f98abda017544b1791ab3b4463cc77))
    - Prefer to receive borrowed `gix_command::Context` when it's just passed on. ([`78a5355`](https://github.com/GitoxideLabs/gitoxide/commit/78a535572643a0657348aea3c2fed0123505f7fe))
    - Merge pull request #1642 from GitoxideLabs/new-release ([`db5c9cf`](https://github.com/GitoxideLabs/gitoxide/commit/db5c9cfce93713b4b3e249cff1f8cc1ef146f470))
</details>

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

 - 15 commits contributed to the release over the course of 26 calendar days.
 - 1 commit was understood as [conventional](https://www.conventionalcommits.org).
 - 0 issues like '(#ID)' were seen in commit messages

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **Uncategorized**
    - Release gix-merge v0.0.0, gix-negotiate v0.16.0, gix-pack v0.54.0, gix-odb v0.64.0, gix-packetline v0.18.0, gix-transport v0.43.0, gix-protocol v0.46.0, gix-revision v0.30.0, gix-refspec v0.26.0, gix-status v0.14.0, gix-submodule v0.15.0, gix-worktree-state v0.14.0, gix v0.67.0, gix-fsck v0.7.0, gitoxide-core v0.42.0, gitoxide v0.38.0 ([`f1364dc`](https://github.com/GitoxideLabs/gitoxide/commit/f1364dcb8aa66e3d8730e38445b045c5b63c56e6))
    - Add new changelog for gix-merge ([`fa3e260`](https://github.com/GitoxideLabs/gitoxide/commit/fa3e2600d7e39011f1d7f410249ebd0426a348a8))
    - Release gix-date v0.9.1, gix-utils v0.1.13, gix-actor v0.33.0, gix-hash v0.15.0, gix-trace v0.1.11, gix-features v0.39.0, gix-hashtable v0.6.0, gix-validate v0.9.1, gix-object v0.45.0, gix-path v0.10.12, gix-glob v0.17.0, gix-quote v0.4.13, gix-attributes v0.23.0, gix-command v0.3.10, gix-packetline-blocking v0.18.0, gix-filter v0.14.0, gix-fs v0.12.0, gix-chunk v0.4.9, gix-commitgraph v0.25.0, gix-revwalk v0.16.0, gix-traverse v0.42.0, gix-worktree-stream v0.16.0, gix-archive v0.16.0, gix-config-value v0.14.9, gix-tempfile v15.0.0, gix-lock v15.0.0, gix-ref v0.48.0, gix-sec v0.10.9, gix-config v0.41.0, gix-prompt v0.8.8, gix-url v0.28.0, gix-credentials v0.25.0, gix-ignore v0.12.0, gix-bitmap v0.2.12, gix-index v0.36.0, gix-worktree v0.37.0, gix-diff v0.47.0, gix-discover v0.36.0, gix-pathspec v0.8.0, gix-dir v0.9.0, gix-mailmap v0.25.0, gix-merge v0.0.0, gix-negotiate v0.16.0, gix-pack v0.54.0, gix-odb v0.64.0, gix-packetline v0.18.0, gix-transport v0.43.0, gix-protocol v0.46.0, gix-revision v0.30.0, gix-refspec v0.26.0, gix-status v0.14.0, gix-submodule v0.15.0, gix-worktree-state v0.14.0, gix v0.67.0, gix-fsck v0.7.0, gitoxide-core v0.42.0, gitoxide v0.38.0, safety bump 41 crates ([`3f7e8ee`](https://github.com/GitoxideLabs/gitoxide/commit/3f7e8ee2c5107aec009eada1a05af7941da9cb4d))
    - Merge pull request #1624 from EliahKagan/update-repo-url ([`795962b`](https://github.com/GitoxideLabs/gitoxide/commit/795962b107d86f58b1f7c75006da256d19cc80ad))
    - Update gitoxide repository URLs ([`64ff0a7`](https://github.com/GitoxideLabs/gitoxide/commit/64ff0a77062d35add1a2dd422bb61075647d1a36))
    - Merge pull request #1612 from Byron/merge ([`37c1e4c`](https://github.com/GitoxideLabs/gitoxide/commit/37c1e4c919382c9d213bd5ca299ed659d63ab45d))
    - Add some performance traces for blob-merges ([`b25fe4d`](https://github.com/GitoxideLabs/gitoxide/commit/b25fe4d052250ddace9a118b2247537b2a7fa09e))
    - Merge pull request #1611 from Byron/merge ([`5ffccd2`](https://github.com/GitoxideLabs/gitoxide/commit/5ffccd2f08d70576347e3ae17a66ca5a60f1d81c))
    - Make `blob::Platform::filter_mode` public. ([`b26eb26`](https://github.com/GitoxideLabs/gitoxide/commit/b26eb2618c1764f699530e251ddef4e6cf456ddd))
    - Merge pull request #1585 from Byron/merge ([`2261de4`](https://github.com/GitoxideLabs/gitoxide/commit/2261de470aeb77be080f9e423e1513bde85d9cc0))
    - Add platform tests and implementation ([`eb37dc3`](https://github.com/GitoxideLabs/gitoxide/commit/eb37dc36d8c42f5a7714c641244ce4a13111b0a1))
    - Add all relevant tests for the merge processing pipeline ([`a6f3e30`](https://github.com/GitoxideLabs/gitoxide/commit/a6f3e30017343c01ba61c49fe74ffc69e443a33c))
    - Implement `text` and `binary` merge algorithms, also with baseline tests for correctness. ([`0762846`](https://github.com/GitoxideLabs/gitoxide/commit/07628465a0a3f047ec809d287c9a4567b4acd607))
    - Sketch the entire API surface to capture all parts of blob-merges ([`9efa09f`](https://github.com/GitoxideLabs/gitoxide/commit/9efa09f10d042dc4d5db4edf4589594450a30b31))
    - Add the `gix-merge` crate for capturing merge algorithms ([`65efcb7`](https://github.com/GitoxideLabs/gitoxide/commit/65efcb7624031ae478056fbb49a07ef176f0c96b))
</details>

