use super::*;
use crate::features::scan::scan;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture() -> (std::path::PathBuf, ScanOutcome) {
    let path = std::env::temp_dir().join(format!(
        "tree-lens-view-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(path.join("inside/deep")).unwrap();
    fs::create_dir(path.join("empty")).unwrap();
    fs::write(path.join("inside/needle.txt"), [0; 20]).unwrap();
    fs::write(path.join("inside/deep/another"), [0; 10]).unwrap();
    fs::write(path.join("big"), [0; 200]).unwrap();
    let scan = scan(&path, &AtomicBool::new(false), |_| {}).unwrap();
    (path, scan)
}

#[test]
fn filtering_reveals_ancestors_and_sort_is_descending() {
    let (path, mut scan) = fixture();
    let expanded = HashSet::new();
    let mut tree = VisibleTree::new();
    sort_tree(&mut scan, SortBy::Name);
    tree.rebuild(&scan, &expanded, "");
    assert_eq!(tree.len(), 1);
    tree.rebuild(&scan, &expanded, "NEEDLE");
    let filtered = tree.rows(&scan, 0..tree.len());
    assert_eq!(filtered.len(), 3);
    assert_eq!(scan.nodes[filtered[2].0].display_name(), "needle.txt");
    assert_eq!(filtered[2].1, 2);
    tree.rebuild(&scan, &expanded, "missing");
    assert!(tree.is_empty());
    sort_tree(&mut scan, SortBy::Logical);
    tree.rebuild(&scan, &HashSet::from([scan.root]), "");
    assert_eq!(
        scan.nodes[tree.row_at(&scan, 1).unwrap().0].display_name(),
        "big"
    );
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn viewport_matches_reference_at_every_position_and_sort() {
    let (path, mut scan) = fixture();
    let expanded: HashSet<_> = scan
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Directory)
        .map(|n| n.id)
        .collect();
    let mut tree = VisibleTree::new();
    for sort in [
        SortBy::Allocated,
        SortBy::Name,
        SortBy::Logical,
        SortBy::Files,
    ] {
        sort_tree(&mut scan, sort);
        tree.rebuild(&scan, &expanded, "");
        let mut expected = Vec::new();
        let mut stack = vec![(scan.root, 0)];
        while let Some((id, depth)) = stack.pop() {
            expected.push((id, depth));
            stack.extend(
                scan.nodes[id]
                    .children
                    .iter()
                    .rev()
                    .map(|id| (*id, depth + 1)),
            );
        }
        assert_eq!(tree.len(), expected.len());
        for start in 0..=expected.len() {
            assert_eq!(
                tree.rows(&scan, start..start + 3),
                expected[start..(start + 3).min(expected.len())]
            );
        }
        assert_eq!(tree.row_at(&scan, tree.len()), None);
    }
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn row_index_is_inverse_for_sort_filter_and_collapsed_branches() {
    let (path, mut scan) = fixture();
    let expanded = scan.nodes.iter().map(|node| node.id).collect();
    let mut tree = VisibleTree::new();
    for sort in [
        SortBy::Name,
        SortBy::Logical,
        SortBy::Allocated,
        SortBy::Files,
    ] {
        sort_tree(&mut scan, sort);
        for query in ["", "needle", "deep", "missing"] {
            for open in [&expanded, &HashSet::from([scan.root])] {
                tree.rebuild(&scan, open, query);
                for node in &scan.nodes {
                    let expected = (0..tree.len())
                        .find(|&index| tree.row_at(&scan, index).map(|row| row.0) == Some(node.id));
                    assert_eq!(tree.row_index(&scan, node.id), expected);
                }
                assert_eq!(tree.row_index(&scan, usize::MAX), None);
            }
        }
    }
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn expansion_state_only_contains_directories_and_collapse_releases_cache() {
    let (path, scan) = fixture();
    let mut expanded = HashSet::from([scan.root]);
    let mut tree = VisibleTree::new();
    tree.rebuild(&scan, &expanded, "");
    tree.expand_one_level(&scan, &mut expanded);
    assert!(expanded
        .iter()
        .all(|id| scan.nodes[*id].kind == NodeKind::Directory));
    tree.rebuild(&scan, &expanded, "");
    assert!(tree.len() > scan.nodes[scan.root].children.len() + 1);
    tree.rebuild(&scan, &HashSet::new(), "");
    assert_eq!(tree.len(), 1);
    assert_eq!(tree.estimated_heap_bytes(), 0);
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn hundred_thousand_flat_files_do_not_create_a_flat_row_cache() {
    use std::path::PathBuf;
    use std::time::Duration;
    const FILES: usize = 100_000;
    let file = Node {
        id: 0,
        parent: Some(0),
        children: Vec::new(),
        name: std::ffi::OsStr::new("entry").into(),
        kind: NodeKind::File,
        own_logical_bytes: 0,
        own_allocated_bytes: 0,
        total_logical_bytes: 0,
        total_allocated_bytes: 0,
        file_count: 1,
        directory_count: 0,
        issue_count: 0,
        error: None,
        hard_link_duplicate: false,
    };
    let mut root = file.clone();
    root.kind = NodeKind::Directory;
    root.parent = None;
    root.children = (1..=FILES).collect();
    root.file_count = FILES as u64;
    root.directory_count = 1;
    let mut nodes = Vec::with_capacity(FILES + 1);
    nodes.push(root);
    for id in 1..=FILES {
        let mut node = file.clone();
        node.id = id;
        nodes.push(node);
    }
    let scan = ScanOutcome {
        nodes,
        root: 0,
        root_path: PathBuf::from("/synthetic"),
        elapsed: Duration::ZERO,
        cancelled: false,
        issues: Vec::new(),
    };
    let mut expanded = HashSet::from([0]);
    let mut tree = VisibleTree::new();
    tree.rebuild(&scan, &expanded, "");
    assert_eq!(tree.len(), FILES + 1);
    assert!(tree.estimated_heap_bytes() < 1024);
    assert_eq!(
        tree.rows(&scan, FILES - 49..FILES + 1),
        (FILES - 49..=FILES).map(|id| (id, 1)).collect::<Vec<_>>()
    );
    tree.expand_one_level(&scan, &mut expanded);
    assert_eq!(expanded.len(), 1);
    tree.rebuild(&scan, &expanded, "entry");
    assert_eq!(tree.len(), FILES + 1);
    assert!(
        tree.estimated_heap_bytes() < 20_000,
        "search must use packed bits, not one bool/row record per file"
    );
}

#[test]
fn filtered_and_partially_open_views_match_reference() {
    let (path, mut scan) = fixture();
    let directories: Vec<_> = scan
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Directory)
        .map(|n| n.id)
        .collect();
    let mut tree = VisibleTree::new();
    for mask in 0..(1 << directories.len()) {
        let expanded: HashSet<_> = directories
            .iter()
            .enumerate()
            .filter_map(|(bit, id)| ((mask & (1 << bit)) != 0).then_some(*id))
            .collect();
        for query in ["", "i", "another", "empty", "nothing"] {
            sort_tree(&mut scan, SortBy::Name);
            tree.rebuild(&scan, &expanded, query);
            let mut matched: Vec<_> = scan
                .nodes
                .iter()
                .map(|n| query.is_empty() || n.display_name().to_lowercase().contains(query))
                .collect();
            for node in scan.nodes.iter().rev() {
                if matched[node.id] {
                    if let Some(parent) = node.parent {
                        matched[parent] = true;
                    }
                }
            }
            let mut expected = Vec::new();
            let mut stack = vec![(scan.root, 0)];
            while let Some((id, depth)) = stack.pop() {
                if !matched[id] {
                    continue;
                }
                expected.push((id, depth));
                if !query.is_empty() || expanded.contains(&id) {
                    stack.extend(
                        scan.nodes[id]
                            .children
                            .iter()
                            .rev()
                            .map(|id| (*id, depth + 1)),
                    );
                }
            }
            assert_eq!(
                tree.rows(&scan, 0..usize::MAX),
                expected,
                "mask={mask} query={query}"
            );
        }
    }
    fs::remove_dir_all(path).unwrap();
}
