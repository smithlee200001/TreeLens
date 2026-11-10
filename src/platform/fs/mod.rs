//! Shared native filesystem primitives. No feature or presentation dependencies.
#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(windows)]
pub(crate) mod windows;
