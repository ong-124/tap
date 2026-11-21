# Tab 补全候选项包含 command-aliases

## 背景

用户反馈：Tab 补全中不包含 alias。现状是 `[input-bar] command-aliases`（如 `k = "kubectl"`）只参与**动态路由**——`route()` 经 `resolve_tool` 把 alias 行解析到 Cobra 工具后按工具补参数；而**静态命令位**（首词补全）的候选只来自 PATH 可执行文件 + 内建表（`command_candidates`），alias 本身永不出现在候选列：裸打 `k<Tab>` 无任何候选，用户只能凭记忆手打 alias。

## 设计

alias 是用户显式配置的命令名，补全语义上属于命令位候选，并入 `command_candidates`：

- 候选列**前置于**内建表与 PATH 扫描：alias 少且用户自选，必须躲开 PATH 扫描触顶的 `MAX_CANDIDATES` 截断；最终统一按 insert 排序，位置前插只影响截断存活不影响展示顺序。
- 大小写敏感 `starts_with`（与命令位既有约定一致，见 20261120 大小写任务拍板：命令与 ssh 主机补全维持大小写敏感）。
- `seen` 去重沿用：alias 遮蔽同名 PATH 命令时只占一行（insert 相同，无信息损失）；内建表在 alias 之后插入，同名时 alias 胜出（display 更有信息量）。
- 行展示 `k → kubectl`（`display` 仅用于候选行渲染与测宽，LCP/插入全走 `insert`，已核实 `apply_candidates_at` 两路径），`insert` = alias 本身（`sh_escape`，即 shell 实际执行的词）；单候选命中补空格（`is_dir: false`）。
- `gather` / `command_candidates` 签名加 `aliases: &HashMap<String, String>`。

## 实施记录

- frontends/tapterm/src/completion.rs：
  - `command_candidates(prefix, path_dirs, aliases)`：alias 候选列前插（display `名 → 展开`、insert = alias、`seen` 去重、大小写敏感前缀），内建与 PATH 扫描不动
  - `gather(word_start_char, word, sources, aliases)`：命令位分支透传
  - 测试：既有 20 处 gather 调用补 `&no_aliases()`；新增 2 用例——alias 候选存在且 display 为展开、长前缀不命中、非首词位不命中；alias 遮蔽 PATH 同名可执行文件时两源合一行为断言（`kill` 内建同前缀共存核实）
- frontends/tapterm/src/renderer/input_bar.rs：
  - `tab_complete` 两处静态分支（`Command|Path` 与 dynamic 关闭的 legacy 分支）调 `gather` 时传 `&self.aliases`（bar 经 `set_options` 已持有配置表，无需经 `Sources` 再传一层）

## 边界（不改）

- alias 只影响**本地首词位**：ssh 会话内首词走 `RemoteCommands`（远端 compgen），远端环境没有本地 alias，不注入。
- alias 行的参数补全仍走既有动态路由（`resolve_tool` → Cobra `__complete`），本任务不涉及。
- ghost/↑ 历史对 alias 无感知（alias 提交后按普通命令行入库）。

## 验证

- cargo test -p tapterm：389 passed, 0 failed（+2 新用例）
- 触碰文件 clippy 无告警、rustfmt 干净
- 待用户手测：配置 `command-aliases` 后 `k<Tab>` 候选行显示 `k → kubectl`、Tab 补出 `k `（带空格）、与同名 PATH 命令只占一行、ssh 会话内补全行为不变

## 状态变更记录

- [20261121-1557] （创建）→ 已完成：alias 并入静态命令位候选并测试通过，待用户手测。
