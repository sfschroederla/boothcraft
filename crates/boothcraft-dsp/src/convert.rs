//! Sample-rate conversion, dither and channel conversion.

use audioadapter_buffers::direct::InterleavedSlice;
use boothcraft_core::{Document, Marker, MarkerKind, Rope};
use rubato::{
    calculate_cutoff, Async, FixedAsync, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

/// Resample one channel with a high-quality windowed-sinc filter. The output length is
/// `len * to / from` rounded to the nearest frame (exact integer math), with the filter delay removed.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let window = WindowFunction::Blackman2;
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: calculate_cutoff(256, window),
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 256,
        window,
    };
    let ratio = to as f64 / from as f64;
    let mut r = Async::<f32>::new_sinc(ratio, 1.0, &params, 1024, 1, FixedAsync::Input)
        .expect("valid resampler parameters");
    let cap = r.process_all_needed_output_len(input.len());
    let mut out = vec![0.0f32; cap];
    let src = InterleavedSlice::new(input, 1, input.len()).expect("mono input adapter");
    let mut dst = InterleavedSlice::new_mut(&mut out, 1, cap).expect("mono output adapter");
    let (_, written) = r
        .process_all_into_buffer(&src, &mut dst, input.len(), None)
        .expect("resample");
    // rubato rounds the length up from a floating-point ratio, which can add a stray frame.
    let exact = ((input.len() as u64 * to as u64 + from as u64 / 2) / from as u64) as usize;
    out.truncate(written.min(exact));
    out
}

fn scale_marker(m: &Marker, ratio: f64) -> Marker {
    let at = (m.at as f64 * ratio).round() as usize;
    let kind = match m.kind {
        MarkerKind::Cue => MarkerKind::Cue,
        MarkerKind::Range { len } => MarkerKind::Range {
            len: (len as f64 * ratio).round() as usize,
        },
    };
    Marker {
        name: m.name.clone(),
        at,
        kind,
    }
}

/// A copy of `doc` at `rate`, with markers moved to the same times.
pub fn resample_document(doc: &Document, rate: u32) -> Document {
    if rate == doc.sample_rate {
        return doc.clone();
    }
    let channels = doc
        .channels
        .iter()
        .map(|c| Rope::from_samples(&resample(&c.to_vec(0..c.len()), doc.sample_rate, rate)))
        .collect();
    let mut out = Document::new(rate, channels);
    let ratio = rate as f64 / doc.sample_rate as f64;
    for m in &doc.markers {
        out.add_marker(scale_marker(m, ratio));
    }
    out
}

/// How to fold several channels down to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Downmix {
    /// Average all channels.
    Mix,
    /// Keep only the first channel.
    First,
}

/// Change the channel count. Going up duplicates the mono (or first) channel; going down uses `downmix`.
pub fn convert_channels(doc: &Document, channels: usize, downmix: Downmix) -> Document {
    let have = doc.channel_count();
    if channels == have || channels == 0 {
        return doc.clone();
    }
    let ropes: Vec<Rope> = if channels < have && channels == 1 && downmix == Downmix::Mix {
        let n = doc.frames();
        let mut sum = vec![0f32; n];
        for c in &doc.channels {
            for (s, v) in sum.iter_mut().zip(c.to_vec(0..n)) {
                *s += v;
            }
        }
        let k = 1.0 / have as f32;
        sum.iter_mut().for_each(|s| *s *= k);
        vec![Rope::from_samples(&sum)]
    } else {
        (0..channels)
            .map(|i| doc.channels.get(i).unwrap_or(&doc.channels[0]).clone())
            .collect()
    };
    let mut out = Document::new(doc.sample_rate, ropes);
    out.markers = doc.markers.clone();
    out
}

/// Triangular-PDF dither of ±1 LSB, added before quantizing to `bits`.
pub struct Dither {
    state: u32,
}

impl Dither {
    pub fn new(seed: u32) -> Dither {
        Dither { state: seed.max(1) }
    }

    /// Uniform in `[-0.5, 0.5)`.
    fn uniform(&mut self) -> f32 {
        // xorshift32
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x as f64 / (u32::MAX as f64 + 1.0) - 0.5) as f32
    }

    pub fn apply(&mut self, samples: &mut [f32], bits: u32) {
        let lsb = 1.0 / (1u64 << (bits - 1)) as f32;
        for s in samples {
            *s += (self.uniform() + self.uniform()) * lsb;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rising_crossings(v: &[f32]) -> usize {
        v.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn resample_keeps_duration_and_pitch() {
        let (from, to) = (48_000, 44_100);
        let secs = 2.0;
        let sine: Vec<f32> = (0..(from as f64 * secs) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / from as f32).sin() * 0.5)
            .collect();
        let out = resample(&sine, from, to);
        let expected = sine.len() * to as usize / from as usize;
        assert_eq!(out.len(), expected);
        // 440 Hz for 2 s is 880 cycles in both.
        let (a, b) = (rising_crossings(&sine), rising_crossings(&out));
        assert!((a as i64 - b as i64).abs() <= 1, "{a} vs {b}");
        // Amplitude preserved in the steady-state middle.
        let peak = out[10_000..80_000].iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
    }

    #[test]
    fn resample_document_moves_markers() {
        let mut d = Document::new(48_000, vec![Rope::from_samples(&vec![0.0; 48_000])]);
        d.add_marker(Marker::range("r", 24_000, 4_800));
        let out = resample_document(&d, 96_000);
        assert_eq!(out.sample_rate, 96_000);
        assert_eq!(out.frames(), 96_000);
        assert_eq!(out.markers[0], Marker::range("r", 48_000, 9_600));
    }

    #[test]
    fn dither_is_small_and_unbiased() {
        let mut v = vec![0.0f32; 100_000];
        Dither::new(7).apply(&mut v, 16);
        let lsb = 1.0 / 32768.0;
        assert!(v.iter().all(|x| x.abs() < lsb), "within ±1 LSB");
        let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
        assert!(mean.abs() < lsb as f64 * 0.01, "mean {mean}");
        assert!(v.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn channel_conversion() {
        let d = Document::new(
            8_000,
            vec![
                Rope::from_samples(&[1.0, 1.0]),
                Rope::from_samples(&[0.0, -1.0]),
            ],
        );
        let mixed = convert_channels(&d, 1, Downmix::Mix);
        assert_eq!(mixed.channels[0].to_vec(0..2), vec![0.5, 0.0]);
        let first = convert_channels(&d, 1, Downmix::First);
        assert_eq!(first.channels[0].to_vec(0..2), vec![1.0, 1.0]);
        let up = convert_channels(&first, 2, Downmix::Mix);
        assert_eq!(up.channel_count(), 2);
        assert_eq!(up.channels[1].to_vec(0..2), vec![1.0, 1.0]);
    }
}
