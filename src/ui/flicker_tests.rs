//! Animated-mode regressions: modal safety, retained layout, and actual pixels.
use super::{install_theme, TreeLensApp};
use crate::features::delete::DeletionMode;
use gpui_kit::{
    component::WindowExt,
    point, px, size,
    test::{TestQueryExt, TestWindowExt},
    AnyWindowHandle, App, AppContext, Bounds, ElementId, Entity, HeadlessAppContext, InputEvent,
    Role, ScrollDelta, ScrollEvent, TestAppContext, Window, WindowBounds, WindowOptions,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tree-lens-flicker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("synthetic.txt"), b"fixture only").unwrap();
        for index in 0..40 {
            std::fs::write(root.join(format!("filler-{index:02}.txt")), b"fixture only").unwrap();
        }
        // macOS temp_dir can begin with /var, an alias of /private/var.
        // Use the physical fixture path, as the deletion safety tests do, so
        // the real symlink-ancestor guard is not bypassed or tripped by setup.
        Self(std::fs::canonicalize(root).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn open(cx: &mut App, fixture: &Fixture) -> (AnyWindowHandle, Entity<TreeLensApp>) {
    gpui_kit::init(cx);
    install_theme(cx);
    // Test helpers often reduce motion. This regression must exercise real animations.
    cx.set_reduce_motion(false);
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: size(px(1440.), px(760.)),
            })),
            focus: false,
            show: false,
            ..Default::default()
        },
        cx,
        |window, cx| {
            cx.new(|cx| {
                let mut app = TreeLensApp::new(Some(fixture.0.clone()), window, cx);
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.session.busy() && Instant::now() < deadline {
                    app.poll_session(window, cx);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(!app.session.busy());
                app.reset_projection();
                // No filesystem mutation, no native Trash invocation, no external data.
                app.delete_operation = |_| Ok(());
                app
            })
        },
    )
    .unwrap()
}
fn settled(window: &mut Window, cx: &mut App) {
    std::thread::sleep(Duration::from_millis(350));
    for _ in 0..4 {
        window.simulate_next_frame(cx);
        window.render_frame(cx);
    }
    assert_eq!(
        window.simulate_next_frame(cx),
        0,
        "retained content must not schedule animation frames after settling"
    );
}
fn lifecycle(
    view: &Entity<TreeLensApp>,
    window: &mut Window,
    cx: &mut App,
    mut capture: impl FnMut(&str, &mut Window, &mut App),
) {
    assert!(!cx.reduce_motion());
    let id = view
        .read(cx)
        .session
        .result
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.name.as_ref() == std::ffi::OsStr::new("synthetic.txt"))
        .unwrap()
        .id;
    view.update(cx, |app, cx| app.select_node(id, cx));
    window.render_frame(cx);
    settled(window, cx);
    let root_bounds = window.find("displayed-root").bounds();
    let scan_bounds = window.find("scan-path").bounds();
    let refresh_bounds = window.find("refresh").bounds();
    capture("idle", window, cx);
    view.update(cx, |app, cx| {
        app.request_delete_mode(id, DeletionMode::Permanent, window, cx)
    });
    window.render_frame(cx);
    assert!(
        window.has_active_dialog(cx),
        "confirmation did not open: {}",
        view.read(cx).session.status
    );
    assert!(window.find("delete-confirm").visible());
    capture("open", window, cx);
    // A transparent overlay must still block pointer dispatch to background controls.
    let generation = view.read(cx).generation;
    window.click("refresh", cx);
    assert_eq!(view.read(cx).generation, generation);
    assert!(window.has_active_dialog(cx));
    // Unlike Refresh, row selection/scrolling has no dialog guard. Exercise
    // an exposed point to the left of the dialog to prove the modal shield.
    let root = view.read(cx).session.result.as_ref().unwrap().root;
    let exposed = window.find(("row", root)).bounds().origin + point(px(20.), px(20.));
    let scroll_before = view.read(cx).scroll.0.borrow().base_handle.offset();
    window.click_at(("row", root), point(px(20.), px(20.)), cx);
    assert_eq!(view.read(cx).session.selected, Some(id));
    window.dispatch_event(
        ScrollEvent {
            position: exposed,
            delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
            ..Default::default()
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    assert_eq!(
        view.read(cx).scroll.0.borrow().base_handle.offset(),
        scroll_before
    );
    for key in ["tab", "shift-tab"] {
        for _ in 0..8 {
            window.press(key, cx);
            let buttons = window.find_all_by_role(Role::Button);
            let focused: Vec<_> = buttons
                .iter()
                .filter(|button| button.focused() == Some(true))
                .collect();
            assert!(!focused.is_empty(), "Tab must focus a dialog control");
            for button in focused {
                assert!(
                    button.path().contains(&ElementId::from("dialog")),
                    "focus escaped modal: {button:?}"
                );
            }
        }
    }
    window.press("enter", cx);
    assert!(
        view.read(cx).deletion.is_none(),
        "Enter must never confirm deletion"
    );
    window.click("delete-cancel", cx);
    window.render_frame(cx);
    assert!(!window.has_active_dialog(cx));
    assert!(
        window.try_find("delete-confirm").is_none(),
        "no fading dialog surface after cancel"
    );
    capture("cancel", window, cx);
    view.update(cx, |app, cx| {
        app.request_delete_mode(id, DeletionMode::Permanent, window, cx)
    });
    window.render_frame(cx);
    window.click("delete-confirm", cx);
    window.render_frame(cx);
    assert!(view.read(cx).deletion.is_some());
    assert!(view.read(cx).pending_deletion.is_none());
    assert!(!window.has_active_dialog(cx));
    assert!(
        window.try_find("delete-confirm").is_none(),
        "no fading dialog surface after confirm"
    );
    assert_eq!(window.find("displayed-root").bounds(), root_bounds);
    assert_eq!(window.find("scan-path").bounds(), scan_bounds);
    assert_eq!(
        window.find("refresh").bounds(),
        refresh_bounds,
        "pending deletion must preserve refresh bounds"
    );
    capture("confirm-pending", window, cx);
    settled(window, cx);
    capture("pending-settled", window, cx);
    let deadline = Instant::now() + Duration::from_secs(5);
    while view.read(cx).deletion.is_some() && Instant::now() < deadline {
        view.update(cx, |app, cx| app.poll_deletion(window, cx));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(view.read(cx).session.reconciling());
    window.render_frame(cx);
    assert_eq!(
        window.find("cancel").bounds(),
        refresh_bounds,
        "refresh/cancel must occupy identical bounds"
    );
    capture("reconcile", window, cx);
    settled(window, cx);
    while view.read(cx).session.busy() && Instant::now() < deadline {
        view.update(cx, |app, cx| app.poll_session(window, cx));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!view.read(cx).busy());
    window.render_frame(cx);
    assert_eq!(window.find("displayed-root").bounds(), root_bounds);
    assert_eq!(window.find("scan-path").bounds(), scan_bounds);
    assert_eq!(window.find("refresh").bounds(), refresh_bounds);
    capture("done", window, cx);
}

#[gpui_kit::test]
fn animated_deletion_has_no_lingering_surface_or_busy_animation(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (handle, view) = cx.update(|cx| open(cx, &fixture));
    cx.update_window(handle, |_, window, cx| {
        lifecycle(&view, window, cx, |_, _, _| {})
    })
    .unwrap();
}

/// Requires a real offscreen renderer (Metal on macOS, software Vulkan is suitable on Linux).
/// Run explicitly: cargo test --features ui-tests deletion_text_pixels -- --ignored --nocapture
#[test]
#[ignore = "requires an available GPU or software Vulkan renderer"]
fn deletion_text_pixels_are_identical_across_frames() {
    let fixture = Fixture::new();
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    let (handle, view) = cx.update(|cx| open(cx, &fixture));
    cx.run_until_parked();
    let mut baseline: Option<Vec<u8>> = None;
    cx.update_window(handle, |_, window, cx| {
        lifecycle(&view, window, cx, |stage, window, cx| {
            window.render_frame(cx);
            let frame = window
                .render_to_image()
                .expect("offscreen renderer must be available");
            // Entire title/header and sidebar, deliberately excluding the status bar,
            // selection detail and changed tree rows. The 600px centered dialog does
            // not cover either region. Comparing actual RGBA catches text dimming,
            // disappearance, glyph changes and displacement, not just node bounds.
            let scale = frame.width() as f32 / 1440.;
            let mut pixels = Vec::new();
            for (x0, y0, x1, y1) in [(0., 0., 1440., 110.), (0., 110., 165., 680.)] {
                for y in (y0 * scale) as u32..(y1 * scale) as u32 {
                    for x in (x0 * scale) as u32..(x1 * scale) as u32 {
                        pixels.extend_from_slice(&frame.get_pixel(x, y).0);
                    }
                }
            }
            if let Some(expected) = &baseline {
                let changed = pixels
                    .iter()
                    .zip(expected.iter())
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(changed, 0, "unchanged title/sidebar RGBA bytes at {stage}");
            } else {
                baseline = Some(pixels);
            }
        });
    })
    .unwrap();
}
