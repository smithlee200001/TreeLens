//! Small platform vocabulary and native user-folder lookup.
use std::path::PathBuf;

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
    #[cfg(target_os = "linux")]
    {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        if !home.is_absolute() {
            return None;
        }
        if suffix.is_empty() {
            return Some(home);
        }
        let key = match suffix {
            "Downloads" => "XDG_DOWNLOAD_DIR",
            "Desktop" => "XDG_DESKTOP_DIR",
            "Documents" => "XDG_DOCUMENTS_DIR",
            _ => return None,
        };
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        match std::fs::read_to_string(config.join("user-dirs.dirs")) {
            Ok(contents) => xdg_user_dir(&contents, key, &home),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(home.join(suffix)),
            Err(_) => None,
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
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

/// Parse the quoted xdg-user-dirs format without executing shell code.
/// Only a leading $HOME is expanded. Invalid or absent entries are not guessed.
#[cfg(target_os = "linux")]
fn xdg_user_dir(contents: &str, key: &str, home: &std::path::Path) -> Option<PathBuf> {
    let value = contents.lines().rev().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        (name.trim() == key).then_some(value.trim())
    })?;
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    let (base, value) = if value == "$HOME" {
        return Some(home.to_path_buf());
    } else if let Some(value) = value.strip_prefix("$HOME/") {
        (Some(home), value.trim_start_matches('/'))
    } else {
        (None, value)
    };
    let mut parsed = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => match chars.next()? {
                escaped @ ('\\' | '"' | '$' | '`') => parsed.push(escaped),
                _ => return None,
            },
            '"' | '$' | '`' => return None,
            character if character.is_control() => return None,
            character => parsed.push(character),
        }
    }
    let path = base.map_or_else(|| PathBuf::from(&parsed), |home| home.join(&parsed));
    path.is_absolute().then_some(path)
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn localized_redirected_and_disabled_xdg_folders() {
        let home = Path::new("/home/test");
        assert_eq!(
            xdg_user_dir("XDG_DOWNLOAD_DIR=\"$HOME/下载\"", "XDG_DOWNLOAD_DIR", home),
            Some(home.join("下载"))
        );
        assert_eq!(
            xdg_user_dir(
                "XDG_DESKTOP_DIR=\"/mnt/data/desktop\"",
                "XDG_DESKTOP_DIR",
                home
            ),
            Some(PathBuf::from("/mnt/data/desktop"))
        );
        assert_eq!(
            xdg_user_dir("XDG_DESKTOP_DIR=\"$HOME\"", "XDG_DESKTOP_DIR", home),
            Some(home.to_path_buf())
        );
        assert_eq!(
            xdg_user_dir("XDG_DESKTOP_DIR=\"$HOME//nested\"", "XDG_DESKTOP_DIR", home),
            Some(home.join("nested"))
        );
    }

    #[test]
    fn xdg_parser_is_not_a_shell() {
        for value in [
            "relative",
            "$OTHER/path",
            "$(touch /tmp/never)",
            "`whoami`",
            "${HOME}/x",
            "$HOME/x\" # ignored",
        ] {
            let config = format!("XDG_DOWNLOAD_DIR=\"{value}\"");
            assert!(
                xdg_user_dir(&config, "XDG_DOWNLOAD_DIR", Path::new("/home/test")).is_none(),
                "{value}"
            );
        }
        assert!(xdg_user_dir(
            "# XDG_DOWNLOAD_DIR=\"/tmp/a\"",
            "XDG_DOWNLOAD_DIR",
            Path::new("/home/test")
        )
        .is_none());
        assert_eq!(
            xdg_user_dir(
                r#"XDG_DOWNLOAD_DIR="$HOME/a\$b\\c\"d""#,
                "XDG_DOWNLOAD_DIR",
                Path::new("/home/test")
            ),
            Some(PathBuf::from("/home/test/a$b\\c\"d"))
        );
    }
}
