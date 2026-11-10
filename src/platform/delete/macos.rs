pub(super) use super::unix::identity;
use super::{directory_identity, entry_identity};
use std::path::{Component, Path, PathBuf};
pub(crate) use validate_macos_ancestors as validate_root;
pub(crate) fn validate_target(original_root: &Path, path: &Path) -> Result<(), String> {
    validate_macos_ancestors(original_root)?;
    validate_macos_ancestors(path.parent().ok_or("路径没有上级目录。")?)
}
pub(crate) fn validate_entry_path(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_subtree_root(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_macos_ancestors(path: &Path) -> Result<(), String> {
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                current.pop();
            }
            Component::CurDir => continue,
            component => current.push(component.as_os_str()),
        }
        directory_identity(&current)?;
    }
    Ok(())
}

pub(crate) fn move_to_trash(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let exact = path.to_str().ok_or("无法无损转换文件夹路径。")?;
    let url = NSURL::fileURLWithPath_isDirectory(
        &NSString::from_str(exact),
        entry_identity(path)?.directory,
    );
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| {
            format!(
                "移到废纸篓失败：{}。未尝试永久删除。",
                error.localizedDescription()
            )
        })
}
