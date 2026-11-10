pub(super) use super::unix::identity;
use crate::platform::fs::linux::MountTable;
use std::{fs, path::Path};
pub(crate) fn validate_root(path: &Path) -> Result<(), String> {
    crate::platform::fs::linux::validate_directory_ancestors(path)
        .map_err(|error| format!("无法安全检查 Linux 文件夹路径：{error}"))
}
pub(crate) fn validate_target(original_root: &Path, path: &Path) -> Result<(), String> {
    crate::platform::fs::linux::validate_directory_ancestors(original_root)
        .and_then(|()| {
            crate::platform::fs::linux::validate_directory_ancestors(
                path.parent()
                    .ok_or_else(|| std::io::Error::other("Missing parent"))?,
            )
        })
        .map_err(|error| format!("无法安全检查 Linux 文件夹路径：{error}"))?;
    let mounts = MountTable::read()
        .map_err(|error| format!("无法读取 Linux 挂载信息，已取消删除操作：{error}"))?;
    reject_linux_subtree_mounts(path, &mounts)
}
pub(crate) fn validate_entry_path(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_subtree_root(path: &Path) -> Result<(), String> {
    reject_linux_subtree_mounts(path, &MountTable::read().map_err(|e| e.to_string())?)
}
fn reject_linux_subtree_mounts(path: &Path, mounts: &MountTable) -> Result<(), String> {
    if mounts.has_mount_at_or_below(path) {
        return Err("不能删除挂载点或包含挂载点的项目（包括 bind mount）。".into());
    }
    Ok(())
}

/// GIO's trash operation implements the FreeDesktop Trash specification and
/// returns an error when recycling is unavailable. Never use `gio remove`, `rm`,
/// or `--force`, and never fall back to permanent deletion.
pub(crate) fn move_to_trash(path: &Path) -> Result<(), String> {
    linux_trash_with(path, |path| {
        std::process::Command::new("/usr/bin/gio")
            .arg("trash")
            .arg("--")
            .arg(path)
            .output()
    })
}

pub(crate) fn linux_trash_with(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
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
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only remove this test's unique synthetic fixture, never call Trash.
            let _ = fs::remove_dir_all(&self.0);
        }
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
    fn subtree_mount_guard_rejects_nested_and_file_bind_mounts() {
        for mount in [
            "/scan/parent",
            "/scan/parent/child",
            "/scan/parent/child/file",
        ] {
            let description = format!("1 0 8:1 / / rw - ext4 /dev/sda rw\n2 1 8:1 /source {mount} rw - ext4 /dev/sda rw\n");
            let mounts =
                crate::platform::fs::linux::MountTable::parse(description.as_bytes()).unwrap();
            assert!(reject_linux_subtree_mounts(Path::new("/scan/parent"), &mounts).is_err());
            assert!(reject_linux_subtree_mounts(Path::new("/scan/sibling"), &mounts).is_ok());
        }
    }
}
