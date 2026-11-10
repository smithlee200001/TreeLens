//! Confirmation-bound recursive inventory; native safety checks stay in platform.
use super::*;
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SubtreeEntry {
    pub(super) identity: Identity,
    // In-place edits should invalidate confirmation too. Directory mtimes are
    // excluded because our own bottom-up removals change them.
    pub(super) file_state: Option<(u64, std::time::SystemTime)>,
}

pub(super) type Subtree = std::collections::BTreeMap<PathBuf, SubtreeEntry>;

pub(super) fn subtree_entry(path: &Path) -> Result<SubtreeEntry, String> {
    let identity = entry_identity(path)?;
    let file_state = if identity.directory {
        None
    } else {
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        Some((
            metadata.len(),
            metadata.modified().map_err(|e| e.to_string())?,
        ))
    };
    Ok(SubtreeEntry {
        identity,
        file_state,
    })
}

/// Snapshot without following links. Every directory is checked again before
/// enumeration; this is conservative validation, not an atomic filesystem lock.
pub(super) fn snapshot_subtree(
    root: &Path,
    device: u64,
    cancelled: &AtomicBool,
) -> Result<Subtree, String> {
    platform::validate_subtree_root(root)?;
    let mut result = Subtree::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if cancelled.load(Ordering::Relaxed) {
            return Err("已取消内容检查；没有删除任何项目。".into());
        }
        for ancestor in path.ancestors().skip(1).take_while(|p| p.starts_with(root)) {
            let expected: &SubtreeEntry = result.get(ancestor).ok_or("目录结构已改变。")?;
            if directory_identity(ancestor)? != expected.identity {
                return Err("上级目录已改变，请重新扫描。".into());
            }
        }
        platform::validate_entry_path(&path)?;
        let entry = subtree_entry(&path)?;
        if entry.identity.device != device || path.to_str().is_none() {
            return Err("子目录包含跨磁盘项目或无法准确显示的名称，已取消删除。".into());
        }
        if entry.identity.directory {
            for child in fs::read_dir(&path).map_err(|e| format!("无法检查全部子项目：{e}"))?
            {
                if cancelled.load(Ordering::Relaxed) {
                    return Err("已取消内容检查；没有删除任何项目。".into());
                }
                pending.push(child.map_err(|e| e.to_string())?.path());
            }
        }
        result.insert(path, entry);
    }
    Ok(result)
}
