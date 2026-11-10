//! One iterative traversal, independent of native filesystem details.
use super::model::{
    reconstruct_path, Node, NodeId, NodeKind, ScanIssue, ScanOutcome, ScanProgress,
};
use crate::platform::scan::{Adapter, EntryAccounting, EntryKind, NativeAdapter};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_ISSUES: usize = 500;

/// Avoid allocator churn for tiny savings; no element cloning is involved.
fn shrink_excess<T>(items: &mut Vec<T>) {
    let spare = items.capacity().saturating_sub(items.len());
    if spare >= items.len() / 4 && spare.saturating_mul(std::mem::size_of::<T>()) >= 4096 {
        items.shrink_to_fit();
    }
}

pub(super) fn make_node(
    id: NodeId,
    parent: Option<NodeId>,
    name: Box<OsStr>,
    entry: &EntryAccounting,
) -> Node {
    let kind = match entry.kind {
        EntryKind::Directory => NodeKind::Directory,
        EntryKind::File => NodeKind::File,
        EntryKind::Symlink => NodeKind::Symlink,
        EntryKind::Other => NodeKind::Other,
    };
    let logical = entry.logical;
    let allocated = entry.allocated;
    let duplicate = entry.duplicate;
    let account = entry.account;
    Node {
        id,
        parent,
        children: Vec::new(),
        name,
        kind,
        own_logical_bytes: logical,
        own_allocated_bytes: allocated,
        total_logical_bytes: logical,
        total_allocated_bytes: allocated,
        file_count: u64::from(account && kind == NodeKind::File),
        directory_count: u64::from(account && kind == NodeKind::Directory),
        issue_count: 0,
        error: None,
        hard_link_duplicate: duplicate,
    }
}

pub(super) fn issue(
    node: &mut Node,
    path: &Path,
    issues: &mut Vec<ScanIssue>,
    state: &mut ScanProgress,
    message: String,
) {
    node.issue_count = node.issue_count.saturating_add(1);
    state.issues = state.issues.saturating_add(1);
    // Keep the first per-node message, rather than unbounded repeated messages.
    if node.error.is_none() {
        node.error = Some(message.clone().into_boxed_str());
    }
    if issues.len() < MAX_ISSUES {
        issues.push(ScanIssue {
            path: path.to_path_buf(),
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
}

/// Scan one directory without recursion on the Rust call stack.
///
/// Invalid/non-directory roots return an error. Descendant failures are retained
/// as issues and do not abort other subtrees. Cancellation returns the discovered
/// portion with `cancelled = true`; totals must then be labelled partial.
pub fn scan(
    path: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
) -> io::Result<ScanOutcome> {
    scan_with_adapter(path, cancel, progress, NativeAdapter::new())
}

// The adapter owns platform startup, no-follow checks and physical accounting.
// The service owns traversal, cancellation, bounded issues, totals and ordering.
pub(super) fn scan_with_adapter(
    path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanProgress),
    mut adapter: impl Adapter,
) -> io::Result<ScanOutcome> {
    let start = Instant::now();
    let (path, root_entry) = adapter.start(path)?;
    let root_name = path.file_name().unwrap_or(path.as_os_str()).into();
    let mut nodes = vec![make_node(0, None, root_name, &root_entry)];
    let root_path = path;
    let mut latest_id = 0;
    let mut pending = vec![0];
    let mut issues = Vec::new();
    let mut state = ScanProgress::default();
    count(&nodes[0], &mut state);
    state.current_path = root_path.clone();
    progress(state.clone());
    let mut last_progress = Instant::now();
    let mut cancelled = false;
    'walk: while let Some(id) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let directory_path = reconstruct_path(&nodes, &root_path, id);
        latest_id = id;
        // Recheck before opening: directories may have changed since discovery.
        if let Err(message) = adapter.validate_directory(&directory_path) {
            issue(
                &mut nodes[id],
                &directory_path,
                &mut issues,
                &mut state,
                message,
            );
            continue;
        }
        let entries = match fs::read_dir(&directory_path) {
            Ok(entries) => entries,
            Err(e) => {
                issue(
                    &mut nodes[id],
                    &directory_path,
                    &mut issues,
                    &mut state,
                    e.to_string(),
                );
                continue;
            }
        };
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'walk;
            }
            match entry {
                Err(e) => {
                    latest_id = id;
                    issue(
                        &mut nodes[id],
                        &directory_path,
                        &mut issues,
                        &mut state,
                        e.to_string(),
                    );
                }
                Ok(entry) => {
                    let child_path = entry.path();
                    let entry_accounting = adapter.inspect_entry(&entry, &child_path);
                    let child_id = nodes.len();
                    let mut child = make_node(
                        child_id,
                        Some(id),
                        entry.file_name().into_boxed_os_str(),
                        &entry_accounting,
                    );
                    if entry_accounting.descend {
                        pending.push(child_id);
                    }
                    if let Some(message) = entry_accounting.issue {
                        issue(&mut child, &child_path, &mut issues, &mut state, message);
                    }
                    count(&child, &mut state);
                    nodes[id].children.push(child_id);
                    nodes.push(child);
                    latest_id = child_id;
                }
            }
            if last_progress.elapsed() >= Duration::from_millis(100) {
                state.current_path = reconstruct_path(&nodes, &root_path, latest_id);
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
        shrink_excess(&mut children);
        nodes[id].children = children;
    }
    // Free traversal-only storage before potentially resizing the arena.
    drop(pending);
    drop(adapter);
    shrink_excess(&mut nodes);
    state.current_path = reconstruct_path(&nodes, &root_path, latest_id);
    progress(state);
    Ok(ScanOutcome {
        nodes,
        root: 0,
        root_path,
        elapsed: start.elapsed(),
        cancelled,
        issues,
    })
}
