//! Folder-only, recoverable deletion. No permanent-delete fallback exists.
//!
//! Identity checks narrow the race between confirmation and moving to Trash, but
//! NSFileManager accepts a pathname, not an open directory handle. Another process
//! can still replace a component after the last check. This is not an atomic
//! security boundary against a concurrently hostile filesystem.

use crate::scanner::{NodeId, NodeKind, ScanOutcome};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

fn directory_identity(path: &Path) -> Result<Identity, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法检查文件夹 {}：{error}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("目标或上级路径不是普通文件夹，请重新扫描。".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    Err("此平台不支持安全的文件夹身份检查。".into())
}

/// A single-use confirmation snapshot. Construct when opening the confirmation
/// dialog, keep its path visible, then consume it only after explicit confirmation.
#[derive(Debug)]
pub struct DeletionRequest {
    original_root: PathBuf,
    root: PathBuf,
    path: PathBuf,
    identities: Vec<(PathBuf, Identity)>,
}

impl DeletionRequest {
    pub fn prepare(scan: &ScanOutcome, id: NodeId) -> Result<Self, String> {
        if id == scan.root {
            return Err("不能删除本次扫描的根目录。".into());
        }
        let node = scan.nodes.get(id).ok_or("所选项目已失效，请重新扫描。")?;
        if node.kind != NodeKind::Directory {
            return Err("只能将普通文件夹移到废纸篓。".into());
        }
        // Do not use path_for on unchecked parent links: stale/malformed input
        // must fail rather than panic, loop, or escape the root via a filename.
        let mut names = Vec::new();
        let mut cursor = id;
        for _ in 0..scan.nodes.len() {
            if cursor == scan.root {
                break;
            }
            let node = scan.nodes.get(cursor).ok_or("文件夹路径已失效。")?;
            let name = Path::new(node.name.as_ref());
            let mut components = name.components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || node.kind != NodeKind::Directory
            {
                return Err("文件夹路径不安全，请重新扫描。".into());
            }
            names.push(node.name.as_ref());
            cursor = node.parent.ok_or("文件夹不属于扫描根目录。")?;
        }
        if cursor != scan.root || names.is_empty() {
            return Err("文件夹不属于扫描根目录。".into());
        }
        directory_identity(&scan.root_path)?;
        let root = fs::canonicalize(&scan.root_path)
            .map_err(|error| format!("无法确认扫描根目录：{error}"))?;
        let root_identity = directory_identity(&root)?;
        let mut identities = vec![(root.clone(), root_identity)];
        let mut path = root.clone();
        for name in names.into_iter().rev() {
            path.push(name);
            let identity = directory_identity(&path)?;
            if identity.device != identities[0].1.device {
                return Err("不能删除挂载点或跨磁盘的文件夹。".into());
            }
            identities.push((path.clone(), identity));
        }
        if path == root || path.parent().is_none() || !path.starts_with(&root) {
            return Err("不能删除根目录或扫描范围外的文件夹。".into());
        }
        // Foundation path strings must be exact. Never act on display_name or
        // to_string_lossy: they can identify a different entry.
        if path.to_str().is_none() {
            return Err("此文件夹路径包含非 UTF-8 字节，请在 Finder 中处理。".into());
        }
        let request = Self {
            original_root: scan.root_path.clone(),
            root,
            path,
            identities,
        };
        request.revalidate()?;
        Ok(request)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn revalidate(&self) -> Result<(), String> {
        directory_identity(&self.original_root)?;
        if fs::canonicalize(&self.original_root)
            .map_err(|error| format!("无法重新检查扫描根目录：{error}"))?
            != self.root
        {
            return Err("扫描根目录已改变，请重新扫描。".into());
        }
        for (path, expected) in &self.identities {
            if directory_identity(path)? != *expected {
                return Err("文件夹或上级目录已改变，已取消操作。请重新扫描。".into());
            }
        }
        Ok(())
    }

    pub fn execute(self) -> Result<(), String> {
        self.execute_with(move_to_trash)
    }

    fn execute_with(self, trash: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
        self.revalidate()?;
        trash(&self.path)
    }
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let exact = path.to_str().ok_or("无法无损转换文件夹路径。")?;
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(exact), true);
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| {
            format!(
                "移到废纸篓失败：{}。未尝试永久删除。",
                error.localizedDescription()
            )
        })
}

#[cfg(not(target_os = "macos"))]
fn move_to_trash(_: &Path) -> Result<(), String> {
    Err("仅 macOS 支持移到废纸篓；没有删除任何文件。".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::scanner::scan;
    use std::ffi::OsString;
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "tree-lens-trash-test-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::create_dir_all(path.join("parent/child")).unwrap();
            fs::write(path.join("file"), b"synthetic fixture").unwrap();
            Self(path)
        }
        fn scan(&self) -> ScanOutcome {
            scan(&self.0, &AtomicBool::new(false), |_| {}).unwrap()
        }
        fn request(&self) -> DeletionRequest {
            let scan = self.scan();
            DeletionRequest::prepare(&scan, id(&scan, "child")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only remove this test's unique synthetic fixture, never call Trash.
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn id(scan: &ScanOutcome, name: &str) -> NodeId {
        scan.nodes
            .iter()
            .find(|node| node.name.as_ref() == std::ffi::OsStr::new(name))
            .unwrap()
            .id
    }

    #[test]
    fn valid_folder_reaches_mock_once_without_removing_anything() {
        let fixture = Fixture::new();
        let request = fixture.request();
        let expected = fs::canonicalize(fixture.0.join("parent/child")).unwrap();
        assert_eq!(request.path(), expected);
        let mut calls = 0;
        request
            .execute_with(|path| {
                calls += 1;
                assert_eq!(path, expected);
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert!(expected.is_dir());
    }

    #[test]
    fn rejects_root_files_and_invalid_ids() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        for target in [scan.root, id(&scan, "file"), usize::MAX] {
            assert!(DeletionRequest::prepare(&scan, target).is_err());
        }
    }

    #[test]
    fn rejects_filesystem_root_request() {
        let fixture = Fixture::new();
        let mut scan = fixture.scan();
        scan.root_path = PathBuf::from("/");
        assert!(DeletionRequest::prepare(&scan, scan.root).is_err());
    }

    #[test]
    fn rejects_escaping_names_and_parent_cycles() {
        let fixture = Fixture::new();
        for name in ["..", "/", "../outside", "a/b"] {
            let mut scan = fixture.scan();
            let target = id(&scan, "child");
            scan.nodes[target].name = std::ffi::OsStr::new(name).into();
            assert!(DeletionRequest::prepare(&scan, target).is_err());
        }
        let mut scan = fixture.scan();
        let target = id(&scan, "child");
        scan.nodes[target].parent = Some(target);
        assert!(DeletionRequest::prepare(&scan, target).is_err());
    }

    #[test]
    fn rejects_symlink_and_symlinked_ancestor() {
        let fixture = Fixture::new();
        symlink("parent", fixture.0.join("link")).unwrap();
        let scan = fixture.scan();
        assert!(DeletionRequest::prepare(&scan, id(&scan, "link")).is_err());
        let child = id(&scan, "child");
        fs::rename(fixture.0.join("parent"), fixture.0.join("saved")).unwrap();
        symlink("saved", fixture.0.join("parent")).unwrap();
        assert!(DeletionRequest::prepare(&scan, child).is_err());
    }

    #[test]
    fn rejects_non_utf8_paths_without_lossy_action() {
        let fixture = Fixture::new();
        let name = OsString::from_vec(b"invalid-\xff".to_vec());
        fs::create_dir(fixture.0.join(&name)).unwrap();
        let scan = fixture.scan();
        let target = scan
            .nodes
            .iter()
            .find(|node| node.name.as_ref() == name)
            .unwrap()
            .id;
        assert!(DeletionRequest::prepare(&scan, target).is_err());
    }

    #[test]
    fn replaced_target_is_rejected_before_mock() {
        let fixture = Fixture::new();
        let request = fixture.request();
        let path = fixture.0.join("parent/child");
        fs::rename(&path, fixture.0.join("old-child")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(request
            .execute_with(|_| panic!("must not trash replacement"))
            .is_err());
    }

    #[test]
    fn replaced_ancestor_is_rejected_before_mock() {
        let fixture = Fixture::new();
        let request = fixture.request();
        fs::rename(fixture.0.join("parent"), fixture.0.join("old-parent")).unwrap();
        fs::create_dir_all(fixture.0.join("parent/child")).unwrap();
        assert!(request
            .execute_with(|_| panic!("must not trash replacement"))
            .is_err());
    }

    #[test]
    fn replaced_scan_root_is_rejected_before_mock() {
        let fixture = Fixture::new();
        let request = fixture.request();
        // Keep both roots inside an independently owned synthetic container.
        let container = Fixture::new();
        fs::rename(&fixture.0, container.0.join("old-root")).unwrap();
        fs::create_dir_all(fixture.0.join("parent/child")).unwrap();
        assert!(request
            .execute_with(|_| panic!("must not trash replacement root"))
            .is_err());
    }

    #[test]
    fn symlink_swap_after_confirmation_is_rejected_before_mock() {
        for target in ["parent", "parent/child"] {
            let fixture = Fixture::new();
            let request = fixture.request();
            let path = fixture.0.join(target);
            let saved = fixture.0.join("saved");
            fs::rename(&path, &saved).unwrap();
            symlink(&saved, &path).unwrap();
            assert!(request
                .execute_with(|_| panic!("must not follow symlink"))
                .is_err());
        }
    }

    #[test]
    fn missing_target_is_rejected_before_mock() {
        let fixture = Fixture::new();
        let request = fixture.request();
        fs::rename(fixture.0.join("parent/child"), fixture.0.join("moved")).unwrap();
        assert!(request
            .execute_with(|_| panic!("must not trash missing folder"))
            .is_err());
    }

    #[test]
    fn native_failure_has_no_delete_fallback() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .request()
                .execute_with(|_| Err("mock denied".into())),
            Err("mock denied".into())
        );
        assert!(fixture.0.join("parent/child").is_dir());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_keeps_folder() {
        let fixture = Fixture::new();
        assert!(fixture.request().execute().is_err());
        assert!(fixture.0.join("parent/child").is_dir());
    }
}
