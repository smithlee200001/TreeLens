//! Existing HOME-based folder behavior for macOS and other non-Windows hosts.
use std::path::PathBuf;

pub fn shortcut_path(suffix: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(suffix))
}
