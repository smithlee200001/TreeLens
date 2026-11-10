use super::*;
use crate::features::scan::scan;
#[cfg(target_os = "linux")]
use crate::platform::delete::linux_trash_with;
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
fn linux_rejects_symlink_ancestor_above_scan_root() {
    let fixture = Fixture::new();
    let mut scan = fixture.scan();
    let container = Fixture::new();
    symlink(&fixture.0, container.0.join("alias")).unwrap();
    scan.root_path = container.0.join("alias");
    assert!(DeletionRequest::prepare(&scan, id(&scan, "child")).is_err());
    assert!(crate::platform::fs::linux::validate_directory_ancestors(
        &container.0.join("alias/parent")
    )
    .is_err());
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
    DeletionRequest::prepare_with_mode(&scan, id(&scan, "parent"), DeletionMode::Permanent).unwrap()
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
    assert!(
        DeletionRequest::prepare_with_mode(&scan, id(&scan, "file"), DeletionMode::Permanent)
            .is_err()
    );
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
