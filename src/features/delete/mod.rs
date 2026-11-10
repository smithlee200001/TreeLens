//! Recoverable file/folder deletion and explicitly confirmed permanent deletion.
//! Trash failures never fall back to permanent deletion.
//!
//! Identity checks narrow the race between confirmation and deletion, but
//! native Trash APIs and remove_file accept a pathname, not a bound identity. Another process
//! can still replace a component after the last check. This is not an atomic
//! security boundary against a concurrently hostile filesystem.

use crate::features::scan::{NodeId, NodeKind, ScanOutcome};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::platform::delete::{
    self as platform, directory_identity, entry_identity, move_to_trash, Identity,
};
mod snapshot;
use snapshot::{snapshot_subtree, subtree_entry, Subtree};

/// The confirmation binds the selected action; Trash never escalates to Permanent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionMode {
    Trash,
    Permanent,
}

/// A single-use confirmation snapshot. Construct when opening the confirmation
/// dialog, keep its path visible, then consume it only after explicit confirmation.
#[derive(Debug)]
pub struct DeletionRequest {
    original_root: PathBuf,
    root: PathBuf,
    path: PathBuf,
    identities: Vec<(PathBuf, Identity)>,
    mode: DeletionMode,
    subtree: Option<Subtree>,
}

impl DeletionRequest {
    pub fn prepare(scan: &ScanOutcome, id: NodeId) -> Result<Self, String> {
        Self::prepare_with_mode(scan, id, DeletionMode::Trash)
    }

    pub fn prepare_with_mode(
        scan: &ScanOutcome,
        id: NodeId,
        mode: DeletionMode,
    ) -> Result<Self, String> {
        Self::prepare_target_with_mode(scan, id, mode)?.snapshot_contents()
    }

    /// Fast target-only validation; recursive capture must run off the UI thread.
    pub fn prepare_target_with_mode(
        scan: &ScanOutcome,
        id: NodeId,
        mode: DeletionMode,
    ) -> Result<Self, String> {
        if scan.nodes.get(scan.root).is_none_or(|root| {
            root.kind != NodeKind::Directory || root.parent.is_some() || root.id != scan.root
        }) {
            return Err("扫描根目录数据已失效。".into());
        }
        if id == scan.root {
            return Err("不能删除本次扫描的根目录。".into());
        }
        let node = scan.nodes.get(id).ok_or("所选项目已失效，请重新扫描。")?;
        if !matches!(node.kind, NodeKind::Directory | NodeKind::File) {
            return Err("只能删除普通文件或文件夹。".into());
        }
        let target_directory = node.kind == NodeKind::Directory;
        // Do not use path_for on unchecked parent links: stale/malformed input
        // must fail rather than panic, loop, or escape the root via a filename.
        let mut names = Vec::new();
        let mut cursor = id;
        for _ in 0..scan.nodes.len() {
            if cursor == scan.root {
                break;
            }
            let node = scan.nodes.get(cursor).ok_or("文件夹路径已失效。")?;
            let name = Path::new(node.name.as_ref());
            let mut components = name.components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || node.id != cursor
                || (cursor != id && node.kind != NodeKind::Directory)
            {
                return Err("文件夹路径不安全，请重新扫描。".into());
            }
            names.push(node.name.as_ref());
            let parent = node.parent.ok_or("项目不属于扫描根目录。")?;
            if scan
                .nodes
                .get(parent)
                .is_none_or(|parent_node| !parent_node.children.contains(&cursor))
                || (node.kind == NodeKind::File && !node.children.is_empty())
            {
                return Err("扫描目录结构已失效，请重新扫描。".into());
            }
            cursor = parent;
        }
        if cursor != scan.root || names.is_empty() {
            return Err("文件夹不属于扫描根目录。".into());
        }
        platform::validate_root(&scan.root_path)?;
        directory_identity(&scan.root_path)?;
        let root = fs::canonicalize(&scan.root_path)
            .map_err(|error| format!("无法确认扫描根目录：{error}"))?;
        let root_identity = directory_identity(&root)?;
        let mut identities = vec![(root.clone(), root_identity)];
        let mut path = root.clone();
        let count = names.len();
        for (index, name) in names.into_iter().rev().enumerate() {
            path.push(name);
            let identity = entry_identity(&path)?;
            let expected_directory = index + 1 < count || target_directory;
            if identity.directory != expected_directory {
                return Err("项目类型已改变，请重新扫描。".into());
            }
            if identity.device != identities[0].1.device {
                return Err("不能删除挂载点或跨磁盘的文件夹。".into());
            }
            identities.push((path.clone(), identity));
        }
        if path == root || path.parent().is_none() || !path.starts_with(&root) {
            return Err("不能删除根目录或扫描范围外的文件夹。".into());
        }
        // Foundation path strings must be exact. Never act on display_name or
        // to_string_lossy: they can identify a different entry.
        // The confirmation UI displays Unicode. Refuse names it cannot show
        // exactly, even though Windows APIs can preserve unpaired UTF-16 units.
        if path.to_str().is_none() {
            return Err("此项目路径包含非 UTF-8 / Unicode 名称，无法准确显示确认路径，请在系统文件管理器中处理。".into());
        }
        let request = Self {
            original_root: scan.root_path.clone(),
            root,
            path,
            identities,
            mode,
            subtree: None,
        };
        request.revalidate()?;
        Ok(request)
    }

    /// Capture before showing confirmation. Dropping this request or a worker's
    /// result cancels preparation without deleting anything.
    pub fn snapshot_contents(self) -> Result<Self, String> {
        self.snapshot_contents_with_cancel(&AtomicBool::new(false))
    }

    pub fn snapshot_contents_with_cancel(mut self, cancelled: &AtomicBool) -> Result<Self, String> {
        if cancelled.load(Ordering::Relaxed) {
            return Err("已取消内容检查；没有删除任何项目。".into());
        }
        self.revalidate()?;
        if self.mode == DeletionMode::Permanent
            && self
                .identities
                .last()
                .is_some_and(|(_, identity)| identity.directory)
        {
            self.subtree = Some(snapshot_subtree(
                &self.path,
                self.identities[0].1.device,
                cancelled,
            )?);
            self.revalidate()?;
        }
        Ok(self)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn mode(&self) -> DeletionMode {
        self.mode
    }

    fn revalidate(&self) -> Result<(), String> {
        platform::validate_target(&self.original_root, &self.path)?;
        directory_identity(&self.original_root)?;
        if fs::canonicalize(&self.original_root)
            .map_err(|error| format!("无法重新检查扫描根目录：{error}"))?
            != self.root
        {
            return Err("扫描根目录已改变，请重新扫描。".into());
        }
        for (path, expected) in &self.identities {
            if entry_identity(path)? != *expected {
                return Err("项目或上级目录已改变，已取消操作。请重新扫描。".into());
            }
        }
        Ok(())
    }

    pub fn execute(self) -> Result<(), String> {
        match self.mode {
            DeletionMode::Trash => self.execute_with(move_to_trash),
            DeletionMode::Permanent => {
                self.revalidate()?;
                if let Some(subtree) = &self.subtree {
                    if snapshot_subtree(
                        &self.path,
                        self.identities[0].1.device,
                        &AtomicBool::new(false),
                    )? != *subtree
                    {
                        return Err("文件夹内容已改变，已取消操作。请重新扫描并确认。".into());
                    }
                    // Never recursively sweep a live directory: remove exactly
                    // the confirmed entries. New children make remove_dir fail.
                    for (path, expected) in subtree.iter().rev() {
                        let remove = || -> Result<(), String> {
                            self.revalidate()?;
                            for ancestor in path
                                .ancestors()
                                .skip(1)
                                .take_while(|p| p.starts_with(&self.path))
                            {
                                let expected = subtree.get(ancestor).ok_or("目录结构已改变。")?;
                                if directory_identity(ancestor)? != expected.identity {
                                    return Err("上级目录已改变。".into());
                                }
                            }
                            platform::validate_entry_path(path)?;
                            if subtree_entry(path)? != *expected {
                                return Err("子项目已改变。".into());
                            }
                            if expected.identity.directory {
                                fs::remove_dir(path)
                            } else {
                                fs::remove_file(path)
                            }
                            .map_err(|e| e.to_string())
                        };
                        remove().map_err(|error| {
                            format!("彻底删除中止：{error}。部分已确认项目可能已删除，请重新扫描。")
                        })?;
                    }
                    Ok(())
                } else {
                    if self
                        .identities
                        .last()
                        .is_none_or(|(_, identity)| identity.directory)
                    {
                        return Err("文件夹尚未完成内容检查，已取消彻底删除。".into());
                    }
                    fs::remove_file(&self.path).map_err(|error| format!("彻底删除失败：{error}"))
                }
            }
        }
    }

    fn execute_with(self, trash: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
        if self.mode != DeletionMode::Trash {
            return Err("确认的删除方式与回收站操作不一致。".into());
        }
        self.revalidate()?;
        trash(&self.path)
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
