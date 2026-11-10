# 按功能划分 + 平台适配层

本次重构基于 `main` 的 `231aa923eac1f09b16a2cd31cc864ae57f29ae40`，包含已合并 PR #4 的 0.5.3 保留视图修复。它改变代码组织和依赖方向，不新增删除方式、不改变扫描统计口径，也不发布新版本。Cargo 依赖、锁文件和系统要求保持不变。

## 目录与职责

```text
src/
├── main.rs                       # 参数、应用启动、窗口生命周期
├── lib.rs                        # 公共入口和旧 API 兼容导出
├── features/
│   ├── scan/
│   │   ├── model.rs              # 紧凑节点、精确路径、结果与进度
│   │   ├── service.rs            # 迭代遍历、取消、问题汇总、排序
│   │   └── tests.rs
│   ├── delete/
│   │   ├── mod.rs                # 单次确认请求、目标校验、执行流程
│   │   ├── snapshot.rs           # 永久删除前的子树快照和内容复核
│   │   └── tests.rs
│   └── explorer/
│       ├── session/              # 后台扫描、筛选、结果替换、状态映射
│       ├── tree/                 # 虚拟行索引、展开、排序
│       └── footprint.rs          # 有界空间地图投影
├── platform/
│   ├── scan/                     # Unix/Windows 元数据、挂载与分配量适配
│   ├── delete/                   # Linux/macOS/Windows 身份/路径/回收站适配
│   ├── fs/                       # Linux 挂载表、Windows 只读元数据句柄
│   ├── folders/                  # XDG、Windows Known Folders、HOME
│   ├── paths.rs                  # 平台路径输入规则和 ~ 展开
│   ├── desktop.rs                # 文件管理器、快捷键、提示文字（gui 特性）
│   └── vocabulary.rs             # 平台术语与统计说明
└── ui/
    ├── mod.rs                    # 界面状态和视图渲染
    ├── controller.rs             # 用户操作、异步协调、刷新与删除确认
    ├── theme.rs                  # 主题、颜色、快捷键绑定
    └── tests.rs                  # GPUI 交互回归

tests/architecture.rs             # 层间依赖守卫、旧 API 兼容检查
```

`features` 按用户要做的事组织；`platform` 按能力组织，每个能力内部选择本机实现。不要复制一套 Windows 应用、一套 macOS 应用或每个平台各写一份遍历算法。Linux 的 bind mount 和 Windows 的 128 位文件标识也不能为了接口统一而丢弃。

## 依赖方向

```text
main / screenshot example → ui → features → platform → OS / std / native libraries
                              ↘ platform::desktop（GUI 系统集成）
features::explorer → features::scan
features::delete  → features::scan（只读取扫描模型）
```

- `platform` 不反向引用 `features` 或 `ui`。适配器只接收路径、元数据、布尔值等必要输入，返回自身的小型数据结构。
- 业务与 UI 生产代码不直接出现 `std::os`、Windows/Foundation API、OS 条件编译或启动系统命令。测试可以使用本平台原生 API 构造夹具。
- 普通 `std::fs` 的目录枚举、文件删除仍留在对应功能中。它们本身已经是 Rust 的跨平台 API；平台层负责额外的本机身份、挂载、链接和原生回收站语义。
- `platform::desktop` 随 `gui` 特性编译，允许调用 GPUI 的系统文件管理器接口。核心扫描、删除、会话和树索引无需 GUI。
- `cfg` 只在平台层选择实现。没有运行时 OS 判断、动态插件、依赖注入容器或每个功能一个 Cargo crate。

`tests/architecture.rs` 对关键源码边界作回归检查，同时检查旧导出类型的一致性。这是轻量源码守卫，不是完整 Rust 依赖分析器；Rust 的模块可见性和 `--no-default-features` 构建提供另一层约束。

## 扫描边界

`features::scan::scan` 创建编译期选中的 `NativeAdapter`，共用一份 `scan_with_adapter` 遍历。内部 `Adapter` 提供三个小接口：

1. `start`：验证/规范化根路径，准备本机挂载信息与根设备身份。
2. `validate_directory`：进入目录前重新检查类型、设备及本机路径约束。
3. `inspect_entry`：获得类型、逻辑/分配量、重复硬链接标记、是否计数/下钻以及问题说明。

接口使用泛型静态分发；它不是公开插件 API。Windows 保留原来的查询函数测试接缝。Unix 硬链接键仍是两个 `u64`，Windows 保留卷标识和完整 `u128` 文件标识。Linux 排除挂载项发生在硬链接去重之前，避免挂载文件抢走真实文件的空间归属。

遍历仍为非递归栈、精确原生路径、有界问题列表、饱和累加、最终反向聚合和原有排序。文件内容不读取，符号链接/重解析点不下钻，取消仍返回已发现的部分结果。共享块、克隆与活跃文件系统的一致性限制没有改变。

## 删除边界

`features::delete::DeletionRequest` 继续是字段私有、不可克隆、执行时被消费的一次性确认快照。模式在准备时绑定；准备、显示确认和执行分开。文件夹永久删除的内容快照在后台准备，执行前重新核对，逐项执行时继续检查。

平台适配器负责身份和路径差异：

- macOS：设备/inode 身份、祖先检查、Foundation 废纸篓接口。
- Windows：不跟随重解析点的身份查询、固定本地磁盘/路径限制、原生 Shell 回收站及原进度回调约束。
- Linux：设备/inode、祖先路径、包含 bind mount 的挂载检查，以及 `/usr/bin/gio trash --`。

共同业务规则仍在功能层：拒绝扫描根、越界/损坏节点、非普通项目、身份或内容变化、无法准确显示的名称；永久删除失败可能部分完成，必须明确报错。回收失败不转成永久删除，不自动重试、不提权、不改权限。测试仅处理自行创建的合成数据。

这些检查没有把路径接口变成原子操作。检查与实际文件系统调用之间的并发替换窗口仍然存在，不应宣称能抵抗恶意并发修改。

## 刷新与 UI

`Session` 保持 GUI 独立，扫描线程只发送进度和结果。`ui::controller` 负责定时轮询和通知 UI，不把窗口对象交给工作线程。

- 切换根目录时保留旧树、地图和统计，明确展示旧显示路径与新扫描路径；成功后一次性切换，失败或取消保持原状态。
- 删除结束后保留视图，在后台只读核对完整根目录，完成后映射展开、选择、筛选、地图和滚动锚点。失败则保留旧视图并标记过期。
- 单工作线程、结果代次、旧回调保护和取消已排队结果的优先级都保留。
- 完整核对仍有 O(N) 扫描/结果应用开销；这不是增量扫描或性能改写。

二进制与 `ui_snapshot` 现在共同使用库中的 `ui`，不再通过 `#[path]` 各编译一份应用模块。21 个 GPUI 交互测试只运行一份。截图仍使用真实 GPUI 离屏渲染，而非浏览器模型。

## 兼容与扩展

旧入口 `tree_lens::{scanner,deletion,session,tree,footprint}` 是新模块的 `pub use`，没有复制类型或实现。现有使用方可继续编译；新代码推荐使用 `tree_lens::features::scan`、`features::delete` 和 `features::explorer`。

添加新功能时先放入对应 feature；只有 OS 行为确有差异时再扩展平台接口。添加平台时补充 scan/delete/folders/desktop 能力，并为链接、挂载、身份变化、取消和失败路径增加测试。新平台不能提供可靠身份或回收能力时应明确返回不支持，不用逻辑文件长度冒充已知平台的物理分配量，也不以永久删除替代回收。

验证命令与本次实际结果见 [TESTING-architecture.md](../TESTING-architecture.md)。原生打包流程和 macOS、Windows、Debian/Ubuntu CI 继续沿用原脚本。
