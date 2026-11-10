//! Native Known Folders preserve localization and redirection.
use std::path::PathBuf;

pub fn shortcut_path(suffix: &str) -> Option<PathBuf> {
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

#[cfg(test)]
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
