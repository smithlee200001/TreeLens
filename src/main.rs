#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--version")) {
        println!("Tree Lens {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("--help")) {
        println!("Tree Lens {}\nUsage: tree-lens [DIRECTORY]\n       tree-lens --version\n\nNative disk usage viewer. A desktop session and graphics driver are required.\nScanning is read-only; moving a folder to Trash requires confirmation in the GUI.", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.next().is_some() {
        eprintln!("Only one directory may be opened. Usage: tree-lens [DIRECTORY]");
        std::process::exit(2);
    }
    let initial_path = first.map(std::path::PathBuf::from);
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
