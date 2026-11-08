use eframe::egui::{self, Color32, FontFamily, RichText, Vec2};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tree_lens::format_bytes;
use tree_lens::scanner::{self, NodeId, NodeKind, ScanOutcome, ScanProgress};
use tree_lens::tree::{visible_rows, SortBy};

const ACCENT: Color32 = Color32::from_rgb(97, 201, 174);
const MUTED: Color32 = Color32::from_rgb(149, 160, 177);
const WARN: Color32 = Color32::from_rgb(242, 187, 98);
const ROW_HEIGHT: f32 = 31.0;

enum WorkerEvent {
    Progress(ScanProgress),
    Finished(Result<ScanOutcome, String>),
}

pub struct TreeLensApp {
    path_text: String,
    root_path: Option<PathBuf>,
    initial_path: Option<PathBuf>,
    result: Option<ScanOutcome>,
    receiver: Option<mpsc::Receiver<WorkerEvent>>,
    cancel: Option<Arc<AtomicBool>>,
    started: Option<Instant>,
    progress: Option<ScanProgress>,
    status: String,
    expanded: HashSet<NodeId>,
    rows: Vec<(NodeId, usize)>,
    query: String,
    sort: SortBy,
    selected: Option<NodeId>,
    show_issues: bool,
    show_about: bool,
}

impl TreeLensApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        Self::with_context(&cc.egui_ctx, initial_path)
    }

    fn with_context(ctx: &egui::Context, initial_path: Option<PathBuf>) -> Self {
        install_fonts(ctx);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(22, 26, 34);
        visuals.window_fill = Color32::from_rgb(30, 35, 44);
        visuals.selection.bg_fill = Color32::from_rgb(37, 83, 78);
        visuals.selection.stroke.color = ACCENT;
        ctx.set_visuals(visuals);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = Vec2::new(10.0, 8.0);
            style.spacing.button_padding = Vec2::new(12.0, 7.0);
        });
        Self {
            path_text: initial_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            initial_path,
            root_path: None,
            result: None,
            receiver: None,
            cancel: None,
            started: None,
            progress: None,
            status: "选择一个文件夹，了解空间都用在了哪里。".into(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            query: String::new(),
            sort: SortBy::default(),
            selected: None,
            show_issues: false,
            show_about: false,
        }
    }

    fn busy(&self) -> bool {
        self.receiver.is_some()
    }

    fn choose_folder(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        {
            // Cocoa panels must be created from the UI thread, not the scanner.
            let mut dialog = rfd::FileDialog::new().set_title("选择要分析的文件夹");
            if let Some(root) = &self.root_path {
                dialog = dialog.set_directory(root);
            }
            if let Some(path) = dialog.pick_folder() {
                self.start(path, ctx);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            self.status = "此开发平台请在路径栏输入文件夹；macOS 提供原生选择窗口。".into();
        }
    }

    fn start(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.path_text = path.display().to_string();
        self.root_path = Some(path.clone());
        self.result = None;
        self.rows.clear();
        self.expanded.clear();
        self.selected = None;
        self.query.clear();
        self.progress = None;
        self.started = Some(Instant::now());
        self.status = "正在扫描…".into();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let context = ctx.clone();
        let spawn = std::thread::Builder::new()
            .name("disk-scanner".into())
            .spawn(move || {
                let result = scanner::scan(&path, &worker_cancel, |progress| {
                    let _ = sender.send(WorkerEvent::Progress(progress));
                    context.request_repaint();
                })
                .map_err(|error| error.to_string());
                let _ = sender.send(WorkerEvent::Finished(result));
                context.request_repaint();
            });
        match spawn {
            Ok(_) => {
                self.cancel = Some(cancel);
                self.receiver = Some(receiver);
            }
            Err(error) => {
                self.status = format!("无法启动扫描：{error}");
            }
        }
    }

    fn poll(&mut self) {
        let mut completed = false;
        if let Some(receiver) = &self.receiver {
            loop {
                match receiver.try_recv() {
                    Ok(WorkerEvent::Progress(progress)) => self.progress = Some(progress),
                    Ok(WorkerEvent::Finished(result)) => {
                        match result {
                            Ok(scan) => {
                                let root = &scan.nodes[scan.root];
                                self.status = if scan.cancelled {
                                    "扫描已取消，下方仅显示已扫描部分，不代表完整大小。".into()
                                } else if root.issue_count > 0 {
                                    format!(
                                        "扫描结束 · {} 处未能完整读取，统计可能偏小",
                                        root.issue_count
                                    )
                                } else {
                                    format!("扫描完成 · {:.1} 秒", scan.elapsed.as_secs_f64())
                                };
                                self.expanded.insert(scan.root);
                                self.selected = Some(scan.root);
                                self.result = Some(scan);
                            }
                            Err(error) => self.status = format!("扫描失败：{error}"),
                        }
                        completed = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.status = "扫描线程意外结束，请重新扫描。".into();
                        completed = true;
                        break;
                    }
                }
            }
        }
        if completed {
            self.receiver = None;
            self.cancel = None;
            self.rebuild_rows();
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows = self
            .result
            .as_ref()
            .map(|scan| visible_rows(scan, &self.expanded, &self.query, self.sort))
            .unwrap_or_default();
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("◈").size(29.0).color(ACCENT));
            ui.label(RichText::new("Tree Lens").size(26.0).strong());
            ui.label(RichText::new("磁盘空间，一目了然").color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("关于 / 统计说明").clicked() {
                    self.show_about = true;
                }
            });
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let busy = self.busy();
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(RichText::new("选择文件夹").color(ACCENT)),
                )
                .clicked()
            {
                self.choose_folder(ctx);
            }
            let input = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.path_text)
                    .hint_text("输入绝对路径，例如 /Users/你的用户名/Downloads")
                    .desired_width((ui.available_width() - 170.0).max(180.0)),
            );
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(
                    !busy && !self.path_text.trim().is_empty(),
                    egui::Button::new("扫描"),
                )
                .clicked()
                || (enter && !busy && !self.path_text.trim().is_empty())
            {
                self.start(expand_home(&self.path_text), ctx);
            }
            if busy {
                let cancelling = self
                    .cancel
                    .as_ref()
                    .is_some_and(|c| c.load(Ordering::Relaxed));
                if ui
                    .add_enabled(
                        !cancelling,
                        egui::Button::new(if cancelling { "取消中…" } else { "取消" }),
                    )
                    .clicked()
                {
                    if let Some(cancel) = &self.cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            } else if ui
                .add_enabled(self.root_path.is_some(), egui::Button::new("刷新"))
                .clicked()
            {
                if let Some(path) = self.root_path.clone() {
                    self.start(path, ctx);
                }
            }
        });
    }

    fn summaries(&self, ui: &mut egui::Ui) {
        let (allocated, logical, files, dirs) = if let Some(scan) = &self.result {
            let root = &scan.nodes[scan.root];
            (
                root.total_allocated_bytes,
                root.total_logical_bytes,
                root.file_count,
                root.directory_count.saturating_sub(1),
            )
        } else if let Some(progress) = &self.progress {
            (
                progress.allocated_bytes,
                progress.logical_bytes,
                progress.files,
                progress.directories.saturating_sub(1),
            )
        } else {
            (0, 0, 0, 0)
        };
        let partial = self.busy()
            || self
                .result
                .as_ref()
                .is_some_and(|s| s.cancelled || s.nodes[s.root].issue_count > 0);
        let cards = [
            (
                if partial {
                    "已统计磁盘占用"
                } else {
                    "磁盘占用"
                },
                format_bytes(allocated),
            ),
            ("逻辑大小", format_bytes(logical)),
            ("文件", files.to_string()),
            ("子文件夹", dirs.to_string()),
        ];
        ui.columns(4, |columns| {
            for (column, (title, value)) in columns.iter_mut().zip(cards) {
                egui::Frame::new()
                    .fill(Color32::from_rgb(31, 37, 47))
                    .corner_radius(9)
                    .inner_margin(16)
                    .show(column, |ui| {
                        ui.set_min_width((ui.available_width() - 1.0).max(60.0));
                        ui.label(RichText::new(title).color(MUTED).size(13.0));
                        ui.label(RichText::new(value).size(24.0).strong().color(ACCENT));
                    });
            }
        });
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("文件夹树").strong().size(17.0));
            let changed = ui
                .add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("按名称筛选…")
                        .desired_width(210.0),
                )
                .changed();
            if changed {
                self.rebuild_rows();
            }
            if !self.query.is_empty() && ui.small_button("清除").clicked() {
                self.query.clear();
                self.rebuild_rows();
            }
            if ui.button("收起全部").clicked() {
                self.expanded.clear();
                if let Some(scan) = &self.result {
                    self.expanded.insert(scan.root);
                }
                self.rebuild_rows();
            }
            if ui.button("展开一级").clicked() {
                let ids: Vec<_> = self.rows.iter().map(|(id, _)| *id).collect();
                self.expanded.extend(ids);
                self.rebuild_rows();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{} 行", self.rows.len())).color(MUTED));
            });
        });
        ui.add_space(7.0);
        if self.result.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(85.0);
                if self.busy() {
                    ui.spinner();
                }
                ui.label(
                    RichText::new(if self.busy() {
                        "正在读取文件信息"
                    } else {
                        "从一个文件夹开始"
                    })
                    .size(23.0),
                );
                ui.label(
                    RichText::new(if self.busy() {
                        "扫描在后台进行，你可以随时取消。"
                    } else {
                        "点击“选择文件夹”，或将文件夹拖到窗口中。"
                    })
                    .color(MUTED),
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new("只读分析 · 不删除文件 · 不跟随符号链接")
                        .small()
                        .color(MUTED),
                );
            });
            return;
        }
        let width = ui.available_width().max(1010.0);
        let name_width = width - 610.0;
        let mut next_sort = None;
        let mut toggle = None;
        let mut selection = None;
        let mut reveal = None;
        egui::ScrollArea::horizontal()
            .id_salt("horizontal-tree")
            .show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    for (label, column_width, sort) in [
                        ("名称", name_width, SortBy::Name),
                        ("磁盘占用 ↓", 125.0, SortBy::Allocated),
                        ("逻辑大小 ↓", 125.0, SortBy::Logical),
                        ("占父级", 190.0, self.sort),
                        ("文件数 ↓", 120.0, SortBy::Files),
                    ] {
                        ui.allocate_ui_with_layout(
                            Vec2::new(column_width, 27.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                if ui
                                    .selectable_label(
                                        self.sort == sort && label != "占父级",
                                        RichText::new(label).color(MUTED),
                                    )
                                    .clicked()
                                    && label != "占父级"
                                {
                                    next_sort = Some(sort);
                                }
                            },
                        );
                    }
                });
                ui.separator();
                let scan = self.result.as_ref().expect("result checked above");
                egui::ScrollArea::vertical()
                    .id_salt("vertical-tree")
                    .auto_shrink([false, false])
                    .show_rows(ui, ROW_HEIGHT, self.rows.len(), |ui, range| {
                        for index in range {
                            let (id, depth) = self.rows[index];
                            let node = &scan.nodes[id];
                            let is_dir = node.kind == NodeKind::Directory;
                            let is_open =
                                self.expanded.contains(&id) || !self.query.trim().is_empty();
                            let fill = if self.selected == Some(id) {
                                Color32::from_rgb(36, 63, 65)
                            } else if index % 2 == 0 {
                                Color32::from_rgb(26, 31, 40)
                            } else {
                                Color32::TRANSPARENT
                            };
                            let (row_rect, _) = ui.allocate_exact_size(
                                Vec2::new(width, ROW_HEIGHT),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(row_rect, 4.0, fill);
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(row_rect)
                                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                            );
                            child.spacing_mut().item_spacing.y = 0.0;
                            child.allocate_ui_with_layout(
                                Vec2::new(name_width, ROW_HEIGHT),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add_space((depth.min(20) as f32) * 14.0);
                                    if is_dir && !node.children.is_empty() {
                                        if ui
                                            .add_sized(
                                                [18.0, 24.0],
                                                egui::Button::new(if is_open {
                                                    "▾"
                                                } else {
                                                    "▸"
                                                })
                                                .frame(false),
                                            )
                                            .clicked()
                                            && self.query.trim().is_empty()
                                        {
                                            toggle = Some(id);
                                        }
                                    } else {
                                        ui.add_space(18.0);
                                    }
                                    let icon = match node.kind {
                                        NodeKind::Directory => "▰",
                                        NodeKind::Symlink => "↗",
                                        NodeKind::File => "▪",
                                        NodeKind::Other => "·",
                                    };
                                    ui.label(RichText::new(icon).color(if is_dir {
                                        ACCENT
                                    } else {
                                        MUTED
                                    }));
                                    let label = format!(
                                        "{}{}{}",
                                        node.name,
                                        if node.hard_link_duplicate {
                                            "  [硬链接]"
                                        } else {
                                            ""
                                        },
                                        if node.issue_count > 0 { "  ⚠" } else { "" }
                                    );
                                    let response = ui.add(
                                        egui::Label::new(RichText::new(label).color(
                                            if node.error.is_some() {
                                                WARN
                                            } else {
                                                Color32::from_rgb(223, 229, 236)
                                            },
                                        ))
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                    );
                                    if response.clicked() {
                                        selection = Some(id);
                                    }
                                    if response.double_clicked()
                                        && is_dir
                                        && self.query.trim().is_empty()
                                    {
                                        toggle = Some(id);
                                    }
                                    response.clone().on_hover_text(format!(
                                        "{}{}",
                                        node.path.display(),
                                        node.error
                                            .as_ref()
                                            .map(|e| format!("\n{e}"))
                                            .unwrap_or_default()
                                    ));
                                    response.context_menu(|ui| {
                                        if ui.button("复制完整路径").clicked() {
                                            ui.ctx().copy_text(node.path.display().to_string());
                                            ui.close_menu();
                                        }
                                        #[cfg(target_os = "macos")]
                                        if ui.button("在 Finder 中显示").clicked() {
                                            reveal = Some(node.path.clone());
                                            ui.close_menu();
                                        }
                                    });
                                },
                            );
                            column_value(
                                &mut child,
                                125.0,
                                format_bytes(node.total_allocated_bytes),
                                ACCENT,
                            );
                            column_value(
                                &mut child,
                                125.0,
                                format_bytes(node.total_logical_bytes),
                                MUTED,
                            );
                            let parent_total = node
                                .parent
                                .map(|p| scan.nodes[p].total_allocated_bytes)
                                .unwrap_or(node.total_allocated_bytes);
                            let fraction = if parent_total == 0 {
                                0.0
                            } else {
                                node.total_allocated_bytes as f64 / parent_total as f64
                            };
                            child.allocate_ui_with_layout(
                                Vec2::new(190.0, ROW_HEIGHT),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::ProgressBar::new(fraction as f32)
                                            .desired_width(112.0)
                                            .desired_height(7.0)
                                            .fill(ACCENT),
                                    );
                                    ui.label(
                                        RichText::new(format!("{:.1}%", fraction * 100.0))
                                            .small()
                                            .color(MUTED),
                                    );
                                },
                            );
                            column_value(&mut child, 120.0, node.file_count.to_string(), MUTED);
                        }
                    });
            });
        if let Some(id) = toggle {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
            self.rebuild_rows();
        }
        if let Some(sort) = next_sort {
            self.sort = sort;
            self.rebuild_rows();
        }
        if let Some(id) = selection {
            self.selected = Some(id);
        }
        // Keep the no-op Linux type explicit so the development build also compiles.
        let reveal: Option<PathBuf> = reveal.take();
        if let Some(path) = reveal {
            reveal_in_finder(path, &mut self.status);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        egui::Window::new("读取问题")
            .open(&mut self.show_issues)
            .default_width(700.0)
            .show(ctx, |ui| {
                ui.label("无法访问的内容没有纳入完整统计。应用不会请求管理员权限或修改权限。");
                ui.label(
                    "macOS 系统设置 → 隐私与安全性 → 完全磁盘访问权限，可由你自行决定是否授权。",
                );
                if let Some(scan) = &self.result {
                    let count = scan.nodes[scan.root].issue_count;
                    ui.label(format!("共 {count} 处问题，以下最多展示 500 条。"));
                    egui::ScrollArea::both().max_height(430.0).show(ui, |ui| {
                        for issue in &scan.issues {
                            ui.label(RichText::new(issue.path.display().to_string()).color(WARN));
                            ui.label(&issue.message);
                            ui.separator();
                        }
                    });
                }
            });
        egui::Window::new("关于 Tree Lens").open(&mut self.show_about).default_width(530.0).show(ctx, |ui| {
            ui.heading("Tree Lens 0.1");
            ui.label("一个用 Rust 编写的只读磁盘空间分析工具，当前面向 macOS。");
            ui.separator();
            for text in [
                "磁盘占用：文件系统报告的已分配块数 × 512，包括文件夹元数据。",
                "逻辑大小：文件内容长度；文件夹为子项累计，不计文件夹本身的逻辑长度。",
                "同一扫描中的硬链接只计算一次磁盘占用，逻辑大小按路径分别计算。哪条链接承担占用取决于扫描顺序。",
                "不跟随符号链接，也不进入其他设备的挂载点；隐藏文件和 .app 包都会扫描。",
                "APFS 克隆、压缩、快照及共享块可能让统计与真正可释放空间不同；此数字不是删除后必然可回收的容量。",
                "扫描不会读取文件内容，不主动下载 iCloud 内容。若系统仍需访问云端元数据，耗时由系统决定。",
                "文件扫描期间仍可能变化；报错或取消后，结果仅代表已读取部分。所有单位使用 1024 进制。",
                "此版本不提供删除、移动或清理操作。⌘O 选择文件夹；⌘R 刷新。",
            ] { ui.label(text); ui.add_space(5.0); }
        });
    }
}

impl eframe::App for TreeLensApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if let Some(path) = self.initial_path.take() {
            self.start(path, ctx);
        }
        if !self.busy() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
                self.choose_folder(ctx);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::R)) {
                if let Some(path) = self.root_path.clone() {
                    self.start(path, ctx);
                }
            }
            let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
            if let Some(path) = dropped {
                self.start(path, ctx);
            }
        }
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .inner_margin(22)
                    .fill(Color32::from_rgb(22, 26, 34)),
            )
            .show(ctx, |ui| {
                self.header(ui, ctx);
            });
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .inner_margin(12)
                    .fill(Color32::from_rgb(26, 31, 40)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.busy() {
                        ui.spinner();
                    }
                    let status = if self.busy() {
                        let entries = self.progress.as_ref().map_or(0, |p| p.entries);
                        let seconds = self.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
                        format!("已检查 {entries} 个项目 · {seconds:.1} 秒")
                    } else {
                        self.status.clone()
                    };
                    ui.label(RichText::new(status).color(MUTED));
                    if self
                        .result
                        .as_ref()
                        .is_some_and(|s| s.nodes[s.root].issue_count > 0)
                        && ui.button("查看问题").clicked()
                    {
                        self.show_issues = true;
                    }
                });
                let path = if self.busy() {
                    self.progress.as_ref().map(|p| &p.current_path)
                } else {
                    self.result
                        .as_ref()
                        .and_then(|s| self.selected.map(|id| &s.nodes[id].path))
                };
                if let Some(path) = path {
                    ui.add(
                        egui::Label::new(
                            RichText::new(path.display().to_string())
                                .small()
                                .color(MUTED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(path.display().to_string());
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(22)
                    .fill(Color32::from_rgb(22, 26, 34)),
            )
            .show(ctx, |ui| {
                self.summaries(ui);
                ui.add_space(22.0);
                self.tree(ui);
            });
        self.dialogs(ctx);
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl Drop for TreeLensApp {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn column_value(ui: &mut egui::Ui, width: f32, value: String, color: Color32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(RichText::new(value).monospace().color(color));
        },
    );
}

fn expand_home(input: &str) -> PathBuf {
    let input = input.trim();
    if input == "~" || input.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(input.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(input)
}

fn reveal_in_finder(path: PathBuf, status: &mut String) {
    #[cfg(target_os = "macos")]
    if let Err(error) = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .spawn()
    {
        *status = format!("无法打开 Finder：{error}");
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, status);
    }
}

fn install_fonts(ctx: &egui::Context) {
    let candidates = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    let mut fonts = egui::FontDefinitions::default();
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "system-cjk".into(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            fonts
                .families
                .get_mut(&FontFamily::Proportional)
                .unwrap()
                .push("system-cjk".into());
            fonts
                .families
                .get_mut(&FontFamily::Monospace)
                .unwrap()
                .push("system-cjk".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn wait_for_scan(app: &mut TreeLensApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.busy() && Instant::now() < deadline {
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!app.busy(), "scan did not complete in five seconds");
    }

    #[test]
    fn headless_ui_renders_empty_and_results_at_multiple_sizes() {
        let ctx = egui::Context::default();
        let mut app = TreeLensApp::with_context(&ctx, None);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        for loaded in [false, true] {
            if loaded {
                app.start(path.clone(), &ctx);
                wait_for_scan(&mut app);
            }
            for size in [[1180.0, 780.0], [820.0, 560.0]] {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::from(size),
                    )),
                    ..Default::default()
                };
                let output = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.header(ui, ctx);
                        app.summaries(ui);
                        app.tree(ui);
                    });
                    app.dialogs(ctx);
                });
                assert!(!output.shapes.is_empty());
                let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
                assert!(!meshes.is_empty());
            }
        }
        app.query = "scanner".into();
        app.rebuild_rows();
        assert_eq!(app.rows.len(), 2);
        app.query.clear();
        app.expanded.clear();
        app.rebuild_rows();
        assert_eq!(app.rows.len(), 1);
    }

    #[test]
    fn failed_scan_can_be_retried_and_duplicate_start_is_ignored() {
        let ctx = egui::Context::default();
        let mut app = TreeLensApp::with_context(&ctx, None);
        let missing =
            std::env::temp_dir().join(format!("tree-lens-missing-{}", std::process::id()));
        assert!(!missing.exists());
        app.start(missing, &ctx);
        let ignored = PathBuf::from("this-second-request-must-not-start");
        app.start(ignored.clone(), &ctx);
        assert_ne!(app.root_path, Some(ignored));
        wait_for_scan(&mut app);
        assert!(app.result.is_none());
        assert!(app.status.starts_with("扫描失败"));
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        app.start(directory, &ctx);
        wait_for_scan(&mut app);
        assert!(app.result.is_some());
        assert!(!app.rows.is_empty());
        assert!(app.cancel.is_none());
    }

    #[test]
    fn input_paths_and_font_fallback_are_available() {
        assert_eq!(expand_home(" /tmp/test "), PathBuf::from("/tmp/test"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~"), PathBuf::from(&home));
            assert_eq!(
                expand_home("~/Downloads"),
                PathBuf::from(home).join("Downloads")
            );
        }
        assert!(fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().is_dir());
    }
}
