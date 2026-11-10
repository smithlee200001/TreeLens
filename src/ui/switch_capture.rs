//! Optional real-renderer check of the first frame after changing scan roots.
use super::{install_theme, TreeLensApp};
use gpui_kit::{
    component::{Theme, ThemeMode},
    px, size,
    test::{TestQueryExt, TestWindowExt},
    AppContext, Bounds, HeadlessAppContext, Role, WindowBounds, WindowOptions,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires an available GPU or software Vulkan renderer"]
fn directory_switch_renders_target_before_worker_poll() {
    let fixture = super::tests::TrashFixture::new();
    let before = fixture.0.join("scan");
    let target = fixture.0.join("new-directory");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("new-file.txt"), b"synthetic only").unwrap();
    let output = std::env::var_os("TREE_LENS_SWITCH_CAPTURE_DIR").map(PathBuf::from);
    if let Some(output) = &output {
        std::fs::create_dir_all(output).unwrap();
    }
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    let (handle, view) = cx.update(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        install_theme(cx);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Default::default(),
                    size: size(px(1440.), px(920.)),
                })),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| TreeLensApp::new(Some(before.clone()), window, cx)),
        )
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |app, cx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            assert_eq!(app.session.result.as_ref().unwrap().root_path, before);
            app.start(target.clone(), window, cx);
        });
        // No worker result has been polled for the target. The immediate frame
        // must already show its loading state and must contain no previous rows.
        window.render_frame(cx);
        assert!(window.find_all_by_role(Role::TreeItem).is_empty());
        assert!(window
            .find_by_label(&format!("正在扫描：{}", target.display()))
            .visible());
        assert!(view.read(cx).session.result.is_none());
        let loading = window.render_to_image().expect("offscreen renderer");
        assert!(loading.width() > 0 && loading.height() > 0);
        if let Some(output) = &output {
            loading
                .save(output.join("immediate-switch-loading.png"))
                .unwrap();
        }
        view.update(cx, |app, cx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy() && Instant::now() < deadline {
                app.poll_session(window, cx);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.busy());
            assert_eq!(app.session.result.as_ref().unwrap().root_path, target);
        });
        window.render_frame(cx);
        assert!(!window.find_all_by_role(Role::TreeItem).is_empty());
        let loaded = window.render_to_image().expect("offscreen renderer");
        if let Some(output) = &output {
            loaded
                .save(output.join("immediate-switch-loaded.png"))
                .unwrap();
        }
    })
    .unwrap();
}
