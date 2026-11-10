# 功能模块与平台适配层重构验证

日期：2026-11-10。基线：GitHub main `231aa923eac1f09b16a2cd31cc864ae57f29ae40`，PR #4 已合并。以该提交的 59 个 Git blob 校验建立独立工作副本，没有使用旧 0.3.x 本地源码作为修改基线。

## 本次范围

按 `features/scan`、`features/delete`、`features/explorer` 组织功能，平台原生能力集中到 `platform/`，UI 拆分为视图、控制器、主题与测试。原公开模块使用兼容导出。没有修改版本号、依赖、Cargo.lock、打包脚本或远端工作流。详细说明见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 已执行并通过

环境：Debian 13 x86_64 受限容器，官方 Rust/Cargo 1.99.1。GUI 验证设置 `RUST_FONTCONFIG_DLOPEN=1`；通过工作区内链接使用系统已有的 libxcb、libxkbcommon、libxkbcommon-x11 运行库。首次 GUI 链接因缺少无版本开发链接失败，补齐工作区链接后通过；没有安装系统软件或更改系统配置。

- 修改前 `cargo test --locked --no-default-features --all-targets`：81 项核心测试通过，3 项默认忽略。
- 最终 `cargo fmt --all -- --check` 与 `git diff --check`：通过。
- 最终 `cargo test --locked --no-default-features --all-targets`：82 项核心测试 + 3 项架构/兼容测试通过；3 项原有默认忽略。
- `cargo check --locked --features ui-tests --all-targets`：通过。
- 最终 `cargo test --locked --features ui-tests --all-targets`：82 项核心 + 21 项 GPUI 交互 + 3 项架构测试通过。UI 已由库共享，二进制/截图示例不再重复运行同组交互测试。
- `cargo test --locked --no-default-features --lib linux_gio_ -- --ignored`：额外 2 项通过。仅使用唯一合成 HOME/XDG_DATA_HOME，测试自己的文件与目录回收往返，不访问用户正常回收站。
- `cargo clippy --locked --no-default-features --all-targets -- -D warnings`：通过。
- `cargo clippy --locked --features ui-tests --all-targets -- -D warnings`：通过。
- `cargo check --locked --no-default-features --tests --target x86_64-pc-windows-msvc`：通过，无警告。
- `cargo check --locked --no-default-features --tests --target aarch64-apple-darwin`：通过，无警告。
- `cargo run --locked --no-default-features --example scan -- src`：通过，40 个源文件、12 个子目录、0 项读取问题。
- 最终源码清单 SHA-256、ZIP 校验和补丁在基线副本上的 `git apply --check` 与文件树一致性检查：通过。

新增测试明确让 Linux 排除的文件挂载先于普通硬链接被检查，验证它不会占走真实文件的空间归属。架构测试检查生产业务/UI 中的本机 API 或 OS 分支、平台对功能/视图的反向依赖，以及旧公开模块与新类型的一致性。

代码复查确认 Linux/Windows 文件系统原语保持一致，原生回收站及 Windows 回调逻辑保留；删除请求仍为绑定模式的单次确认，失败不回退或重试。扫描适配层复用已有子路径，避免为每个项目新增一次完整路径分配。未进行新的性能基准，因此不宣称性能提升。

## 环境限制与未验证项

- 显式运行默认忽略的 `linux_socket_is_not_opened_for_reading`：**未通过**。夹具在 `UnixListener::bind` 创建 Unix socket 时收到 `Operation not permitted`，尚未进入待测扫描流程。没有绕过权限限制或修改安全设置；该项仍需允许 Unix socket 的正常 Linux 主机/CI 验证。
- 未运行 Windows/macOS 原生 GUI、真实 Shell/Finder 回收站或实机屏幕/DPI/显卡驱动验收。跨目标类型检查不等同于本机链接和运行。
- 未为本次修改推送 GitHub、创建 PR、触发新的远端 CI、生成平台安装包、签名或发布。此前 main/PR 的 CI 成功不能替代本次提交的 CI。
- GUI 交互测试验证保留视图、取消、重复操作、代次/旧回调保护与删除确认；不把无头测试视为真实显示器的闪屏保证。
- 原扫描/删除的活跃文件系统竞争窗口、全根目录只读核对和 O(N) 结果映射成本保持不变。

## 后续验证

本地应用补丁后可运行上面的格式、核心、GUI、lint 命令。之后在原 Windows、macOS、Debian/Ubuntu 工作流运行完整测试与原生打包，并按 TESTING-root-switch.md / TESTING-delete-refresh.md 对合成目录进行人工验收。
