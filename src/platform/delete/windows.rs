use super::{directory_identity, entry_identity, Identity};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
pub(super) fn identity(path: &Path, metadata: &fs::Metadata) -> Result<Identity, String> {
    let entry = crate::platform::fs::windows::query(path)
        .map_err(|error| format!("无法检查项目身份：{error}"))?;
    if entry.reparse || entry.directory != metadata.is_dir() {
        return Err("不能删除重解析点、符号链接或联接目录。".into());
    }
    Ok(Identity {
        device: entry.volume,
        inode: entry.id,
        directory: entry.directory,
    })
}
pub(crate) use validate_windows_ancestors as validate_root;
pub(crate) use validate_windows_ancestors as validate_entry_path;
pub(crate) fn validate_subtree_root(_: &Path) -> Result<(), String> {
    Ok(())
}
pub(crate) fn validate_target(original_root: &Path, path: &Path) -> Result<(), String> {
    validate_windows_ancestors(original_root)?;
    validate_windows_ancestors(path)?;
    if fs::canonicalize(path).map_err(|e| e.to_string())? != path {
        return Err("文件夹路径已改变，请重新扫描。".into());
    }
    Ok(())
}
/// Only drive-letter, fixed local volumes are offered to the Shell. Inspect the
/// entire original chain before canonicalization, which would hide junctions.
pub(crate) fn validate_windows_ancestors(path: &Path) -> Result<(), String> {
    use std::path::Prefix;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut components = absolute.components();
    let drive = match components.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
            _ => return Err("仅支持本地固定磁盘的回收站，不支持网络或设备路径。".into()),
        },
        _ => return Err("需要完整的本地磁盘路径。".into()),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err("不支持相对磁盘路径。".into());
    }
    let root = [drive as u16, b':' as u16, b'\\' as u16, 0];
    // DRIVE_FIXED = 3. Removable, remote, optical and unknown drives fail closed.
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != 3 {
        return Err("此磁盘不支持安全的回收站操作。".into());
    }
    for component in components {
        match component {
            Component::Normal(name) => {
                use std::os::windows::ffi::OsStrExt;
                let units: Vec<_> = name.encode_wide().collect();
                if units.contains(&0)
                    || units.contains(&(b':' as u16))
                    || matches!(units.last(), Some(32 | 46))
                {
                    return Err("路径可能被 Windows Shell 重新解释，已取消操作。".into());
                }
            }
            _ => return Err("文件夹路径不是规范的本地路径。".into()),
        }
    }
    let mut expected_volume = None;
    for (index, ancestor) in absolute.ancestors().enumerate() {
        let identity = if index == 0 {
            entry_identity(ancestor)?
        } else {
            directory_identity(ancestor)?
        };
        if expected_volume.is_some_and(|volume| volume != identity.device) {
            return Err("不能删除挂载点或跨磁盘的文件夹。".into());
        }
        expected_volume = Some(identity.device);
    }
    Ok(())
}

fn exact_windows_path(path: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err("路径包含空字符，已取消操作。".into());
    }
    units.push(0);
    Ok(units)
}

pub(crate) fn move_to_trash(path: &Path) -> Result<(), String> {
    windows_recycle::recycle(path)
}

mod windows_recycle {
    use super::*;
    use std::sync::{Arc, Mutex};
    use windows::core::{implement, Error, Ref, Result as WinResult, HRESULT, PCWSTR};
    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::*;

    #[derive(Default)]
    struct Outcome {
        completed: bool,
        failure: Option<String>,
    }

    #[implement(IFileOperationProgressSink)]
    struct RecycleSink {
        outcome: Arc<Mutex<Outcome>>,
        expected: Identity,
    }

    impl RecycleSink_Impl {
        fn reject(&self, reason: &str) -> WinResult<()> {
            self.outcome.lock().unwrap().failure = Some(reason.into());
            Err(Error::from_hresult(E_ABORT))
        }
    }

    // These flags alone are not a no-permanent-delete guarantee. The sink vetoes
    // any direct-delete callback, including a Shell fallback after recycling fails.
    // https://learn.microsoft.com/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifileoperationprogresssink-predeleteitem
    #[allow(non_snake_case, unused_variables)]
    impl IFileOperationProgressSink_Impl for RecycleSink_Impl {
        fn StartOperations(&self) -> WinResult<()> {
            Ok(())
        }
        fn FinishOperations(&self, result: HRESULT) -> WinResult<()> {
            if result.is_err() {
                return self.reject("回收站操作未完成。");
            }
            Ok(())
        }
        fn PreDeleteItem(&self, flags: u32, item: Ref<'_, IShellItem>) -> WinResult<()> {
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                return self.reject("此项目无法放入回收站；已拒绝永久删除。");
            }
            let Some(item) = item.as_ref() else {
                return self.reject("无法验证 Shell 项目。");
            };
            // Check the path actually resolved by Shell, not a lossy display name.
            let resolved = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }?;
            use std::os::windows::ffi::OsStringExt;
            let path = PathBuf::from(unsafe { std::ffi::OsString::from_wide(resolved.as_wide()) });
            unsafe { CoTaskMemFree(Some(resolved.0.cast())) };
            if validate_windows_ancestors(&path).is_err()
                || entry_identity(&path).ok().as_ref() != Some(&self.expected)
            {
                return self.reject("Shell 项目与已确认的项目不一致，已取消操作。");
            }
            Ok(())
        }
        fn PostDeleteItem(
            &self,
            flags: u32,
            item: Ref<'_, IShellItem>,
            result: HRESULT,
            recycled: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            // A success-like skip code is not a completed recycle. A nonnull
            // destination is documented to identify the item in the Recycle Bin.
            if result != HRESULT(0) || recycled.is_none() {
                return self.reject("未能确认项目已进入回收站。请检查项目位置。");
            }
            self.outcome.lock().unwrap().completed = true;
            Ok(())
        }
        fn PreRenameItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostRenameItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            hrrename: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn PreMoveItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostMoveItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            hrmove: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn PreCopyItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostCopyItem(
            &self,
            dwflags: u32,
            psiitem: Ref<'_, IShellItem>,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            hrcopy: HRESULT,
            psinewlycreated: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn PreNewItem(
            &self,
            dwflags: u32,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
        ) -> WinResult<()> {
            self.reject("不允许执行非回收站操作。")
        }
        fn PostNewItem(
            &self,
            dwflags: u32,
            psidestinationfolder: Ref<'_, IShellItem>,
            psznewname: &PCWSTR,
            psztemplatename: &PCWSTR,
            dwfileattributes: u32,
            hrnew: HRESULT,
            psinewitem: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn UpdateProgress(&self, iworktotal: u32, iworksofar: u32) -> WinResult<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> WinResult<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> WinResult<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> WinResult<()> {
            Ok(())
        }
    }

    pub(super) fn recycle(path: &Path) -> Result<(), String> {
        validate_windows_ancestors(path)?;
        let expected = entry_identity(path)?;
        let exact = exact_windows_path(path)?;
        // execute() runs on a fresh worker. Never borrow an incompatible COM
        // apartment; failed initialization leaves the filesystem untouched.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| format!("无法初始化回收站操作：{e}"))?;
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _apartment = Apartment;
        let outcome = Arc::new(Mutex::new(Outcome::default()));
        let sink: IFileOperationProgressSink = RecycleSink {
            outcome: outcome.clone(),
            expected,
        }
        .into();
        let perform = || -> WinResult<()> {
            unsafe {
                let operation: IFileOperation =
                    CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
                operation.SetOperationFlags(
                    FOFX_RECYCLEONDELETE
                        | FOFX_EARLYFAILURE
                        | FOFX_ADDUNDORECORD
                        | FOF_NOERRORUI
                        | FOF_SILENT
                        | FOF_NORECURSEREPARSE,
                )?;
                let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(exact.as_ptr()), None)?;
                operation.DeleteItem(&item, &sink)?;
                operation.PerformOperations()?;
                if operation.GetAnyOperationsAborted()?.as_bool() {
                    return Err(Error::from_hresult(E_ABORT));
                }
                Ok(())
            }
        };
        let result = perform();
        let state = outcome.lock().unwrap();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        result.map_err(|e| format!("移到回收站失败：{e}。未尝试永久删除。"))?;
        if !state.completed {
            return Err("回收站操作未确认完成。".into());
        }
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn sink() -> (IFileOperationProgressSink, Arc<Mutex<Outcome>>) {
            let state = Arc::new(Mutex::new(Outcome::default()));
            (
                RecycleSink {
                    outcome: state.clone(),
                    expected: Identity {
                        device: 1,
                        inode: 1,
                        directory: true,
                    },
                }
                .into(),
                state,
            )
        }
        #[test]
        fn direct_delete_callback_is_vetoed_without_touching_disk() {
            let (sink, state) = sink();
            assert!(unsafe { sink.PreDeleteItem(0, None::<&IShellItem>) }.is_err());
            assert!(state.lock().unwrap().failure.is_some());
        }
        #[test]
        fn per_item_error_and_missing_recycle_destination_are_failures() {
            for result in [HRESULT(0), E_ABORT, HRESULT(1)] {
                let (sink, state) = sink();
                assert!(unsafe {
                    sink.PostDeleteItem(0, None::<&IShellItem>, result, None::<&IShellItem>)
                }
                .is_err());
                assert!(!state.lock().unwrap().completed);
                assert!(state.lock().unwrap().failure.is_some());
            }
        }
        #[test]
        fn finish_error_is_not_lost() {
            let (sink, state) = sink();
            assert!(unsafe { sink.FinishOperations(E_ABORT) }.is_err());
            assert!(state.lock().unwrap().failure.is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn exact_utf16_never_substitutes_unpaired_surrogates() {
        use std::os::windows::ffi::OsStringExt;
        let units = [67, 58, 92, 0xd800, 0x6587, 0xd83e, 0xdd80];
        let name = std::ffi::OsString::from_wide(&units);
        let path = PathBuf::from(name);
        assert_eq!(
            exact_windows_path(&path).unwrap(),
            [units.as_slice(), &[0]].concat()
        );
        assert!(
            exact_windows_path(Path::new(&std::ffi::OsString::from_wide(&[65, 0, 66]))).is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn unc_and_device_paths_are_rejected_without_shell_operation() {
        for path in [
            r"\\server\share\folder",
            r"\\?\UNC\server\share\folder",
            r"\\.\C:\folder",
        ] {
            assert!(validate_windows_ancestors(Path::new(path)).is_err());
        }
    }
}
