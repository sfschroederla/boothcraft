use crate::biquad::{butterworth_q, Biquad, Coeffs};
use crate::effect::Effect;
use crate::param::{db_to_gain, defaults, leak, ParamSpec, Smoothed, Unit};
use std::sync::LazyLock;

pub const PEAKS: usize = 5;
const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct"];

/// Parameter indices.
pub mod idx {
    pub const HP_ON: usize = 0;
    pub const HP_FREQ: usize = 1;
    pub const HP_SLOPE: usize = 2;
    pub const LS_ON: usize = 3;
    pub const LS_FREQ: usize = 4;
    pub const LS_GAIN: usize = 5;
    /// Peak band `k` uses `PEAK + 4k ..`: on, freq, gain, q.
    pub const PEAK: usize = 6;
    pub const HS_ON: usize = 26;
    pub const HS_FREQ: usize = 27;
    pub const HS_GAIN: usize = 28;
    pub const LP_ON: usize = 29;
    pub const LP_FREQ: usize = 30;
    pub const LP_SLOPE: usize = 31;
    pub const OUTPUT: usize = 32;
}

pub static SPECS: LazyLock<Vec<ParamSpec>> = LazyLock::new(|| {
    let freq = |id: &'static str, name: &'static str, f: f32| {
        ParamSpec::new(id, name, Unit::Hz, 20.0, 20_000.0, f).log()
    };
    let gain =
        |id: &'static str, name: &'static str| ParamSpec::new(id, name, Unit::Db, -24.0, 24.0, 0.0);
    let mut v = vec![
        ParamSpec::toggle("hp_on", "High-pass", false),
        freq("hp_freq", "High-pass Freq", 40.0),
        ParamSpec::choice("hp_slope", "High-pass Slope", SLOPES, 0),
        ParamSpec::toggle("ls_on", "Low Shelf", true),
        freq("ls_freq", "Low Shelf Freq", 100.0),
        gain("ls_gain", "Low Shelf Gain"),
    ];
    for (k, f) in [200.0, 500.0, 1000.0, 2500.0, 6000.0]
        .into_iter()
        .enumerate()
    {
        let n = k + 1;
        v.push(ParamSpec::toggle(
            leak(format!("p{n}_on")),
            leak(format!("Band {n}")),
            true,
        ));
        v.push(freq(
            leak(format!("p{n}_freq")),
            leak(format!("Band {n} Freq")),
            f,
        ));
        v.push(gain(
            leak(format!("p{n}_gain")),
            leak(format!("Band {n} Gain")),
        ));
        v.push(
            ParamSpec::new(
                leak(format!("p{n}_q")),
                leak(format!("Band {n} Q")),
                Unit::None,
                0.1,
                16.0,
                1.0,
            )
            .log(),
        );
    }
    v.extend([
        ParamSpec::toggle("hs_on", "High Shelf", true),
        freq("hs_freq", "High Shelf Freq", 8000.0),
        gain("hs_gain", "High Shelf Gain"),
        ParamSpec::toggle("lp_on", "Low-pass", false),
        freq("lp_freq", "Low-pass Freq", 18_000.0),
        ParamSpec::choice("lp_slope", "Low-pass Slope", SLOPES, 0),
        ParamSpec::new("output", "Output Gain", Unit::Db, -24.0, 24.0, 0.0),
    ]);
    debug_assert_eq!(v.len(), idx::OUTPUT + 1);
    v
});

const SECTIONS: usize = 2 + 1 + PEAKS + 1 + 2;

/// Coefficients for every section (disabled ones are identity). Shared by the effect and the
/// response graph so they always agree.
pub fn design(values: &[f32], fs: f32) -> [Coeffs; SECTIONS] {
    let on = |i: usize| values[i] >= 0.5;
    let mut c = [Coeffs::IDENTITY; SECTIONS];
    // 12 dB/oct is one Butterworth section, 24 dB/oct two.
    let order = |slope: f32| if slope >= 0.5 { 4 } else { 2 };
    if on(idx::HP_ON) {
        let n = order(values[idx::HP_SLOPE]);
        for (k, sec) in c.iter_mut().take(n / 2).enumerate() {
            *sec = Coeffs::highpass(fs, values[idx::HP_FREQ], butterworth_q(n, k));
        }
    }
    if on(idx::LS_ON) {
        c[2] = Coeffs::low_shelf(fs, values[idx::LS_FREQ], values[idx::LS_GAIN]);
    }
    for k in 0..PEAKS {
        let b = idx::PEAK + 4 * k;
        if on(b) {
            c[3 + k] = Coeffs::peaking(fs, values[b + 1], values[b + 3], values[b + 2]);
        }
    }
    if on(idx::HS_ON) {
        c[3 + PEAKS] = Coeffs::high_shelf(fs, values[idx::HS_FREQ], values[idx::HS_GAIN]);
    }
    if on(idx::LP_ON) {
        let n = order(values[idx::LP_SLOPE]);
        for (k, sec) in c[4 + PEAKS..].iter_mut().take(n / 2).enumerate() {
            *sec = Coeffs::lowpass(fs, values[idx::LP_FREQ], butterworth_q(n, k));
        }
    }
    c
}

/// Total response in dB at `f`, including output gain.
pub fn response_db(values: &[f32], fs: f32, f: f32) -> f32 {
    design(values, fs)
        .iter()
        .filter(|c| **c != Coeffs::IDENTITY)
        .map(|c| c.magnitude_db(fs, f))
        .sum::<f32>()
        + values[idx::OUTPUT]
}

pub struct ParametricEq {
    values: Vec<f32>,
    sections: [Biquad; SECTIONS],
    rate: f32,
    output: Smoothed,
}

impl ParametricEq {
    pub fn new() -> ParametricEq {
        let mut e = ParametricEq {
            values: defaults(&SPECS),
            sections: Default::default(),
            rate: 48_000.0,
            output: Smoothed::new(1.0),
        };
        e.update();
        e
    }
}

impl Default for ParametricEq {
    fn default() -> Self {
        ParametricEq::new()
    }
}

impl Effect for ParametricEq {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        for s in &mut self.sections {
            s.prepare(channels);
        }
        self.output.prepare(sample_rate, 20.0);
        self.update();
    }

    fn reset(&mut self) {
        for s in &mut self.sections {
            s.reset();
        }
        self.output.snap();
    }

    fn update(&mut self) {
        for (s, c) in self
            .sections
            .iter_mut()
            .zip(design(&self.values, self.rate))
        {
            s.coeffs = c;
        }
        self.output.set(db_to_gain(self.values[idx::OUTPUT]));
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        for s in &mut self.sections {
            for (c, ch) in buf.iter_mut().enumerate() {
                s.run(c, ch);
            }
        }
        let n = buf.first().map_or(0, |c| c.len());
        for i in 0..n {
            let g = self.output.tick();
            for ch in buf.iter_mut() {
                ch[i] *= g;
            }
        }
    }
}
