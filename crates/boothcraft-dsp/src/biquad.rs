//! Second-order IIR sections (RBJ "Audio EQ Cookbook") and their frequency response.

use std::f64::consts::PI;

/// Normalized biquad coefficients (a0 = 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

/// Keep design frequencies safely below Nyquist.
fn clamp_freq(fs: f32, f: f32) -> f64 {
    (f as f64).clamp(1.0, fs as f64 * 0.49)
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn from(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Coeffs {
        Coeffs {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    fn w0(fs: f32, f: f32) -> (f64, f64, f64) {
        let w = 2.0 * PI * clamp_freq(fs, f) / fs as f64;
        (w, w.cos(), w.sin())
    }

    pub fn lowpass(fs: f32, f: f32, q: f32) -> Coeffs {
        let (_, c, s) = Self::w0(fs, f);
        let alpha = s / (2.0 * q as f64);
        Self::from(
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    pub fn highpass(fs: f32, f: f32, q: f32) -> Coeffs {
        let (_, c, s) = Self::w0(fs, f);
        let alpha = s / (2.0 * q as f64);
        Self::from(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    /// Band-pass with 0 dB peak gain.
    pub fn bandpass(fs: f32, f: f32, q: f32) -> Coeffs {
        let (_, c, s) = Self::w0(fs, f);
        let alpha = s / (2.0 * q as f64);
        Self::from(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn peaking(fs: f32, f: f32, q: f32, gain_db: f32) -> Coeffs {
        let (_, c, s) = Self::w0(fs, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = s / (2.0 * q as f64);
        Self::from(
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        )
    }

    /// Shelves use slope S = 1 (the steepest without overshoot).
    pub fn low_shelf(fs: f32, f: f32, gain_db: f32) -> Coeffs {
        let (_, c, s) = Self::w0(fs, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = s / 2.0 * std::f64::consts::SQRT_2;
        let k = 2.0 * a.sqrt() * alpha;
        Self::from(
            a * ((a + 1.0) - (a - 1.0) * c + k),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - k),
            (a + 1.0) + (a - 1.0) * c + k,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - k,
        )
    }

    pub fn high_shelf(fs: f32, f: f32, gain_db: f32) -> Coeffs {
        let (_, c, s) = Self::w0(fs, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = s / 2.0 * std::f64::consts::SQRT_2;
        let k = 2.0 * a.sqrt() * alpha;
        Self::from(
            a * ((a + 1.0) + (a - 1.0) * c + k),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - k),
            (a + 1.0) - (a - 1.0) * c + k,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - k,
        )
    }

    /// |H| in dB at frequency `f`.
    pub fn magnitude_db(&self, fs: f32, f: f32) -> f32 {
        let w = 2.0 * PI * f as f64 / fs as f64;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (b0, b1, b2, a1, a2) = (
            self.b0 as f64,
            self.b1 as f64,
            self.b2 as f64,
            self.a1 as f64,
            self.a2 as f64,
        );
        let num_re = b0 + b1 * c1 + b2 * c2;
        let num_im = -(b1 * s1 + b2 * s2);
        let den_re = 1.0 + a1 * c1 + a2 * c2;
        let den_im = -(a1 * s1 + a2 * s2);
        let mag2 =
            (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im).max(1e-300);
        (10.0 * mag2.max(1e-30).log10()) as f32
    }
}

/// One biquad with independent state per channel (transposed direct form II).
#[derive(Clone, Debug)]
pub struct Biquad {
    pub coeffs: Coeffs,
    state: Vec<[f32; 2]>,
}

impl Default for Biquad {
    fn default() -> Self {
        Biquad {
            coeffs: Coeffs::IDENTITY,
            state: Vec::new(),
        }
    }
}

impl Biquad {
    pub fn prepare(&mut self, channels: usize) {
        self.state = vec![[0.0; 2]; channels];
    }

    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [0.0; 2]);
    }

    #[inline]
    pub fn tick(&mut self, ch: usize, x: f32) -> f32 {
        let c = &self.coeffs;
        let Some(z) = self.state.get_mut(ch) else {
            return x;
        };
        let y = c.b0 * x + z[0];
        z[0] = c.b1 * x - c.a1 * y + z[1];
        z[1] = c.b2 * x - c.a2 * y;
        y
    }

    /// Filter a whole channel in place.
    pub fn run(&mut self, ch: usize, buf: &mut [f32]) {
        if self.coeffs == Coeffs::IDENTITY {
            return;
        }
        for x in buf.iter_mut() {
            *x = self.tick(ch, *x);
        }
    }
}

/// Q of section `k` (0-based) of an `order`-th order Butterworth filter (even order). Allocation-free.
pub fn butterworth_q(order: usize, k: usize) -> f32 {
    let n = order.max(2) & !1;
    (1.0 / (2.0 * ((2 * k + 1) as f64 * PI / (2 * n) as f64).cos())) as f32
}

/// All section Qs of an `order`-th order Butterworth filter.
pub fn butterworth_qs(order: usize) -> Vec<f32> {
    let n = order.max(2) & !1;
    (0..n / 2).map(|k| butterworth_q(n, k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    #[test]
    fn peaking_hits_its_gain_at_centre() {
        for g in [-12.0, -3.0, 6.0, 15.0] {
            let c = Coeffs::peaking(FS, 1000.0, 1.4, g);
            assert!((c.magnitude_db(FS, 1000.0) - g).abs() < 0.05, "{g}");
            assert!(c.magnitude_db(FS, 20.0).abs() < 0.2, "flat far away");
        }
        let flat = Coeffs::peaking(FS, 1000.0, 1.0, 0.0);
        assert!(flat.magnitude_db(FS, 1000.0).abs() < 1e-4);
    }

    #[test]
    fn shelves_and_passes() {
        let ls = Coeffs::low_shelf(FS, 200.0, 6.0);
        assert!((ls.magnitude_db(FS, 20.0) - 6.0).abs() < 0.1);
        assert!(ls.magnitude_db(FS, 10_000.0).abs() < 0.1);
        let hs = Coeffs::high_shelf(FS, 4000.0, -9.0);
        assert!((hs.magnitude_db(FS, 20_000.0) + 9.0).abs() < 0.3);
        let lp = Coeffs::lowpass(FS, 1000.0, std::f32::consts::FRAC_1_SQRT_2);
        assert!(
            (lp.magnitude_db(FS, 1000.0) + 3.01).abs() < 0.05,
            "-3 dB at cutoff"
        );
        assert!(lp.magnitude_db(FS, 10_000.0) < -35.0);
        let hp = Coeffs::highpass(FS, 1000.0, std::f32::consts::FRAC_1_SQRT_2);
        assert!((hp.magnitude_db(FS, 1000.0) + 3.01).abs() < 0.05);
        let bp = Coeffs::bandpass(FS, 2000.0, 2.0);
        assert!(bp.magnitude_db(FS, 2000.0).abs() < 0.01);
    }

    #[test]
    fn processing_matches_the_response() {
        let mut b = Biquad {
            coeffs: Coeffs::peaking(FS, 1000.0, 1.0, -12.0),
            ..Default::default()
        };
        b.prepare(1);
        let mut sine: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / FS).sin())
            .collect();
        b.run(0, &mut sine);
        let peak = sine[24_000..].iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(
            (20.0 * peak.log10() + 12.0).abs() < 0.1,
            "{}",
            20.0 * peak.log10()
        );
    }

    #[test]
    fn butterworth_sections() {
        assert_eq!(butterworth_qs(2), vec![std::f32::consts::FRAC_1_SQRT_2]);
        let q4 = butterworth_qs(4);
        assert!((q4[0] - 0.5412).abs() < 1e-3 && (q4[1] - 1.3066).abs() < 1e-3);
    }
}
