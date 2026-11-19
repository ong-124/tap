# 输入栏自建历史库 + 虚化预选 + Tab 补全

## Context

输入栏三个痛点：① ↑/↓ 只回显本会话内存历史；② Tab 直通 PTY（提示符下触发 shell 对空行的补全，屏幕乱码）；③ 无 zsh-autosuggestions 式虚化建议。

经讨论收敛的产品哲学（用户明确拍板）：**tap 记录的是「你对我的输入」，不负责终端历史**。提示符下所有按键都先进输入栏 → tap 天然全量可见提交过的命令，自建持久历史库即可达成 ↑/↓ 回显与虚化预选，**不做** HISTFILE 解析、不做 shell 集成、不做镜像。

用户确认的决策：
- 历史来源 = tap 自建记录（持久化，含时间戳 + cwd 元数据，为将来「分类」备料）；排序**新近优先**
- 虚化预选 = zsh-autosuggestions 语义：前缀匹配、光标尾部才显示、`→`/`End` 整体接受、编辑即消失、preedit 期间压制
- Tab 补全 = **仅文件系统**（首词 PATH 可执行 + 内建；其余词按焦点 pane cwd），多候选 = 候选行 + Tab 循环（早前已确认），唯一命中目录补 `/`、其余补空格
- Shift+Tab（BackTab）保持直通；用户自定义裸 Tab 绑定优先于输入栏（bindings 层在前，逃生口）

## 文件改动

### 1. 新建 `frontends/rioterm/src/input_history.rs`（存储，cfg(not(wasm32))）

- `const MAX_ENTRIES: usize = 10_000`；`struct Entry { ts: u64, cwd: String, line: String }`
- `store_path()` = `rio_backend::config::config_dir_path().join("input-history")`
- `load(path, cap) -> Vec<String>`：只返回 line（旧→新序），InputBar 现有 `history` 字段类型不变、10 个旧测试零改动；TSV `splitn(3, '\t')`，不足 2 tab 的坏行跳过；超 cap 重写文件为尾部
- `append(path, &Entry)`：`OpenOptions::append+create`（O_APPEND）单次 write_all；前置 `create_dir_all(parent)`；ts 取 UNIX_EPOCH 秒；cwd 由调用方传入（模块不做 syscall）
- 并发：多窗口启动并发重写为良性（后写者胜、均 ≤cap）

### 2. 新建 `frontends/rioterm/src/completion.rs`（补全，纯函数）

- `gather(word, cwd: Option<&Path>, path_dirs: &[PathBuf]) -> Vec<String>`（cap 100）——**PATH 以参数传入**（测试不能 set_var）；生产包装从 `std::env::var("PATH")` 读
- 首词判定（word_start==0、无 `/`、非 `~`/`$` 开头）→ PATH 可执行（unix `PermissionsExt::mode() & 0o111`）+ `const SHELL_BUILTINS: [&str; 30]`；否则 fs 模式：`~` 展开、目录列项、隐藏文件按前缀过滤
- `common_prefix(&[&str]) -> String`；不做 PATH 缓存（首次 Tab 扫描 1-3ms，循环复用已存候选，TTL 缓存不值）

### 3. `frontends/rioterm/src/renderer/input_bar.rs`

- 新字段：`suggestion: Option<String>`、`completion: Option<CompletionState{candidates, index, word_start, word_end}>`、`record_history: bool`、`suggestions: bool`
- `set_history(Vec<String>)`、`set_options(record, suggestions)`
- `refresh_suggestion()`：**变异时刷新**（render 每帧因 caret 闪烁重跑，避免每帧扫万条历史）；条件 `suggestions && preedit 无 && history_pos 无 && cursor==char_count && buffer 非空`；`history.iter().rev()` 首个 starts_with 且 != buffer；在每个变异器（insert/backspace/delete×3/move×4/history×3/clear/take/set_preedit）末尾调用 + `invalidate_completion()`
- `accept_suggestion() -> bool`：buffer.push_str + cursor 置尾
- `tab_complete(cwd: Option<&Path>)`：空词/0 候选仅消费；唯一 → 插入 + 后缀（`/`/空格）；多候选 → LCP + 置 state，再次 Tab 循环替换 `buffer[word_start..word_end]`（替换前 fork_from_history）；状态机在 input_bar 内，词提取用 `Vec<char>` + 既有 `byte_index`
- `set_locked(true)` 清 suggestion/completion
- render：建议宽度并入 L539 预算式（建议生效时 cursor 必在尾部，两段 view_start 二分原样不动）；caret_x 后以 DIM_TEXT_COLOR 画建议（与 preedit 互斥自然压制），caret rect 盖其首 2px；候选行 `cand_font = font_size*0.5`、`row_y = text_y - cand_font - 6.0`，仅当 `row_y >= panel_y + border_width + 4` 才画，逐段 measure+draw（当前项 TEXT_COLOR 其余 DIM_TEXT_COLOR），超 `content_right` 即止（不用 elide_tail——只保头部）

### 4. `frontends/rioterm/src/screen/mod.rs`

- ArrowRight（L1130）：`if !accept_suggestion() { move_right() }`；End（L1138）：有 suggestion 先 accept 否则 move_end
- L1112 match 加 `NamedKey::Tab`：读焦点 pane cwd（owned，`teletypewriter::foreground_process_path(*main_fd, shell_pid)`，cfg(not(windows))，仿 context/mod.rs:1462 范式）→ `tab_complete(cwd.as_deref())`，无条件 `return true`
- `submit_input_bar_line`（L1197）重排：先取 owned cwd → `take_submitted_line()` → **过 `close_if_exited` 检查后**再 `record_history && !line.is_empty()` 时 append 落盘 → send_write（修复：现行 take 后才 close_if_exited，退出的行不能落盘）；相邻去重一致性：仅当 runtime push 发生（`history.last() == Some(&line)`）才 append
- Screen::new（renderer 创建后 L233）：`renderer.input_bar.set_history(input_history::load(...))`（cfg(not(wasm32))；**不放 Renderer::new**——热重载会白做 IO 且被旧 bar 覆盖）

### 5. `rio-backend/src/config/input_bar.rs` + 接线

- InputBarConfig 加 `record_history`（rename `record-history`，default_bool_true）、`suggestions`（default true），字段级 rename 风格；`use crate::config::defaults::default_bool_true`（defaults.rs:4 pub）；Default impl 同步
- 接线两处：renderer/mod.rs:349-353 初始化加 `set_options`；screen/mod.rs:581-582 热重载 take/restore 加 `old_input_bar.set_options(...)`

### 6. `frontends/rioterm/src/main.rs`

- mod 区加 `#[cfg(not(target_arch = "wasm32"))] mod input_history;`、`mod completion;`（wasm 目标保护——rioterm 有 wasm32 target）

## 测试（inline，临时文件 temp_dir+唯一名+手工清理，不加依赖）

- input_history：TSV 追加+重载 roundtrip、相邻去重、cap 重写为尾部、坏行跳过、空 cwd（双 tab）、两次独立 append 并存
- input_bar：建议出现/左移消失/编辑消失/preedit 压制/→ 与 End 接受/最近优先（两条匹配取新）/buffer==整条无建议/浏览历史时压制/Tab 后建议刷新；completion：首词判定、LCP、循环替换、失效、唯一后缀 `/` 与空格、空词 no-op、多字节词（中文/emoji）
- completion：临时目录 gather（隐藏过滤、目录后缀）、common_prefix
- config：两键默认 true、显式 false 解析
- 既有 10 个 input_bar 测试须零改动通过

## 文档（用户规则）

- 新建 `task/20261119-HHMM_输入栏自建历史库与虚化预选.md`（固定节「状态变更记录」；背景写明接管 20261118-1941 的 Tab 直通已知限制与内存态历史）
- `todolist.md` 末尾新增条目 + 子任务（存储模块/虚化预选/Tab 补全/路由与持久化接线/文档验证），最终编辑消息含完整条目块
- `docs/20261118-2150_快捷键清单.md` L131（↑/↓ 全历史）、L134（Tab 补全、Esc 仍直通）重写，新增 → 接受虚化建议、End 行
- 旧任务文档 20261118-1941 不动（完结条目原位保留）

## 验证

`cargo build -p rioterm` → `cargo test -p rioterm` → `cargo clippy -p rioterm --all-targets` → `cargo fmt --all`（每步保持绿色，按第 6 节执行顺序推进）

用户手测清单（任务文档记录）：重启 tap 后 ↑ 可回显上次会话命令；虚化建议出现/→ 接受/编辑消失/最近优先；Tab 命令与路径补全、候选行循环、目录 `/` 后缀；分屏焦点切换后 Tab 按 pane cwd 补全；`[input-bar] record-history/suggestions` 热重载；锁定态不出现建议。
