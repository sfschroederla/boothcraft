use boothcraft_ui::BoothcraftApp;

/// The plugin formats this build can host.
#[cfg(feature = "plugins")]
fn plugin_formats() -> Vec<Box<dyn boothcraft_plugin::PluginFormat>> {
    #[allow(unused_mut)]
    let mut formats: Vec<Box<dyn boothcraft_plugin::PluginFormat>> =
        vec![Box::new(boothcraft_plugin_vst3::Vst3Format::new())];
    #[cfg(feature = "mock-plugins")]
    if std::env::var("BOOTHCRAFT_PLUGINS").is_ok_and(|v| v == "mock") {
        formats.push(Box::new(boothcraft_plugin::mock::MockFormat::new()));
    }
    formats
}

fn main() -> eframe::Result {
    #[cfg(feature = "plugins")]
    let formats = {
        let formats = plugin_formats();
        // Hidden mode: when started as `--plugin-scan-worker <format> <path>` this probes one
        // plugin file and exits. Must come before anything treats arguments as file paths.
        boothcraft_plugin::maybe_run_scan_worker(&formats);
        formats
    };
    #[cfg(not(feature = "plugins"))]
    let formats = Vec::new();

    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: egui_viewport(),
        ..Default::default()
    };
    eframe::run_native(
        "Boothcraft",
        options,
        Box::new(move |cc| Ok(Box::new(BoothcraftApp::new(cc, paths, formats)))),
    )
}

fn egui_viewport() -> eframe::egui::ViewportBuilder {
    eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 760.0])
        .with_title("Boothcraft")
        // Fixes the folder eframe persists window geometry to, independent of the window title.
        .with_app_id("com.schroeds.boothcraft")
}
