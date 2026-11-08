//! Real offscreen GPUI rendering, not a browser mockup. Requires a working GPU
//! or software graphics driver. Usage: cargo run --features ui-tests --example
//! ui_snapshot -- /path/to/scan output.png [width height]
#[path = "../src/app.rs"]
mod app;

use gpui_kit::{
    component::{Theme, ThemeMode},
    test::TestWindowExt,
    *,
};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let scan = args.next().map(PathBuf::from);
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| "tree-lens.png".into());
    let width = args
        .next()
        .and_then(|s| s.to_str()?.parse::<f32>().ok())
        .unwrap_or(1440.0);
    let height = args
        .next()
        .and_then(|s| s.to_str()?.parse::<f32>().ok())
        .unwrap_or(920.0);
    let deletion_preview = args.next().is_some_and(|flag| flag == "--delete-dialog");
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        app::install_theme(cx);
    });
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Default::default(),
                    size: size(px(width), px(height)),
                })),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| app::TreeLensApp::new(scan, window, cx)),
        )
    })?;
    // The scanner is a real OS thread; the GPUI test clock is deterministic.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(50));
        cx.advance_clock(Duration::from_millis(80));
        cx.run_until_parked();
    }
    if deletion_preview {
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.preview_delete_confirmation(window, cx));
        })?;
        cx.advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))?;
    cx.capture_screenshot(handle)?.save(&output)?;
    println!("Saved real GPUI offscreen render: {}", output.display());
    Ok(())
}
