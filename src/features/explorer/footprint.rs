//! Bounded, read-only treemap projection. Never duplicates the scan or all children.
use crate::features::scan::{NodeId, ScanOutcome};

#[derive(Debug, Clone)]
pub struct Tile {
    pub node: Option<NodeId>,
    pub label: String,
    pub bytes: u64,
    /// Normalized x, y, width, height. Areas represent allocated bytes.
    pub rect: [f32; 4],
}

/// Largest 11 direct children plus an aggregate remainder (including own metadata).
/// Work is O(children × 11), with at most 12 tiles retained, regardless of tree size.
pub fn project(scan: &ScanOutcome, parent: NodeId) -> Vec<Tile> {
    let root = &scan.nodes[parent];
    let mut largest: Vec<NodeId> = Vec::with_capacity(12);
    for &id in &root.children {
        if scan.nodes[id].total_allocated_bytes == 0 {
            continue;
        }
        let pos = largest.partition_point(|&other| {
            scan.nodes[other].total_allocated_bytes >= scan.nodes[id].total_allocated_bytes
        });
        if pos < 11 {
            largest.insert(pos, id);
            largest.truncate(11);
        }
    }
    let mut tiles: Vec<_> = largest
        .into_iter()
        .map(|id| Tile {
            node: Some(id),
            label: scan.nodes[id].display_name().into_owned(),
            bytes: scan.nodes[id].total_allocated_bytes,
            rect: [0.0; 4],
        })
        .collect();
    let shown = tiles
        .iter()
        .fold(0u64, |sum, t| sum.saturating_add(t.bytes));
    let remainder = root.total_allocated_bytes.saturating_sub(shown);
    if remainder > 0 {
        tiles.push(Tile {
            node: None,
            label: "其他 / 目录元数据".into(),
            bytes: remainder,
            rect: [0.0; 4],
        });
    }
    split(&mut tiles, [0.0, 0.0, 1.0, 1.0]);
    tiles
}

fn split(tiles: &mut [Tile], rect: [f32; 4]) {
    if tiles.is_empty() {
        return;
    }
    if tiles.len() == 1 {
        tiles[0].rect = rect;
        return;
    }
    let total: f64 = tiles.iter().map(|t| t.bytes as f64).sum();
    let mut subtotal = 0.0;
    let mut pivot = 1;
    for (index, tile) in tiles.iter().enumerate().take(tiles.len() - 1) {
        subtotal += tile.bytes as f64;
        pivot = index + 1;
        if subtotal >= total / 2.0 {
            break;
        }
    }
    let fraction = (subtotal / total) as f32;
    let (left, right) = tiles.split_at_mut(pivot);
    let [x, y, w, h] = rect;
    // The displayed chart is approximately 4:1, not a square.
    if w * 4.0 >= h {
        split(left, [x, y, w * fraction, h]);
        split(right, [x + w * fraction, y, w * (1.0 - fraction), h]);
    } else {
        split(left, [x, y, w, h * fraction]);
        split(right, [x, y + h * fraction, w, h * (1.0 - fraction)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_has_finite_proportional_areas_and_no_overlap() {
        let mut tiles: Vec<_> = [80, 41, 32, 16, 8, 4, 2, 1]
            .into_iter()
            .map(|bytes| Tile {
                node: None,
                label: String::new(),
                bytes,
                rect: [0.0; 4],
            })
            .collect();
        split(&mut tiles, [0.0, 0.0, 1.0, 1.0]);
        let total: u64 = tiles.iter().map(|t| t.bytes).sum();
        for (i, a) in tiles.iter().enumerate() {
            let [x, y, w, h] = a.rect;
            assert!(x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0);
            assert!(x + w <= 1.00001 && y + h <= 1.00001);
            assert!((w * h - a.bytes as f32 / total as f32).abs() < 0.00001);
            for b in &tiles[i + 1..] {
                let [bx, by, bw, bh] = b.rect;
                let overlap_w = (x + w).min(bx + bw) - x.max(bx);
                let overlap_h = (y + h).min(by + bh) - y.max(by);
                assert!(overlap_w < 0.00001 || overlap_h < 0.00001);
            }
        }
    }
    #[test]
    fn empty_layout_is_safe() {
        split(&mut [], [0.0, 0.0, 1.0, 1.0]);
    }
}
