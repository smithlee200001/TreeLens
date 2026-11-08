# Tree Lens 0.2 验证记录

验证日期：2026-11-03。执行环境为 Linux x86_64，Rust 1.99.1。源码中面向 macOS 的部分另做了 Apple Silicon 目标类型检查。

## 已通过

- `cargo fmt --all -- --check`
- `cargo test --locked --all-targets`：23 项测试通过（18 项核心/数据模型 + 5 项无窗口 GUI/状态测试）。
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo check --locked --target aarch64-apple-darwin --all-targets`：包括 macOS 文件夹选择窗口和 Finder 菜单代码，类型检查通过。
- `cargo test --locked --no-default-features`：18 项核心测试通过。
- `cargo run --locked --no-default-features --example scan -- src`：正常扫描源码目录，错误数为 0。
- `bash -n scripts/bundle-macos.sh`：脚本语法通过。

## 覆盖重点

- 目录累计值、空目录、`.app` 包作为目录处理。
- 硬链接逻辑大小按路径统计、分配空间去重。
- 稀疏文件使用分配块而非文件长度。
- 符号链接循环和失效链接不递归，文件/符号链接作为根时返回错误。
- 中途取消、扫描时目录消失、错误详情最多 500 条但总数完整。
- 按大小排序、大小写不敏感筛选及祖先路径显示。
- 无窗口 egui 生成界面和绘制网格：空状态、扫描结果、1180×780 和 820×560 两种尺寸。
- 扫描失败后重试、忙碌状态重复开始不创建第二个扫描。
- 100,000 个文件的树索引：普通平面目录不分配全部行缓存，筛选使用压缩位图。
- 100,000 个项目的 egui 界面只生成视口规模的绘制数据，而非所有项目。
- 不同折叠组合/筛选结果/排序的索引与参考遍历逐项一致。
- 10,000 次进度更新只保留最新一次，旧进度不会累积。
- 128 层深路径和非 UTF-8 文件名精确还原。

## 尚未验证

- 没有在真实 macOS 系统链接、启动或点击原生窗口。目标类型检查不等于 macOS 应用已运行。
- 未执行 Apple 原生选择窗口、Finder 跳转、系统隐私授权、APFS/iCloud 真实卷、Intel Mac 手动测试。
- 未实际生成/签名/公证 `.app`，也未运行仓库附带的远端 CI。
- Linux 环境无法建立可用图形显示，因此没有原生 GUI 截图；上述 GUI 测试为无窗口布局和绘制逻辑测试，不替代视觉验收。

在 Mac 上按 README 的运行和手动验收步骤完成这些检查后，再考虑公开分发。

## 内存基准

旧版与新版扫描同一 100,000 文件样本，各重复五次。核心扫描保留堆减少约 68%，展开树索引保留堆减少约 99.5%。具体原始字节数、测量范围及可复现脚本见 `benchmarks/README.md`。这些结果不是整个进程的 RSS 或 macOS GUI 实测。
