use super::Identity;
use std::{fs::Metadata, os::unix::fs::MetadataExt, path::Path};
pub(super) fn identity(_: &Path, metadata: &Metadata) -> Result<Identity, String> {
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino() as u128,
        directory: metadata.is_dir(),
    })
}
