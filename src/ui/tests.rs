//! Interaction and retained-view regressions against the shared library view.
use super::{install_theme, TreeLensApp, DELETE_LABEL, TRASH_NAME, TRASH_SUPPORTED};
use crate::features::{delete::DeletionMode, explorer::tree::SortBy, scan::NodeKind};
use gpui_kit::component::WindowExt;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext, Bounds, Entity, TestAppContext, WindowBounds,
    WindowOptions,
};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

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
                            app.poll_session(window, cx);
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
        window.press(crate::platform::desktop::REFRESH_SHORTCUT, cx);
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
        Self(std::fs::canonicalize(root).unwrap())
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
    deletion_fixture(cx, 1080.0)
}

fn deletion_fixture(
    cx: &mut TestAppContext,
    width: f32,
) -> (TrashFixture, AnyWindowHandle, Entity<TreeLensApp>, usize) {
    let temp = TrashFixture::new();
    let (handle, view) = fixture(cx, width, false);
    let id = cx
        .update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| {
                app.start(temp.0.join("scan"), window, cx);
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.session.busy() && Instant::now() < deadline {
                    app.poll_session(window, cx);
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

fn mock_trash(request: crate::features::delete::DeletionRequest) -> Result<(), String> {
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
            app.delete_operation = mock_trash;
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
                app.poll_session(window, cx);
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
            app.delete_operation = |_| Err("mock permission denied".into());
            app.request_delete(id, window, cx);
        });
        window.click("delete-confirm", cx);
        view.update(cx, |app, cx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_deletion(window, cx);
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            assert!(app
                .deletion_notice
                .as_ref()
                .unwrap()
                .contains("mock permission denied"));
            assert_eq!(app.session.selected, Some(id));
            assert!(app.deletable_item(id, DeletionMode::Trash));
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
            assert!(!app.deletable_item(root, DeletionMode::Trash));
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

fn file_fixture(
    cx: &mut TestAppContext,
    width: f32,
) -> (TrashFixture, AnyWindowHandle, Entity<TreeLensApp>, usize) {
    let (temp, handle, view, _) = deletion_fixture(cx, width);
    let id = cx.update(|cx| {
        view.update(cx, |app, cx| {
            let id = app
                .session
                .result
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::File)
                .unwrap()
                .id;
            app.session.expand_one_level();
            app.session.select(id);
            app.reset_projection();
            cx.notify();
            id
        })
    });
    (temp, handle, view, id)
}

fn checked_file_operation(request: crate::features::delete::DeletionRequest) -> Result<(), String> {
    let path = request.path();
    assert_eq!(path.file_name().unwrap(), "content.txt");
    let fixture = path.parent().unwrap().parent().unwrap().parent().unwrap();
    assert!(fixture
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("tree-lens-ui-trash-"));
    match request.mode() {
        DeletionMode::Trash => {
            std::fs::rename(path, fixture.join("mock-file-trash")).map_err(|e| e.to_string())
        }
        // Execute only after validating that this is our isolated synthetic file.
        DeletionMode::Permanent => request.execute(),
    }
}

#[gpui_kit::test]
fn file_inspector_buttons_cancel_enter_and_mode_are_safe(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = file_fixture(cx, 1440.0);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("delete-selected").visible());
        assert!(window.find("delete-permanent-selected").visible());
        assert!(window.find("delete-permanent-selected").bounds().bottom() < px(760.0));
        window.click("delete-permanent-selected", cx);
        window.render_frame(cx);
        assert!(window.find("delete-warning").visible());
        assert!(window.find_by_label("彻底删除（不可撤销）").visible());
        assert_eq!(
            view.read(cx).pending_deletion.as_ref().unwrap().1.mode(),
            DeletionMode::Permanent
        );
        window.press("enter", cx);
        assert!(view.read(cx).deletion.is_none());
        assert!(window.has_active_dialog(cx));
        // A second operation cannot switch the visible confirmation's mode.
        view.update(cx, |app, cx| app.request_delete(id, window, cx));
        assert_eq!(
            view.read(cx).pending_deletion.as_ref().unwrap().1.mode(),
            DeletionMode::Permanent
        );
        window.click("delete-cancel", cx);
        assert!(view.read(cx).pending_deletion.is_none());
        window.click("delete-selected", cx);
        assert_eq!(
            view.read(cx).pending_deletion.as_ref().unwrap().1.mode(),
            DeletionMode::Trash
        );
        window.press("escape", cx);
        assert!(!window.has_active_dialog(cx));
        assert!(view.read(cx).pending_deletion.is_none());
        window.click("delete-permanent-selected", cx);
        window.render_frame(cx);
        window.within("dialog-close").click("close", cx);
        assert!(view.read(cx).pending_deletion.is_none());
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();
    assert!(temp.0.join("scan/folder-to-trash/content.txt").is_file());
}

#[gpui_kit::test]
fn files_expose_both_context_actions_at_minimum_width(cx: &mut TestAppContext) {
    let (_temp, handle, view, id) = file_fixture(cx, 1080.0);
    for mode in [DeletionMode::Trash, DeletionMode::Permanent] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.right_click(("row", id), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find_by_label(DELETE_LABEL).visible());
            assert!(window.find_by_label("彻底删除…").visible());
            let item = window.find_by_label(if mode == DeletionMode::Trash {
                DELETE_LABEL
            } else {
                "彻底删除…"
            });
            window.click(item.path().last().unwrap().clone(), cx);
            assert_eq!(
                view.read(cx).pending_deletion.as_ref().unwrap().1.mode(),
                mode
            );
            window.render_frame(cx);
            assert!(window.find("delete-confirm").bounds().bottom() < px(760.0));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn permanent_confirmation_is_single_use_and_refreshes_file_counts(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = file_fixture(cx, 1080.0);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.delete_operation = checked_file_operation;
            app.request_delete_mode(id, DeletionMode::Permanent, window, cx);
        });
        window.click("delete-confirm", cx);
        view.update(cx, |app, cx| {
            assert!(app.pending_deletion.is_none());
            assert!(app.deletion.is_some());
            app.confirm_delete(window, cx);
            let generation = app.generation;
            app.start(temp.0.clone(), window, cx);
            assert_eq!(app.generation, generation);
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_deletion(window, cx);
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            let scan = app.session.result.as_ref().unwrap();
            assert_eq!(
                scan.path_for(app.session.selected.unwrap()),
                temp.0.join("scan/folder-to-trash")
            );
            assert_eq!(scan.nodes[scan.root].file_count, 0);
            assert!(app.deletion_notice.as_ref().unwrap().contains("已彻底删除"));
        });
    })
    .unwrap();
    assert!(!temp.0.join("scan/folder-to-trash/content.txt").exists());
    assert!(temp.0.join("scan/folder-to-trash").is_dir());
}

#[gpui_kit::test]
fn file_trash_failure_never_changes_mode_or_removes_file(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = file_fixture(cx, 1080.0);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.delete_operation = |request| {
                assert_eq!(request.mode(), DeletionMode::Trash);
                Err("mock trash unavailable".into())
            };
            app.request_delete(id, window, cx);
        });
        window.click("delete-confirm", cx);
        view.update(cx, |app, cx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_deletion(window, cx);
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(app
                .deletion_notice
                .as_ref()
                .unwrap()
                .contains("mock trash unavailable"));
            assert_eq!(app.session.selected, Some(id));
            app.request_delete_mode(id, DeletionMode::Permanent, window, cx);
            assert_eq!(
                app.pending_deletion.as_ref().unwrap().1.mode(),
                DeletionMode::Permanent
            );
        });
        window.click("delete-cancel", cx);
    })
    .unwrap();
    assert!(temp.0.join("scan/folder-to-trash/content.txt").is_file());
}
#[gpui_kit::test]
fn file_trash_success_preserves_recoverable_content_and_refreshes(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = file_fixture(cx, 1080.0);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.delete_operation = checked_file_operation;
            app.request_delete(id, window, cx);
        });
        window.click("delete-confirm", cx);
        view.update(cx, |app, cx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_deletion(window, cx);
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            let scan = app.session.result.as_ref().unwrap();
            assert_eq!(scan.nodes[scan.root].file_count, 0);
            assert_eq!(
                scan.path_for(app.session.selected.unwrap()),
                temp.0.join("scan/folder-to-trash")
            );
            assert!(app.deletion_notice.as_ref().unwrap().contains(TRASH_NAME));
        });
    })
    .unwrap();
    assert_eq!(
        std::fs::read(temp.0.join("mock-file-trash")).unwrap(),
        b"synthetic"
    );
    assert!(!temp.0.join("scan/folder-to-trash/content.txt").exists());
}
#[gpui_kit::test]
fn folder_permanent_warning_cancel_and_recursive_confirmation(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = deletion_fixture(cx, 1440.0);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.session.select(id);
            app.delete_operation = |request| {
                assert_eq!(request.mode(), DeletionMode::Permanent);
                assert_eq!(request.path().file_name().unwrap(), "folder-to-trash");
                assert!(request
                    .path()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("tree-lens-ui-trash-"));
                request.execute()
            };
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.find("delete-permanent-selected").visible());
        window.click("delete-permanent-selected", cx);
        wait_for_deletion_preparation(&view, window, cx);
        window.render_frame(cx);
        assert!(window
            .find_by_label("彻底删除整个文件夹（不可撤销）")
            .visible());
        assert!(window.find("delete-confirm").bounds().bottom() < px(760.0));
        window.press("enter", cx);
        assert!(window.has_active_dialog(cx));
        assert!(view.read(cx).deletion.is_none());
        window.click("delete-cancel", cx);
        assert!(temp.0.join("scan/folder-to-trash/content.txt").is_file());
        window.click("delete-permanent-selected", cx);
        wait_for_deletion_preparation(&view, window, cx);
        window.click("delete-confirm", cx);
        view.update(cx, |app, cx| {
            assert!(app.pending_deletion.is_none());
            app.confirm_delete(window, cx);
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_deletion(window, cx);
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            let scan = app.session.result.as_ref().unwrap();
            assert_eq!(scan.nodes.len(), 1);
            assert_eq!(app.session.selected, Some(scan.root));
            assert!(app.deletion_notice.as_ref().unwrap().contains("已彻底删除"));
        });
    })
    .unwrap();
    assert!(!temp.0.join("scan/folder-to-trash").exists());
    assert!(temp.0.join("scan").is_dir());
}
fn wait_for_deletion_preparation(
    view: &Entity<TreeLensApp>,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) {
    view.update(cx, |app, cx| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.preparing_deletion.is_some() && Instant::now() < deadline {
            app.poll_deletion_preparation(window, cx);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.preparing_deletion.is_none());
    });
}

#[gpui_kit::test]
fn cancelling_folder_preparation_never_opens_confirmation_or_deletes(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = deletion_fixture(cx, 1080.0);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.request_delete_mode(id, DeletionMode::Permanent, window, cx)
        });
        assert!(view.read(cx).preparing_deletion.is_some());
        let generation = view.read(cx).generation;
        view.update(cx, |app, cx| app.start(temp.0.clone(), window, cx));
        assert_eq!(view.read(cx).generation, generation);
        window.press("escape", cx);
        assert!(view.read(cx).preparing_deletion.is_none());
        assert!(view.read(cx).pending_deletion.is_none());
        assert!(view.read(cx).deletion.is_none());
        std::thread::sleep(Duration::from_millis(20));
        view.update(cx, |app, cx| app.poll_deletion_preparation(window, cx));
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();
    assert!(temp.0.join("scan/folder-to-trash/content.txt").is_file());
}
#[gpui_kit::test]
fn deletion_keeps_list_mounted_and_preserves_viewport_and_navigation(cx: &mut TestAppContext) {
    for query in ["", "entry"] {
        let (temp, handle, view, _) = deletion_fixture(cx, 1440.0);
        let folder = temp.0.join("scan/folder-to-trash");
        for index in 0..80 {
            std::fs::write(folder.join(format!("entry-{index:03}.txt")), b"fixture").unwrap();
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let (anchor_before, rows_before, generation) = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |app, cx| {
                    app.start(temp.0.join("scan"), window, cx);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while app.busy() && Instant::now() < deadline {
                        app.poll_session(window, cx);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert!(!app.busy());
                    app.session.sort_by(SortBy::Name);
                    app.session.expand_one_level();
                    let directory = app.session.node_for_path(&folder).unwrap();
                    app.select_node(directory, cx);
                    let selected = app
                        .session
                        .node_for_path(&folder.join("entry-060.txt"))
                        .unwrap();
                    app.select_node(selected, cx);
                    app.search_input
                        .update(cx, |input, cx| input.set_value(query, window, cx));
                    app.session.set_query(query.into());
                    std::thread::sleep(Duration::from_millis(190));
                    app.poll_session(window, cx);
                });
                window.render_frame(cx);
                view.update(cx, |app, _| {
                    let state = app.scroll.0.borrow();
                    state
                        .base_handle
                        .set_offset(point(px(0.0), -px(40.0) * 40.0 - px(7.0)));
                });
                window.render_frame(cx);
                let before = view.update(cx, |app, _| {
                    let anchor = app.scroll_anchor().unwrap();
                    assert_eq!(anchor.1, 40, "anchor uses row height, not viewport height");
                    app.deletion = Some(receiver);
                    app.deletion_mode = Some(DeletionMode::Trash);
                    (anchor, app.session.rows.len(), app.generation)
                });
                window.render_frame(cx);
                assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
                assert!(window.find("map-root").visible());
                assert_eq!(view.read(cx).session.rows.len(), before.1);
                before
            })
            .unwrap();
        // Only change this test's own synthetic file, not the OS Trash.
        std::fs::remove_file(folder.join("content.txt")).unwrap();
        sender.send(Ok(())).unwrap();
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.poll_deletion(window, cx));
            assert!(view.read(cx).session.reconciling());
            assert_eq!(view.read(cx).session.rows.len(), rows_before);
            window.render_frame(cx);
            assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
            assert!(window.find("map-root").visible());
            let deadline = Instant::now() + Duration::from_secs(5);
            while view.read(cx).busy() && Instant::now() < deadline {
                view.update(cx, |app, cx| app.poll_session(window, cx));
                window.render_frame(cx);
                assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
                assert!(window.find("map-root").visible());
                assert!(view.read(cx).session.result.is_some());
                std::thread::sleep(Duration::from_millis(1));
            }
            let app = view.read(cx);
            assert!(!app.busy());
            let scan = app.session.result.as_ref().unwrap();
            assert_eq!(scan.nodes[scan.root].file_count, 80);
            assert_eq!(
                scan.path_for(app.session.selected.unwrap()),
                folder.join("entry-060.txt")
            );
            let directory = app.session.node_for_path(&folder).unwrap();
            assert_eq!(app.map_parent, Some(directory));
            assert!(app.session.expanded.contains(&directory));
            assert_eq!(app.session.sort, SortBy::Name);
            assert_eq!(app.session.query, query);
            assert_eq!(app.search_input.read(cx).value().as_ref(), query);
            assert_eq!(app.generation, generation.wrapping_add(1));
            let after = app.scroll_anchor().unwrap();
            assert_eq!(after.0, anchor_before.0, "same top pathname");
            assert_eq!(
                after.2.y + after.1 as f32 * after.3,
                anchor_before.2.y + anchor_before.1 as f32 * anchor_before.3,
                "same fractional pixel offset"
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn first_scan_keeps_search_typed_while_worker_is_running(cx: &mut TestAppContext) {
    let temp = TrashFixture::new();
    let (handle, view) = fixture(cx, 1080.0, false);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.start(temp.0.join("scan"), window, cx);
            assert!(!app.session.replacing());
            app.search_input
                .update(cx, |input, cx| input.set_value("content", window, cx));
            app.session.set_query("content".into());
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            assert_eq!(app.search_input.read(cx).value().as_ref(), "content");
            assert_eq!(app.session.query, "content");
            std::thread::sleep(Duration::from_millis(190));
            app.poll_session(window, cx);
            assert_eq!(app.session.applied_query(), "content");
            assert_eq!(app.session.rows.len(), 3);
        });
    })
    .unwrap();
}

#[gpui_kit::test]
fn switching_scan_roots_keeps_panels_until_atomic_success_or_retains_on_failure(
    cx: &mut TestAppContext,
) {
    for outcome in ["success", "error", "cancel"] {
        let (temp, handle, view, _) = deletion_fixture(cx, 1440.0);
        let old_root = temp.0.join("scan");
        let old_folder = old_root.join("folder-to-trash");
        for index in 0..60 {
            std::fs::write(old_folder.join(format!("entry-{index:03}.txt")), b"fixture").unwrap();
        }
        let new_root = temp.0.join("new-root");
        if outcome != "error" {
            std::fs::create_dir(&new_root).unwrap();
            std::fs::write(new_root.join("replacement-only.txt"), b"new fixture").unwrap();
        }
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| {
                app.start(old_root.clone(), window, cx);
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.busy() && Instant::now() < deadline {
                    app.poll_session(window, cx);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(!app.busy());
                app.session.sort_by(SortBy::Name);
                app.session.expand_one_level();
                let folder = app.session.node_for_path(&old_folder).unwrap();
                app.select_node(folder, cx);
                let selected = app
                    .session
                    .node_for_path(&old_folder.join("entry-040.txt"))
                    .unwrap();
                app.select_node(selected, cx);
                app.search_input
                    .update(cx, |input, cx| input.set_value("entry", window, cx));
                app.session.set_query("entry".into());
                std::thread::sleep(Duration::from_millis(190));
                app.poll_session(window, cx);
            });
            window.render_frame(cx);
            view.update(cx, |app, _| {
                app.scroll
                    .0
                    .borrow()
                    .base_handle
                    .set_offset(point(px(0.0), -px(503.0)));
            });
            window.render_frame(cx);
            let map_bounds = window.find("map-root").bounds();
            let (generation, revision, summary, selected, expanded, map_parent, scroll) = {
                let app = view.read(cx);
                (
                    app.generation,
                    app.session.revision(),
                    app.session.summary(),
                    app.session.selected,
                    app.session.expanded.clone(),
                    app.map_parent,
                    app.scroll.0.borrow().base_handle.offset(),
                )
            };
            view.update(cx, |app, cx| {
                app.start(new_root.clone(), window, cx);
                assert!(app.session.replacing());
                assert_eq!(app.session.result.as_ref().unwrap().root_path, old_root);
                assert_eq!(app.session.summary(), summary);
                assert_eq!(app.session.selected, selected);
                assert_eq!(app.session.expanded, expanded);
                assert_eq!(app.map_parent, map_parent);
                assert_eq!(app.session.query, "entry");
                assert!(!app.deletable_item(selected.unwrap(), DeletionMode::Permanent));
                app.request_delete(selected.unwrap(), window, cx);
                assert!(app.pending_deletion.is_none());
                app.start(temp.0.join("another-root"), window, cx);
                assert_eq!(
                    app.session.root_path,
                    Some(new_root.clone()),
                    "busy start is ignored"
                );
                assert_eq!(app.generation, generation.wrapping_add(1));
            });
            window.render_frame(cx);
            assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
            assert_eq!(
                window.find("map-root").bounds(),
                map_bounds,
                "no loading layout shift"
            );
            assert!(window
                .find_by_label(&format!("当前显示：{}", old_root.display()))
                .visible());
            assert!(window
                .find_by_label(&format!("正在扫描：{}", new_root.display()))
                .visible());
            if outcome == "cancel" {
                view.update(cx, |app, _| app.session.cancel());
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while view.read(cx).busy() && Instant::now() < deadline {
                view.update(cx, |app, cx| app.poll_session(window, cx));
                window.render_frame(cx);
                assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
                assert_eq!(window.find("map-root").bounds(), map_bounds);
                assert!(view.read(cx).session.result.is_some());
                std::thread::sleep(Duration::from_millis(1));
            }
            let app = view.read(cx);
            assert!(!app.busy());
            if outcome == "success" {
                let scan = app.session.result.as_ref().unwrap();
                assert_eq!(scan.root_path, new_root);
                assert_eq!(app.session.revision(), revision.wrapping_add(1));
                assert_eq!(app.generation, generation.wrapping_add(2));
                assert_eq!(app.session.selected, Some(scan.root));
                assert_eq!(app.map_parent, Some(scan.root));
                assert_eq!(app.session.summary().2, 1);
                assert!(app.session.query.is_empty());
                assert!(app.search_input.read(cx).value().is_empty());
                assert_eq!(app.scroll.0.borrow().base_handle.offset().y, px(0.0));
                assert_eq!(
                    app.path_input.read(cx).value().as_ref(),
                    new_root.to_str().unwrap()
                );
            } else {
                assert_eq!(app.session.result.as_ref().unwrap().root_path, old_root);
                assert_eq!(app.session.root_path, Some(old_root.clone()));
                assert_eq!(app.session.revision(), revision);
                assert_eq!(app.session.summary(), summary);
                assert_eq!(app.session.selected, selected);
                assert_eq!(app.session.expanded, expanded);
                assert_eq!(app.map_parent, map_parent);
                assert_eq!(app.session.query, "entry");
                assert_eq!(app.search_input.read(cx).value().as_ref(), "entry");
                assert_eq!(app.scroll.0.borrow().base_handle.offset(), scroll);
                assert_eq!(
                    app.path_input.read(cx).value().as_ref(),
                    old_root.to_str().unwrap()
                );
                assert!(app.session.status.contains("保留"));
                assert!(app.deletable_item(selected.unwrap(), DeletionMode::Trash));
            }
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn uncertain_deletion_reconciles_without_retry_or_empty_frame(cx: &mut TestAppContext) {
    for disconnect in [false, true] {
        let (temp, handle, view, id) = file_fixture(cx, 1080.0);
        let (sender, receiver) = mpsc::sync_channel(1);
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, _| {
                app.deletion = Some(receiver);
                app.deletion_mode = Some(DeletionMode::Trash);
                assert_eq!(app.session.selected, Some(id));
            });
            // A Trash API can report error/disconnect after changing disk.
            // Revalidate every outcome, but never retry the deletion.
            std::fs::remove_file(temp.0.join("scan/folder-to-trash/content.txt")).unwrap();
            if !disconnect {
                sender.send(Err("mock uncertain outcome".into())).unwrap();
            }
            drop(sender);
            view.update(cx, |app, cx| app.poll_deletion(window, cx));
            assert!(view.read(cx).session.reconciling());
            window.render_frame(cx);
            assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
            assert!(window.find("map-root").visible());
            let deadline = Instant::now() + Duration::from_secs(5);
            while view.read(cx).busy() && Instant::now() < deadline {
                view.update(cx, |app, cx| app.poll_session(window, cx));
                window.render_frame(cx);
                assert!(!window.find_all_by_role(gpui_kit::Role::TreeItem).is_empty());
                assert!(window.find("map-root").visible());
                std::thread::sleep(Duration::from_millis(1));
            }
            let app = view.read(cx);
            assert!(!app.busy());
            assert_eq!(app.session.summary().2, 0);
            assert!(app
                .deletion_notice
                .as_ref()
                .unwrap()
                .contains("不会自动重试"));
            assert!(app.pending_deletion.is_none());
            assert!(app.deletion.is_none());
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn partial_permanent_failure_refreshes_and_does_not_retry(cx: &mut TestAppContext) {
    let (temp, handle, view, id) = deletion_fixture(cx, 1080.0);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            app.delete_operation = |request| {
                assert_eq!(request.mode(), DeletionMode::Permanent);
                assert_eq!(request.path().file_name().unwrap(), "folder-to-trash");
                assert!(request
                    .path()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("tree-lens-ui-trash-"));
                // Only simulate a partial failure inside this synthetic fixture.
                std::fs::remove_file(request.path().join("content.txt")).unwrap();
                Err("mock partial failure: 部分项目可能已删除".into())
            };
            app.request_delete_mode(id, DeletionMode::Permanent, window, cx);
        });
        wait_for_deletion_preparation(&view, window, cx);
        window.click("delete-confirm", cx);
        view.update(cx, |app, cx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_deletion(window, cx);
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            let scan = app.session.result.as_ref().unwrap();
            assert_eq!(scan.nodes[scan.root].file_count, 0);
            assert_eq!(scan.nodes.len(), 2);
            assert_eq!(
                scan.path_for(app.session.selected.unwrap()),
                temp.0.join("scan/folder-to-trash")
            );
            assert!(app
                .deletion_notice
                .as_ref()
                .unwrap()
                .contains("mock partial failure"));
            assert!(app.pending_deletion.is_none());
            app.confirm_delete(window, cx);
            assert!(app.deletion.is_none());
        });
    })
    .unwrap();
    assert!(temp.0.join("scan/folder-to-trash").is_dir());
    assert!(!temp.0.join("scan/folder-to-trash/content.txt").exists());
}
