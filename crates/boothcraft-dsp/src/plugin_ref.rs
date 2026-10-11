//! What a rack slot remembers about a hosted plugin. `dsp` never loads plugins; it only stores
//! enough to find one again and to keep its saved state intact when the plugin is missing.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Identifies a plugin across sessions and machines.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRef {
    /// "vst3", "au", later "clap".
    pub format: String,
    /// Format-specific unique id: a VST3 class id as 32 hex digits, or an AU `type-subtype-manu`.
    pub uid: String,
    /// Display name and vendor, kept so a missing plugin can still be named in the UI.
    pub name: String,
    pub vendor: String,
}

impl PluginRef {
    /// The registry-style effect id, e.g. `vst3:5653544147...`.
    pub fn effect_id(&self) -> String {
        format!("{}:{}", self.format, self.uid)
    }
}

/// A plugin's opaque saved state. Serialised as base64 so rack files stay JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginState(pub Vec<u8>);

impl Serialize for PluginState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for PluginState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map(PluginState)
            .map_err(serde::de::Error::custom)
    }
}

/// Identity of one rack slot's effect instance. It lets a rebuild keep a live plugin instance
/// instead of re-instantiating it. Never saved: each loaded or cloned-fresh slot gets a new one,
/// while `Clone` keeps it (the clone is the same slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(pub u64);

impl SlotId {
    pub fn fresh() -> SlotId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        SlotId(NEXT.fetch_add(1, Relaxed))
    }
}

impl Default for SlotId {
    fn default() -> SlotId {
        SlotId::fresh()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_byte_for_byte() {
        let bytes: Vec<u8> = (0..=255).collect();
        let json = serde_json::to_string(&PluginState(bytes.clone())).unwrap();
        assert_eq!(serde_json::from_str::<PluginState>(&json).unwrap().0, bytes);
        assert!(serde_json::from_str::<PluginState>("\"!!\"").is_err());
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(SlotId::fresh(), SlotId::fresh());
    }
}
