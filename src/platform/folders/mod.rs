//! Native user-folder lookup selected once at the platform boundary.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::shortcut_path;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::shortcut_path;
#[cfg(not(any(windows, target_os = "linux")))]
mod unix_home;
#[cfg(not(any(windows, target_os = "linux")))]
pub use unix_home::shortcut_path;

pub fn home_dir() -> Option<std::path::PathBuf> {
    shortcut_path("")
}
