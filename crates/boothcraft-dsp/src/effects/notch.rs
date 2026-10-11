use crate::biquad::{Biquad, Coeffs};
use crate::effect::Effect;
use crate::param::{defaults, leak, ParamSpec, Unit};
use std::sync::LazyLock;

pub const NOTCHES: usize = 6;

/// Each notch: on, frequency, depth (dB), Q. Defaults sit on mains-hum harmonics; only the first is on.
static SPECS: LazyLock<Vec<ParamSpec>> = LazyLock::new(|| {
    let mut v = Vec::new();
    for k in 0..NOTCHES {
        let n = k + 1;
        v.push(ParamSpec::toggle(
            leak(format!("n{n}_on")),
            leak(format!("Notch {n}")),
            k == 0,
        ));
        v.push(
            ParamSpec::new(
                leak(format!("n{n}_freq")),
                leak(format!("Notch {n} Freq")),
                Unit::Hz,
                20.0,
                20_000.0,
                60.0 * n as f32,
            )
            .log(),
        );
        v.push(ParamSpec::new(
            leak(format!("n{n}_depth")),
            leak(format!("Notch {n} Depth")),
            Unit::Db,
            -90.0,
            0.0,
            -30.0,
        ));
        v.push(
            ParamSpec::new(
                leak(format!("n{n}_q")),
                leak(format!("Notch {n} Q")),
                Unit::None,
                1.0,
                100.0,
                30.0,
            )
            .log(),
        );
    }
    v
});

pub struct Notch {
    values: Vec<f32>,
    filters: [Biquad; NOTCHES],
    rate: f32,
}

impl Notch {
    pub fn new() -> Notch {
        let mut n = Notch {
            values: defaults(&SPECS),
            filters: Default::default(),
            rate: 48_000.0,
        };
        n.update();
        n
    }
}

impl Default for Notch {
    fn default() -> Self {
        Notch::new()
    }
}

impl Effect for Notch {
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
        for f in &mut self.filters {
            f.prepare(channels);
        }
        self.update();
    }

    fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
    }

    fn update(&mut self) {
        for (k, f) in self.filters.iter_mut().enumerate() {
            let v = &self.values[4 * k..4 * k + 4];
            f.coeffs = if v[0] >= 0.5 {
                Coeffs::peaking(self.rate, v[1], v[3], v[2])
            } else {
                Coeffs::IDENTITY
            };
        }
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        for f in &mut self.filters {
            for (c, ch) in buf.iter_mut().enumerate() {
                f.run(c, ch);
            }
        }
    }
}
