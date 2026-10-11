//! The scan cache and blocklist, `cache.json`.
//!
//! An entry is keyed by format and path and valid while the file's modification time and size are
//! unchanged, so unchanged plugins are never re-probed. A plugin that crashed its scan, or timed
//! out twice, is blocked until the file changes or the user unblocks it.

use crate::settings::write_atomic;
use crate::traits::PluginDescriptor;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockReason {
    /// The scan worker died (a crash or abort) while probing.
    Crash(String),
    /// Probing timed out, and again when retried alone with a longer limit.
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Ok(Vec<PluginDescriptor>),
    /// Loaded but has no usable effects, or reported an error. Not blocked; just not listed.
    Invalid(String),
    Blocked(BlockReason),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub mtime_ns: u128,
    pub size: u64,
}

impl Fingerprint {
    pub fn of(path: &Path) -> Option<Fingerprint> {
        let m = std::fs::metadata(path).ok()?;
        let mtime_ns = m
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(Fingerprint {
            mtime_ns,
            size: m.len(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub fingerprint: Fingerprint,
    pub outcome: Outcome,
}

/// Bumped when scanning changes in a way that makes old results untrustworthy. Version 2: a
/// worker that crashed while *exiting* (after answering) used to be recorded as blocked.
pub const CACHE_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    #[serde(default)]
    pub version: u32,
    pub entries: BTreeMap<String, CacheEntry>,
}

impl Default for ScanCache {
    fn default() -> Self {
        ScanCache {
            version: CACHE_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

pub fn key(format: &str, path: &Path) -> String {
    format!("{format}|{}", path.display())
}

impl ScanCache {
    pub fn load(dir: &Path) -> ScanCache {
        let mut cache: ScanCache = std::fs::read(dir.join("cache.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if cache.version < CACHE_VERSION {
            // Blocks recorded by an older scanner may be false: probe those files again.
            cache
                .entries
                .retain(|_, e| !matches!(e.outcome, Outcome::Blocked(_)));
            cache.version = CACHE_VERSION;
        }
        cache
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        write_atomic(
            &dir.join("cache.json"),
            &serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// The entry for `path`, if the file is unchanged since it was recorded.
    pub fn fresh(&self, format: &str, path: &Path) -> Option<&CacheEntry> {
        let e = self.entries.get(&key(format, path))?;
        (Fingerprint::of(path).as_ref() == Some(&e.fingerprint)).then_some(e)
    }

    pub fn record(&mut self, format: &str, path: &Path, outcome: Outcome) {
        if let Some(fingerprint) = Fingerprint::of(path) {
            self.entries.insert(
                key(format, path),
                CacheEntry {
                    fingerprint,
                    outcome,
                },
            );
        }
    }

    /// Everything blocked, for the Plugin Manager's list.
    pub fn blocked(&self) -> Vec<(PathBuf, &BlockReason)> {
        self.entries
            .iter()
            .filter_map(|(k, e)| match &e.outcome {
                Outcome::Blocked(r) => Some((PathBuf::from(k.split_once('|')?.1), r)),
                _ => None,
            })
            .collect()
    }

    /// Forget a block so the next scan probes the file again.
    pub fn unblock(&mut self, path: &Path) {
        self.entries.retain(|k, e| {
            !(matches!(e.outcome, Outcome::Blocked(_))
                && k.ends_with(&format!("|{}", path.display())))
        });
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &PluginDescriptor> {
        self.entries.values().flat_map(|e| match &e.outcome {
            Outcome::Ok(d) => d.as_slice(),
            _ => &[],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_from_an_older_scanner_are_dropped_on_load() {
        let dir = std::env::temp_dir().join(format!("bc-cache-ver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = r#"{"entries":{
            "vst3|/a.vst3":{"fingerprint":{"mtime_ns":1,"size":2},"outcome":{"Blocked":{"Crash":"x"}}},
            "vst3|/b.vst3":{"fingerprint":{"mtime_ns":1,"size":2},"outcome":{"Invalid":"no effects"}}}}"#;
        std::fs::write(dir.join("cache.json"), old).unwrap();
        let c = ScanCache::load(&dir);
        assert_eq!(c.version, CACHE_VERSION);
        assert!(c.blocked().is_empty(), "the old block is forgotten");
        assert_eq!(c.entries.len(), 1, "other results are kept");
        std::fs::remove_dir_all(dir).ok();
    }
}
