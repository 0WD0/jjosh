# jjosh

**把多个代码仓库放在一起开发，再把修改送回各自的仓库。**

jjosh 将 [Jujutsu（jj）](https://www.jj-vcs.dev/) 的修改管理方式与 [Josh](https://github.com/josh-project/josh) 的 Git 历史转换、仓库组合能力集成在一个命令行工具里：开发时使用统一的工作区，上游仍然可以保持独立的仓库。

当前 link 工作流依赖 Josh 的实验性功能，需要显式启用。建议先在临时仓库和自己的发布分支上试用。

## 为什么需要它？

一个功能可能同时涉及应用、公共库和底层工具，而它们分别属于不同的仓库。你希望一起修改、测试和整理提交，但不想为了这次开发永久合并仓库。

jjosh 可以把它们组合成这样的工作区：

```text
workspace/
├── app/       ← 应用仓库
├── library/   ← 公共库仓库
└── tools/     ← 工具仓库
```

这些目录中的代码可以直接编辑。你用 jj 的命令管理本地修改，用 `link update` 获取来源仓库的新版本，再用 `link push` 分别导出对应目录的修改。

**一起开发，不等于一起发布。** 各个仓库仍然分别推送，不提供跨仓库的原子发布。

## 不认识 jj 和 Josh 也没关系

### jj：管理修改和版本历史

版本管理工具负责记录代码如何变化，支持回看历史、协作和恢复误操作。jj 是一个兼容 Git 仓库的版本管理工具，但不要求照搬 Git 的操作方式：

- 工作区本身对应一个持续更新的提交；通常在执行命令时记录文件变化，不需要先 `git add`。
- 可以拆分、合并和重新排列修改；改动较早的提交时，后续提交会自动迁移到新的基础上。
- 操作日志记录本地版本管理操作，可以用 `undo` 撤销误操作。它不代表能够撤销已经推送到远端的内容。

jjosh 保留 jj 的常规命令，例如 `status`、`diff`、`log`、`new`、`split`、`squash` 和 `rebase`，另外增加 `link`、`projection` 与 `native` 命令。

### Josh：调整仓库的内容和历史视图

Josh 可以把一个 Git 仓库转换成另一个视图。例如，把大仓库里的 `services/api/` 提取出来，作为根目录展示，并保留与它相关的历史。

这不是简单复制文件或隐藏目录，而是转换 Git 历史。jjosh 的 link 工作流借助这些能力，将外部仓库的内容和历史组合进指定路径，并将适用的本地修改映射回来源结构。

jj 管原生历史与工作副本，包括本地路径布局；Josh 管跨仓库的历史投影。jjosh 将两者放进同一个开发流程，但原生 sparse 不依赖 Josh。

## 原生 sparse：选择与映射工作副本

本地 jj 集成接续了 pmandloi28 的 [fileset sparse PR #9760](https://github.com/jj-vcs/jj/pull/9760)，
并参考 [Sparse Patterns V2](https://github.com/jj-vcs/jj/blob/main/docs/design/sparse-v2.md) 的配置对象与双坐标模型、[PR #9996](https://github.com/jj-vcs/jj/pull/9996) 的有序规则设计。
这不表示这些提案已在上游实现或合并。

```sh
jjosh sparse set 'glob:"**/*.rs" ~ src/generated'
jjosh sparse set --add 'src/generated/needed.rs'
jjosh sparse list
jjosh sparse edit
jjosh sparse reset
```

选择规则按顺序保存，包含取并集、排除取差集；每条规则仍可使用复合 fileset。位置参数替换选择规则，保留映射；同一次调用先应用 `--add`，再应用 `--remove`。持久化的是结构化规则和映射，不是 DSL 字符串；别名在输入时展开。`sparse list` 和 `sparse edit` 使用根相对的 `+ FILESET` / `- FILESET` 行，以及 `map ["源路径","目标路径"]` 行。编辑器内容始终使用 canonical repository 坐标；空内容表示空选择和 identity 映射。

### 应用布局与共享源

假设仓库已经包含下列 canonical 源目录，可以让 Sunshine 在工作副本根构建，同时使用共享依赖：

```sh
jjosh sparse set root:src/Sunshine root:src/moonlight-common-c root:src/msquic
jjosh sparse set \
  --remove root:src/Sunshine/third-party/moonlight-common-c \
  --remove root:src/Sunshine/third-party/msquic \
  --remove root-file:src/Sunshine/third-party
jjosh sparse map set \
  src/Sunshine=. \
  src/moonlight-common-c=third-party/moonlight-common-c \
  src/msquic=third-party/msquic
```

源路径相对 canonical repository 根，目标路径相对物理工作副本根。`map set` 一次替换全部映射；没有显式映射时才使用 identity 映射，显式映射之外的源不会检出。`map reset` 只恢复 identity 映射；`sparse reset` 同时恢复全部选择和 identity 映射。这些命令不导入或迁移现有源仓库。

在 `third-party/msquic/` 修改或新建文件，普通 snapshot 就写回 `src/msquic/`。只有一份完整的 canonical 原生树，不生成投影提交，也不经过 Josh 或 projection push/unapply。未检出的内容和原生冲突仍在完整提交中；revision 操作不会被限制为可见文件。

映射必须有唯一写入归属，包括尚未创建的文件。重复可写别名、目标覆盖以及文件占用另一映射的父目录都会被拒绝。上例最后一条排除只排除 canonical 路径 `src/Sunshine/third-party` 本身作为文件的可能性，不排除它的整个子树。无法证明安全的重叠 glob 域也会拒绝，而不是按当前树猜测。编辑器中的 `map-file ["源","目标"]` 可表达精确文件映射。

普通路径参数按物理工作副本和当前目录解释；`root:`、`root-file:`、`root-glob:` 始终指向 canonical repository 路径。跨多个映射边界的物理前缀和物理 glob 不会被近似转换：命令会要求使用明确的 canonical fileset。`--remove` 是排除匹配路径，不是删除一个旧前缀条目。

### 操作历史、工作副本与兼容性

选择与映射作为一个不可变配置对象由 operation View 引用，可以一起 undo/redo/restore。首次实际更改会先记录本地旧配置的基线；更早没有记录的历史保持未知，不拿今天的配置补造历史。并发配置冲突保留全部备选并维持实际布局，使用 `sparse edit` 或 `sparse reset` 明确解决。

`--ignore-working-copy` 可修改已登记的期望配置而不改磁盘；下一次正常命令先按实际旧布局记录本地修改，再应用新配置。`--at-op=@` 仍然可写。各 workspace 共享仓库，但有各自的提交指针、配置和磁盘文件；`workspace add --sparse-patterns=copy` 复制完整配置，`full` / `empty` 使用 identity 映射。其他 workspace 不会实时同步磁盘。

非 identity 映射不能用于 colocated Git 工作树；普通 identity sparse 仍可 colocate，也可与同仓库中的映射工作副本并存。

旧前缀和 `local-fileset` 本地状态可迁移。新建工作副本以及本版本首次写入本地状态后的工作副本使用 `local-working-copy-mapped`；首次记录版本化配置时，标准 operation store 升级为 `simple_op_store_working_copy_patterns`。旧二进制加载时会拒绝，reset 不会降级。切换前退出旧 jj 进程，不要手改类型标记；已运行的旧进程不受加载时检查保护。前一实验版的 `simple_op_store_sparse` 字符串操作库会被明确拒绝，不作为新对象 ID 格式读取。

实际布局更新中断后，`sparse-update-in-progress` 会阻止继续 snapshot，避免把未完成检出误当成用户删除。它不是自动回滚机制：保留现有文件，从健康 workspace 创建替代工作副本，再核对未记录的本地修改；不要删除标记后强行 snapshot。恢复步骤见 [filesets 文档](jj/docs/filesets.md#interrupted-materialization)。

## 从源码构建

需要：

- Rust 1.95 或更新版本，以及 Cargo。
- 系统编译工具链；`git` 命令需要在 `PATH` 中。
- 完整的源码目录，包括 `jj/` 和 `josh/`，它们是本项目的本地依赖。

当前 link / projection 集成测试面向 Unix；本地传输使用 `env` 命令，不应据此假定原生 Windows 已受支持。

开发时，在本仓库根目录执行：

```sh
cargo build -p jjosh-cli
cargo run -p jjosh-cli -- --help
```

普通 `cargo build` / `cargo test` 会优先复用已有锁定版本，在依赖声明变化时自动同步根目录 `Cargo.lock`，无需手改锁文件或额外执行 `cargo update`。`jj/` 和 `josh/` 自己的锁文件不替代根工作区的锁文件；将根锁文件的变化保留在本地集成提交中。

更新内嵌来源后，正常构建即可同时完成依赖同步和二进制更新：

```sh
jjosh link update
cargo build --release -p jjosh-cli
```

`link update` 只更新来源和提交图，不隐式运行项目的构建脚本。CI 或需要验证已提交锁文件的构建再加 `--locked`：

```sh
cargo test --locked -p jjosh-cli --tests
```

`--locked` 的含义是禁止调整锁文件，依赖声明与锁文件不一致时失败；不是开发时自动同步依赖的选项。

安装到 Cargo 的可执行文件目录：

```sh
cargo install --locked --path crates/jjosh-cli
jjosh --help
```

确保 Cargo 的可执行文件目录在 `PATH` 中，默认是 `~/.cargo/bin`。不需要额外安装独立的 `jj` 或 `josh` 可执行文件，也不需要为下面的流程启动 Josh 代理服务。

## 快速开始：引入、修改、同步、发布

下面的 URL 是占位示例：将来源替换为实际仓库，将发布地址替换为你有写入权限的仓库或 fork。示例假设来源分支叫 `main`。

### 1. 创建工作区

```sh
export JOSH_EXPERIMENTAL_FEATURES=1
jjosh git init --object-hash sha1 workspace
cd workspace
jjosh config set --repo user.name "Your Name"
jjosh config set --repo user.email "you@example.com"
```

`JOSH_EXPERIMENTAL_FEATURES=1` 需要在后续执行 link 命令的 shell 中保持启用。

### 2. 把外部仓库放进一个目录

```sh
jjosh link add library https://github.com/ORG/library.git \
  --target main \
  --push-url https://github.com/YOU/library.git \
  --push-target jjosh/demo
```

这会将来源仓库放到 `library/`，并在 `library/.link.josh` 中记录来源、固定版本、挂载规则和发布配置。

link 的元数据与提交图由 jjosh 管理，Josh 负责通用过滤与反向历史映射。工作区保留普通目录和 `.link.josh` 文件，不依赖上游已删除的 `josh-link` 或旧 `Embed` 操作，也不要求把目录换成 submodule。

- `--target main`：后续从来源仓库的 `main` 分支获取更新。
- `--push-url`：单独指定发布仓库；不指定就不能使用 `link push` 发布这个 link。
- `--push-target jjosh/demo`：将修改发布到独立分支，而不是直接改来源主分支。
- 默认 `--mode embedded`：引入挂载路径下的来源历史，适合持续开发。也支持面向源码快照引入的 `--mode snapshot`。

重复 `link add`，选择不同目录，即可引入其他仓库。若只需要来源的一部分，可以在 URL 后添加 Josh 过滤表达式，例如 `':/src'`：来源的 `src/file.rs` 将映射到挂载目录中的 `file.rs`。

### 3. 修改代码

```sh
jjosh new -m "Update library behavior"
# 用编辑器修改 library/ 下的文件。
jjosh status
jjosh diff
jjosh log
```

`@` 表示当前工作区对应的提交。你可以继续用 `jjosh split`、`jjosh squash` 等命令整理修改；不熟悉 jj 时，可以先从 [jj 教程](https://docs.jj-vcs.dev/latest/tutorial/) 开始。

### 4. 获取上游更新

```sh
jjosh link update library
# 不指定目录，则更新当前版本中的全部 links：
jjosh link update
```

jjosh 用本地书签 `jjosh/trunk` 标记不含本地补丁的组合基线，用 `jjosh/source/<encoded-path>` 标记各挂载路径的来源投影。例如 `library/` 对应 `jjosh/source/library`，`vendor/library/` 对应 `jjosh/source/vendor%2Flibrary`。这些书签与提交图一起由 jj operation 管理。本地修改位于基线上方；更新时，基线前进，本地修改自动迁移到新的基础上。

这不意味着自动消除所有冲突。若上游和本地修改冲突，需要先解决冲突，再发布。

`jjosh op restore` 可以恢复本地提交、组合基线、来源书签及版本化 pin；colocated 和非 colocated 工作区都支持恢复后继续执行 `git import` 与 `link update`。远端已发布的内容、发布 lease 和 Git 配置不随 operation 回滚。

如果工作区仍使用旧的 `trunk@jjosh` / `<branch>@link-<encoded-path>` 远端标记，或更早的 Embed 历史，先显式迁移：

```sh
jjosh link migrate
```

已有干净 native 基线时，迁移保留提交图、工作区内容与 `.link.josh`，将标记转成本地书签，并移除 jjosh 拥有的旧合成远端；普通远端和发布 lease 保留。更早的 Embed 图会重建干净原生基线，并将本地修改重新接回。迁移会更新仓库配置，Git 侧清理不属于 operation 的原子回滚范围；完成后再执行 `link add` 或 `link update`。

### 5. 发布这个目录的修改

```sh
jjosh link push library --dry-run
jjosh link push library
```

这两条命令默认导出当前提交 `@` 中该 link 的内容，并映射回来源仓库的目录结构。上例会推送到你配置的发布仓库的 `jjosh/demo` 分支，不会将整个组合工作区推过去。

发布历史按目标 link 的实际文件变化裁剪：原本就为空的提交、只修改其他目录的提交，以及过滤后变空的提交，都不会仅因为带有描述而被保留。裁剪覆盖整段新增导出历史，不限于末端 `@`，也不会因此拒绝推送。空分支造成的重复或祖先父边会折叠；仍连接不同有效分支的合并关系会保留。本地 jj 工作区、提交历史和既有上游历史不变。

若要明确选择一个提交，使用 `-r <revision>`；它只选择导出历史的终点，不绕过内容裁剪，因此显式 `-r @` 与默认选择遵循同一规则。若要覆盖默认发布分支，使用 `--to <branch>`。预检与实际推送应使用相同的选择参数。

`--dry-run` 会检查导出和远端更新是否可行，但不更新远端，也不预留分支；实际推送仍可能因远端随后发生变化而被拒绝。

## 投影视图：局部开发与双向发布

`projection` 将一个仓库转换成可独立开发的历史视图，不必先将它挂载到组合工作区的子目录。支持 colocated 和非 colocated 的 jj 工作副本：

```sh
jjosh git init --no-colocate api-view
cd api-view
jjosh config set --repo user.name "Your Name"
jjosh config set --repo user.email "you@example.com"
jjosh projection remote add api https://github.com/ORG/monorepo.git ':/services/api'
jjosh projection fetch --remote api
jjosh new 'main@api' -m "Adjust API behavior"
# 修改视图根目录中的代码。
jjosh bookmark set publish -r @
jjosh projection push --remote api --to main -r publish
```

`push` 必须显式给出 `-r` 和目标分支 `--to`。`-r` 接受原生 jj revision、bookmark 或 `@`，精确发布所选提交，不猜测空工作提交是否应该被跳过。重复发布同一版本不会因为工作副本后来多了空提交而改变选择；显式选择空提交则仍会发布它。

Josh 负责历史过滤与反向写回；jj 负责工作副本快照、引用导入和本地事务。`fetch` 不替你选择开发基线，但正常的 jj 导入可能重写本地后继或创建新的工作提交。`push` 不在发布后自动 fetch、checkout 或 rebase。配置的投影 remote 禁止普通 `git push` 绕过反向过滤。

预览只读取已记录的原生状态，不快照工作文件、不更新引用：

```sh
jjosh projection status ':/src' -r @
```

### 版本化视图与共享依赖

jjosh 将这种布局称为“投影视图”（view），通过 `--view` 选择。`jjosh workspace` 仍是 jj 原生的工作副本管理命令。底层继续使用 Josh 的 `workspace.josh` 文件和 `:workspace=...` 过滤语法，便于与独立 Josh 工具互操作；不创建另一套映射格式。

例如，monorepo 只保存一份共享依赖：

```text
src/
├── Sunshine/
├── Artemis/
├── moonlight-common-c/
└── msquic/
```

`src/Sunshine/workspace.josh`：

```text
third-party/moonlight-common-c = :/src/moonlight-common-c
third-party/msquic = :/src/msquic
```

`src/Artemis/workspace.josh`：

```text
app/src/main/jni/moonlight-core/moonlight-common-c = :/src/moonlight-common-c
third-party/msquic = :/src/msquic
```

然后分别取得它们的视图：

```sh
jjosh git init --no-colocate sunshine-view
jjosh -R sunshine-view projection remote add mono /path/to/monorepo.git --view src/Sunshine
jjosh -R sunshine-view projection fetch --remote mono
jjosh -R sunshine-view new main@mono

jjosh git init --colocate artemis-view
jjosh -R artemis-view projection remote add mono /path/to/monorepo.git --view src/Artemis
jjosh -R artemis-view projection fetch --remote mono
jjosh -R artemis-view new main@mono
```

`--view` 是源仓库根目录下的相对路径，与位置参数 FILTER 二选一；不是当前磁盘上的工作副本路径，也不要求该目录已经存在。可用 `projection status --view src/Sunshine -r REV` 预览包含该定义的原始版本。

在 Sunshine 视图修改 `third-party/moonlight-common-c/`，反向写回后修改落到 `src/moonlight-common-c/`。Artemis 获取同一 monorepo 版本后，在自己的 JNI 路径看到该修改。两边使用普通文件，不是 gitlink、符号链接或实时共享的工作目录。

映射本身也是版本历史的一部分：

- 添加映射后，push 再 fetch 会补齐新可见的共享内容。Josh 可以为投影增加合成 merge，接入库的既有历史；源历史不必因此产生 merge。
- 删除映射但保留视图内的文件，会将它们变成应用自己的独立副本；不会删除原来的共享源。
- 同时删除映射和对应文件，只移除这个视图中的路径，其他视图仍可继续使用共享源。
- 将已有本地目录映射到新的共享位置，可以发布它的内容；改变视图内的路径时，同时修改映射和移动文件。

不要在原始项目路径保留与映射重叠的 gitlink 或重复依赖文件，再假定映射会自动替换它们。迁移这些内容、调整 submodule 初始化脚本，是独立的显式步骤。依赖嵌套布局出现在投影视图里，不会自动填进原始 monorepo 工作副本；要在视图中构建，或另行配置直接构建时的依赖路径。投影也不替代依赖版本选择、ABI 检查或预编译库重建。

### 接回经过历史转换的已发布版本

改变映射后，Josh 可能规范化 `workspace.josh`、补入文件并改变父关系；返回的提交可以保留 change ID，但拥有不同的 commit ID。这不是“原 DAG 必须不变”的迁移操作。

先检查返回的版本，再用 jj 接续开发：

```sh
jjosh projection fetch --remote mono
jjosh diff --from <published-commit-id> --to main@mono
# 没有尚待迁移的本地后继时，从返回的版本开始下一项修改：
jjosh new main@mono
# 若还有未发布后继，则迁移那些后继，而非再次重放已发布的映射提交：
jjosh rebase -s <first-unpublished-commit> -d main@mono
```

核对后，可以用 `jj abandon <old-published-commit-id>` 退役旧的本地表示；先处理它的未发布后继。不要盲目把已发布的映射提交本身 rebase 到返回的新版本上：定义文件被规范化后，重复应用它可能产生真实的文本冲突。jjosh 不自动放弃本地提交，也不隐藏 divergence 或冲突。

### 将局部历史迁入另一个已有仓库

在已有投影视图中，为接收仓库配置目标布局并先获取基线：

```sh
jjosh projection remote add destination /path/to/existing.git ':/vendor/api'
jjosh projection fetch --remote destination
jjosh projection push --remote destination --to imported -r publish --base main --merge
```

`--base` 指定接收仓库用于反向转换的源分支；`--merge` 在该基线上创建合并，保留接收仓库历史。这些操作直接使用 Josh 的反向过滤，不建立 split/rejoin 协议。核对迁入结果后，原仓库删除迁出目录是另一个普通提交，不与远端发布组成原子事务。

### 投影边界

- 当前只投影分支引用，不自动导入 tag；原始获取与公开投影引用隔离。成功 fetch 后，来源删除分支或更换为空视图会移除相应的投影分支，不清理其他 remote 或本地 tag。fetch 要求来源通过 `HEAD` 广告默认分支，尚不支持完全无分支的来源仓库。
- 原生未解决冲突尚不能通过此 Git 历史投影通道保真转换。预览和发布拒绝包含冲突的所选祖先历史；fetch 在公开投影引用前检查源历史的 `jj:trees`。干净的后继提交不能掩盖仍含原生冲突的祖先。下面的 `native` 工作流直接处理 jj 原生提交和树项，不代表任意 Josh 历史 filter 都获得了冲突语义。
- 这版 jj 默认写入 `change-id` Git header，jjosh 不覆盖用户的设置。它有助于保留投影中的身份，但不能追溯恢复旧 Git 提交的所有原生身份，也不保证任意 Git 工具都保留该 header。
- `projection push` 使用普通的非快进拒绝；`--force` 可以绕过它。它没有 `link push` 的目标观测/lease 契约，不提供跨远端原子发布。`--dry-run` 不更新远端，但可快照本地工作副本并写入转换所需的对象。

## 将已有 jj 项目纳入 monorepo

`native import` 直接读取已有 jj 仓库，将原生修改图放入 monorepo；不要求先导出 bundle，也不要求来源有上游。目标可以是新仓库，也可以是已有 monorepo。每个导入项对应一个完整来源仓库，NAME 是 canonical 顶层目录和引用命名空间。

```sh
# 在来源中显式记录要迁入的工作文件。导入本身不快照或写入来源。
jjosh -R /path/to/ebox status
jjosh -R /path/to/ekp status

# 已有 monorepo 可以跳过 init。
jjosh git init --no-colocate combined
jjosh -R combined native import \
  --source ebox=/path/to/ebox \
  --source ekp=/path/to/ekp

# 保留已有 monorepo 的 @，用普通 jj 操作选择组合入口。
jjosh -R combined new @ ebox/workspace/default ekp/workspace/default \
  -m "Integrate native projects"
```

导入读取固定 operation 的当前 view 和所需父历史，包括未发布的 change、空提交、有序 merge 父边、当前 divergence、冲突引用的正负项，以及原生冲突树和标签。不导入旧 operation/evolution 历史；来源须先协调多个 operation heads。未记录的磁盘修改不包含在输入中。只读捕获不依赖源索引，也不会为读取而补造旧 Git commit 的 jj 身份。

NAME 只允许 ASCII 字母、数字、`-`、`_`。本地书签和现有 tag 变为 `NAME/原名`，远端观测变为 `NAME-原远端`。来源 `@git` 只是其本地 Git backend 的观测：与本地引用相同的项不重复导入，不同的项仍保留。来源 workspace 的选择成为 `NAME/workspace/工作区名` 书签；它们是选择组合入口的普通书签，不是已挂载的 workspace，组合后可按需删除。已占用的目录和命名空间会被拒绝。多个输入在一个 jj 事务中发布，不自动选择 checkout 或 rebase 策略；目标自身的工作文件按正常 jj 命令先快照。

### 获取上游更新和外部贡献

来源和发布目标不绑定。可以从上游、贡献者的 fork，或另一个本地 jj workspace 获取指定分支：

```sh
jjosh -R combined native fetch ebox https://example.org/upstream/ebox.git \
  --branch main --remote upstream
jjosh -R combined native fetch ebox /path/to/contributor-jj-workspace \
  --branch topic --remote contributor

jjosh -R combined log -r 'ebox/topic@ebox-contributor'
jjosh -R combined new @ 'ebox/topic@ebox-contributor' -m 'Integrate contribution'
# 冲突保留为普通 jj 冲突；编辑解决后执行 status，或使用 jj resolve。
```

`fetch` 将观察结果记录为 `PROJECT/BRANCH@PROJECT-REMOTE`。未跟踪的观察不会改动本地书签；显式 `bookmark track` 后按 jj 的引用合并规则更新对应书签。它不替你 checkout、合并工作头或重排后继。可以使用普通 `new`、`rebase`、`squash` 选择整合方式，不要求所有已导入历史都建立在一个全局 `jjosh/trunk` 上。

本地 jj 来源保留其原生元数据和冲突引用；只读取选定书签的闭包。Git 来源按 jj 的 Git 编码读取新提交，已知源祖先复用导入对应关系，因此不会把原先只存于 extras 的 change ID 重新猜成另一个身份。来源重写可以产生真正的 divergence，不自动 converge 或丢弃旧修改。

### 向任意 remote / branch 发布子项目

```sh
jjosh -R combined native push ebox \
  --remote https://example.org/my-fork/ebox.git --branch feature -r @ --dry-run
jjosh -R combined native push ebox \
  --remote https://example.org/my-fork/ebox.git --branch feature -r @

# 可换成另一个 URL 或已配置的 Git remote，不需要改项目历史中的 marker。
jjosh -R combined native push ebox --remote review --branch alternate -r my-change
```

发布只投影所选项目。没有该项目变化的本地提交被裁剪，其他项目的内容不导出，canonical 修改图不因发布而被拆分或重写。所选项目在发布 revision 中有未解决冲突时拒绝发布；其他项目的冲突不阻止它。保留下来的冲突祖先使用 jj 原生 Git 编码，不把其传输树冒充已解决源码。

已导入的原始提交作为历史边界复用；新增发布提交记录到 canonical change 的对应关系。例如一个 change 同时修改 Ebox 和 Ekp，只发布 Ebox 后再次收到相同提交，会回到对应的 canonical change，而不是制造另一个只含 Ebox 的同 change-ID 版本。基于这次发布的外部贡献接在已知 canonical 边界上，保留其中的 Ekp 内容；真正不同的外部改写仍须按 jj 语义整合。

发布保护复用 `link` 的按目标记录的 lease，但配置 remote 会先解析为实际 URL，避免 remote 改名或改 URL 后误用旧记录。首次发布只允许创建分支或快进；已有观测时使用 force-with-lease；`--force` 显式绕过保护。多个发布 URL 的 remote 须选择一个明确 URL。没有跨远端原子发布；远端已更新而本地记录失败时，需要核对远端后恢复记录。

### 原生状态、兼容性与边界

- 只有来源分支观测使用普通 jj remote bookmarks。native fetch 通过 jj 原有的 Git export 同步本次更新的观测；不自动导出其他本地书签，也不替非 colocated 仓库导入无关的 Git 变化。
- 导入／发布的提交对应关系存于 `refs/jjosh/native/NAME/` 私有 Git refs，不注册 remote，不生成 `origin/<hash>` 书签，不参与书签跟踪。初次导入只记录覆盖源图的边界，沿父边恢复其余对应关系；部分发布会裁剪父历史，仍须记录新输出与原提交的对应关系。私有 refs 同时保活两边的 Git 对象，不是可任意清除的 Josh 缓存。
- `op restore` 对书签和工作区遵循普通 jj 行为：colocated 仓库自动同步；非 colocated 仓库可用 `git export` 将恢复结果写回 Git refs。不可变提交的转换记录和已发生的远端发布不随 `op restore` 撤销；它们不自动恢复任何工作头或分支观测。重写本地书签不会把旧版本的转换记录改指向新版本。
- 旧版 `jjosh-native-NAME` 伪远端使用 `jjosh native migrate` 显式迁移：先写私有对应关系，再通过普通 jj Git export 删除旧引用，不改写提交图或工作文件，并移除与本地引用相同的 `NAME-git` 观测。冲突记录或被跟踪的内部书签须先解决／取消跟踪。恢复到迁移前的 operation 后，可再次运行迁移。来源或旧 bundle 含这类记录时，先迁移来源并重新导出。
- native 操作不新增 operation-store 格式。更早使用 `simple_op_store_native_remotes` 的实验仓库仍需保留原仓库并从来源重新导入；不要手改格式标记。已有 sparse 格式要求不受影响。
- 当前只支持 SHA-1 Git backend 和完整来源仓库的顶层重定位。目录／父 ID 变换会使旧签名失效；映射提交移除并报告签名，原始边界对象保留。后端不能保留原生字段时拒绝导入。
- gitlink 保留外部 commit ID，不递归导入子模块，也不自动展开文件。浅克隆、缺失对象和旧提交缺失原生身份需先在来源中明确处理。
- 不自动重写来源内部的 `.link.josh`、构建配置或共享依赖布局。工作区的物理布局继续由 native sparse 管理。
- 现有 tag 的来源名称和原生引用目标按命名空间保留；tag 的跨项目语义、注释／签名及发布管理仍是未决问题，这里不提供新的 tag 发布策略。

### 可选的离线状态包

无法直接访问来源时，仍可使用 `native export FILE`、`native inspect FILE`，以及 `native import --source NAME=FILE`。包是包含 `manifest.json` 和自包含 Git pack 的版本化 TAR，保留当前原生修改图、引用和树对象。将一个 monorepo 整体作为 NAME 导入时，会建立 NAME 的新对应关系；不会复制来源内部项目的私有转换记录或发布 lease，也不会自动启用其内部项目的 native 命令。导出不覆盖已有文件，inspect 验证格式和闭包但不认证发布者。Bundle 是原生图的运输选项，不是整个仓库运行状态的备份；完整备份须保留 jj metadata 和 Git backend。

## 整体迁移本地修改图

`native transplant` 在当前仓库内改写显式选定的可变提交图，不逐目录导出，也不裁剪空提交。它保留 change ID、作者、描述和有序父边；Git commit ID 会改变。合并提交相对原父树的自身修改会被重放，未解决冲突的每个带符号树项都会映射到新路径。这是原生图迁移的契约，不是 Josh 投影必须保持原 DAG 的要求。

```sh
jjosh native transplant \
  -r 'old-base..local-tip' \
  --map jj=src/jj --map josh=src/josh --map .=src/jjosh \
  --exclude jj/.link.josh --exclude josh/.link.josh \
  --parent old-base=new-base --dry-run
```

确认计划后去掉 `--dry-run` 才会应用。示例中的提交和基线必须已经存在于当前仓库；命令不负责跨仓库获取对象，也不自动建立或重定位 `.link.josh`、来源书签和组合基线。不能把当前 fork tip 或带本地修改的快照当作已核实的上游基线。

- `.` 是未被其他映射接管的根目录回退。每个保留路径必须有唯一归属；碰撞检查覆盖同一个冲突树的所有项，防止不同源路径映射后相互抵消。不同历史版本之间的目录重命名可以归一化。
- 选集必须包含所有受影响的可见后代，也必须自行包括希望迁移的侧支历史。`old-base..local-tip` 可以包含从独立根合入的侧支，`old-base::` 不一定包含它。
- 每条离开选集的父边都要显式映射；`--parent OLD=OLD` 表示保留该父边。Git 的无父提交在 jj 中仍有 `root()` 父边。把它替换成已有内容的基线，会以空树为原基础重放整棵源树，可能产生真实的 add/add 冲突；显式映射不是基线正确性的证明。
- gitlink 必须用其完整路径显式 `--exclude`；命令不会自动把子模块展开为文件。不要排除需要保留的源码。
- 预检拒绝不完整选集、选集中同一 change ID 的多个版本、路径碰撞、父边合并或循环、不可变提交、历史操作和多个操作头。不会用 `--ignore-immutable` 或 `converge` 隐藏问题。
- 未导入 jj 的 Git 引用或 checkout、进行中的 Git 操作，以及需要兼容性元数据导入的旧提交，都必须先单独处理。选中的当前工作区还必须是新鲜且已快照的；不要在执行期间并发切换或编辑工作区。

`--dry-run` 检查图映射和源树项，不写新提交，不修改操作头、Git refs、jj view 或工作区内容；底层读取、合并及快照检查可能留下可回收的对象或缓存。它不计算重放后的最终冲突，不能当作迁移结果已经保真的证明。

应用通过一次 jj 事务发布图和引用改写，但不是整个文件系统的原子回滚：底层提交写入会保留 keep refs，Git 导出、操作发布和工作区 checkout 发生在不同阶段，I/O 失败可能留下部分这些状态。真实迁移应先在独立目录演练，核对每个 change ID、父边以及需要保留的树内容，再决定采用结果；原操作日志不会随 Git DAG 自动迁入。

## 当前边界与发布安全

- **只支持 SHA-1 Git 后端。** link / projection / native 不支持 SHA-256 Git 仓库。
- **投影不是权限隔离。** 当前 projection 会先获取原始历史再在本地转换；不能用它保证其他目录的内容不被下载或访问。
- **来源更新不能任意改写历史。** embedded link 更新要求来源历史及过滤后的历史向前推进；旧式 Embed 图需要先显式执行 `jjosh link migrate`，不会自动迁移。
- **组合书签是本地状态。** `jjosh/trunk` 和 `jjosh/source/*` 是 jjosh 管理的保留命名空间，默认不可改写，不再创建合成远端。更新来源或发布单个挂载目录应使用 `link update/push`；这些投影书签不是来源仓库的原始提交。
- **同一来源的多个挂载可以有 divergence。** 保留来源的显式 change ID，不因跨挂载重复而拒绝导入，也不自动改写身份或调用 `converge`。可用 commit ID 或 change offset 区分版本。`jj converge` 会替换提交并重放后代，不是单纯消除标记；来源投影默认 immutable，显式改写它们应先评估对隔离历史和后续更新的影响。
- `link push` 的发布保护按目标分别记录。jjosh 按精确的远端 URL 和目标分支，在本地记住最近成功推送或显式获取到的位置。只有远端仍匹配该位置，才允许受保护的历史改写（force-with-lease）。没有记录时，只允许创建分支或快进推送。
- `link push --force` 会绕过上述保护。不要把它当成推送被拒绝后的常规重试选项；先确认不会丢弃远端的他人修改。预检和失败的推送都不会刷新记录的位置。

## 本仓库也是一个组合工作区

```text
jjosh/
├── jj/                 # Jujutsu 源码及其 .link.josh
├── josh/               # Josh 源码及其 .link.josh
└── crates/jjosh-cli/    # jj 命令入口与 Josh 的集成
```

根工作区通过本地路径依赖使用 `jj/` 和 `josh/`，因此可以在同一处修改集成层及两边的底层实现，再分别发布对应的修改。

进一步阅读：

- [jj 项目介绍](jj/README.md) 与 [jj 教程](https://docs.jj-vcs.dev/latest/tutorial/)
- [Josh 项目介绍](josh/README.md) 与 [过滤表达式参考](https://josh-project.github.io/josh/reference/filters.html)
- [link 命令实现](crates/jjosh-cli/src/link.rs) 与 [工作流测试](crates/jjosh-cli/tests/link_workflow.rs)
- [projection 命令实现](crates/jjosh-cli/src/projection.rs)、[导入测试](crates/jjosh-cli/tests/projection_fetch.rs) 与 [投影视图双向回归](crates/jjosh-cli/tests/projection_workflow.rs)
- [原生状态包](crates/jjosh-cli/src/native_bundle.rs)、[原生导入](crates/jjosh-cli/src/native_import.rs) 与 [状态包回归测试](crates/jjosh-cli/tests/native_import.rs)
