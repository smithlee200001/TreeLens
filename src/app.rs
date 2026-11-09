//! Native GPUI Kit presentation. Scanning and virtual indexing live in `session`.
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, PopupMenuItem},
    spinner::Spinner,
    tooltip::Tooltip,
    Disableable, Icon, IconName, Sizable, Theme, TitleBar, WindowExt,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{ops::Range, path::PathBuf, sync::mpsc, time::Duration};
use tree_lens::{
    deletion::DeletionRequest,
    footprint::{self, Tile},
    format_bytes,
    platform::{self, DELETE_LABEL, TRASH_NAME, TRASH_SUPPORTED},
    scanner::{NodeId, NodeKind},
    session::{expand_home, Session},
    tree::SortBy,
};

const BG: u32 = 0x0b1017;
const PANEL: u32 = 0x111923;
const BORDER: u32 = 0x243241;
const TEXT: u32 = 0xe8f1f8;
const MUTED: u32 = 0x8fa3b8;
const CYAN: u32 = 0x69e0d3;
const LIME: u32 = 0xc4ed9c;
const AMBER: u32 = 0xf6c77b;
const PALETTE: [(u32, u32); 8] = [
    (0x194c4b, 0x69e0d3),
    (0x253f5b, 0x8dbdfb),
    (0x43375c, 0xc4a4f7),
    (0x485337, 0xc4ed9c),
    (0x584332, 0xf6c77b),
    (0x50364b, 0xeab3d6),
    (0x244b56, 0x8fdae9),
    (0x3d475a, 0xc4d1e4),
];

gpui_kit::actions!(
    tree_lens,
    [OpenFolder, Refresh, FocusSearch, CancelScan, Quit]
);

pub fn install_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.font_size = px(14.0);
        theme.radius = px(9.0);
        theme.colors.background = rgb(BG).into();
        theme.colors.foreground = rgb(TEXT).into();
        theme.colors.border = rgb(BORDER).into();
        theme.colors.input = rgb(0x0c141e).into();
        theme.colors.primary = rgb(CYAN).into();
        theme.colors.button_primary = rgb(CYAN).into();
        theme.colors.button_primary_hover = rgb(0x91eee3).into();
        theme.colors.button_primary_active = rgb(0x4cc3b7).into();
        theme.colors.button_primary_foreground = rgb(0x092322).into();
        theme.colors.button = rgb(0x1a2634).into();
        theme.colors.button_hover = rgb(0x26394a).into();
        theme.colors.button_foreground = rgb(TEXT).into();
        theme.colors.accent = rgb(0x213b40).into();
        theme.colors.accent_foreground = rgb(CYAN).into();
    });
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-o", OpenFolder, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-r", Refresh, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FocusSearch, None),
        KeyBinding::new("escape", CancelScan, Some("TreeLens")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", Quit, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-o", OpenFolder, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-r", Refresh, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FocusSearch, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

pub struct TreeLensApp {
    session: Session,
    path_input: Entity<InputState>,
    search_input: Entity<InputState>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    tiles: Vec<Tile>,
    map_parent: Option<NodeId>,
    generation: u64,
    choosing: bool,
    _subscriptions: Vec<Subscription>,
    _ticker: Task<()>,
    picker: Option<Task<()>>,
    pending_deletion: Option<(u64, DeletionRequest)>,
    deletion: Option<mpsc::Receiver<Result<(), String>>>,
    deletion_notice: Option<String>,
    trash_operation: fn(DeletionRequest) -> Result<(), String>,
}

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
                    this.poll_deletion(window, cx);
                    let was_busy = this.session.busy();
                    let search_pending = this.session.query != this.session.applied_query();
                    if this.session.poll() {
                        if was_busy && !this.session.busy() {
                            this.reset_projection();
                            this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        }
                        if search_pending {
                            this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        }
                        cx.notify();
                    }
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
            deletion: None,
            deletion_notice: None,
            trash_operation: DeletionRequest::execute,
        };
        if let Some(path) = initial {
            app.start(path, window, cx);
        }
        app
    }

    fn start(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.pending_deletion.is_some() {
            return;
        }
        self.pending_deletion = None;
        self.deletion_notice = None;
        let _ = self.session.start(path.clone());
        self.generation = self.generation.wrapping_add(1);
        self.path_input.update(cx, |input, cx| {
            input.set_value(path.display().to_string(), window, cx)
        });
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.tiles.clear();
        self.map_parent = None;
        cx.notify();
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn reset_projection(&mut self) {
        self.map_parent = self.session.result.as_ref().map(|scan| scan.root);
        self.tiles = self
            .session
            .result
            .as_ref()
            .map(|scan| footprint::project(scan, scan.root))
            .unwrap_or_default();
    }

    fn select_node(&mut self, id: NodeId, cx: &mut Context<Self>) {
        self.session.select(id);
        if let Some(scan) = &self.session.result {
            if scan.nodes[id].kind == NodeKind::Directory && self.map_parent != Some(id) {
                self.map_parent = Some(id);
                self.tiles = footprint::project(scan, id);
            }
        }
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if let Some(path) = self.session.root_path.clone() {
            self.start(path, window, cx);
        }
    }

    fn copy_path(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if let Some(scan) = &self.session.result {
            cx.write_to_clipboard(ClipboardItem::new_string(
                scan.path_for(id).display().to_string(),
            ));
            self.session.status = "已复制完整路径".into();
            cx.notify();
        }
    }

    fn reveal(&mut self, id: NodeId, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(scan) = &self.session.result {
            if let Err(error) = std::process::Command::new("/usr/bin/open")
                .arg("-R")
                .arg(scan.path_for(id))
                .spawn()
            {
                self.session.status = format!("无法打开 Finder：{error}");
            }
        }
        #[cfg(windows)]
        if let Some(scan) = &self.session.result {
            // GPUI uses the Windows shell's PIDL API, not shell command strings.
            let path = scan.path_for(id);
            if path.parent().is_none() {
                // Drive/UNC-share roots have no parent item to select.
                cx.open_with_system(&path);
            } else {
                cx.reveal_path(&path);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(scan) = &self.session.result {
            // GPUI opens the given path before invoking the desktop portal.
            // Pass only a directory: opening a FIFO/device/file here is unsafe,
            // and dangling links or unreadable files should still reveal a parent.
            let path = scan.path_for(id);
            let directory = if scan.nodes[id].kind == NodeKind::Directory {
                path.as_path()
            } else {
                path.parent().unwrap_or(&path)
            };
            cx.reveal_path(directory);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = id;
            self.session.status = "此平台尚不支持原生文件管理器显示。".into();
        }
        cx.notify();
    }

    fn busy(&self) -> bool {
        self.session.busy() || self.deletion.is_some()
    }

    fn deletable_folder(&self, id: NodeId) -> bool {
        !self.busy()
            && !self.choosing
            && self.session.result.as_ref().is_some_and(|scan| {
                id != scan.root
                    && scan.nodes.get(id).is_some_and(|node| {
                        node.kind == NodeKind::Directory && node.error.is_none()
                    })
            })
    }

    fn request_delete(&mut self, id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.deletable_folder(id) || window.has_active_dialog(cx) {
            return;
        }
        let scan = self.session.result.as_ref().expect("deletable scan");
        let request = match DeletionRequest::prepare(scan, id) {
            Ok(request) => request,
            Err(error) => {
                self.deletion_notice = None;
                self.session.status = format!("无法删除文件夹：{error}");
                cx.notify();
                return;
            }
        };
        let path = request.path().display().to_string();
        let size = format_bytes(scan.nodes[id].total_allocated_bytes);
        self.session.select(id);
        self.pending_deletion = Some((self.generation, request));
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel_view = view.clone();
            let confirm_view = view.clone();
            let close_view = view.clone();
            dialog
                .title("删除文件夹？")
                .w(px(600.0))
                .centered()
                .overlay_closable(false)
                // Enter must not silently choose the destructive action.
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = close_view.update(cx, |this, cx| {
                        this.pending_deletion = None;
                        cx.notify();
                    });
                })
                .child(
                    div().flex().flex_col().gap_3().text_sm()
                        .child(format!("这个文件夹及其全部内容将移到{TRASH_NAME}，不会永久删除。"))
                        .child(div().id("delete-path").test_support().max_h(px(160.0))
                            .overflow_y_scroll().p_3().rounded_lg().bg(rgb(0x192632))
                            .child(path.clone()))
                        .child(format!("扫描时的磁盘占用：{size}。包含所有内容，即使被筛选或未完整扫描。"))
                        .child(div().text_color(rgb(AMBER)).child(
                            format!("可在系统{TRASH_NAME}中恢复。移入{TRASH_NAME}不会立即释放空间；本应用不会清空{TRASH_NAME}。"))),
                )
                .footer(
                    div().flex().justify_end().gap_2()
                        .child(Button::new("delete-cancel").label("取消")
                            .on_click(move |_, window, cx| {
                                let _ = cancel_view.update(cx, |this, cx| {
                                    this.pending_deletion = None;
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            }))
                        .child(Button::new("delete-confirm").danger().label(format!("移到{TRASH_NAME}"))
                            .on_click(move |_, window, cx| {
                                let _ = confirm_view.update(cx, |this, cx| {
                                    this.confirm_delete(window, cx);
                                });
                            })),
                )
        });
        cx.notify();
    }

    /// Offscreen preview only. Even if a test clicks Confirm, no filesystem
    /// mutation is permitted through this preview's replacement backend.
    #[cfg(feature = "ui-tests")]
    #[allow(dead_code)]
    pub(crate) fn preview_delete_confirmation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trash_operation = |_| Err("界面预览不会执行删除。".into());
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

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((generation, request)) = self.pending_deletion.take() else {
            return;
        };
        window.close_dialog(cx);
        if generation != self.generation || self.busy() || self.choosing {
            self.session.status = "扫描结果已变化，请重新选择要删除的文件夹。".into();
            cx.notify();
            return;
        }
        let operation = self.trash_operation;
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("folder-trash".into())
            .spawn(move || {
                let _ = sender.send(operation(request));
            }) {
            Ok(_) => {
                self.deletion = Some(receiver);
                self.deletion_notice = None;
                self.session.status = format!("正在移到{TRASH_NAME}…请勿移动或重命名此文件夹。");
            }
            Err(error) => self.session.status = format!("无法启动删除操作：{error}"),
        }
        cx.notify();
    }

    fn poll_deletion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = match self.deletion.as_ref().map(|receiver| receiver.try_recv()) {
            Some(Ok(result)) => result,
            Some(Err(mpsc::TryRecvError::Disconnected)) => Err(
                "删除线程意外结束。请在系统文件管理器中确认文件夹的位置，再刷新；不会自动重试。"
                    .into(),
            ),
            _ => return,
        };
        self.deletion = None;
        match result {
            Ok(()) => {
                // Rebuild all accounting, including hard-link attribution. Never subtract
                // stale subtree totals or leave a selected ID from the previous scan.
                if let Some(root) = self.session.root_path.clone() {
                    self.start(root, window, cx);
                }
                self.deletion_notice =
                    Some(format!("已移到{TRASH_NAME}，可在系统{TRASH_NAME}中恢复"));
            }
            Err(error) => {
                self.deletion_notice = None;
                self.session.status = format!("未能确认移到{TRASH_NAME}：{error} 请检查后刷新。");
            }
        }
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy() || self.choosing;
        let mut nav = div()
            .flex()
            .flex_col()
            .w(px(170.0))
            .flex_shrink_0()
            .p_4()
            .gap_3()
            .bg(rgb(0x0c131c))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .mb_2()
                    .child("WORKSPACE"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0x173138))
                    .text_color(rgb(CYAN))
                    .child(Icon::new(IconName::Folder).size_4())
                    .child("空间总览"),
            )
            .child(
                div()
                    .mt_5()
                    .mb_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("快速扫描"),
            );
        for (index, (label, suffix)) in [
            ("主文件夹", ""),
            ("下载", "Downloads"),
            ("桌面", "Desktop"),
            ("文稿", "Documents"),
        ]
        .into_iter()
        .enumerate()
        {
            nav = nav.child(
                Button::new(("shortcut", index))
                    .ghost()
                    .label(label)
                    .icon(IconName::Folder)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(path) = platform::shortcut_path(suffix) {
                            this.start(path, window, cx);
                        } else {
                            this.session.status =
                                "无法定位此系统文件夹，请使用文件夹选择器。".into();
                            cx.notify();
                        }
                    })),
            );
        }
        nav.child(div().flex_1())
            .child(
                div()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(
                        div()
                            .text_color(rgb(LIME))
                            .mb_2()
                            .child("本地运行 · 删除需确认"),
                    )
                    .child("不读取文件内容，不上传扫描结果。"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x63778c))
                    .child("GPUI KIT / 0.4.0"),
            )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(26.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child("让空间，看得见。"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child("从文件夹到每个字节，找到真正占用空间的地方。"),
                            ),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(0x354b40))
                            .text_xs()
                            .text_color(rgb(LIME))
                            .child("●  LOCAL / TRASH"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("open-folder")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label("选择文件夹")
                            .disabled(busy || self.choosing)
                            .tooltip(if cfg!(target_os = "macos") {
                                "选择文件夹 · ⌘O"
                            } else {
                                "选择文件夹 · Ctrl+O"
                            })
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.path_input)
                                .disabled(busy)
                                .aria_label("扫描文件夹路径"),
                        ),
                    )
                    .child(
                        Button::new("scan-path")
                            .label("扫描")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let path = this.path_input.read(cx).value().to_string();
                                if !path.trim().is_empty() {
                                    this.start(expand_home(&path), window, cx);
                                }
                            })),
                    )
                    .child(if self.session.busy() {
                        Button::new("cancel")
                            .label(if self.session.cancelling() {
                                "取消中…"
                            } else {
                                "取消"
                            })
                            .disabled(self.session.cancelling())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.cancel();
                                cx.notify();
                            }))
                    } else {
                        Button::new("refresh")
                            .icon(IconName::RotateCw)
                            .tooltip(if cfg!(target_os = "macos") {
                                "重新扫描 · ⌘R"
                            } else {
                                "重新扫描 · Ctrl+R"
                            })
                            .disabled(busy || self.session.root_path.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx)))
                    }),
            )
    }

    fn summaries(&self) -> Div {
        let (allocated, logical, files, folders) = self.session.summary();
        let labels = [
            (
                if self.session.partial() {
                    "已统计磁盘占用"
                } else {
                    "磁盘占用"
                },
                format_bytes(allocated),
                "ALLOCATED",
                CYAN,
            ),
            ("逻辑大小", format_bytes(logical), "LOGICAL SIZE", 0x9dc5ff),
            ("文件", format_count(files), "FILES INDEXED", 0xc4a4f7),
            ("子文件夹", format_count(folders), "DIRECTORIES", LIME),
        ];
        div()
            .flex()
            .gap_3()
            .children(labels.into_iter().map(|(label, value, detail, color)| {
                div()
                    .flex_1()
                    .min_w_0()
                    .p_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(linear_gradient(
                        140.0,
                        linear_color_stop(rgb(0x172430), 0.0),
                        linear_color_stop(rgb(PANEL), 1.0),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_sm().text_color(rgb(MUTED)).child(label))
                            .child(div().size(px(6.0)).rounded_full().bg(rgb(color))),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(26.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(color))
                            .child(value),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(9.0))
                            .text_color(rgb(0x6f879e))
                            .child(detail),
                    )
            }))
    }

    fn footprint(&self, cx: &mut Context<Self>) -> Div {
        let parent = self.map_parent.and_then(|id| {
            self.session
                .result
                .as_ref()
                .map(|scan| (id, scan.nodes[id].display_name().into_owned()))
        });
        let mut chart = div()
            .relative()
            .w_full()
            .h(px(124.0))
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(0x0b131d));
        for (index, tile) in self.tiles.iter().enumerate() {
            let [x, y, w, h] = tile.rect;
            let (bg, fg) = PALETTE[index % PALETTE.len()];
            let id = tile.node;
            let tooltip = format!("{} · {}", tile.label, format_bytes(tile.bytes));
            chart = chart.child(
                div()
                    .id(("tile", index))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .absolute()
                    .left(relative(x))
                    .top(relative(y))
                    .w(relative(w))
                    .h(relative(h))
                    .p(px(2.0))
                    .child(
                        div()
                            .size_full()
                            .overflow_hidden()
                            .rounded_lg()
                            .p(px(if h < 0.35 { 6.0 } else { 12.0 }))
                            .border_1()
                            .border_color(rgb(fg).opacity(0.22))
                            .bg(linear_gradient(
                                135.0,
                                linear_color_stop(rgb(bg), 0.0),
                                linear_color_stop(rgb(bg).opacity(0.65), 1.0),
                            ))
                            .hover(|style| style.border_color(rgb(fg)).bg(rgb(bg)))
                            .when(id.is_some(), |el| el.cursor_pointer())
                            .when(h > 0.18 && w > 0.04, |el| {
                                el.child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(fg))
                                        .child(tile.label.clone()),
                                )
                            })
                            .when(h > 0.3 && w > 0.08, |el| {
                                el.child(
                                    div()
                                        .mt_1()
                                        .text_xs()
                                        .text_color(rgb(TEXT))
                                        .child(format_bytes(tile.bytes)),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(id) = id {
                            this.select_node(id, cx);
                        }
                    })),
            );
        }
        if self.tiles.is_empty() {
            chart = chart.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(if self.session.busy() {
                        "正在绘制你的空间地图…"
                    } else if self.session.result.is_some() {
                        "此目录暂无已分配空间"
                    } else {
                        "扫描后，这里将显示真实的空间分布"
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("空间地图"))
                            .child(
                                div().text_xs().text_color(rgb(MUTED)).child(
                                    parent
                                        .as_ref()
                                        .map(|(_, name)| name.clone())
                                        .unwrap_or_else(|| "等待扫描".into()),
                                ),
                            ),
                    )
                    .child(
                        Button::new("map-root")
                            .ghost()
                            .xsmall()
                            .label("返回根目录")
                            .disabled(self.session.result.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reset_projection();
                                cx.notify();
                            })),
                    ),
            )
            .child(chart)
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child("面积 ∝ 已分配空间 · 点击目录深入查看 · 最多显示 11 项，其余合并"),
            )
    }

    fn tree_panel(&self, cx: &mut Context<Self>) -> Div {
        let filtered = !self.session.query.trim().is_empty();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .rounded_xl()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("文件树"))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{} 行",
                                format_count(self.session.rows.len() as u64)
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div().w(px(190.0)).child(
                                    Input::new(&self.search_input)
                                        .small()
                                        .cleanable(true)
                                        .prefix(Icon::new(IconName::Search).size_4())
                                        .aria_label("按文件名筛选"),
                                ),
                            )
                            .child(
                                Button::new("collapse")
                                    .ghost()
                                    .small()
                                    .label("收起")
                                    .disabled(filtered)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.collapse_all();
                                        this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("expand")
                                    .ghost()
                                    .small()
                                    .label("展开一级")
                                    .disabled(filtered)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.expand_one_level();
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(self.tree_header(cx))
            .child(if self.session.result.is_some() {
                if self.session.rows.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(MUTED))
                        .child("没有匹配的名称，试试其他关键词。")
                        .into_any_element()
                } else {
                    uniform_list(
                        "file-tree",
                        self.session.rows.len(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            let rows = this
                                .session
                                .result
                                .as_ref()
                                .map(|scan| this.session.rows.rows(scan, range))
                                .unwrap_or_default();
                            rows.into_iter()
                                .map(|(id, depth)| this.tree_row(id, depth, cx))
                                .collect()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .into_any_element()
                }
            } else {
                self.empty_state(cx).into_any_element()
            })
    }

    fn tree_header(&self, cx: &mut Context<Self>) -> Div {
        let sort = self.session.sort;
        div()
            .flex()
            .items_center()
            .h(px(34.0))
            .px_3()
            .gap_2()
            .bg(rgb(0x0e1620))
            .border_y_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .child(sort_button("名称", SortBy::Name, sort, "sort-name", cx)),
            )
            .child(div().w(px(110.0)).flex().items_center().child(sort_button(
                "磁盘占用",
                SortBy::Allocated,
                sort,
                "sort-allocated",
                cx,
            )))
            .child(div().w(px(105.0)).flex().items_center().child(sort_button(
                "逻辑大小",
                SortBy::Logical,
                sort,
                "sort-logical",
                cx,
            )))
            .child(
                div()
                    .w(px(128.0))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("占父级"),
            )
            .child(div().w(px(62.0)).flex().items_center().child(sort_button(
                "文件数",
                SortBy::Files,
                sort,
                "sort-files",
                cx,
            )))
    }

    fn tree_row(&self, id: NodeId, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let scan = self
            .session
            .result
            .as_ref()
            .expect("only render rows for a scan");
        let node = &scan.nodes[id];
        let directory = node.kind == NodeKind::Directory;
        let open =
            self.session.expanded.contains(&id) || !self.session.applied_query().trim().is_empty();
        let parent_bytes = node
            .parent
            .map(|p| scan.nodes[p].total_allocated_bytes)
            .unwrap_or(node.total_allocated_bytes);
        let fraction = if parent_bytes == 0 {
            0.0
        } else {
            (node.total_allocated_bytes as f64 / parent_bytes as f64).clamp(0.0, 1.0) as f32
        };
        let selected = self.session.selected == Some(id);
        let view = cx.entity().downgrade();
        let generation = self.generation;
        let delete_enabled = TRASH_SUPPORTED && self.deletable_folder(id);
        let icon = match node.kind {
            NodeKind::Directory => IconName::Folder,
            NodeKind::Symlink => IconName::ExternalLink,
            _ => IconName::File,
        };
        div()
            .id(("row", id))
            .w_full()
            .test_support()
            .role(Role::TreeItem)
            .aria_label(node.display_name().into_owned())
            .aria_selected(selected)
            .aria_level(depth + 1)
            .when(directory, |el| el.aria_expanded(open))
            .flex()
            .items_center()
            .h(px(40.0))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(rgb(0x182430))
            .bg(rgb(if selected { 0x18353a } else { PANEL }))
            .hover(|style| style.bg(rgb(0x1b2a37)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.select_node(id, cx);
                if event.click_count() == 2 {
                    this.session.toggle(id);
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl(px(depth.min(18) as f32 * 14.0))
                    .child(div().id(("chevron", id)).w(px(16.0)).flex_shrink_0().when(
                        directory && !node.children.is_empty(),
                        |el| {
                            el.child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size_3()
                                .text_color(rgb(MUTED)),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.session.toggle(id);
                                    cx.notify();
                                },
                            ))
                        },
                    ))
                    .child(Icon::new(icon).size_4().text_color(rgb(if directory {
                        CYAN
                    } else {
                        0x829bb4
                    })))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(rgb(if node.issue_count > 0 { AMBER } else { TEXT }))
                            .child(format!(
                                "{}{}{}",
                                node.display_name(),
                                if node.hard_link_duplicate {
                                    " · 硬链接"
                                } else {
                                    ""
                                },
                                if node.issue_count > 0 { " ⚠" } else { "" }
                            )),
                    ),
            )
            .child(
                div()
                    .w(px(110.0))
                    .text_sm()
                    .text_color(rgb(CYAN))
                    .child(format_bytes(node.total_allocated_bytes)),
            )
            .child(
                div()
                    .w(px(105.0))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format_bytes(node.total_logical_bytes)),
            )
            .child(
                div()
                    .w(px(128.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(74.0))
                            .h(px(5.0))
                            .rounded_full()
                            .bg(rgb(0x253540))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(fraction))
                                    .rounded_full()
                                    .bg(rgb(if selected { LIME } else { CYAN })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(MUTED))
                            .child(format!("{:.0}%", fraction * 100.0)),
                    ),
            )
            .child(
                div()
                    .w(px(62.0))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format_count(node.file_count)),
            )
            .context_menu(move |menu, _, _| {
                let copy_view = view.clone();
                let reveal_view = view.clone();
                let delete_view = view.clone();
                menu.item(
                    PopupMenuItem::new("复制完整路径").on_click(move |_, _, cx| {
                        let _ = copy_view.update(cx, |this, cx| {
                            if this.generation == generation {
                                this.copy_path(id, cx);
                            }
                        });
                    }),
                )
                .when(TRASH_SUPPORTED, |menu| {
                    menu.item(PopupMenuItem::new(platform::REVEAL_LABEL).on_click(
                        move |_, _, cx| {
                            let _ = reveal_view.update(cx, |this, cx| {
                                if this.generation == generation {
                                    this.reveal(id, cx);
                                }
                            });
                        },
                    ))
                })
                .when(directory, |menu| {
                    menu.separator().item(
                        PopupMenuItem::new(DELETE_LABEL)
                            .disabled(!delete_enabled)
                            .on_click(move |_, window, cx| {
                                let _ = delete_view.update(cx, |this, cx| {
                                    if this.generation == generation {
                                        this.request_delete(id, window, cx);
                                    }
                                });
                            }),
                    )
                })
            })
            .into_any_element()
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.session.busy();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_5()
            .child(
                div()
                    .size(px(56.0))
                    .rounded_xl()
                    .bg(rgb(0x173a3b))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if busy {
                        Spinner::new().color(rgb(CYAN).into()).into_any_element()
                    } else {
                        Icon::new(IconName::FolderOpen)
                            .size_8()
                            .text_color(rgb(CYAN))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if busy {
                        "正在发现你的空间"
                    } else {
                        "每个文件，都有自己的位置"
                    }),
            )
            .child(div().text_sm().text_color(rgb(MUTED)).child(if busy {
                "扫描在后台进行，可随时取消。"
            } else {
                "选择文件夹，或将它拖入窗口开始分析。"
            }))
            .when(!busy, |el| {
                el.child(
                    Button::new("empty-open")
                        .primary()
                        .label("开始探索")
                        .disabled(self.choosing)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                        ),
                )
            })
    }

    fn inspector(&self, cx: &mut Context<Self>) -> Div {
        let mut panel = div()
            .w(px(220.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child("INSPECTOR / 项目详情"),
            );
        if let Some((scan, id)) = self.session.result.as_ref().zip(self.session.selected) {
            let node = &scan.nodes[id];
            panel = panel
                .child(
                    Icon::new(if node.kind == NodeKind::Directory {
                        IconName::FolderOpen
                    } else {
                        IconName::File
                    })
                    .size_8()
                    .text_color(rgb(CYAN)),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(node.display_name().into_owned()),
                )
                .child(
                    div()
                        .text_size(px(28.0))
                        .text_color(rgb(CYAN))
                        .child(format_bytes(node.total_allocated_bytes)),
                )
                .child(div().text_xs().text_color(rgb(MUTED)).child("已分配空间"))
                .child(detail("逻辑大小", format_bytes(node.total_logical_bytes)))
                .child(detail("文件", format_count(node.file_count)))
                .child(detail(
                    "子文件夹",
                    format_count(
                        node.directory_count
                            .saturating_sub(u64::from(node.kind == NodeKind::Directory)),
                    ),
                ))
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("完整路径"),
                )
                .child(
                    div()
                        .text_xs()
                        .overflow_hidden()
                        .child(scan.path_for(id).display().to_string()),
                )
                .when(node.hard_link_duplicate, |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(AMBER))
                            .child("硬链接：已在其他路径计入分配空间。"),
                    )
                })
                .when(node.issue_count > 0, |panel| {
                    panel.child(div().text_xs().text_color(rgb(AMBER)).child(format!(
                        "包含 {} 处读取问题，统计可能偏小。",
                        node.issue_count
                    )))
                })
                .child(
                    Button::new("copy-selected")
                        .label("复制路径")
                        .icon(IconName::Copy)
                        .on_click(cx.listener(move |this, _, _, cx| this.copy_path(id, cx))),
                )
                .when(TRASH_SUPPORTED, |panel| {
                    panel.child(
                        Button::new("reveal-selected")
                            .label(platform::REVEAL_LABEL)
                            .icon(IconName::ExternalLink)
                            .on_click(cx.listener(move |this, _, _, cx| this.reveal(id, cx))),
                    )
                })
                .when(node.kind == NodeKind::Directory, |panel| {
                    panel.child(
                        Button::new("delete-selected")
                            .danger()
                            .label("删除文件夹…")
                            .disabled(!TRASH_SUPPORTED || !self.deletable_folder(id))
                            .tooltip(if id == scan.root {
                                "不能删除当前扫描根目录；请先扫描它的父文件夹。"
                            } else if !TRASH_SUPPORTED {
                                "此平台尚不支持移到系统回收站/废纸篓。"
                            } else {
                                "确认后将整个文件夹移到系统回收站/废纸篓，不永久删除。"
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request_delete(id, window, cx);
                            })),
                    )
                });
        } else {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("选择文件树中的项目，在这里查看详细信息。"),
            );
        }
        panel.child(div().flex_1()).child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child(platform::STORAGE_CAVEAT),
        )
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> Div {
        let status = if self.deletion.is_some() {
            format!("正在移到{TRASH_NAME}…请勿移动或重命名此文件夹。")
        } else if self.session.busy() {
            format!(
                "已检查 {} 个项目 · {}",
                self.session.progress.as_ref().map_or(0, |p| p.entries),
                if self.session.cancelling() {
                    "正在取消"
                } else {
                    "后台扫描中"
                }
            )
        } else {
            self.session.status.clone()
        };
        let status = self
            .deletion_notice
            .as_ref()
            .map_or_else(|| status.clone(), |notice| format!("{notice} · {status}"));
        let issue_count = self
            .session
            .result
            .as_ref()
            .map_or(0, |s| s.nodes[s.root].issue_count);
        div()
            .flex()
            .items_center()
            .gap_3()
            .h(px(36.0))
            .px_4()
            .border_t_1()
            .border_color(rgb(BORDER))
            .bg(rgb(0x0c131c))
            .child(if self.busy() {
                Spinner::new()
                    .xsmall()
                    .color(rgb(CYAN).into())
                    .into_any_element()
            } else {
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(rgb(if self.session.partial() { AMBER } else { CYAN }))
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(if self.session.partial() { AMBER } else { MUTED }))
                    .child(status),
            )
            .when(issue_count > 0, |el| {
                el.child(
                    Button::new("issues")
                        .disabled(self.deletion.is_some())
                        .ghost()
                        .xsmall()
                        .label(format!("{issue_count} 处问题"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.show_dialog(true, window, cx)),
                        ),
                )
            })
            .child(
                Button::new("about")
                    .disabled(self.deletion.is_some())
                    .ghost()
                    .xsmall()
                    .label("统计说明")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.show_dialog(false, window, cx)),
                    ),
            )
    }

    fn show_dialog(&self, issues: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.deletion.is_some() || window.has_active_dialog(cx) {
            return;
        }
        let records = if issues {
            self.session
                .result
                .as_ref()
                .map(|s| s.issues.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let count = self
            .session
            .result
            .as_ref()
            .map_or(0, |s| s.nodes[s.root].issue_count);
        window.open_dialog(cx, move |dialog, _, _| {
            let mut content = div().id("dialog-scroll").max_h(px(400.0)).overflow_y_scroll().flex().flex_col().gap_3().text_sm().text_color(rgb(MUTED));
            if issues {
                content = content.child("无法访问的内容没有纳入完整统计。应用不会提权或修改权限。")
                    .child(format!("共 {count} 处问题，最多保存 500 条详情。"));
                for issue in &records {
                    content = content.child(div().p_3().rounded_lg().bg(rgb(0x192632))
                        .child(div().text_color(rgb(AMBER)).child(issue.path.display().to_string()))
                        .child(issue.message.clone()));
                }
            } else {
                for line in [
                    "Tree Lens 0.4.0 · 原生 Rust / GPUI Kit 0.9",
                    "扫描只读取文件系统元数据，不读取文件内容，不上传结果。删除文件夹需单独确认，仅移到系统回收站/废纸篓。",
                    platform::ALLOCATION_HELP,
                    "不跟随符号链接，不跨入不同设备的挂载点。扫描取消或发生权限错误时，结果只是已扫描部分。",
                    platform::STORAGE_CAVEAT,
                    platform::SHORTCUT_HELP,
                    "空间地图展示当前目录直接子项；虚拟文件树仅为当前视口构造行。",
                ] { content = content.child(line); }
            }
            dialog.title(if issues { "读取问题" } else { "关于与统计说明" }).w(px(640.0)).centered().child(content)
        });
    }
}

impl Render for TreeLensApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = window.viewport_size().width >= px(1340.0);
        let detail_visible = window.viewport_size().width >= px(1220.0);
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .key_context("TreeLens")
            .on_action(
                cx.listener(|this, _: &OpenFolder, window, cx| this.choose_folder(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if !window.has_active_dialog(cx) {
                    this.search_input
                        .update(cx, |input, cx| input.focus(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &CancelScan, window, cx| {
                if window.has_active_dialog(cx) {
                    this.pending_deletion = None;
                    window.close_dialog(cx);
                } else {
                    this.session.cancel();
                }
                cx.notify();
            }))
            .on_action(|_: &Quit, _, cx| cx.quit())
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.0.first() {
                    this.start(path.clone(), window, cx);
                }
            }))
            .child(
                TitleBar::new().bg(rgb(0x0c131c)).child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .size_full()
                        .child(div().text_color(rgb(CYAN)).child("◈"))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Tree Lens"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child("/ SPACE OBSERVATORY"),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(wide, |el| el.child(self.sidebar(cx)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .p_4()
                            .gap_3()
                            .child(self.toolbar(cx))
                            .child(self.summaries())
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_h_0()
                                    .gap_4()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .gap_4()
                                            .when(self.session.result.is_some(), |el| {
                                                el.child(self.footprint(cx))
                                            })
                                            .child(self.tree_panel(cx)),
                                    )
                                    .when(detail_visible, |el| el.child(self.inspector(cx))),
                            ),
                    ),
            )
            .child(self.status_bar(cx))
    }
}

fn detail(label: &'static str, value: String) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_2()
        .text_xs()
        .child(div().text_color(rgb(MUTED)).child(label))
        .child(value)
}

fn sort_button(
    label: &str,
    sort: SortBy,
    active: SortBy,
    id: &'static str,
    cx: &mut Context<TreeLensApp>,
) -> Button {
    Button::new(id)
        .justify_start()
        .px_0()
        .ghost()
        .xsmall()
        .label(format!(
            "{label}{}",
            if sort == active {
                if sort == SortBy::Name {
                    " ↑"
                } else {
                    " ↓"
                }
            } else {
                ""
            }
        ))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.session.sort_by(sort);
            cx.notify();
        }))
}

fn format_count(value: u64) -> String {
    let text = value.to_string();
    let mut result = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().enumerate() {
        if index > 0 && (text.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

#[cfg(all(test, feature = "ui-tests"))]
mod ui_tests {
    use super::{install_theme, TreeLensApp, DELETE_LABEL, TRASH_NAME, TRASH_SUPPORTED};
    use gpui_kit::component::WindowExt;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use gpui_kit::{
        px, size, AnyWindowHandle, AppContext, Bounds, Entity, TestAppContext, WindowBounds,
        WindowOptions,
    };
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    use tree_lens::{scanner::NodeKind, tree::SortBy};

    fn fixture(
        cx: &mut TestAppContext,
        width: f32,
        loaded: bool,
    ) -> (AnyWindowHandle, Entity<TreeLensApp>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_theme(cx);
        });
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(width), px(760.0)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        let initial =
                            loaded.then(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
                        let mut app = TreeLensApp::new(initial, window, cx);
                        if loaded {
                            let deadline = Instant::now() + Duration::from_secs(5);
                            while app.session.busy() && Instant::now() < deadline {
                                app.session.poll();
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            assert!(!app.session.busy());
                            assert!(app.session.result.is_some());
                            app.reset_projection();
                        }
                        app
                    })
                },
            )
        })
        .expect("open test app")
    }

    #[gpui_kit::test]
    fn empty_view_and_dialog_work_at_minimum_size(cx: &mut TestAppContext) {
        let (handle, _) = fixture(cx, 1080.0, false);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let start = window.find("empty-open");
            assert!(start.visible(), "start bounds: {:?}", start.bounds());
            assert!(start.bounds().size.width > px(0.0));
            assert!(start.bounds().bottom() < px(760.0));
            window.click("about", cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn platform_refresh_shortcut_and_escape_work(cx: &mut TestAppContext) {
        let (handle, view) = fixture(cx, 1080.0, true);
        cx.update_window(handle, |_, window, cx| {
            let generation = view.read(cx).generation;
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-r"
                } else {
                    "ctrl-r"
                },
                cx,
            );
            assert_eq!(view.read(cx).generation, generation.wrapping_add(1));
            window.press("escape", cx);
            assert!(view.read(cx).session.cancelling() || !view.read(cx).session.busy());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn loaded_view_sorts_and_keeps_input_state(cx: &mut TestAppContext) {
        let (handle, view) = fixture(cx, 1440.0, true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let rows = window.find_all_by_role(gpui_kit::Role::TreeItem);
            assert!(!rows.is_empty());
            assert!(rows[0].bounds().size.width > px(900.0));
            window.click("sort-name", cx);
            assert_eq!(view.read(cx).session.sort, SortBy::Name);
            window.click("expand", cx);
            window.click("collapse", cx);
            assert!(!view.read(cx).session.rows.is_empty());
            let field = window.find_by_label("按文件名筛选");
            assert!(field.bounds().size.width > px(0.0));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn hundred_thousand_nodes_only_render_a_viewport(cx: &mut TestAppContext) {
        let (handle, view) = fixture(cx, 1440.0, true);
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                let scan = view.session.result.as_mut().unwrap();
                let root = scan.root;
                let mut template = scan
                    .nodes
                    .iter()
                    .find(|node| node.kind == NodeKind::File)
                    .unwrap()
                    .clone();
                template.children.clear();
                template.parent = Some(root);
                template.name = std::ffi::OsString::from("sample.bin").into_boxed_os_str();
                scan.nodes[root].children.clear();
                for _ in 0..100_000 {
                    let id = scan.nodes.len();
                    let mut node = template.clone();
                    node.id = id;
                    scan.nodes.push(node);
                    scan.nodes[root].children.push(id);
                }
                view.session.collapse_all();
                view.reset_projection();
                cx.notify();
            })
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let drawn = window.find_all_by_role(gpui_kit::Role::TreeItem).len();
            assert!(drawn > 0 && drawn < 100, "rendered {drawn} rows");
            assert_eq!(view.read(cx).session.rows.len(), 100_001);
        })
        .unwrap();
    }

    struct TrashFixture(PathBuf);
    impl TrashFixture {
        fn new() -> Self {
            // Wall-clock resolution varies by OS. Parallel tests must never
            // share a directory, even when they observe the same timestamp.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "tree-lens-ui-trash-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            // Claim the unique root atomically rather than silently reusing it.
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir_all(root.join("scan/folder-to-trash")).unwrap();
            std::fs::write(root.join("scan/folder-to-trash/content.txt"), b"synthetic").unwrap();
            Self(root)
        }
    }
    impl Drop for TrashFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn trash_fixture(
        cx: &mut TestAppContext,
    ) -> (TrashFixture, AnyWindowHandle, Entity<TreeLensApp>, usize) {
        let temp = TrashFixture::new();
        let (handle, view) = fixture(cx, 1080.0, false);
        let id = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |app, cx| {
                    app.start(temp.0.join("scan"), window, cx);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while app.session.busy() && Instant::now() < deadline {
                        app.session.poll();
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert!(!app.session.busy());
                    app.reset_projection();
                    app.session
                        .result
                        .as_ref()
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|node| node.name.as_ref() == std::ffi::OsStr::new("folder-to-trash"))
                        .unwrap()
                        .id
                })
            })
            .unwrap();
        (temp, handle, view, id)
    }

    fn mock_trash(request: tree_lens::deletion::DeletionRequest) -> Result<(), String> {
        let path = request.path();
        assert_eq!(path.file_name().unwrap(), "folder-to-trash");
        let fixture = path.parent().unwrap().parent().unwrap();
        assert!(fixture
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tree-lens-ui-trash-"));
        // Only rename this test's synthetic directory. Never call the OS Trash.
        std::fs::rename(path, fixture.join("mock-trash")).map_err(|error| error.to_string())
    }

    #[gpui_kit::test]
    fn deletion_dialog_enter_cancel_escape_and_reopen_are_safe(cx: &mut TestAppContext) {
        let (temp, handle, view, id) = trash_fixture(cx);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.request_delete(id, window, cx));
            window.render_frame(cx);
            assert!(window.has_active_dialog(cx));
            assert!(window.find("delete-path").visible());
            assert!(window.find("delete-confirm").bounds().bottom() < px(760.0));
            window.press("enter", cx);
            assert!(window.has_active_dialog(cx));
            assert!(view.read(cx).deletion.is_none());
            // A drop/new scan while confirming must not change the request.
            view.update(cx, |app, cx| app.start(temp.0.clone(), window, cx));
            assert_eq!(
                view.read(cx).session.root_path.as_ref(),
                Some(&temp.0.join("scan"))
            );
            window.click("delete-cancel", cx);
            assert!(!window.has_active_dialog(cx));
            assert!(view.read(cx).pending_deletion.is_none());
            view.update(cx, |app, cx| app.request_delete(id, window, cx));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert!(view.read(cx).deletion.is_none());
            assert!(view.read(cx).pending_deletion.is_none());
            view.update(cx, |app, cx| app.request_delete(id, window, cx));
            // Windows also has a title-bar Close button with this local ID.
            window.render_frame(cx);
            window.within("dialog-close").click("close", cx);
            assert!(!window.has_active_dialog(cx));
            assert!(view.read(cx).pending_deletion.is_none());
            view.update(cx, |app, cx| app.request_delete(id, window, cx));
            window.click("delete-cancel", cx);
        })
        .unwrap();
        assert!(temp.0.join("scan/folder-to-trash/content.txt").is_file());
    }

    #[gpui_kit::test]
    fn deletion_confirmation_is_single_use_and_refreshes_selection(cx: &mut TestAppContext) {
        let (temp, handle, view, id) = trash_fixture(cx);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| {
                app.trash_operation = mock_trash;
                app.request_delete(id, window, cx);
            });
            window.click("delete-confirm", cx);
            view.update(cx, |app, cx| {
                assert!(app.pending_deletion.is_none());
                assert!(app.deletion.is_some());
                app.show_dialog(false, window, cx);
                assert!(!window.has_active_dialog(cx));
                app.confirm_delete(window, cx); // repeated dispatch must do nothing
                let generation = app.generation;
                app.start(temp.0.clone(), window, cx); // blocked during I/O
                assert_eq!(app.generation, generation);
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.busy() && Instant::now() < deadline {
                    app.poll_deletion(window, cx);
                    app.session.poll();
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(!app.busy());
                let scan = app.session.result.as_ref().unwrap();
                assert_eq!(app.session.selected, Some(scan.root));
                assert_eq!(scan.nodes.len(), 1);
                assert!(app.deletion_notice.as_ref().unwrap().contains(TRASH_NAME));
                assert!(app.search_input.read(cx).value().is_empty());
            });
        })
        .unwrap();
        assert!(!temp.0.join("scan/folder-to-trash").exists());
        assert!(temp.0.join("mock-trash/content.txt").is_file());
    }

    #[gpui_kit::test]
    fn deletion_failure_preserves_tree_and_allows_retry(cx: &mut TestAppContext) {
        let (temp, handle, view, id) = trash_fixture(cx);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| {
                app.trash_operation = |_| Err("mock permission denied".into());
                app.request_delete(id, window, cx);
            });
            window.click("delete-confirm", cx);
            view.update(cx, |app, cx| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.deletion.is_some() && Instant::now() < deadline {
                    app.poll_deletion(window, cx);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(!app.busy());
                assert!(app.session.status.contains("mock permission denied"));
                assert_eq!(app.session.selected, Some(id));
                assert!(app.deletable_folder(id));
                app.request_delete(id, window, cx);
            });
            assert!(window.has_active_dialog(cx));
            window.click("delete-cancel", cx);
        })
        .unwrap();
        assert!(temp.0.join("scan/folder-to-trash/content.txt").is_file());
    }

    #[gpui_kit::test]
    fn root_and_stale_confirmation_never_start_deletion(cx: &mut TestAppContext) {
        let (_temp, handle, view, id) = trash_fixture(cx);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| {
                let root = app.session.result.as_ref().unwrap().root;
                assert!(!app.deletable_folder(root));
                app.request_delete(root, window, cx);
                assert!(app.pending_deletion.is_none());
                app.request_delete(id, window, cx);
                app.generation += 1;
            });
            window.click("delete-confirm", cx);
            assert!(view.read(cx).deletion.is_none());
            assert!(view.read(cx).session.status.contains("已变化"));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn folder_context_menu_is_present_and_root_is_disabled(cx: &mut TestAppContext) {
        let (_temp, handle, view, id) = trash_fixture(cx);
        cx.update_window(handle, |_, window, cx| {
            let root = view.read(cx).session.result.as_ref().unwrap().root;
            window.render_frame(cx);
            window.right_click(("row", root), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let item = window.find_by_label(DELETE_LABEL);
            assert!(item.visible());
            window.click(item.path().last().unwrap().clone(), cx);
            assert!(view.read(cx).pending_deletion.is_none());
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.right_click(("row", id), cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let item = window.find_by_label(DELETE_LABEL);
            assert!(item.visible());
            window.click(item.path().last().unwrap().clone(), cx);
            assert_eq!(view.read(cx).pending_deletion.is_some(), TRASH_SUPPORTED);
            window.press("escape", cx);
            assert!(view.read(cx).pending_deletion.is_none());
        })
        .unwrap();
    }
}
