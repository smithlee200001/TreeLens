//! Linux mount topology and no-follow path checks, shared by scanning and Trash.
//! This is a snapshot of a live namespace, not protection against hostile races.
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};

pub(crate) struct MountTable {
    points: HashSet<PathBuf>,
}

impl MountTable {
    pub(crate) fn read() -> io::Result<Self> {
        Self::parse(&fs::read("/proc/self/mountinfo")?)
    }

    pub(crate) fn parse(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid Linux mountinfo");
        let mut points = HashSet::new();
        for line in bytes.split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
            let fields: Vec<_> = line.split(|&b| b == b' ').collect();
            let separator = fields
                .iter()
                .position(|field| *field == b"-")
                .ok_or_else(invalid)?;
            if separator < 6 || fields.len() < separator + 4 {
                return Err(invalid());
            }
            let path = PathBuf::from(OsString::from_vec(unescape(fields[4])?));
            if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(invalid());
            }
            points.insert(path);
        }
        // An empty/truncated namespace description must not silently disable checks.
        if !points.contains(Path::new("/")) {
            return Err(invalid());
        }
        Ok(Self { points })
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.points.contains(path)
    }

    pub(crate) fn has_mount_at_or_below(&self, path: &Path) -> bool {
        self.points.iter().any(|point| point.starts_with(path))
    }
}

fn unescape(field: &[u8]) -> io::Result<Vec<u8>> {
    let mut result = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\' {
            let escape = field.get(index + 1..index + 4);
            let byte = match escape {
                Some(b"040") => b' ',
                Some(b"011") => b'\t',
                Some(b"012") => b'\n',
                Some(b"134") => b'\\',
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid mountinfo path escape",
                    ))
                }
            };
            result.push(byte);
            index += 4;
        } else {
            if field[index] == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NUL in mountinfo path",
                ));
            }
            result.push(field[index]);
            index += 1;
        }
    }
    Ok(result)
}

pub(crate) fn validate_directory_ancestors(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose an absolute directory path",
        ));
    }
    // Walk in path order so `link/..` cannot hide a symbolic link. Ordinary
    // parent components remain usable in paths typed by the user or CLI.
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                current.pop();
            }
            Component::CurDir => continue,
            component => current.push(component.as_os_str()),
        }
        if current.as_os_str().is_empty() {
            current.push("/");
        }
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::other(
                "Directory path contains a symbolic link or non-directory ancestor",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(extra: &[u8]) -> io::Result<MountTable> {
        MountTable::parse(&[b"1 0 8:1 / / rw - ext4 /dev/sda rw\n".as_slice(), extra].concat())
    }

    #[test]
    fn mount_paths_preserve_escaped_and_non_utf8_bytes() {
        let mounts =
            table(b"2 1 8:1 /bind /a\\040b\\011c\\012d\\134e\xff rw shared:1 - ext4 /dev/sda rw\n")
                .unwrap();
        let path = PathBuf::from(OsString::from_vec(b"/a b\tc\nd\\e\xff".to_vec()));
        assert!(mounts.contains(&path));
    }

    #[test]
    fn same_device_bind_mounts_and_descendants_are_detected() {
        let mounts =
            table(b"2 1 8:1 /elsewhere /scan/folder/bind rw - ext4 /dev/sda rw\n").unwrap();
        assert!(mounts.contains(Path::new("/scan/folder/bind")));
        assert!(mounts.has_mount_at_or_below(Path::new("/scan/folder")));
        assert!(mounts.has_mount_at_or_below(Path::new("/scan/folder/bind")));
        assert!(!mounts.has_mount_at_or_below(Path::new("/scan/fol")));
        assert!(!mounts.has_mount_at_or_below(Path::new("/scan/other")));
    }

    #[test]
    fn malformed_or_missing_mountinfo_fails_closed() {
        for bytes in [
            b"".as_slice(),
            b"broken",
            b"1 0 8:1 / /relative rw - ext4 /dev/sda rw",
        ] {
            assert!(MountTable::parse(bytes).is_err());
        }
        for bytes in [
            b"2 1 8:1 / relative rw - ext4 /dev/sda rw\n".as_slice(),
            b"2 1 8:1 / /bad\\777 rw - ext4 /dev/sda rw\n",
            b"2 1 8:1 / /bad\\ rw - ext4 /dev/sda rw\n",
            b"2 1 8:1 / /a/../b rw - ext4 /dev/sda rw\n",
        ] {
            assert!(table(bytes).is_err());
        }
    }

    #[test]
    fn current_namespace_has_root_mount() {
        assert!(MountTable::read().unwrap().contains(Path::new("/")));
    }
}
