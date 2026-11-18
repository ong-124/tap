# tap 任务清单

基于 rio fork 的终端项目改造任务。状态以【状态词】为准：待开始 / 进行中 / 已完成 / 放弃。

- [ ] 【待开始】Warp 式底部统一输入框（[详情](task/20261118-1941_Warp式底部统一输入框.md)）
  - 输入栏 UI 与渲染层（底部固定栏，sugarloaf 富文本）
  - 输入缓冲与编辑（光标移动、历史记录）
  - 提交发送整行命令到 PTY
  - 可配置启用/禁用（保留传统直接输入模式）
- [x] 【已完成】背景图透明度与字符层分离（[详情](task/20261118-1941_背景图透明度与字符层分离.md)）
  - 复现问题并定位根因（opacity 作用层级）：wgpu/Vulkan 路径背景图绘制晚于 grid 文本层，半透明图像直接盖在字符上
  - 背景图独立暗化/透明度处理，不影响字符渲染：背景图绘制前移至 grid 之前（与 Metal 一致），基色改为主题背景色
  - 字符对比度验证：待用户实测（opacity 仅影响图像层混合比例，字符全对比度）
  - 默认背景图：默认启用 background/t013c2f41e0a5020acc.jpg，空 path 可退出
- [ ] 【待开始】前缀键（老板键）可配置化（[详情](task/20261118-1941_前缀键可配置化.md)）
  - 设计统一 prefix 配置项（一处配置，全部绑定生效）
  - 配置加载与默认值（现默认 ctrl+shift）
  - 绑定生成改造，去除散落的硬编码前缀
- [ ] 【待开始】纯键盘选择与复制（终端面板 vim 可视模式）（[详情](task/20261118-1941_纯键盘选择与复制.md)）
  - 调研现有 vi_mode 与需求差距
  - 直接在终端面板进入选择模式（不依赖鼠标）
  - vim 风格移动/选择/复制（y 复制、v 可视选择）
  - 选中内容进剪贴/缓冲区，配合中键输出
- [x] 【已完成】TERM 固定为 xterm-256color 兼容 SSH 远程（[详情](task/20261118-2000_TERM固定为xterm-256color兼容SSH远程.md)）
  - 统一三处 TERM 设置点（rioterm main / librio pty / flatpak spawn），固定为 xterm-256color
  - 保留 COLORTERM=truecolor，本地会话回归验证（颜色、按键、vim/less/tmux）
  - ssh 远程验证（无 unknown terminal type；远端 tmux 因 Tc 丢失降 256 色为已知代价）
- [x] 【已完成】配置文件默认位置改为 tap 目录（[详情](task/20261118-2053_配置文件默认位置改为tap目录.md)）
  - 三平台 `config_dir_path()` 默认目录 `rio` → `tap`（Linux `~/.config/tap/config.toml`）
  - 派生路径自动跟随（config.toml / themes / log / watcher），`RIO_CONFIG_HOME` 覆盖保留
  - 迁移：旧 `~/.config/rio/config.toml` 需手动移动/复制到新位置
- [x] 【已完成】默认配置固化（字体/字号/窗口/透明度）（[详情](task/20261118-2059_默认配置固化字体字号窗口.md)）
  - 字号默认 18，字体默认 Fira Code SemiBold（family 覆盖全部 slot，缺失时 warning + 内置兜底）
  - 窗口默认 Maximized；默认 opacity 经用户复核定为 1.0（曾设 0.8 后改回，透明度留给用户按需配置）
  - 配合默认背景图实现无配置文件开箱即用；验证：config 88 测试 + sugarloaf 138 测试通过，待用户冷启动实测
- [x] 【已完成】构建产物改名为 tap（[详情](task/20261118-2110_构建产物改名为tap.md)）
  - `[[bin]] name` rio → tap（target/{debug,release}/tap），deb 资源路径同步
  - APPLICATION_ID "Rio" → "Tap"（WM_CLASS / Wayland app_id），desktop 文件改名 tap.desktop 并同步 Exec/Icon/StartupWMClass
  - Makefile release 目标产物路径更新；nix/TERM_PROGRAM/缓存目录等品牌残留未动（见任务文档）
