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

jjosh 保留 jj 的常规命令，例如 `status`、`diff`、`log`、`new`、`split`、`squash` 和 `rebase`，另外增加 `link` 与 `projection` 命令。

### Josh：调整仓库的内容和历史视图

Josh 可以把一个 Git 仓库转换成另一个视图。例如，把大仓库里的 `services/api/` 提取出来，作为根目录展示，并保留与它相关的历史。

这不是简单复制文件或隐藏目录，而是转换 Git 历史。Josh 的 link 机制还可以将外部仓库的内容和历史组合进指定路径，并将适用的本地修改映射回来源结构。

**jj 管“修改怎么组织”，Josh 管“仓库怎么呈现”，jjosh 将两者放进同一个开发流程。**

## 从源码构建

需要：

- Rust 1.95 或更新版本，以及 Cargo。
- 系统编译工具链；`git` 命令需要在 `PATH` 中。
- 完整的源码目录，包括 `jj/` 和 `josh/`，它们是本项目的本地依赖。

当前 link / projection 集成测试面向 Unix；本地传输使用 `env` 命令，不应据此假定原生 Windows 已受支持。

在本仓库根目录执行：

```sh
cargo build --locked -p jjosh-cli
cargo run --locked -p jjosh-cli -- --help
```

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

jjosh 维护一个名为 `trunk@jjosh` 的书签（指向某个提交的名字），标记不含本地补丁的组合基线。本地修改位于基线上方；更新时，基线前进，本地修改自动迁移到新的基础上。

这不意味着自动消除所有冲突。若上游和本地修改冲突，需要先解决冲突，再发布。

### 5. 发布这个目录的修改

```sh
jjosh link push library --dry-run
jjosh link push library
```

这两条命令默认导出当前提交 `@` 中该 link 的内容，并映射回来源仓库的目录结构。上例会推送到你配置的发布仓库的 `jjosh/demo` 分支，不会将整个组合工作区推过去。

发布历史按目标 link 的实际文件变化裁剪：原本就为空的提交、只修改其他目录的提交，以及过滤后变空的提交，都不会仅因为带有描述而被保留。裁剪覆盖整段新增导出历史，不限于末端 `@`，也不会因此拒绝推送。空分支造成的重复或祖先父边会折叠；仍连接不同有效分支的合并关系会保留。本地 jj 工作区、提交历史和既有上游历史不变。

若要明确选择一个提交，使用 `-r <revision>`；它只选择导出历史的终点，不绕过内容裁剪，因此显式 `-r @` 与默认选择遵循同一规则。若要覆盖默认发布分支，使用 `--to <branch>`。预检与实际推送应使用相同的选择参数。

`--dry-run` 会检查导出和远端更新是否可行，但不更新远端，也不预留分支；实际推送仍可能因远端随后发生变化而被拒绝。

## 只需要一个仓库的局部视图？

使用 `projection`，不必将它挂载到组合工作区的子目录。下面在另一个新工作区中，只导入大仓库里的 `services/api/` 视图：

```sh
jjosh git init --object-hash sha1 api-workspace
cd api-workspace
jjosh config set --repo user.name "Your Name"
jjosh config set --repo user.email "you@example.com"
jjosh projection remote add api https://github.com/ORG/monorepo.git ':/services/api'
jjosh projection fetch --remote api
jjosh new 'main@api'
```

示例假设来源分支叫 `main`。`fetch` 导入转换后的历史，不自动切换工作区；`new 'main@api'` 才会在该版本上开始工作，此时 `services/api/` 的内容出现在工作区根目录。

也可以预览当前版本的某个局部视图，而不切换工作区或更新引用：

```sh
jjosh projection status ':/src'
```

当前 `projection` 提供 `status`、`remote add` 和 `fetch`，**没有 `projection push`**。它的远端配置禁止直接通过普通 Git 推送；需要双向开发和发布时，应选择 link 工作流。

## 当前边界与发布安全

- **只支持 SHA-1 Git 后端。** link / projection 不支持 SHA-256 Git 仓库。
- **投影不是权限隔离。** 当前 projection 会先获取原始历史再在本地转换；不能用它保证其他目录的内容不被下载或访问。
- **来源更新不能任意改写历史。** embedded link 更新要求来源历史及过滤后的历史向前推进；非快进更新会被拒绝。旧式 Embed 图也不会自动迁移。
- **来源书签不是普通远端。** `<branch>@link-<encoded-path>` 和 `trunk@jjosh` 是组合历史的标记，不要对这些合成远端使用普通 `git fetch/push`，应使用 `link update/push`。
- **发布保护按目标分别记录。** jjosh 按精确的远端 URL 和目标分支，在本地记住最近成功推送或显式获取到的位置。只有远端仍匹配该位置，才允许受保护的历史改写（force-with-lease）。没有记录时，只允许创建分支或快进推送。
- **`--force` 会绕过上述保护。** 不要把它当成推送被拒绝后的常规重试选项；先确认不会丢弃远端的他人修改。预检和失败的推送都不会刷新记录的位置。

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
- [projection 命令实现](crates/jjosh-cli/src/projection.rs) 与 [导入测试](crates/jjosh-cli/tests/projection_fetch.rs)
