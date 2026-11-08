//! Iterative, read-only filesystem accounting. Symlinks are never intentionally
//! followed and child mount points are not traversed. This is a best-effort view
//! of a live filesystem, not an atomic snapshot. APFS clones/compression may share
//! blocks that cannot be attributed exactly using portable filesystem metadata.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub type NodeId = usize;
const MAX_ISSUES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub path: PathBuf,
    pub name: String,
    pub kind: NodeKind,
    pub own_logical_bytes: u64,
    pub own_allocated_bytes: u64,
    pub total_logical_bytes: u64,
    pub total_allocated_bytes: u64,
    /// Regular-file pathnames, including this node when it is a regular file.
    pub file_count: u64,
    /// Directories, including this node when it is a directory.
    pub directory_count: u64,
    /// All issues in the subtree, even when detailed issue storage is capped.
    pub issue_count: u64,
    pub error: Option<String>,
    pub hard_link_duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug)]
pub struct ScanOutcome {
    pub nodes: Vec<Node>,
    pub root: NodeId,
    pub elapsed: Duration,
    pub cancelled: bool,
    /// At most 500 details. The root issue_count retains the full count.
    pub issues: Vec<ScanIssue>,
}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub entries: u64,
    pub files: u64,
    pub directories: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub issues: u64,
    pub current_path: PathBuf,
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

fn sizes(metadata: &Metadata, seen: &mut HashSet<(u64, u64)>) -> (u64, bool) {
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

fn make_node(
    id: NodeId,
    parent: Option<NodeId>,
    path: PathBuf,
    metadata: Option<&Metadata>,
    seen: &mut HashSet<(u64, u64)>,
) -> Node {
    let kind = match metadata {
        Some(m) if m.is_dir() => NodeKind::Directory,
        Some(m) if m.is_file() => NodeKind::File,
        Some(m) if m.file_type().is_symlink() => NodeKind::Symlink,
        _ => NodeKind::Other,
    };
    let logical = metadata.map_or(0, |m| if m.is_dir() { 0 } else { m.len() });
    let (allocated, duplicate) = metadata.map_or((0, false), |m| sizes(m, seen));
    Node {
        id,
        parent,
        children: Vec::new(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path,
        kind,
        own_logical_bytes: logical,
        own_allocated_bytes: allocated,
        total_logical_bytes: logical,
        total_allocated_bytes: allocated,
        file_count: u64::from(kind == NodeKind::File),
        directory_count: u64::from(kind == NodeKind::Directory),
        issue_count: 0,
        error: None,
        hard_link_duplicate: duplicate,
    }
}

fn issue(node: &mut Node, issues: &mut Vec<ScanIssue>, state: &mut ScanProgress, message: String) {
    node.issue_count = node.issue_count.saturating_add(1);
    state.issues = state.issues.saturating_add(1);
    // Keep the first per-node message, rather than unbounded repeated messages.
    if node.error.is_none() {
        node.error = Some(message.clone());
    }
    if issues.len() < MAX_ISSUES {
        issues.push(ScanIssue {
            path: node.path.clone(),
            message,
        });
    }
}

fn count(node: &Node, state: &mut ScanProgress) {
    state.entries = state.entries.saturating_add(1);
    state.files = state.files.saturating_add(node.file_count);
    state.directories = state.directories.saturating_add(node.directory_count);
    state.logical_bytes = state.logical_bytes.saturating_add(node.own_logical_bytes);
    state.allocated_bytes = state
        .allocated_bytes
        .saturating_add(node.own_allocated_bytes);
    state.current_path.clone_from(&node.path);
}

/// Scan one directory without recursion on the Rust call stack.
///
/// Invalid/non-directory roots return an error. Descendant failures are retained
/// as issues and do not abort other subtrees. Cancellation returns the discovered
/// portion with `cancelled = true`; totals must then be labelled partial.
pub fn scan(
    path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanProgress),
) -> io::Result<ScanOutcome> {
    let start = Instant::now();
    // Make relative input independent of the caller's later working-directory changes.
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a directory, not a file or symbolic link",
        ));
    }
    let root_device = device(&metadata);
    let mut seen = HashSet::new();
    let mut nodes = vec![make_node(0, None, path, Some(&metadata), &mut seen)];
    let mut pending = vec![0];
    let mut issues = Vec::new();
    let mut state = ScanProgress::default();
    count(&nodes[0], &mut state);
    progress(state.clone());
    let mut last_progress = Instant::now();
    let mut cancelled = false;
    'walk: while let Some(id) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        // Recheck before opening: directories may have changed since discovery.
        match fs::symlink_metadata(&nodes[id].path) {
            Ok(m) if m.is_dir() && device(&m) == root_device => {}
            Ok(_) => {
                issue(
                    &mut nodes[id],
                    &mut issues,
                    &mut state,
                    "Directory changed type or device during scan; skipped".into(),
                );
                continue;
            }
            Err(e) => {
                issue(&mut nodes[id], &mut issues, &mut state, e.to_string());
                continue;
            }
        }
        let entries = match fs::read_dir(&nodes[id].path) {
            Ok(entries) => entries,
            Err(e) => {
                issue(&mut nodes[id], &mut issues, &mut state, e.to_string());
                continue;
            }
        };
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'walk;
            }
            match entry {
                Err(e) => issue(&mut nodes[id], &mut issues, &mut state, e.to_string()),
                Ok(entry) => {
                    let child_path = entry.path();
                    let metadata = fs::symlink_metadata(&child_path);
                    let child_id = nodes.len();
                    let mut child = make_node(
                        child_id,
                        Some(id),
                        child_path,
                        metadata.as_ref().ok(),
                        &mut seen,
                    );
                    match metadata {
                        Err(e) => issue(&mut child, &mut issues, &mut state, e.to_string()),
                        Ok(m) if child.kind == NodeKind::Directory => {
                            if device(&m) != root_device {
                                issue(
                                    &mut child,
                                    &mut issues,
                                    &mut state,
                                    "Mount point skipped (different filesystem)".into(),
                                );
                            } else {
                                pending.push(child_id);
                            }
                        }
                        _ => {}
                    }
                    count(&child, &mut state);
                    nodes[id].children.push(child_id);
                    nodes.push(child);
                }
            }
            if last_progress.elapsed() >= Duration::from_millis(100) {
                progress(state.clone());
                last_progress = Instant::now();
            }
        }
    }
    // Every child is allocated after its parent, so reverse order is postorder.
    for id in (1..nodes.len()).rev() {
        let parent = nodes[id].parent.expect("non-root nodes have parents");
        let (before, after) = nodes.split_at_mut(id);
        let child = &after[0];
        let parent = &mut before[parent];
        parent.total_logical_bytes = parent
            .total_logical_bytes
            .saturating_add(child.total_logical_bytes);
        parent.total_allocated_bytes = parent
            .total_allocated_bytes
            .saturating_add(child.total_allocated_bytes);
        parent.file_count = parent.file_count.saturating_add(child.file_count);
        parent.directory_count = parent.directory_count.saturating_add(child.directory_count);
        parent.issue_count = parent.issue_count.saturating_add(child.issue_count);
    }
    for id in 0..nodes.len() {
        let mut children = std::mem::take(&mut nodes[id].children);
        children.sort_unstable_by(|a, b| {
            nodes[*b]
                .total_allocated_bytes
                .cmp(&nodes[*a].total_allocated_bytes)
                .then_with(|| nodes[*a].name.cmp(&nodes[*b].name))
                .then_with(|| a.cmp(b))
        });
        nodes[id].children = children;
    }
    progress(state);
    Ok(ScanOutcome {
        nodes,
        root: 0,
        elapsed: start.elapsed(),
        cancelled,
        issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
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
        let mut n = make_node(0, None, PathBuf::from("missing"), None, &mut HashSet::new());
        let mut issues = Vec::new();
        let mut p = ScanProgress::default();
        for _ in 0..600 {
            issue(&mut n, &mut issues, &mut p, "error".into());
        }
        assert_eq!(issues.len(), 500);
        assert_eq!(n.issue_count, 600);
        assert_eq!(p.issues, 600);
    }
}
