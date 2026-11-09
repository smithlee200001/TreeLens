# 更新记录

## 0.5.0 · Debian / Ubuntu support (2026-11-09)

- Add Linux desktop integration, XDG localized/redirected user folders, Ctrl shortcuts and correct platform copy.
- Preserve Unix allocated-block and hardlink accounting, skip same-device bind mounts, and reject symlink ancestors on Linux.
- Add explicit-confirmation FreeDesktop Trash through GIO, fail closed on missing mount information, reject mount points/contained mounts, and never fall back to permanent deletion.
- Add Debian 12/13 and Ubuntu 22.04/24.04 CI, native distro-specific .deb/tar packaging, desktop entry, icon, dependency and validation documentation.
- Retain merged Windows/macOS support, compact tree storage and virtualized UI. See TESTING.md for evidence and untested targets.

## 0.4.0 · Windows support (2026-11-09)

- Add Windows x64 MSVC target, native folder picker, Explorer reveal, Known Folder shortcuts and Ctrl keyboard shortcuts.
- Account for Windows allocation, sparse/compressed files and hardlinks using metadata-only handles; skip reparse points and junctions.
- Add explicit-confirmation Windows Recycle Bin backend with identity checks, per-item result validation and permanent-delete veto. Restrict deletion to fixed local drives.
- Preserve macOS behavior, compact nodes, virtual list rendering and filtering/cancellation semantics.
- Add Windows tests, CI and portable development ZIP packaging. Native Windows build/runtime validation is pending; see TESTING.md.

## 0.3.1 · 文件夹删除菜单

- 在文件树的文件夹右键菜单加入「删除文件夹…（移到废纸篓）」，宽窗口详情栏也有入口。
- 确认框显示完整目标路径，并说明会移动整个文件夹及全部内容。Enter 不会默认确认，取消和 Esc 不执行操作。
- macOS 使用原生 `NSFileManager` 废纸篓接口；不永久删除、不清空废纸篓，不在失败时降级为永久删除。
- 后台执行期间阻止新扫描和重复删除；成功后重新扫描原根目录，清除筛选与旧选择，更新统计和地图，选中根目录。
- 扫描根目录、文件、符号链接、跨设备目录和无效路径禁止删除；确认时记录目录身份，执行前复核目标及上级目录。
- 明确处理权限、路径变化、后台线程和废纸篓失败，不自动重试。非 UTF-8 路径需在 Finder 中处理。
- 保留 GPUI Kit 0.9 界面与原有扫描、虚拟树功能。删除仅在 macOS 可用。

验证范围及未验收项见 `TESTING.md`。源码包不是经过签名、公证的 Mac 安装包。
