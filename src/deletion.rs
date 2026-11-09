//! Folder-only, recoverable deletion. No permanent-delete fallback exists.
//!
//! Identity checks narrow the race between confirmation and moving to Trash, but
//! NSFileManager and Windows Shell accept a pathname, not an open directory handle. Another process
//! can still replace a component after the last check. This is not an atomic
//! security boundary against a concurrently hostile filesystem.

use crate::scanner::{NodeId, NodeKind, ScanOutcome};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u128,
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
            inode: metadata.ino() as u128,
        })
    }
    #[cfg(windows)]
    {
        let entry = crate::windows_fs::query(path)
            .map_err(|error| format!("无法检查文件夹身份：{error}"))?;
        if !entry.directory || entry.reparse {
            return Err("不能删除重解析点、符号链接或联接目录。".into());
        }
        Ok(Identity {
            device: entry.volume,
            inode: entry.id,
        })
    }
    #[cfg(not(any(unix, windows)))]
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
        #[cfg(windows)]
        validate_windows_ancestors(&scan.root_path)?;
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
        // The confirmation UI displays Unicode. Refuse names it cannot show
        // exactly, even though Windows APIs can preserve unpaired UTF-16 units.
        if path.to_str().is_none() {
            return Err("此文件夹路径包含非 UTF-8 / Unicode 名称，无法准确显示确认路径，请在系统文件管理器中处理。".into());
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
        #[cfg(windows)]
        {
            validate_windows_ancestors(&self.original_root)?;
            validate_windows_ancestors(&self.path)?;
            if fs::canonicalize(&self.path).map_err(|e| e.to_string())? != self.path {
                return Err("文件夹路径已改变，请重新扫描。".into());
            }
        }
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

#[cfg(not(any(target_os = "macos", windows)))]
fn move_to_trash(_: &Path) -> Result<(), String> {
    Err("此平台不支持移到废纸篓或回收站；没有删除任何文件。".into())
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use crate::scanner::scan;
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_node_without_using_existing_lossy_path() {
        let fixture = Fixture::new();
        let mut scan = fixture.scan();
        let target = id(&scan, "child");
        let name = OsString::from_vec(b"invalid-\xff".to_vec());
        // APFS rejects invalid UTF-8 filenames. Keep this path in memory and
        // create its valid, lossy lookalike to catch accidental substitution.
        let lookalike = fixture
            .0
            .join("parent")
            .join(name.to_string_lossy().as_ref());
        fs::create_dir(&lookalike).unwrap();
        scan.nodes[target].name = name.into_boxed_os_str();
        assert!(DeletionRequest::prepare(&scan, target)
            .and_then(|request| request.execute_with(|_| panic!("must not trash lossy path")))
            .is_err());
        assert!(lookalike.is_dir());
    }

    // macOS APFS cannot create this fixture; the in-memory case above still runs.
    #[cfg(all(unix, not(target_os = "macos")))]
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
        assert!(DeletionRequest::prepare(&scan, target)
            .unwrap_err()
            .contains("非 UTF-8"));
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

    #[cfg(unix)]
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

    #[cfg(windows)]
    #[test]
    fn unicode_folder_reaches_mock_with_exact_path() {
        let fixture = Fixture::new();
        let name = "文件夹-é-🦀";
        fs::create_dir(fixture.0.join(name)).unwrap();
        let scan = fixture.scan();
        let request = DeletionRequest::prepare(&scan, id(&scan, name)).unwrap();
        request
            .execute_with(|path| {
                assert_eq!(path.file_name().unwrap(), std::ffi::OsStr::new(name));
                Ok(())
            })
            .unwrap();
        assert!(fixture.0.join(name).is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn exact_utf16_never_substitutes_unpaired_surrogates() {
        use std::os::windows::ffi::OsStringExt;
        let units = [67, 58, 92, 0xd800, 0x6587, 0xd83e, 0xdd80];
        let name = std::ffi::OsString::from_wide(&units);
        let path = PathBuf::from(name);
        assert_eq!(
            exact_windows_path(&path).unwrap(),
            [units.as_slice(), &[0]].concat()
        );
        assert!(
            exact_windows_path(Path::new(&std::ffi::OsString::from_wide(&[65, 0, 66]))).is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn surrogate_name_never_acts_on_lossy_lookalike() {
        use std::os::windows::ffi::OsStringExt;
        let fixture = Fixture::new();
        let name = std::ffi::OsString::from_wide(&[0xd800]);
        let lookalike = fixture
            .0
            .join("parent")
            .join(name.to_string_lossy().as_ref());
        fs::create_dir(&lookalike).unwrap();
        let mut scan = fixture.scan();
        let target = id(&scan, "child");
        scan.nodes[target].name = name.into_boxed_os_str();
        assert!(DeletionRequest::prepare(&scan, target)
            .and_then(|r| r.execute_with(|_| panic!("must not act on lookalike")))
            .is_err());
        assert!(lookalike.is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_existing_non_unicode_folder_before_confirmation() {
        use std::os::windows::ffi::OsStringExt;
        let fixture = Fixture::new();
        let name = std::ffi::OsString::from_wide(&[0x61, 0xd800]);
        fs::create_dir(fixture.0.join(&name)).unwrap();
        let scan = fixture.scan();
        let target = scan
            .nodes
            .iter()
            .find(|node| node.name.as_ref() == name)
            .unwrap()
            .id;
        let error = DeletionRequest::prepare(&scan, target).unwrap_err();
        assert!(error.contains("Unicode"));
        assert!(fixture.0.join(name).is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn unc_and_device_paths_are_rejected_without_shell_operation() {
        for path in [
            r"\\server\share\folder",
            r"\\?\UNC\server\share\folder",
            r"\\.\C:\folder",
        ] {
            assert!(validate_windows_ancestors(Path::new(path)).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn junction_swap_is_rejected_before_mock() {
        let fixture = Fixture::new();
        let request = fixture.request();
        let path = fixture.0.join("parent");
        let saved = fixture.0.join("saved");
        fs::rename(&path, &saved).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&path)
            .arg(&saved)
            .status()
            .unwrap();
        assert!(status.success(), "synthetic junction creation failed");
        assert!(request
            .execute_with(|_| panic!("must not follow junction"))
            .is_err());
        // Remove the junction itself, never its destination.
        fs::remove_dir(&path).unwrap();
        assert!(saved.join("child").is_dir());
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

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unsupported_platform_keeps_folder() {
        let fixture = Fixture::new();
        assert!(fixture.request().execute().is_err());
        assert!(fixture.0.join("parent/child").is_dir());
    }
}

/// Only drive-letter, fixed local volumes are offered to the Shell. Inspect the
/// entire original chain before canonicalization, which would hide junctions.
#[cfg(windows)]
fn validate_windows_ancestors(path: &Path) -> Result<(), String> {
    use std::path::Prefix;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut components = absolute.components();
    let drive = match components.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
            _ => return Err("仅支持本地固定磁盘的回收站，不支持网络或设备路径。".into()),
        },
        _ => return Err("需要完整的本地磁盘路径。".into()),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err("不支持相对磁盘路径。".into());
    }
    let root = [drive as u16, b':' as u16, b'\\' as u16, 0];
    // DRIVE_FIXED = 3. Removable, remote, optical and unknown drives fail closed.
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != 3 {
        return Err("此磁盘不支持安全的回收站操作。".into());
    }
    for component in components {
        match component {
            Component::Normal(name) => {
                use std::os::windows::ffi::OsStrExt;
                let units: Vec<_> = name.encode_wide().collect();
                if units.contains(&0)
                    || units.contains(&(b':' as u16))
                    || matches!(units.last(), Some(32 | 46))
                {
                    return Err("路径可能被 Windows Shell 重新解释，已取消操作。".into());
                }
            }
            _ => return Err("文件夹路径不是规范的本地路径。".into()),
        }
    }
    let mut expected_volume = None;
    for ancestor in absolute.ancestors() {
        let identity = directory_identity(ancestor)?;
        if expected_volume.is_some_and(|volume| volume != identity.device) {
            return Err("不能删除挂载点或跨磁盘的文件夹。".into());
        }
        expected_volume = Some(identity.device);
    }
    Ok(())
}

#[cfg(windows)]
fn exact_windows_path(path: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err("路径包含空字符，已取消操作。".into());
    }
    units.push(0);
    Ok(units)
}

#[cfg(windows)]
fn move_to_trash(path: &Path) -> Result<(), String> {
    windows_recycle::recycle(path)
}

#[cfg(windows)]
mod windows_recycle {
    use super::*;
    use std::sync::{Arc, Mutex};
    use windows::core::{implement, Error, Ref, Result as WinResult, HRESULT, PCWSTR};
    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::*;

    #[derive(Default)]
    struct Outcome {
        completed: bool,
        failure: Option<String>,
    }

    #[implement(IFileOperationProgressSink)]
    struct RecycleSink {
        outcome: Arc<Mutex<Outcome>>,
        expected: Identity,
    }

    impl RecycleSink_Impl {
        fn reject(&self, reason: &str) -> WinResult<()> {
            self.outcome.lock().unwrap().failure = Some(reason.into());
            Err(Error::from_hresult(E_ABORT))
        }
    }

    // These flags alone are not a no-permanent-delete guarantee. The sink vetoes
    // any direct-delete callback, including a Shell fallback after recycling fails.
    // https://learn.microsoft.com/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifileoperationprogresssink-predeleteitem
    #[allow(non_snake_case, unused_variables)]
    impl IFileOperationProgressSink_Impl for RecycleSink_Impl {
        fn StartOperations(&self) -> WinResult<()> {
            Ok(())
        }
        fn FinishOperations(&self, result: HRESULT) -> WinResult<()> {
            if result.is_err() {
                return self.reject("回收站操作未完成。");
            }
            Ok(())
        }
        fn PreDeleteItem(&self, flags: u32, item: Ref<'_, IShellItem>) -> WinResult<()> {
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                return self.reject("此项目无法放入回收站；已拒绝永久删除。");
            }
            let Some(item) = item.as_ref() else {
                return self.reject("无法验证 Shell 项目。");
            };
            // Check the path actually resolved by Shell, not a lossy display name.
            let resolved = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }?;
            use std::os::windows::ffi::OsStringExt;
            let path = PathBuf::from(unsafe { std::ffi::OsString::from_wide(resolved.as_wide()) });
            unsafe { CoTaskMemFree(Some(resolved.0.cast())) };
            if validate_windows_ancestors(&path).is_err()
                || directory_identity(&path).ok().as_ref() != Some(&self.expected)
            {
                return self.reject("Shell 项目与已确认的文件夹不一致，已取消操作。");
            }
            Ok(())
        }
        fn PostDeleteItem(
            &self,
            flags: u32,
            item: Ref<'_, IShellItem>,
            result: HRESULT,
            recycled: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            // A success-like skip code is not a completed recycle. A nonnull
            // destination is documented to identify the item in the Recycle Bin.
            if result != HRESULT(0) || recycled.is_none() {
                return self.reject("未能确认文件夹已进入回收站。请检查文件夹位置。");
            }
            self.outcome.lock().unwrap().completed = true;
            Ok(())
        }
        fn PreRenameItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostRenameItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            hrrename: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn PreMoveItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostMoveItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            hrmove: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn PreCopyItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostCopyItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            hrcopy: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn PreNewItem(
            &self,
            dwflags: u32,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostNewItem(
            &self,
            dwflags: u32,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            psztemplatename: &PCWSTR,
            dwfileattributes: u32,
            hrnew: HRESULT,
            psinewitem: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn UpdateProgress(&self, iworktotal: u32, iworksofar: u32) -> WinResult<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> WinResult<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> WinResult<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> WinResult<()> {
            Ok(())
        }
    }

    pub(super) fn recycle(path: &Path) -> Result<(), String> {
        validate_windows_ancestors(path)?;
        let expected = directory_identity(path)?;
        let exact = exact_windows_path(path)?;
        // execute() runs on a fresh worker. Never borrow an incompatible COM
        // apartment; failed initialization leaves the filesystem untouched.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| format!("无法初始化回收站操作：{e}"))?;
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _apartment = Apartment;
        let outcome = Arc::new(Mutex::new(Outcome::default()));
        let sink: IFileOperationProgressSink = RecycleSink {
            outcome: outcome.clone(),
            expected,
        }
        .into();
        let perform = || -> WinResult<()> {
            unsafe {
                let operation: IFileOperation =
                    CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
                operation.SetOperationFlags(
                    FOFX_RECYCLEONDELETE
                        | FOFX_EARLYFAILURE
                        | FOFX_ADDUNDORECORD
                        | FOF_NOERRORUI
                        | FOF_SILENT
                        | FOF_NORECURSEREPARSE,
                )?;
                let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(exact.as_ptr()), None)?;
                operation.DeleteItem(&item, &sink)?;
                operation.PerformOperations()?;
                if operation.GetAnyOperationsAborted()?.as_bool() {
                    return Err(Error::from_hresult(E_ABORT));
                }
                Ok(())
            }
        };
        let result = perform();
        let state = outcome.lock().unwrap();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        result.map_err(|e| format!("移到回收站失败：{e}。未尝试永久删除。"))?;
        if !state.completed {
            return Err("回收站操作未确认完成。".into());
        }
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn sink() -> (IFileOperationProgressSink, Arc<Mutex<Outcome>>) {
            let state = Arc::new(Mutex::new(Outcome::default()));
            (
                RecycleSink {
                    outcome: state.clone(),
                    expected: Identity {
                        device: 1,
                        inode: 1,
                    },
                }
                .into(),
                state,
            )
        }
        #[test]
        fn direct_delete_callback_is_vetoed_without_touching_disk() {
            let (sink, state) = sink();
            assert!(unsafe { sink.PreDeleteItem(0, None::<&IShellItem>) }.is_err());
            assert!(state.lock().unwrap().failure.is_some());
        }
        #[test]
        fn per_item_error_and_missing_recycle_destination_are_failures() {
            for result in [HRESULT(0), E_ABORT, HRESULT(1)] {
                let (sink, state) = sink();
                assert!(unsafe {
                    sink.PostDeleteItem(0, None::<&IShellItem>, result, None::<&IShellItem>)
                }
                .is_err());
                assert!(!state.lock().unwrap().completed);
                assert!(state.lock().unwrap().failure.is_some());
            }
        }
        #[test]
        fn finish_error_is_not_lost() {
            let (sink, state) = sink();
            assert!(unsafe { sink.FinishOperations(E_ABORT) }.is_err());
            assert!(state.lock().unwrap().failure.is_some());
        }
    }
}
