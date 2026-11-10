//! Recoverable file/folder deletion and explicitly confirmed permanent deletion.
//! Trash failures never fall back to permanent deletion.
//!
//! Identity checks narrow the race between confirmation and deletion, but
//! native Trash APIs and remove_file accept a pathname, not a bound identity. Another process
//! can still replace a component after the last check. This is not an atomic
//! security boundary against a concurrently hostile filesystem.

use crate::scanner::{NodeId, NodeKind, ScanOutcome};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u128,
    directory: bool,
}

fn entry_identity(path: &Path) -> Result<Identity, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法检查项目 {}：{error}", path.display()))?;
    if !(metadata.is_dir() || metadata.is_file()) || metadata.file_type().is_symlink() {
        return Err("目标不是普通文件或文件夹，请重新扫描。".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Identity {
            device: metadata.dev(),
            inode: metadata.ino() as u128,
            directory: metadata.is_dir(),
        })
    }
    #[cfg(windows)]
    {
        let entry =
            crate::windows_fs::query(path).map_err(|error| format!("无法检查项目身份：{error}"))?;
        if entry.reparse || entry.directory != metadata.is_dir() {
            return Err("不能删除重解析点、符号链接或联接目录。".into());
        }
        Ok(Identity {
            device: entry.volume,
            inode: entry.id,
            directory: entry.directory,
        })
    }
    #[cfg(not(any(unix, windows)))]
    Err("此平台不支持安全的项目身份检查。".into())
}

fn directory_identity(path: &Path) -> Result<Identity, String> {
    let identity = entry_identity(path)?;
    if !identity.directory {
        return Err("上级路径不是普通文件夹，请重新扫描。".into());
    }
    Ok(identity)
}

/// The confirmation binds the selected action; Trash never escalates to Permanent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionMode {
    Trash,
    Permanent,
}

#[cfg(target_os = "macos")]
fn validate_macos_ancestors(path: &Path) -> Result<(), String> {
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

#[derive(Debug, PartialEq, Eq)]
struct SubtreeEntry {
    identity: Identity,
    // In-place edits should invalidate confirmation too. Directory mtimes are
    // excluded because our own bottom-up removals change them.
    file_state: Option<(u64, std::time::SystemTime)>,
}

type Subtree = std::collections::BTreeMap<PathBuf, SubtreeEntry>;

fn subtree_entry(path: &Path) -> Result<SubtreeEntry, String> {
    let identity = entry_identity(path)?;
    let file_state = if identity.directory {
        None
    } else {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        Some((
            metadata.len(),
            metadata.modified().map_err(|e| e.to_string())?,
        ))
    };
    Ok(SubtreeEntry {
        identity,
        file_state,
    })
}

/// Snapshot without following links. Every directory is checked again before
/// enumeration; this is conservative validation, not an atomic filesystem lock.
fn snapshot_subtree(root: &Path, device: u64, cancelled: &AtomicBool) -> Result<Subtree, String> {
    #[cfg(target_os = "linux")]
    reject_linux_subtree_mounts(
        root,
        &crate::linux_fs::MountTable::read().map_err(|e| e.to_string())?,
    )?;
    let mut result = Subtree::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if cancelled.load(Ordering::Relaxed) {
            return Err("已取消内容检查；没有删除任何项目。".into());
        }
        for ancestor in path.ancestors().skip(1).take_while(|p| p.starts_with(root)) {
            let expected: &SubtreeEntry = result.get(ancestor).ok_or("目录结构已改变。")?;
            if directory_identity(ancestor)? != expected.identity {
                return Err("上级目录已改变，请重新扫描。".into());
            }
        }
        #[cfg(windows)]
        validate_windows_ancestors(&path)?;
        let entry = subtree_entry(&path)?;
        if entry.identity.device != device || path.to_str().is_none() {
            return Err("子目录包含跨磁盘项目或无法准确显示的名称，已取消删除。".into());
        }
        if entry.identity.directory {
            for child in fs::read_dir(&path).map_err(|e| format!("无法检查全部子项目：{e}"))?
            {
                if cancelled.load(Ordering::Relaxed) {
                    return Err("已取消内容检查；没有删除任何项目。".into());
                }
                pending.push(child.map_err(|e| e.to_string())?.path());
            }
        }
        result.insert(path, entry);
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn reject_linux_subtree_mounts(
    path: &Path,
    mounts: &crate::linux_fs::MountTable,
) -> Result<(), String> {
    if mounts.has_mount_at_or_below(path) {
        return Err("不能删除挂载点或包含挂载点的项目（包括 bind mount）。".into());
    }
    Ok(())
}

/// A single-use confirmation snapshot. Construct when opening the confirmation
/// dialog, keep its path visible, then consume it only after explicit confirmation.
#[derive(Debug)]
pub struct DeletionRequest {
    original_root: PathBuf,
    root: PathBuf,
    path: PathBuf,
    identities: Vec<(PathBuf, Identity)>,
    mode: DeletionMode,
    subtree: Option<Subtree>,
}

impl DeletionRequest {
    pub fn prepare(scan: &ScanOutcome, id: NodeId) -> Result<Self, String> {
        Self::prepare_with_mode(scan, id, DeletionMode::Trash)
    }

    pub fn prepare_with_mode(
        scan: &ScanOutcome,
        id: NodeId,
        mode: DeletionMode,
    ) -> Result<Self, String> {
        Self::prepare_target_with_mode(scan, id, mode)?.snapshot_contents()
    }

    /// Fast target-only validation; recursive capture must run off the UI thread.
    pub fn prepare_target_with_mode(
        scan: &ScanOutcome,
        id: NodeId,
        mode: DeletionMode,
    ) -> Result<Self, String> {
        if scan.nodes.get(scan.root).is_none_or(|root| {
            root.kind != NodeKind::Directory || root.parent.is_some() || root.id != scan.root
        }) {
            return Err("扫描根目录数据已失效。".into());
        }
        if id == scan.root {
            return Err("不能删除本次扫描的根目录。".into());
        }
        let node = scan.nodes.get(id).ok_or("所选项目已失效，请重新扫描。")?;
        if !matches!(node.kind, NodeKind::Directory | NodeKind::File) {
            return Err("只能删除普通文件或文件夹。".into());
        }
        let target_directory = node.kind == NodeKind::Directory;
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
                || node.id != cursor
                || (cursor != id && node.kind != NodeKind::Directory)
            {
                return Err("文件夹路径不安全，请重新扫描。".into());
            }
            names.push(node.name.as_ref());
            let parent = node.parent.ok_or("项目不属于扫描根目录。")?;
            if scan
                .nodes
                .get(parent)
                .is_none_or(|parent_node| !parent_node.children.contains(&cursor))
                || (node.kind == NodeKind::File && !node.children.is_empty())
            {
                return Err("扫描目录结构已失效，请重新扫描。".into());
            }
            cursor = parent;
        }
        if cursor != scan.root || names.is_empty() {
            return Err("文件夹不属于扫描根目录。".into());
        }
        #[cfg(target_os = "macos")]
        validate_macos_ancestors(&scan.root_path)?;
        #[cfg(target_os = "linux")]
        crate::linux_fs::validate_directory_ancestors(&scan.root_path)
            .map_err(|error| format!("无法安全检查 Linux 文件夹路径：{error}"))?;
        #[cfg(windows)]
        validate_windows_ancestors(&scan.root_path)?;
        directory_identity(&scan.root_path)?;
        let root = fs::canonicalize(&scan.root_path)
            .map_err(|error| format!("无法确认扫描根目录：{error}"))?;
        let root_identity = directory_identity(&root)?;
        let mut identities = vec![(root.clone(), root_identity)];
        let mut path = root.clone();
        let count = names.len();
        for (index, name) in names.into_iter().rev().enumerate() {
            path.push(name);
            let identity = entry_identity(&path)?;
            let expected_directory = index + 1 < count || target_directory;
            if identity.directory != expected_directory {
                return Err("项目类型已改变，请重新扫描。".into());
            }
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
            return Err("此项目路径包含非 UTF-8 / Unicode 名称，无法准确显示确认路径，请在系统文件管理器中处理。".into());
        }
        let request = Self {
            original_root: scan.root_path.clone(),
            root,
            path,
            identities,
            mode,
            subtree: None,
        };
        request.revalidate()?;
        Ok(request)
    }

    /// Capture before showing confirmation. Dropping this request or a worker's
    /// result cancels preparation without deleting anything.
    pub fn snapshot_contents(self) -> Result<Self, String> {
        self.snapshot_contents_with_cancel(&AtomicBool::new(false))
    }

    pub fn snapshot_contents_with_cancel(mut self, cancelled: &AtomicBool) -> Result<Self, String> {
        if cancelled.load(Ordering::Relaxed) {
            return Err("已取消内容检查；没有删除任何项目。".into());
        }
        self.revalidate()?;
        if self.mode == DeletionMode::Permanent
            && self
                .identities
                .last()
                .is_some_and(|(_, identity)| identity.directory)
        {
            self.subtree = Some(snapshot_subtree(
                &self.path,
                self.identities[0].1.device,
                cancelled,
            )?);
            self.revalidate()?;
        }
        Ok(self)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn mode(&self) -> DeletionMode {
        self.mode
    }

    fn revalidate(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            validate_macos_ancestors(&self.original_root)?;
            validate_macos_ancestors(self.path.parent().ok_or("路径没有上级目录。")?)?;
        }
        #[cfg(target_os = "linux")]
        {
            crate::linux_fs::validate_directory_ancestors(&self.original_root)
                .and_then(|()| {
                    crate::linux_fs::validate_directory_ancestors(
                        self.path
                            .parent()
                            .ok_or_else(|| std::io::Error::other("Missing parent"))?,
                    )
                })
                .map_err(|error| format!("无法安全检查 Linux 文件夹路径：{error}"))?;
            let mounts = crate::linux_fs::MountTable::read()
                .map_err(|error| format!("无法读取 Linux 挂载信息，已取消删除操作：{error}"))?;
            if mounts.has_mount_at_or_below(&self.path) {
                return Err("不能删除挂载点或包含挂载点的项目（包括 bind mount）。".into());
            }
        }
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
            if entry_identity(path)? != *expected {
                return Err("项目或上级目录已改变，已取消操作。请重新扫描。".into());
            }
        }
        Ok(())
    }

    pub fn execute(self) -> Result<(), String> {
        match self.mode {
            DeletionMode::Trash => self.execute_with(move_to_trash),
            DeletionMode::Permanent => {
                self.revalidate()?;
                if let Some(subtree) = &self.subtree {
                    if snapshot_subtree(
                        &self.path,
                        self.identities[0].1.device,
                        &AtomicBool::new(false),
                    )? != *subtree
                    {
                        return Err("文件夹内容已改变，已取消操作。请重新扫描并确认。".into());
                    }
                    // Never recursively sweep a live directory: remove exactly
                    // the confirmed entries. New children make remove_dir fail.
                    for (path, expected) in subtree.iter().rev() {
                        let remove = || -> Result<(), String> {
                            self.revalidate()?;
                            for ancestor in path
                                .ancestors()
                                .skip(1)
                                .take_while(|p| p.starts_with(&self.path))
                            {
                                let expected = subtree.get(ancestor).ok_or("目录结构已改变。")?;
                                if directory_identity(ancestor)? != expected.identity {
                                    return Err("上级目录已改变。".into());
                                }
                            }
                            #[cfg(windows)]
                            validate_windows_ancestors(path)?;
                            if subtree_entry(path)? != *expected {
                                return Err("子项目已改变。".into());
                            }
                            if expected.identity.directory {
                                fs::remove_dir(path)
                            } else {
                                fs::remove_file(path)
                            }
                            .map_err(|e| e.to_string())
                        };
                        remove().map_err(|error| {
                            format!("彻底删除中止：{error}。部分已确认项目可能已删除，请重新扫描。")
                        })?;
                    }
                    Ok(())
                } else {
                    if self
                        .identities
                        .last()
                        .is_none_or(|(_, identity)| identity.directory)
                    {
                        return Err("文件夹尚未完成内容检查，已取消彻底删除。".into());
                    }
                    fs::remove_file(&self.path).map_err(|error| format!("彻底删除失败：{error}"))
                }
            }
        }
    }

    fn execute_with(self, trash: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
        if self.mode != DeletionMode::Trash {
            return Err("确认的删除方式与回收站操作不一致。".into());
        }
        self.revalidate()?;
        trash(&self.path)
    }
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
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

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn move_to_trash(_: &Path) -> Result<(), String> {
    Err("此平台不支持移到废纸篓或回收站；没有删除任何文件。".into())
}

/// GIO's trash operation implements the FreeDesktop Trash specification and
/// returns an error when recycling is unavailable. Never use `gio remove`, `rm`,
/// or `--force`, and never fall back to permanent deletion.
#[cfg(target_os = "linux")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    linux_trash_with(path, |path| {
        std::process::Command::new("/usr/bin/gio")
            .arg("trash")
            .arg("--")
            .arg(path)
            .output()
    })
}

#[cfg(target_os = "linux")]
fn linux_trash_with(
    path: &Path,
    run: impl FnOnce(&Path) -> std::io::Result<std::process::Output>,
) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("回收操作需要绝对路径。".into());
    }
    let output = run(path).map_err(|error| {
        format!("无法启动 /usr/bin/gio：{error}。请安装 libglib2.0-bin；未尝试永久删除。")
    })?;
    if !output.status.success() {
        return Err(format!(
            "移到回收站失败：{}。未尝试永久删除。",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => {
            Err("回收操作结束但原路径仍存在，无法确认成功；请重新扫描。未尝试永久删除。".into())
        }
        Err(error) => Err(format!(
            "回收操作后无法确认原路径状态：{error}；请检查系统回收站并重新扫描。"
        )),
    }
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
            Self(fs::canonicalize(path).unwrap())
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_backend_failure_missing_gio_and_false_success_are_errors() {
        use std::os::unix::process::ExitStatusExt;
        let fixture = Fixture::new();
        let path = fixture.0.join("parent/child");
        assert!(linux_trash_with(&path, |_| Err(std::io::Error::from(
            std::io::ErrorKind::NotFound
        )))
        .unwrap_err()
        .contains("libglib2.0-bin"));
        for status in [0, 1 << 8] {
            let result = linux_trash_with(&path, |received| {
                assert_eq!(received, path);
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(status),
                    stdout: Vec::new(),
                    stderr: b"Trash unavailable".to_vec(),
                })
            });
            assert!(result.is_err());
            assert!(path.is_dir());
        }
        assert!(
            linux_trash_with(Path::new("relative"), |_| panic!("must not invoke backend")).is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_backend_mock_move_verifies_source_disappearance() {
        use std::os::unix::process::ExitStatusExt;
        let fixture = Fixture::new();
        let path = fixture.0.join("parent/child");
        linux_trash_with(&path, |received| {
            fs::rename(received, fixture.0.join("mock-trash"))?;
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        })
        .unwrap();
        assert!(fixture.0.join("mock-trash").is_dir());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_rejects_symlink_ancestor_above_scan_root() {
        let fixture = Fixture::new();
        let mut scan = fixture.scan();
        let container = Fixture::new();
        symlink(&fixture.0, container.0.join("alias")).unwrap();
        scan.root_path = container.0.join("alias");
        assert!(DeletionRequest::prepare(&scan, id(&scan, "child")).is_err());
        assert!(
            crate::linux_fs::validate_directory_ancestors(&container.0.join("alias/parent"))
                .is_err()
        );
    }

    /// Opt-in integration test: all source files, HOME and XDG Trash stay inside
    /// one unique synthetic fixture. It never accesses the user's normal Trash.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires /usr/bin/gio; uses only an isolated synthetic HOME/Trash"]
    fn linux_gio_round_trip_in_isolated_trash() {
        let fixture = Fixture::new();
        let data = fixture.0.join("data");
        fs::create_dir(&data).unwrap();
        let path = fixture.0.join("parent/child");
        fs::write(path.join("payload"), b"synthetic recoverable content").unwrap();
        let request = fixture.request();
        request
            .execute_with(|path| {
                linux_trash_with(path, |path| {
                    std::process::Command::new("/usr/bin/gio")
                        .args(["trash", "--"])
                        .arg(path)
                        .env("HOME", &fixture.0)
                        .env("XDG_DATA_HOME", &data)
                        .env("GIO_USE_VFS", "local")
                        .output()
                })
            })
            .unwrap();
        assert!(!path.exists());
        let files: Vec<_> = fs::read_dir(data.join("Trash/files"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let infos: Vec<_> = fs::read_dir(data.join("Trash/info"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(infos.len(), 1);
        assert_eq!(
            fs::read(files[0].path().join("payload")).unwrap(),
            b"synthetic recoverable content"
        );
        let info = fs::read_to_string(infos[0].path()).unwrap();
        assert!(info.starts_with("[Trash Info]\n"));
        assert!(info.contains("Path="));
        assert!(info.contains("DeletionDate="));
        // Restore only this fixture by a same-filesystem rename, then the fixture
        // cleanup removes the now-unused metadata and its own synthetic files.
        fs::rename(files[0].path(), &path).unwrap();
        assert_eq!(
            fs::read(path.join("payload")).unwrap(),
            b"synthetic recoverable content"
        );
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
    fn regular_file_trash_preserves_exact_path_and_never_falls_back() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        let request = DeletionRequest::prepare(&scan, id(&scan, "file")).unwrap();
        assert_eq!(request.mode(), DeletionMode::Trash);
        let expected = fixture.0.join("file");
        assert_eq!(request.path(), expected);
        assert_eq!(
            request.execute_with(|path| {
                assert_eq!(path, expected);
                Err("mock trash unavailable".into())
            }),
            Err("mock trash unavailable".into())
        );
        assert_eq!(fs::read(expected).unwrap(), b"synthetic fixture");
    }

    #[test]
    fn permanent_file_deletion_only_removes_confirmed_synthetic_file() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        let request =
            DeletionRequest::prepare_with_mode(&scan, id(&scan, "file"), DeletionMode::Permanent)
                .unwrap();
        assert_eq!(request.mode(), DeletionMode::Permanent);
        request.execute().unwrap();
        assert!(!fixture.0.join("file").exists());
        assert!(fixture.0.join("parent/child").is_dir());
        assert!(fixture.0.is_dir());
    }

    fn permanent_folder(fixture: &Fixture) -> DeletionRequest {
        let scan = fixture.scan();
        DeletionRequest::prepare_with_mode(&scan, id(&scan, "parent"), DeletionMode::Permanent)
            .unwrap()
    }

    #[test]
    fn permanent_folder_removes_nested_and_empty_unicode_directories() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.0.join("parent/中文-é/empty")).unwrap();
        fs::write(fixture.0.join("parent/中文-é/内容🦀"), b"payload").unwrap();
        fs::write(fixture.0.join("parent/child/nested"), b"nested").unwrap();
        fs::hard_link(
            fixture.0.join("parent/child/nested"),
            fixture.0.join("outside-link"),
        )
        .unwrap();
        permanent_folder(&fixture).execute().unwrap();
        assert!(!fixture.0.join("parent").exists());
        assert_eq!(fs::read(fixture.0.join("outside-link")).unwrap(), b"nested");
        assert_eq!(
            fs::read(fixture.0.join("file")).unwrap(),
            b"synthetic fixture"
        );
    }

    #[test]
    fn target_only_folder_request_cannot_execute_without_snapshot() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        let request = DeletionRequest::prepare_target_with_mode(
            &scan,
            id(&scan, "parent"),
            DeletionMode::Permanent,
        )
        .unwrap();
        assert!(request.subtree.is_none());
        assert!(request.execute().is_err());
        assert!(fixture.0.join("parent/child").is_dir());
        let request = DeletionRequest::prepare_target_with_mode(
            &scan,
            id(&scan, "parent"),
            DeletionMode::Permanent,
        )
        .unwrap();
        request.snapshot_contents().unwrap().execute().unwrap();
        assert!(!fixture.0.join("parent").exists());
    }

    #[test]
    fn cancelled_folder_snapshot_never_deletes() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        let request = DeletionRequest::prepare_target_with_mode(
            &scan,
            id(&scan, "parent"),
            DeletionMode::Permanent,
        )
        .unwrap();
        assert!(request
            .snapshot_contents_with_cancel(&AtomicBool::new(true))
            .is_err());
        assert!(fixture.0.join("parent/child").is_dir());
        assert!(snapshot_subtree(
            &fixture.0.join("parent"),
            directory_identity(&fixture.0).unwrap().device,
            &AtomicBool::new(true)
        )
        .is_err());
        assert!(fixture.0.join("parent/child").is_dir());
    }

    #[test]
    fn dropping_folder_confirmation_keeps_everything() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("parent/child/payload"), b"payload").unwrap();
        drop(permanent_folder(&fixture));
        assert_eq!(
            fs::read(fixture.0.join("parent/child/payload")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn changed_descendants_cancel_before_any_deletion() {
        for change in ["add", "remove", "replace", "edit", "directory"] {
            let fixture = Fixture::new();
            let payload = fixture.0.join("parent/child/payload");
            fs::write(&payload, b"original").unwrap();
            fs::write(fixture.0.join("parent/untouched"), b"untouched").unwrap();
            let request = permanent_folder(&fixture);
            match change {
                "add" => fs::write(fixture.0.join("parent/new-file"), b"new").unwrap(),
                "remove" => fs::rename(&payload, fixture.0.join("saved")).unwrap(),
                "replace" => {
                    fs::rename(&payload, fixture.0.join("saved")).unwrap();
                    fs::write(&payload, b"replacement").unwrap();
                }
                "edit" => fs::write(&payload, b"different size payload").unwrap(),
                _ => {
                    fs::rename(
                        fixture.0.join("parent/child"),
                        fixture.0.join("saved-child"),
                    )
                    .unwrap();
                    fs::create_dir(fixture.0.join("parent/child")).unwrap();
                }
            }
            assert!(request.execute().is_err(), "{change}");
            assert_eq!(
                fs::read(fixture.0.join("parent/untouched")).unwrap(),
                b"untouched"
            );
            assert!(fixture.0.join("parent/child").is_dir());
        }
    }

    #[cfg(unix)]
    #[test]
    fn descendant_symlinks_are_rejected_and_outside_targets_survive() {
        for after_confirmation in [false, true] {
            let fixture = Fixture::new();
            let outside = Fixture::new();
            let request = after_confirmation.then(|| permanent_folder(&fixture));
            symlink(&outside.0, fixture.0.join("parent/child/outside")).unwrap();
            if let Some(request) = request {
                assert!(request.execute().is_err());
            } else {
                let scan = fixture.scan();
                assert!(DeletionRequest::prepare_with_mode(
                    &scan,
                    id(&scan, "parent"),
                    DeletionMode::Permanent
                )
                .is_err());
            }
            assert_eq!(
                fs::read(outside.0.join("file")).unwrap(),
                b"synthetic fixture"
            );
            assert!(fs::symlink_metadata(fixture.0.join("parent/child/outside"))
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn permanent_folder_rejects_non_unicode_descendant() {
        let fixture = Fixture::new();
        let name = OsString::from_vec(b"invalid-\xff".to_vec());
        let path = fixture.0.join("parent").join(name);
        fs::write(&path, b"preserve").unwrap();
        let scan = fixture.scan();
        assert!(DeletionRequest::prepare_with_mode(
            &scan,
            id(&scan, "parent"),
            DeletionMode::Permanent
        )
        .is_err());
        assert_eq!(fs::read(path).unwrap(), b"preserve");
    }

    #[cfg(windows)]
    #[test]
    fn permanent_folder_rejects_descendant_junction() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let request = permanent_folder(&fixture);
        let junction = fixture.0.join("parent/child/junction");
        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside.0)
            .status()
            .unwrap();
        assert!(status.success(), "synthetic junction creation failed");
        assert!(request.execute().is_err());
        let scan = fixture.scan();
        assert!(DeletionRequest::prepare_with_mode(
            &scan,
            id(&scan, "parent"),
            DeletionMode::Permanent
        )
        .is_err());
        assert_eq!(
            fs::read(outside.0.join("file")).unwrap(),
            b"synthetic fixture"
        );
        fs::remove_dir(junction).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn subtree_mount_guard_rejects_nested_and_file_bind_mounts() {
        for mount in [
            "/scan/parent",
            "/scan/parent/child",
            "/scan/parent/child/file",
        ] {
            let description = format!("1 0 8:1 / / rw - ext4 /dev/sda rw\n2 1 8:1 /source {mount} rw - ext4 /dev/sda rw\n");
            let mounts = crate::linux_fs::MountTable::parse(description.as_bytes()).unwrap();
            assert!(reject_linux_subtree_mounts(Path::new("/scan/parent"), &mounts).is_err());
            assert!(reject_linux_subtree_mounts(Path::new("/scan/sibling"), &mounts).is_ok());
        }
    }

    #[test]
    fn permanent_deletion_preserves_other_hard_links() {
        let fixture = Fixture::new();
        fs::hard_link(fixture.0.join("file"), fixture.0.join("other-link")).unwrap();
        let scan = fixture.scan();
        DeletionRequest::prepare_with_mode(&scan, id(&scan, "file"), DeletionMode::Permanent)
            .unwrap()
            .execute()
            .unwrap();
        assert!(!fixture.0.join("file").exists());
        assert_eq!(
            fs::read(fixture.0.join("other-link")).unwrap(),
            b"synthetic fixture"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires /usr/bin/gio; uses only an isolated synthetic HOME/Trash"]
    fn linux_gio_file_round_trip_in_isolated_trash() {
        let fixture = Fixture::new();
        let data = fixture.0.join("data");
        fs::create_dir(&data).unwrap();
        let scan = fixture.scan();
        let request = DeletionRequest::prepare(&scan, id(&scan, "file")).unwrap();
        request
            .execute_with(|path| {
                linux_trash_with(path, |path| {
                    std::process::Command::new("/usr/bin/gio")
                        .args(["trash", "--"])
                        .arg(path)
                        .env("HOME", &fixture.0)
                        .env("XDG_DATA_HOME", &data)
                        .env("GIO_USE_VFS", "local")
                        .output()
                })
            })
            .unwrap();
        assert!(!fixture.0.join("file").exists());
        let files: Vec<_> = fs::read_dir(data.join("Trash/files"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let infos: Vec<_> = fs::read_dir(data.join("Trash/info"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(infos.len(), 1);
        assert_eq!(fs::read(files[0].path()).unwrap(), b"synthetic fixture");
        let info = fs::read_to_string(infos[0].path()).unwrap();
        assert!(info.starts_with("[Trash Info]\n"));
        assert!(info.contains("Path="));
        assert!(info.contains("DeletionDate="));
        fs::rename(files[0].path(), fixture.0.join("file")).unwrap();
        assert_eq!(
            fs::read(fixture.0.join("file")).unwrap(),
            b"synthetic fixture"
        );
    }

    #[test]
    fn permanent_mode_rejects_root_and_cannot_use_trash_dispatch() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        for target in [scan.root, usize::MAX] {
            assert!(
                DeletionRequest::prepare_with_mode(&scan, target, DeletionMode::Permanent).is_err()
            );
        }
        let request =
            DeletionRequest::prepare_with_mode(&scan, id(&scan, "file"), DeletionMode::Permanent)
                .unwrap();
        assert!(request
            .execute_with(|_| panic!("must not change confirmed mode"))
            .is_err());
        assert!(fixture.0.join("file").is_file());
    }

    #[test]
    fn file_replacement_kind_change_and_disappearance_cancel_both_modes() {
        for mode in [DeletionMode::Trash, DeletionMode::Permanent] {
            for replacement in ["file", "directory", "missing"] {
                let fixture = Fixture::new();
                let scan = fixture.scan();
                let request =
                    DeletionRequest::prepare_with_mode(&scan, id(&scan, "file"), mode).unwrap();
                let path = fixture.0.join("file");
                fs::rename(&path, fixture.0.join("saved-file")).unwrap();
                match replacement {
                    "file" => fs::write(&path, b"replacement").unwrap(),
                    "directory" => fs::create_dir(&path).unwrap(),
                    _ => (),
                }
                let result = if mode == DeletionMode::Trash {
                    request.execute_with(|_| panic!("must not trash replacement"))
                } else {
                    request.execute()
                };
                assert!(result.is_err());
                assert_eq!(
                    fs::read(fixture.0.join("saved-file")).unwrap(),
                    b"synthetic fixture"
                );
                if replacement == "file" {
                    assert_eq!(fs::read(&path).unwrap(), b"replacement");
                }
                if replacement == "directory" {
                    assert!(path.is_dir());
                }
            }
        }
    }

    #[test]
    fn permanent_file_rejects_replaced_ancestor() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("parent/nested-file"), b"original").unwrap();
        let scan = fixture.scan();
        let request = DeletionRequest::prepare_with_mode(
            &scan,
            id(&scan, "nested-file"),
            DeletionMode::Permanent,
        )
        .unwrap();
        fs::rename(fixture.0.join("parent"), fixture.0.join("saved-parent")).unwrap();
        fs::create_dir(fixture.0.join("parent")).unwrap();
        fs::write(fixture.0.join("parent/nested-file"), b"replacement").unwrap();
        assert!(request.execute().is_err());
        assert_eq!(
            fs::read(fixture.0.join("parent/nested-file")).unwrap(),
            b"replacement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn permanent_file_rejects_symlink_swap() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        let request =
            DeletionRequest::prepare_with_mode(&scan, id(&scan, "file"), DeletionMode::Permanent)
                .unwrap();
        fs::rename(fixture.0.join("file"), fixture.0.join("saved-file")).unwrap();
        symlink("saved-file", fixture.0.join("file")).unwrap();
        assert!(request.execute().is_err());
        assert!(fs::symlink_metadata(fixture.0.join("file"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read(fixture.0.join("saved-file")).unwrap(),
            b"synthetic fixture"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_rejects_symlink_above_scan_root() {
        let fixture = Fixture::new();
        let container = Fixture::new();
        symlink(&fixture.0, container.0.join("alias")).unwrap();
        let mut scan = fixture.scan();
        scan.root_path = container.0.join("alias");
        assert!(DeletionRequest::prepare_with_mode(
            &scan,
            id(&scan, "file"),
            DeletionMode::Permanent
        )
        .is_err());
    }

    #[test]
    fn rejects_root_and_invalid_ids() {
        let fixture = Fixture::new();
        let scan = fixture.scan();
        for target in [scan.root, usize::MAX] {
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

    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
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
    for (index, ancestor) in absolute.ancestors().enumerate() {
        let identity = if index == 0 {
            entry_identity(ancestor)?
        } else {
            directory_identity(ancestor)?
        };
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
                || entry_identity(&path).ok().as_ref() != Some(&self.expected)
            {
                return self.reject("Shell 项目与已确认的项目不一致，已取消操作。");
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
                return self.reject("未能确认项目已进入回收站。请检查项目位置。");
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
        let expected = entry_identity(path)?;
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
                        directory: true,
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
