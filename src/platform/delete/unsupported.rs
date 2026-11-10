#[cfg(unix)]
pub(super) use super::unix::identity;
use std::path::Path;
#[cfg(not(unix))]
pub(super) fn identity(_: &Path, _: &std::fs::Metadata) -> Result<super::Identity, String> {
    Err("此平台不支持安全的项目身份检查。".into())
}
pub(crate) fn validate_root(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_target(_: &Path, _: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_entry_path(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_subtree_root(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn move_to_trash(_: &Path) -> Result<(), String> {
    Err("此平台不支持移到废纸篓或回收站；没有删除任何文件。".into())
}
