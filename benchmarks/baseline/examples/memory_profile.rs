use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;
use tree_lens::{
    scanner::{scan, NodeKind},
    tree,
};
struct Meter;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
fn add(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        System.dealloc(p, layout);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = System.realloc(p, layout, size);
        if !q.is_null() {
            if size >= layout.size() {
                add(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static ALLOCATOR: Meter = Meter;
fn start() -> usize {
    let n = LIVE.load(Ordering::Relaxed);
    PEAK.store(n, Ordering::Relaxed);
    n
}
fn stats(base: usize) -> (usize, usize) {
    (
        LIVE.load(Ordering::Relaxed) - base,
        PEAK.load(Ordering::Relaxed) - base,
    )
}
fn main() {
    let path = std::env::args_os()
        .nth(1)
        .expect("usage: memory_profile FIXTURE_PATH");
    let baseline = start();
    let t = Instant::now();
    let scan = scan(std::path::Path::new(&path), &AtomicBool::new(false), |_| {}).unwrap();
    let scan_time = t.elapsed();
    let scan_stats = stats(baseline);
    let expanded: HashSet<_> = scan
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Directory)
        .map(|n| n.id)
        .collect();
    let expansion_bytes = LIVE.load(Ordering::Relaxed) - baseline - scan_stats.0;
    let base = start();
    let t = Instant::now();
    let view = tree::visible_rows(&scan, &expanded, "", tree::SortBy::Allocated);
    let view_time = t.elapsed();
    let view_stats = stats(base);
    let row_count = view.len();
    let base = start();
    let t = Instant::now();
    let viewport = view[..50.min(row_count)].to_vec();
    std::hint::black_box(&viewport);
    let viewport_time = t.elapsed();
    let viewport_stats = stats(base);
    drop(viewport);
    // Original UI reused cached rows on scroll; copy a window for comparison.
    let t = Instant::now();
    let end = view[row_count.saturating_sub(50)..].to_vec();
    std::hint::black_box(&end);
    let end_time = t.elapsed();
    println!("nodes={} files={} logical_bytes={} node_size={} scan_retained_bytes={} scan_peak_bytes={} scan_ms={:.3} expanded_set_bytes={} view_retained_bytes={} view_peak_bytes={} view_build_ms={:.3} row_count={} viewport_retained_bytes={} viewport_peak_bytes={} viewport_us={:.3} end_scroll_us={:.3}",scan.nodes.len(),scan.nodes[scan.root].file_count,scan.nodes[scan.root].total_logical_bytes,std::mem::size_of::<tree_lens::scanner::Node>(),scan_stats.0,scan_stats.1,scan_time.as_secs_f64()*1000.0,expansion_bytes,view_stats.0,view_stats.1,view_time.as_secs_f64()*1000.0,row_count,viewport_stats.0,viewport_stats.1,viewport_time.as_secs_f64()*1e6,end_time.as_secs_f64()*1e6);
}
