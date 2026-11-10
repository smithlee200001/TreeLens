use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type NodeId = usize;
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
    /// Exact filename only. Full paths are reconstructed from parent links.
    pub name: Box<OsStr>,
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
    pub error: Option<Box<str>>,
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
    pub root_path: PathBuf,
    pub elapsed: Duration,
    pub cancelled: bool,
    /// At most 500 details. The root issue_count retains the full count.
    pub issues: Vec<ScanIssue>,
}

impl Node {
    /// Lossy conversion is for display only; filesystem operations retain exact bytes.
    pub fn display_name(&self) -> Cow<'_, str> {
        self.name.to_string_lossy()
    }
}

impl ScanOutcome {
    /// Reconstruct an exact path without storing a full path for every entry.
    /// Panics if the ID does not belong to this outcome.
    pub fn path_for(&self, id: NodeId) -> PathBuf {
        reconstruct_path(&self.nodes, &self.root_path, id)
    }
}

pub(super) fn reconstruct_path(nodes: &[Node], root_path: &Path, mut id: NodeId) -> PathBuf {
    let mut components = Vec::new();
    while let Some(parent) = nodes[id].parent {
        components.push(nodes[id].name.as_ref());
        id = parent;
    }
    let mut path = root_path.to_path_buf();
    for component in components.into_iter().rev() {
        path.push(component);
    }
    path
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
