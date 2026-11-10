//! Exact OS path input handling, independent of scan sessions.
use std::path::{Path, PathBuf};

/// Expand only `~`, `~/…` and (on Windows) `~\\…`, never another user's home or embedded tildes.
pub fn expand_home(input: &str) -> PathBuf {
    let home = crate::platform::home_dir();
    expand_home_with(input, home.as_deref())
}

pub(crate) fn expand_home_with(input: &str, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    if let Some(home) = home {
        if input == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = input.strip_prefix("~/") {
            #[cfg(windows)]
            return home.join(rest.trim_start_matches(['/', '\\']));
            #[cfg(not(windows))]
            return home.join(rest.trim_start_matches('/'));
        }
        #[cfg(windows)]
        if let Some(rest) = input.strip_prefix("~\\") {
            return home.join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(input)
}
