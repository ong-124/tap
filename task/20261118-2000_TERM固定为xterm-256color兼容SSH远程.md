# 任务：TERM 固定为 xterm-256color 兼容 SSH 远程

## 背景与目标

本地安装 tap terminfo 后，TERM 被设为 `xterm-tap`（或 `tap`）。ssh 会把 TERM 原样转发到远端，而远端机器几乎都 没有 `xterm-tap`/`tap` 的 terminfo，导致 `unknown terminal type` 报错、less/vim/htop 等表现异常。

目标：TERM 固定为 `xterm-256color`（各远端 ncurses-base 必带），消除 ssh 远程兼容问题。

## 可行性结论（20261118 调研）

可行，且主体接近"改字符串"，理由：

- tap 输出的转义序列本身是 xterm 兼容的（`misc/tap.terminfo` 的 `tap+base` 全部为标准 xterm 序列），谎报 `xterm-256color` 不会破坏按键/光标行为
- `COLORTERM=truecolor` 仍会设置，现代程序（nvim、bat、tmux 3.2+ 等）据此启用真彩色，颜色能力基本无损
- `xterm-256color` 在 Linux/macOS/BSD 远端普遍存在

但有三处"不只是字符串"的细节：

1. **TERM 决定点有三处，需一起改**：
   - `frontends/tapterm/src/main.rs` L49-64（`setup_environment_variables`：xterm-tap → tap → xterm-256color 的探测链）
   - `libtap/src/lib.rs` L624-636（pty spawn 环境同一套探测逻辑）
   - `teletypewriter/src/unix/mod.rs` L617-618（flatpak 沙箱场景硬编码 `--env=TERM=tap`）
   - 建议抽公共 helper（如放在 teletypewriter 或 libtap），三处共用；或最小改动：三处直接统一写死
2. **远端 tmux 真彩降级**：tap terminfo 里有 `Tc`（truecolor）能力声明，固定为 xterm-256color 后远端 tmux 内部默认降为 256 色，需 `set -as terminal-overrides ",*:Tc"` 才恢复。属可接受的已知代价，文档/注释说明即可
3. **可配置性取舍**：tap 现有 `env-vars` 配置本可覆盖 TERM（main.rs 中 env_vars 在 TERM 设置之后应用），但要求用户手工配置不直观。本任务按"直接固定"实现，预留后续配置项（如 `term = ...`）的扩展空间

## 关键子任务

1. 统一三处 TERM 设置点（tapterm main / libtap pty / flatpak spawn），固定为 `xterm-256color`，保留 `COLORTERM=truecolor`
2. 本地会话回归验证（颜色、按键、vim/less/tmux）
3. ssh 远程会话验证（无 unknown terminal type、远端 tmux 颜色表现记录）

## 实施记录

### 20261118-2010 代码实施

三处改动，均为字符串级：

1. `frontends/tapterm/src/main.rs` `setup_environment_variables`：删除 `xterm-tap`/`tap` terminfo 探测链，TERM 固定为 `xterm-256color`（注释说明 ssh 兼容动机）
2. `libtap/src/lib.rs` pty spawn 环境：同步删除探测逻辑，固定 `TERM=xterm-256color`，`COLORTERM=truecolor` 不变
3. `teletypewriter/src/unix/mod.rs` flatpak 分支：`--env=TERM=tap` → `--env=TERM=xterm-256color`

说明：

- `teletypewriter::terminfo_exists` 保留未删（pub API，无 dead_code 告警）
- 按用户要求未编译；本地与 ssh 远程验证由用户自行执行
- 用户确认基本不再使用 tmux，远端 tmux Tc 降级影响可忽略

## 状态变更记录

- [20261118-2000] 无 → 待开始：任务创建
- [20261118-2010] 待开始 → 已完成：代码实施完成（三处 TERM 设置点统一固定为 xterm-256color）；编译与本地/ssh 验证由用户自行执行
