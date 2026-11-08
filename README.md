# Tree Lens 0.3.1 · GPUI Kit

Rust 编写的 macOS 磁盘空间分析工具。0.3.1 新增需要确认的文件夹删除菜单，删除只移到废纸篓。0.3 将 **eframe / egui 完全替换为 GPUI Kit 0.9.0**，使用原生 GPU 窗口与控件，不是网页套壳，也没有保留另一套旧 UI。

> **交付状态：已通过 39 项核心测试、8 项 GPUI 交互测试、严格 Clippy 和 Apple Silicon 删除后端跨目标类型检查；macOS 实机废纸篓与原生窗口验收尚未执行。** 具体验证范围见 [TESTING.md](TESTING.md)。此源码包不是签名安装包。

## 新界面

![Tree Lens 0.3.1 删除确认框](docs/preview-delete.png)

上图为新版删除确认框，由应用真实 GPUI 离屏渲染器生成，使用专门创建的示例目录，不是 macOS 实机截图。另见 0.3 基础界面历史预览：[宽窗口](docs/preview-wide.png)、[最小窗口](docs/preview-minimum.png)。

- 深蓝黑工作台、青绿主色、渐变统计卡片与彩色空间地图。
- 空间地图按真实分配量布局，点击目录可深入，支持返回根目录。最多保留 11 个直接子项和合并的剩余项，不复制完整扫描树。
- GPUI `uniform_list` 虚拟文件树：仅生成视口行，继续使用 0.2 的紧凑节点与按需树索引。
- 宽窗口显示快捷目录侧栏和项目详情栏；较窄窗口隐藏辅助侧栏，保持树列表宽度。
- GPUI Kit 原生输入、按钮、图标、上下文菜单、对话框和扫描动画。使用系统字体及其中文回退，不把整个 CJK 字体文件读入应用。
- macOS 系统目录选择器由 GPUI 提供，不再依赖 `rfd`。

## 保留的功能

文件夹选择、绝对路径/`~` 输入、文件夹拖放、后台扫描、最新进度合并、取消及部分结果提示、刷新、目录展开/收起、四种排序、180ms 防抖名称筛选、复制完整路径、Finder 显示与读取问题列表。

扫描器和虚拟树算法与 0.2 **逐字节一致**。扫描时不读取文件内容，不上传结果。文件夹移动到废纸篓必须由你单独确认。符号链接不展开，硬链接分配空间在同次扫描中去重，跨设备挂载点跳过。

## 删除文件夹

1. 扫描要处理的文件夹的**父目录**，等待扫描结束。
2. 在文件树中右键目标文件夹，选择 **删除文件夹…（移到废纸篓）**。宽窗口也可用右侧详情栏的「删除文件夹…」。
3. 核对确认框中的完整路径，再点击 **移到废纸篓**。取消、Esc 或关闭确认框不会删除；单按 Enter 不会默认确认。
4. 成功后自动重新扫描，清除筛选和旧选择，回到根目录并更新统计与空间地图。

**行为与安全边界：**

- 使用 macOS 原生废纸篓，移动的是整个文件夹及全部内容，包括筛选隐藏、未完整统计及扫描后新增的内容。
- 不永久删除，不清空废纸篓，不在失败时尝试其他删除方式。可在 Finder 的废纸篓中恢复；移入废纸篓不会立即释放磁盘空间。
- 当前扫描根目录、文件、符号链接、跨设备目录不能删除。想处理某个根目录，请改为扫描它的父目录。
- 执行前再次检查目录和上级路径身份。目标丢失、被替换、变成符号链接或无权限时显示错误，不自动重试。不提权、不改变权限。
- 非 UTF-8 路径明确拒绝，需在 Finder 中处理；不会用显示用的替换字符执行操作。
- 原生废纸篓使用路径接口，检查与实际移动之间仍有极短的竞争窗口，不能防御其他进程同时恶意替换路径。确认后请勿并行移动、重命名目标或其上级目录。
- Linux 开发版的删除菜单禁用；测试使用合成临时目录和模拟后端，不操作真实废纸篓。

## 在 Mac 上运行

当前 GPUI Kit 官方要求 **macOS 15 或更高版本**、Xcode Command Line Tools、**Rust 1.92 或更新版本**。这比 0.2 的系统要求更高。请先确认本机满足要求。

1. 如尚未安装 Apple 开发工具：`xcode-select --install`。
2. 从 [Rust 官方网站](https://rust-lang.org/tools/install/) 安装 Rust，重新打开终端后检查 `rustc --version` 和 `cargo --version`。
3. 在项目目录执行：

```sh
cargo run --locked --release
```

指定扫描目录：

```sh
cargo run --locked --release -- "$HOME/Downloads"
```

本包包含本次解析生成的新版 `Cargo.lock`，`gpui-kit` 固定为 `=0.9.0`。请保留锁文件并使用 `--locked`，不要用旧版 egui 锁文件替换。

快捷键：`⌘O` 选择文件夹、`⌘R` 刷新、`⌘F` 筛选、`Esc` 关闭对话框或取消扫描、`⌘Q` 退出。Linux 开发环境同时支持 Ctrl+O/R/F，但正式目标仍是 macOS。

## 打包

```sh
bash scripts/bundle-macos.sh
open "dist/Tree Lens.app"
```

脚本使用包内锁文件构建，仅在锁文件缺失时生成，最低系统设为 macOS 15。默认生成当前 Mac 架构；可传入 `aarch64-apple-darwin` 或 `x86_64-apple-darwin`，需先安装对应 Rust target。它不是 Universal Binary 构建器。

本机开发 `.app` 不包含 Developer ID 签名和 Apple 公证。公开分发需独立完成自己的签名、公证和真实 Mac 验收；项目不绕过 Gatekeeper。

## 验证

```sh
cargo fmt --all -- --check
cargo test --locked --no-default-features --all-targets
cargo check --locked --all-targets
cargo test --locked --features ui-tests --all-targets
cargo clippy --locked --features ui-tests --all-targets -- -D warnings
cargo run --locked --no-default-features --example scan -- src
```

核心测试不需要显示系统。GUI 完整编译仍需要平台依赖。Linux 要求及 macOS 最低版本参见 [GPUI Kit 官方安装文档](https://gpui-kit.com/docs/installation/)。仓库附带 macOS 15 CI 工作流，**本次没有上传或触发远端 CI**。

手动验收：选择目录 → 关闭选择器 → 再选择 → 展开/收起 → 排序/筛选/清除 → 点击地图深入/回根 → 复制/Finder → 扫描时取消 → 立即重复点击 → 完成后重扫 → 拖入目录 → 读取错误 → 说明弹窗 Esc/Tab → 最小/宽窗口 → 中文长路径 → 扫描中关闭窗口。

## 统计口径

- **磁盘占用**：Unix `metadata.blocks() × 512`，包括目录元数据。**逻辑大小**为非目录项目长度，目录为后代长度之和。
- **硬链接**：同一 `(device, inode)` 的非目录项目只贡献一次分配空间；逻辑大小按路径计数。贡献哪个路径由枚举顺序决定。
- **APFS**：克隆、共享块、快照、压缩和可清除空间不能靠 `stat` 精确归属。此值不等于删除后可释放空间，也不保证与系统储存空间一致。
- **iCloud**：不主动读取文件内容或请求下载，元数据访问仍由系统与提供商处理。
- **权限**：问题单独记录，不提权、不修改权限。需要时可由你自行在系统设置中授权完全磁盘访问，不必为普通目录扫描开启。
- **一致性**：扫描为实时尽力而为视图，不是原子快照；扫描中变化及路径替换竞争可能影响结果。
- **容量**使用 1024 进制的 KiB/MiB/GiB。文件数只计常规文件。
- **内存**仍随扫描项目数增长。0.2 的内存基准可在 [benchmarks/README.md](benchmarks/README.md) 复现；它不是 0.3 GPUI 整个进程的内存或 GPU 基准。新版不声称降低整体 GUI 内存或提高特定帧率。

## 结构与来源

- `src/main.rs`：GPUI Kit 启动、原生窗口、主题与生命周期。
- `src/app.rs`：GPUI Kit 界面、系统选择器、虚拟列表、菜单和对话框。
- `src/session.rs`：独立 GUI 的扫描会话、取消、进度与筛选。
- `src/deletion.rs`：文件夹身份验证、单次确认请求及 macOS 原生废纸篓后端。
- `src/footprint.rs`：有界面积图投影、布局测试。
- `src/scanner.rs` / `src/tree.rs`：原 0.2 扫描及低内存虚拟树。
- `scripts/bundle-macos.sh`：本地 `.app`；`.github/workflows/`：待运行的 macOS 检查。

采用 Longbridge 的 [GPUI Kit](https://github.com/longbridge/gpui-kit)，核对了 crates.io 发布的 0.9.0 源码及其 GPUI 0.3.11 API。官方入口是 `gpui_kit::*` 和 `gpui_kit::component`，不是另一个同名库。参考：[开始使用](https://gpui-kit.com/docs/getting-started/)、[任务与后台执行](https://gpui-kit.com/docs/task/)。第三方软件保留各自许可证。

## 可复现的界面检查

`ui-tests` 启用 GPUI Kit 官方测试支持。`examples/ui_snapshot.rs` 使用真实离屏 GPU 渲染器，可在有可用图形驱动的开发环境生成界面图，不以浏览器或手绘图替代。

```sh
cargo run --locked --features ui-tests --example ui_snapshot -- src preview.png
```

它等待约 1.5 秒让扫描进度汇入界面。大目录若尚未完成，截图会保留真实的扫描中状态。离屏检查不替代 macOS 系统选择器、Finder、窗口拖放和隐私权限验收。


### 删除确认框预览

使用测试特性生成确认框截图，不执行删除：

```sh
cargo run --locked --features ui-tests --example ui_snapshot -- /path/to/sample-root delete-preview.png 1080 760 --delete-dialog
```

示例目录需包含至少一个子文件夹。此预览替换为拒绝操作的模拟后端，即使点击确认也不会删除。实际 macOS 废纸篓、权限错误与恢复步骤仍需实机验收。
