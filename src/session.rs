//! GUI-independent scan lifecycle and virtual-tree state.
//!
//! Call `poll` periodically on the UI thread (including while search is pending),
//! and repaint when it returns true. No GUI objects cross into the scan thread.
use crate::scanner::{self, NodeId, NodeKind, ScanOutcome, ScanProgress};
use crate::tree::{sort_tree, SortBy, VisibleTree};
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
    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "tree-lens-session-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn scan(&self) -> ScanOutcome {
            scanner::scan(&self.0, &AtomicBool::new(false), |_| {}).unwrap()
        }
        fn session(&self) -> Session {
            let mut session = Session::new();
            let scan = self.scan();
            session.root_path = Some(self.0.clone());
            session.expanded.insert(scan.root);
            session.selected = Some(scan.root);
            session.result = Some(scan);
            session.rebuild_rows();
            session
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn finish(session: &mut Session) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.busy() {
            session.poll();
            assert!(Instant::now() < deadline, "scan worker did not finish");
            std::thread::yield_now();
        }
    }

    #[test]
    fn reconciliation_retains_view_then_maps_navigation_and_external_changes() {
        let fixture = Fixture::new();
        let dir = fixture.0.join("folder");
        std::fs::create_dir(&dir).unwrap();
        let deleted = dir.join("deleted");
        let survivor = dir.join("survivor");
        std::fs::write(&deleted, b"removed").unwrap();
        std::fs::write(&survivor, b"left behind after partial delete").unwrap();
        let mut session = fixture.session();
        let folder_id = session.node_for_path(&dir).unwrap();
        session.toggle(folder_id);
        session.select(session.node_for_path(&deleted).unwrap());
        session.sort_by(SortBy::Name);
        let old_summary = session.summary();
        let old_rows = session.rows.len();
        let old_selected = session.selected;
        std::fs::remove_file(&deleted).unwrap();
        let added = fixture.0.join("external");
        std::fs::write(&added, b"new outside change").unwrap();
        assert!(session.reconcile());
        assert!(session.busy() && session.reconciling());
        assert_eq!(session.summary(), old_summary);
        assert_eq!(session.rows.len(), old_rows);
        assert_eq!(session.selected, old_selected);
        assert!(!session.start(fixture.0.join("other")));
        assert!(!session.refresh());
        assert!(!session.reconcile());
        finish(&mut session);
        assert!(!session.reconciling() && !session.stale());
        assert_eq!(session.selected, session.node_for_path(&dir));
        assert!(session
            .expanded
            .contains(&session.node_for_path(&dir).unwrap()));
        assert!(session.node_for_path(&deleted).is_none());
        assert!(session.node_for_path(&survivor).is_some());
        assert!(session.node_for_path(&added).is_some());
        assert_eq!(session.sort, SortBy::Name);
        assert_eq!(session.summary().2, 2);
        let scan = session.result.as_ref().unwrap();
        let names: Vec<_> = scan.nodes[scan.root]
            .children
            .iter()
            .map(|id| scan.nodes[*id].display_name().into_owned())
            .collect();
        assert_eq!(names, ["external", "folder"]);
    }

    #[test]
    fn reconciliation_preserves_pending_debounce_and_latest_navigation() {
        let fixture = Fixture::new();
        let dir = fixture.0.join("folder");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("match"), b"data").unwrap();
        let mut session = fixture.session();
        let (sender, receiver) = mpsc::sync_channel(1);
        session.receiver = Some(receiver);
        session.reconciling = true;
        session.select(session.node_for_path(&dir).unwrap());
        session.toggle(session.node_for_path(&dir).unwrap());
        session.sort_by(SortBy::Logical);
        session.set_query("match".into());
        let changed = session.query_changed.unwrap();
        sender.send(Ok(fixture.scan())).unwrap();
        session.poll_at(changed + Duration::from_millis(179));
        assert_eq!(session.query, "match");
        assert_eq!(session.applied_query(), "");
        assert_eq!(session.query_changed, Some(changed));
        assert_eq!(session.selected, session.node_for_path(&dir));
        assert!(session
            .expanded
            .contains(&session.node_for_path(&dir).unwrap()));
        assert_eq!(session.sort, SortBy::Logical);
        session.poll_at(changed + QUERY_DEBOUNCE);
        assert_eq!(session.applied_query(), "match");
        // A second replacement preserves the already applied query as well.
        assert!(session.reconcile());
        finish(&mut session);
        assert_eq!(session.applied_query(), "match");
        assert_eq!(session.rows.len(), 3);
    }

    #[test]
    fn reconciliation_failure_cancel_and_disconnect_keep_old_snapshot_stale() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("file"), b"data").unwrap();
        for mode in 0..3 {
            let mut session = fixture.session();
            let previous = session.summary();
            let rows = session.rows.len();
            let (sender, receiver) = mpsc::sync_channel(1);
            session.receiver = Some(receiver);
            session.reconciling = true;
            match mode {
                0 => sender.send(Err("unavailable".into())).unwrap(),
                1 => {
                    let mut scan = fixture.scan();
                    scan.cancelled = true;
                    sender.send(Ok(scan)).unwrap();
                }
                _ => {}
            }
            drop(sender);
            assert!(session.poll());
            assert!(!session.busy() && !session.reconciling());
            assert!(session.stale() && session.partial());
            assert!(session.status.contains("统计已过期"));
            assert_eq!(session.summary(), previous);
            assert_eq!(session.rows.len(), rows);
            assert!(session.reconcile());
            finish(&mut session);
            assert!(!session.stale());
        }
    }

    #[test]
    fn deleting_selected_ancestors_falls_back_to_root() {
        let fixture = Fixture::new();
        let dir = fixture.0.join("parent/child");
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = fixture.session();
        session.select(session.node_for_path(&dir).unwrap());
        std::fs::remove_dir_all(fixture.0.join("parent")).unwrap();
        assert!(session.reconcile());
        finish(&mut session);
        assert_eq!(
            session.selected,
            Some(session.result.as_ref().unwrap().root)
        );
    }

    #[cfg(unix)]
    #[test]
    fn reconciliation_reassigns_removed_hard_link_owner_without_double_counting() {
        let fixture = Fixture::new();
        let first = fixture.0.join("first");
        let second = fixture.0.join("second");
        std::fs::write(&first, vec![42; 16384]).unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        let mut session = fixture.session();
        let scan = session.result.as_ref().unwrap();
        let owner = scan
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File && !n.hard_link_duplicate)
            .unwrap();
        let allocation = owner.own_allocated_bytes;
        let owner_path = scan.path_for(owner.id);
        std::fs::remove_file(owner_path).unwrap();
        assert!(session.reconcile());
        finish(&mut session);
        let scan = session.result.as_ref().unwrap();
        let survivor = scan
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .unwrap();
        assert!(!survivor.hard_link_duplicate);
        assert_eq!(survivor.own_allocated_bytes, allocation);
        assert_eq!(session.summary().2, 1);
    }
    #[test]
    fn hundred_thousand_node_reconciliation_remaps_dense_ids_and_search() {
        const FILES: usize = 99_998;
        fn synthetic(replacement: bool) -> ScanOutcome {
            let template = crate::scanner::Node {
                id: 0,
                parent: None,
                children: Vec::new(),
                name: std::ffi::OsStr::new("root").into(),
                kind: NodeKind::Directory,
                own_logical_bytes: 0,
                own_allocated_bytes: 0,
                total_logical_bytes: FILES as u64,
                total_allocated_bytes: FILES as u64 * 4096,
                file_count: FILES as u64,
                directory_count: 2,
                issue_count: 0,
                error: None,
                hard_link_duplicate: false,
            };
            let mut nodes = Vec::with_capacity(100_000);
            nodes.push(template.clone());
            if replacement {
                let mut added = template.clone();
                added.id = 1;
                added.parent = Some(0);
                added.name = std::ffi::OsStr::new("new-file").into();
                added.kind = NodeKind::File;
                added.own_logical_bytes = 1;
                added.own_allocated_bytes = 4096;
                added.total_logical_bytes = 1;
                added.total_allocated_bytes = 4096;
                added.file_count = 1;
                added.directory_count = 0;
                nodes.push(added);
            }
            let folder_id = nodes.len();
            let mut folder = template.clone();
            folder.id = folder_id;
            folder.parent = Some(0);
            folder.name = std::ffi::OsStr::new("folder").into();
            folder.file_count = (FILES - usize::from(replacement)) as u64;
            folder.total_logical_bytes = folder.file_count;
            folder.total_allocated_bytes = folder.file_count * 4096;
            folder.directory_count = 1;
            nodes.push(folder);
            nodes[0].children = if replacement {
                vec![1, folder_id]
            } else {
                vec![folder_id]
            };
            for position in 0..FILES - usize::from(replacement) {
                // New scan deletes file-000000, adds new-file, and reverses all
                // surviving file IDs while keeping parents before descendants.
                let number = if replacement {
                    FILES - 1 - position
                } else {
                    position
                };
                let id = nodes.len();
                let mut file = template.clone();
                file.id = id;
                file.parent = Some(folder_id);
                file.name =
                    std::ffi::OsString::from(format!("file-{number:06}")).into_boxed_os_str();
                file.kind = NodeKind::File;
                file.own_logical_bytes = 1;
                file.own_allocated_bytes = 4096;
                file.total_logical_bytes = 1;
                file.total_allocated_bytes = 4096;
                file.file_count = 1;
                file.directory_count = 0;
                nodes.push(file);
                nodes[folder_id].children.push(id);
            }
            assert_eq!(nodes.len(), 100_000);
            ScanOutcome {
                nodes,
                root: 0,
                root_path: std::env::temp_dir().join("synthetic-tree"),
                elapsed: Duration::ZERO,
                cancelled: false,
                issues: Vec::new(),
            }
        }
        let old = synthetic(false);
        let folder = old.root_path.join("folder");
        let selected_path = folder.join("file-050000");
        let mut session = Session::new();
        session.root_path = Some(old.root_path.clone());
        session.result = Some(old);
        session.expanded = HashSet::from([0, 1]);
        session.selected = session.node_for_path(&selected_path);
        let old_selected = session.selected;
        session.query = "file-050000".into();
        session.applied_query = session.query.clone();
        session.rebuild_rows();
        assert_eq!(session.rows.len(), 3);
        let (sender, receiver) = mpsc::sync_channel(1);
        session.receiver = Some(receiver);
        session.reconciling = true;
        sender.send(Ok(synthetic(true))).unwrap();
        let started = Instant::now();
        assert!(session.poll());
        let elapsed = started.elapsed();
        eprintln!("100,000-node reconciliation poll (remap + filtered row rebuild + old scan drop): {elapsed:?}");
        assert!(!session.busy());
        assert_ne!(session.selected, old_selected);
        assert_eq!(session.selected, session.node_for_path(&selected_path));
        assert_eq!(session.node_for_path(&folder), Some(2));
        assert_eq!(session.expanded, HashSet::from([0, 2]));
        assert_eq!(
            session.summary(),
            (FILES as u64 * 4096, FILES as u64, FILES as u64, 1)
        );
        assert_eq!(session.applied_query(), "file-050000");
        assert_eq!(session.rows.len(), 3);
        assert!(session.node_for_path(&folder.join("file-000000")).is_none());
        assert!(session
            .node_for_path(&session.root_path.as_ref().unwrap().join("new-file"))
            .is_some());
    }
    #[test]
    fn replacing_root_retains_view_until_atomic_success() {
        let original = Fixture::new();
        let replacement = Fixture::new();
        std::fs::write(original.0.join("old"), b"previous").unwrap();
        std::fs::write(replacement.0.join("new"), b"replacement").unwrap();
        let mut session = original.session();
        session.select(session.node_for_path(&original.0.join("old")).unwrap());
        session.sort_by(SortBy::Name);
        session.set_query("old".into());
        session.poll_at(session.query_changed.unwrap() + QUERY_DEBOUNCE);
        session.set_query("pending".into());
        let debounce = session.query_changed;
        let old_selected = session.selected;
        let old_expanded = session.expanded.clone();
        let old_summary = session.summary();
        let old_rows = session.rows.len();
        let revision = session.revision();
        assert!(session.start(replacement.0.clone()));
        assert!(session.replacing() && session.busy());
        assert!(!session.reconciling());
        assert_eq!(session.result.as_ref().unwrap().root_path, original.0);
        assert_eq!(session.root_path.as_ref(), Some(&replacement.0));
        assert_eq!(session.selected, old_selected);
        assert_eq!(session.expanded, old_expanded);
        assert_eq!(session.summary(), old_summary);
        assert_eq!(session.rows.len(), old_rows);
        assert_eq!(session.query, "pending");
        assert_eq!(session.applied_query(), "old");
        assert_eq!(session.query_changed, debounce);
        assert_eq!(session.revision(), revision);
        assert!(!session.start(original.0.clone()));
        assert!(!session.reconcile());
        assert_eq!(session.root_path.as_ref(), Some(&replacement.0));
        finish(&mut session);
        assert!(!session.replacing());
        assert_eq!(session.revision(), revision + 1);
        assert_eq!(session.result.as_ref().unwrap().root_path, replacement.0);
        assert_eq!(session.expanded, HashSet::from([0]));
        assert_eq!(session.selected, Some(0));
        assert_eq!(session.query, "");
        assert_eq!(session.applied_query(), "");
        assert_eq!(session.query_changed, None);
        assert_eq!(session.sort, SortBy::Name);
        assert!(session.node_for_path(&replacement.0.join("new")).is_some());
    }

    #[test]
    fn failed_root_replacement_preserves_old_view_and_revision() {
        let original = Fixture::new();
        let replacement = Fixture::new();
        std::fs::write(original.0.join("old"), b"previous").unwrap();
        for mode in 0..4 {
            let mut session = original.session();
            session.stale = true; // Failed replacement must not clear an old warning.
            session.select(session.node_for_path(&original.0.join("old")).unwrap());
            session.set_query("old".into());
            let changed = session.query_changed.unwrap();
            session.poll_at(changed + QUERY_DEBOUNCE);
            let selected = session.selected;
            let summary = session.summary();
            let rows = session.rows.len();
            let revision = session.revision();
            session.replacing = true;
            session.root_path = Some(replacement.0.clone());
            let (sender, receiver) = mpsc::sync_channel(1);
            session.receiver = Some(receiver);
            match mode {
                0 => sender
                    .send(Err("cannot scan requested root".into()))
                    .unwrap(),
                1 => {
                    let mut scan = replacement.scan();
                    scan.cancelled = true;
                    sender.send(Ok(scan)).unwrap();
                }
                2 => {} // disconnected worker
                _ => {
                    // Exercise the spawn-error rollback without exhausting OS threads.
                    session.replacement_failed("无法启动扫描");
                    session.replacing = false;
                    session.receiver = None;
                }
            }
            drop(sender);
            session.poll_at(changed + QUERY_DEBOUNCE);
            assert!(!session.busy() && !session.replacing());
            assert_eq!(session.revision(), revision);
            assert_eq!(session.root_path.as_ref(), Some(&original.0));
            assert_eq!(session.result.as_ref().unwrap().root_path, original.0);
            assert_eq!(session.selected, selected);
            assert_eq!(session.summary(), summary);
            assert_eq!(session.rows.len(), rows);
            assert_eq!(session.query, "old");
            assert_eq!(session.applied_query(), "old");
            assert!(session.stale());
            assert!(session.status.contains("保留原目录视图"));
        }
    }

    #[test]
    fn initial_cancelled_scan_still_installs_partial_result() {
        let fixture = Fixture::new();
        let mut session = Session::new();
        let mut scan = fixture.scan();
        scan.cancelled = true;
        let (sender, receiver) = mpsc::sync_channel(1);
        session.receiver = Some(receiver);
        sender.send(Ok(scan)).unwrap();
        session.poll();
        assert!(session.result.as_ref().unwrap().cancelled);
        assert_eq!(session.revision(), 1);
        assert!(session.partial());
        assert!(!session.replacing());
    }
    #[test]
    fn cancellation_after_worker_completion_keeps_retained_view() {
        let original = Fixture::new();
        let replacement = Fixture::new();
        std::fs::write(original.0.join("old"), b"previous").unwrap();
        for replacing in [true, false] {
            let mut session = original.session();
            session.select(session.node_for_path(&original.0.join("old")).unwrap());
            let selected = session.selected;
            let summary = session.summary();
            let rows = session.rows.len();
            let revision = session.revision();
            session.replacing = replacing;
            session.reconciling = !replacing;
            if replacing {
                session.root_path = Some(replacement.0.clone());
            }
            let (sender, receiver) = mpsc::sync_channel(1);
            session.receiver = Some(receiver);
            session.cancellation = Some(Arc::new(AtomicBool::new(false)));
            // The worker finished before cancellation, but its result has not
            // yet been applied. User cancellation still wins at this boundary.
            sender
                .send(Ok(if replacing {
                    replacement.scan()
                } else {
                    original.scan()
                }))
                .unwrap();
            session.cancel();
            assert!(session.cancelling());
            assert!(session.poll());
            assert!(!session.busy());
            assert!(!session.replacing() && !session.reconciling());
            assert_eq!(session.revision(), revision);
            assert_eq!(session.root_path.as_ref(), Some(&original.0));
            assert_eq!(session.result.as_ref().unwrap().root_path, original.0);
            assert_eq!(session.selected, selected);
            assert_eq!(session.summary(), summary);
            assert_eq!(session.rows.len(), rows);
            assert_eq!(session.stale(), !replacing);
        }
    }
}
