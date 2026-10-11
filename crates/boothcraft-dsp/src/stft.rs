//! Short-time Fourier transform framework: streaming analysis and overlap-add synthesis, shared
//! by the spectral display and by spectral effects (noise reduction, DeHummer, …).

use crate::effect::Effect;
use crate::param::ParamSpec;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Analysis/synthesis window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Hann,
    BlackmanHarris,
}

impl Window {
    pub fn coefficients(self, n: usize) -> Vec<f32> {
        let tau = std::f32::consts::TAU;
        match self {
            Window::Hann => (0..n)
                .map(|i| 0.5 - 0.5 * (tau * i as f32 / n as f32).cos())
                .collect(),
            Window::BlackmanHarris => {
                let (a0, a1, a2, a3) = (0.358_75f32, 0.488_29, 0.141_28, 0.011_68);
                (0..n)
                    .map(|i| {
                        let x = tau * i as f32 / n as f32;
                        a0 - a1 * x.cos() + a2 * (2.0 * x).cos() - a3 * (3.0 * x).cos()
                    })
                    .collect()
            }
        }
    }
}

/// Repeated one-shot magnitude spectra of a fixed size, for a display (the spectrogram) rather
/// than the overlap-add reconstruction [`StftProcessor`] is for: each call is an independent FFT
/// of its own window of samples, with no synthesis and no state carried between calls.
pub struct SpectrumAnalyzer {
    fft_size: usize,
    window: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    spectrum: Vec<Complex32>,
    mag: Vec<f32>,
}

impl SpectrumAnalyzer {
    pub fn new(fft_size: usize, window: Window) -> SpectrumAnalyzer {
        let mut planner = RealFftPlanner::<f32>::new();
        SpectrumAnalyzer {
            fft_size,
            window: window.coefficients(fft_size),
            forward: planner.plan_fft_forward(fft_size),
            input: vec![0.0; fft_size],
            spectrum: vec![Complex32::default(); fft_size / 2 + 1],
            mag: vec![0.0; fft_size / 2 + 1],
        }
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Magnitude per bin (`fft_size / 2 + 1` of them, DC to Nyquist) for `samples`, which may be
    /// shorter than `fft_size` (e.g. a window centred near the start or end of a file) — the rest
    /// is treated as silence.
    pub fn magnitude(&mut self, samples: &[f32]) -> &[f32] {
        self.input.fill(0.0);
        for (dst, (&s, &w)) in self.input.iter_mut().zip(samples.iter().zip(&self.window)) {
            *dst = s * w;
        }
        let _ = self.forward.process(&mut self.input, &mut self.spectrum);
        for (m, c) in self.mag.iter_mut().zip(&self.spectrum) {
            *m = c.norm();
        }
        &self.mag
    }
}

/// Implemented by anything that transforms one analysis frame's spectrum. The parameter accessors
/// mirror [`Effect`]'s, so [`SpectralEffect`] can delegate straight to them.
pub trait SpectralFrame: Send {
    fn specs(&self) -> &'static [ParamSpec];
    fn values(&self) -> &[f32];
    fn values_mut(&mut self) -> &mut [f32];

    /// `bins` holds `fft_size / 2 + 1` complex bins (DC to Nyquist) for channel `ch`. Mutate in
    /// place; the inverse transform and overlap-add happen automatically around this call.
    fn frame(&mut self, bins: &mut [Complex32], ch: usize);

    fn prepare(&mut self, _sample_rate: f32, _fft_size: usize, _channels: usize) {}
    fn reset(&mut self) {}
    fn update(&mut self) {}
}

struct ChannelState {
    fft_size: usize,
    /// Samples received but not yet formed into a full analysis frame.
    input: Vec<f32>,
    /// Overlap-add accumulator, always exactly `fft_size` long, aligned to the next frame to analyze.
    output: Vec<f32>,
    /// Finished output samples, ready to hand back to the caller.
    ready: std::collections::VecDeque<f32>,
    fft_in: Vec<f32>,
    fft_out: Vec<Complex32>,
    ifft_out: Vec<f32>,
}

impl ChannelState {
    fn new(fft_size: usize, bins: usize) -> ChannelState {
        ChannelState {
            fft_size,
            input: Vec::new(),
            output: vec![0.0; fft_size],
            // Seeded with `fft_size` zeros so `ready` can never underflow: production (`ready`
            // growing by `hop` per analyzed frame) always trails consumption (one sample popped
            // per input sample pushed) by at most `fft_size - 1`, regardless of how the caller
            // chunks `process` calls, so this margin is always enough. Without it, a caller whose
            // chunk size doesn't divide `hop` can hit a transient deficit; the real sample that
            // was going to fill that slot is still delivered, just one slot later, which
            // compounds into ever-growing latency over a long stream instead of a fixed one.
            ready: std::iter::repeat_n(0.0, fft_size).collect(),
            fft_in: vec![0.0; fft_size],
            fft_out: vec![Complex32::default(); bins],
            ifft_out: vec![0.0; fft_size],
        }
    }

    fn reset(&mut self) {
        self.input.clear();
        self.output.fill(0.0);
        self.ready.clear();
        self.ready.extend(std::iter::repeat_n(0.0, self.fft_size));
    }
}

/// Streaming STFT: analyzes input in overlapping windows, hands each frame's spectrum to a
/// [`SpectralFrame`], and resynthesizes by overlap-add. Introduces latency equal to `fft_size`.
pub struct StftProcessor {
    fft_size: usize,
    hop: usize,
    window: Vec<f32>,
    /// Scales the synthesis window so overlap-add reconstructs unity gain.
    synth_scale: f32,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    channels: Vec<ChannelState>,
}

impl StftProcessor {
    /// `fft_size` must be a power of two from 256 to 8192; overlap is fixed at 75% (`hop =
    /// fft_size / 4`). Use [`Window::Hann`], the only one of the two windows that's COLA
    /// (constant-overlap-add) at this ratio; `BlackmanHarris` is for single-shot analysis instead
    /// (no overlap-add reconstruction to satisfy COLA for).
    pub fn new(fft_size: usize, window: Window, channels: usize) -> StftProcessor {
        let hop = fft_size / 4;
        let win = window.coefficients(fft_size);
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft_size);
        let inverse = planner.plan_fft_inverse(fft_size);
        let bins = fft_size / 2 + 1;
        // The inverse transform is unnormalized (IFFT(FFT(x)) == fft_size * x), and each output
        // sample receives contributions from several overlapping frames, each scaled by the
        // synthesis window: dividing by both reconstructs unity gain.
        let synth_scale = 1.0 / (fft_size as f32 * cola_synth_scale(&win, hop));
        StftProcessor {
            fft_size,
            hop,
            synth_scale,
            window: win,
            forward,
            inverse,
            channels: (0..channels.max(1))
                .map(|_| ChannelState::new(fft_size, bins))
                .collect(),
        }
    }

    pub fn latency(&self) -> usize {
        self.fft_size
    }

    pub fn reset(&mut self) {
        self.channels.iter_mut().for_each(ChannelState::reset);
    }

    /// Process `buf` in place: pushes it as new input, runs every analysis frame it completes, and
    /// overwrites `buf` with that many samples of (delayed) output.
    pub fn process(&mut self, buf: &mut [&mut [f32]], effect: &mut impl SpectralFrame) {
        let (fft_size, hop) = (self.fft_size, self.hop);
        for (ch, samples) in buf.iter_mut().enumerate() {
            let Some(state) = self.channels.get_mut(ch) else {
                continue;
            };
            state.input.extend_from_slice(samples);
            while state.input.len() >= fft_size {
                for (dst, (x, w)) in state
                    .fft_in
                    .iter_mut()
                    .zip(state.input.iter().zip(&self.window))
                {
                    *dst = x * w;
                }
                let _ = self.forward.process(&mut state.fft_in, &mut state.fft_out);
                effect.frame(&mut state.fft_out, ch);
                let _ = self
                    .inverse
                    .process(&mut state.fft_out, &mut state.ifft_out);
                for (o, (x, w)) in state
                    .output
                    .iter_mut()
                    .zip(state.ifft_out.iter().zip(&self.window))
                {
                    *o += x * w * self.synth_scale;
                }
                state.ready.extend(state.output.drain(..hop));
                state.output.extend(std::iter::repeat_n(0.0, hop));
                state.input.drain(..hop);
            }
            for s in samples.iter_mut() {
                *s = state.ready.pop_front().unwrap_or(0.0);
            }
        }
    }
}

/// The constant-overlap-add normalization for `window` at hop spacing `hop`, found by summing a
/// window applied (and reapplied, for the analysis+synthesis pair) at every overlapping shift and
/// reading the steady-state value far from the edges. Works for any COLA-compatible window/hop
/// pair, not just the one pair this module currently exposes.
fn cola_synth_scale(window: &[f32], hop: usize) -> f32 {
    let n = window.len();
    let periods = (n / hop).max(1) + 2;
    let total = n + periods * hop;
    let mut sum = vec![0.0f32; total];
    let mut start = 0;
    while start + n <= total {
        for (s, &w) in sum[start..start + n].iter_mut().zip(window) {
            *s += w * w;
        }
        start += hop;
    }
    sum[total / 2]
}

/// Wraps a [`SpectralFrame`] as a real-time [`Effect`], so spectral effects work in the Effects
/// Rack and in live preview exactly like any other effect, with their STFT latency reported.
pub struct SpectralEffect<T: SpectralFrame> {
    inner: T,
    fft_size: usize,
    window: Window,
    stft: Option<StftProcessor>,
}

impl<T: SpectralFrame> SpectralEffect<T> {
    pub fn new(inner: T, fft_size: usize, window: Window) -> SpectralEffect<T> {
        SpectralEffect {
            inner,
            fft_size,
            window,
            stft: None,
        }
    }
}

impl<T: SpectralFrame> Effect for SpectralEffect<T> {
    fn specs(&self) -> &'static [ParamSpec] {
        self.inner.specs()
    }

    fn values(&self) -> &[f32] {
        self.inner.values()
    }

    fn values_mut(&mut self) -> &mut [f32] {
        self.inner.values_mut()
    }

    fn update(&mut self) {
        self.inner.update();
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.inner.prepare(sample_rate, self.fft_size, channels);
        self.stft = Some(StftProcessor::new(self.fft_size, self.window, channels));
    }

    fn reset(&mut self) {
        if let Some(s) = &mut self.stft {
            s.reset();
        }
        self.inner.reset();
    }

    fn latency(&self) -> usize {
        self.stft.as_ref().map_or(0, StftProcessor::latency)
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        if let Some(stft) = &mut self.stft {
            stft.process(buf, &mut self.inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_analyzer_peaks_at_the_tone_bin() {
        let fft_size = 2048;
        let rate = 48_000.0;
        let bin_hz = rate / fft_size as f32;
        let bin = 100;
        let freq = bin as f32 * bin_hz;
        let samples: Vec<f32> = (0..fft_size)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / rate).sin())
            .collect();
        let mut analyzer = SpectrumAnalyzer::new(fft_size, Window::Hann);
        let mag = analyzer.magnitude(&samples);
        let (peak_bin, _) =
            mag.iter().enumerate().fold(
                (0, 0.0f32),
                |(bi, bv), (i, &v)| if v > bv { (i, v) } else { (bi, bv) },
            );
        assert_eq!(peak_bin, bin);
    }

    struct Identity;
    impl SpectralFrame for Identity {
        fn specs(&self) -> &'static [ParamSpec] {
            &[]
        }
        fn values(&self) -> &[f32] {
            &[]
        }
        fn values_mut(&mut self) -> &mut [f32] {
            &mut []
        }
        fn frame(&mut self, _bins: &mut [Complex32], _ch: usize) {}
    }

    fn run(fft_size: usize, window: Window, input: &[f32], block: usize) -> Vec<f32> {
        let mut stft = StftProcessor::new(fft_size, window, 1);
        let mut effect = Identity;
        let mut out = Vec::with_capacity(input.len());
        for chunk in input.chunks(block) {
            let mut buf = chunk.to_vec();
            stft.process(&mut [&mut buf], &mut effect);
            out.extend_from_slice(&buf);
        }
        out
    }

    #[test]
    fn identity_frame_reconstructs_the_input() {
        // Hann is the OLA window `StftProcessor` is built for (COLA-exact at 75% overlap).
        // Blackman-Harris stays in `Window` for S2's single-shot display FFT, which has no
        // overlap-add reconstruction to satisfy COLA for.
        for (fft_size, window) in [(1024, Window::Hann), (2048, Window::Hann)] {
            let n = fft_size * 8;
            let input: Vec<f32> = (0..n)
                .map(|i| (i as f32 * 0.01).sin() * 0.5 + (i as f32 * 0.037).sin() * 0.2)
                .collect();
            let out = run(fft_size, window, &input, 777);
            let latency = fft_size;
            // Compare the steady-state region, well clear of the latency ramp-up and the final
            // partial frame's ramp-down.
            let (start, end) = (latency + fft_size, n - fft_size);
            let worst = (start..end)
                .map(|i| (out[i] - input[i - latency]).abs())
                .fold(0.0, f32::max);
            assert!(worst < 1e-4, "{fft_size:?} {window:?}: worst error {worst}");
        }
    }

    #[test]
    fn identity_frame_reconstructs_regardless_of_caller_chunk_size() {
        // Any chunk size reconstructs with the exact same latency: the margin pre-seeded into
        // `ready` guarantees it never underflows, no matter how the caller's chunk size happens
        // to align with the hop.
        let fft_size = 1024;
        let n = fft_size * 8;
        let input: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.01).sin() * 0.5 + (i as f32 * 0.037).sin() * 0.2)
            .collect();
        for block in [1, 100, 256, 777, 1024, 4096] {
            let out = run(fft_size, Window::Hann, &input, block);
            let latency = fft_size;
            let (start, end) = (latency + fft_size, n - fft_size);
            let worst = (start..end)
                .map(|i| (out[i] - input[i - latency]).abs())
                .fold(0.0, f32::max);
            assert!(worst < 1e-4, "block={block}: worst error {worst}");
        }
    }

    #[test]
    fn latency_matches_fft_size() {
        let stft = StftProcessor::new(2048, Window::Hann, 2);
        assert_eq!(stft.latency(), 2048);
    }

    #[test]
    fn spectral_effect_reports_latency_and_processes() {
        struct Gain(Vec<f32>);
        impl SpectralFrame for Gain {
            fn specs(&self) -> &'static [ParamSpec] {
                &[]
            }
            fn values(&self) -> &[f32] {
                &self.0
            }
            fn values_mut(&mut self) -> &mut [f32] {
                &mut self.0
            }
            fn frame(&mut self, bins: &mut [Complex32], _ch: usize) {
                for b in bins {
                    *b *= 2.0;
                }
            }
        }
        let mut e = SpectralEffect::new(Gain(vec![]), 1024, Window::Hann);
        e.prepare(48_000.0, 1, 4096);
        e.reset();
        assert_eq!(e.latency(), 1024);
        let n = 1024 * 6;
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.02).sin() * 0.1).collect();
        let mut buf = input.clone();
        e.process(&mut [&mut buf[..]]);
        let (start, end) = (1024 * 2, n - 1024);
        let worst = (start..end)
            .map(|i| (buf[i] - input[i - 1024] * 2.0).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-3, "worst error {worst}");
    }
}
