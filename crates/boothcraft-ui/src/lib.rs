//! egui front end for Boothcraft.

mod app;
mod effects_ui;
mod generate_ui;
mod icons;
mod loudness_ui;
mod markers_panel;
mod meters;
#[cfg(target_os = "macos")]
mod native_menu;
mod ops;
mod param_ui;
mod plugin_manager_ui;
mod scale;
mod selview_panel;
mod spectral;
mod view;
mod waveform;

pub use app::BoothcraftApp;
