# Tree Lens 0.5.0 Linux 验证记录

日期：2026-11-09。基线：[main 080f18a](https://github.com/smithlee200001/TreeLens/commit/080f18a9bbc08655f474011375328da68035cf2a)，已包含合并后的 Windows PR #1。此次没有推送 GitHub、触发远端 CI 或发布 release。

执行环境：Debian GNU/Linux 13 (trixie) x86_64 的受限开发容器，官方 Rust/Cargo 1.99.1。它不是完整 Debian 桌面，且不具有标准已安装系统库的 dpkg 所属关系/ABI 元数据。

## 已执行并通过

- `cargo fmt --all -- --check`、`git diff --check`。
- `cargo test --locked --no-default-features --all-targets`：55 项核心测试通过，2 项需显式启用的集成测试默认忽略。
- `cargo test --locked --features ui-tests --all-targets`：55 项核心测试及 9 项 GPUI 交互测试通过。截图示例复用同组 9 项测试再执行一次，共 73 次通过，不计默认忽略的测试。
- `cargo clippy --locked --features ui-tests --all-targets -- -D warnings`：完整 GUI/示例/测试严格检查通过。
- 显式运行 `linux_gio_round_trip_in_isolated_trash`：1 项真实 `/usr/bin/gio` 回收测试通过。只操作唯一合成目录，子进程隔离 HOME/XDG_DATA_HOME，验证 Trash/files 内容和 .trashinfo，再用同文件系统 rename 恢复。没有操作正常用户回收站，也不代表桌面文件管理器的恢复 UI 已验收。
- Linux 新测试覆盖 XDG 本地化/重定向/禁用目录和不执行 shell 的解析、挂载表转义/非 UTF-8/损坏输入、同设备目录及文件 bind mount 的模拟排除、硬链接归属不受跳过项目污染、符号链接上级拒绝、普通 `..` 路径规范化、悬空链接、不支持回收/缺少 GIO/假成功的错误处理。
- 原有稀疏文件、硬链接、Unicode/非 UTF-8 路径、取消/部分结果、目录身份替换、删除确认单次消费、取消/Esc/Enter/重复确认、100,000 节点虚拟树测试继续通过。
- `cargo clippy --locked --no-default-features --all-targets --target x86_64-pc-windows-msvc -- -D warnings`：Windows 核心跨目标类型检查通过。
- `cargo check --locked --no-default-features --lib --target aarch64-apple-darwin`：macOS 核心及 Foundation 后端跨目标类型检查通过。以上两项均不是 Windows/macOS 原生链接或 GUI 执行。
- 真实 GPUI 离屏渲染完成并检查最小尺寸界面/回收确认框；使用软件图形驱动及合成目录，预览后端不执行删除。
- 全部 shell 脚本语法、桌面入口 desktop-file-validate、四发行版 workflow YAML、源码空白检查通过。

本地 GUI 构建利用现有运行库，在工作区创建链接器所需的库链接，并设置 `RUST_FONTCONFIG_DLOPEN=1`。没有系统安装、提权或修改系统设置。常规发行版构建应安装 scripts/install-linux-deps.sh 列出的官方开发依赖；CI 不采用上述工作区兼容方式。

## 构建和打包

- 完整 x86_64 GNU/Linux 优化 release GUI 已构建，使用锁文件、thin LTO 和项目原有 release 配置。
- `bash scripts/bundle-linux.sh --tar-only` 已生成 Debian 13 amd64 开发 tar.gz，解包后的 `--version` / `--help`、动态库解析和 SHA-256 校验通过。包含桌面入口、图标、许可证及实际 ELF 依赖信息；它不是跨发行版通用包。
- `.deb` 默认使用 dpkg-shlibdeps 从实际 ELF 生成最低 ABI 依赖。在本容器缺少标准 dpkg 元数据时明确失败，不编造最低依赖版本。不能据此宣称 `.deb` 已验收。
- 包脚本的 `.deb`/tar 结构、root 所有权、桌面入口和校验和已通过合成 ELF 测试。合成测试产物不作为 Tree Lens 可运行程序交付。

## 尚未执行，不代表通过

- Debian 12、Ubuntu 22.04/24.04 原生构建、测试和安装。已加入官方发行版容器 CI 矩阵，但此次没有远端执行。
- 真实桌面 X11/Wayland 窗口、不同缩放、拖放、目录选择门户取消/重开、文件管理器集成和桌面恢复 UI。
- 真实 bind mount/跨设备/可移动盘、只读盘、回收站满/不可用、网络文件系统与桌面权限失败路径。挂载排除使用合成 mountinfo 测试，没有执行挂载操作。
- `linux_socket_is_not_opened_for_reading` 在本受限运行环境尝试建立 Unix socket 时被拒绝（EPERM）。该测试保留为显式启用，已配置在普通 Linux CI 容器运行，本地不声称通过。
- ARM64 Linux、32 位、非 glibc 发行版、完整 Windows/macOS 本版本桌面回归、签名及正式分发。

## 安全边界

扫描只读取元数据，使用 `st_blocks × 512` 和 `(device, inode)` 硬链接去重。Linux 挂载表在扫描开始时读取，扫描和回收均不是原子文件系统快照。Btrfs/ZFS 的共享块、压缩、快照、预留空间不能精确归属；统计不是可释放空间。

Linux 回收只在明确确认后调用固定 `/usr/bin/gio trash -- <path>`。执行前重新读取挂载表并检查目标/上级身份，拒绝符号链接、挂载点及包含挂载点的目录，无永久删除回退。路径检查与 GIO/桌面 API 使用路径之间仍存在并发替换窗口，不是对抗恶意并发修改的安全边界。

历史验证记录见 [TESTING-v0.4.md](TESTING-v0.4.md) 和更早记录。
