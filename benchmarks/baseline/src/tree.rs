//! Pure view-model logic, kept separate from rendering for headless tests.
use crate::scanner::{Node, NodeId, ScanOutcome};
use std::cmp::Ordering;
use std::collections::HashSet;

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
            Self::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            Self::Files => b.file_count.cmp(&a.file_count),
        };
        primary
            .then_with(|| a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    }
}

/// Only creates rows currently visible. Search keeps ancestors and reveals matches.
pub fn visible_rows(
    scan: &ScanOutcome,
    expanded: &HashSet<NodeId>,
    query: &str,
    sort: SortBy,
) -> Vec<(NodeId, usize)> {
    let query = query.trim().to_lowercase();
    let filtering = !query.is_empty();
    let mut matched = vec![!filtering; scan.nodes.len()];
    if filtering {
        for node in &scan.nodes {
            matched[node.id] = node.name.to_lowercase().contains(&query);
        }
        // The scanner allocates every parent before its children.
        for node in scan.nodes.iter().rev() {
            if matched[node.id] {
                if let Some(parent) = node.parent {
                    matched[parent] = true;
                }
            }
        }
    }
    let mut rows = Vec::new();
    let mut pending = vec![(scan.root, 0)];
    while let Some((id, depth)) = pending.pop() {
        if !matched[id] {
            continue;
        }
        rows.push((id, depth));
        if filtering || expanded.contains(&id) {
            let mut children = scan.nodes[id].children.clone();
            children.sort_by(|a, b| sort.compare(&scan.nodes[*a], &scan.nodes[*b]));
            pending.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::scan;
    use std::fs;
    use std::sync::atomic::AtomicBool;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn filtering_reveals_ancestors_and_sort_is_descending() {
        let path = std::env::temp_dir().join(format!(
            "tree-lens-view-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(path.join("inside")).unwrap();
        fs::write(path.join("inside/needle.txt"), [0; 20]).unwrap();
        fs::write(path.join("big"), [0; 200]).unwrap();
        let scan = scan(&path, &AtomicBool::new(false), |_| {}).unwrap();
        let expanded = HashSet::new();
        assert_eq!(visible_rows(&scan, &expanded, "", SortBy::Name).len(), 1);
        let filtered = visible_rows(&scan, &expanded, "NEEDLE", SortBy::Name);
        assert_eq!(filtered.len(), 3);
        assert_eq!(scan.nodes[filtered[2].0].name, "needle.txt");
        assert_eq!(filtered[2].1, 2);
        assert!(visible_rows(&scan, &expanded, "missing", SortBy::Name).is_empty());
        let rows = visible_rows(&scan, &HashSet::from([scan.root]), "", SortBy::Logical);
        assert_eq!(scan.nodes[rows[1].0].name, "big");
        fs::remove_dir_all(path).unwrap();
    }
}
