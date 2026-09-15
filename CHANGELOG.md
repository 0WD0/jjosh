# jjosh 变更记录

这里仅记录 jjosh 相对上游 Jujutsu/Josh/gitoxide 的产品与兼容性变化。
`jj/`、`josh/`、`gitoxide/` 内的上游 CHANGELOG、README 与用户文档属于各自上游；jjosh-only change 不应为了说明本地行为去修改它们，上游发行说明随对应 upstream remote 同步。

## Unreleased

### 不兼容变化

- subproject 的项目定义、稳定 label、不可变 binding、connection 身份和转换观察由 JJ operation 持有。
  当前写入格式继续是 `simple_op_store_projects_v3`；嵌套项目只放宽拓扑校验，不改变 ProjectRecord、View 编码或 operation-store 类型。v1/v2 仅保留读取兼容，当前版本不提供旧格式迁移器。
- `project migrate` 与通用 legacy-adoption 实现已退役。仍停留在旧 project store 的仓库，需要先用支持对应格式的历史 jjosh 完成升级。
- 旧的 `jjosh native`、`link` 以及早期 projection/project 管理入口不再作为兼容别名保留。日常远端同步统一走 `jjosh git fetch/push`，项目管理统一走 `jjosh project`。
- project 本地引用继续使用 `NAME#LABEL` 约定。活动 label 会占用对应后缀；不会为了同时支持同名字面根引用而自动重新解释已有 refs。
- jjosh 的 sparse 工作副本使用 fileset 选择与显式路径映射；复杂 sparse/layout 状态写入 operation，并使用新的 working-copy / operation-store 格式标记，旧 writer 会被拒绝。

### 主要能力

- 项目根允许严格包含；父项目交付完整子树，父子身份、连接、观察与 lease 独立。`touched_projects` 同时列出实际被触及的父、子范围。
- `log` 默认显示提交实际触及的子项目；`touched_projects` 模板字段支持自定义展示，按当前加载的 project View 和原生 diff 语义计算，不改变提交或引用身份。
- 一个 monorepo 共享完整 JJ 开发图，同时为多个 project 建立独立交付范围。ProjectId、BindingId、ConnectionId 分别承担项目、转换关系和逻辑 remote 实例的稳定身份。
- project remote 支持项目内独立的 `origin`/`upstream` 等别名；Git 物理 handle 与逻辑别名分离，rename 保留 connection/binding 身份，remove 后重新 add 是新实例。
- fetch/push 在网络操作前解析 project 范围、binding、endpoint 和目标 ref。转换观察与 publication lease 分离；canonical 版本相同也不会丢掉新的 raw/source 证据。
- `project import --nested` 可把本地 JJ 仓库作为新的外层项目导入；`--preserve` 保留可支持的项目、binding、connection、来源 provenance 和 publication ledger。
- filtered/view 来源支持 shallow fetch、deepen/unshallow、按 raw OID 获取以及临时 fetch mirror；来源 raw 历史和 normalization generation 作为历史逆转换材料独立保留。
- project metadata 冲突仍可被 list/show/template 等诊断路径读取；精确 selector、transport、tracking 修改和其他 mutation 继续要求身份验证成功。
- remote 管理使用 Git 原生 config/ref 锁、ConnectionId ownership 和 expected-value/CAS，避免覆盖未知并发修改；跨资源操作不声称全局原子性。
- transformed bookmark/tag 发布支持 project 路由与精确 endpoint/ref lease；签名 annotation 在需要重写时不会被当作仍然有效的签名继续传播。

### 工作副本

- `jjosh sparse set` 使用普通 fileset；`--add` / `--remove` 形成有序选择规则。
- `jjosh sparse map set SOURCE=DEST` 将 canonical 路径映射到物理工作目录位置，不改变 commit 中的路径、project canonical root 或远端表示。
- sparse/layout 是 operation-versioned 的工作副本状态；恢复历史 operation 可以恢复期望布局，但不会把已发生的外部发布或本机 remote 配置一起回退。
