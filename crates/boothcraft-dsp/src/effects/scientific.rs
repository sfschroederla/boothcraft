use crate::biquad::{butterworth_q, Biquad, Coeffs};
use crate::effect::Effect;
use crate::param::{db_to_gain, defaults, ParamSpec, Smoothed, Unit};

const TYPES: &[&str] = &["Butterworth", "Chebyshev"];
const MODES: &[&str] = &["Low Pass", "High Pass", "Band Pass", "Band Stop"];
const ORDERS: &[&str] = &["2", "4", "6", "8"];

static SPECS: [ParamSpec; 7] = [
    ParamSpec::choice("type", "Type", TYPES, 0),
    ParamSpec::choice("mode", "Mode", MODES, 0),
    ParamSpec::choice("order", "Order", ORDERS, 1),
    ParamSpec::new("low", "Cutoff / Low Edge", Unit::Hz, 20.0, 20_000.0, 1000.0).log(),
    ParamSpec::new("high", "High Edge", Unit::Hz, 20.0, 20_000.0, 4000.0).log(),
    ParamSpec::new("ripple", "Ripple (Chebyshev)", Unit::Db, 0.01, 3.0, 0.5).log(),
    ParamSpec::new("gain", "Output Gain", Unit::Db, -24.0, 24.0, 0.0),
];

/// Up to four second-order sections, stored inline so designing never allocates.
#[derive(Clone, Copy, Debug)]
pub struct Sections {
    pub coeffs: [Coeffs; 4],
    pub len: usize,
    /// Gain that keeps the passband at or below 0 dB.
    pub gain: f32,
}

impl Sections {
    const NONE: Sections = Sections {
        coeffs: [Coeffs::IDENTITY; 4],
        len: 0,
        gain: 1.0,
    };

    pub fn as_slice(&self) -> &[Coeffs] {
        &self.coeffs[..self.len]
    }
}

/// Section `k` of a normalized analog low-pass prototype (cutoff 1 rad/s) as `(ω0, Q)`.
fn prototype_section(chebyshev: bool, order: usize, ripple_db: f32, k: usize) -> (f64, f64) {
    if !chebyshev {
        return (1.0, butterworth_q(order, k) as f64);
    }
    let n = order as f64;
    let eps = (10f64.powf(ripple_db as f64 / 10.0) - 1.0).sqrt();
    let v = (1.0 / eps).asinh() / n;
    let theta = (2 * k + 1) as f64 * std::f64::consts::PI / (2.0 * n);
    let (sigma, omega) = (v.sinh() * theta.sin(), v.cosh() * theta.cos());
    let w0 = (sigma * sigma + omega * omega).sqrt();
    (w0, w0 / (2.0 * sigma))
}

/// Even-order Chebyshev has its DC gain at the bottom of the ripple; scale so peaks sit at 0 dB.
fn passband_gain(chebyshev: bool, ripple_db: f32) -> f32 {
    if chebyshev {
        10f32.powf(-ripple_db / 20.0)
    } else {
        1.0
    }
}

/// Digital design frequency for a section whose analog frequency is `scale` times the cutoff `f`.
/// The cutoff is pre-warped once, so every section shares the same bilinear mapping and the
/// cascade matches the analog prototype exactly at the cutoff.
fn section_freq(fs: f32, f: f32, scale: f64) -> f32 {
    let pi = std::f64::consts::PI;
    let warped = (pi * (f as f64).min(fs as f64 * 0.49) / fs as f64).tan();
    ((warped * scale).atan() * fs as f64 / pi) as f32
}

fn sections(fs: f32, f: f32, chebyshev: bool, order: usize, ripple: f32, high: bool) -> Sections {
    let order = order.clamp(2, 8) & !1;
    let mut s = Sections {
        coeffs: [Coeffs::IDENTITY; 4],
        len: order / 2,
        gain: passband_gain(chebyshev, ripple),
    };
    for k in 0..s.len {
        let (w0, q) = prototype_section(chebyshev, order, ripple, k);
        s.coeffs[k] = if high {
            Coeffs::highpass(fs, section_freq(fs, f, 1.0 / w0), q as f32)
        } else {
            Coeffs::lowpass(fs, section_freq(fs, f, w0), q as f32)
        };
    }
    s
}

pub fn lowpass_sections(fs: f32, f: f32, chebyshev: bool, order: usize, ripple: f32) -> Sections {
    sections(fs, f, chebyshev, order, ripple, false)
}

pub fn highpass_sections(fs: f32, f: f32, chebyshev: bool, order: usize, ripple: f32) -> Sections {
    sections(fs, f, chebyshev, order, ripple, true)
}

/// Butterworth or Chebyshev type I filters of order 2–8 built from cascaded biquads. Band-pass
/// cascades a high-pass and a low-pass; band-stop sums a low-pass and a high-pass in parallel.
pub struct Scientific {
    values: Vec<f32>,
    a: Vec<Biquad>,
    b: Vec<Biquad>,
    gain_a: f32,
    gain_b: f32,
    output: Smoothed,
    scratch: Vec<Vec<f32>>,
    rate: f32,
    parallel: bool,
}

impl Scientific {
    pub fn new() -> Scientific {
        let mut s = Scientific {
            values: defaults(&SPECS),
            a: Vec::new(),
            b: Vec::new(),
            gain_a: 1.0,
            gain_b: 1.0,
            output: Smoothed::new(1.0),
            scratch: Vec::new(),
            rate: 48_000.0,
            parallel: false,
        };
        s.update();
        s
    }

    fn mode(&self) -> usize {
        self.values[1] as usize
    }
}

impl Default for Scientific {
    fn default() -> Self {
        Scientific::new()
    }
}

/// Most sections a chain can need: band-pass at order 8 is 4 high-pass + 4 low-pass sections.
const MAX_A: usize = 8;
const MAX_B: usize = 4;

/// Load coefficients into a pre-allocated chain, leaving the rest as identity (skipped).
/// Never allocates, so it is safe on the audio thread.
fn install(dst: &mut [Biquad], coeffs: &[Coeffs]) {
    for (i, b) in dst.iter_mut().enumerate() {
        b.coeffs = coeffs.get(i).copied().unwrap_or(Coeffs::IDENTITY);
    }
}

impl Effect for Scientific {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, max_block: usize) {
        self.rate = sample_rate;
        self.scratch = vec![vec![0.0; max_block]; channels];
        // Allocate the most sections any setting can need, so `update` never allocates.
        self.a = vec![Biquad::default(); MAX_A];
        self.b = vec![Biquad::default(); MAX_B];
        for s in self.a.iter_mut().chain(self.b.iter_mut()) {
            s.prepare(channels);
        }
        self.output.prepare(sample_rate, 20.0);
        self.update();
    }

    fn reset(&mut self) {
        for s in self.a.iter_mut().chain(self.b.iter_mut()) {
            s.reset();
        }
        self.output.snap();
    }

    fn update(&mut self) {
        let cheb = self.values[0] >= 0.5;
        let order = 2 * (self.values[2] as usize + 1);
        let (lo, hi, ripple, fs) = (
            self.values[3],
            self.values[4].max(self.values[3]),
            self.values[5],
            self.rate,
        );
        let (first, second, parallel) = match self.mode() {
            0 => (
                lowpass_sections(fs, lo, cheb, order, ripple),
                Sections::NONE,
                Sections::NONE,
            ),
            1 => (
                highpass_sections(fs, lo, cheb, order, ripple),
                Sections::NONE,
                Sections::NONE,
            ),
            // Band-pass: high-pass then low-pass in one series chain.
            2 => (
                highpass_sections(fs, lo, cheb, order, ripple),
                lowpass_sections(fs, hi, cheb, order, ripple),
                Sections::NONE,
            ),
            // Band-stop: low-pass below the band in parallel with high-pass above it.
            _ => (
                lowpass_sections(fs, lo, cheb, order, ripple),
                Sections::NONE,
                highpass_sections(fs, hi, cheb, order, ripple),
            ),
        };
        let mut chain = [Coeffs::IDENTITY; MAX_A];
        for (dst, c) in chain
            .iter_mut()
            .zip(first.as_slice().iter().chain(second.as_slice()))
        {
            *dst = *c;
        }
        install(&mut self.a, &chain);
        install(&mut self.b, parallel.as_slice());
        self.gain_a = first.gain * if second.len > 0 { second.gain } else { 1.0 };
        self.gain_b = parallel.gain;
        self.parallel = parallel.len > 0;
        self.output.set(db_to_gain(self.values[6]));
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        let parallel = self.parallel;
        if parallel {
            for (s, ch) in self.scratch.iter_mut().zip(buf.iter()) {
                s[..n].copy_from_slice(ch);
            }
        }
        for sec in &mut self.a {
            for (c, ch) in buf.iter_mut().enumerate() {
                sec.run(c, ch);
            }
        }
        if parallel {
            for sec in &mut self.b {
                for (c, s) in self.scratch.iter_mut().enumerate() {
                    sec.run(c, &mut s[..n]);
                }
            }
        }
        for i in 0..n {
            let g = self.output.tick();
            for (c, ch) in buf.iter_mut().enumerate() {
                let extra = if parallel {
                    self.scratch[c][i] * self.gain_b
                } else {
                    0.0
                };
                ch[i] = (ch[i] * self.gain_a + extra) * g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    fn total(s: &Sections, f: f32) -> f32 {
        s.as_slice()
            .iter()
            .map(|c| c.magnitude_db(FS, f))
            .sum::<f32>()
            + 20.0 * s.gain.log10()
    }

    #[test]
    fn butterworth_lowpass_order_4() {
        let c = lowpass_sections(FS, 1000.0, false, 4, 0.0);
        assert!((total(&c, 1000.0) + 3.01).abs() < 0.01);
        assert!(total(&c, 100.0).abs() < 0.01);
        let at_2k = total(&c, 2000.0);
        assert!((-25.5..-23.5).contains(&at_2k), "{at_2k}");
    }

    #[test]
    fn chebyshev_ripple_stays_in_band() {
        for order in [2, 4, 6, 8] {
            let c = lowpass_sections(FS, 2000.0, true, order, 1.0);
            for f in (20..1800).step_by(20) {
                let m = total(&c, f as f32);
                assert!((-1.01..=0.01).contains(&m), "order {order} at {f} Hz: {m}");
            }
            assert!(total(&c, 8000.0) < -20.0, "order {order} rolls off");
        }
    }

    #[test]
    fn highpass_mirrors_lowpass() {
        let c = highpass_sections(FS, 1000.0, false, 2, 0.0);
        assert!((total(&c, 1000.0) + 3.01).abs() < 0.1);
        assert!(total(&c, 10_000.0).abs() < 0.05);
    }
}
