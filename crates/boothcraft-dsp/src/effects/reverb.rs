use crate::delayline::{DelayLine, OnePole};
use crate::effect::Effect;
use crate::param::{defaults, ParamSpec, Unit};

// ---------------------------------------------------------------------------------------------
// Reverb: Freeverb-style parallel combs into series all-passes, per channel.
// ---------------------------------------------------------------------------------------------

static REVERB_SPECS: [ParamSpec; 5] = [
    ParamSpec::new("size", "Room Size", Unit::Percent, 0.0, 100.0, 50.0),
    ParamSpec::new("damping", "Damping", Unit::Percent, 0.0, 100.0, 50.0),
    ParamSpec::new("width", "Width", Unit::Percent, 0.0, 100.0, 100.0),
    ParamSpec::new("predelay", "Pre-delay", Unit::Ms, 0.0, 200.0, 10.0),
    ParamSpec::new("mix", "Mix", Unit::Percent, 0.0, 100.0, 30.0),
];

/// Freeverb comb and all-pass lengths in samples at 44.1 kHz; the right channel adds `SPREAD`.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

#[derive(Clone, Default)]
struct Comb {
    line: DelayLine,
    len: f32,
    store: f32,
}

#[derive(Clone, Default)]
struct Channel {
    pre: DelayLine,
    combs: Vec<Comb>,
    allpasses: Vec<(DelayLine, f32)>,
}

pub struct Reverb {
    values: Vec<f32>,
    rate: f32,
    chans: Vec<Channel>,
}

impl Reverb {
    pub fn new() -> Reverb {
        Reverb {
            values: defaults(&REVERB_SPECS),
            rate: 48_000.0,
            chans: Vec::new(),
        }
    }
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb::new()
    }
}

impl Effect for Reverb {
    fn specs(&self) -> &'static [ParamSpec] {
        &REVERB_SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        let scale = sample_rate / 44_100.0;
        self.chans = (0..channels)
            .map(|c| {
                let extra = if c % 2 == 1 { SPREAD } else { 0 };
                let len = |n: usize| ((n + extra) as f32 * scale).round().max(1.0);
                Channel {
                    pre: DelayLine::new((0.2 * sample_rate) as usize + 2),
                    combs: COMBS
                        .iter()
                        .map(|&n| Comb {
                            line: DelayLine::new(len(n) as usize + 1),
                            len: len(n),
                            store: 0.0,
                        })
                        .collect(),
                    allpasses: ALLPASSES
                        .iter()
                        .map(|&n| (DelayLine::new(len(n) as usize + 1), len(n)))
                        .collect(),
                }
            })
            .collect();
    }

    fn reset(&mut self) {
        for ch in &mut self.chans {
            ch.pre.clear();
            for c in &mut ch.combs {
                c.line.clear();
                c.store = 0.0;
            }
            for (a, _) in &mut ch.allpasses {
                a.clear();
            }
        }
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        let v = &self.values;
        let feedback = 0.7 + 0.28 * v[0] / 100.0;
        let damp = 0.4 * v[1] / 100.0;
        let width = v[2] / 100.0;
        let pre = (v[3] * 0.001 * self.rate).max(1.0);
        let mix = v[4] / 100.0;
        let stereo = buf.len() >= 2;
        let mut wet = [0f32; 2];
        for i in 0..n {
            for (c, ch) in self.chans.iter_mut().enumerate().take(buf.len()) {
                let x = buf[c][i];
                ch.pre.push(x);
                let input = ch.pre.read(pre) * 0.015;
                let mut out = 0.0;
                for comb in &mut ch.combs {
                    let y = comb.line.read(comb.len);
                    comb.store = y * (1.0 - damp) + comb.store * damp;
                    comb.line.push(input + comb.store * feedback);
                    out += y;
                }
                for (ap, len) in &mut ch.allpasses {
                    let delayed = ap.read(*len);
                    ap.push(out + delayed * 0.5);
                    out = delayed - out;
                }
                if c < 2 {
                    wet[c] = out;
                }
                if !stereo || c >= 2 {
                    buf[c][i] = x * (1.0 - mix) + out * mix;
                }
            }
            if stereo {
                // Width blends each side's tail with the other's.
                let (a, b) = ((1.0 + width) / 2.0, (1.0 - width) / 2.0);
                let (l, r) = (wet[0] * a + wet[1] * b, wet[1] * a + wet[0] * b);
                buf[0][i] = buf[0][i] * (1.0 - mix) + l * mix;
                buf[1][i] = buf[1][i] * (1.0 - mix) + r * mix;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Studio Reverb: 8-line feedback delay network with a Householder mixing matrix.
// ---------------------------------------------------------------------------------------------

static STUDIO_SPECS: [ParamSpec; 9] = [
    ParamSpec::new("size", "Room Size", Unit::Percent, 0.0, 100.0, 60.0),
    ParamSpec::new("decay", "Decay", Unit::Seconds, 0.2, 20.0, 1.8).log(),
    ParamSpec::new(
        "damping",
        "High Frequency Damping",
        Unit::Hz,
        1000.0,
        20_000.0,
        6000.0,
    )
    .log(),
    ParamSpec::new(
        "early",
        "Early Reflections",
        Unit::Percent,
        0.0,
        100.0,
        50.0,
    ),
    ParamSpec::new("width", "Width", Unit::Percent, 0.0, 100.0, 100.0),
    ParamSpec::new("low_cut", "Low Cut", Unit::Hz, 20.0, 1000.0, 80.0).log(),
    ParamSpec::new("high_cut", "High Cut", Unit::Hz, 1000.0, 20_000.0, 12_000.0).log(),
    ParamSpec::new("dry", "Dry", Unit::Percent, 0.0, 100.0, 100.0),
    ParamSpec::new("wet", "Wet", Unit::Percent, 0.0, 100.0, 30.0),
];

/// Mutually prime-ish line lengths in ms at room size 100%.
const FDN_MS: [f32; 8] = [29.7, 37.1, 41.1, 43.7, 53.3, 59.9, 67.7, 73.1];
/// Early-reflection taps (ms at room size 100%) and their gains; odd taps go right.
const EARLY: [(f32, f32); 6] = [
    (7.0, 0.8),
    (11.3, 0.7),
    (17.9, 0.55),
    (23.1, 0.45),
    (29.7, 0.35),
    (37.3, 0.25),
];

pub struct StudioReverb {
    values: Vec<f32>,
    rate: f32,
    lines: Vec<DelayLine>,
    lens: [f32; 8],
    gains: [f32; 8],
    damp: [OnePole; 8],
    early: DelayLine,
    hi_cut: [OnePole; 2],
    lo_cut: [OnePole; 2],
}

impl StudioReverb {
    pub fn new() -> StudioReverb {
        StudioReverb {
            values: defaults(&STUDIO_SPECS),
            rate: 48_000.0,
            lines: Vec::new(),
            lens: [1.0; 8],
            gains: [0.0; 8],
            damp: Default::default(),
            early: DelayLine::default(),
            hi_cut: Default::default(),
            lo_cut: Default::default(),
        }
    }

    fn room_scale(&self) -> f32 {
        0.3 + 1.2 * self.values[0] / 100.0
    }
}

impl Default for StudioReverb {
    fn default() -> Self {
        StudioReverb::new()
    }
}

impl Effect for StudioReverb {
    fn specs(&self) -> &'static [ParamSpec] {
        &STUDIO_SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, _channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        // Room for the largest room size (scale 1.5).
        self.lines = FDN_MS
            .iter()
            .map(|ms| DelayLine::new((ms * 1.5 * 0.001 * sample_rate) as usize + 4))
            .collect();
        self.early = DelayLine::new((40.0 * 1.5 * 0.001 * sample_rate) as usize + 4);
        self.update();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.early.clear();
        self.damp.iter_mut().for_each(OnePole::reset);
        self.hi_cut.iter_mut().for_each(OnePole::reset);
        self.lo_cut.iter_mut().for_each(OnePole::reset);
    }

    // The delay network walks several parallel per-line arrays; indexing reads clearest here.
    #[allow(clippy::needless_range_loop)]
    fn update(&mut self) {
        let scale = self.room_scale();
        let t60 = self.values[1];
        for k in 0..8 {
            self.lens[k] = (FDN_MS[k] * scale * 0.001 * self.rate).max(2.0);
            // Gain per pass so the loop decays 60 dB in `t60` seconds.
            self.gains[k] = 10f32.powf(-3.0 * self.lens[k] / (t60 * self.rate));
            self.damp[k].set(self.values[2], self.rate);
        }
        for f in &mut self.hi_cut {
            f.set(self.values[6], self.rate);
        }
        for f in &mut self.lo_cut {
            f.set(self.values[5], self.rate);
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn process(&mut self, buf: &mut [&mut [f32]]) {
        if self.lines.is_empty() {
            return;
        }
        let n = buf.first().map_or(0, |c| c.len());
        let v = &self.values;
        let (early_lvl, width, dry, wet) = (v[3] / 100.0, v[4] / 100.0, v[7] / 100.0, v[8] / 100.0);
        let scale = self.room_scale();
        let stereo = buf.len() >= 2;
        for i in 0..n {
            let (inl, inr) = if stereo {
                (buf[0][i], buf[1][i])
            } else {
                (buf[0][i], buf[0][i])
            };
            let mono = 0.5 * (inl + inr);
            // Early reflections from a tapped delay of the mono input.
            self.early.push(mono);
            let mut er = [0f32; 2];
            for (k, (ms, g)) in EARLY.iter().enumerate() {
                er[k % 2] += self.early.read(ms * scale * 0.001 * self.rate) * g;
            }
            // Late reverb: read all lines, mix with a Householder matrix, damp and feed back.
            let mut y = [0f32; 8];
            for k in 0..8 {
                y[k] = self.lines[k].read(self.lens[k]);
            }
            let sum: f32 = y.iter().sum::<f32>() * (2.0 / 8.0);
            for k in 0..8 {
                let input = if k % 2 == 0 { inl } else { inr } * 0.25;
                let fb = self.damp[k].tick((y[k] - sum) * self.gains[k]);
                self.lines[k].push(input + fb);
            }
            let late = [y[0] + y[2] + y[4] + y[6], y[1] + y[3] + y[5] + y[7]];
            let mut out = [0f32; 2];
            for s in 0..2 {
                let raw = late[s] * 0.35 + er[s] * early_lvl * 0.5;
                let hc = self.hi_cut[s].tick(raw);
                out[s] = hc - self.lo_cut[s].tick(hc);
            }
            let (a, b) = ((1.0 + width) / 2.0, (1.0 - width) / 2.0);
            let (l, r) = (out[0] * a + out[1] * b, out[1] * a + out[0] * b);
            if stereo {
                buf[0][i] = buf[0][i] * dry + l * wet;
                buf[1][i] = buf[1][i] * dry + r * wet;
                for ch in buf.iter_mut().skip(2) {
                    ch[i] = ch[i] * dry + 0.5 * (l + r) * wet;
                }
            } else {
                buf[0][i] = buf[0][i] * dry + 0.5 * (l + r) * wet;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail_energy(mut e: Box<dyn Effect>, from: usize, to: usize) -> f32 {
        e.prepare(48_000.0, 2, 48_000);
        e.reset();
        let mut l = vec![0.0f32; to];
        let mut r = vec![0.0f32; to];
        l[0] = 1.0;
        r[0] = 1.0;
        for (a, b) in l.chunks_mut(4096).zip(r.chunks_mut(4096)) {
            e.process(&mut [a, b]);
        }
        l[from..to].iter().chain(&r[from..to]).map(|x| x * x).sum()
    }

    #[test]
    fn studio_decay_controls_tail_length() {
        let short = |d: f32| {
            let mut e = StudioReverb::new();
            e.set_param(1, d);
            e.set_param(7, 0.0);
            tail_energy(Box::new(e), 48_000, 96_000)
        };
        let (s, l) = (short(0.5), short(6.0));
        assert!(s.is_finite() && l.is_finite());
        assert!(
            l > s * 100.0,
            "6 s decay leaves far more tail after 1 s than 0.5 s: {s} vs {l}"
        );
        assert!(s < 1e-6, "0.5 s decay is essentially gone after 1 s: {s}");
    }

    #[test]
    fn reverbs_are_dry_when_wet_is_zero() {
        let input: Vec<f32> = (0..8000).map(|i| (i as f32 * 0.02).sin()).collect();
        for (mut e, wet) in [
            (Box::new(Reverb::new()) as Box<dyn Effect>, 4),
            (Box::new(StudioReverb::new()), 8),
        ] {
            e.set_param(wet, 0.0);
            e.prepare(48_000.0, 2, 8000);
            e.reset();
            let (mut a, mut b) = (input.clone(), input.clone());
            e.process(&mut [&mut a[..], &mut b[..]]);
            assert_eq!(a, input);
        }
    }

    #[test]
    fn freeverb_tail_is_bounded() {
        let mut e = Reverb::new();
        e.set_param(0, 100.0);
        e.set_param(1, 0.0);
        let energy = tail_energy(Box::new(e), 0, 240_000);
        assert!(energy.is_finite() && energy < 1e3, "{energy}");
    }
}
