//! View lifecycle, navigation, and guarded deletion orchestration.
use super::*;

impl TreeLensApp {
    pub fn new(initial: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入路径，或拖入文件夹…")
                .default_value(
                    initial
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                )
        });
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("筛选名称…"));
        let subscriptions = vec![
            cx.subscribe_in(&path_input, window, |this, state, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let value = state.read(cx).value().to_string();
                    if !value.trim().is_empty() {
                        this.start(expand_home(&value), window, cx);
                    }
                }
            }),
            cx.subscribe_in(&search_input, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.session.set_query(state.read(cx).value().to_string());
                    cx.notify();
                }
            }),
        ];
        let ticker = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            if this
                .update_in(cx, |this, window, cx| {
                    this.poll_deletion_preparation(window, cx);
                    this.poll_deletion(window, cx);
                    this.poll_session(window, cx);
                })
                .is_err()
            {
                break;
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut app = Self {
            session: Session::new(),
            path_input,
            search_input,
            focus,
            scroll: UniformListScrollHandle::new(),
            tiles: Vec::new(),
            map_parent: None,
            generation: 0,
            choosing: false,
            _subscriptions: subscriptions,
            _ticker: ticker,
            picker: None,
            pending_deletion: None,
            preparing_deletion: None,
            deletion: None,
            deletion_notice: None,
            deletion_mode: None,
            delete_operation: DeletionRequest::execute,
        };
        if let Some(path) = initial {
            app.start(path, window, cx);
        }
        app
    }

    pub(super) fn start(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.pending_deletion.is_some() {
            return;
        }
        self.pending_deletion = None;
        self.deletion_notice = None;
        let retained_view = self.session.result.is_some();
        if !self.session.start(path.clone()) {
            if retained_view {
                if let Some(root) = &self.session.root_path {
                    self.path_input.update(cx, |input, cx| {
                        input.set_value(root.display().to_string(), window, cx)
                    });
                }
            }
            cx.notify();
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.path_input.update(cx, |input, cx| {
            input.set_value(path.display().to_string(), window, cx)
        });
        // A new scan root does not evict the displayed root. Keep its exact
        // panels and navigation until a complete replacement is accepted.
        if !retained_view {
            self.search_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.tiles.clear();
            self.map_parent = None;
        }
        cx.notify();
    }

    pub(super) fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.choosing || window.has_active_dialog(cx) {
            return;
        }
        self.choosing = true;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择要分析的文件夹".into()),
        });
        self.picker = Some(cx.spawn_in(window, async move |this, cx| {
            let result = picker.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.choosing = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.start(path, window, cx);
                        }
                    }
                    Ok(Ok(None)) => {} // Dismissing the native picker preserves the prior scan.
                    Ok(Err(error)) => {
                        this.session.status = format!("无法打开文件夹选择窗口：{error}")
                    }
                    Err(error) => this.session.status = format!("文件夹选择窗口已断开：{error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Reconciliation keeps the same list and panels mounted. Capture navigation
    /// immediately before applying a worker result, so scrolling, selection and
    /// map navigation performed while the worker ran are never rolled back.
    pub(super) fn poll_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_busy = self.session.busy();
        let was_replacing = self.session.replacing();
        let revision = self.session.revision();
        let reconciling = self.session.reconciling();
        let applied_query = self.session.applied_query().to_owned();
        let map_path = reconciling
            .then(|| {
                self.map_parent
                    .and_then(|id| self.session.result.as_ref().map(|scan| scan.path_for(id)))
            })
            .flatten();
        let anchor = reconciling.then(|| self.scroll_anchor()).flatten();
        if !self.session.poll() {
            return;
        }
        let query_changed = applied_query != self.session.applied_query();
        if was_busy && !self.session.busy() {
            let accepted = revision != self.session.revision();
            if accepted {
                // Every accepted scan can recycle dense IDs, not only deletion.
                self.generation = self.generation.wrapping_add(1);
            }
            if reconciling {
                if let Some(scan) = &self.session.result {
                    let parent = map_path
                        .as_deref()
                        .and_then(|path| {
                            path.ancestors().find_map(|ancestor| {
                                self.session
                                    .node_for_path(ancestor)
                                    .filter(|&id| scan.nodes[id].kind == NodeKind::Directory)
                            })
                        })
                        .unwrap_or(scan.root);
                    self.map_parent = Some(parent);
                    self.tiles = footprint::project(scan, parent);
                }
                if !query_changed {
                    if let Some(anchor) = anchor {
                        self.restore_scroll_anchor(anchor);
                    }
                }
            } else if accepted {
                self.reset_projection();
                self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                if was_replacing {
                    self.search_input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            if was_replacing {
                // On failure/cancellation Session restores the displayed root.
                // Never leave an editable target path claiming to be the view.
                if let Some(root) = &self.session.root_path {
                    self.path_input.update(cx, |input, cx| {
                        input.set_value(root.display().to_string(), window, cx)
                    });
                }
            }
        }
        if query_changed {
            self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    pub(super) fn scroll_anchor(&self) -> Option<(PathBuf, usize, Point<Pixels>, Pixels)> {
        let state = self.scroll.0.borrow();
        // Respect an explicit user scroll scheduled for the next layout.
        if state.deferred_scroll_to_item.is_some() {
            return None;
        }
        // GPUI last_item_size.item is the viewport, not one row.
        let height = px(TREE_ROW_HEIGHT);
        let offset = state.base_handle.offset();
        let index = ((-offset.y / height).floor().max(0.0) as usize)
            .min(self.session.rows.len().saturating_sub(1));
        let scan = self.session.result.as_ref()?;
        let (id, _) = self.session.rows.row_at(scan, index)?;
        Some((scan.path_for(id), index, offset, height))
    }

    pub(super) fn restore_scroll_anchor(
        &self,
        (path, old_index, offset, height): (PathBuf, usize, Point<Pixels>, Pixels),
    ) {
        let Some(scan) = &self.session.result else {
            return;
        };
        // If the top row itself disappeared, retain its nearest row position.
        // GPUI clamps at the bottom if the list became shorter.
        let index = self
            .session
            .node_for_path(&path)
            .and_then(|id| self.session.rows.row_index(scan, id))
            .unwrap_or(old_index.min(self.session.rows.len().saturating_sub(1)));
        let state = self.scroll.0.borrow();
        state.base_handle.set_offset(point(
            offset.x,
            offset.y + (old_index as f32 - index as f32) * height,
        ));
    }

    pub(super) fn reset_projection(&mut self) {
        self.map_parent = self.session.result.as_ref().map(|scan| scan.root);
        self.tiles = self
            .session
            .result
            .as_ref()
            .map(|scan| footprint::project(scan, scan.root))
            .unwrap_or_default();
    }

    pub(super) fn select_node(&mut self, id: NodeId, cx: &mut Context<Self>) {
        self.session.select(id);
        if let Some(scan) = &self.session.result {
            if scan
                .nodes
                .get(id)
                .is_some_and(|node| node.kind == NodeKind::Directory)
                && self.map_parent != Some(id)
            {
                self.map_parent = Some(id);
                self.tiles = footprint::project(scan, id);
            }
        }
        cx.notify();
    }

    pub(super) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if let Some(path) = self.session.root_path.clone() {
            self.start(path, window, cx);
        }
    }

    pub(super) fn copy_path(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if !self
            .session
            .result
            .as_ref()
            .is_some_and(|scan| id < scan.nodes.len())
        {
            return;
        }
        if let Some(scan) = &self.session.result {
            cx.write_to_clipboard(ClipboardItem::new_string(
                scan.path_for(id).display().to_string(),
            ));
            self.session.status = "已复制完整路径".into();
            cx.notify();
        }
    }

    pub(super) fn reveal(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if !self
            .session
            .result
            .as_ref()
            .is_some_and(|scan| id < scan.nodes.len())
        {
            return;
        }
        if let Some(scan) = &self.session.result {
            if let Err(error) = desktop::reveal(
                &scan.path_for(id),
                scan.nodes[id].kind == NodeKind::Directory,
                cx,
            ) {
                self.session.status = error;
            }
        }
        cx.notify();
    }

    pub(super) fn busy(&self) -> bool {
        self.session.busy() || self.deletion.is_some() || self.preparing_deletion.is_some()
    }

    pub(super) fn deletable_item(&self, id: NodeId, _mode: DeletionMode) -> bool {
        !self.busy()
            && !self.session.stale()
            && !self.choosing
            && self.session.result.as_ref().is_some_and(|scan| {
                id != scan.root
                    && scan.nodes.get(id).is_some_and(|node| {
                        node.error.is_none()
                            && matches!(node.kind, NodeKind::File | NodeKind::Directory)
                    })
            })
    }

    pub(super) fn request_delete(
        &mut self,
        id: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_delete_mode(id, DeletionMode::Trash, window, cx);
    }

    pub(super) fn request_delete_mode(
        &mut self,
        id: NodeId,
        mode: DeletionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !TRASH_SUPPORTED || !self.deletable_item(id, mode) || window.has_active_dialog(cx) {
            return;
        }
        let scan = self.session.result.as_ref().expect("deletable scan");
        let request = match DeletionRequest::prepare_target_with_mode(scan, id, mode) {
            Ok(request) => request,
            Err(error) => {
                self.deletion_notice = None;
                self.session.status = format!("无法删除项目：{error}");
                cx.notify();
                return;
            }
        };
        if mode == DeletionMode::Permanent && scan.nodes[id].kind == NodeKind::Directory {
            let (sender, receiver) = mpsc::sync_channel(1);
            let cancelled = Arc::new(AtomicBool::new(false));
            let worker_cancelled = Arc::clone(&cancelled);
            match std::thread::Builder::new()
                .name("prepare-folder-deletion".into())
                .spawn(move || {
                    let _ = sender.send(request.snapshot_contents_with_cancel(&worker_cancelled));
                }) {
                Ok(_) => {
                    self.preparing_deletion = Some(DeletionPreparation {
                        generation: self.generation,
                        id,
                        receiver,
                        cancelled,
                    });
                    self.deletion_notice = None;
                    self.session.status =
                        "正在核对文件夹全部内容…按 Esc 取消；尚未删除任何项目。".into();
                }
                Err(error) => self.session.status = format!("无法准备删除：{error}"),
            }
            cx.notify();
            return;
        }
        self.show_delete_confirmation(id, request, window, cx);
    }

    pub(super) fn poll_deletion_preparation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match self
            .preparing_deletion
            .as_ref()
            .map(|state| state.receiver.try_recv())
        {
            Some(Ok(result)) => result,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err("文件夹检查意外结束；没有执行删除。".into())
            }
            _ => return,
        };
        let state = self.preparing_deletion.take().expect("preparation exists");
        if state.generation != self.generation
            || self.busy()
            || self.choosing
            || window.has_active_dialog(cx)
        {
            self.session.status = "准备操作已过期，请重新选择项目；没有执行删除。".into();
        } else {
            match result {
                Ok(request) => self.show_delete_confirmation(state.id, request, window, cx),
                Err(error) => self.session.status = format!("无法准备彻底删除：{error}"),
            }
        }
        cx.notify();
    }

    pub(super) fn show_delete_confirmation(
        &mut self,
        id: NodeId,
        request: DeletionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scan = self.session.result.as_ref().expect("confirmed scan");
        let mode = request.mode();
        let path = request.path().display().to_string();
        let size = format_bytes(scan.nodes[id].total_allocated_bytes);
        let directory = scan.nodes[id].kind == NodeKind::Directory;
        let permanent = mode == DeletionMode::Permanent;
        let title = if permanent && directory {
            "彻底删除整个文件夹？".to_string()
        } else if permanent {
            "彻底删除文件？".to_string()
        } else if directory {
            format!("将文件夹移到{TRASH_NAME}？")
        } else {
            format!("将文件移到{TRASH_NAME}？")
        };
        let description = if permanent && directory {
            format!("这个文件夹及其中的全部子文件、子文件夹将被彻底删除，不会进入{TRASH_NAME}，无法通过本应用或{TRASH_NAME}恢复。此操作不可撤销。")
        } else if permanent {
            format!("此文件将被彻底删除，不会进入{TRASH_NAME}，无法通过本应用或{TRASH_NAME}恢复。此操作不可撤销。")
        } else if directory {
            format!("这个文件夹及其全部内容将移到{TRASH_NAME}，不会永久删除。")
        } else {
            format!("此文件将移到{TRASH_NAME}，不会永久删除。")
        };
        let detail = if directory {
            format!("扫描时的磁盘占用：{size}。包含所有内容，即使被筛选或未完整扫描。")
        } else {
            format!("扫描时的磁盘占用：{size}。只删除所选路径，其他硬链接不受影响；实际释放空间可能不同。")
        };
        let warning = if permanent && directory {
            "请核对完整路径。执行中发生权限或路径变化等错误时，可能只有部分内容已删除，且无法撤销。此操作不是安全擦除。".to_string()
        } else if permanent {
            "请核对完整路径，确认文件不再需要。这不是安全擦除，不保证覆盖磁盘上的数据。".to_string()
        } else {
            format!("可在系统{TRASH_NAME}中恢复。移入{TRASH_NAME}不会立即释放空间；本应用不会清空{TRASH_NAME}。")
        };
        let confirm_label = if permanent && directory {
            "彻底删除整个文件夹（不可撤销）".to_string()
        } else if permanent {
            "彻底删除（不可撤销）".to_string()
        } else {
            format!("移到{TRASH_NAME}")
        };
        self.session.select(id);
        self.pending_deletion = Some((self.generation, request));
        self.session.status = "等待删除确认；尚未删除任何项目。".into();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel_view = view.clone();
            let confirm_view = view.clone();
            let close_view = view.clone();
            dialog
                .title(title.clone())
                .w(px(600.0))
                .centered()
                // Keep unrelated text at the same brightness. The dialog's
                // full-window input shield and focus trap remain active even
                // without a painted backdrop; only the dim/slide fade is gone.
                .entrance(gpui_kit::component::dialog::DialogEntrance::None)
                .overlay(false)
                .overlay_closable(false)
                // Enter must not silently choose the destructive action.
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = close_view.update(cx, |this, cx| {
                        if this.pending_deletion.take().is_some() {
                            this.session.status = "已取消删除；没有删除任何项目。".into();
                        }
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .text_sm()
                        .child(
                            div()
                                .id("delete-warning")
                                .test_support()
                                .child(description.clone()),
                        )
                        .child(
                            div()
                                .id("delete-path")
                                .test_support()
                                .max_h(px(160.0))
                                .overflow_y_scroll()
                                .p_3()
                                .rounded_lg()
                                .bg(rgb(0x192632))
                                .child(path.clone()),
                        )
                        .child(detail.clone())
                        .child(div().text_color(rgb(AMBER)).child(warning.clone())),
                )
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("delete-cancel").label("取消").on_click(
                            move |_, window, cx| {
                                let _ = cancel_view.update(cx, |this, cx| {
                                    if this.pending_deletion.take().is_some() {
                                        this.session.status =
                                            "已取消删除；没有删除任何项目。".into();
                                    }
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            },
                        ))
                        .child(
                            Button::new("delete-confirm")
                                .danger()
                                .label(confirm_label.clone())
                                .on_click(move |_, window, cx| {
                                    let _ = confirm_view.update(cx, |this, cx| {
                                        this.confirm_delete(window, cx);
                                    });
                                }),
                        ),
                )
        });
        cx.notify();
    }

    /// Offscreen preview only. Even if a test clicks Confirm, no filesystem
    /// mutation is permitted through this preview's replacement backend.
    #[cfg(feature = "ui-tests")]
    #[allow(dead_code)]
    pub fn preview_delete_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_operation = |_| Err("界面预览不会执行删除。".into());
        let id = self.session.result.as_ref().and_then(|scan| {
            scan.nodes
                .iter()
                .find(|node| node.id != scan.root && node.kind == NodeKind::Directory)
                .map(|node| node.id)
        });
        if let Some(id) = id {
            self.request_delete(id, window, cx);
        }
    }

    /// Synthetic file preview: this backend cannot delete anything.
    #[cfg(feature = "ui-tests")]
    #[allow(dead_code)]
    pub fn preview_file_deletion(
        &mut self,
        mode: Option<DeletionMode>,
        kind: NodeKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_operation = |_| Err("界面预览不会执行删除。".into());
        if let Some(id) = self.session.result.as_ref().and_then(|scan| {
            scan.nodes
                .iter()
                .find(|node| node.id != scan.root && node.kind == kind)
                .map(|node| node.id)
        }) {
            self.session.select(id);
            self.session.expand_one_level();
            self.reset_projection();
            if let Some(mode) = mode {
                self.request_delete_mode(id, mode, window, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((generation, request)) = self.pending_deletion.take() else {
            return;
        };
        window.close_dialog(cx);
        if generation != self.generation || self.busy() || self.choosing {
            self.session.status = "扫描结果已变化，请重新选择要删除的项目。".into();
            cx.notify();
            return;
        }
        let mode = request.mode();
        let operation = self.delete_operation;
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("file-deletion".into())
            .spawn(move || {
                let _ = sender.send(operation(request));
            }) {
            Ok(_) => {
                self.deletion = Some(receiver);
                self.deletion_mode = Some(mode);
                self.deletion_notice = None;
                self.session.status = "正在处理删除…请勿移动或重命名此项目。".into();
            }
            Err(error) => self.session.status = format!("无法启动删除操作：{error}"),
        }
        cx.notify();
    }

    pub(super) fn poll_deletion(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let result = match self.deletion.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(result)) => result,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err("删除线程意外结束。请在系统文件管理器中确认项目状态；不会自动重试。".into())
            }
            _ => return,
        };
        self.deletion = None;
        let mode = self.deletion_mode.take().unwrap_or(DeletionMode::Trash);
        self.deletion_notice = Some(match result {
            Ok(()) => match mode {
                DeletionMode::Trash => format!("已移到{TRASH_NAME}，可在系统{TRASH_NAME}中恢复"),
                DeletionMode::Permanent => "已彻底删除所选项目；此操作不可撤销".into(),
            },
            Err(error) => format!("未能确认删除完成：{error} 不会自动重试。"),
        });
        // All outcomes can have changed disk state, including partial Trash or
        // permanent failures. Revalidate accounting read-only in the background,
        // never clear the displayed tree or reuse a stale subtraction estimate.
        self.session.reconcile();
        cx.notify();
    }
}
