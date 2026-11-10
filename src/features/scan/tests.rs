#[cfg(windows)]
use super::model::reconstruct_path;
#[cfg(any(target_os = "linux", windows))]
use super::service::scan_with_adapter;
use super::service::{issue, make_node};
use super::*;
#[cfg(any(target_os = "linux", windows))]
use crate::platform::scan::NativeAdapter;
use crate::platform::scan::{EntryAccounting, FileKey};
use std::ffi::OsStr;
use std::fs;
#[cfg(any(target_os = "linux", windows))]
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::Duration;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "tree-lens-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn scan(&self) -> ScanOutcome {
        scan(&self.0, &AtomicBool::new(false), |_| {}).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_bind_mount_is_visible_but_not_counted_or_traversed() {
    let temp = Temp::new();
    let mounted = temp.0.join("bind");
    fs::create_dir(&mounted).unwrap();
    fs::write(mounted.join("must-not-count"), vec![1u8; 8192]).unwrap();
    fs::write(temp.0.join("ordinary"), b"ok").unwrap();
    let table = crate::platform::fs::linux::MountTable::parse(
        format!(
            "1 0 0:1 / / rw - tmpfs tmpfs rw\n2 1 0:1 /source {} rw - tmpfs tmpfs rw\n",
            mounted.display()
        )
        .as_bytes(),
    )
    .unwrap();
    let scan = scan_impl(&temp.0, &AtomicBool::new(false), |_| {}, Some(table)).unwrap();
    let node = scan
        .nodes
        .iter()
        .find(|node| node.name.as_ref() == OsStr::new("bind"))
        .unwrap();
    assert!(node.children.is_empty());
    assert_eq!(node.total_allocated_bytes, 0);
    assert_eq!(node.issue_count, 1);
    assert_eq!(scan.nodes[scan.root].total_logical_bytes, 2);
    assert_eq!(scan.nodes[scan.root].file_count, 1);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_file_bind_mount_does_not_steal_hardlink_allocation() {
    use std::os::unix::fs::MetadataExt;
    let temp = Temp::new();
    let regular = temp.0.join("ordinary");
    let mounted = temp.0.join("file-mount");
    fs::write(&regular, vec![7u8; 8192]).unwrap();
    fs::hard_link(&regular, &mounted).unwrap();
    let table = crate::platform::fs::linux::MountTable::parse(
        format!(
            "1 0 0:1 / / rw - tmpfs tmpfs rw\n2 1 0:1 /source {} rw - tmpfs tmpfs rw\n",
            mounted.display()
        )
        .as_bytes(),
    )
    .unwrap();
    let scan = scan_impl(&temp.0, &AtomicBool::new(false), |_| {}, Some(table)).unwrap();
    let excluded = scan
        .nodes
        .iter()
        .find(|n| n.name.as_ref() == OsStr::new("file-mount"))
        .unwrap();
    assert_eq!(excluded.kind, NodeKind::File);
    assert_eq!(excluded.total_allocated_bytes, 0);
    assert_eq!(excluded.total_logical_bytes, 0);
    assert_eq!(excluded.file_count, 0);
    assert!(!excluded.hard_link_duplicate);
    assert_eq!(excluded.issue_count, 1);
    let file = scan
        .nodes
        .iter()
        .find(|n| n.name.as_ref() == OsStr::new("ordinary"))
        .unwrap();
    assert!(!file.hard_link_duplicate);
    assert_eq!(
        file.total_allocated_bytes,
        fs::metadata(regular).unwrap().blocks() * 512
    );
    assert_eq!(scan.nodes[0].file_count, 1);
    assert_eq!(scan.nodes[0].total_logical_bytes, 8192);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_rejects_root_reached_through_symlink_ancestor() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    fs::create_dir_all(temp.0.join("real/child")).unwrap();
    symlink(temp.0.join("real"), temp.0.join("alias")).unwrap();
    assert!(scan(&temp.0.join("alias/child"), &AtomicBool::new(false), |_| {}).is_err());
    assert!(scan(
        &temp.0.join("alias/../real"),
        &AtomicBool::new(false),
        |_| {}
    )
    .is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_normalizes_parent_components_after_no_follow_checks() {
    let temp = Temp::new();
    fs::create_dir(temp.0.join("child")).unwrap();
    let result = scan(&temp.0.join("child/.."), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(result.root_path, fs::canonicalize(&temp.0).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_dangling_link_is_not_followed() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    symlink("missing-target", temp.0.join("dangling")).unwrap();
    let scan = temp.scan();
    assert_eq!(scan.nodes[0].file_count, 0);
    assert_eq!(scan.nodes.len(), 2);
    assert!(scan.nodes.iter().any(|node| node.kind == NodeKind::Symlink));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Requires Unix socket permission; run explicitly on normal Linux/CI hosts"]
fn linux_socket_is_not_opened_for_reading() {
    use std::os::unix::net::UnixListener;
    let temp = Temp::new();
    let _listener = UnixListener::bind(temp.0.join("socket")).unwrap();
    let scan = temp.scan();
    assert_eq!(scan.nodes[0].file_count, 0);
    assert_eq!(scan.nodes.len(), 2);
    assert_eq!(scan.nodes[1].kind, NodeKind::Other);
}

#[test]
fn nested_aggregation_and_order() {
    let t = Temp::new();
    fs::create_dir(t.0.join("nested.app")).unwrap();
    fs::write(t.0.join("a"), b"abc").unwrap();
    fs::write(t.0.join("nested.app/b"), b"12345").unwrap();
    let s = t.scan();
    let root = &s.nodes[s.root];
    assert_eq!(root.total_logical_bytes, 8);
    assert_eq!(root.file_count, 2);
    assert_eq!(root.directory_count, 2);
    assert_eq!(
        root.total_allocated_bytes,
        s.nodes.iter().map(|n| n.own_allocated_bytes).sum::<u64>()
    );
    assert!(root
        .children
        .windows(2)
        .all(|w| s.nodes[w[0]].total_allocated_bytes >= s.nodes[w[1]].total_allocated_bytes));
    assert!(!s.cancelled);
    assert!(s.issues.is_empty());
}

#[test]
fn empty_directory() {
    let s = Temp::new().scan();
    assert_eq!(s.nodes.len(), 1);
    assert_eq!(s.nodes[0].file_count, 0);
    assert_eq!(s.nodes[0].directory_count, 1);
    assert_eq!(s.nodes[0].total_logical_bytes, 0);
}

#[test]
fn cancellation_and_final_progress() {
    let t = Temp::new();
    fs::write(t.0.join("file"), b"x").unwrap();
    let cancel = AtomicBool::new(false);
    let mut updates = Vec::new();
    let s = scan(&t.0, &cancel, |p| {
        updates.push(p);
        cancel.store(true, Ordering::Relaxed);
    })
    .unwrap();
    assert!(s.cancelled);
    assert_eq!(s.nodes.len(), 1);
    assert_eq!(updates.len(), 2);
    assert_eq!(
        updates.last().unwrap().allocated_bytes,
        s.nodes[0].total_allocated_bytes
    );
}

#[test]
fn invalid_roots() {
    let t = Temp::new();
    fs::write(t.0.join("file"), b"x").unwrap();
    for path in [t.0.join("file"), t.0.join("missing")] {
        assert!(scan(&path, &AtomicBool::new(false), |_| {}).is_err());
    }
}

#[cfg(unix)]
#[test]
fn symlink_cycle_is_not_followed() {
    use std::os::unix::fs::symlink;
    let t = Temp::new();
    symlink(&t.0, t.0.join("cycle")).unwrap();
    symlink(t.0.join("absent"), t.0.join("broken")).unwrap();
    let s = t.scan();
    assert_eq!(s.nodes.len(), 3);
    assert_eq!(s.nodes[0].directory_count, 1);
    assert_eq!(s.nodes[0].file_count, 0);
    assert!(s.nodes[1..].iter().all(|n| n.kind == NodeKind::Symlink));
    assert!(scan(&t.0.join("cycle"), &AtomicBool::new(false), |_| {}).is_err());
}

#[cfg(unix)]
#[test]
fn hardlinks_count_logical_twice_and_allocated_once() {
    use std::os::unix::fs::MetadataExt;
    let t = Temp::new();
    fs::write(t.0.join("a"), vec![42_u8; 8192]).unwrap();
    fs::hard_link(t.0.join("a"), t.0.join("b")).unwrap();
    let s = t.scan();
    assert_eq!(s.nodes[0].total_logical_bytes, 16384);
    assert_eq!(s.nodes[0].file_count, 2);
    assert_eq!(s.nodes.iter().filter(|n| n.hard_link_duplicate).count(), 1);
    assert_eq!(
        s.nodes[0].total_allocated_bytes - s.nodes[0].own_allocated_bytes,
        fs::metadata(t.0.join("a")).unwrap().blocks() * 512
    );
}

#[cfg(unix)]
#[test]
fn sparse_file_uses_blocks_not_length() {
    use std::os::unix::fs::MetadataExt;
    let t = Temp::new();
    let path = t.0.join("sparse");
    fs::File::create(&path)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let s = t.scan();
    let file = &s.nodes[1];
    assert_eq!(file.own_logical_bytes, 64 * 1024 * 1024);
    assert_eq!(
        file.own_allocated_bytes,
        fs::metadata(path).unwrap().blocks() * 512
    );
}

#[test]
fn errors_are_aggregated_when_directory_disappears() {
    let t = Temp::new();
    let mut first = true;
    let s = scan(&t.0, &AtomicBool::new(false), |_| {
        if first {
            first = false;
            fs::remove_dir(&t.0).unwrap();
        }
    })
    .unwrap();
    assert_eq!(s.nodes[0].issue_count, 1);
    assert_eq!(s.issues.len(), 1);
    assert!(s.nodes[0].error.is_some());
    assert!(!s.cancelled);
}

#[test]
fn issue_details_are_bounded_but_count_is_not() {
    let mut n = make_node(
        0,
        None,
        OsStr::new("missing").into(),
        &EntryAccounting::unknown(),
    );
    let mut issues = Vec::new();
    let mut p = ScanProgress::default();
    for _ in 0..600 {
        issue(
            &mut n,
            Path::new("missing"),
            &mut issues,
            &mut p,
            "error".into(),
        );
    }
    assert_eq!(issues.len(), 500);
    assert_eq!(n.issue_count, 600);
    assert_eq!(p.issues, 600);
}

#[test]
fn deep_paths_are_reconstructed_iteratively() {
    let t = Temp::new();
    let mut path = t.0.clone();
    for _ in 0..128 {
        path.push("d");
        fs::create_dir(&path).unwrap();
    }
    path.push("leaf");
    fs::write(&path, b"ok").unwrap();
    let s = t.scan();
    let leaf = s.nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
    assert_eq!(s.path_for(leaf.id), path);
    assert_eq!(s.path_for(s.root), t.0);
    assert_eq!(leaf.display_name(), "leaf");
    assert_eq!(s.nodes[s.root].directory_count, 129);
    assert_eq!(s.nodes[s.root].total_logical_bytes, 2);
    assert!(s.nodes[1..]
        .iter()
        .all(|n| !n.name.as_encoded_bytes().contains(&b'/')));
}

#[cfg(unix)]
#[test]
fn non_utf8_names_reconstruct_exact_paths_in_memory() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    // No filesystem fixture: APFS refuses invalid UTF-8 filenames, but
    // path reconstruction must retain exact bytes on every Unix platform.
    let names = [b"root-\xff".as_slice(), b"dir-\xfe", b"file-\xfd"];
    let mut nodes: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(id, name)| {
            make_node(
                id,
                id.checked_sub(1),
                OsString::from_vec(name.to_vec()).into_boxed_os_str(),
                &EntryAccounting::unknown(),
            )
        })
        .collect();
    nodes[0].children.push(1);
    nodes[1].children.push(2);
    let root_path = Path::new("/").join(nodes[0].name.as_ref());
    let directory = root_path.join(nodes[1].name.as_ref());
    let file = directory.join(nodes[2].name.as_ref());
    let scan = ScanOutcome {
        nodes,
        root: 0,
        root_path: root_path.clone(),
        elapsed: Duration::ZERO,
        cancelled: false,
        issues: Vec::new(),
    };
    assert_eq!(scan.path_for(0), root_path);
    assert_eq!(scan.path_for(1), directory);
    assert_eq!(scan.path_for(2), file);
    assert_eq!(scan.nodes[2].display_name(), "file-\u{fffd}");
    assert_ne!(
        scan.path_for(2),
        PathBuf::from(file.to_string_lossy().as_ref())
    );
}

// macOS APFS cannot create this fixture; the in-memory case above still runs.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_names_retain_exact_filesystem_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let t = Temp::new();
    let directory = t.0.join(OsString::from_vec(vec![b'd', 0xff]));
    fs::create_dir(&directory).unwrap();
    let file = directory.join(OsString::from_vec(vec![b'f', 0xfe]));
    fs::write(&file, b"exact").unwrap();
    let s = scan(&directory, &AtomicBool::new(false), |_| {}).unwrap();
    let leaf = s.nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
    assert_eq!(s.path_for(s.root), directory);
    assert_eq!(s.path_for(leaf.id), file);
    assert_eq!(fs::read(s.path_for(leaf.id)).unwrap(), b"exact");
    assert!(leaf.display_name().contains('\u{fffd}'));
    // Also traverse the invalid-UTF8 directory as a non-root node.
    let s = t.scan();
    let leaf = s.nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
    assert_eq!(s.path_for(leaf.id), file);
}

#[test]
fn final_progress_has_latest_exact_path() {
    let t = Temp::new();
    let path = t.0.join("only-file");
    fs::write(&path, b"content").unwrap();
    let mut updates = Vec::new();
    let s = scan(&t.0, &AtomicBool::new(false), |p| updates.push(p)).unwrap();
    assert_eq!(updates.first().unwrap().current_path, t.0);
    assert_eq!(updates.last().unwrap().current_path, path);
    assert_eq!(updates.last().unwrap().entries, s.nodes.len() as u64);
    assert_eq!(
        updates.last().unwrap().logical_bytes,
        s.nodes[s.root].total_logical_bytes
    );
}
#[cfg(windows)]
#[test]
fn windows_hardlinks_use_volume_and_file_identity() {
    let t = Temp::new();
    let original = t.0.join("original");
    fs::write(&original, vec![42; 8192]).unwrap();
    fs::hard_link(&original, t.0.join("duplicate")).unwrap();
    let expected = crate::platform::fs::windows::query(&original).unwrap();
    assert!(expected.allocated > 0);
    let s = t.scan();
    assert!(s.issues.is_empty(), "{:?}", s.issues);
    assert_eq!(s.nodes[0].total_logical_bytes, 16384);
    assert_eq!(s.nodes[0].total_allocated_bytes, expected.allocated);
    assert_eq!(s.nodes.iter().filter(|n| n.hard_link_duplicate).count(), 1);
}

// These Windows integration fixtures deliberately fail with diagnostics on
// unsupported filesystems; CI must use a local NTFS temp directory. Junction
// creation needs no symlink privilege, administrator rights, or Developer Mode.
#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("run Windows mklink");
    assert!(
        output.status.success(),
        "junction fixture failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[test]
fn windows_junctions_and_reparse_roots_are_not_followed() {
    let t = Temp::new();
    let outside = Temp::new();
    fs::write(outside.0.join("must-not-appear"), b"outside").unwrap();
    let link = t.0.join("junction");
    junction(&link, &outside.0);
    let s = t.scan();
    assert_eq!(s.nodes.len(), 2);
    assert_eq!(s.nodes[0].total_logical_bytes, 0);
    assert_eq!(s.nodes[0].issue_count, 1);
    assert!(scan(&link, &AtomicBool::new(false), |_| {}).is_err());
    fs::create_dir(outside.0.join("nested")).unwrap();
    assert!(scan(&link.join("nested"), &AtomicBool::new(false), |_| {}).is_err());
    fs::remove_dir(&link).unwrap();
    assert!(outside.0.join("must-not-appear").exists());
}

#[cfg(windows)]
#[test]
fn windows_junction_cycle_is_bounded() {
    let t = Temp::new();
    let link = t.0.join("cycle");
    junction(&link, &t.0);
    let s = t.scan();
    assert_eq!(s.nodes.len(), 2);
    assert_eq!(s.nodes[0].issue_count, 1);
    fs::remove_dir(link).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_sparse_file_counts_physical_allocation() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::{Ioctl::FSCTL_SET_SPARSE, IO::DeviceIoControl};
    let t = Temp::new();
    let path = t.0.join("sparse");
    let file = fs::File::create(&path).unwrap();
    let mut returned = 0;
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            FSCTL_SET_SPARSE,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .expect("sparse fixture requires a local filesystem supporting sparse files (NTFS)");
    file.set_len(64 * 1024 * 1024).unwrap();
    drop(file);
    let expected = crate::platform::fs::windows::query(&path)
        .unwrap()
        .allocated;
    assert!(expected < 64 * 1024 * 1024);
    let s = t.scan();
    assert!(s.issues.is_empty(), "{:?}", s.issues);
    assert_eq!(s.nodes[1].own_logical_bytes, 64 * 1024 * 1024);
    assert_eq!(s.nodes[1].own_allocated_bytes, expected);
}

#[cfg(windows)]
#[test]
fn windows_compressed_file_counts_compressed_storage() {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::{Ioctl::FSCTL_SET_COMPRESSION, IO::DeviceIoControl};
    let t = Temp::new();
    let path = t.0.join("compressed");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let compression: u16 = 1; // COMPRESSION_FORMAT_DEFAULT
    let mut returned = 0;
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            FSCTL_SET_COMPRESSION,
            Some((&compression as *const u16).cast()),
            2,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .expect("compressed fixture requires an NTFS volume supporting compression");
    file.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let expected = crate::platform::fs::windows::query(&path)
        .unwrap()
        .allocated;
    assert!(expected < 1024 * 1024);
    let s = t.scan();
    assert!(s.issues.is_empty(), "{:?}", s.issues);
    assert_eq!(s.nodes[1].own_allocated_bytes, expected);
    assert_eq!(s.nodes[1].own_logical_bytes, 1024 * 1024);
}

#[cfg(windows)]
#[test]
fn windows_names_preserve_unicode_and_unpaired_utf16() {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let t = Temp::new();
    let name = OsString::from("资料 🦀 e\u{301}.txt");
    let path = t.0.join(&name);
    fs::write(&path, b"exact").unwrap();
    let s = t.scan();
    assert_eq!(s.nodes[1].name.as_ref(), name.as_os_str());
    assert_eq!(s.path_for(1), path);
    // Windows paths can contain unpaired surrogates. Keep exact code units
    // in memory independently of the filesystem's filename restrictions.
    let units = [b'x' as u16, 0xd800, b'y' as u16];
    let n = make_node(
        1,
        Some(0),
        OsString::from_wide(&units).into_boxed_os_str(),
        &EntryAccounting::unknown(),
    );
    let path = reconstruct_path(&[s.nodes[0].clone(), n], &t.0, 1);
    assert_eq!(
        path.file_name().unwrap().encode_wide().collect::<Vec<_>>(),
        units
    );
}
#[cfg(windows)]
#[test]
fn windows_unqueryable_files_remain_visible_with_issues() {
    let t = Temp::new();
    let path = t.0.join("unqueryable");
    fs::write(&path, b"visible metadata").unwrap();
    let mut failures = 0;
    let s = scan_impl(
        &t.0,
        &AtomicBool::new(false),
        |_| {},
        |candidate| {
            if candidate == path {
                failures += 1;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected metadata denial",
                ))
            } else {
                crate::platform::fs::windows::query(candidate)
            }
        },
    )
    .unwrap();
    assert_eq!(failures, 1);
    assert_eq!(s.nodes.len(), 2);
    let child = &s.nodes[1];
    assert_eq!(child.kind, NodeKind::File);
    assert_eq!(child.own_logical_bytes, 16);
    assert_eq!(child.own_allocated_bytes, 0);
    assert_eq!(child.issue_count, 1);
    assert!(child
        .error
        .as_deref()
        .unwrap()
        .contains("Allocation/identity unavailable"));
    assert_eq!(s.nodes[0].issue_count, 1);
}
#[cfg(windows)]
#[test]
fn windows_exclusive_data_lock_does_not_require_file_data_access() {
    use std::os::windows::fs::OpenOptionsExt;
    let t = Temp::new();
    let path = t.0.join("locked");
    fs::write(&path, b"visible metadata").unwrap();
    let expected = crate::platform::fs::windows::query(&path)
        .unwrap()
        .allocated;
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    // Windows permits FILE_READ_ATTRIBUTES despite exclusive data sharing.
    // This protects the metadata-only contract against accidental widening.
    let s = t.scan();
    assert!(s.issues.is_empty(), "{:?}", s.issues);
    assert_eq!(s.nodes.len(), 2);
    assert_eq!(s.nodes[1].kind, NodeKind::File);
    assert_eq!(s.nodes[1].own_logical_bytes, 16);
    assert_eq!(s.nodes[1].own_allocated_bytes, expected);
}

#[cfg(windows)]
#[test]
fn windows_directory_replaced_by_junction_is_rechecked() {
    let t = Temp::new();
    let outside = Temp::new();
    fs::write(outside.0.join("must-not-appear"), b"outside").unwrap();
    let saved = t.0.with_extension("saved");
    let mut replaced = false;
    let result = scan(&t.0, &AtomicBool::new(false), |_| {
        if !replaced {
            fs::rename(&t.0, &saved).unwrap();
            junction(&t.0, &outside.0);
            replaced = true;
        }
    });
    // Restore the fixture before assertions, so normal cleanup cannot follow
    // the junction even on an assertion failure.
    fs::remove_dir(&t.0).unwrap();
    fs::rename(&saved, &t.0).unwrap();
    let s = result.unwrap();
    assert_eq!(s.nodes.len(), 1);
    assert_eq!(s.nodes[0].issue_count, 1);
    assert!(s.nodes[0]
        .error
        .as_deref()
        .unwrap()
        .contains("reparse point"));
    assert!(outside.0.join("must-not-appear").exists());
}
#[test]
fn hardlink_key_preserves_platform_storage_width() {
    #[cfg(unix)]
    assert_eq!(std::mem::size_of::<FileKey>(), 16);
    #[cfg(windows)]
    assert_eq!(std::mem::size_of::<FileKey>(), 32);
}

#[cfg(windows)]
#[test]
fn windows_drive_relative_roots_are_rejected() {
    for path in [r"C:folder", "C:", r"Z:relative\nested"] {
        let error = scan(Path::new(path), &AtomicBool::new(false), |_| {}).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("Drive-relative"));
    }
}

// Keep deterministic native failure/mount fixtures without exposing platform
// configuration in the public scan API.
#[cfg(target_os = "linux")]
fn scan_impl(
    path: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
    table: Option<crate::platform::fs::linux::MountTable>,
) -> io::Result<ScanOutcome> {
    scan_with_adapter(
        path,
        cancel,
        progress,
        NativeAdapter::with_mount_table(table),
    )
}

#[cfg(windows)]
fn scan_impl(
    path: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
    query: impl FnMut(&Path) -> io::Result<crate::platform::fs::windows::EntryInfo>,
) -> io::Result<ScanOutcome> {
    scan_with_adapter(path, cancel, progress, NativeAdapter::with_query(query))
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_excludes_mount_before_claiming_hardlink_identity() {
    use crate::platform::scan::Adapter;
    use std::os::unix::fs::MetadataExt;
    let temp = Temp::new();
    let original = temp.0.join("ordinary");
    let mount = temp.0.join("mounted");
    fs::write(&original, vec![7_u8; 8192]).unwrap();
    fs::hard_link(&original, &mount).unwrap();
    let table = crate::platform::fs::linux::MountTable::parse(
        format!(
            "1 0 0:1 / / rw - tmpfs tmpfs rw\n2 1 0:1 /source {} rw - tmpfs tmpfs rw\n",
            mount.display()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut adapter = NativeAdapter::with_mount_table(Some(table));
    adapter.start(&temp.0).unwrap();
    let entries: Vec<_> = fs::read_dir(&temp.0).unwrap().map(Result::unwrap).collect();
    // Force the excluded alias to arrive first; directory enumeration order
    // must not decide whether the real file receives physical allocation.
    let excluded =
        adapter.inspect_entry(entries.iter().find(|e| e.path() == mount).unwrap(), &mount);
    assert!(!excluded.account);
    assert!(!excluded.descend);
    assert!(!excluded.duplicate);
    assert_eq!((excluded.logical, excluded.allocated), (0, 0));
    assert!(excluded.issue.is_some());
    let ordinary = adapter.inspect_entry(
        entries.iter().find(|e| e.path() == original).unwrap(),
        &original,
    );
    assert!(ordinary.account);
    assert!(!ordinary.duplicate);
    assert_eq!(
        ordinary.allocated,
        fs::metadata(original).unwrap().blocks() * 512
    );
    assert!(ordinary.issue.is_none());
}
