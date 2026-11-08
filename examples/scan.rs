//! Headless smoke test: cargo run --no-default-features --example scan -- /path
use std::{path::PathBuf, sync::atomic::AtomicBool};
use tree_lens::{format_bytes, scanner};

fn main() {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("Usage: cargo run --no-default-features --example scan -- /path/to/folder");
        std::process::exit(2);
    };
    match scanner::scan(&path, &AtomicBool::new(false), |_| {}) {
        Ok(scan) => {
            let root = &scan.nodes[scan.root];
            println!("{}\nAllocated: {}\nLogical: {}\nFiles: {}\nSubfolders: {}\nIssues: {}\nElapsed: {:.3}s",
                scan.root_path.display(), format_bytes(root.total_allocated_bytes),
                format_bytes(root.total_logical_bytes), root.file_count,
                root.directory_count.saturating_sub(1), root.issue_count, scan.elapsed.as_secs_f64());
            for id in root.children.iter().take(20) {
                let node = &scan.nodes[*id];
                println!(
                    "{:>12}  {:>12}  {}",
                    format_bytes(node.total_allocated_bytes),
                    format_bytes(node.total_logical_bytes),
                    node.display_name()
                );
            }
            if root.issue_count > 0 {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("Scan failed: {error}");
            std::process::exit(1);
        }
    }
}
