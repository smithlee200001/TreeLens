mod app;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title("Tree Lens · 磁盘空间分析"),
        ..Default::default()
    };
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    eframe::run_native(
        "Tree Lens",
        options,
        Box::new(move |cc| Ok(Box::new(app::TreeLensApp::new(cc, initial_path)))),
    )
}
