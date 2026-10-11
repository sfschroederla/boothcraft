use crate::delayline::{DelayLine, Lfo};
use crate::effect::Effect;
use crate::param::{defaults, ParamSpec, Unit};

const VOICES: &[&str] = &["2", "3", "4", "5", "6", "7", "8"];

static CHORUS_SPECS: [ParamSpec; 7] = [
    ParamSpec::choice("voices", "Voices", VOICES, 1),
    ParamSpec::new("delay", "Delay Time", Unit::Ms, 5.0, 50.0, 15.0),
    ParamSpec::new("depth", "Modulation Depth", Unit::Ms, 0.0, 10.0, 3.0),
    ParamSpec::new("rate", "Modulation Rate", Unit::Hz, 0.05, 10.0, 0.6).log(),
    ParamSpec::new("feedback", "Feedback", Unit::Percent, 0.0, 90.0, 0.0),
    ParamSpec::new("spread", "Stereo Spread", Unit::Percent, 0.0, 100.0, 60.0),
    ParamSpec::new("mix", "Mix", Unit::Percent, 0.0, 100.0, 50.0),
];

static FLANGER_SPECS: [ParamSpec; 6] = [
    ParamSpec::new("initial", "Initial Delay", Unit::Ms, 0.1, 10.0, 0.5),
    ParamSpec::new("final", "Final Delay", Unit::Ms, 0.1, 20.0, 5.0),
    ParamSpec::new("rate", "Modulation Rate", Unit::Hz, 0.02, 10.0, 0.25).log(),
    ParamSpec::new("feedback", "Feedback", Unit::Percent, -95.0, 95.0, 50.0),
    ParamSpec::new("phasing", "Stereo Phasing", Unit::Percent, 0.0, 100.0, 50.0),
    ParamSpec::new("mix", "Mix", Unit::Percent, 0.0, 100.0, 50.0),
];

const STAGES: &[&str] = &["2", "4", "6", "8", "10", "12"];

static PHASER_SPECS: [ParamSpec; 7] = [
    ParamSpec::choice("stages", "Stages", STAGES, 2),
    ParamSpec::new("depth", "Depth", Unit::Percent, 0.0, 100.0, 70.0),
    ParamSpec::new("rate", "Modulation Rate", Unit::Hz, 0.02, 10.0, 0.4).log(),
    ParamSpec::new("feedback", "Feedback", Unit::Percent, -95.0, 95.0, 30.0),
    ParamSpec::new("center", "Center Frequency", Unit::Hz, 200.0, 5000.0, 900.0).log(),
    ParamSpec::new("phasing", "Stereo Phasing", Unit::Percent, 0.0, 100.0, 50.0),
    ParamSpec::new("mix", "Mix", Unit::Percent, 0.0, 100.0, 50.0),
];

/// Chorus and Flanger: modulated short delays mixed with the dry signal.
pub struct ModDelay {
    flanger: bool,
    specs: &'static [ParamSpec],
    values: Vec<f32>,
    rate: f32,
    lfo: Lfo,
    lines: Vec<DelayLine>,
    fb: Vec<f32>,
}

impl ModDelay {
    pub fn chorus() -> ModDelay {
        ModDelay {
            flanger: false,
            specs: &CHORUS_SPECS,
            values: defaults(&CHORUS_SPECS),
            rate: 48_000.0,
            lfo: Lfo::default(),
            lines: Vec::new(),
            fb: Vec::new(),
        }
    }

    pub fn flanger() -> ModDelay {
        ModDelay {
            flanger: true,
            specs: &FLANGER_SPECS,
            values: defaults(&FLANGER_SPECS),
            rate: 48_000.0,
            lfo: Lfo::default(),
            lines: Vec::new(),
            fb: Vec::new(),
        }
    }
}

impl Effect for ModDelay {
    fn specs(&self) -> &'static [ParamSpec] {
        self.specs
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        self.lines = vec![DelayLine::new((0.07 * sample_rate) as usize); channels];
        self.fb = vec![0.0; channels];
        self.update();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.fb.fill(0.0);
        self.lfo.reset();
    }

    fn update(&mut self) {
        let hz = if self.flanger {
            self.values[2]
        } else {
            self.values[3]
        };
        self.lfo.set_rate(hz, self.rate);
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        let v = &self.values;
        let ms = self.rate * 0.001;
        for i in 0..n {
            for (c, ch) in buf.iter_mut().enumerate() {
                let x = ch[i];
                let side = (c % 2) as f32;
                let (wet, fb, mix) = if self.flanger {
                    let (lo, hi) = (v[0].min(v[1]) * ms, v[0].max(v[1]) * ms);
                    let m = 0.5 + 0.5 * self.lfo.peek(side * v[4] / 200.0);
                    (
                        self.lines[c].read(lo + (hi - lo) * m),
                        v[3] / 100.0,
                        v[5] / 100.0,
                    )
                } else {
                    let voices = v[0] as usize + 2;
                    let mut sum = 0.0;
                    for k in 0..voices {
                        let offset = k as f32 / voices as f32 + side * v[5] / 400.0;
                        sum += self.lines[c].read((v[1] + v[2] * self.lfo.peek(offset)) * ms);
                    }
                    (sum / voices as f32, v[4] / 100.0, v[6] / 100.0)
                };
                self.lines[c].push(x + self.fb[c] * fb);
                self.fb[c] = wet;
                ch[i] = x * (1.0 - mix) + wet * mix;
            }
            self.lfo.advance();
        }
    }
}

/// A chain of first-order all-pass filters whose break frequency sweeps with an LFO.
pub struct Phaser {
    values: Vec<f32>,
    rate: f32,
    lfo: Lfo,
    /// Per channel: all-pass states for up to 12 stages, plus the feedback sample.
    state: Vec<[f32; 13]>,
}

impl Phaser {
    pub fn new() -> Phaser {
        Phaser {
            values: defaults(&PHASER_SPECS),
            rate: 48_000.0,
            lfo: Lfo::default(),
            state: Vec::new(),
        }
    }
}

impl Default for Phaser {
    fn default() -> Self {
        Phaser::new()
    }
}

impl Effect for Phaser {
    fn specs(&self) -> &'static [ParamSpec] {
        &PHASER_SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        self.state = vec![[0.0; 13]; channels];
        self.update();
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [0.0; 13]);
        self.lfo.reset();
    }

    fn update(&mut self) {
        self.lfo.set_rate(self.values[2], self.rate);
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        let v = &self.values;
        let stages = 2 * (v[0] as usize + 1);
        let (depth, fb, center, mix) = (v[1] / 100.0, v[3] / 100.0, v[4], v[6] / 100.0);
        for i in 0..n {
            for (c, ch) in buf.iter_mut().enumerate() {
                let m = self.lfo.peek((c % 2) as f32 * v[5] / 200.0);
                // Sweep the break frequency up to two octaves either side of the centre.
                let f = (center * 4f32.powf(m * depth)).min(self.rate * 0.45);
                let t = (std::f32::consts::PI * f / self.rate).tan();
                let a = (t - 1.0) / (t + 1.0);
                let s = &mut self.state[c];
                let mut y = ch[i] + s[12] * fb;
                for z in s.iter_mut().take(stages) {
                    let out = a * y + *z;
                    *z = y - a * out;
                    y = out;
                }
                s[12] = y;
                ch[i] = ch[i] * (1.0 - mix) + y * mix;
            }
            self.lfo.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_dry(mut e: Box<dyn Effect>, mix_index: usize) {
        e.set_param(mix_index, 0.0);
        e.prepare(48_000.0, 2, 4096);
        e.reset();
        let input: Vec<f32> = (0..4096).map(|i| (i as f32 * 0.05).sin()).collect();
        let (mut a, mut b) = (input.clone(), input.clone());
        e.process(&mut [&mut a[..], &mut b[..]]);
        assert_eq!(a, input);
        assert_eq!(b, input);
    }

    #[test]
    fn zero_mix_is_dry() {
        check_dry(Box::new(ModDelay::chorus()), 6);
        check_dry(Box::new(ModDelay::flanger()), 5);
        check_dry(Box::new(Phaser::new()), 6);
    }

    #[test]
    fn phaser_allpass_keeps_level_without_feedback() {
        let mut p = Phaser::new();
        p.set_all(&[2.0, 70.0, 0.4, 0.0, 900.0, 0.0, 100.0]);
        p.prepare(48_000.0, 1, 48_000);
        p.reset();
        let mut x: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / 48_000.0).sin())
            .collect();
        p.process(&mut [&mut x[..]]);
        let peak = x[24_000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(
            (peak - 1.0).abs() < 0.05,
            "all-pass chain is unity gain: {peak}"
        );
    }
}
