//! OS adapters. Native APIs and target selection stay behind this boundary.
pub(crate) mod delete;
#[cfg(feature = "gui")]
pub(crate) mod desktop;
mod folders;
pub(crate) mod fs;
pub(crate) mod paths;
pub(crate) mod scan;
mod vocabulary;

pub use folders::{home_dir, shortcut_path};
pub use vocabulary::*;
