//! Crash marker: which plugins were loaded in this session.
//!
//! `inflight.json` is written when the first plugin loads and removed on a clean exit. If it is
//! still there at the next start, the app died with those plugins loaded, and offers to disable
//! them.

use crate::settings::write_atomic;
use boothcraft_dsp::PluginRef;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
struct Marker {
    pid: u32,
    plugins: Vec<PluginRef>,
}

pub struct Inflight {
    path: PathBuf,
    marker: Marker,
}

impl Inflight {
    /// Start a session. Returns the marker plus the plugins loaded when the previous session
    /// ended unexpectedly (empty after a clean exit).
    pub fn start(dir: &Path) -> (Inflight, Vec<PluginRef>) {
        let path = dir.join("inflight.json");
        let previous = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Marker>(&b).ok())
            .filter(|m| m.pid != std::process::id())
            .map(|m| m.plugins)
            .unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        (
            Inflight {
                path,
                marker: Marker {
                    pid: std::process::id(),
                    plugins: Vec::new(),
                },
            },
            previous,
        )
    }

    /// Record that `plugin` is about to be loaded. Written before the load, since a crash inside
    /// the plugin is exactly what this is for.
    pub fn note_loading(&mut self, plugin: &PluginRef) {
        if self.marker.plugins.contains(plugin) {
            return;
        }
        self.marker.plugins.push(plugin.clone());
        if let Ok(bytes) = serde_json::to_vec(&self.marker) {
            let _ = write_atomic(&self.path, &bytes);
        }
    }

    /// Clean exit.
    pub fn finish(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str) -> PluginRef {
        PluginRef {
            format: "vst3".into(),
            uid: name.into(),
            name: name.into(),
            vendor: "V".into(),
        }
    }

    #[test]
    fn unclean_exit_is_reported_once() {
        let dir = std::env::temp_dir().join(format!("bc-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (mut s1, previous) = Inflight::start(&dir);
        assert!(previous.is_empty());
        s1.note_loading(&plugin("a"));
        s1.note_loading(&plugin("a"));
        s1.note_loading(&plugin("b"));
        // The marker is on disk before the session ends, so a crash leaves it behind.
        assert!(dir.join("inflight.json").exists());

        // Simulate the next launch after a crash: another process id wrote it.
        let text = std::fs::read_to_string(dir.join("inflight.json")).unwrap();
        let pid = std::process::id();
        std::fs::write(
            dir.join("inflight.json"),
            text.replace(&format!("\"pid\":{pid}"), "\"pid\":1"),
        )
        .unwrap();
        let (s2, previous) = Inflight::start(&dir);
        assert_eq!(previous, vec![plugin("a"), plugin("b")]);
        s2.finish();

        // A clean exit leaves nothing behind.
        let (mut s3, previous) = Inflight::start(&dir);
        assert!(previous.is_empty());
        s3.note_loading(&plugin("c"));
        s3.finish();
        let (_s4, previous) = Inflight::start(&dir);
        assert!(previous.is_empty());
        drop(s1);
        std::fs::remove_dir_all(dir).ok();
    }
}
