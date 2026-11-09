# Tree Lens 0.4.0 验证记录

日期：2026-11-09。执行环境：Linux x86_64，官方 Rust/Cargo 1.99.1。
基线：GitHub `smithlee200001/TreeLens` 的 `main`，提交 `6e8b60ddbcb115f54d76eb2aad192d4dca4bf47a`。开始修改前逐一核验了全部 37 个文件的 Git blob 哈希，与远端一致。

## 已执行并通过

- `cargo fmt --all -- --check`、`git diff --check`。
- `cargo test --locked --no-default-features --all-targets`：42 项核心测试通过。
- `cargo test --locked --features ui-tests --all-targets`：42 项核心测试和 9 项 GPUI UI 测试通过。UI 测试在截图示例中再执行一次，共 60 次测试执行、51 个不同测试。
- `cargo clippy --locked --features ui-tests --all-targets -- -D warnings`：Linux 完整 GUI/示例/测试严格检查通过。
- `cargo clippy --locked --no-default-features --all-targets --target x86_64-pc-windows-msvc -- -D warnings`：Windows 核心库、测试和非 GUI 示例跨目标类型检查及严格 lint 通过。**不是 Windows 链接、执行或 GUI 编译。**
- `cargo check --locked --target aarch64-apple-darwin --no-default-features --lib`：macOS 核心库和 Foundation 废纸篓接口跨目标类型检查通过。
- `cargo run --locked --no-default-features --example scan -- src`：实际源码目录扫描通过。
- `bash -n scripts/bundle-macos.sh`：Mac 打包脚本语法检查通过。Windows workflow YAML 已解析核查。
- `src/tree.rs`、`src/footprint.rs` 与基线一致；Unix 硬链接 key 仍为 16 字节，Windows 完整文件 ID key 为 32 字节，有测试防止改变 Unix 内存开销。100,000 节点虚拟列表测试继续通过。

## Windows 原生核心测试

GitHub Windows Server 2022 x64/MSVC 已在提交 `25197f8bd508a99f5adf619aa1575d4f952c4006` 运行并通过全部 53 项核心测试，包括下列 Windows 专用夹具。证据：[Windows CI](https://github.com/smithlee200001/TreeLens/actions/runs/42612087760)。该轮 GUI 已完成编译/链接；一个界面测试因窗口关闭按钮与弹窗关闭按钮的 ID 重名失败，现已将选择器限定到弹窗内。最终 GUI/release/打包结果以 [PR #1 最新检查](https://github.com/smithlee200001/TreeLens/pull/1) 为准。

## Windows 专用测试覆盖（原生核心已通过）

- 实际 NTFS 夹具：普通文件分配量、硬链接、稀疏文件、压缩文件、中文/emoji 路径、文件独占数据锁不影响元数据读取；注入属性查询拒绝时项目仍可见且报问题。
- 联接点不跟随、根目录/上级含联接点拒绝、扫描过程中换成联接点拒绝，以及有歧义的驱动器相对路径拒绝。
- 系统已知文件夹路径、`~\\` 展开和 UNC/盘符路径保留。
- 文件夹回收请求：根/普通文件/无效节点/路径逃逸拒绝，确认后目标、上级、根替换和目标消失拒绝，Unicode 路径保真及无法准确显示的非 Unicode 名称拒绝。
- 模拟 Shell 回调：拒绝直接删除，失败/跳过 HRESULT、缺少回收目标及整体完成错误不可报成功。

测试仅操作自己建立的唯一临时夹具。Windows 联接测试使用无需管理员的 `mklink /J`，稀疏/压缩测试要求 NTFS，遇到不支持的文件系统明确失败，不静默略过。所有自动化删除测试使用 mock，不进入真实回收站。

## 未执行，不代表通过

- Windows release HLSL 编译、PowerShell ZIP 打包脚本及 ZIP 运行不在上述本地检查范围内；远端构建状态以 PR 的最新 CI 为准。
- Windows 实际窗口、Direct3D 渲染、系统目录选择器、资源管理器显示、文件夹拖放、100%/150%/200% DPI、非英文或重定向系统文件夹端到端检查。
- **Windows 实际回收站成功/恢复、被禁用/容量不足/超大项目/权限或锁定失败路径。** 必须在可丢弃测试环境验证，详见 [WINDOWS.md](docs/WINDOWS.md)。回调单元测试不能替代 Shell 失败路径实测。
- 本版本 macOS 原生 GUI/Metal、Finder/废纸篓实机验收、Intel Mac、签名/公证。
- Windows ARM64、32 位、MinGW；性能/全进程内存/GPU 基准。
- 此记录的本地检查阶段尚未执行远端 Windows/macOS CI。上传后的实际状态以对应提交的 GitHub Actions 和草稿 PR 为准；没有发布正式二进制。

## 重要边界

Windows 删除只支持本地固定磁盘，拒绝网络共享、可移动盘、设备路径、重解析点/联接点、跨卷和 Shell 可能重新解释的尾随点/空格/流名称。失败不主动调用另一种删除方式，不提权，不清空回收站。

扫描仍是实时尽力而为视图，不是原子快照。Win32 路径访问及 Shell 回收操作在检查和使用之间存在竞争窗口，尤其是其他进程替换中间目录时；不可描述为对抗恶意并发修改的安全边界。

Linux GUI 测试使用现有系统动态库，工作区内提供 xcb/xkbcommon 链接，并设置 `RUST_FONTCONFIG_DLOPEN=1`。没有改动系统安全设置，也没有在 Linux 冒充 Windows 实机测试。

旧版记录见 [TESTING-v0.3.1.md](TESTING-v0.3.1.md)、[TESTING-v0.3.md](TESTING-v0.3.md)、[TESTING-v0.2.md](TESTING-v0.2.md)。
