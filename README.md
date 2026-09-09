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

**jj 管“修改怎么组织”，Josh 管“仓库怎么呈现”，jjosh 将两者放进同一个开发流程。**

## 原生 sparse：按 fileset 选择工作副本文件

本地 jj 集成接续了 pmandloi28 的 [fileset sparse PR #9760](https://github.com/jj-vcs/jj/pull/9760)，并修正表达式往返、空旧状态迁移和旧二进制误读问题。这不表示该 PR 已在上游合并。

```sh
jjosh sparse set 'glob:"**/*.rs" ~ src/generated'
jjosh sparse set --add 'src/generated/needed.rs'
jjosh sparse list
jjosh sparse edit
jjosh sparse reset
```

位置参数替换当前选择；`--add` 取并集，`--remove` 取差集，同一次调用先加入再排除。输入按当前目录解释；`sparse list` 输出可重新解析的根相对表达式。`sparse edit` 编辑一条可以跨行的表达式，空白表示不选择文件，而不是把每行当成独立路径。

这与旧版 sparse 有行为差异：`--remove` 现在可以排除已选目录的子路径，不再只是删除一个已列出的前缀；旧的工作副本根相对输入需改用 `root:`，或者从仓库根运行。

未检出的内容仍在完整原生提交里，不会被当成删除；`file list`、revision 操作不会因此被限制为可见文件。新 workspace 可继承整个筛选表达式。这里只支持筛选，不包含路径重排、Josh workspace 定义绑定或 sparse 配置的 operation 版本化。

旧前缀状态可读取。首次使用不能表示成旧前缀列表的表达式时，标准本地工作副本会标为 `local-fileset`，旧二进制随后加载时明确拒绝；`sparse reset` 不自动降级这个标记。切换前应退出仍在运行的旧 jj 进程，不要手改类型标记。自定义 working-copy 实现需自行处理兼容性。

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
- 原生未解决冲突尚不能通过此 Git 历史投影通道保真转换。预览和发布拒绝包含冲突的所选祖先历史；fetch 在公开投影引用前检查源历史的 `jj:trees`。干净的后继提交不能掩盖仍含原生冲突的祖先。`native` 状态包可以保留这些冲突，但不是冲突投影的替代实现。
- 这版 jj 默认写入 `change-id` Git header，jjosh 不覆盖用户的设置。它有助于保留投影中的身份，但不能追溯恢复旧 Git 提交的所有原生身份，也不保证任意 Git 工具都保留该 header。
- `projection push` 使用普通的非快进拒绝；`--force` 可以绕过它。它没有 `link push` 的目标观测/lease 契约，不提供跨远端原子发布。`--dry-run` 不更新远端，但可快照本地工作副本并写入转换所需的对象。

## 用原生状态包聚合已有 jj 仓库

已有 jj 仓库的本地状态不能只靠 Git bundle 保真。`native export` 将已记录的当前 jj view、原生提交元数据和所需内容对象封存到一个自包含文件；`native import` 再将各包的历史整体放进不同目录。它不通过 patch 重放，也不要求先选择一个统一上游基线。

```sh
# 在各来源中显式记录工作文件；export 本身不快照、不修改来源。
jjosh -R /path/to/ebox status
jjosh -R /path/to/ebox native export /tmp/ebox.jjbundle
jjosh -R /path/to/ekp status
jjosh -R /path/to/ekp native export /tmp/ekp.jjbundle

# 不需要来源仓库，也不需要当前目录有 jj 仓库。
jjosh native inspect /tmp/ebox.jjbundle

jjosh git init --colocate combined
# 也支持 --no-colocate；包的格式不依赖目标工作区模式。
jjosh -R combined native import \
  --source ebox=/tmp/ebox.jjbundle \
  --source ekp=/tmp/ekp.jjbundle
jjosh -R combined new ebox/workspace/default ekp/workspace/default \
  -m "Aggregate native sources"
```

- 状态包是版本化 TAR，只有 `manifest.json` 与完整、非 thin 的 `objects.pack`。manifest 保存原生身份和 view；Git pack 只承担对象传输，包含原生冲突树的所有项，不能代替原生元数据。`inspect` 检查格式、图闭包及对象完整性；它不认证发布者身份。导出原子发布新文件，拒绝覆盖已有文件。
- 范围是当前 view 的所有提交引用及父祖先闭包，包括冲突引用的删除项。保留 change ID、作者和提交者的时间、描述、有序父边、空提交、合并、独立根、当前可见 divergence，以及带符号树项和冲突标签。不会扫描旧 operation/evolution 历史作为额外导出根，也不导出配置、凭据或未记录的工作文件。
- `--source NAME=FILE` 中的 NAME 同时是顶层目录和引用命名空间，只允许 ASCII 字母、数字、`-`、`_`。本地书签和 tag 变为 `NAME/原名`，远端观测别名变为 `NAME-原远端`，外来工作区选择变为 `NAME/workspace/工作区名` 书签；不创建虚假的已附加工作区。重名会报错，不覆盖引用。
- 导入要求新建的 SHA-1 Git 后端 jj 仓库，支持 colocated 和非 colocated。在一个 jj 事务中发布导入 view，但不改变 checkout、Git HEAD 或 index；随后使用普通 `new` 聚合。源 Git refs/HEAD 不会被安装成目标后端的缓存。失败可能留下未发布的内容对象及私有 keep refs，但不发布部分来源的 view。
- 目录和父提交变换会改变 Git commit ID，原始提交签名因此失效。包内保留原始签名，导入时移除并报告数量。不会承诺保留未建模的任意 Git 扩展头；目标后端若不能保留指定原生字段，会拒绝导入而非静默改写。
- gitlink 保留外部仓库的 commit ID，不递归打包子模块仓库，也不自动展开文件。浅克隆或缺失对象必须先补全。需要从旧 Git 数据生成兼容性 jj 元数据的来源，须先单独完成导入。
- `native` 不建立 `.link.josh`、`jjosh/trunk` 或发布绑定，也不把本地工作头认作上游 pin。目录聚合后的构建接线、共享依赖处理及来源同步策略仍是显式操作。

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
