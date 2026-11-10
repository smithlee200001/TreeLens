//! Iterative, read-only filesystem accounting. Symlinks are never intentionally
//! followed and child mount points are not traversed. This is a best-effort view
//! of a live filesystem, not an atomic snapshot. Clones/compression/snapshots may
//! share blocks that cannot be attributed exactly using filesystem metadata.

mod model;
mod service;

pub use model::{Node, NodeId, NodeKind, ScanIssue, ScanOutcome, ScanProgress};
pub use service::scan;

#[cfg(test)]
mod tests;
