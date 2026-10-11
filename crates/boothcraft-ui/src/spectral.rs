//! Spectral (frequency/pitch) view: a worker-thread-computed spectrogram image per channel,
//! uploaded as an egui texture, plus YIN pitch tracking for the Pitch view mode.

use crate::ops::OpenFile;
use crate::view::View;
use boothcraft_core::{ChannelMask, Rope};
use boothcraft_dsp::{SpectrumAnalyzer, StftWindow};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{mpsc, Arc};

/// Which content the waveform editor currently shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Waveform,
    Frequency,
    Pitch,
    /// Waveform in the top half of each lane, spectrogram in the bottom half.
    Split,
}

impl ViewMode {
    pub const ALL: [ViewMode; 4] = [
        ViewMode::Waveform,
        ViewMode::Frequency,
        ViewMode::Pitch,
        ViewMode::Split,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ViewMode::Waveform => "Waveform",
            ViewMode::Frequency => "Spectral Frequency",
            ViewMode::Pitch => "Spectral Pitch",
            ViewMode::Split => "Split",
        }
    }

    /// Whether this mode needs a spectrogram computed at all (`Waveform` alone does not).
    pub fn needs_render(self) -> bool {
        matches!(
            self,
            ViewMode::Frequency | ViewMode::Pitch | ViewMode::Split
        )
    }
}

/// Spectrogram colour maps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMap {
    #[default]
    Fire,
    Grayscale,
}

impl ColorMap {
    pub const ALL: [ColorMap; 2] = [ColorMap::Fire, ColorMap::Grayscale];

    pub fn name(self) -> &'static str {
        match self {
            ColorMap::Fire => "Fire",
            ColorMap::Grayscale => "Grayscale",
        }
    }

    fn color(self, t: f32) -> [u8; 3] {
        let t = t.clamp(0.0, 1.0);
        match self {
            ColorMap::Grayscale => {
                let v = (t * 255.0).round() as u8;
                [v, v, v]
            }
            ColorMap::Fire => fire(t),
        }
    }
}

/// Audition-style black -> violet -> red -> amber -> white gradient.
fn fire(t: f32) -> [u8; 3] {
    const STOPS: [(f32, [u8; 3]); 5] = [
        (0.00, [0, 0, 10]),
        (0.25, [45, 0, 110]),
        (0.50, [190, 20, 20]),
        (0.75, [255, 170, 0]),
        (1.00, [255, 255, 230]),
    ];
    for w in STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return lerp3(c0, c1, k.clamp(0.0, 1.0));
        }
    }
    STOPS[4].1
}

fn lerp3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Frequency axis scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FreqAxis {
    #[default]
    Log,
    Linear,
}

impl FreqAxis {
    pub const ALL: [FreqAxis; 2] = [FreqAxis::Log, FreqAxis::Linear];

    pub fn name(self) -> &'static str {
        match self {
            FreqAxis::Log => "Log",
            FreqAxis::Linear => "Linear",
        }
    }
}

/// FFT sizes offered in the settings menu (256-8192, per the spec).
pub const FFT_SIZES: [usize; 6] = [256, 512, 1024, 2048, 4096, 8192];

/// Spectral view settings, shared by every open file (alongside [`crate::waveform::DisplayOptions`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralOptions {
    pub mode: ViewMode,
    pub fft_size: usize,
    pub window: StftWindow,
    pub color_map: ColorMap,
    pub freq_axis: FreqAxis,
    /// dB below the (approximate) full-scale reference that maps to the bottom of the colour map.
    pub db_floor: f32,
}

impl Default for SpectralOptions {
    fn default() -> Self {
        SpectralOptions {
            mode: ViewMode::default(),
            fft_size: 2048,
            window: StftWindow::Hann,
            color_map: ColorMap::default(),
            freq_axis: FreqAxis::default(),
            db_floor: -90.0,
        }
    }
}

const MIN_HZ: f32 = 20.0;
const PITCH_MIN_HZ: f32 = 50.0;
const PITCH_MAX_HZ: f32 = 2000.0;
const YIN_THRESHOLD: f32 = 0.15;
/// Pitch is only re-estimated every Nth column (held flat in between): a YIN pass is quadratic in
/// the lowest pitch's period, so one estimate per pixel on a wide, zoomed-out view would make the
/// (already asynchronous, worker-thread) spectrogram refresh noticeably slower for no visual gain.
const PITCH_STRIDE: usize = 4;

/// One channel's rendered spectrogram, as plain pixel data (conversion to an egui texture happens
/// on the UI thread that receives it).
struct SpectrogramImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    /// Estimated pitch in Hz per column (`None` where unvoiced or not computed), same length as `width`.
    pitch: Vec<Option<f32>>,
}

impl SpectrogramImage {
    fn color_image(&self) -> egui::ColorImage {
        egui::ColorImage::from_rgba_unmultiplied([self.width, self.height], &self.rgba)
    }
}

/// Everything that determines a spectrogram render; a cached render is reused as long as this is
/// unchanged, and a running job is cancelled and restarted if it changes mid-render.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stamp {
    revision: u64,
    start: f64,
    frames_per_px: f64,
    width_px: usize,
    height_px: usize,
    rate: u32,
    channel_count: usize,
    mask: ChannelMask,
    opts: SpectralOptions,
}

struct JobHandle {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Vec<SpectrogramImage>>,
}

impl JobHandle {
    fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    fn poll(&self) -> Option<Vec<SpectrogramImage>> {
        self.rx.try_recv().ok()
    }
}

/// Per-file cache: the last computed spectrogram (as textures, so painting is just blitting an
/// image) and pitch curve, plus any job still computing the next one. Lives on [`OpenFile`].
#[derive(Default)]
pub struct SpectrogramCache {
    stamp: Option<Stamp>,
    job: Option<(Stamp, JobHandle)>,
    textures: Vec<egui::TextureHandle>,
    pitch: Vec<Vec<Option<f32>>>,
}

impl SpectrogramCache {
    pub fn texture(&self, channel: usize) -> Option<&egui::TextureHandle> {
        self.textures.get(channel)
    }

    pub fn pitch(&self, channel: usize) -> &[Option<f32>] {
        self.pitch.get(channel).map_or(&[], |v| v.as_slice())
    }

    /// The visible range the current textures/pitch were rendered for (for mapping them back onto
    /// the lane rect, since the view may have moved on since a still-running job started).
    pub fn rendered_range(&self) -> Option<(f64, f64)> {
        self.stamp
            .map(|s| (s.start, s.start + s.width_px as f64 * s.frames_per_px))
    }
}

/// Make sure `cache` reflects the document's current content, view and settings, starting or
/// restarting a background render if not, and picking up a finished one if available. Call once
/// per frame before painting; cheap when already up to date. The caller passes `channels` as an
/// owned clone (callers need `file.doc()`'s borrow to end before they can also pass
/// `&mut file.spectrogram`, so they always clone once; cloning a [`Rope`] is cheap — it shares
/// blocks structurally, not samples) — it's only actually used if a render needs to start, and
/// dropped (just a few `Arc` decrements) otherwise.
#[allow(clippy::too_many_arguments)]
pub fn ensure(
    cache: &mut SpectrogramCache,
    ctx: &egui::Context,
    channels: Vec<Rope>,
    revision: u64,
    rate: u32,
    mask: ChannelMask,
    view: &View,
    opts: &SpectralOptions,
    width_px: usize,
    height_px: usize,
) {
    if width_px == 0 || height_px == 0 {
        return;
    }
    let stamp = Stamp {
        revision,
        start: view.start,
        frames_per_px: view.frames_per_px,
        width_px,
        height_px,
        rate,
        channel_count: channels.len(),
        mask,
        opts: *opts,
    };
    if cache.stamp == Some(stamp) {
        return;
    }
    let already_running = cache.job.as_ref().is_some_and(|(s, _)| *s == stamp);
    if !already_running {
        if let Some((_, old)) = cache.job.take() {
            old.cancel();
        }
        cache.job = Some((stamp, spawn(channels, stamp)));
    }
    if let Some((job_stamp, job)) = &cache.job {
        if let Some(images) = job.poll() {
            cache.textures = images
                .iter()
                .map(|img| {
                    ctx.load_texture(
                        "spectrogram",
                        img.color_image(),
                        egui::TextureOptions::LINEAR,
                    )
                })
                .collect();
            cache.pitch = images.into_iter().map(|img| img.pitch).collect();
            cache.stamp = Some(*job_stamp);
            cache.job = None;
        }
    }
}

/// Paint one channel's cached spectrogram into `rect` (stretched to fill it exactly), plus the
/// pitch curve overlay in [`ViewMode::Pitch`]. Call after [`ensure`].
pub fn draw(
    painter: &egui::Painter,
    rect: egui::Rect,
    file: &OpenFile,
    channel: usize,
    opts: &SpectralOptions,
    view: &View,
) {
    let cache = &file.spectrogram;
    if let Some(tex) = cache.texture(channel) {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), rect, uv, egui::Color32::WHITE);
    } else {
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(12));
    }
    if opts.mode != ViewMode::Pitch {
        return;
    }
    let Some((rstart, rend)) = cache.rendered_range() else {
        return;
    };
    let pitch = cache.pitch(channel);
    if pitch.is_empty() {
        return;
    }
    let nyquist = (file.doc().sample_rate as f32 * 0.5).max(MIN_HZ * 2.0);
    let span = (rend - rstart).max(1.0);
    let pts: Vec<egui::Pos2> = pitch
        .iter()
        .enumerate()
        .filter_map(|(i, hz)| {
            let hz = (*hz)?;
            let frame = rstart + (i as f64 + 0.5) * span / pitch.len() as f64;
            let x = rect.left() + view.frame_to_px(frame);
            let y = rect.top()
                + freq_to_row(opts.freq_axis, hz, rect.height() as usize, MIN_HZ, nyquist);
            (x >= rect.left() && x <= rect.right()).then_some(egui::pos2(x, y))
        })
        .collect();
    if pts.len() > 1 {
        painter.add(egui::Shape::line(
            pts,
            egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 255, 160)),
        ));
    }
}

fn spawn(channels: Vec<Rope>, stamp: Stamp) -> JobHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let images: Option<Vec<SpectrogramImage>> = channels
            .iter()
            .map(|rope| render_channel(rope, stamp, &c))
            .collect();
        if let Some(images) = images {
            let _ = tx.send(images);
        }
    });
    JobHandle { cancel, rx }
}

fn render_channel(rope: &Rope, stamp: Stamp, cancel: &AtomicBool) -> Option<SpectrogramImage> {
    let opts = stamp.opts;
    let total = rope.len();
    let rate = stamp.rate as f32;
    let nyquist = (rate * 0.5).max(MIN_HZ * 2.0);
    let bin_hz = rate / opts.fft_size as f32;
    let bins = opts.fft_size / 2 + 1;
    // Peak magnitude of a full-scale sine through a Hann-windowed, unnormalized FFT is roughly
    // `fft_size / 4`; dividing by that puts a full-scale tone near 0 dB. It's an approximation for
    // display purposes, not a calibrated level meter (the adjustable dB floor accommodates that).
    let reference = (opts.fft_size as f32 / 4.0).max(1.0);
    let mut analyzer = SpectrumAnalyzer::new(opts.fft_size, opts.window);
    let want_pitch = opts.mode == ViewMode::Pitch;
    let pitch_win = yin_window_len(rate);

    let mut rgba = vec![0u8; stamp.width_px * stamp.height_px * 4];
    let mut pitch = vec![None; stamp.width_px];
    let mut last_pitch = None;
    // `x` indexes both `rgba` (2D, by row and column) and `pitch`, and also drives the frame
    // position, so this isn't a single-collection iteration `enumerate()` would simplify.
    #[allow(clippy::needless_range_loop)]
    for x in 0..stamp.width_px {
        if cancel.load(Relaxed) {
            return None;
        }
        let center = stamp.start + (x as f64 + 0.5) * stamp.frames_per_px;
        let samples = read_window(
            rope,
            center - opts.fft_size as f64 / 2.0,
            opts.fft_size,
            total,
        );
        let mag = analyzer.magnitude(&samples);
        for y in 0..stamp.height_px {
            let freq = row_freq(opts.freq_axis, y, stamp.height_px, MIN_HZ, nyquist);
            let m = sample_bin(mag, freq, bin_hz, bins);
            let db = 20.0 * (m / reference).max(1e-9).log10();
            let t = ((db - opts.db_floor) / -opts.db_floor).clamp(0.0, 1.0);
            let [r, g, b] = opts.color_map.color(t);
            let i = (y * stamp.width_px + x) * 4;
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
        if want_pitch {
            if x % PITCH_STRIDE == 0 {
                let win = read_window(rope, center - pitch_win as f64 / 2.0, pitch_win, total);
                last_pitch = yin_pitch(&win, rate);
            }
            pitch[x] = last_pitch;
        }
    }
    Some(SpectrogramImage {
        width: stamp.width_px,
        height: stamp.height_px,
        rgba,
        pitch,
    })
}

/// `len` samples of `rope` centred so the window starts at `start_f` (frame units, may be
/// fractional or out of range); out-of-range parts are zero-padded.
fn read_window(rope: &Rope, start_f: f64, len: usize, total: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; len];
    let start = start_f.round() as i64;
    let end = start + len as i64;
    let clamped_start = start.max(0) as usize;
    let clamped_end = (end.max(0) as usize).min(total);
    if clamped_start >= clamped_end || clamped_start >= total {
        return out;
    }
    let data = rope.to_vec(clamped_start..clamped_end);
    let offset = (clamped_start as i64 - start) as usize;
    out[offset..offset + data.len()].copy_from_slice(&data);
    out
}

/// The frequency a given image row represents: row 0 (top) is `max_hz`, the last row (bottom) is
/// `min_hz`, matching how a spectrogram is conventionally drawn.
fn row_freq(axis: FreqAxis, row: usize, height: usize, min_hz: f32, max_hz: f32) -> f32 {
    let t = row as f32 / (height.max(2) - 1) as f32;
    match axis {
        FreqAxis::Linear => max_hz - t * (max_hz - min_hz),
        FreqAxis::Log => {
            let (lo, hi) = (min_hz.max(1.0).ln(), max_hz.ln());
            (hi - t * (hi - lo)).exp()
        }
    }
}

/// Inverse of [`row_freq`]: the row (may be fractional) a given frequency maps to.
fn freq_to_row(axis: FreqAxis, freq: f32, height: usize, min_hz: f32, max_hz: f32) -> f32 {
    let t = match axis {
        FreqAxis::Linear => (max_hz - freq) / (max_hz - min_hz),
        FreqAxis::Log => {
            let (lo, hi) = (min_hz.max(1.0).ln(), max_hz.ln());
            (hi - freq.max(1.0).ln()) / (hi - lo)
        }
    };
    t.clamp(0.0, 1.0) * (height.max(2) - 1) as f32
}

/// Linearly interpolated magnitude at `freq`.
fn sample_bin(mag: &[f32], freq: f32, bin_hz: f32, bins: usize) -> f32 {
    if freq <= 0.0 || bin_hz <= 0.0 {
        return mag[0];
    }
    let pos = freq / bin_hz;
    let i0 = (pos.floor() as usize).min(bins - 1);
    if i0 + 1 >= bins {
        return mag[bins - 1];
    }
    let frac = pos - i0 as f32;
    mag[i0] * (1.0 - frac) + mag[i0 + 1] * frac
}

/// Hz gridlines for the scale strip: `(row, label)` pairs, decluttered so labels never crowd.
pub fn hz_gridlines(axis: FreqAxis, height_px: usize, rate: u32) -> Vec<(f32, String)> {
    let nyquist = rate as f32 * 0.5;
    const LOG_CANDIDATES: [f32; 19] = [
        20.0, 30.0, 50.0, 70.0, 100.0, 150.0, 200.0, 300.0, 500.0, 700.0, 1_000.0, 1_500.0,
        2_000.0, 3_000.0, 5_000.0, 7_000.0, 10_000.0, 15_000.0, 20_000.0,
    ];
    const LINEAR_STEP: f32 = 2_000.0;
    let candidates: Vec<f32> = match axis {
        FreqAxis::Log => LOG_CANDIDATES.to_vec(),
        FreqAxis::Linear => {
            let steps = (nyquist / LINEAR_STEP).floor() as i32;
            (0..=steps).map(|k| k as f32 * LINEAR_STEP).collect()
        }
    };
    let mut out = Vec::new();
    let mut last_y = f32::MAX;
    for hz in candidates {
        if hz < MIN_HZ || hz > nyquist {
            continue;
        }
        let y = freq_to_row(axis, hz, height_px, MIN_HZ, nyquist);
        if (last_y - y).abs() < 14.0 {
            continue;
        }
        last_y = y;
        out.push((y, format_hz(hz)));
    }
    out
}

fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.0}k", hz / 1000.0)
    } else {
        format!("{hz:.0}")
    }
}

fn yin_window_len(rate: f32) -> usize {
    let tau_max = (rate / PITCH_MIN_HZ) as usize;
    // Strictly more than `2 * tau_max`, so `yin_pitch`'s own length check always passes for a
    // window sized by this function: it needs `n = len - tau_max >= 1` comparison pairs per lag.
    tau_max * 2 + 1
}

/// Classic YIN pitch estimation: a difference function, cumulative mean normalization, an
/// absolute threshold with parabolic interpolation. Returns `None` for unvoiced/no-pitch content.
fn yin_pitch(samples: &[f32], rate: f32) -> Option<f32> {
    let tau_max = (rate / PITCH_MIN_HZ) as usize;
    let tau_min = (rate / PITCH_MAX_HZ).max(2.0) as usize;
    if tau_max < 2 || samples.len() <= tau_max * 2 {
        return None;
    }
    let n = samples.len() - tau_max;
    let mut diff = vec![0.0f32; tau_max + 1];
    for (tau, slot) in diff.iter_mut().enumerate().skip(1) {
        let mut sum = 0.0f32;
        for i in 0..n {
            let d = samples[i] - samples[i + tau];
            sum += d * d;
        }
        *slot = sum;
    }
    let mut cmnd = vec![1.0f32; tau_max + 1];
    let mut running = 0.0f32;
    for tau in 1..=tau_max {
        running += diff[tau];
        cmnd[tau] = if running > 0.0 {
            diff[tau] * tau as f32 / running
        } else {
            1.0
        };
    }
    let mut tau_est = None;
    let mut tau = tau_min.max(1);
    while tau <= tau_max {
        if cmnd[tau] < YIN_THRESHOLD {
            while tau < tau_max && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            tau_est = Some(tau);
            break;
        }
        tau += 1;
    }
    let tau = tau_est?;
    let (x0, x1, x2) = (
        if tau > 1 { cmnd[tau - 1] } else { cmnd[tau] },
        cmnd[tau],
        if tau < tau_max {
            cmnd[tau + 1]
        } else {
            cmnd[tau]
        },
    );
    let denom = x0 - 2.0 * x1 + x2;
    let shift = if denom.abs() > 1e-12 {
        (0.5 * (x0 - x2) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let refined = tau as f32 + shift;
    (refined > 0.0).then(|| rate / refined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_channel_produces_a_correctly_shaped_nonblack_image() {
        let rate = 48_000;
        let n = rate as usize * 2;
        let rope = Rope::from_samples(
            &(0..n)
                .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / rate as f32).sin() * 0.3)
                .collect::<Vec<_>>(),
        );
        let cancel = AtomicBool::new(false);
        for mode in [ViewMode::Frequency, ViewMode::Pitch] {
            let opts = SpectralOptions {
                mode,
                fft_size: 2048,
                ..SpectralOptions::default()
            };
            let stamp = Stamp {
                revision: 0,
                start: 0.0,
                frames_per_px: n as f64 / 400.0,
                width_px: 400,
                height_px: 150,
                rate,
                channel_count: 1,
                mask: ChannelMask::ALL,
                opts,
            };
            let img = render_channel(&rope, stamp, &cancel).expect("should render");
            assert_eq!(img.width, 400);
            assert_eq!(img.height, 150);
            assert_eq!(img.rgba.len(), 400 * 150 * 4);
            assert!(
                img.rgba.iter().any(|&b| b != 0),
                "image should not be all black"
            );
            if mode == ViewMode::Pitch {
                let voiced: Vec<f32> = img.pitch.iter().flatten().copied().collect();
                assert!(
                    !voiced.is_empty(),
                    "a clean 440 Hz tone should be voiced somewhere"
                );
                for hz in voiced {
                    assert!(
                        hz.is_finite() && (hz - 440.0).abs() < 10.0,
                        "detected {hz}, expected ~440"
                    );
                }
            }
        }
    }

    #[test]
    fn yin_finds_a_known_tone() {
        let rate = 48_000.0;
        let freq = 220.0;
        let n = yin_window_len(rate);
        let samples: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / rate).sin())
            .collect();
        let detected = yin_pitch(&samples, rate).expect("a clean tone should be voiced");
        assert!(
            (detected - freq).abs() < 2.0,
            "detected {detected}, expected ~{freq}"
        );
    }

    #[test]
    fn yin_silence_is_unvoiced_or_stable() {
        let rate = 48_000.0;
        let n = yin_window_len(rate);
        let samples = vec![0.0f32; n];
        // Silence must not panic or divide by zero; a result (if any) should be finite.
        if let Some(hz) = yin_pitch(&samples, rate) {
            assert!(hz.is_finite());
        }
    }

    #[test]
    fn row_freq_round_trips_through_its_inverse() {
        for axis in FreqAxis::ALL {
            let (height, min_hz, max_hz) = (300usize, MIN_HZ, 20_000.0f32);
            for row in [0usize, 50, 150, 299] {
                let freq = row_freq(axis, row, height, min_hz, max_hz);
                let back = freq_to_row(axis, freq, height, min_hz, max_hz);
                assert!(
                    (back - row as f32).abs() < 0.5,
                    "{axis:?} row {row}: freq {freq} -> row {back}"
                );
            }
        }
    }

    #[test]
    fn color_maps_cover_the_full_range_without_panicking() {
        for map in ColorMap::ALL {
            let black = map.color(0.0);
            let white = map.color(1.0);
            assert_ne!(black, white);
            for i in 0..=10 {
                map.color(i as f32 / 10.0);
            }
        }
    }
}
