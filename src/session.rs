//! GUI-independent scan lifecycle and virtual-tree state.
//!
//! Call `poll` periodically on the UI thread (including while search is pending),
//! and repaint when it returns true. No GUI objects cross into the scan thread.
use crate::scanner::{self, NodeId, NodeKind, ScanOutcome, ScanProgress};
use crate::tree::{sort_tree, SortBy, VisibleTree};
use std::collections::HashSet;
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
        self.root_path = Some(path.clone());
        self.result = None;
        self.rows = VisibleTree::new();
        self.query_changed = None;
        self.applied_query.clear();
        self.expanded.clear();
        self.selected = None;
        self.query.clear();
        self.progress = None;
        self.status = "正在扫描…".into();
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
                self.status = format!("无法启动扫描：{error}");
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
                    changed = true;
                }
            }
        }
        let event = self.receiver.as_ref().map(|receiver| receiver.try_recv());
        let mut completed = false;
        match event {
            Some(Ok(result)) => {
                match result {
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
                        self.expanded.insert(scan.root);
                        self.selected = Some(scan.root);
                        self.result = Some(scan);
                    }
                    Err(error) => self.status = format!("扫描失败：{error}"),
                }
                completed = true;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.status = "扫描线程意外结束，请重新扫描。".into();
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

impl Drop for Session {
    fn drop(&mut self) {
        // Never join on the UI thread: filesystem I/O may take an arbitrary time.
        if let Some(flag) = &self.cancellation {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Expand only `~`, `~/…` and (on Windows) `~\\…`, never another user's home or embedded tildes.
pub fn expand_home(input: &str) -> PathBuf {
    let home = crate::platform::home_dir();
    expand_home_with(input, home.as_deref())
}

fn expand_home_with(input: &str, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    if let Some(home) = home {
        if input == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = input.strip_prefix("~/") {
            #[cfg(windows)]
            return home.join(rest.trim_start_matches(['/', '\\']));
            #[cfg(not(windows))]
            return home.join(rest.trim_start_matches('/'));
        }
        #[cfg(windows)]
        if let Some(rest) = input.strip_prefix("~\\") {
            return home.join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_expansion_is_exact_and_preserves_missing_home() {
        let home = Some(Path::new("/Users/test"));
        assert_eq!(expand_home_with(" ~ ", home), PathBuf::from("/Users/test"));
        assert_eq!(
            expand_home_with("~/Downloads", home),
            PathBuf::from("/Users/test/Downloads")
        );
        assert_eq!(
            expand_home_with("~other/file", home),
            PathBuf::from("~other/file")
        );
        assert_eq!(expand_home_with("~/file", None), PathBuf::from("~/file"));
        assert_eq!(
            expand_home_with(" /tmp/~file ", home),
            PathBuf::from("/tmp/~file")
        );
    }

    #[cfg(windows)]
    #[test]
    fn expands_windows_home_without_mistaking_drive_or_unc_paths() {
        let home = Some(Path::new(r"C:\Users\测试"));
        assert_eq!(
            expand_home_with(r"~\Downloads", home),
            PathBuf::from(r"C:\Users\测试\Downloads")
        );
        assert_eq!(
            expand_home_with(r"~\\Downloads", home),
            PathBuf::from(r"C:\Users\测试\Downloads")
        );
        for path in [
            r"D:\data",
            r"\\server\share",
            r"\\?\C:\data",
            r"~other\file",
        ] {
            assert_eq!(expand_home_with(path, home), PathBuf::from(path));
        }
    }

    #[test]
    fn query_debounce_is_applied_once_without_sleeping() {
        let mut session = Session::new();
        session.set_query("logs".into());
        let first = session.query_changed.unwrap();
        assert!(!session.poll_at(first + Duration::from_millis(179)));
        session.set_query("logs".into());
        assert_eq!(session.query_changed, Some(first));
        assert!(session.poll_at(first + QUERY_DEBOUNCE));
        assert_eq!(session.applied_query(), "logs");
        assert!(!session.poll_at(first + Duration::from_secs(1)));
        session.set_query(String::new());
        let cleared = session.query_changed.unwrap();
        assert!(session.poll_at(cleared + QUERY_DEBOUNCE));
        assert_eq!(session.applied_query(), "");
    }

    #[test]
    fn start_stays_guarded_during_cancellation_and_drop_signals_worker() {
        let mut session = Session::new();
        assert!(!session.refresh());
        session.cancel();
        let (_sender, receiver) = mpsc::sync_channel(1);
        let flag = Arc::new(AtomicBool::new(false));
        session.receiver = Some(receiver);
        session.cancellation = Some(Arc::clone(&flag));
        assert!(!session.start(PathBuf::from("ignored")));
        assert!(session.root_path.is_none());
        session.cancel();
        assert!(session.busy());
        assert!(session.cancelling());
        assert!(!session.start(PathBuf::from("still-ignored")));
        flag.store(false, Ordering::Relaxed);
        drop(session);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn progress_slot_keeps_only_latest_update() {
        let slot: ProgressSlot = Arc::new(Mutex::new(None));
        let mut session = Session::new();
        session.latest_progress = Some(Arc::clone(&slot));
        for entries in 0..1000 {
            *slot.lock().unwrap() = Some(ScanProgress {
                entries,
                ..ScanProgress::default()
            });
        }
        assert!(session.poll());
        assert_eq!(session.progress.as_ref().unwrap().entries, 999);
        assert!(slot.lock().unwrap().is_none());
        assert!(!session.poll());
    }

    #[test]
    fn completion_indexes_result_and_search_updates_visible_rows() {
        let mut session = Session::new();
        let root = crate::scanner::Node {
            id: 0,
            parent: None,
            children: Vec::new(),
            name: std::ffi::OsStr::new("root").into(),
            kind: NodeKind::Directory,
            own_logical_bytes: 0,
            own_allocated_bytes: 0,
            total_logical_bytes: 42,
            total_allocated_bytes: 4096,
            file_count: 1,
            directory_count: 1,
            issue_count: 0,
            error: None,
            hard_link_duplicate: false,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        session.receiver = Some(receiver);
        sender
            .send(Ok(ScanOutcome {
                nodes: vec![root],
                root: 0,
                root_path: PathBuf::from("/example"),
                elapsed: Duration::from_secs(1),
                cancelled: false,
                issues: Vec::new(),
            }))
            .unwrap();
        assert!(session.poll());
        assert!(!session.busy());
        assert!(!session.partial());
        assert_eq!(session.summary(), (4096, 42, 1, 0));
        assert_eq!(session.selected, Some(0));
        assert_eq!(session.rows.len(), 1);
        session.set_query("missing".into());
        let changed = session.query_changed.unwrap();
        assert!(!session.poll_at(changed + Duration::from_millis(179)));
        assert_eq!(session.rows.len(), 1);
        assert!(session.poll_at(changed + QUERY_DEBOUNCE));
        assert_eq!(session.rows.len(), 0);
        assert!(!session.poll_at(changed + Duration::from_secs(1)));
        session.select(999);
        assert_eq!(session.selected, Some(0));
    }

    #[test]
    fn disconnected_worker_becomes_retryable() {
        let mut session = Session::new();
        let (sender, receiver) = mpsc::sync_channel(1);
        session.receiver = Some(receiver);
        drop(sender);
        assert!(session.poll());
        assert!(!session.busy());
        assert!(session.status.contains("意外结束"));
        assert!(!session.poll());
    }
}
