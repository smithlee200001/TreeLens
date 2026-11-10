# Tree Lens 0.5.1 文件与文件夹删除验证

日期：2026-11-10。基于 [GitHub main 7aca86e4](https://github.com/smithlee200001/TreeLens/commit/7aca86e4bbc90620486dbc17afc017c2941c7680)（0.5.0）。本记录涵盖发布前的源码验证；该阶段未推送远程、创建 PR、触发 GitHub CI、发布 release 或更改用户真实文件。后续 GitHub 发布和 CI 结果以对应 PR 记录为准。

## 本次实现

- 文件和文件夹都有「移到回收站 / 废纸篓…」和「彻底删除…」，在右键菜单与宽窗口详情栏可用。
- 彻底删除使用独立的危险操作确认，显示完整路径，明确不可撤销、不会进入回收站；文件夹明确包含全部子文件与子文件夹。Enter 不默认确认，取消/Esc/关闭不删除。
- 文件夹彻底删除先在后台采集内容快照，可用 Esc 取消准备。点击确认后复核全部子项，只按从子项到父目录的顺序删除已确认内容。内容变化、内部符号链接/重解析点、特殊文件、跨设备、挂载点和不可准确显示的名称会拒绝。
- 回收失败绝不升级为彻底删除。根目录受保护，单次请求绑定操作类型，目标及祖先身份在执行前复核。
- 成功后刷新统计、选择和地图。彻底删除中途失败时提示可能已部分删除，重新扫描，不自动重试。

## 已执行并通过

环境：Debian 13 x86_64 受限开发容器，Rust/Cargo 1.99.1。GUI 使用工作区内的链接库引用、RUST_FONTCONFIG_DLOPEN=1 和软件图形驱动；没有安装系统软件或修改系统设置。

- `cargo fmt --all -- --check`、`git diff --check`。
- `cargo test --locked --no-default-features --all-targets`：70 项核心测试通过，3 项默认忽略。
- `cargo test --locked --features ui-tests --all-targets`：70 项核心测试 + 17 项 GPUI 交互测试通过。截图示例还复用这 17 项 UI 测试，合计 104 次通过；不是 104 项独立测试。
- `cargo test --locked --no-default-features --lib linux_gio_ -- --ignored`：文件和文件夹 2 项真实 GIO 回收往返测试通过。它们使用独立的合成 HOME/XDG Trash，验证内容和元数据并恢复，只操作自己的夹具。
- `cargo clippy --locked --features ui-tests --all-targets -- -D warnings`。
- `cargo check --locked --no-default-features --tests --target x86_64-pc-windows-msvc`：Windows 核心及测试跨目标类型检查。
- `cargo check --locked --no-default-features --tests --target aarch64-apple-darwin`：macOS 核心、Foundation 后端及测试跨目标类型检查。
- shell 打包脚本语法检查。
- 真实 GPUI 离屏渲染并人工检查了宽窗口双按钮、文件回收确认、文件彻底删除确认、1080×760 逻辑尺寸下的文件夹彻底删除确认。截图见 docs/preview-file-*.png 和 docs/preview-folder-permanent.png；不是原生 macOS/Windows 桌面截图。

测试覆盖：普通文件/嵌套文件夹/空文件夹/Unicode、硬链接不误删其他路径、根目录/失效节点/畸形父链、目标与祖先替换、类型变化、内部链接/挂载点、确认后新增/删除/编辑子项、无快照拒绝执行、后台准备取消、Enter/取消/Esc/关闭、模式不可被第二次点击替换、重复确认、删除后刷新，以及模拟部分失败后的刷新和不重试。

## 未执行，不代表通过

- 本版本在真实 Windows/macOS 的 GUI、回收站成功/失败/恢复、签名及发布构建。跨目标检查不是原生链接或执行；Windows 新增的内部 junction 拒绝测试需原生 CI 执行。
- Debian 12/Ubuntu 22.04/24.04 的本版本原生运行。现有 Linux CI 矩阵已更新为运行文件及文件夹 GIO 测试，但未远程触发。
- 真实桌面 X11/Wayland、不同 DPI、完整目录选择器、可移动/网络磁盘、回收站满/禁用、真实权限失败与真实挂载操作。挂载保护通过合成挂载表验证。
- 默认忽略的 Unix socket 权限测试本次没有执行。
- 没有生成本版本原生安装包；本次交付是可构建源码及验证记录。

## 安全与性能边界

最终删除仍是路径接口。检查与实际操作之间有并发替换窗口，不能宣称对恶意并发文件系统修改具备原子安全保证。确认后请勿并行移动或重命名目标或祖先目录。递归删除可能因权限、设备变化或并发修改中途失败，已经删除的内容不可撤销。

永久删除文件夹对内部链接采取保守拒绝策略，不跟随链接、不自动转为其他删除方式。macOS 符号链接别名路径需改用真实路径。Windows 删除只支持本地固定磁盘。大型文件夹的全部内容检查与逐项保护会增加耗时和内存占用；准备过程在后台并可取消。

彻底删除不是安全擦除，其他硬链接、快照、备份或底层磁盘恢复不在本应用控制范围内。历史 0.5.0 验证记录保留在 [TESTING-v0.5.md](TESTING-v0.5.md)。
