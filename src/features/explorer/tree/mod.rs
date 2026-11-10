//! Virtual tree indexing: no flattened allocation proportional to expanded files.
use crate::features::scan::{Node, NodeId, NodeKind, ScanOutcome};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortBy {
    #[default]
    Allocated,
    Logical,
    Name,
    Files,
}

impl SortBy {
    fn compare(self, a: &Node, b: &Node) -> Ordering {
        let primary = match self {
            Self::Allocated => b.total_allocated_bytes.cmp(&a.total_allocated_bytes),
            Self::Logical => b.total_logical_bytes.cmp(&a.total_logical_bytes),
            // Unicode-aware, without allocating lowercase strings for every comparison.
            Self::Name => a
                .display_name()
                .chars()
                .flat_map(char::to_lowercase)
                .cmp(b.display_name().chars().flat_map(char::to_lowercase)),
            Self::Files => b.file_count.cmp(&a.file_count),
        };
        primary
            .then_with(|| a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    }
}

/// Reuse the scanner's child vectors. Sorting is needed only when the sort changes.
pub fn sort_tree(scan: &mut ScanOutcome, sort: SortBy) {
    for id in 0..scan.nodes.len() {
        let mut children = std::mem::take(&mut scan.nodes[id].children);
        children.sort_unstable_by(|a, b| sort.compare(&scan.nodes[*a], &scan.nodes[*b]));
        scan.nodes[id].children = children;
    }
}

#[derive(Default, Debug)]
struct Branch {
    /// Total visible rows including the branch's own row.
    rows: usize,
    /// Present only when filtering removes some direct children.
    filtered_children: Option<Vec<usize>>,
    /// (child position, cumulative extra descendant rows after this child).
    /// Flat files and collapsed folders need no entry here.
    extra_rows: Vec<(usize, usize)>,
}

impl Branch {
    fn children_len(&self, node: &Node) -> usize {
        self.filtered_children
            .as_ref()
            .map_or(node.children.len(), Vec::len)
    }

    fn child_at(&self, node: &Node, position: usize) -> NodeId {
        node.children[self
            .filtered_children
            .as_ref()
            .map_or(position, |ids| ids[position])]
    }

    fn child_start(&self, position: usize) -> usize {
        let preceding = self
            .extra_rows
            .partition_point(|(child, _)| *child < position);
        position
            + if preceding == 0 {
                0
            } else {
                self.extra_rows[preceding - 1].1
            }
    }
}

/// Memory is proportional to expanded directories, not every expanded file.
/// Search adds a packed match bitset and, only where necessary, filtered child positions.
#[derive(Default, Debug)]
pub struct VisibleTree {
    branches: HashMap<NodeId, Branch>,
    matches: Vec<u64>,
    filtering: bool,
    total_rows: usize,
}

impl VisibleTree {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.total_rows
    }
    pub fn is_empty(&self) -> bool {
        self.total_rows == 0
    }

    fn matches(&self, id: NodeId) -> bool {
        !self.filtering || (self.matches[id / 64] & (1 << (id % 64))) != 0
    }

    pub fn rebuild(&mut self, scan: &ScanOutcome, expanded: &HashSet<NodeId>, query: &str) {
        // Drop old caches, rather than retaining the peak capacity after collapsing.
        self.branches = HashMap::new();
        self.matches = Vec::new();
        self.total_rows = 0;
        let query = query.trim().to_lowercase();
        self.filtering = !query.is_empty();
        if self.filtering {
            self.matches = vec![0; scan.nodes.len().div_ceil(64)];
            for node in &scan.nodes {
                if node.display_name().to_lowercase().contains(&query) {
                    self.matches[node.id / 64] |= 1 << (node.id % 64);
                }
            }
            for node in scan.nodes.iter().rev() {
                if self.matches(node.id) {
                    if let Some(parent) = node.parent {
                        self.matches[parent / 64] |= 1 << (parent % 64);
                    }
                }
            }
        }
        if !self.matches(scan.root) {
            return;
        }
        self.total_rows = 1;
        // Only directories that are actually visible and open enter the index.
        let mut open = Vec::new();
        let mut pending = vec![scan.root];
        while let Some(id) = pending.pop() {
            let node = &scan.nodes[id];
            if node.children.is_empty() || !(self.filtering || expanded.contains(&id)) {
                continue;
            }
            open.push(id);
            pending.extend(node.children.iter().copied().filter(|child| {
                !scan.nodes[*child].children.is_empty()
                    && self.matches(*child)
                    && (self.filtering || expanded.contains(child))
            }));
        }
        // Parents precede descendants in open, so compute subtree counts backwards.
        for id in open.into_iter().rev() {
            let node = &scan.nodes[id];
            let filtered_children =
                if self.filtering && node.children.iter().any(|child| !self.matches(*child)) {
                    Some(
                        node.children
                            .iter()
                            .enumerate()
                            .filter_map(|(i, child)| self.matches(*child).then_some(i))
                            .collect(),
                    )
                } else {
                    None
                };
            let mut branch = Branch {
                rows: 1,
                filtered_children,
                extra_rows: Vec::new(),
            };
            let mut extra = 0;
            for position in 0..branch.children_len(node) {
                let child = branch.child_at(node, position);
                let count = self.branches.get(&child).map_or(1, |b| b.rows);
                branch.rows += count;
                if count > 1 {
                    extra += count - 1;
                    branch.extra_rows.push((position, extra));
                }
            }
            self.branches.insert(id, branch);
        }
        self.total_rows = self.branches.get(&scan.root).map_or(1, |b| b.rows);
    }

    /// Seek directly to a row by binary-searching sibling spans, without walking
    /// or allocating all preceding rows. Iterative even for deeply nested trees.
    pub fn row_at(&self, scan: &ScanOutcome, mut row: usize) -> Option<(NodeId, usize)> {
        if row >= self.total_rows {
            return None;
        }
        let mut id = scan.root;
        let mut depth = 0;
        while row != 0 {
            row -= 1;
            let branch = self.branches.get(&id)?;
            let node = &scan.nodes[id];
            let mut left = 0;
            let mut right = branch.children_len(node);
            while left < right {
                let middle = left + (right - left) / 2;
                if branch.child_start(middle) <= row {
                    left = middle + 1;
                } else {
                    right = middle;
                }
            }
            let position = left.checked_sub(1)?;
            row -= branch.child_start(position);
            id = branch.child_at(node, position);
            depth += 1;
        }
        Some((id, depth))
    }

    /// Locate one visible node without allocating a flattened row list. This is
    /// the inverse of `row_at`, used to keep the viewport anchored after updates.
    pub fn row_index(&self, scan: &ScanOutcome, mut id: NodeId) -> Option<usize> {
        if self.is_empty() || id >= scan.nodes.len() || !self.matches(id) {
            return None;
        }
        let mut row = 0;
        while id != scan.root {
            let parent = scan.nodes[id].parent?;
            let branch = self.branches.get(&parent)?;
            let node = &scan.nodes[parent];
            let position = (0..branch.children_len(node))
                .find(|&position| branch.child_at(node, position) == id)?;
            row += 1 + branch.child_start(position);
            id = parent;
        }
        Some(row)
    }

    /// Allocates only the requested viewport, typically a few dozen rows.
    pub fn rows(&self, scan: &ScanOutcome, range: Range<usize>) -> Vec<(NodeId, usize)> {
        (range.start.min(self.total_rows)..range.end.min(self.total_rows))
            .filter_map(|row| self.row_at(scan, row))
            .collect()
    }

    /// Expand currently visible directories only. Never put every file in a HashSet.
    pub fn expand_one_level(&self, scan: &ScanOutcome, expanded: &mut HashSet<NodeId>) {
        if self.is_empty() {
            return;
        }
        for node in &scan.nodes {
            if node.kind == NodeKind::Directory
                && self.matches(node.id)
                && (node.id == scan.root
                    || node.parent.is_some_and(|p| self.branches.contains_key(&p)))
            {
                expanded.insert(node.id);
            }
        }
    }

    /// Heap payload estimate (excluding allocator/hash-table metadata).
    pub fn estimated_heap_bytes(&self) -> usize {
        self.matches.capacity() * std::mem::size_of::<u64>()
            + self.branches.capacity() * std::mem::size_of::<(NodeId, Branch)>()
            + self
                .branches
                .values()
                .map(|b| {
                    b.extra_rows.capacity() * std::mem::size_of::<(usize, usize)>()
                        + b.filtered_children
                            .as_ref()
                            .map_or(0, |v| v.capacity() * std::mem::size_of::<usize>())
                })
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests;
