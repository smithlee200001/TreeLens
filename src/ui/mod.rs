//! Native GPUI presentation shared by the desktop entry point and snapshots.
//! Feature state lives in `features`; OS integrations live in `platform`.
mod controller;
#[cfg(all(test, feature = "ui-tests"))]
mod flicker_tests;
#[cfg(all(test, feature = "ui-tests"))]
mod inspector_tests;
#[cfg(all(test, feature = "ui-tests"))]
mod switch_capture;
#[cfg(all(test, feature = "ui-tests"))]
mod tests;
mod theme;

use crate::{
    features::{
        delete::{DeletionMode, DeletionRequest},
        explorer::{
            footprint::{self, Tile},
            session::{expand_home, Session},
            tree::SortBy,
        },
        scan::{NodeId, NodeKind},
    },
    format_bytes,
    platform::{self, desktop, DELETE_LABEL, TRASH_NAME, TRASH_SUPPORTED},
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, PopupMenuItem},
    scroll::ScrollableElement,
    spinner::Spinner,
    tooltip::Tooltip,
    Disableable, Icon, IconName, Sizable, TitleBar, WindowExt,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    ops::Range,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
pub use theme::install_theme;
use theme::*;

const TREE_ROW_HEIGHT: f32 = 40.0;
gpui_kit::actions!(
    tree_lens,
    [OpenFolder, Refresh, FocusSearch, CancelScan, Quit]
);

struct DeletionPreparation {
    generation: u64,
    id: NodeId,
    receiver: mpsc::Receiver<Result<DeletionRequest, String>>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for DeletionPreparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
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
    preparing_deletion: Option<DeletionPreparation>,
    deletion: Option<mpsc::Receiver<Result<(), String>>>,
    deletion_notice: Option<String>,
    deletion_mode: Option<DeletionMode>,
    delete_operation: fn(DeletionRequest) -> Result<(), String>,
}

impl TreeLensApp {
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.navigation_blocked() || self.choosing;
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
                    // Busy still disables actions, but does not pulse text
                    // throughout the otherwise unchanged navigation panel.
                    .text_color(rgb(TEXT))
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
                    .child(concat!("GPUI KIT / ", env!("CARGO_PKG_VERSION"))),
            )
    }

    /// Always occupies the existing subtitle line: loading a new root must not
    /// insert/remove panels or shift the viewport just to show its progress.
    fn root_context(&self) -> Div {
        let displayed = if self.session.busy() && !self.session.reconciling() {
            self.session
                .root_path
                .as_ref()
                .map(|path| format!("正在扫描：{}", path.display()))
        } else if let Some(scan) = &self.session.result {
            Some(format!("当前显示：{}", scan.root_path.display()))
        } else {
            self.session
                .root_path
                .as_ref()
                .map(|path| format!("当前目录：{}", path.display()))
        }
        .unwrap_or_else(|| "选择文件夹，查看空间占用。".into());
        div().flex().gap_3().h(px(20.0)).min_w_0().text_xs().child(
            div()
                .id("displayed-root")
                .test_support()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(rgb(MUTED))
                .aria_label(displayed.clone())
                .tooltip({
                    let label = displayed.clone();
                    move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                })
                .child(displayed),
        )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy();
        let navigation_blocked = self.navigation_blocked() || self.choosing;
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
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(26.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child("让空间，看得见。"),
                            )
                            .child(self.root_context()),
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
                            .child("●  LOCAL / FILES"),
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
                            .disabled(navigation_blocked)
                            .when(navigation_blocked, |button| {
                                button
                                    .text_color(rgb(0x092322))
                                    .bg(rgb(CYAN))
                                    .border_color(rgb(CYAN))
                            })
                            .tooltip(desktop::OPEN_TOOLTIP)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.path_input)
                                // Read-only keeps text/copying stable during deletion;
                                // ordinary scans can be replaced by a new path.
                                .readonly(navigation_blocked)
                                .aria_label("扫描文件夹路径"),
                        ),
                    )
                    .child(
                        Button::new("scan-path")
                            .label("扫描")
                            .disabled(navigation_blocked)
                            .when(navigation_blocked, stable_navigation_button)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let path = this.path_input.read(cx).value().to_string();
                                if !path.trim().is_empty() {
                                    this.start(expand_home(&path), window, cx);
                                }
                            })),
                    )
                    .child(if self.session.busy() {
                        Button::new("cancel")
                            .w(px(80.0))
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
                            .w(px(80.0))
                            .icon(IconName::RotateCw)
                            .tooltip(desktop::REFRESH_TOOLTIP)
                            .disabled(busy || self.session.root_path.is_none())
                            .when(busy, stable_navigation_button)
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
        let generation = self.generation;
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
                            if this.generation == generation {
                                this.select_node(id, cx);
                            }
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
                    .child(if self.session.busy() && self.session.result.is_none() {
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
        let delete_enabled = TRASH_SUPPORTED && self.deletable_item(id, DeletionMode::Trash);
        let permanent_enabled = TRASH_SUPPORTED && self.deletable_item(id, DeletionMode::Permanent);
        let file = node.kind == NodeKind::File;
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
            .h(px(TREE_ROW_HEIGHT))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(rgb(0x182430))
            .bg(rgb(if selected { 0x18353a } else { PANEL }))
            .hover(|style| style.bg(rgb(0x1b2a37)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if this.generation != generation {
                    return;
                }
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
                                    if this.generation != generation {
                                        return;
                                    }
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
                let permanent_view = view.clone();
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
                .when(directory || file, |menu| {
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
                .when(directory || file, |menu| {
                    menu.item(
                        PopupMenuItem::new("彻底删除…")
                            .disabled(!permanent_enabled)
                            .on_click(move |_, window, cx| {
                                let _ = permanent_view.update(cx, |this, cx| {
                                    if this.generation == generation {
                                        this.request_delete_mode(
                                            id,
                                            DeletionMode::Permanent,
                                            window,
                                            cx,
                                        );
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
        let generation = self.generation;
        let panel = div()
            .w(px(220.0))
            .min_h_0()
            .overflow_hidden()
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
                    .id("inspector-heading")
                    .test_support()
                    .flex_shrink_0()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child("INSPECTOR / 项目详情"),
            );
        // Only this viewport shrinks. Its single natural-height content column
        // keeps text and actions intact even in a short window.
        let mut body = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w_full()
            .min_w_0()
            .gap_3();
        if let Some((scan, id)) = self.session.result.as_ref().zip(self.session.selected) {
            let node = &scan.nodes[id];
            let name = node.display_name().into_owned();
            let path = scan.path_for(id).display().to_string();
            body = body
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
                        .id("inspector-name")
                        .test_support()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .aria_label(name.clone())
                        .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
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
                        .id("inspector-path")
                        .test_support()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .text_xs()
                        .whitespace_normal()
                        .aria_label(path.clone())
                        .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
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
                        .on_click(cx.listener(move |this, _, _, cx| { if this.generation == generation { this.copy_path(id, cx); } })),
                )
                .when(TRASH_SUPPORTED, |panel| {
                    panel.child(
                        Button::new("reveal-selected")
                            .label(platform::REVEAL_LABEL)
                            .icon(IconName::ExternalLink)
                            .on_click(cx.listener(move |this, _, _, cx| { if this.generation == generation { this.reveal(id, cx); } })),
                    )
                })
                .when(
                    matches!(node.kind, NodeKind::Directory | NodeKind::File),
                    |panel| {
                        panel.child(
                            Button::new("delete-selected")
                                .danger()
                                .label(DELETE_LABEL)
                                .disabled(
                                    !TRASH_SUPPORTED
                                        || !self.deletable_item(id, DeletionMode::Trash),
                                )
                                .tooltip(if id == scan.root {
                                    "不能删除当前扫描根目录；请先扫描它的父文件夹。"
                                } else if !TRASH_SUPPORTED {
                                    "此平台尚不支持移到系统回收站/废纸篓。"
                                } else {
                                    "确认后将所选项目移到系统回收站/废纸篓，不永久删除。"
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if this.generation == generation { this.request_delete(id, window, cx); }
                                })),
                        )
                    },
                )
                .when(matches!(node.kind, NodeKind::File | NodeKind::Directory), |panel| {
                    panel.child(
                        Button::new("delete-permanent-selected")
                            .danger()
                            .label("彻底删除…")
                            .disabled(
                                !TRASH_SUPPORTED
                                    || !self.deletable_item(id, DeletionMode::Permanent),
                            )
                            .tooltip("单独确认后彻底删除所选项目及文件夹全部内容，不进入回收站/废纸篓，无法撤销。")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.generation == generation { this.request_delete_mode(id, DeletionMode::Permanent, window, cx); }
                            })),
                    )
                });
        } else {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("选择文件树中的项目，在这里查看详细信息。"),
            );
        }
        panel
            .child(
                div()
                    .id("inspector-scroll")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(div().size_full().child(body).overflow_y_scrollbar()),
            )
            .child(
                div()
                    .id("inspector-caveat")
                    .test_support()
                    .flex_shrink_0()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child(platform::STORAGE_CAVEAT),
            )
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> Div {
        let status = if self.preparing_deletion.is_some() {
            "正在核对文件夹全部内容…按 Esc 取消；尚未删除任何项目。".into()
        } else if self.deletion.is_some() {
            match self.deletion_mode {
                Some(DeletionMode::Permanent) => "正在彻底删除…请勿移动或重命名。".into(),
                _ => format!("正在移到{TRASH_NAME}…请勿移动或重命名此项目。"),
            }
        } else if self.session.reconciling() {
            "正在后台核对列表与大小，保留当前位置…".into()
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
            // A spinner inside this view requests a full TreeLensApp render
            // every animation frame. Retained work only needs its status text
            // and a static indicator, not repeated redraws of unchanged glyphs.
            .child(
                div()
                    .id("status-indicator")
                    .test_support()
                    .w(px(16.0))
                    .h(px(16.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if self.busy() && self.session.result.is_none() {
                        Spinner::new()
                            .xsmall()
                            .color(rgb(CYAN).into())
                            .into_any_element()
                    } else {
                        div()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(rgb(if self.busy() || self.session.partial() {
                                AMBER
                            } else {
                                CYAN
                            }))
                            .into_any_element()
                    }),
            )
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
                        .disabled(self.deletion.is_some() || self.preparing_deletion.is_some())
                        .text_color(rgb(TEXT))
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
                    .disabled(self.deletion.is_some() || self.preparing_deletion.is_some())
                    .text_color(rgb(TEXT))
                    .ghost()
                    .xsmall()
                    .label("统计说明")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.show_dialog(false, window, cx)),
                    ),
            )
    }

    fn show_dialog(&self, issues: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.deletion.is_some()
            || self.preparing_deletion.is_some()
            || window.has_active_dialog(cx)
        {
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
                    concat!("Tree Lens ", env!("CARGO_PKG_VERSION"), " · 原生 Rust / GPUI Kit 0.9"),
                    "扫描只读取文件系统元数据，不读取文件内容，不上传结果。文件和文件夹均可移到系统回收站/废纸篓或彻底删除，均需单独确认；彻底删除文件夹会删除其中全部内容。",
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
                    if this.pending_deletion.take().is_some() {
                        this.session.status = "已取消删除；没有删除任何项目。".into();
                    }
                    window.close_dialog(cx);
                } else if this.preparing_deletion.take().is_some() {
                    this.session.status = "已取消删除准备；没有删除任何项目。".into();
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

/// Keep short-lived navigation locks semantically disabled without changing
/// their idle colors. The status bar explains why actions are temporarily held.
fn stable_navigation_button(button: Button) -> Button {
    button
        .text_color(rgb(TEXT))
        .bg(rgb(0x1a2634))
        .border_color(rgb(0x0c141e))
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
