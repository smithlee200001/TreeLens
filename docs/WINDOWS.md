# Windows 构建与验证

Windows 支持目标为 **Windows 10 1809 或更新版本 / Windows 11、x64、MSVC**。
这是开发构建目标，并不表示已经在所有 Windows 版本或显卡上完成实机验证。
macOS 构建方式不变。当前不提供 Windows ARM64、32 位或 MinGW 构建保证。

## 工具与运行环境

- 安装稳定版 Rust **1.92 或更新版本**，选择 `x86_64-pc-windows-msvc`。
- 安装 Visual Studio 2022 或 Build Tools 2022 的 **Desktop development with C++（使用 C++ 的桌面开发）**工作负载，包括 MSVC x64/x86 工具和 Windows 10/11 SDK。
- SDK 必须包含 `rc.exe`、`mt.exe` 和 **`fxc.exe`**。建议在 “Developer PowerShell for VS 2022” 中构建。
- GPUI 使用 Direct3D 11 / DXGI，运行需要可用的图形驱动和桌面会话。后端尝试 11.1、11.0、10.1 特性级别，不应把“装有 DirectX”理解为任意虚拟机、远程桌面或无显卡环境都能运行。
- 使用默认 MSVC 动态运行库构建；目标机器可能需要微软官方的 [Visual C++ Redistributable x64](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist)。ZIP 不捆绑系统 DLL 或运行库安装程序。

GPUI Kit 0.9.0 的默认后端是 `gpui-pre 0.3.11`，Windows 平台包是 `gpui-pre-windows 0.3.11`。`gpui-fast` 是另一项可选功能，本项目未启用。
Windows 后端 release 构建会调用 `fxc.exe` 编译 HLSL；只跑核心测试或 `cargo check` 不能替代完整 GUI release 构建。
若 SDK 查找失败，可显式设置已安装编译器的路径，例如：

```powershell
$env:GPUI_FXC_PATH = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64\fxc.exe'
```

把示例 SDK 版本换成自己安装的版本。无需单独安装旧版 DirectX SDK。

## 构建

在仓库根目录运行：

```powershell
rustup target add x86_64-pc-windows-msvc
cargo fmt --all -- --check
cargo test --locked --no-default-features --lib --target x86_64-pc-windows-msvc
cargo test --locked --features ui-tests --all-targets --target x86_64-pc-windows-msvc
cargo clippy --locked --features ui-tests --all-targets --target x86_64-pc-windows-msvc -- -D warnings
./scripts/bundle-windows.ps1
```

输出为 `dist/Tree-Lens-Windows-x64.zip`。解压后运行 `tree-lens.exe`；也可以传入扫描目录：

```powershell
& '.\Tree-Lens-Windows-x64\tree-lens.exe' 'C:\Users\你的用户名\Downloads'
```

打包脚本执行带 `--locked` 的完整 release GUI 构建，检查 EXE 的 PE 子系统确实为 Windows GUI，再收集项目说明、许可证和依赖许可证文件。它遵循 Cargo 的 target 目录设置，暂存目录保留供排查，不递归删除已有目录。开发 ZIP 未签名；正式对外发布前应完成签名和许可证审查。不要通过关闭系统安全功能来绕过警告。

GPUI 已默认提供含 `asInvoker`、Windows 10 兼容声明及 PerMonitorV2 DPI 设置的嵌入清单，本项目不再重复嵌入第二份清单。release GUI 不创建控制台；排查启动错误时可使用 `cargo run -- 'C:\path'` 的调试构建。

## 自动化验证范围

`.github/workflows/windows.yml` 在 `windows-2022` 上执行上述格式、核心测试、GPUI UI 逻辑测试、严格 Clippy 和真实 release 构建，并上传 ZIP。测试只应操作自己创建的临时夹具，不对用户文件执行删除或回收站操作。

**UI 逻辑测试通过不等于原生窗口通过实机验证。** GitHub 托管 runner 的桌面/显卡条件不能代表用户机器；发布前仍需运行下面的交互检查。提交至 GitHub 或手动运行工作流后会产生 Windows 构建结果。请核对工作流对应的提交，不把旧版本结果视为当前版本通过。

## Windows 实机验收清单

在非管理员的 Windows 10/11 x64 桌面会话中，使用一个新建的临时目录和可丢弃的文件：

1. 解压并双击 release EXE，确认正常开窗且无额外控制台；窗口关闭后进程退出。
2. 从目录选择器选取夹具，验证扫描、取消、刷新、排序、展开折叠和重复扫描。
3. 覆盖盘符根目录、含空格及中文的目录；可访问时检查 UNC 共享。确认拒绝访问的目录不会令整个扫描失败。
4. 验证符号链接、目录联接点不会造成循环遍历；如测试需要创建链接，仅使用自己的临时夹具和已允许的系统权限。
5. 在 100%、150%、200% 缩放和不同 DPI 的显示器间移动窗口，检查文字、鼠标命中和布局。
6. 仅对新建的可丢弃夹具测试删除确认的取消路径，以及确认后进入回收站和从回收站恢复。对不支持回收站的目标，确认应用不会静默改为永久删除。绝不拿现有用户文件验证。
7. 测试文件被占用、只读/无权限目录，以及网络路径异常。记录错误显示而不是只记录是否崩溃。
8. 在未安装 Rust/VS 的另一台机器验证运行库要求、解压后启动和资源加载；记录 Windows 版本、GPU、驱动版本、构建提交和结果。

## 已核查的上游依据

以下为本项目锁定版本的源代码入口，升级依赖时应重新检查：

- [GPUI Kit 0.9.0 Cargo 配置](https://docs.rs/crate/gpui-kit/0.9.0/source/Cargo.toml)：默认功能和 `gpui-pre` 后端。
- [GPUI 0.3.11 构建脚本](https://docs.rs/crate/gpui-pre/0.3.11/source/build.rs) 与 [Windows manifest](https://docs.rs/crate/gpui-pre/0.3.11/source/resources/windows/gpui.manifest.xml)。
- [Windows 后端构建脚本](https://docs.rs/crate/gpui-pre-windows/0.3.11/source/build.rs)：release shader 编译及 `GPUI_FXC_PATH`。
- [DirectX 设备创建](https://docs.rs/crate/gpui-pre-windows/0.3.11/source/src/directx_devices.rs)：DXGI factory 和 D3D 特性级别。

### 回收站失败路径的发布前验收

回收后端申请 `FOFX_RECYCLEONDELETE`，并在 `PreDeleteItem` 中拒绝未带 `TSF_DELETE_RECYCLE_IF_POSSIBLE` 的操作。它检查 `PerformOperations`、取消状态和每项完成结果，此回收路径不调用永久删除 API。另行选择并明确确认的「彻底删除」支持文件和文件夹，使用文件/空目录删除 API，不进入回收站。文件夹先在后台记录全部内容，确认后再次复核，再逐项从子项到父目录删除；不跟随内部链接或重解析点，不跨卷。执行失败可能留下部分删除结果，此时显示警告并刷新。彻底删除不是安全擦除。**当前只验证了回调逻辑与类型，尚未通过真实 Windows Shell 的失败路径验收。**

在隔离的测试账户或测试虚拟机中，使用可丢弃的小夹具覆盖回收站禁用、容量不足/超出容量、权限拒绝、文件占用、系统取消；必须确认原内容仍在且没有静默永久删除。不要为了测试修改用户正在使用的回收站设置。不要把 `PostDeleteItem` 的事后结果检查当成永久删除的预防证明。正式发布前记录系统版本和实际结果。
