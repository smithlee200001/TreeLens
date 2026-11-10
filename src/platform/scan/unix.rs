//! Unix physical allocation and no-follow validation. Linux additionally
//! excludes bind mounts using its mount table. Other targets retain the
//! original portable logical-size fallback.
use super::{absolute_path, root_metadata, Adapter, EntryAccounting, EntryKind, FileKey};
#[cfg(target_os = "linux")]
use crate::platform::fs::linux::{validate_directory_ancestors, MountTable};
use std::collections::HashSet;
use std::fs::{self, DirEntry, Metadata};
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct NativeAdapter {
    root_device: u64,
    seen: HashSet<FileKey>,
    #[cfg(target_os = "linux")]
    mount_table: Option<MountTable>,
}

impl NativeAdapter {
    pub fn new() -> Self {
        Self {
            root_device: 0,
            seen: HashSet::new(),
            #[cfg(target_os = "linux")]
            mount_table: None,
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub fn with_mount_table(mount_table: Option<MountTable>) -> Self {
        Self {
            mount_table,
            ..Self::new()
        }
    }

    fn account(&mut self, metadata: &Metadata, account: bool) -> EntryAccounting {
        let mut result = EntryAccounting::from_metadata(metadata, account);
        if account {
            (result.allocated, result.duplicate) = sizes(metadata, &mut self.seen);
        }
        result
    }
}

impl Adapter for NativeAdapter {
    fn start(&mut self, path: &Path) -> io::Result<(PathBuf, EntryAccounting)> {
        let path = absolute_path(path)?;
        #[cfg(target_os = "linux")]
        let path = {
            validate_directory_ancestors(&path)?;
            // Normalize only after rejecting every symlink ancestor.
            let path = fs::canonicalize(path)?;
            if self.mount_table.is_none() {
                self.mount_table = Some(MountTable::read()?);
            }
            path
        };
        let metadata = root_metadata(&path)?;
        self.root_device = device(&metadata);
        Ok((path, self.account(&metadata, true)))
    }

    fn validate_directory(&mut self, path: &Path) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        validate_directory_ancestors(path)
            .map_err(|error| format!("Directory or ancestor changed; skipped: {error}"))?;
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() && device(&m) == self.root_device => Ok(()),
            Ok(_) => Err("Directory changed type or device during scan; skipped".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn inspect_entry(&mut self, _entry: &DirEntry, path: &Path) -> EntryAccounting {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => return EntryAccounting::failure(error),
        };
        // File bind mounts are excluded before touching the hardlink set.
        #[cfg(target_os = "linux")]
        let excluded_mount = self
            .mount_table
            .as_ref()
            .expect("adapter started")
            .contains(path)
            || device(&metadata) != self.root_device;
        #[cfg(not(target_os = "linux"))]
        let excluded_mount = false;
        let mut result = self.account(&metadata, !excluded_mount);
        if excluded_mount {
            result.issue =
                Some("Mount point skipped (including file/directory bind mounts)".into());
        } else if result.kind == EntryKind::Directory {
            if device(&metadata) != self.root_device {
                result.issue = Some("Mount point skipped (different filesystem)".into());
            } else {
                result.descend = true;
            }
        }
        result
    }
}

#[cfg(unix)]
fn device(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}
#[cfg(not(unix))]
fn device(_: &Metadata) -> u64 {
    0
}

fn sizes(metadata: &Metadata, seen: &mut HashSet<FileKey>) -> (u64, bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let duplicate = !metadata.is_dir()
            && metadata.nlink() > 1
            && !seen.insert((metadata.dev(), metadata.ino()));
        (
            if duplicate {
                0
            } else {
                metadata.blocks().saturating_mul(512)
            },
            duplicate,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = seen;
        (if metadata.is_dir() { 0 } else { metadata.len() }, false)
    }
}
