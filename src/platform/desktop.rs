//! Native desktop actions and keyboard vocabulary. No explorer view state lives here.
use gpui_kit::App;
use std::path::Path;

pub const OPEN_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "cmd-o"
} else {
    "ctrl-o"
};
pub const REFRESH_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "cmd-r"
} else {
    "ctrl-r"
};
pub const SEARCH_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "cmd-f"
} else {
    "ctrl-f"
};
pub const QUIT_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "cmd-q"
} else {
    "ctrl-q"
};
pub const OPEN_TOOLTIP: &str = if cfg!(target_os = "macos") {
    "选择文件夹 · ⌘O"
} else {
    "选择文件夹 · Ctrl+O"
};
pub const REFRESH_TOOLTIP: &str = if cfg!(target_os = "macos") {
    "重新扫描 · ⌘R"
} else {
    "重新扫描 · Ctrl+R"
};

/// Reveal an item without opening special files or constructing shell commands.
pub fn reveal(path: &Path, is_directory: bool, cx: &mut App) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = (is_directory, cx);
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("无法打开 Finder：{error}"))
    }
    #[cfg(windows)]
    {
        let _ = is_directory;
        // GPUI uses the Windows shell's PIDL API, not shell command strings.
        if path.parent().is_none() {
            // Drive/UNC-share roots have no parent item to select.
            cx.open_with_system(path);
        } else {
            cx.reveal_path(path);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        // GPUI opens the path before invoking the portal. Only pass a directory:
        // opening a FIFO/device/file is unsafe; dangling links still reveal a parent.
        let directory = if is_directory {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        cx.reveal_path(directory);
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (path, is_directory, cx);
        Err("此平台尚不支持原生文件管理器显示。".into())
    }
}
