# 更新记录

## 0.5.3 · 切换扫描目录时保留视图 (2026-11-10)

- 选择新扫描目录和手动重新扫描时，旧列表、地图与统计持续可见，完成后再一次性切换。
- 在固定高度的标题说明行同时标注「当前显示」与「正在扫描」路径，长路径可悬停查看；新扫描期间禁用删除。
- 取消、错误、扫描线程断开或启动失败保留原目录、展开、选择、筛选、地图与滚动位置；重复启动受单工作线程保护。
- 取消优先于尚未应用的已完成结果；成功切换后失效旧节点回调，重置新根目录导航，首次扫描期间输入的筛选词不会被清空。
- 包含 0.5.2 全部删除刷新修复，见 TESTING-root-switch.md。

## 0.5.2 · 删除后保留视图 (2026-11-10)

- 删除成功、部分失败或结果不确定时，不再清空结果和重建整页加载界面。
- 保留展开、选择、筛选、排序、空间地图目录和列表像素滚动锚点；已删除选择退回最近保留的上级。
- 后台只读核对全根目录，正确重新归属硬链接和反映外部变化，完成后一次性替换列表/统计；不是只扫描被删目录。
- 核对失败/取消保留旧视图并标记过期，禁用再次删除，要求重新扫描。原删除确认与安全后端不变，不自动重试。
- 新增持续可见帧、滚动/导航保持、未知删除结果和状态映射回归测试。

## 0.5.1 · 文件回收与彻底删除 (2026-11-10)

- 文件和文件夹新增「移到回收站 / 废纸篓…」「彻底删除…」右键菜单和宽窗口详情按钮。
- 彻底删除使用独立的不可撤销确认，展示完整路径；Enter 不确认，取消/Esc/关闭安全退出。
- 后端保留路径/身份/类型/根目录/符号链接/挂载检查；回收失败不升级为永久删除。文件夹彻底删除前后台核对内容，确认后再次复核，只逐项删除已确认内容；拒绝内部链接和挂载点。
- 单次确认绑定操作类型；成功或彻底删除部分失败后重新扫描并更新选中项和统计。
- 新增文件级安全、失败、硬链接、真实隔离 GIO 回收和 GPUI 交互测试。

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
