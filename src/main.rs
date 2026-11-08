mod app;

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

fn main() {
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    application().with_assets(assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        app::install_theme(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1440.0), px(920.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(1080.0), px(760.0))),
            app_id: Some("local.treelens.app".into()),
            ..TitleBar::window_options()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, move |window, cx| {
            window.set_window_title("Tree Lens · 磁盘空间分析");
            cx.new(|cx| app::TreeLensApp::new(initial_path, window, cx))
        }) {
            eprintln!("无法打开 Tree Lens：{error}");
            cx.quit();
        }
        cx.activate(true);
    });
}
