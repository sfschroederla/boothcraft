//! ITU-R BS.1770 / EBU R128 loudness: measurement, a live meter, and Match Loudness.

use crate::param::{db_to_gain, gain_to_db};
use crate::rack::{render_offline, RackDesc, SlotDesc};
use ebur128::{EbuR128, Mode};
use std::sync::atomic::{AtomicBool, AtomicU32};

/// Loudness of a whole file or selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    /// Integrated loudness in LUFS (`-inf` for silence).
    pub integrated: f64,
    /// Loudness range in LU.
    pub range: f64,
    /// Highest true (inter-sample) peak across channels, in dBTP.
    pub true_peak_db: f64,
}

/// Measure planar channels (stereo is weighted per BS.1770; more than 2 channels are treated as
/// equally weighted).
pub fn measure(chans: &[Vec<f32>], rate: u32) -> Measurement {
    let ch = chans.len().max(1);
    let n = chans.first().map_or(0, Vec::len);
    let mut m = EbuR128::new(ch as u32, rate, Mode::I | Mode::LRA | Mode::TRUE_PEAK)
        .expect("valid loudness meter config");
    let mut block = Vec::with_capacity(4096 * ch);
    for start in (0..n).step_by(4096) {
        let end = (start + 4096).min(n);
        block.clear();
        for i in start..end {
            block.extend(chans.iter().map(|c| c[i]));
        }
        let _ = m.add_frames_f32(&block);
    }
    let true_peak = (0..ch as u32)
        .filter_map(|c| m.true_peak(c).ok())
        .fold(0.0f64, f64::max);
    Measurement {
        integrated: m.loudness_global().unwrap_or(f64::NEG_INFINITY),
        range: m.loudness_range().unwrap_or(0.0),
        true_peak_db: 20.0 * true_peak.max(1e-12).log10(),
    }
}

/// Bring `chans` to `target` LUFS integrated. With `ceiling_db`, a look-ahead limiter then keeps
/// sample peaks at or below it (which can pull the loudness slightly under target). Returns the
/// gain applied, or `None` for silence.
pub fn match_loudness(
    chans: &mut [Vec<f32>],
    rate: u32,
    target: f64,
    ceiling_db: Option<f32>,
) -> Option<f32> {
    let before = measure(chans, rate).integrated;
    if !before.is_finite() {
        return None;
    }
    let gain_db = (target - before) as f32;
    let g = db_to_gain(gain_db);
    chans.iter_mut().flatten().for_each(|x| *x *= g);
    if let Some(ceiling) = ceiling_db {
        let peak = chans.iter().flatten().fold(0f32, |m, x| m.max(x.abs()));
        if gain_to_db(peak) > ceiling {
            let mut desc = RackDesc::default();
            let mut slot = SlotDesc::new("hard_limiter")?;
            slot.values[0] = ceiling;
            desc.slots[0] = Some(slot);
            render_offline(
                &desc,
                chans,
                rate as f32,
                &AtomicU32::new(0),
                &AtomicBool::new(false),
            );
        }
    }
    Some(gain_db)
}

/// Live momentary / short-term / integrated loudness of whatever is playing.
pub struct LiveMeter {
    meter: Option<EbuR128>,
    channels: usize,
    rate: u32,
    max_true_peak: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LiveReading {
    pub momentary: f64,
    pub short_term: f64,
    pub integrated: f64,
    pub true_peak_db: f64,
}

impl LiveMeter {
    pub fn new(channels: usize, rate: u32) -> LiveMeter {
        let mut m = LiveMeter {
            meter: None,
            channels: channels.max(1),
            rate,
            max_true_peak: 0.0,
        };
        m.reset();
        m
    }

    pub fn reset(&mut self) {
        self.meter = EbuR128::new(
            self.channels as u32,
            self.rate,
            Mode::I | Mode::S | Mode::TRUE_PEAK,
        )
        .ok();
        self.max_true_peak = 0.0;
    }

    /// Feed interleaved samples.
    pub fn push(&mut self, interleaved: &[f32]) {
        if let Some(m) = &mut self.meter {
            let usable = interleaved.len() - interleaved.len() % self.channels;
            let _ = m.add_frames_f32(&interleaved[..usable]);
            for c in 0..self.channels as u32 {
                if let Ok(p) = m.true_peak(c) {
                    self.max_true_peak = self.max_true_peak.max(p);
                }
            }
        }
    }

    pub fn reading(&self) -> LiveReading {
        let Some(m) = &self.meter else {
            return LiveReading::default();
        };
        LiveReading {
            momentary: m.loudness_momentary().unwrap_or(f64::NEG_INFINITY),
            short_term: m.loudness_shortterm().unwrap_or(f64::NEG_INFINITY),
            integrated: m.loudness_global().unwrap_or(f64::NEG_INFINITY),
            true_peak_db: 20.0 * self.max_true_peak.max(1e-12).log10(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, secs: f32) -> Vec<f32> {
        (0..(48_000.0 * secs) as usize)
            .map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / 48_000.0).sin() * amp)
            .collect()
    }

    #[test]
    fn reference_tone_reads_as_expected() {
        // BS.1770: each channel's mean square is -23.01 dB (a sine's RMS is 3.01 dB under its peak);
        // two channels add 3.01 dB, and the -0.691 dB constant cancels K-weighting at 1 kHz: -20.0 LUFS.
        let s = sine(db_to_gain(-20.0), 10.0);
        let m = measure(&[s.clone(), s], 48_000);
        assert!((m.integrated + 20.0).abs() < 0.05, "{}", m.integrated);
        assert!((m.true_peak_db + 20.0).abs() < 0.2);
    }

    #[test]
    fn match_hits_the_target() {
        let s = sine(0.05, 8.0);
        let mut chans = vec![s.clone(), s];
        let gain = match_loudness(&mut chans, 48_000, -16.0, Some(-1.0)).unwrap();
        assert!(gain > 0.0);
        let after = measure(&chans, 48_000);
        assert!(
            (after.integrated + 16.0).abs() < 0.1,
            "{}",
            after.integrated
        );
        assert!(chans
            .iter()
            .flatten()
            .all(|x| x.abs() <= db_to_gain(-1.0) * 1.0001));
    }

    #[test]
    fn limiter_holds_the_ceiling_when_target_is_too_loud() {
        let s = sine(0.5, 6.0);
        let mut chans = vec![s.clone(), s];
        match_loudness(&mut chans, 48_000, -5.0, Some(-1.0)).unwrap();
        let peak = chans.iter().flatten().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak <= db_to_gain(-1.0) * 1.0001, "{peak}");
    }

    #[test]
    fn silence_is_left_alone() {
        let mut chans = vec![vec![0.0; 48_000]];
        assert_eq!(match_loudness(&mut chans, 48_000, -23.0, None), None);
    }
}
