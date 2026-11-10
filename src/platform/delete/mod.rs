//! Native identity, path-safety and recoverable-deletion adapters.
//! This layer knows nothing about scan models or confirmation policy.
//!
//! The selected adapter validates the original root before canonicalization,
//! rechecks native ancestor/mount constraints for the target, and supplies exact
//! entry identities. These checks do not mutate the filesystem. `move_to_trash`
//! performs one recoverable operation only, with no retry or permanent fallback.
//! The feature owns confirmation, snapshot comparison, and permanent deletion.
//! Path-based native calls still cannot eliminate concurrent replacement races.
use std::{fs, path::Path};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported as native;
#[cfg(windows)]
use windows as native;

pub(crate) use native::{
    move_to_trash, validate_entry_path, validate_root, validate_subtree_root, validate_target,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) device: u64,
    pub(crate) inode: u128,
    pub(crate) directory: bool,
}

pub(crate) fn entry_identity(path: &Path) -> Result<Identity, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法检查项目 {}：{error}", path.display()))?;
    if !(metadata.is_dir() || metadata.is_file()) || metadata.file_type().is_symlink() {
        return Err("目标不是普通文件或文件夹，请重新扫描。".into());
    }
    native::identity(path, &metadata)
}

pub(crate) fn directory_identity(path: &Path) -> Result<Identity, String> {
    let identity = entry_identity(path)?;
    if !identity.directory {
        return Err("上级路径不是普通文件夹，请重新扫描。".into());
    }
    Ok(identity)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) use linux::linux_trash_with;
