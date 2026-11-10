//! GUI-independent scan lifecycle and virtual-tree state.
//!
//! Call `poll` periodically on the UI thread (including while search is pending),
//! and repaint when it returns true. No GUI objects cross into the scan thread.
use crate::features::explorer::tree::{sort_tree, SortBy, VisibleTree};
use crate::features::scan::{self as scanner, NodeId, NodeKind, ScanOutcome, ScanProgress};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const QUERY_DEBOUNCE: Duration = Duration::from_millis(180);
type ProgressSlot = Arc<Mutex<Option<ScanProgress>>>;

/// Public fields are for presentation; use the methods to change tree state.
pub struct Session {
    pub root_path: Option<PathBuf>,
    pub result: Option<ScanOutcome>,
    pub progress: Option<ScanProgress>,
    pub status: String,
    pub expanded: HashSet<NodeId>,
    pub rows: VisibleTree,
    pub query: String,
    pub sort: SortBy,
    pub selected: Option<NodeId>,
    receiver: Option<mpsc::Receiver<Result<ScanOutcome, String>>>,
    cancellation: Option<Arc<AtomicBool>>,
    latest_progress: Option<ProgressSlot>,
    query_changed: Option<Instant>,
    applied_query: String,
    reconciling: bool,
    replacing: bool,
    revision: u64,
    stale: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            root_path: None,
            result: None,
            progress: None,
            status: "选择一个文件夹，了解空间都用在了哪里。".into(),
            expanded: HashSet::new(),
            rows: VisibleTree::new(),
            query: String::new(),
            sort: SortBy::default(),
            selected: None,
            receiver: None,
            cancellation: None,
            latest_progress: None,
            query_changed: None,
            applied_query: String::new(),
            reconciling: false,
            replacing: false,
            revision: 0,
            stale: false,
        }
    }
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }

    /// True while a read-only replacement scan is running over the retained view.
    pub fn reconciling(&self) -> bool {
        self.reconciling
    }

    /// A requested root is being scanned while the previous tree stays visible.
    pub fn replacing(&self) -> bool {
        self.replacing
    }

    /// Changes only when a new scan is installed, not on failed retained scans.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The displayed scan could not be revalidated and may describe deleted entries.
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Resolve a retained exact path after dense scanner IDs have changed.
    pub fn node_for_path(&self, path: &Path) -> Option<NodeId> {
        find_path(self.result.as_ref()?, path)
    }

    pub fn cancelling(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    /// Start a background scan. Returns false if already busy or spawning failed.
    /// Cancellation must finish before another scan can start.
    pub fn start(&mut self, path: PathBuf) -> bool {
        if self.busy() {
            return false;
        }
        self.reconciling = false;
        self.replacing = self.result.is_some();
        self.root_path = Some(path.clone());
        if !self.replacing {
            self.stale = false;
            self.reset_navigation();
            self.rows = VisibleTree::new();
        }
        self.progress = None;
        self.status = "正在扫描…".into();
        self.spawn_scan(path)
    }

    /// Revalidate after a filesystem change without clearing the displayed tree.
    /// A full scan correctly reassigns hard-link accounting and discovers external
    /// changes and entries left behind by partially successful deletion. No paths
    /// are deleted here. Keep only one worker so old completions cannot replace a
    /// newer session; cancellation must finish before another scan can begin.
    pub fn reconcile(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        let Some(scan) = self.result.as_ref() else {
            return false;
        };
        // Use the scanned absolute root, not potentially relative user input.
        let path = scan.root_path.clone();
        self.reconciling = true;
        self.progress = None;
        self.status = "正在更新统计，保留当前视图…".into();
        self.spawn_scan(path)
    }

    fn spawn_scan(&mut self, path: PathBuf) -> bool {
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::sync_channel(1);
        let latest_progress: ProgressSlot = Arc::new(Mutex::new(None));
        let worker_progress = Arc::clone(&latest_progress);
        let spawn = std::thread::Builder::new()
            .name("disk-scanner".into())
            .spawn(move || {
                let result = scanner::scan(&path, &worker_cancel, |progress| {
                    // Overwrite rather than enqueue: a paused UI uses constant space.
                    if let Ok(mut latest) = worker_progress.lock() {
                        *latest = Some(progress);
                    }
                })
                .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        match spawn {
            Ok(_) => {
                self.latest_progress = Some(latest_progress);
                self.cancellation = Some(cancellation);
                self.receiver = Some(receiver);
                true
            }
            Err(error) => {
                if self.reconciling {
                    self.reconciliation_failed(&format!("无法启动扫描：{error}"));
                    self.reconciling = false;
                } else if self.replacing {
                    self.replacement_failed(&format!("无法启动扫描：{error}"));
                    self.replacing = false;
                } else {
                    self.status = format!("无法启动扫描：{error}");
                }
                false
            }
        }
    }

    /// Cooperative, nonblocking cancellation; partial results arrive via `poll`.
    pub fn cancel(&mut self) {
        if let Some(flag) = &self.cancellation {
            flag.store(true, Ordering::Relaxed);
            self.status = "正在取消扫描…".into();
        }
    }

    pub fn refresh(&mut self) -> bool {
        match self.root_path.clone() {
            Some(path) => self.start(path),
            None => false,
        }
    }

    /// Drain the latest progress and a possible result, then apply due search.
    /// Returns whether the displayed state changed. Progress behind a retained
    /// result is consumed but does not invalidate that unchanged view.
    /// Idle polls do not rebuild or sort the tree.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(latest) = &self.latest_progress {
            if let Ok(mut slot) = latest.lock() {
                if let Some(progress) = slot.take() {
                    self.progress = Some(progress);
                    changed = self.result.is_none();
                }
            }
        }
        let event = self.receiver.as_ref().map(|receiver| receiver.try_recv());
        let mut completed = false;
        match event {
            Some(Ok(result)) => {
                match result {
                    Ok(scan) if self.replacing && (scan.cancelled || self.cancelling()) => {
                        self.replacement_failed("扫描已取消");
                    }
                    Ok(scan) if self.reconciling && (scan.cancelled || self.cancelling()) => {
                        self.reconciliation_failed("更新已取消");
                    }
                    Ok(mut scan) => {
                        let root = &scan.nodes[scan.root];
                        self.status = if scan.cancelled {
                            "扫描已取消，下方仅显示已扫描部分，不代表完整大小。".into()
                        } else if root.issue_count > 0 {
                            format!(
                                "扫描结束 · {} 处未能完整读取，统计可能偏小",
                                root.issue_count
                            )
                        } else {
                            format!("扫描完成 · {:.1} 秒", scan.elapsed.as_secs_f64())
                        };
                        if self.sort != SortBy::Allocated {
                            sort_tree(&mut scan, self.sort);
                        }
                        if self.reconciling {
                            self.retain_navigation(&scan);
                        } else {
                            if self.replacing {
                                self.reset_navigation();
                                self.root_path = Some(scan.root_path.clone());
                            }
                            self.expanded.insert(scan.root);
                            self.selected = Some(scan.root);
                        }
                        self.stale = false;
                        self.result = Some(scan);
                        self.revision = self.revision.wrapping_add(1);
                    }
                    Err(error) if self.replacing => {
                        self.replacement_failed(&format!("扫描失败：{error}"));
                    }
                    Err(error) if self.reconciling => {
                        self.reconciliation_failed(&format!("扫描失败：{error}"));
                    }
                    Err(error) => self.status = format!("扫描失败：{error}"),
                }
                completed = true;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                if self.replacing {
                    self.replacement_failed("扫描线程意外结束");
                } else if self.reconciling {
                    self.reconciliation_failed("扫描线程意外结束");
                } else {
                    self.status = "扫描线程意外结束，请重新扫描。".into();
                }
                completed = true;
            }
            _ => {}
        }
        let query_due = self
            .query_changed
            .is_some_and(|time| now.saturating_duration_since(time) >= QUERY_DEBOUNCE);
        if query_due {
            self.applied_query.clone_from(&self.query);
            self.query_changed = None;
        }
        if completed {
            self.replacing = false;
            self.reconciling = false;
            self.receiver = None;
            self.cancellation = None;
            self.latest_progress = None;
        }
        if completed || query_due {
            self.rebuild_rows();
            changed = true;
        }
        changed
    }

    fn reset_navigation(&mut self) {
        self.query_changed = None;
        self.applied_query.clear();
        self.expanded.clear();
        self.selected = None;
        self.query.clear();
    }

    fn replacement_failed(&mut self, reason: &str) {
        self.root_path = self.result.as_ref().map(|scan| scan.root_path.clone());
        self.status = format!("{reason}；保留原目录视图。");
    }

    fn reconciliation_failed(&mut self, reason: &str) {
        self.stale = true;
        self.status = format!("{reason}；保留原视图，统计已过期，请重新扫描。");
    }

    fn retain_navigation(&mut self, replacement: &ScanOutcome) {
        let Some(old) = &self.result else { return };
        // Temporary dense mapping is bounded by the old scan. Match exact names
        // within matched parents, avoiding quadratic sibling searches and stored
        // full paths for every node. No deleted-node tombstones survive the swap.
        let mut remap = vec![None; old.nodes.len()];
        let mut pending = vec![(old.root, replacement.root)];
        while let Some((before, after)) = pending.pop() {
            remap[before] = Some(after);
            let children: HashMap<_, _> = replacement.nodes[after]
                .children
                .iter()
                .map(|id| (replacement.nodes[*id].name.as_ref(), *id))
                .collect();
            for child in &old.nodes[before].children {
                if let Some(new_id) = children.get(old.nodes[*child].name.as_ref()) {
                    pending.push((*child, *new_id));
                }
            }
        }
        self.expanded = self
            .expanded
            .iter()
            .filter_map(|id| remap.get(*id).copied().flatten())
            .filter(|id| replacement.nodes[*id].kind == NodeKind::Directory)
            .collect();
        self.selected = self.selected.and_then(|mut id| loop {
            if let Some(mapped) = remap.get(id).copied().flatten() {
                return Some(mapped);
            }
            id = old.nodes.get(id)?.parent?;
        });
    }

    fn rebuild_rows(&mut self) {
        if let Some(scan) = &self.result {
            self.rows.rebuild(scan, &self.expanded, &self.applied_query);
        } else {
            self.rows = VisibleTree::new();
        }
    }

    pub fn applied_query(&self) -> &str {
        &self.applied_query
    }

    /// Restart the 180ms quiet period only when the text actually changes.
    pub fn set_query(&mut self, query: String) {
        if self.query != query {
            self.query = query;
            self.query_changed = Some(Instant::now());
        }
    }

    pub fn sort_by(&mut self, sort: SortBy) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        if let Some(scan) = &mut self.result {
            sort_tree(scan, sort);
            self.rebuild_rows();
        }
    }

    /// Filtered trees automatically expose matching descendants.
    pub fn toggle(&mut self, id: NodeId) {
        if !self.query.trim().is_empty() || !self.applied_query.trim().is_empty() {
            return;
        }
        let Some(node) = self.result.as_ref().and_then(|scan| scan.nodes.get(id)) else {
            return;
        };
        if node.kind != NodeKind::Directory || node.children.is_empty() {
            return;
        }
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild_rows();
    }

    /// Keep the root open, collapsing all descendants.
    pub fn collapse_all(&mut self) {
        if !self.query.trim().is_empty() || !self.applied_query.trim().is_empty() {
            return;
        }
        self.expanded.clear();
        if let Some(scan) = &self.result {
            self.expanded.insert(scan.root);
        }
        self.rebuild_rows();
    }

    pub fn expand_one_level(&mut self) {
        if !self.query.trim().is_empty() || !self.applied_query.trim().is_empty() {
            return;
        }
        if let Some(scan) = &self.result {
            self.rows.expand_one_level(scan, &mut self.expanded);
            self.rebuild_rows();
        }
    }

    pub fn select(&mut self, id: NodeId) {
        if self
            .result
            .as_ref()
            .is_some_and(|scan| id < scan.nodes.len())
        {
            self.selected = Some(id);
        }
    }

    pub fn partial(&self) -> bool {
        self.busy()
            || self.stale
            || self
                .result
                .as_ref()
                .is_some_and(|scan| scan.cancelled || scan.nodes[scan.root].issue_count > 0)
    }

    /// Allocated bytes, logical bytes, files, and subfolders (excluding root).
    pub fn summary(&self) -> (u64, u64, u64, u64) {
        if let Some(scan) = &self.result {
            let root = &scan.nodes[scan.root];
            (
                root.total_allocated_bytes,
                root.total_logical_bytes,
                root.file_count,
                root.directory_count.saturating_sub(1),
            )
        } else if let Some(progress) = &self.progress {
            (
                progress.allocated_bytes,
                progress.logical_bytes,
                progress.files,
                progress.directories.saturating_sub(1),
            )
        } else {
            (0, 0, 0, 0)
        }
    }
}

/// Resolve exact OS filenames, including non-Unicode names, without retaining a
/// full-path index or tombstones for every entry in the scan.
fn find_path(scan: &ScanOutcome, path: &Path) -> Option<NodeId> {
    let relative = path.strip_prefix(&scan.root_path).ok()?;
    let mut id = scan.root;
    for component in relative.components() {
        id = *scan.nodes[id]
            .children
            .iter()
            .find(|child| scan.nodes[**child].name.as_ref() == component.as_os_str())?;
    }
    Some(id)
}

impl Drop for Session {
    fn drop(&mut self) {
        // Never join on the UI thread: filesystem I/O may take an arbitrary time.
        if let Some(flag) = &self.cancellation {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

pub use crate::platform::paths::expand_home;
#[cfg(test)]
use crate::platform::paths::expand_home_with;

#[cfg(test)]
mod tests;
