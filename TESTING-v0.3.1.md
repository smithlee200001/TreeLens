# Tree Lens 0.3.1 验证记录

日期：2026-11-08。本次执行环境：Linux x86_64，官方 Rust/Cargo 1.99.1。GPUI Kit 0.9.0、GPUI 0.3.11，具体依赖以 `Cargo.lock` 为准。

## 已执行并通过

- `cargo fmt --all -- --check`。
- `cargo test --locked --no-default-features --all-targets`：39 项核心测试通过。
- `cargo check --locked --all-targets`：完整 GUI 及常规示例类型检查通过。
- `cargo test --locked --features ui-tests --all-targets`：39 项核心测试、8 项 GPUI 交互测试通过。截图示例再次执行同样 8 项界面测试，共 55 次执行、47 个不同测试。
- `cargo clippy --locked --features ui-tests --all-targets -- -D warnings`：严格检查通过。
- `cargo check --locked --target aarch64-apple-darwin --no-default-features --lib`：Apple Silicon macOS 核心库与原生 Foundation 废纸篓调用的跨目标类型检查通过。**这不是 macOS 链接或运行测试，也不覆盖 macOS GUI。**
- `cargo run --locked --no-default-features --example scan -- src`：实际扫描源码目录成功。
- `bash -n scripts/bundle-macos.sh`：打包脚本语法检查通过。
- 与交付的 0.3 源码比较：`src/scanner.rs`、`src/tree.rs`、`src/session.rs`、`src/footprint.rs` 未改动。

## 新增测试范围

原有 26 项核心测试和 3 项 GUI 测试保留，新增：

### 13 项删除后端测试

使用测试进程创建的唯一临时目录，不调用真实废纸篓。覆盖单次调用、根目录与普通文件、无效节点、文件系统根、路径逃逸与循环父指针、符号链接及符号链接上级目录、非 UTF-8 路径、确认后替换目标/上级/根目录、确认后换成符号链接、目标消失、模拟系统错误无永久删除降级，以及非 macOS 明确拒绝。

### 5 项 GPUI 交互测试

- 最小窗口中显示完整目标确认框；Enter 不执行删除，取消、Esc、重新打开均安全；确认期间阻止新扫描。
- 模拟成功只执行一次，阻止执行中再次扫描与说明弹窗，重新扫描后选中根目录、清除筛选，旧节点消失。
- 模拟权限失败保留树与选择，显示原因，可重新确认，不自动重试。
- 根目录与过期扫描版本的确认不会启动删除。
- 真实右键事件打开文件夹菜单，根目录操作无效，非 macOS 菜单点击不会删除。

成功模拟仅将该测试自己的临时子文件夹重命名到合成存放位置，没有操作任何用户文件，也没有使用真实系统废纸篓。GPUI 使用官方无窗口事件与布局测试支持，不能替代 macOS 系统验收。

## 真实离屏渲染

`docs/preview-delete.png` 使用应用的真实 GPUI 离屏渲染器、系统现有 SwiftShader Vulkan 驱动生成。1080 × 760 逻辑像素、2 倍输出，数据为专门创建的 3 个子目录、3 个文件、约 1.75 MiB 的合成数据。

已检查中文显示、完整路径、删除范围说明、废纸篓恢复说明、取消和确认按钮及窗口边界。截图预览后端始终拒绝删除，未移动文件。

`docs/preview-wide.png` 与 `docs/preview-minimum.png` 保留为 0.3 基础界面的历史预览，不作为 0.3.1 删除功能验收证据。

## Linux 验证配置

使用工作区安装的官方 Rust，不改变系统 Rust。`RUST_FONTCONFIG_DLOPEN=1` 动态加载现有字体库；`LIBRARY_PATH` 指向工作区中现有 xcb/xkbcommon 系统运行库的链接。渲染使用现有 SwiftShader 驱动。此配置仅用于本次 Linux 开发检查，不需要在 Mac 复制。

## 未执行，不代表通过

- macOS 原生窗口启动与 GUI 编译/链接、Metal 渲染、Intel Mac 编译与运行。
- **真实 `NSFileManager` 移到废纸篓、Finder 恢复、权限拒绝、只读卷/外置卷/iCloud 等真实文件系统行为。**
- macOS 系统目录选择器、Finder 显示、操作系统拖放和隐私权限的端到端测试。
- `.app` 打包执行、签名、Apple 公证、远端 CI。
- 新版本整个 GUI 进程的内存、GPU 占用或帧率基准。

原生废纸篓 API 是路径接口。已在确认时记录、执行前复核根目录、上级和目标的设备/inode，但最后检查与移动之间仍有竞争窗口，不能保证抵御其他进程同时恶意替换路径。界面建议操作期间不要移动或重命名相关目录。

未推送 GitHub，未发布，未触发远端 CI。旧版本记录见 `TESTING-v0.3.md` 和 `TESTING-v0.2.md`。

## Mac 手动验收清单

只用专门建立的临时测试目录：

1. 扫描父目录，右键子目录，取消、Esc、关闭后验证所有内容仍在。
2. 再打开确认框核对路径，确认后检查 Finder 废纸篓、应用自动刷新及根目录选中。
3. 从 Finder 废纸篓恢复该临时目录，刷新应用验证恢复。
4. 验证根目录、文件、符号链接、挂载点不可删除；确认后替换目标应报错。
5. 测试系统权限拒绝和不可放入废纸篓的卷，不应出现永久删除或自动重试。
6. 验证最小/宽窗口、长中文路径、筛选状态及重复点击确认。
