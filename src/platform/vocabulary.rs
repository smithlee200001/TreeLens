//! Platform-specific presentation vocabulary, without GUI dependencies.

pub const TRASH_SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "linux", windows));
pub const TRASH_NAME: &str = if cfg!(any(windows, target_os = "linux")) {
    "回收站"
} else {
    "废纸篓"
};
pub const REVEAL_LABEL: &str = if cfg!(windows) {
    "在资源管理器中显示"
} else {
    if cfg!(target_os = "linux") {
        "在文件管理器中打开目录"
    } else {
        "在 Finder 中显示"
    }
};
pub const DELETE_LABEL: &str = if cfg!(any(windows, target_os = "linux")) {
    "移到回收站…"
} else {
    "移到废纸篓…"
};
pub const SHORTCUT_HELP: &str = if cfg!(target_os = "macos") {
    "快捷键：⌘O 选择文件夹 · ⌘R 刷新 · ⌘F 筛选 · Esc 关闭说明或取消扫描。"
} else {
    "快捷键：Ctrl+O 选择文件夹 · Ctrl+R 刷新 · Ctrl+F 筛选 · Esc 关闭说明或取消扫描。"
};
pub const ALLOCATION_HELP: &str = if cfg!(windows) {
    "磁盘占用来自 Windows 分配空间元数据（稀疏/压缩文件使用压缩大小）；逻辑大小为文件长度。硬链接分配空间按卷和文件 ID 去重。"
} else {
    "磁盘占用来自已分配块数 × 512；逻辑大小为文件长度。硬链接分配空间在同次扫描中去重。"
};
pub const STORAGE_CAVEAT: &str = if cfg!(windows) {
    "NTFS/ReFS 共享块、去重与快照无法精确归属；重解析点和云占位文件跳过。此数值不代表可释放空间。"
} else {
    if cfg!(target_os = "linux") {
        "Btrfs/ZFS 共享块、压缩、快照及文件系统预留空间无法精确归属；子挂载点跳过。此数值不代表可释放空间。"
    } else {
        "APFS 共享块、快照与压缩可能影响分配量。此数值不代表可释放空间。"
    }
};
