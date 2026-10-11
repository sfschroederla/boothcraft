//! Every built-in effect, by id. Drives the Effects menu, the rack's add menu and presets.

use crate::effect::{Effect, OfflineEffect};
use crate::effects;

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
