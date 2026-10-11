use crate::biquad::{Biquad, Coeffs};
use crate::effect::Effect;
use crate::effects::compressor::time_coeff;
use crate::param::{db_to_gain, defaults, ParamSpec, Unit};

const MODES: &[&str] = &["Broadband", "Multiband"];

static SPECS: [ParamSpec; 6] = [
    ParamSpec::choice("mode", "Mode", MODES, 1),
    ParamSpec::new("threshold", "Threshold", Unit::Db, -60.0, 0.0, -30.0),
    ParamSpec::new(
        "freq",
        "Center Frequency",
        Unit::Hz,
        2000.0,
        16_000.0,
        6500.0,
    )
    .log(),
    ParamSpec::new("q", "Bandwidth (Q)", Unit::None, 0.5, 6.0, 1.5).log(),
    ParamSpec::new("max", "Maximum Reduction", Unit::Db, 0.0, 40.0, 24.0),
    ParamSpec::toggle("listen", "Output Sibilance Only", false),
];

/// Detects energy in a band (sibilance) and turns it down above a threshold: either the whole
/// signal (broadband) or only that band (multiband).
pub struct DeEsser {
    values: Vec<f32>,
    rate: f32,
    detect: Biquad,
    band: Biquad,
    env: f32,
    attack: f32,
    release: f32,
}

impl DeEsser {
    pub fn new() -> DeEsser {
        let mut d = DeEsser {
            values: defaults(&SPECS),
            rate: 48_000.0,
            detect: Biquad::default(),
            band: Biquad::default(),
            env: 0.0,
            attack: 0.0,
            release: 0.0,
        };
        d.update();
        d
    }
}

impl Default for DeEsser {
    fn default() -> Self {
        DeEsser::new()
    }
}

impl Effect for DeEsser {
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
        self.detect.prepare(1);
        self.band.prepare(channels);
        self.update();
    }

    fn reset(&mut self) {
        self.detect.reset();
        self.band.reset();
        self.env = 0.0;
    }

    fn update(&mut self) {
        let c = Coeffs::bandpass(self.rate, self.values[2], self.values[3]);
        self.detect.coeffs = c;
        self.band.coeffs = c;
        self.attack = time_coeff(self.rate, 1.0);
        self.release = time_coeff(self.rate, 60.0);
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let multiband = self.values[0] >= 0.5;
        let (threshold, max) = (self.values[1], self.values[4]);
        let listen = self.values[5] >= 0.5;
        let n = buf.first().map_or(0, |c| c.len());
        let k = 1.0 / buf.len().max(1) as f32;
        for i in 0..n {
            let mono = buf.iter().map(|ch| ch[i]).sum::<f32>() * k;
            let s = self.detect.tick(0, mono).abs();
            let coeff = if s > self.env {
                self.attack
            } else {
                self.release
            };
            self.env = s + (self.env - s) * coeff;
            let level = 20.0 * self.env.max(1e-9).log10();
            let reduction = (level - threshold).clamp(0.0, max);
            let g = db_to_gain(-reduction);
            for (c, ch) in buf.iter_mut().enumerate() {
                let x = ch[i];
                let band = if multiband || listen {
                    self.band.tick(c, x)
                } else {
                    0.0
                };
                ch[i] = if listen {
                    band * g
                } else if multiband {
                    x + band * (g - 1.0)
                } else {
                    x * g
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, amp: f32) -> Vec<f32> {
        (0..48_000)
            .map(|i| (i as f32 * std::f32::consts::TAU * f / 48_000.0).sin() * amp)
            .collect()
    }

    fn level_db(v: &[f32]) -> f32 {
        20.0 * v[30_000..].iter().fold(0f32, |m, x| m.max(x.abs())).log10()
    }

    #[test]
    fn sibilance_is_reduced_and_lows_are_not() {
        let mut d = DeEsser::new();
        d.prepare(48_000.0, 1, 48_000);
        d.reset();
        let mut ess = tone(6500.0, 0.5);
        d.process(&mut [&mut ess[..]]);
        assert!(
            level_db(&ess) < -12.0,
            "sibilant tone at {} dB",
            level_db(&ess)
        );
        d.reset();
        let mut low = tone(200.0, 0.5);
        d.process(&mut [&mut low[..]]);
        assert!(
            (level_db(&low) - 20.0 * 0.5f32.log10()).abs() < 0.5,
            "200 Hz untouched in multiband"
        );
    }
}
