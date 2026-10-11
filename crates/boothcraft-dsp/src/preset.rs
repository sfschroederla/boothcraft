//! Effect and rack presets, stored as JSON in the user's config directory.
//!
//! Effect presets key values by parameter id, so they survive parameters being added or reordered.

use crate::param::ParamSpec;
use crate::plugin_ref::{PluginRef, PluginState};
use crate::rack::RackDesc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub values: BTreeMap<String, f32>,
    /// Plugin presets: the plugin's own saved state, which is authoritative over `values`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PluginState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginRef>,
}

impl Preset {
    pub fn capture(name: &str, specs: &[ParamSpec], values: &[f32]) -> Preset {
        Preset {
            name: name.to_string(),
            values: specs
                .iter()
                .zip(values)
                .map(|(s, v)| (s.id.to_string(), *v))
                .collect(),
            state: None,
            plugin: None,
        }
    }

    /// Values in `specs` order; parameters the preset does not mention keep their defaults.
    pub fn values_for(&self, specs: &[ParamSpec]) -> Vec<f32> {
        specs
            .iter()
            .map(|s| {
                self.values
                    .get(s.id)
                    .copied()
                    .map_or(s.default, |v| s.clamp(v))
            })
            .collect()
    }
}

/// `…/Boothcraft/presets`, or `None` if the platform has no config directory.
pub fn root() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Boothcraft").map(|d| d.config_dir().join("presets"))
}

/// A name safe as a file or folder name on every platform. Also maps effect ids such as
/// `vst3:ABCD`, whose colon is invalid in Windows paths.
pub fn dir_for(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Presets saved for `effect_id` under `dir`, sorted by name.
pub fn load(dir: &Path, effect_id: &str) -> Vec<Preset> {
    let mut out: Vec<Preset> = std::fs::read_dir(dir.join(dir_for(effect_id)))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn save(dir: &Path, effect_id: &str, preset: &Preset) -> std::io::Result<()> {
    let d = dir.join(dir_for(effect_id));
    std::fs::create_dir_all(&d)?;
    let json = serde_json::to_vec_pretty(preset).map_err(std::io::Error::other)?;
    std::fs::write(d.join(format!("{}.json", dir_for(&preset.name))), json)
}

pub fn delete(dir: &Path, effect_id: &str, name: &str) -> std::io::Result<()> {
    std::fs::remove_file(
        dir.join(dir_for(effect_id))
            .join(format!("{}.json", dir_for(name))),
    )
}

pub fn save_rack(path: &Path, rack: &RackDesc) -> std::io::Result<()> {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(rack).map_err(std::io::Error::other)?,
    )
}

pub fn load_rack(path: &Path) -> std::io::Result<RackDesc> {
    serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::Unit;

    static SPECS: [ParamSpec; 2] = [
        ParamSpec::new("a", "A", Unit::Db, -10.0, 10.0, 0.0),
        ParamSpec::new("b", "B", Unit::Db, -10.0, 10.0, 1.0),
    ];

    #[test]
    fn save_load_delete() {
        let dir = std::env::temp_dir().join(format!("bc-presets-{}", std::process::id()));
        let p = Preset::capture("Loud / safe?", &SPECS, &[5.0, -2.0]);
        save(&dir, "amplify", &p).unwrap();
        let loaded = load(&dir, "amplify");
        assert_eq!(loaded, vec![p.clone()]);
        assert_eq!(loaded[0].values_for(&SPECS), vec![5.0, -2.0]);
        delete(&dir, "amplify", &p.name).unwrap();
        assert!(load(&dir, "amplify").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plugin_ids_get_portable_folder_names() {
        assert_eq!(dir_for("vst3:ABCD"), "vst3_ABCD");
        let dir = std::env::temp_dir().join(format!("bc-presets-plug-{}", std::process::id()));
        let mut p = Preset::capture("Warm", &SPECS, &[1.0, 2.0]);
        p.state = Some(PluginState(vec![1, 2, 3]));
        save(&dir, "vst3:ABCD", &p).unwrap();
        assert_eq!(load(&dir, "vst3:ABCD"), vec![p]);
        assert!(dir.join("vst3_ABCD").is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_and_out_of_range_values() {
        let mut p = Preset::capture("x", &SPECS[..1], &[50.0]);
        p.values.insert("gone".into(), 3.0);
        assert_eq!(
            p.values_for(&SPECS),
            vec![10.0, 1.0],
            "clamped; unknown ignored; missing default"
        );
    }

    #[test]
    fn rack_round_trip() {
        let path = std::env::temp_dir().join(format!("bc-rack-{}.json", std::process::id()));
        let mut r = RackDesc::single("amplify").unwrap();
        r.mix = 0.5;
        save_rack(&path, &r).unwrap();
        assert_eq!(load_rack(&path).unwrap(), r);
        std::fs::remove_file(path).ok();
    }
}
