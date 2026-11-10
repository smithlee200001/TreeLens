//! Windows metadata-only, no-follow handle queries and full-width identities.
use super::{absolute_path, root_metadata, Adapter, EntryAccounting, EntryKind, FileKey};
use crate::platform::fs::windows::{self, EntryInfo};
use std::collections::HashSet;
use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};

type NativeQuery = fn(&Path) -> io::Result<EntryInfo>;

pub(crate) struct NativeAdapter<Q = NativeQuery> {
    query: Q,
    root_device: u64,
    seen: HashSet<FileKey>,
}

impl NativeAdapter {
    pub fn new() -> Self {
        Self::with_query(windows::query as NativeQuery)
    }
}

impl<Q> NativeAdapter<Q> {
    pub fn with_query(query: Q) -> Self {
        Self {
            query,
            root_device: 0,
            seen: HashSet::new(),
        }
    }
}

impl<Q: FnMut(&Path) -> io::Result<EntryInfo>> Adapter for NativeAdapter<Q> {
    fn start(&mut self, path: &Path) -> io::Result<(PathBuf, EntryAccounting)> {
        if !path.has_root()
            && matches!(
                path.components().next(),
                Some(std::path::Component::Prefix(_))
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Drive-relative paths are ambiguous; choose an absolute path such as C:\\folder",
            ));
        }
        let path = absolute_path(path)?;
        let metadata = root_metadata(&path)?;
        if windows::is_reparse(&metadata) {
            return Err(io::Error::other("Reparse-point roots are not scanned"));
        }
        for ancestor in path.ancestors().skip(1).filter(|p| p.has_root()) {
            if windows::is_reparse(&fs::symlink_metadata(ancestor)?) {
                return Err(io::Error::other("Root has a reparse-point ancestor"));
            }
        }
        let info = (self.query)(&path)?;
        if info.reparse || !info.directory {
            return Err(io::Error::other("Root changed type or is a reparse point"));
        }
        self.root_device = info.volume;
        // Preserve Windows' zero directory allocation at the root.
        Ok((path, EntryAccounting::from_metadata(&metadata, true)))
    }

    fn validate_directory(&mut self, path: &Path) -> Result<(), String> {
        let info = (self.query)(path).map_err(|error| error.to_string())?;
        if !info.directory || info.reparse || info.volume != self.root_device {
            Err("Directory changed type, became a reparse point, or changed volume; skipped".into())
        } else {
            Ok(())
        }
    }

    fn inspect_entry(&mut self, entry: &DirEntry, path: &Path) -> EntryAccounting {
        // Enumeration retains logical size/type even if handle queries fail.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return EntryAccounting::failure(error),
        };
        let mut result = EntryAccounting::from_metadata(&metadata, true);
        if windows::is_reparse(&metadata) {
            result.issue = Some(
                "Reparse point skipped (link, junction, mount point, or cloud placeholder)".into(),
            );
            return result;
        }
        match (self.query)(path) {
            Err(error) => {
                result.issue = Some(format!(
                    "Allocation/identity unavailable; allocation omitted: {error}"
                ))
            }
            Ok(info) if info.reparse || info.volume != self.root_device => {
                result.issue = Some("Reparse point or different volume skipped".into());
            }
            Ok(info) if info.directory != (result.kind == EntryKind::Directory) => {
                result.issue = Some("Entry changed type during scan; skipped".into());
            }
            Ok(info) => {
                result.duplicate =
                    !info.directory && info.links > 1 && !self.seen.insert((info.volume, info.id));
                result.allocated = if result.duplicate { 0 } else { info.allocated };
                result.descend = info.directory;
            }
        }
        result
    }
}
