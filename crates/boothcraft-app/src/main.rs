use boothcraft_ui::BoothcraftApp;

fn main() -> eframe::Result {
    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: egui_viewport(),
        ..Default::default()
    };
    eframe::run_native(
        "Boothcraft",
        options,
        Box::new(move |cc| Ok(Box::new(BoothcraftApp::new(cc, paths)))),
    )
}

fn egui_viewport() -> eframe::egui::ViewportBuilder {
    eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 760.0])
        .with_title("Boothcraft")
        // Fixes the folder eframe persists window geometry to, independent of the window title.
        .with_app_id("com.schroeds.boothcraft")
}
