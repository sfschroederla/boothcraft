//! Plugin settings and file locations. Kept next to the presets, under the config directory, so
//! the CLI and the app share them (unlike eframe's own `app.ron`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `…/Boothcraft/plugins`, or `None` if the platform has no config directory.
pub fn default_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Boothcraft").map(|d| d.config_dir().join("plugins"))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    /// Whether plugins are loaded at all (the Preferences opt-in).
    pub enabled: bool,
    /// Folders searched in addition to each format's standard locations.
    pub extra_paths: Vec<PathBuf>,
    /// Effect ids (`vst3:…`) the user or the crash prompt turned off.
    pub disabled: BTreeSet<String>,
}

impl PluginSettings {
    pub fn load(dir: &Path) -> PluginSettings {
        std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        write_atomic(
            &dir.join("settings.json"),
            &serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?,
        )
    }
}

/// Write through a temp file and rename, so a crash never leaves half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
