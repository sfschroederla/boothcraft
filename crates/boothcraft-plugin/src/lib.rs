//! Hosting third-party audio plugins, independent of the plugin format.
//!
//! Formats (VST3, AudioUnit, later CLAP) implement [`PluginFormat`] and [`PluginInstance`] in their
//! own crates. This crate owns everything format-neutral: the rack effect that wraps a plugin
//! ([`PluginEffect`]), the main-thread [`PluginManager`], scanning in child processes, the scan
//! cache and blocklist, settings, and the crash marker.

pub mod cache;
pub mod catalog;
pub mod crash;
pub mod effect;
pub mod error;
pub mod main_thread;
pub mod manager;
#[cfg(feature = "mock")]
pub mod mock;
pub mod pending;
pub mod scan;
pub mod settings;
pub mod traits;

pub use cache::{BlockReason, Outcome, ScanCache};
pub use catalog::Catalog;
pub use effect::PluginEffect;
pub use error::HostError;
pub use main_thread::MainThread;
pub use manager::{PluginManager, Pump};
pub use pending::Pending;
pub use scan::{maybe_run_scan_worker, ScanConfig, ScanReport, WorkerCommand};
pub use settings::PluginSettings;
pub use traits::*;
