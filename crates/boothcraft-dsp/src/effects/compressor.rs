use crate::effect::Effect;
use crate::param::{db_to_gain, defaults, ParamSpec, Unit};

static SPECS: [ParamSpec; 6] = [
    ParamSpec::new("threshold", "Threshold", Unit::Db, -60.0, 0.0, -20.0),
    ParamSpec::new("ratio", "Ratio", Unit::Ratio, 1.0, 30.0, 4.0).log(),
    ParamSpec::new("knee", "Knee", Unit::Db, 0.0, 24.0, 6.0),
    ParamSpec::new("attack", "Attack", Unit::Ms, 0.1, 300.0, 10.0).log(),
    ParamSpec::new("release", "Release", Unit::Ms, 5.0, 3000.0, 100.0).log(),
    ParamSpec::new("makeup", "Output Gain", Unit::Db, 0.0, 30.0, 0.0),
];

/// Output level for input level `x` (dB) under a soft-knee compressor curve.
pub fn static_curve(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    if knee > 0.0 && 2.0 * over.abs() <= knee {
        x + (1.0 / ratio - 1.0) * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        threshold + over / ratio
    } else {
        x
    }
}

/// Feed-forward compressor with stereo-linked peak detection and smoothed gain reduction.
pub struct Compressor {
    values: Vec<f32>,
    rate: f32,
    attack: f32,
    release: f32,
    /// Current gain reduction in dB (≤ 0).
    gr: f32,
}

impl Compressor {
    pub fn new() -> Compressor {
        let mut c = Compressor {
            values: defaults(&SPECS),
            rate: 48_000.0,
            attack: 0.0,
            release: 0.0,
            gr: 0.0,
        };
        c.update();
        c
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Compressor::new()
    }
}

pub(crate) fn time_coeff(rate: f32, ms: f32) -> f32 {
    (-1.0 / (ms * 0.001 * rate).max(1.0)).exp()
}

impl Effect for Compressor {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, _channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        self.update();
    }

    fn reset(&mut self) {
        self.gr = 0.0;
    }

    fn update(&mut self) {
        self.attack = time_coeff(self.rate, self.values[3]);
        self.release = time_coeff(self.rate, self.values[4]);
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let (t, r, k) = (self.values[0], self.values[1], self.values[2]);
        let makeup = self.values[5];
        let n = buf.first().map_or(0, |c| c.len());
        for i in 0..n {
            let peak = buf.iter().fold(0f32, |m, ch| m.max(ch[i].abs()));
            let x = 20.0 * peak.max(1e-9).log10();
            let target = static_curve(x, t, r, k) - x;
            let coeff = if target < self.gr {
                self.attack
            } else {
                self.release
            };
            self.gr = target + (self.gr - target) * coeff;
            let g = db_to_gain(self.gr + makeup);
            for ch in buf.iter_mut() {
                ch[i] *= g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_matches_ratio() {
        assert_eq!(static_curve(-30.0, -20.0, 4.0, 0.0), -30.0);
        assert_eq!(static_curve(-10.0, -20.0, 4.0, 0.0), -17.5);
        // Soft knee is continuous at its edges.
        assert!((static_curve(-23.0, -20.0, 4.0, 6.0) + 23.0).abs() < 1e-5);
        assert!((static_curve(-17.0, -20.0, 4.0, 6.0) - (-20.0 + 3.0 / 4.0)).abs() < 1e-5);
    }

    #[test]
    fn steady_tone_settles_on_the_curve() {
        let mut c = Compressor::new();
        c.set_all(&[-20.0, 4.0, 0.0, 1.0, 200.0, 0.0]);
        c.prepare(48_000.0, 1, 48_000);
        c.reset();
        let amp = db_to_gain(-10.0);
        let mut x: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * amp)
            .collect();
        c.process(&mut [&mut x[..]]);
        let peak = x[40_000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        let db = 20.0 * peak.log10();
        assert!((db + 17.5).abs() < 0.5, "{db}");
    }
}
