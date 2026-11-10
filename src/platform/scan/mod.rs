//! Native scan boundary. Traversal never needs OS metadata extensions or cfgs.
use std::fs::{DirEntry, Metadata};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
pub(crate) use unix::NativeAdapter;
#[cfg(windows)]
pub(crate) use windows::NativeAdapter;

// Keep Unix keys compact while retaining all 128 Windows identity bits.
#[cfg(not(windows))]
pub(crate) type FileKey = (u64, u64);
#[cfg(windows)]
pub(crate) type FileKey = (u64, u128);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

/// Accounting is decided before the traversal sees an entry. In particular,
/// excluded mounts must not claim a hardlink's physical allocation.
pub(crate) struct EntryAccounting {
    pub kind: EntryKind,
    pub logical: u64,
    pub allocated: u64,
    pub duplicate: bool,
    pub account: bool,
    pub descend: bool,
    pub issue: Option<String>,
}

impl EntryAccounting {
    pub fn unknown() -> Self {
        Self {
            kind: EntryKind::Other,
            logical: 0,
            allocated: 0,
            duplicate: false,
            account: true,
            descend: false,
            issue: None,
        }
    }

    fn from_metadata(metadata: &Metadata, account: bool) -> Self {
        let kind = if metadata.file_type().is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Self {
            kind,
            logical: if !account || metadata.is_dir() {
                0
            } else {
                metadata.len()
            },
            account,
            ..Self::unknown()
        }
    }

    fn failure(error: io::Error) -> Self {
        Self {
            issue: Some(error.to_string()),
            ..Self::unknown()
        }
    }
}

/// A compile-time selected, monomorphized adapter. No dynamic dispatch is used.
pub(crate) trait Adapter {
    fn start(&mut self, path: &Path) -> io::Result<(PathBuf, EntryAccounting)>;
    fn validate_directory(&mut self, path: &Path) -> Result<(), String>;
    fn inspect_entry(&mut self, entry: &DirEntry, path: &Path) -> EntryAccounting;
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn root_metadata(path: &Path) -> io::Result<Metadata> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a directory, not a file or symbolic link",
        ));
    }
    Ok(metadata)
}
