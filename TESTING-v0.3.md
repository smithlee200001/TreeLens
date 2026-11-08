# Tree Lens 0.3 验证记录

日期：2026-11-08。执行环境：Linux x86_64，Rust / Cargo 1.99.1 官方工具链；不是 macOS。GPUI Kit 0.9.0，GPUI 0.3.11，依赖版本以包内 `Cargo.lock` 为准。

## 已执行并通过

- `cargo fmt --all -- --check`。
- `cargo test --locked --no-default-features --all-targets`：26 项核心测试通过。
- `cargo check --locked --all-targets`：包括完整 GPUI 界面及常规示例的类型检查通过。
- `cargo test --locked --features ui-tests --all-targets`：26 项核心测试、3 项 GPUI 界面交互测试通过；截图示例复用界面模块，因此同样 3 项界面测试额外执行一次，共 32 次测试执行、29 个不同测试。
- `cargo clippy --locked --features ui-tests --all-targets -- -D warnings`：严格检查通过，无警告。
- `cargo run --locked --no-default-features --example scan -- src`：实际扫描成功，7 个文件，0 个读取问题。
- `bash -n scripts/bundle-macos.sh`：脚本语法通过。
- 对比 v0.2：`src/scanner.rs` 与 `src/tree.rs` 均逐字节相同。

### 26 项核心测试覆盖

原 18 项 scanner/tree/format_bytes 测试保留，包含目录汇总、硬链接去重、稀疏文件、符号链接循环、非 UTF-8 路径、深目录、取消、读取问题上限、树排序和筛选、十万节点按需索引。新增 6 项 Session 测试验证后台结果与筛选同步、线程断开恢复、取消期间防重复启动、销毁时取消、进度合并、180ms 防抖及主目录展开；新增 2 项空间地图测试验证空输入、有限数值、面积比例和不重叠。

旧的 egui 绘制测试随旧界面删除。新版界面由 GPUI 测试直接验证，不使用旧测试冒充。

### 3 项 GPUI 交互测试覆盖

1. 1080 × 760 最小窗口中的空状态与说明对话框，模拟点击打开，Esc 关闭。
2. 通过启动参数扫描真实临时目录，检查结果行、列宽、名称排序、展开/收起和保留的筛选输入实体。
3. 注入 100,000 个文件节点，确认可见树为 100,001 行，但实际生成的界面行少于 100，未全量绘制。

这些为 GPUI 官方无窗口测试环境中的布局及模拟事件测试，不等于操作系统原生窗口的人工验收。

## 实际渲染与视觉检查

使用 `examples/ui_snapshot.rs`、GPUI 官方离屏渲染器及系统现有 Chromium SwiftShader Vulkan 软件驱动，扫描一个真实创建的示例目录，再保存应用的渲染帧：

- `docs/preview-wide.png`：1440 × 920 逻辑像素，2 倍输出，含左右侧栏、统计卡片、空间地图与虚拟文件树。
- `docs/preview-minimum.png`：1080 × 760 逻辑像素，2 倍输出，辅助侧栏自动隐藏，文件树可滚动。

已目视检查中文字体、主要文字、地图比例、列对齐和窗口边界；修复了检查中发现的列表宽度及表头对齐问题，并重新运行测试和截图。截图使用 18 个文件、6 个子目录、141 MiB 的合成演示数据，不包含用户文件。小地图块会有意隐藏部分标签，以 tooltip 提供名称和大小。

软件驱动的离屏截图不是帧率、GPU 性能或 macOS 实机结果；系统字体、标题栏及窗口按钮在 Mac 上可能不同。

## Linux 验证环境说明

本次工具链安装在开发工作区，没有修改系统安装。Linux 构建使用 `RUST_FONTCONFIG_DLOPEN=1`，让已有 fontconfig 运行库动态加载；测试链接通过 `LIBRARY_PATH` 指向工作区中已有 xcb / xkbcommon 运行库的链接。渲染时 `VK_DRIVER_FILES` 指向系统已有 SwiftShader 驱动。

这是当前 Linux 环境的开发配置，不应复制到 Mac。常规 Mac 构建使用 README 命令和 GPUI Kit 官方依赖要求。未单独验证最低 Rust 1.92，当前实际测试版本为 1.99.1。

## 未执行，不代表通过

- macOS 目标编译与链接、真实原生窗口启动、Metal 渲染、Apple Silicon / Intel 双架构验证。
- 系统目录选择器、Finder 显示、操作系统文件拖放、原生剪贴板、macOS 中文字体回退及系统快捷键的端到端测试。
- APFS 克隆/快照/iCloud/完全磁盘访问与权限拒绝的 macOS 实机测试。
- `.app` 打包执行、Developer ID 签名、Apple 公证、远端 CI。
- 新版整个 GUI 进程的内存、GPU 占用、帧率与大型真实磁盘性能基准。

源码包附带 macOS 15 CI 配置和本地打包脚本，但本次没有推送 GitHub 或触发远端工作流。必须在真实 Mac 上完成以上验收后，才可称为经过 macOS 验收的安装包。`TESTING-v0.2.md` 和旧内存基准只记录旧版，不适用于新版整体 GUI 性能。
