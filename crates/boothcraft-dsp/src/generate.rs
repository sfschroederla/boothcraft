//! Generators: tones (with sweeps), coloured noise and DTMF.

use crate::param::db_to_gain;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    Triangle,
    Square,
    Sawtooth,
}

impl Waveform {
    pub const ALL: [Waveform; 4] = [
        Waveform::Sine,
        Waveform::Triangle,
        Waveform::Square,
        Waveform::Sawtooth,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Waveform::Sine => "Sine",
            Waveform::Triangle => "Triangle",
            Waveform::Square => "Square",
            Waveform::Sawtooth => "Sawtooth",
        }
    }
}

/// PolyBLEP correction around a discontinuity, which removes most of the aliasing of naive
/// square and saw waves. `t` is the phase in 0..1, `dt` the phase increment.
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// A tone from `start_hz` to `end_hz` (a logarithmic sweep when they differ).
pub fn tone(
    wave: Waveform,
    start_hz: f32,
    end_hz: f32,
    amplitude_db: f32,
    seconds: f64,
    rate: u32,
) -> Vec<f32> {
    let n = (seconds * rate as f64).round().max(0.0) as usize;
    let amp = db_to_gain(amplitude_db);
    let (f0, f1) = (start_hz.max(0.1) as f64, end_hz.max(0.1) as f64);
    let mut phase = 0f32;
    (0..n)
        .map(|i| {
            let t = if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            };
            let f = (f0 * (f1 / f0).powf(t)).min(rate as f64 * 0.49);
            let dt = (f / rate as f64) as f32;
            let v = match wave {
                Waveform::Sine => (phase * std::f32::consts::TAU).sin(),
                Waveform::Sawtooth => 2.0 * phase - 1.0 - poly_blep(phase, dt),
                Waveform::Square => {
                    let sq = if phase < 0.5 { 1.0 } else { -1.0 };
                    sq + poly_blep(phase, dt) - poly_blep((phase + 0.5).fract(), dt)
                }
                // A triangle's harmonics fall at 12 dB/octave, so the direct form aliases little.
                Waveform::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
            };
            phase = (phase + dt).fract();
            v * amp
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseColor {
    White,
    Pink,
    Brown,
}

impl NoiseColor {
    pub const ALL: [NoiseColor; 3] = [NoiseColor::White, NoiseColor::Pink, NoiseColor::Brown];

    pub fn name(self) -> &'static str {
        match self {
            NoiseColor::White => "White",
            NoiseColor::Pink => "Pink",
            NoiseColor::Brown => "Brown",
        }
    }
}

/// Noise scaled so its peak is `amplitude_db`.
pub fn noise(color: NoiseColor, amplitude_db: f32, seconds: f64, rate: u32, seed: u32) -> Vec<f32> {
    let n = (seconds * rate as f64).round().max(0.0) as usize;
    let mut x = seed.max(1);
    let mut white = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    // Paul Kellet's pink filter; brown is leaky-integrated white.
    let mut b = [0f32; 7];
    let mut brown = 0f32;
    let mut out: Vec<f32> = (0..n)
        .map(|_| {
            let w = white();
            match color {
                NoiseColor::White => w,
                NoiseColor::Pink => {
                    b[0] = 0.99886 * b[0] + w * 0.0555179;
                    b[1] = 0.99332 * b[1] + w * 0.0750759;
                    b[2] = 0.96900 * b[2] + w * 0.153852;
                    b[3] = 0.86650 * b[3] + w * 0.3104856;
                    b[4] = 0.55000 * b[4] + w * 0.5329522;
                    b[5] = -0.7616 * b[5] - w * 0.0168980;
                    let p = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
                    b[6] = w * 0.115926;
                    p
                }
                NoiseColor::Brown => {
                    brown = (brown + w * 0.02) * 0.998;
                    brown
                }
            }
        })
        .collect();
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.0 {
        let g = db_to_gain(amplitude_db) / peak;
        out.iter_mut().for_each(|v| *v *= g);
    }
    out
}

/// The (low, high) frequency pair for a DTMF key, if `c` is one.
pub fn dtmf_freqs(c: char) -> Option<(f32, f32)> {
    const KEYS: [&str; 4] = ["123A", "456B", "789C", "*0#D"];
    const ROWS: [f32; 4] = [697.0, 770.0, 852.0, 941.0];
    const COLS: [f32; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
    let c = c.to_ascii_uppercase();
    KEYS.iter()
        .enumerate()
        .find_map(|(r, row)| row.find(c).map(|col| (ROWS[r], COLS[col])))
}

/// Dial `digits` (unknown characters are skipped), each tone followed by a gap.
pub fn dtmf(digits: &str, tone_ms: f32, gap_ms: f32, amplitude_db: f32, rate: u32) -> Vec<f32> {
    let tone_n = (tone_ms * 0.001 * rate as f32) as usize;
    let gap_n = (gap_ms * 0.001 * rate as f32) as usize;
    let amp = db_to_gain(amplitude_db) * 0.5;
    let mut out = Vec::new();
    for (lo, hi) in digits.chars().filter_map(dtmf_freqs) {
        let w = std::f32::consts::TAU / rate as f32;
        out.extend(
            (0..tone_n).map(|i| ((i as f32 * w * lo).sin() + (i as f32 * w * hi).sin()) * amp),
        );
        out.extend(std::iter::repeat_n(0.0, gap_n));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rising(v: &[f32]) -> usize {
        v.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    /// Goertzel power of frequency `f` in `v`.
    fn goertzel(v: &[f32], f: f32, rate: f32) -> f32 {
        let k = 2.0 * (std::f32::consts::TAU * f / rate).cos();
        let (mut s1, mut s2) = (0f32, 0f32);
        for &x in v {
            let s = x + k * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        s1 * s1 + s2 * s2 - k * s1 * s2
    }

    #[test]
    fn tone_frequency_and_level() {
        for w in Waveform::ALL {
            let v = tone(w, 440.0, 440.0, -6.0, 1.0, 48_000);
            assert_eq!(v.len(), 48_000);
            let c = rising(&v[1000..]);
            let expected = (47_000.0 * 440.0 / 48_000.0) as i64;
            assert!(
                (c as i64 - expected).abs() <= 2,
                "{w:?}: {c} cycles, expected {expected}"
            );
            let peak = v[1000..].iter().fold(0f32, |m, x| m.max(x.abs()));
            assert!(
                peak <= db_to_gain(-6.0) * 1.3 && peak > db_to_gain(-6.0) * 0.6,
                "{w:?} peak {peak}"
            );
        }
    }

    #[test]
    fn sweep_rises() {
        let v = tone(Waveform::Sine, 100.0, 1000.0, 0.0, 2.0, 48_000);
        let (early, late) = (rising(&v[..9600]), rising(&v[86_400..]));
        assert!(late > early * 5, "{early} -> {late}");
    }

    #[test]
    fn noise_colours_and_level() {
        let roughness =
            |v: &[f32]| v.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / v.len() as f32;
        let w = noise(NoiseColor::White, -3.0, 1.0, 48_000, 1);
        let p = noise(NoiseColor::Pink, -3.0, 1.0, 48_000, 1);
        let b = noise(NoiseColor::Brown, -3.0, 1.0, 48_000, 1);
        for v in [&w, &p, &b] {
            let peak = v.iter().fold(0f32, |m, x| m.max(x.abs()));
            assert!((peak - db_to_gain(-3.0)).abs() < 1e-5);
        }
        assert!(roughness(&w) > roughness(&p) && roughness(&p) > roughness(&b));
    }

    #[test]
    fn dtmf_digits_have_their_tones() {
        let v = dtmf("5#", 100.0, 50.0, -6.0, 8000);
        assert_eq!(v.len(), 2 * (800 + 400));
        let five = &v[..800];
        let (on, off) = (
            goertzel(five, 770.0, 8000.0) + goertzel(five, 1336.0, 8000.0),
            goertzel(five, 697.0, 8000.0) + goertzel(five, 1209.0, 8000.0),
        );
        assert!(on > off * 100.0, "5 is 770 + 1336 Hz");
        assert_eq!(dtmf_freqs('#'), Some((941.0, 1477.0)));
        assert_eq!(dtmf_freqs('x'), None);
    }
}
