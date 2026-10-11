//! Every built-in effect, by id. Drives the Effects menu, the rack's add menu and presets.

use crate::effect::{Effect, OfflineEffect};
use crate::effects;
use crate::param::ParamSpec;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Amplitude,
    Dynamics,
    FilterEq,
    DelayEcho,
    Modulation,
    Reverb,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Amplitude => "Amplitude and Compression",
            Category::Dynamics => "Dynamics",
            Category::FilterEq => "Filter and EQ",
            Category::DelayEcho => "Delay and Echo",
            Category::Modulation => "Modulation",
            Category::Reverb => "Reverb",
        }
    }
}

pub enum Kind {
    Realtime(fn() -> Box<dyn Effect>),
    Offline(fn() -> Box<dyn OfflineEffect>),
}

pub struct EffectInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub category: Category,
    pub kind: Kind,
}

impl EffectInfo {
    pub fn is_realtime(&self) -> bool {
        matches!(self.kind, Kind::Realtime(_))
    }
}

pub static EFFECTS: &[EffectInfo] = &[
    EffectInfo {
        id: "amplify",
        name: "Amplify",
        category: Category::Amplitude,
        kind: Kind::Realtime(|| Box::new(effects::amplify::Amplify::new())),
    },
    EffectInfo {
        id: "channel_mixer",
        name: "Channel Mixer",
        category: Category::Amplitude,
        kind: Kind::Realtime(|| Box::new(effects::channel_mixer::ChannelMixer::new())),
    },
    EffectInfo {
        id: "normalize",
        name: "Normalize",
        category: Category::Amplitude,
        kind: Kind::Offline(|| Box::new(effects::normalize::Normalize::new())),
    },
    EffectInfo {
        id: "fade_envelope",
        name: "Fade Envelope",
        category: Category::Amplitude,
        kind: Kind::Offline(|| Box::new(effects::envelope::EnvelopeEffect::fade())),
    },
    EffectInfo {
        id: "gain_envelope",
        name: "Gain Envelope",
        category: Category::Amplitude,
        kind: Kind::Offline(|| Box::new(effects::envelope::EnvelopeEffect::gain())),
    },
    EffectInfo {
        id: "compressor",
        name: "Single-band Compressor",
        category: Category::Amplitude,
        kind: Kind::Realtime(|| Box::new(effects::compressor::Compressor::new())),
    },
    EffectInfo {
        id: "hard_limiter",
        name: "Hard Limiter",
        category: Category::Amplitude,
        kind: Kind::Realtime(|| Box::new(effects::limiter::Limiter::new())),
    },
    EffectInfo {
        id: "deesser",
        name: "DeEsser",
        category: Category::Amplitude,
        kind: Kind::Realtime(|| Box::new(effects::deesser::DeEsser::new())),
    },
    EffectInfo {
        id: "delay",
        name: "Delay",
        category: Category::DelayEcho,
        kind: Kind::Realtime(|| Box::new(effects::delay::DelayEffect::delay())),
    },
    EffectInfo {
        id: "echo",
        name: "Echo",
        category: Category::DelayEcho,
        kind: Kind::Realtime(|| Box::new(effects::delay::DelayEffect::echo())),
    },
    EffectInfo {
        id: "analog_delay",
        name: "Analog Delay",
        category: Category::DelayEcho,
        kind: Kind::Realtime(|| Box::new(effects::delay::DelayEffect::analog())),
    },
    EffectInfo {
        id: "chorus",
        name: "Chorus",
        category: Category::Modulation,
        kind: Kind::Realtime(|| Box::new(effects::modulation::ModDelay::chorus())),
    },
    EffectInfo {
        id: "flanger",
        name: "Flanger",
        category: Category::Modulation,
        kind: Kind::Realtime(|| Box::new(effects::modulation::ModDelay::flanger())),
    },
    EffectInfo {
        id: "phaser",
        name: "Phaser",
        category: Category::Modulation,
        kind: Kind::Realtime(|| Box::new(effects::modulation::Phaser::new())),
    },
    EffectInfo {
        id: "reverb",
        name: "Reverb",
        category: Category::Reverb,
        kind: Kind::Realtime(|| Box::new(effects::reverb::Reverb::new())),
    },
    EffectInfo {
        id: "studio_reverb",
        name: "Studio Reverb",
        category: Category::Reverb,
        kind: Kind::Realtime(|| Box::new(effects::reverb::StudioReverb::new())),
    },
    EffectInfo {
        id: "parametric_eq",
        name: "Parametric Equalizer",
        category: Category::FilterEq,
        kind: Kind::Realtime(|| Box::new(effects::parametric_eq::ParametricEq::new())),
    },
    EffectInfo {
        id: "graphic_eq_10",
        name: "Graphic Equalizer (10 Bands)",
        category: Category::FilterEq,
        kind: Kind::Realtime(|| Box::new(effects::graphic_eq::GraphicEq::ten())),
    },
    EffectInfo {
        id: "graphic_eq_20",
        name: "Graphic Equalizer (20 Bands)",
        category: Category::FilterEq,
        kind: Kind::Realtime(|| Box::new(effects::graphic_eq::GraphicEq::twenty())),
    },
    EffectInfo {
        id: "graphic_eq_30",
        name: "Graphic Equalizer (30 Bands)",
        category: Category::FilterEq,
        kind: Kind::Realtime(|| Box::new(effects::graphic_eq::GraphicEq::thirty())),
    },
    EffectInfo {
        id: "notch",
        name: "Notch Filter",
        category: Category::FilterEq,
        kind: Kind::Realtime(|| Box::new(effects::notch::Notch::new())),
    },
    EffectInfo {
        id: "scientific_filter",
        name: "Scientific Filter",
        category: Category::FilterEq,
        kind: Kind::Realtime(|| Box::new(effects::scientific::Scientific::new())),
    },
];

pub fn info(id: &str) -> Option<&'static EffectInfo> {
    EFFECTS.iter().find(|e| e.id == id)
}

/// A new real-time effect at its defaults, if `id` names one.
pub fn create(id: &str) -> Option<Box<dyn Effect>> {
    match info(id)?.kind {
        Kind::Realtime(make) => Some(make()),
        Kind::Offline(_) => None,
    }
}

pub fn create_offline(id: &str) -> Option<Box<dyn OfflineEffect>> {
    match info(id)?.kind {
        Kind::Offline(make) => Some(make()),
        Kind::Realtime(_) => None,
    }
}

/// Id prefixes of hosted plugin effects (`vst3:<class id>`, `au:<type-subtype-manufacturer>`).
/// Plugin ids are never built in; `dsp` only recognises them so it can tell "missing plugin"
/// from "unknown effect".
pub const PLUGIN_PREFIXES: &[&str] = &["vst3:", "au:", "clap:"];

pub fn is_plugin_id(id: &str) -> bool {
    PLUGIN_PREFIXES.iter().any(|p| id.starts_with(p))
}

struct Cached {
    specs: &'static [ParamSpec],
    defaults: Vec<f32>,
}

/// Built-in effects' specs and defaults, computed once. Lets the UI read parameter lists every
/// frame without constructing an effect.
fn cache() -> &'static HashMap<&'static str, Cached> {
    static CACHE: OnceLock<HashMap<&'static str, Cached>> = OnceLock::new();
    CACHE.get_or_init(|| {
        EFFECTS
            .iter()
            .map(|info| {
                let (specs, defaults): (&[ParamSpec], Vec<f32>) = match &info.kind {
                    Kind::Realtime(make) => {
                        let fx = make();
                        (fx.specs().to_vec().leak(), fx.values().to_vec())
                    }
                    Kind::Offline(make) => {
                        let fx = make();
                        (fx.specs(), fx.values().to_vec())
                    }
                };
                (info.id, Cached { specs, defaults })
            })
            .collect()
    })
}

/// Parameter list of a built-in effect (real-time or offline).
pub fn specs(id: &str) -> Option<&'static [ParamSpec]> {
    cache().get(id).map(|c| c.specs)
}

/// Default parameter values of a built-in effect.
pub fn defaults(id: &str) -> Option<&'static [f32]> {
    cache().get(id).map(|c| c.defaults.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_specs_match_instances() {
        for info in EFFECTS {
            let specs = specs(info.id).unwrap();
            let defaults = defaults(info.id).unwrap();
            assert_eq!(specs.len(), defaults.len(), "{}", info.id);
        }
        assert!(specs("nope").is_none());
        assert!(is_plugin_id("vst3:ABCD") && is_plugin_id("au:aufx-dely-appl"));
        assert!(!is_plugin_id("amplify"));
    }
}
