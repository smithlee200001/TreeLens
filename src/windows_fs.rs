//! Metadata-only Windows accounting. Never open file data or follow the final
//! reparse point. Callers must still validate ancestors: path-based enumeration
//! is a best-effort snapshot and cannot eliminate concurrent rename races.
use std::fs::{Metadata, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryInfo {
    pub volume: u64,
    pub id: u128,
    pub allocated: u64,
    pub directory: bool,
    pub reparse: bool,
    pub links: u32,
}

pub(crate) fn is_reparse(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

fn get_info<T: Default>(handle: HANDLE, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: the caller pairs each class with its documented structure; the
    // buffer is writable, correctly aligned, and remains live for the call.
    unsafe {
        GetFileInformationByHandleEx(
            handle,
            class,
            (&mut value as *mut T).cast(),
            std::mem::size_of::<T>() as u32,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))?;
    Ok(value)
}

pub(crate) fn query(path: &Path) -> io::Result<EntryInfo> {
    let file = OpenOptions::new()
        // FILE_READ_ATTRIBUTES does not request data access or hydrate cloud
        // placeholders. OPEN_REPARSE_POINT prevents final-component redirection.
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    let handle = HANDLE(file.as_raw_handle());
    let attributes: FILE_ATTRIBUTE_TAG_INFO = get_info(handle, FileAttributeTagInfo)?;
    let identity: FILE_ID_INFO = get_info(handle, FileIdInfo)?;
    let id = u128::from_le_bytes(identity.FileId.Identifier);
    if id == 0 {
        return Err(io::Error::other(
            "Filesystem returned an unknown file identity",
        ));
    }
    let standard: FILE_STANDARD_INFO = get_info(handle, FileStandardInfo)?;
    let reparse = attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
    let directory = attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    let allocated = if directory || reparse {
        0
    } else if attributes.FileAttributes
        & (FILE_ATTRIBUTE_COMPRESSED.0 | FILE_ATTRIBUTE_SPARSE_FILE.0)
        != 0
    {
        let compression: FILE_COMPRESSION_INFO = get_info(handle, FileCompressionInfo)?;
        compression.CompressedFileSize
    } else {
        standard.AllocationSize
    };
    let allocated = u64::try_from(allocated)
        .map_err(|_| io::Error::other("Filesystem returned a negative allocation size"))?;
    Ok(EntryInfo {
        volume: identity.VolumeSerialNumber,
        id,
        allocated,
        directory,
        reparse,
        links: standard.NumberOfLinks,
    })
}
