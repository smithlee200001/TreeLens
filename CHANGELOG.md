# 更新记录

## 未发布 · 点击目录立即切换 (2026-11-10)

- 切换到不同扫描根目录时，立即更新目标路径、清除旧树/地图/统计，展示目标目录加载状态；不再等待扫描结束才切换。
- 扫描期间可选择另一个目录，以最后选择为准；单工作线程协作取消旧任务，丢弃旧进度及已排队结果。重复点击同一活动目标不重置筛选。
- 切换失败留在所选目录显示错误，取消只保留该目录已扫描部分，不自动回到原目录。刷新当前目录与删除后核对仍保留列表和导航。
- 删除准备、确认、执行与核对期间仍阻止目录切换，原删除安全后端不变。详见 TESTING-immediate-switch.md。

## 未发布 · 删除文字稳定与详情裁切修复 (2026-11-10)

- 删除确认框直接显示/关闭，不再让整窗背景随遮罩淡入淡出；模态输入拦截、独立确认、Enter 不删除和取消保护保持。
- 保留视图期间使用静态状态指示，消费但不为不可见的扫描进度重绘整页；完成、错误和筛选仍正常更新。
- 短暂忙碌时保持导航文字颜色和路径外观，刷新/取消使用固定宽度，避免邻近文字跳位；按钮仍保持禁用语义和控制器保护。
- 项目详情改为独立滚动正文，文件名和完整路径换行显示，不再被挤压成残缺文字；长内容下操作按钮可滚动到达。
- 新增动画模式交互、前后帧像素比较和长文字布局回归。基线 main `a862703b`；验证及 macOS 实机限制见 TESTING-text-stability.md。

## 未发布 · 功能模块与平台适配层重构 (2026-11-10)

- 基于 PR #4 已合并的 main `231aa923`，保留 0.5.3 扫描、删除与保留视图行为。
- 按 scan、delete、explorer 分组功能；平台层集中原生元数据、回收站、系统目录、路径与桌面集成。
- 共用一份扫描流程，平台原生细节编译期选择；保留原有删除保护、单次确认与失败策略。
- UI 拆分视图、控制器、主题和测试；二进制/截图示例复用库中的 UI，不重复编译/运行同组交互测试。
- 旧公开模块路径保持兼容；新增平台边界和排除挂载项先于硬链接去重的回归检查。
- 不更改依赖、版本号、锁文件或打包流程。详见 docs/ARCHITECTURE.md 与 TESTING-architecture.md。

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
