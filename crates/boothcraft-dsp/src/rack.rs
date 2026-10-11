//! The Effects Rack: up to 16 effects in series with input/output gain and a wet/dry mix.

use crate::effect::Effect;
use crate::param::{db_to_gain, ParamSpec};
use crate::plugin_ref::{PluginRef, PluginState, SlotId};
use crate::registry;
use boothcraft_core::Rope;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

pub const SLOTS: usize = 16;

/// `carry` entry meaning "this slot takes nothing from the old rack".
pub const CARRY_NONE: u8 = u8::MAX;

/// A rack's settings, without any processing state: what the UI edits, presets store, and both
/// preview and Apply are built from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RackDesc {
    pub slots: Vec<Option<SlotDesc>>,
    pub input_gain_db: f32,
    pub output_gain_db: f32,
    /// Wet amount, 0..=1.
    pub mix: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlotDesc {
    /// A built-in effect id, or a plugin id such as `vst3:<class id>`.
    pub effect: String,
    /// Parameter values. For a plugin slot these are only a display cache; `state` is
    /// authoritative, so never `set_all` after loading a plugin's state.
    pub values: Vec<f32>,
    pub bypass: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginRef>,
    /// The plugin's own saved state (kept even while the plugin is missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PluginState>,
    /// Instance identity across rebuilds. Not saved; ignored by `==`.
    #[serde(skip)]
    pub id: SlotId,
}

impl PartialEq for SlotDesc {
    fn eq(&self, o: &SlotDesc) -> bool {
        self.effect == o.effect
            && self.values == o.values
            && self.bypass == o.bypass
            && self.plugin == o.plugin
            && self.state == o.state
    }
}

impl Default for RackDesc {
    fn default() -> Self {
        RackDesc {
            slots: vec![None; SLOTS],
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            mix: 1.0,
        }
    }
}

impl RackDesc {
    /// A rack holding a single effect at its defaults (used by the per-effect dialogs).
    pub fn single(effect_id: &str) -> Option<RackDesc> {
        let mut d = RackDesc::default();
        d.slots[0] = Some(SlotDesc::new(effect_id)?);
        Some(d)
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// Whether any slot is a hosted plugin.
    pub fn has_plugins(&self) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|s| s.plugin.is_some() || registry::is_plugin_id(&s.effect))
    }
}

impl SlotDesc {
    /// A built-in real-time effect at its defaults.
    pub fn new(effect_id: &str) -> Option<SlotDesc> {
        if !registry::info(effect_id)?.is_realtime() {
            return None;
        }
        Some(SlotDesc {
            effect: effect_id.to_string(),
            values: registry::defaults(effect_id)?.to_vec(),
            bypass: false,
            plugin: None,
            state: None,
            id: SlotId::fresh(),
        })
    }

    /// A hosted plugin slot; `values` are the plugin's current parameter values for display.
    pub fn for_plugin(plugin: PluginRef, values: Vec<f32>, state: Option<PluginState>) -> SlotDesc {
        SlotDesc {
            effect: plugin.effect_id(),
            values,
            bypass: false,
            plugin: Some(plugin),
            state,
            id: SlotId::fresh(),
        }
    }
}

/// Everything a rack and its effects are prepared for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RackConfig {
    pub sample_rate: f32,
    pub channels: usize,
    pub max_block: usize,
}

/// Why a slot is not in the built rack. The slot passes audio through.
#[derive(Clone, Debug, PartialEq)]
pub enum SlotError {
    /// Not a built-in effect and not a plugin id.
    Unknown(String),
    /// A plugin that is not installed (or was blocked).
    Missing(String),
    /// A plugin still instantiating; it will be picked up by a later rebuild.
    NotReady,
    /// The plugin refused to load, activate or restore its state.
    Failed(String),
    /// The plugin does not support the requested sample rate.
    UnsupportedRate(f32),
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlotError::Unknown(id) => write!(f, "unknown effect {id}"),
            SlotError::Missing(id) => write!(f, "plugin {id} is not installed"),
            SlotError::NotReady => write!(f, "plugin is still loading"),
            SlotError::Failed(why) => write!(f, "plugin failed: {why}"),
            SlotError::UnsupportedRate(r) => write!(f, "plugin does not support {r} Hz"),
        }
    }
}

impl std::error::Error for SlotError {}

/// Where a rack gets each slot's effect from. `dsp` knows only the built-ins; the plugin host
/// supplies another source. Effects come back prepared and reset, with parameters applied.
pub trait SlotSource {
    fn make(
        &mut self,
        index: usize,
        slot: &SlotDesc,
        cfg: &RackConfig,
    ) -> Result<Box<dyn Effect>, SlotError>;
}

/// The built-in effects.
pub struct BuiltinSource;

impl SlotSource for BuiltinSource {
    fn make(
        &mut self,
        _index: usize,
        slot: &SlotDesc,
        cfg: &RackConfig,
    ) -> Result<Box<dyn Effect>, SlotError> {
        let mut effect = registry::create(&slot.effect)
            .ok_or_else(|| SlotError::Unknown(slot.effect.clone()))?;
        effect.set_all(&slot.values);
        effect.prepare(cfg.sample_rate, cfg.channels, cfg.max_block);
        effect.reset();
        Ok(effect)
    }
}

/// An effect with no parameters that passes audio through. Stands in for a slot whose live
/// instance [`Rack::adopt`] will move in from the previous rack. Zero-sized, so boxing it does
/// not allocate.
pub struct Carried;

impl Effect for Carried {
    fn specs(&self) -> &[ParamSpec] {
        &[]
    }
    fn values(&self) -> &[f32] {
        &[]
    }
    fn values_mut(&mut self) -> &mut [f32] {
        &mut []
    }
    fn prepare(&mut self, _: f32, _: usize, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _: &mut [&mut [f32]]) {}
}

struct Slot {
    effect: Box<dyn Effect>,
    bypass: bool,
}

/// A prepared rack. Construct (allocate) off the audio thread; `process_*` never allocates.
pub struct Rack {
    slots: Vec<Option<Slot>>,
    input_gain: f32,
    output_gain: f32,
    mix: f32,
    channels: usize,
    max_block: usize,
    dry: Vec<Vec<f32>>,
    planar: Vec<Vec<f32>>,
}

impl Rack {
    pub fn new(desc: &RackDesc, sample_rate: f32, channels: usize, max_block: usize) -> Rack {
        let cfg = RackConfig {
            sample_rate,
            channels,
            max_block,
        };
        Rack::build(desc, &cfg, &mut BuiltinSource).0
    }

    /// Build a rack, asking `source` for each slot's effect. Slots the source cannot supply are
    /// left out (they pass audio through) and reported, so callers can refuse to proceed or show
    /// why, rather than silently skipping them.
    pub fn build(
        desc: &RackDesc,
        cfg: &RackConfig,
        source: &mut dyn SlotSource,
    ) -> (Rack, Vec<(usize, SlotError)>) {
        let channels = cfg.channels.max(1);
        let cfg = RackConfig { channels, ..*cfg };
        let mut errors = Vec::new();
        let slots = desc
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let s = s.as_ref()?;
                match source.make(i, s, &cfg) {
                    Ok(effect) => Some(Slot {
                        effect,
                        bypass: s.bypass,
                    }),
                    Err(e) => {
                        errors.push((i, e));
                        None
                    }
                }
            })
            .collect();
        let rack = Rack {
            slots,
            input_gain: db_to_gain(desc.input_gain_db),
            output_gain: db_to_gain(desc.output_gain_db),
            mix: desc.mix.clamp(0.0, 1.0),
            channels,
            max_block: cfg.max_block,
            dry: vec![vec![0.0; cfg.max_block]; channels],
            planar: vec![vec![0.0; cfg.max_block]; channels],
        };
        (rack, errors)
    }

    /// Move live effects from `old` into this rack: slot `i` takes `old`'s slot `carry[i]` (or
    /// keeps its own when `CARRY_NONE`). Only pointers move, so this is safe on the audio thread.
    /// The slots here that receive a carried effect should be [`Carried`] placeholders (or any
    /// effect); whatever they held ends up in `old`, which the caller then retires.
    pub fn adopt(&mut self, old: &mut Rack, carry: &[u8; SLOTS]) {
        for (i, &from) in carry.iter().enumerate() {
            let from = from as usize;
            if from == CARRY_NONE as usize {
                continue;
            }
            if let (Some(Some(mine)), Some(Some(theirs))) =
                (self.slots.get_mut(i), old.slots.get_mut(from))
            {
                std::mem::swap(&mut mine.effect, &mut theirs.effect);
            }
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn set_param(&mut self, slot: usize, index: usize, value: f32) {
        if let Some(Some(s)) = self.slots.get_mut(slot) {
            s.effect.set_param(index, value);
        }
    }

    pub fn set_bypass(&mut self, slot: usize, bypass: bool) {
        if let Some(Some(s)) = self.slots.get_mut(slot) {
            s.bypass = bypass;
        }
    }

    pub fn set_gains(&mut self, input_db: f32, output_db: f32, mix: f32) {
        self.input_gain = db_to_gain(input_db);
        self.output_gain = db_to_gain(output_db);
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Total delay of the active effects, in frames.
    pub fn latency(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|s| !s.bypass)
            .map(|s| s.effect.latency())
            .sum()
    }

    /// Longest tail of the active effects, in frames.
    pub fn tail(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|s| !s.bypass)
            .map(|s| s.effect.tail())
            .max()
            .unwrap_or(0)
    }

    pub fn reset(&mut self) {
        for s in self.slots.iter_mut().flatten() {
            s.effect.reset();
        }
    }

    /// The effect in `slot`, for callers (the plugin host) that need to inspect or retire it.
    pub fn effect_mut(&mut self, slot: usize) -> Option<&mut dyn Effect> {
        match self.slots.get_mut(slot) {
            Some(Some(s)) => Some(s.effect.as_mut()),
            _ => None,
        }
    }

    /// Take every effect out of the rack (for the host to tear down on the right thread).
    pub fn into_effects(self) -> Vec<(usize, Box<dyn Effect>)> {
        self.slots
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| Some((i, s?.effect)))
            .collect()
    }

    /// Process planar audio. Longer buffers are processed in `max_block` pieces.
    pub fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        let mut at = 0;
        while at < n {
            let len = (n - at).min(self.max_block);
            for (c, ch) in buf.iter().enumerate().take(self.channels) {
                self.planar[c][..len].copy_from_slice(&ch[at..at + len]);
            }
            self.process_planar(len);
            for (c, ch) in buf.iter_mut().enumerate().take(self.channels) {
                ch[at..at + len].copy_from_slice(&self.planar[c][..len]);
            }
            at += len;
        }
    }

    /// Process the first `len` frames of planar `Vec` channels in place (the engine's per-channel
    /// scratch buffers), without building a slice array.
    pub fn process_vecs(&mut self, bufs: &mut [Vec<f32>], len: usize) {
        let mut at = 0;
        while at < len {
            let n = (len - at).min(self.max_block);
            for (c, ch) in bufs.iter().enumerate().take(self.channels) {
                self.planar[c][..n].copy_from_slice(&ch[at..at + n]);
            }
            self.process_planar(n);
            for (c, ch) in bufs.iter_mut().enumerate().take(self.channels) {
                ch[at..at + n].copy_from_slice(&self.planar[c][..n]);
            }
            at += n;
        }
    }

    /// Process interleaved audio with the rack's channel count.
    pub fn process_interleaved(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let frames = out.len() / ch;
        let mut at = 0;
        while at < frames {
            let len = (frames - at).min(self.max_block);
            for i in 0..len {
                for c in 0..ch {
                    self.planar[c][i] = out[(at + i) * ch + c];
                }
            }
            self.process_planar(len);
            for i in 0..len {
                for c in 0..ch {
                    out[(at + i) * ch + c] = self.planar[c][i];
                }
            }
            at += len;
        }
    }

    fn process_planar(&mut self, len: usize) {
        let (ig, og, mix) = (self.input_gain, self.output_gain, self.mix);
        for ch in self.planar.iter_mut() {
            ch[..len].iter_mut().for_each(|x| *x *= ig);
        }
        if mix < 1.0 {
            for (d, p) in self.dry.iter_mut().zip(&self.planar) {
                d[..len].copy_from_slice(&p[..len]);
            }
        }
        {
            // Effects see up to 8 channels (any more stay dry), borrowed into a stack array so
            // nothing allocates.
            let mut slices: [&mut [f32]; 8] = Default::default();
            let n = self.planar.len().min(8);
            for (dst, ch) in slices.iter_mut().zip(self.planar.iter_mut()) {
                *dst = &mut ch[..len];
            }
            for slot in self.slots.iter_mut().flatten() {
                if !slot.bypass {
                    slot.effect.process(&mut slices[..n]);
                }
            }
        }
        // Mix and output gain apply to every channel, including those past the effects' 8.
        for (c, ch) in self.planar.iter_mut().enumerate() {
            for (i, x) in ch[..len].iter_mut().enumerate() {
                let wet = *x;
                *x = if mix < 1.0 {
                    (self.dry[c][i] * (1.0 - mix) + wet * mix) * og
                } else {
                    wet * og
                };
            }
        }
    }
}

/// Run `desc` over whole channels offline (Apply). Latency is compensated so the output lines
/// up with the input. `progress` receives 0..=1000 (per mille); setting `cancel` stops early and
/// returns `false`, leaving `chans` partly processed (callers discard them).
pub fn render_offline(
    desc: &RackDesc,
    chans: &mut [Vec<f32>],
    sample_rate: f32,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> bool {
    let mut rack = Rack::new(desc, sample_rate, chans.len(), OFFLINE_BLOCK);
    render_offline_with(&mut rack, chans, progress, cancel)
}

/// Block size for offline rendering; plugin racks are built with this as `max_block`.
pub const OFFLINE_BLOCK: usize = 4096;

/// [`render_offline`] with a rack the caller built (e.g. from plugins). The rack's latency is
/// read here, so it must already be primed.
pub fn render_offline_with(
    rack: &mut Rack,
    chans: &mut [Vec<f32>],
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> bool {
    const BLOCK: usize = OFFLINE_BLOCK;
    let n = chans.first().map_or(0, Vec::len);
    let latency = rack.latency();
    let total = n + latency;
    let mut block: Vec<Vec<f32>> = vec![vec![0.0; BLOCK]; chans.len()];
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(total); chans.len()];
    let mut at = 0;
    while at < total {
        if cancel.load(Relaxed) {
            return false;
        }
        let len = (total - at).min(BLOCK);
        for (b, c) in block.iter_mut().zip(chans.iter()) {
            for (i, x) in b[..len].iter_mut().enumerate() {
                *x = c.get(at + i).copied().unwrap_or(0.0);
            }
        }
        let mut slices: Vec<&mut [f32]> = block.iter_mut().map(|b| &mut b[..len]).collect();
        rack.process(&mut slices);
        for (o, b) in out.iter_mut().zip(&block) {
            o.extend_from_slice(&b[..len]);
        }
        at += len;
        progress.store((at * 1000 / total.max(1)) as u32, Relaxed);
    }
    for (c, o) in chans.iter_mut().zip(out) {
        c.copy_from_slice(&o[latency..latency + n]);
    }
    true
}

/// Like [`render_offline`], but reads `range` directly from `channels` (zero-padded past
/// `range.end`, like a selection near the end of a shorter file) and returns the result as new
/// ropes instead of flat buffers — so a selection covering most or all of a disk-backed file never
/// needs materializing as one giant in-memory copy, before, during or after processing. `None`
/// means the render was cancelled.
pub fn render_rope(
    desc: &RackDesc,
    channels: &[Rope],
    range: Range<usize>,
    sample_rate: f32,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Option<Vec<Rope>> {
    let mut rack = Rack::new(desc, sample_rate, channels.len(), OFFLINE_BLOCK);
    render_rope_with(&mut rack, channels, range, progress, cancel)
}

/// [`render_rope`] with a rack the caller built. The rack's latency is read here, so it must
/// already be primed. Its channel count must equal `channels.len()`.
pub fn render_rope_with(
    rack: &mut Rack,
    channels: &[Rope],
    range: Range<usize>,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Option<Vec<Rope>> {
    const BLOCK: usize = OFFLINE_BLOCK;
    let n = range.end.saturating_sub(range.start);
    let latency = rack.latency();
    let total = n + latency;
    let mut block: Vec<Vec<f32>> = vec![vec![0.0; BLOCK]; channels.len()];
    let mut out: Vec<Rope> = vec![Rope::new(); channels.len()];
    let mut at = 0;
    while at < total {
        if cancel.load(Relaxed) {
            return None;
        }
        let len = (total - at).min(BLOCK);
        for (b, rope) in block.iter_mut().zip(channels) {
            read_clamped(rope, range.start + at, &mut b[..len], range.end);
        }
        let mut slices: Vec<&mut [f32]> = block.iter_mut().map(|b| &mut b[..len]).collect();
        rack.process(&mut slices);
        for (o, b) in out.iter_mut().zip(&block) {
            o.push_samples(&b[..len]);
        }
        at += len;
        progress.store((at * 1000 / total.max(1)) as u32, Relaxed);
    }
    for o in &mut out {
        o.delete(0..latency.min(o.len()));
    }
    Some(out)
}

/// Fill `buf` from `start`, treating anything at or past `hard_end` (which may be before the
/// rope's own length, e.g. a selection that doesn't reach the file's end) as silence.
fn read_clamped(rope: &Rope, start: usize, buf: &mut [f32], hard_end: usize) {
    let avail = hard_end.saturating_sub(start).min(buf.len());
    if avail > 0 {
        rope.read(start, &mut buf[..avail]);
    }
    buf[avail..].fill(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::AllocDisabler;

    #[global_allocator]
    static ALLOC: AllocDisabler = AllocDisabler;

    fn amp_rack(db: f32) -> RackDesc {
        let mut d = RackDesc::single("amplify").unwrap();
        d.slots[0].as_mut().unwrap().values[0] = db;
        d
    }

    fn render(desc: &RackDesc, input: &[f32]) -> Vec<f32> {
        let mut chans = vec![input.to_vec()];
        assert!(render_offline(
            desc,
            &mut chans,
            48_000.0,
            &AtomicU32::new(0),
            &AtomicBool::new(false)
        ));
        chans.remove(0)
    }

    #[test]
    fn neutral_rack_is_identity() {
        let input: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.01).sin()).collect();
        assert_eq!(render(&RackDesc::default(), &input), input);
        assert_eq!(render(&amp_rack(0.0), &input), input);
    }

    #[test]
    fn gain_mix_and_bypass() {
        let input = vec![0.25f32; 5000];
        let doubled = render(&amp_rack(20.0 * 2f32.log10()), &input);
        assert!((doubled[4000] - 0.5).abs() < 1e-5);
        let mut half_wet = amp_rack(20.0 * 2f32.log10());
        half_wet.mix = 0.5;
        assert!((render(&half_wet, &input)[4000] - 0.375).abs() < 1e-5);
        let mut bypassed = amp_rack(20.0);
        bypassed.slots[0].as_mut().unwrap().bypass = true;
        bypassed.output_gain_db = -6.0;
        assert!((render(&bypassed, &input)[10] - 0.25 * db_to_gain(-6.0)).abs() < 1e-6);
    }

    #[test]
    fn interleaved_matches_planar() {
        let desc = amp_rack(-3.0);
        let l: Vec<f32> = (0..3000).map(|i| (i as f32 * 0.02).sin()).collect();
        let r: Vec<f32> = l.iter().map(|x| x * 0.5).collect();
        let mut planar = Rack::new(&desc, 48_000.0, 2, 256);
        let (mut a, mut b) = (l.clone(), r.clone());
        planar.process(&mut [&mut a[..], &mut b[..]]);
        let mut inter: Vec<f32> = l.iter().zip(&r).flat_map(|(x, y)| [*x, *y]).collect();
        Rack::new(&desc, 48_000.0, 2, 256).process_interleaved(&mut inter);
        for i in 0..3000 {
            assert_eq!(inter[2 * i], a[i]);
            assert_eq!(inter[2 * i + 1], b[i]);
        }
    }

    #[test]
    fn offline_render_compensates_latency() {
        // A limiter that never engages must return the input exactly aligned.
        let mut d = RackDesc::single("hard_limiter").unwrap();
        d.slots[0].as_mut().unwrap().values[2] = 10.0;
        let input: Vec<f32> = (0..20_000)
            .map(|i| (i as f32 * 0.003).sin() * 0.25)
            .collect();
        let out = render(&d, &input);
        let worst = out
            .iter()
            .zip(&input)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-6, "misaligned by latency: {worst}");
    }

    #[test]
    fn cancel_stops_rendering() {
        let mut chans = vec![vec![0.0f32; 100_000]];
        assert!(!render_offline(
            &amp_rack(1.0),
            &mut chans,
            48_000.0,
            &AtomicU32::new(0),
            &AtomicBool::new(true)
        ));
    }

    #[test]
    fn unknown_effects_are_skipped_and_reported() {
        let mut d = RackDesc::default();
        d.slots[3] = Some(SlotDesc {
            effect: "no-such-effect".into(),
            values: vec![],
            bypass: false,
            plugin: None,
            state: None,
            id: SlotId::fresh(),
        });
        let input = vec![0.5f32; 100];
        assert_eq!(render(&d, &input), input);
        let cfg = RackConfig {
            sample_rate: 48_000.0,
            channels: 1,
            max_block: 64,
        };
        let (_, errors) = Rack::build(&d, &cfg, &mut BuiltinSource);
        assert_eq!(
            errors,
            vec![(3, SlotError::Unknown("no-such-effect".into()))]
        );
    }

    #[test]
    fn old_rack_files_load_and_plugin_state_round_trips() {
        // The format before plugin support: no plugin/state fields.
        let old = r#"{"slots":[{"effect":"amplify","values":[6.0],"bypass":false},null],
                      "input_gain_db":0.0,"output_gain_db":0.0,"mix":1.0}"#;
        let d: RackDesc = serde_json::from_str(old).unwrap();
        assert_eq!(d.slots[0].as_ref().unwrap().plugin, None);

        let state: Vec<u8> = (0..=255).rev().collect();
        let plugin = PluginRef {
            format: "vst3".into(),
            uid: "ABCD".into(),
            name: "Thing".into(),
            vendor: "Acme".into(),
        };
        let mut d = RackDesc::default();
        d.slots[1] = Some(SlotDesc::for_plugin(
            plugin,
            vec![0.5],
            Some(PluginState(state.clone())),
        ));
        let back: RackDesc = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        assert_eq!(
            back.slots[1].as_ref().unwrap().state.as_ref().unwrap().0,
            state
        );
        assert!(d.has_plugins());
    }

    #[test]
    fn adopt_moves_live_effects_without_allocating() {
        let mut old = Rack::new(&amp_rack(20.0 * 2f32.log10()), 48_000.0, 1, 64);
        // New rack: slot 0 is a carried placeholder, slot 1 a fresh amplify at 0 dB.
        let mut desc = RackDesc::default();
        desc.slots[0] = Some(SlotDesc::new("amplify").unwrap());
        desc.slots[1] = Some(SlotDesc::new("amplify").unwrap());
        let mut new = Rack::new(&desc, 48_000.0, 1, 64);
        let mut carry = [CARRY_NONE; SLOTS];
        carry[0] = 0;
        assert_no_alloc::assert_no_alloc(|| new.adopt(&mut old, &carry));
        let mut buf = vec![0.25f32; 64];
        new.process(&mut [&mut buf[..]]);
        assert!(
            (buf[63] - 0.5).abs() < 1e-5,
            "carried effect (+6 dB) must be live"
        );
    }

    #[test]
    fn channels_past_eight_get_gain_and_mix() {
        let d = RackDesc {
            output_gain_db: -6.0,
            ..RackDesc::default()
        };
        let mut r = Rack::new(&d, 48_000.0, 10, 32);
        let mut bufs: Vec<Vec<f32>> = vec![vec![1.0; 32]; 10];
        let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
        r.process(&mut slices);
        for (c, b) in bufs.iter().enumerate() {
            assert!((b[5] - db_to_gain(-6.0)).abs() < 1e-6, "channel {c}");
        }
    }

    #[test]
    fn render_rope_matches_render_offline() {
        let desc = amp_rack(20.0 * 2f32.log10());
        let input: Vec<f32> = (0..20_000).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
        let mut flat = vec![input.clone()];
        assert!(render_offline(
            &desc,
            &mut flat,
            48_000.0,
            &AtomicU32::new(0),
            &AtomicBool::new(false)
        ));

        let ropes = vec![Rope::from_samples(&input)];
        let out = render_rope(
            &desc,
            &ropes,
            0..input.len(),
            48_000.0,
            &AtomicU32::new(0),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), input.len());
        assert_eq!(out[0].to_vec(0..input.len()), flat[0]);
    }

    #[test]
    fn render_rope_reads_only_the_selection_and_zero_pads_past_it() {
        // A selection in the middle of a longer rope, including a region past where the
        // selection's own end falls well short of the rope's actual length.
        let input: Vec<f32> = (0..50_000).map(|i| i as f32).collect();
        let ropes = vec![Rope::from_samples(&input)];
        let out = render_rope(
            &RackDesc::default(),
            &ropes,
            10_000..10_010,
            48_000.0,
            &AtomicU32::new(0),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(out[0].to_vec(0..10), input[10_000..10_010]);
    }

    #[test]
    fn render_rope_can_be_cancelled() {
        let ropes = vec![Rope::silence(200_000)];
        let cancel = AtomicBool::new(true);
        assert!(render_rope(
            &amp_rack(1.0),
            &ropes,
            0..200_000,
            48_000.0,
            &AtomicU32::new(0),
            &cancel
        )
        .is_none());
    }

    #[test]
    fn render_rope_compensates_latency() {
        let mut d = RackDesc::single("hard_limiter").unwrap();
        d.slots[0].as_mut().unwrap().values[2] = 10.0;
        let input: Vec<f32> = (0..20_000)
            .map(|i| (i as f32 * 0.003).sin() * 0.25)
            .collect();
        let ropes = vec![Rope::from_samples(&input)];
        let out = render_rope(
            &d,
            &ropes,
            0..input.len(),
            48_000.0,
            &AtomicU32::new(0),
            &AtomicBool::new(false),
        )
        .unwrap();
        let worst = out[0]
            .to_vec(0..input.len())
            .iter()
            .zip(&input)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-6, "misaligned by latency: {worst}");
    }
}
