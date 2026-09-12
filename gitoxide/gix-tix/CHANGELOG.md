

## Unreleased

### Chore

 - <csr-id-3e05ca352597ef5966fa4dc4f52456c2424cddad/> add package.include directives to control which files are packaged.
 - <csr-id-17835bccb066bbc47cc137e8ec5d9fe7d5665af0/> bump `rust-version` to 1.70
   That way clippy will allow to use the fantastic `Option::is_some_and()`
   and friends.
 - <csr-id-3bd09ef120945a9669321ea856db4079a5dab930/> change `rust-version` manifest field back to 1.65.
   They didn't actually need to be higher to work, and changing them
   unecessarily can break downstream CI.
   
   Let's keep this value as low as possible, and only increase it when
   more recent features are actually used.
 - <csr-id-aea89c3ad52f1a800abb620e9a4701bdf904ff7d/> upgrade MSRV to v1.70
   Our MSRV follows the one of `helix`, which in turn follows Firefox.

### New Features

 - <csr-id-21401427be1873e72dce7a802587571543e529e7/> add configurable terminal screen modes.
 - <csr-id-e8a9649f01f6d99d2fff5d11885d32b5eabb7727/> limit commit selection highlighting.
 - <csr-id-7118a844ff07f6be2396fa7b85943551a7ebc6d0/> mailmap support with 'm' toggle.
   Additionally, dim disabled toggles.
 - <csr-id-cdbe465db8828a412164873242db8214f5b43baa/> show bots and commit attributions in tix
   Highlight authors identified by the Codex and Claude email addresses, and expose recognized contribution trailers alongside the primary author. Group actors by trailer kind and keep trailer metadata enabled by default behind a dedicated toggle.
   
   The regression fixture covers bot authors, bot co-authors, mixed-case trailer tokens, all recognized attribution kinds, repeated kinds, and malformed actor values. UI coverage verifies grouping, colors, bracketed bot names, and both metadata toggles.
   
   Validated with:
   - GIX_TEST_IGNORE_ARCHIVES=1 cargo test -p gix-tix --features sha1
   - cargo check -p gix-tix --no-default-features --features sha256
   - cargo clippy -p gix-tix --all-targets --features sha1 --no-deps
   - cargo fmt --all -- --check
 - <csr-id-fed0051609450abf49c6aeebdfa36e191f4292c7/> hide revision ancestry in tix
 - <csr-id-6e8f282c0e79dad8f759af04cd82d861bf62cc0d/> keep tix available as a standalone binary
 - <csr-id-31a94aa8e268fb9e3442ce786788624938fce275/> add tix to the gix CLI
 - <csr-id-da73da10fd22581be22c890a130bfded6a781376/> use tig colors in tix
 - <csr-id-dcfe626bf13245aeb5bc3b5e82cbca187075cc87/> add horizontal paging to tix
 - <csr-id-f9482d5dadcdd5abd57a1d8ab445ce1cf04a98dc/> toggle special references in tix
 - <csr-id-e63ffb7fe68829f0534e8d3706e358cf70b6f53a/> show commit dates and author names in tix
 - <csr-id-74784783c2557450d48578cd7472fe5f9ad28e8d/> draw commit graph lanes in tix
 - <csr-id-1cec12b3192445c53e2fe7716d71c262139f764c/> add Vim-style page scrolling to tix
 - <csr-id-5deecf27c4f6fe2d7e6479f5ab3ea39251157109/> let tix quit when history loading completes
 - <csr-id-5704260ee85a6ede0326ebb18fbaf34dfd361a8b/> add the Ratatui tix binary
   Turn gix-tix into the installed tix executable and render its streaming history with Ratatui. The terminal loop accepts multiple revision tips, defaults to HEAD, keeps input responsive with bounded event draining, supports paging/tail navigation and cancellation, and copies full object IDs through OSC52.
   
   Use the latest Rust version requested for this binary. Ratatui TestBackend verifies row, decoration, selection, and footer rendering; key mapping and model/history tests cover the remaining behavior.
   
   Post-implementation Linux checkout observations: first paint and quit completed within 0.11s; the history walker visited 1,352,640 commits in 7.24s with 1,148,321,792 bytes maximum RSS. For reference, git rev-list --count took 0.61s and git log --oneline --decorate took 14.55s. These are observations only, with no optimization threshold.
   
   Validated with cargo test -p gix-tix, cargo clippy -p gix-tix --all-targets -- -D warnings, cargo check -p gix-tix, and cargo build --release -p gix-tix.

### Bug Fixes

 - <csr-id-b5e7a45a4b86ba00670ec3b1fdf8905a9eb6ad00/> better memory handling (use less)
 - <csr-id-3502897677d75a5900f88595c9e47f7a8c313cf3/> ignore broken references in tix decorations
 - <csr-id-ceabbbccf7850c89767ea3b41378f36ddf470967/> require explicit hash selection for gix-tix
 - <csr-id-77876668e5b3944d55cdd2d90a69382443eed3bf/> keep tix metadata visible on wide graphs

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

### Performance

 - <csr-id-40e105647b970e312dff92b677f3f77be14d2bdc/> render tix reactively at most 60 fps
 - <csr-id-48c925ad849911ac21cad2e569899e397cca4216/> intern author names in tix
 - <csr-id-def11571c40b3c94472e9fc88db46fa8dab3f611/> store commit titles in an arena
 - <csr-id-640af5d456b7b834365fc06328e62a80c6666edb/> speed up tix on million-commit histories
 - <csr-id-db9b2a1ce4a4c844c193fe2732c32e80c27610be/> avoid waiting and formatting hidden tix rows

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 49 commits contributed to the release.
 - 1071 days passed between releases.
 - 29 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 1 unique issue was worked on: [#325](https://github.com/GitoxideLabs/gitoxide/issues/325)

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **[#325](https://github.com/GitoxideLabs/gitoxide/issues/325)**
    - Add the tix history and application model ([`774f856`](https://github.com/GitoxideLabs/gitoxide/commit/774f856f32d626506deda2917a974191ec69f32e))
 * **Uncategorized**
    - Update changelogs prior to release ([`3e996df`](https://github.com/GitoxideLabs/gitoxide/commit/3e996dff31d52bc1338402f461a4985bb2f71b9e))
    - Release gix-trace v0.1.21, gix-validate v0.11.3, gix-path v0.12.3, gix-utils v0.3.5, gix-config-value v0.19.0, gix-prompt v0.16.0, gix-sec v0.14.2, gix-url v0.37.0, gix-credentials v0.39.0, safety bump 18 crates ([`f0ec710`](https://github.com/GitoxideLabs/gitoxide/commit/f0ec71076aa1cef3181b77946ee556a89c651b8e))
    - Add configurable terminal screen modes. ([`2140142`](https://github.com/GitoxideLabs/gitoxide/commit/21401427be1873e72dce7a802587571543e529e7))
    - Limit commit selection highlighting. ([`e8a9649`](https://github.com/GitoxideLabs/gitoxide/commit/e8a9649f01f6d99d2fff5d11885d32b5eabb7727))
    - Better memory handling (use less) ([`b5e7a45`](https://github.com/GitoxideLabs/gitoxide/commit/b5e7a45a4b86ba00670ec3b1fdf8905a9eb6ad00))
    - Mailmap support with 'm' toggle. ([`7118a84`](https://github.com/GitoxideLabs/gitoxide/commit/7118a844ff07f6be2396fa7b85943551a7ebc6d0))
    - Merge pull request #2813 from GitoxideLabs/tix-authors ([`dbe7bb6`](https://github.com/GitoxideLabs/gitoxide/commit/dbe7bb6ff0ac53a3056f81cc181411d0d3bfbc3f))
    - Show bots and commit attributions in tix ([`cdbe465`](https://github.com/GitoxideLabs/gitoxide/commit/cdbe465db8828a412164873242db8214f5b43baa))
    - Merge pull request #2809 from GitoxideLabs/gix-tix-mvp ([`443b401`](https://github.com/GitoxideLabs/gitoxide/commit/443b401730e91503666192f502556f334049fbc0))
    - Explain `gix tix` and that it's an experiment ([`b9c0c80`](https://github.com/GitoxideLabs/gitoxide/commit/b9c0c80976da1ec5dd36c7f385d45146eb068947))
    - Ignore broken references in tix decorations ([`3502897`](https://github.com/GitoxideLabs/gitoxide/commit/3502897677d75a5900f88595c9e47f7a8c313cf3))
    - Require explicit hash selection for gix-tix ([`ceabbbc`](https://github.com/GitoxideLabs/gitoxide/commit/ceabbbccf7850c89767ea3b41378f36ddf470967))
    - Hide revision ancestry in tix ([`fed0051`](https://github.com/GitoxideLabs/gitoxide/commit/fed0051609450abf49c6aeebdfa36e191f4292c7))
    - Render tix reactively at most 60 fps ([`40e1056`](https://github.com/GitoxideLabs/gitoxide/commit/40e105647b970e312dff92b677f3f77be14d2bdc))
    - Intern author names in tix ([`48c925a`](https://github.com/GitoxideLabs/gitoxide/commit/48c925ad849911ac21cad2e569899e397cca4216))
    - Keep tix available as a standalone binary ([`6e8f282`](https://github.com/GitoxideLabs/gitoxide/commit/6e8f282c0e79dad8f759af04cd82d861bf62cc0d))
    - Store commit titles in an arena ([`def1157`](https://github.com/GitoxideLabs/gitoxide/commit/def11571c40b3c94472e9fc88db46fa8dab3f611))
    - Add tix to the gix CLI ([`31a94aa`](https://github.com/GitoxideLabs/gitoxide/commit/31a94aa8e268fb9e3442ce786788624938fce275))
    - Use tig colors in tix ([`da73da1`](https://github.com/GitoxideLabs/gitoxide/commit/da73da10fd22581be22c890a130bfded6a781376))
    - Add horizontal paging to tix ([`dcfe626`](https://github.com/GitoxideLabs/gitoxide/commit/dcfe626bf13245aeb5bc3b5e82cbca187075cc87))
    - Toggle special references in tix ([`f9482d5`](https://github.com/GitoxideLabs/gitoxide/commit/f9482d5dadcdd5abd57a1d8ab445ce1cf04a98dc))
    - Keep tix metadata visible on wide graphs ([`7787666`](https://github.com/GitoxideLabs/gitoxide/commit/77876668e5b3944d55cdd2d90a69382443eed3bf))
    - Show commit dates and author names in tix ([`e63ffb7`](https://github.com/GitoxideLabs/gitoxide/commit/e63ffb7fe68829f0534e8d3706e358cf70b6f53a))
    - Speed up tix on million-commit histories ([`640af5d`](https://github.com/GitoxideLabs/gitoxide/commit/640af5d456b7b834365fc06328e62a80c6666edb))
    - Avoid waiting and formatting hidden tix rows ([`db9b2a1`](https://github.com/GitoxideLabs/gitoxide/commit/db9b2a1ce4a4c844c193fe2732c32e80c27610be))
    - Draw commit graph lanes in tix ([`7478478`](https://github.com/GitoxideLabs/gitoxide/commit/74784783c2557450d48578cd7472fe5f9ad28e8d))
    - Add Vim-style page scrolling to tix ([`1cec12b`](https://github.com/GitoxideLabs/gitoxide/commit/1cec12b3192445c53e2fe7716d71c262139f764c))
    - Let tix quit when history loading completes ([`5deecf2`](https://github.com/GitoxideLabs/gitoxide/commit/5deecf27c4f6fe2d7e6479f5ab3ea39251157109))
    - Apply repository formatting to tix ([`d928f86`](https://github.com/GitoxideLabs/gitoxide/commit/d928f8673784ac0b756680ed27bbbd7830b6f60e))
    - Add the Ratatui tix binary ([`5704260`](https://github.com/GitoxideLabs/gitoxide/commit/5704260ee85a6ede0326ebb18fbaf34dfd361a8b))
    - Merge pull request #2568 from GitoxideLabs/dependabot/cargo/cargo-56d6b174d8 ([`ab2fee1`](https://github.com/GitoxideLabs/gitoxide/commit/ab2fee14651202fcb7b3d8178932090c73492014))
    - Update crates to Rust 2024 edition ([`2cb17b2`](https://github.com/GitoxideLabs/gitoxide/commit/2cb17b2e7f6009693a55af907614f705a29d8c29))
    - Remove rust_2018_idioms lint declarations ([`e10d5f6`](https://github.com/GitoxideLabs/gitoxide/commit/e10d5f662df2ee05f973a3167ad215a330ee74e1))
    - Raise MSRV for hash dependency updates ([`3675a8d`](https://github.com/GitoxideLabs/gitoxide/commit/3675a8d61b17845a783bc27912a3f52ac273a4af))
    - Merge pull request #2518 from GitoxideLabs/improvements ([`444a92b`](https://github.com/GitoxideLabs/gitoxide/commit/444a92b0fa1df406cf2f36f8dbe82c2859e04e0b))
    - Add package.include directives to control which files are packaged. ([`3e05ca3`](https://github.com/GitoxideLabs/gitoxide/commit/3e05ca352597ef5966fa4dc4f52456c2424cddad))
    - Merge pull request #2217 from GitoxideLabs/copilot/update-msrv-to-rust-1-82 ([`4da2927`](https://github.com/GitoxideLabs/gitoxide/commit/4da2927629c7ec95b96d62a387c61097e3fc71fa))
    - Update MSRV to 1.82 and replace once_cell with std equivalents ([`6cc8464`](https://github.com/GitoxideLabs/gitoxide/commit/6cc84641cb7be6f70468a90efaafcf142a6b8c4b))
    - Merge pull request #1762 from GitoxideLabs/fix-1759 ([`7ec21bb`](https://github.com/GitoxideLabs/gitoxide/commit/7ec21bb96ce05b29dde74b2efdf22b6e43189aab))
    - Bump `rust-version` to 1.70 ([`17835bc`](https://github.com/GitoxideLabs/gitoxide/commit/17835bccb066bbc47cc137e8ec5d9fe7d5665af0))
    - Merge pull request #1624 from EliahKagan/update-repo-url ([`795962b`](https://github.com/GitoxideLabs/gitoxide/commit/795962b107d86f58b1f7c75006da256d19cc80ad))
    - Update gitoxide repository URLs ([`64ff0a7`](https://github.com/GitoxideLabs/gitoxide/commit/64ff0a77062d35add1a2dd422bb61075647d1a36))
    - Merge branch 'global-lints' ([`37ba461`](https://github.com/GitoxideLabs/gitoxide/commit/37ba4619396974ec9cc41d1e882ac5efaf3816db))
    - Workspace Clippy lint management ([`2e0ce50`](https://github.com/GitoxideLabs/gitoxide/commit/2e0ce506968c112b215ca0056bd2742e7235df48))
    - Merge branch 'msrv' ([`8c492d7`](https://github.com/GitoxideLabs/gitoxide/commit/8c492d7b7e6e5d520b1e3ffeb489eeb88266aa75))
    - Change `rust-version` manifest field back to 1.65. ([`3bd09ef`](https://github.com/GitoxideLabs/gitoxide/commit/3bd09ef120945a9669321ea856db4079a5dab930))
    - Merge branch 'maintenance' ([`4454c9d`](https://github.com/GitoxideLabs/gitoxide/commit/4454c9d66c32a1de75a66639016c73edbda3bd34))
    - Upgrade MSRV to v1.70 ([`aea89c3`](https://github.com/GitoxideLabs/gitoxide/commit/aea89c3ad52f1a800abb620e9a4701bdf904ff7d))
</details>

## v0.0.0 (2023-08-17)

### Chore

 - <csr-id-f7f136dbe4f86e7dee1d54835c420ec07c96cd78/> uniformize deny attributes
 - <csr-id-533e887e80c5f7ede8392884562e1c5ba56fb9a8/> remove default link to cargo doc everywhere

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

### Commit Statistics

<csr-read-only-do-not-edit/>

 - 23 commits contributed to the release.
 - 3 commits were understood as [conventional](https://www.conventionalcommits.org).
 - 2 unique issues were worked on: [#325](https://github.com/GitoxideLabs/gitoxide/issues/325), [#691](https://github.com/GitoxideLabs/gitoxide/issues/691)

### Commit Details

<csr-read-only-do-not-edit/>

<details><summary>view details</summary>

 * **[#325](https://github.com/GitoxideLabs/gitoxide/issues/325)**
    - Update changelog ([`7882203`](https://github.com/GitoxideLabs/gitoxide/commit/7882203f558c98b18a381ec763ff1242c396046e))
    - Empty crate for 'tix' tool ([`2efed08`](https://github.com/GitoxideLabs/gitoxide/commit/2efed088c572380a152f75dc7200f13fe3b055ad))
 * **[#691](https://github.com/GitoxideLabs/gitoxide/issues/691)**
    - Set `rust-version` to 1.64 ([`55066ce`](https://github.com/GitoxideLabs/gitoxide/commit/55066ce5fd71209abb5d84da2998b903504584bb))
 * **Uncategorized**
    - Release gix-tix v0.0.0, gix-note v0.0.0, gix-lfs v0.0.0, gix-fetchhead v0.0.0, gix-sequencer v0.0.0, gix-rebase v0.0.0 ([`0199927`](https://github.com/GitoxideLabs/gitoxide/commit/019992765d3cfc2627cf57e82771c006726c8fbc))
    - Update license field following SPDX 2.1 license expression standard ([`9064ea3`](https://github.com/GitoxideLabs/gitoxide/commit/9064ea31fae4dc59a56bdd3a06c0ddc990ee689e))
    - Merge branch 'corpus' ([`aa16c8c`](https://github.com/GitoxideLabs/gitoxide/commit/aa16c8ce91452a3e3063cf1cf0240b6014c4743f))
    - Change MSRV to 1.65 ([`4f635fc`](https://github.com/GitoxideLabs/gitoxide/commit/4f635fc4429350bae2582d25de86429969d28f30))
    - Merge branch 'main' into auto-clippy ([`3ef5c90`](https://github.com/GitoxideLabs/gitoxide/commit/3ef5c90aebce23385815f1df674c1d28d58b4b0d))
    - Merge branch 'blinxen/main' ([`9375cd7`](https://github.com/GitoxideLabs/gitoxide/commit/9375cd75b01aa22a0e2eed6305fe45fabfd6c1ac))
    - Include license files in all crates ([`facaaf6`](https://github.com/GitoxideLabs/gitoxide/commit/facaaf633f01c857dcf2572c6dbe0a92b7105c1c))
    - Merge branch 'rename-crates' into inform-about-gix-rename ([`c9275b9`](https://github.com/GitoxideLabs/gitoxide/commit/c9275b99ea43949306d93775d9d78c98fb86cfb1))
    - Adjust to renaming of `git-tix` to `gix-tix` ([`531003b`](https://github.com/GitoxideLabs/gitoxide/commit/531003bb03dd83e8870643bbf114008a367c6599))
    - Rename `git-tix` to `gix-tix` ([`5cd02dc`](https://github.com/GitoxideLabs/gitoxide/commit/5cd02dcc56eda79888f1d1344031744457fc04fa))
    - Merge branch 'main' into http-config ([`bcd9654`](https://github.com/GitoxideLabs/gitoxide/commit/bcd9654e56169799eb706646da6ee1f4ef2021a9))
    - Merge branch 'version2021' ([`0e4462d`](https://github.com/GitoxideLabs/gitoxide/commit/0e4462df7a5166fe85c23a779462cdca8ee013e8))
    - Upgrade edition to 2021 in most crates. ([`3d8fa8f`](https://github.com/GitoxideLabs/gitoxide/commit/3d8fa8fef9800b1576beab8a5bc39b821157a5ed))
    - Merge branch 'main' into index-from-tree ([`bc64b96`](https://github.com/GitoxideLabs/gitoxide/commit/bc64b96a2ec781c72d1d4daad38aa7fb8b74f99b))
    - Merge branch 'main' into remote-ls-refs ([`e2ee3de`](https://github.com/GitoxideLabs/gitoxide/commit/e2ee3ded97e5c449933712883535b30d151c7c78))
    - Merge branch 'docsrs-show-features' ([`31c2351`](https://github.com/GitoxideLabs/gitoxide/commit/31c235140cad212d16a56195763fbddd971d87ce))
    - Uniformize deny attributes ([`f7f136d`](https://github.com/GitoxideLabs/gitoxide/commit/f7f136dbe4f86e7dee1d54835c420ec07c96cd78))
    - Remove default link to cargo doc everywhere ([`533e887`](https://github.com/GitoxideLabs/gitoxide/commit/533e887e80c5f7ede8392884562e1c5ba56fb9a8))
    - Merge branch 'main' into repo-status ([`4086335`](https://github.com/GitoxideLabs/gitoxide/commit/40863353a739ec971b49410fbc2ba048b2762732))
    - Release git-tix v0.0.0 ([`31d1882`](https://github.com/GitoxideLabs/gitoxide/commit/31d18826514c65e281f13986123df7c58b3f88b4))
</details>

