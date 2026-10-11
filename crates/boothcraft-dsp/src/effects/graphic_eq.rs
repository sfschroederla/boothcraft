use crate::biquad::{Biquad, Coeffs};
use crate::effect::Effect;
use crate::param::{db_to_gain, defaults, leak, ParamSpec, Smoothed, Unit};
use std::sync::LazyLock;

pub const OCTAVE: &[f32] = &[
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16_000.0,
];
pub const HALF_OCTAVE: &[f32] = &[
    31.5, 44.5, 63.0, 89.0, 125.0, 177.0, 250.0, 354.0, 500.0, 707.0, 1000.0, 1414.0, 2000.0,
    2828.0, 4000.0, 5657.0, 8000.0, 11_314.0, 16_000.0, 22_627.0,
];
pub const THIRD_OCTAVE: &[f32] = &[
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0,
    630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0,
    10_000.0, 12_500.0, 16_000.0, 20_000.0,
];

fn band_specs(freqs: &[f32]) -> Vec<ParamSpec> {
    let mut v: Vec<ParamSpec> = freqs
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let name = if *f >= 1000.0 {
                format!("{} kHz", f / 1000.0)
            } else {
                format!("{f} Hz")
            };
            ParamSpec::new(
                leak(format!("b{}", i + 1)),
                leak(name),
                Unit::Db,
                -20.0,
                20.0,
                0.0,
            )
        })
        .collect();
    v.push(ParamSpec::new(
        "master",
        "Master Gain",
        Unit::Db,
        -20.0,
        20.0,
        0.0,
    ));
    v
}

static SPECS_10: LazyLock<Vec<ParamSpec>> = LazyLock::new(|| band_specs(OCTAVE));
static SPECS_20: LazyLock<Vec<ParamSpec>> = LazyLock::new(|| band_specs(HALF_OCTAVE));
static SPECS_30: LazyLock<Vec<ParamSpec>> = LazyLock::new(|| band_specs(THIRD_OCTAVE));

/// A bank of constant-Q peaking filters at ISO centre frequencies.
pub struct GraphicEq {
    freqs: &'static [f32],
    q: f32,
    specs: &'static [ParamSpec],
    values: Vec<f32>,
    bands: Vec<Biquad>,
    rate: f32,
    master: Smoothed,
}

impl GraphicEq {
    fn with(freqs: &'static [f32], q: f32, specs: &'static [ParamSpec]) -> GraphicEq {
        let mut e = GraphicEq {
            freqs,
            q,
            specs,
            values: defaults(specs),
            bands: vec![Biquad::default(); freqs.len()],
            rate: 48_000.0,
            master: Smoothed::new(1.0),
        };
        e.update();
        e
    }

    pub fn ten() -> GraphicEq {
        GraphicEq::with(OCTAVE, 1.41, &SPECS_10)
    }

    pub fn twenty() -> GraphicEq {
        GraphicEq::with(HALF_OCTAVE, 2.87, &SPECS_20)
    }

    pub fn thirty() -> GraphicEq {
        GraphicEq::with(THIRD_OCTAVE, 4.32, &SPECS_30)
    }

    /// Centre frequency of each band slider.
    pub fn frequencies(&self) -> &'static [f32] {
        self.freqs
    }
}

impl Effect for GraphicEq {
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
        for b in &mut self.bands {
            b.prepare(channels);
        }
        self.master.prepare(sample_rate, 20.0);
        self.update();
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            b.reset();
        }
        self.master.snap();
    }

    fn update(&mut self) {
        for (i, b) in self.bands.iter_mut().enumerate() {
            let g = self.values[i];
            b.coeffs = if g == 0.0 {
                Coeffs::IDENTITY
            } else {
                Coeffs::peaking(self.rate, self.freqs[i], self.q, g)
            };
        }
        self.master.set(db_to_gain(self.values[self.freqs.len()]));
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        for b in &mut self.bands {
            for (c, ch) in buf.iter_mut().enumerate() {
                b.run(c, ch);
            }
        }
        let n = buf.first().map_or(0, |c| c.len());
        for i in 0..n {
            let g = self.master.tick();
            for ch in buf.iter_mut() {
                ch[i] *= g;
            }
        }
    }
}
