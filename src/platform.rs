//! Small platform vocabulary and native user-folder lookup.
use std::path::PathBuf;

pub const TRASH_SUPPORTED: bool = cfg!(any(target_os = "macos", windows));
pub const TRASH_NAME: &str = if cfg!(windows) {
    "回收站"
} else {
    "废纸篓"
};
pub const REVEAL_LABEL: &str = if cfg!(windows) {
    "在资源管理器中显示"
} else {
    "在 Finder 中显示"
};
pub const DELETE_LABEL: &str = if cfg!(windows) {
    "删除文件夹…（移到回收站）"
} else {
    "删除文件夹…（移到废纸篓）"
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
    "APFS 共享块、快照与压缩可能影响分配量。此数值不代表可释放空间。"
};

/// Windows Known Folders respect localization, redirection and OneDrive moves.
/// Do not manufacture e.g. USERPROFILE/Desktop when Windows has moved it.
pub fn shortcut_path(suffix: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Profile,
            SHGetKnownFolderPath, KF_FLAG_DONT_VERIFY,
        };
        let id = match suffix {
            "" => FOLDERID_Profile,
            "Downloads" => FOLDERID_Downloads,
            "Desktop" => FOLDERID_Desktop,
            "Documents" => FOLDERID_Documents,
            _ => return None,
        };
        // SAFETY: Windows returns a NUL-terminated, CoTaskMem-allocated string.
        // Copy exact UTF-16 before freeing it. No lossy conversion or shell parsing.
        unsafe {
            let value = SHGetKnownFolderPath(&id, KF_FLAG_DONT_VERIFY, None).ok()?;
            let path = PathBuf::from(OsString::from_wide(value.as_wide()));
            CoTaskMemFree(Some(value.as_ptr().cast()));
            Some(path)
        }
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(suffix))
    }
}

pub fn home_dir() -> Option<PathBuf> {
    shortcut_path("")
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn known_folder_paths_are_absolute_without_creating_them() {
        for suffix in ["", "Downloads", "Desktop", "Documents"] {
            assert!(super::shortcut_path(suffix)
                .expect("known folder")
                .is_absolute());
        }
        assert!(super::shortcut_path("../outside").is_none());
    }
}
