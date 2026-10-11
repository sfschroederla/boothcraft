//! Pure logic for the waveform's amplitude scale and time ruler.

use crate::view::{format_time, View};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AmpUnits {
    #[default]
    Db,
    Percent,
    Normalized,
    Sample16,
}

impl AmpUnits {
    pub const ALL: [AmpUnits; 4] = [
        AmpUnits::Db,
        AmpUnits::Percent,
        AmpUnits::Normalized,
        AmpUnits::Sample16,
    ];

    /// Short name shown above the scale.
    pub fn short(self) -> &'static str {
        match self {
            AmpUnits::Db => "dB",
            AmpUnits::Percent => "%",
            AmpUnits::Normalized => "norm",
            AmpUnits::Sample16 => "smp",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AmpUnits::Db => "dB",
            AmpUnits::Percent => "Percent",
            AmpUnits::Normalized => "Normalized (±1)",
            AmpUnits::Sample16 => "Sample value (16-bit)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeFormat {
    #[default]
    Time,
    Decimal,
    Samples,
    Smpte,
    /// Bars and beats at a tempo. BPM is stored ×100 so the type stays `Eq`.
    BarsBeats {
        bpm_x100: u32,
        beats_per_bar: u32,
    },
}

impl TimeFormat {
    pub const ALL: [TimeFormat; 5] = [
        TimeFormat::Time,
        TimeFormat::Decimal,
        TimeFormat::Samples,
        TimeFormat::Smpte,
        TimeFormat::bars_beats(120.0, 4),
    ];

    pub const fn bars_beats(bpm: f32, beats_per_bar: u32) -> TimeFormat {
        TimeFormat::BarsBeats {
            bpm_x100: (bpm * 100.0) as u32,
            beats_per_bar,
        }
    }

    /// Same format, ignoring tempo settings.
    pub fn same_kind(self, other: TimeFormat) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    /// Short name shown in the ruler's corner.
    pub fn short(self) -> &'static str {
        match self {
            TimeFormat::Time => "hms",
            TimeFormat::Decimal => "sec",
            TimeFormat::Samples => "samples",
            TimeFormat::Smpte => "smpte",
            TimeFormat::BarsBeats { .. } => "bars",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TimeFormat::Time => "Minutes:Seconds",
            TimeFormat::Decimal => "Decimal seconds",
            TimeFormat::Samples => "Samples",
            TimeFormat::Smpte => "SMPTE 30 fps",
            TimeFormat::BarsBeats { .. } => "Bars and Beats",
        }
    }
}

pub const SMPTE_FPS: f64 = 30.0;

/// Frames per beat and beats per bar for a bars/beats format.
fn beat_grid(rate: u32, bpm_x100: u32, beats_per_bar: u32) -> (f64, u32) {
    let bpm = (bpm_x100.max(100) as f64) / 100.0;
    (rate as f64 * 60.0 / bpm, beats_per_bar.max(1))
}

/// One horizontal amplitude gridline. `value` is a linear amplitude in `0..=1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Gridline {
    pub value: f32,
    pub label: String,
}

const DB_LEVELS: [f32; 22] = [
    0.0, -0.5, -1.0, -1.5, -2.0, -2.5, -3.0, -4.0, -5.0, -6.0, -7.0, -9.0, -12.0, -15.0, -18.0,
    -24.0, -30.0, -36.0, -48.0, -60.0, -72.0, -96.0,
];

/// Gridlines for a lane whose half-height is `half_px` at amplitude zoom `zoom`.
/// Lines closer than `min_gap_px` to the previous accepted line, or outside the lane, are dropped.
pub fn amp_gridlines(units: AmpUnits, zoom: f32, half_px: f32, min_gap_px: f32) -> Vec<Gridline> {
    let px = |v: f32| v * zoom * half_px;
    let mut out: Vec<Gridline> = Vec::new();
    let mut last_px = f32::MAX;
    let mut accept = |value: f32, label: String, out: &mut Vec<Gridline>| {
        let y = px(value);
        if y <= half_px + 0.5 && (last_px - y).abs() >= min_gap_px {
            last_px = y;
            out.push(Gridline { value, label });
        }
    };
    match units {
        AmpUnits::Db => {
            for db in DB_LEVELS {
                accept(10f32.powf(db / 20.0), db_label(db), &mut out);
            }
        }
        _ => {
            // Choose the smallest nice step that keeps lines apart, then walk down from full scale.
            let steps = [0.05f32, 0.1, 0.25, 0.5, 1.0];
            let step = steps
                .into_iter()
                .find(|s| px(*s) >= min_gap_px)
                .unwrap_or(1.0);
            let n = (1.0 / step).round() as i32;
            for k in (1..=n).rev() {
                let v = k as f32 * step;
                accept(v, linear_label(units, v), &mut out);
            }
        }
    }
    out
}

/// Fine levels near full scale get a decimal (`-0.5`), the rest are whole numbers.
fn db_label(db: f32) -> String {
    if db == 0.0 {
        "0".into()
    } else if db > -3.0 {
        format!("{db:.1}")
    } else {
        format!("{db:.0}")
    }
}

fn linear_label(units: AmpUnits, v: f32) -> String {
    match units {
        AmpUnits::Percent => format!("{:.0}%", v * 100.0),
        AmpUnits::Normalized => {
            let s = format!("{v:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
        AmpUnits::Sample16 => format!("{:.0}", (v * 32768.0).min(32767.0)),
        AmpUnits::Db => String::new(),
    }
}

/// Label for the centre line.
pub fn zero_label(units: AmpUnits) -> &'static str {
    if units == AmpUnits::Db {
        "−∞"
    } else {
        "0"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub frame: f64,
    pub major: bool,
    pub label: Option<String>,
}

/// Nice step (1, 2, 5 × 10^k) at least `min`, with the number of minor divisions that suits it.
fn decimal_step(min: f64, floor: f64) -> (f64, u32) {
    let mut base = floor;
    loop {
        for (m, div) in [(1.0, 10u32), (2.0, 4), (5.0, 5)] {
            if base * m >= min {
                return (base * m, div);
            }
        }
        base *= 10.0;
    }
}

/// Seconds-based steps: decimal below one second, then whole minutes/hours that divide cleanly.
fn seconds_step(min_secs: f64) -> (f64, u32) {
    if min_secs < 1.0 {
        return decimal_step(min_secs, 0.001);
    }
    const STEPS: [(f64, u32); 13] = [
        (1.0, 5),
        (2.0, 4),
        (5.0, 5),
        (10.0, 5),
        (15.0, 3),
        (30.0, 6),
        (60.0, 6),
        (120.0, 4),
        (300.0, 5),
        (600.0, 5),
        (900.0, 3),
        (1800.0, 6),
        (3600.0, 6),
    ];
    STEPS
        .into_iter()
        .find(|(s, _)| *s >= min_secs)
        .unwrap_or_else(|| {
            let hours = (min_secs / 3600.0).ceil();
            (hours * 3600.0, 6)
        })
}

/// Ruler ticks covering the visible span, with labels on major ticks about `label_px` apart.
pub fn time_ticks(v: &View, width_px: f32, rate: u32, fmt: TimeFormat, label_px: f32) -> Vec<Tick> {
    let rate_f = rate as f64;
    let (major_frames, div, step_secs) = match fmt {
        TimeFormat::BarsBeats {
            bpm_x100,
            beats_per_bar,
        } => {
            let (beat, bpb) = beat_grid(rate, bpm_x100, beats_per_bar);
            let min = v.frames_per_px * label_px as f64;
            if beat >= min {
                // Label every beat, with sixteenth-note minor ticks.
                (beat, 4, beat / rate_f)
            } else {
                let bar = beat * bpb as f64;
                let mut n = 1u32;
                while (n as f64) * bar < min {
                    n *= 2;
                }
                (
                    n as f64 * bar,
                    if n == 1 { bpb } else { n.min(16) },
                    n as f64 * bar / rate_f,
                )
            }
        }
        TimeFormat::Samples => {
            let (step, div) = decimal_step(v.frames_per_px * label_px as f64, 1.0);
            let div = if step < 10.0 {
                if step >= 5.0 {
                    5
                } else if step >= 2.0 {
                    2
                } else {
                    1
                }
            } else {
                div
            };
            (step, div, step / rate_f)
        }
        _ => {
            let (mut secs, mut div) = seconds_step(v.frames_per_px / rate_f * label_px as f64);
            if fmt == TimeFormat::Smpte && secs < 1.0 / SMPTE_FPS {
                secs = 1.0 / SMPTE_FPS;
                div = 1;
            }
            (secs * rate_f, div, secs)
        }
    };
    let minor = major_frames / div as f64;
    let first = (v.start / minor).floor() as i64;
    let last = ((v.start + v.visible_frames(width_px)) / minor).ceil() as i64;
    (first..=last)
        .filter(|i| *i >= 0)
        .map(|i| {
            let frame = i as f64 * minor;
            let major = i % div as i64 == 0;
            let label = major.then(|| format_tick(frame, rate, fmt, step_secs));
            Tick {
                frame,
                major,
                label,
            }
        })
        .collect()
}

/// Format a ruler label; `step_secs` decides how many fractional digits are worth showing.
pub fn format_tick(frame: f64, rate: u32, fmt: TimeFormat, step_secs: f64) -> String {
    match fmt {
        TimeFormat::Samples => format!("{}", frame.round() as u64),
        TimeFormat::Decimal => {
            let digits = fraction_digits(step_secs);
            format!("{:.*}", digits, frame / rate as f64)
        }
        TimeFormat::Time => {
            if step_secs >= 1.0 {
                let total = (frame / rate as f64).round() as u64;
                let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
                if h > 0 {
                    format!("{h}:{m:02}:{s:02}")
                } else {
                    format!("{m}:{s:02}")
                }
            } else {
                let full = format_time(frame, rate);
                let keep = fraction_digits(step_secs);
                let cut = full.len() - (3 - keep);
                full[..cut].trim_end_matches('.').to_string()
            }
        }
        TimeFormat::Smpte => format_smpte(frame, rate),
        TimeFormat::BarsBeats {
            bpm_x100,
            beats_per_bar,
        } => {
            let (beat, bpb) = beat_grid(rate, bpm_x100, beats_per_bar);
            let beats = (frame / beat).round() as u64;
            let (bar, in_bar) = (beats / bpb as u64 + 1, beats % bpb as u64 + 1);
            if step_secs * rate as f64 >= beat * bpb as f64 - 0.5 {
                format!("{bar}")
            } else {
                format!("{bar}:{in_bar}")
            }
        }
    }
}

fn fraction_digits(step_secs: f64) -> usize {
    if step_secs >= 1.0 {
        0
    } else {
        ((-step_secs.log10()).ceil() as usize).clamp(1, 3)
    }
}

/// Full-precision readout of a position, for the timecode display and panels.
pub fn format_position(frame: f64, rate: u32, fmt: TimeFormat) -> String {
    match fmt {
        TimeFormat::Time => format_time(frame, rate),
        TimeFormat::Decimal => format!("{:.3}", frame / rate as f64),
        TimeFormat::Samples => format!("{}", frame.round().max(0.0) as u64),
        TimeFormat::Smpte => format_smpte(frame, rate),
        TimeFormat::BarsBeats {
            bpm_x100,
            beats_per_bar,
        } => {
            let (beat, bpb) = beat_grid(rate, bpm_x100, beats_per_bar);
            let beats = (frame / beat).max(0.0);
            let bar = (beats / bpb as f64).floor();
            format!("{}:{:.2}", bar as u64 + 1, beats - bar * bpb as f64 + 1.0)
        }
    }
}

/// Parse text typed in `fmt` back to a frame position. Inverse of [`format_position`].
pub fn parse_position(text: &str, rate: u32, fmt: TimeFormat) -> Option<f64> {
    let t = text.trim();
    let num = |s: &str| {
        s.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    let rate_f = rate as f64;
    let frames = match fmt {
        TimeFormat::Samples => num(t)?,
        TimeFormat::Decimal => num(t)? * rate_f,
        TimeFormat::Time => {
            let parts: Vec<&str> = t.split(':').collect();
            if parts.len() > 3 {
                return None;
            }
            let mut secs = 0.0;
            for p in &parts {
                secs = secs * 60.0 + num(p)?;
            }
            secs * rate_f
        }
        TimeFormat::Smpte => {
            let parts: Vec<f64> = t.split(':').map(num).collect::<Option<_>>()?;
            if parts.is_empty() || parts.len() > 4 {
                return None;
            }
            let (ff, rest) = parts.split_last()?;
            let secs = rest.iter().fold(0.0, |acc, v| acc * 60.0 + v);
            (secs + ff / SMPTE_FPS) * rate_f
        }
        TimeFormat::BarsBeats {
            bpm_x100,
            beats_per_bar,
        } => {
            let (beat, bpb) = beat_grid(rate, bpm_x100, beats_per_bar);
            let (bar, in_bar) = match t.split_once(':') {
                Some((b, x)) => (num(b)?, num(x)?),
                None => (num(t)?, 1.0),
            };
            if bar < 1.0 || in_bar < 1.0 {
                return None;
            }
            ((bar - 1.0) * bpb as f64 + (in_bar - 1.0)) * beat
        }
    };
    Some(frames)
}

pub fn format_smpte(frame: f64, rate: u32) -> String {
    let total_frames = (frame / rate as f64 * SMPTE_FPS).round().max(0.0) as u64;
    let fps = SMPTE_FPS as u64;
    let (f, s, m, h) = (
        total_frames % fps,
        total_frames / fps % 60,
        total_frames / (fps * 60) % 60,
        total_frames / (fps * 3600),
    );
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(fpp: f64) -> View {
        View {
            start: 0.0,
            frames_per_px: fpp,
            amp: 1.0,
        }
    }

    #[test]
    fn db_gridlines_are_ordered_and_spaced() {
        let g = amp_gridlines(AmpUnits::Db, 1.0, 100.0, 16.0);
        assert!(g.len() >= 4);
        assert_eq!(g[0].label, "0");
        assert!(g.windows(2).all(|w| w[0].value > w[1].value));
        for w in g.windows(2) {
            assert!((w[0].value - w[1].value) * 100.0 >= 16.0 - 1e-3);
        }
    }

    #[test]
    fn amp_zoom_hides_offscreen_levels_and_adds_detail() {
        let g1 = amp_gridlines(AmpUnits::Db, 1.0, 100.0, 16.0);
        let g8 = amp_gridlines(AmpUnits::Db, 8.0, 100.0, 16.0);
        assert!(
            g8.iter().all(|l| l.value * 8.0 <= 1.0 + 0.01),
            "everything shown fits the lane"
        );
        assert!(g8.first().unwrap().value < g1.first().unwrap().value);
    }

    #[test]
    fn db_labels_use_decimals_only_near_full_scale() {
        let g = amp_gridlines(AmpUnits::Db, 1.0, 500.0, 18.0);
        let labels: Vec<&str> = g.iter().map(|l| l.label.as_str()).collect();
        assert!(
            labels.contains(&"-0.5") && labels.contains(&"-1.0"),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"-12") && !labels.contains(&"-12.0"),
            "{labels:?}"
        );
    }

    #[test]
    fn position_formats() {
        assert_eq!(
            format_position(48_000.0 * 61.5, 48_000, TimeFormat::Time),
            "1:01.500"
        );
        assert_eq!(
            format_position(48_000.0, 48_000, TimeFormat::Samples),
            "48000"
        );
        assert_eq!(
            format_position(24_000.0, 48_000, TimeFormat::Decimal),
            "0.500"
        );
    }

    #[test]
    fn parse_inverts_format() {
        let rate = 48_000;
        let frame = 48_000.0 * 75.5; // 1:15.500
        for fmt in TimeFormat::ALL {
            let text = format_position(frame, rate, fmt);
            let back = parse_position(&text, rate, fmt)
                .unwrap_or_else(|| panic!("{fmt:?} could not parse {text}"));
            assert!(
                (back - frame).abs() <= rate as f64 / SMPTE_FPS,
                "{fmt:?}: {text} -> {back}"
            );
        }
        assert_eq!(
            parse_position("2:00", rate, TimeFormat::Time),
            Some(120.0 * 48_000.0)
        );
        assert_eq!(
            parse_position("1:00:00.5", rate, TimeFormat::Time),
            Some(3600.5 * 48_000.0)
        );
        assert_eq!(
            parse_position("3:2", rate, TimeFormat::bars_beats(120.0, 4)),
            Some(9.0 * 24_000.0)
        );
        for bad in ["", "abc", "-1", "1:2:3:4"] {
            assert_eq!(parse_position(bad, rate, TimeFormat::Time), None, "{bad:?}");
        }
        assert_eq!(
            parse_position("0:1", rate, TimeFormat::bars_beats(120.0, 4)),
            None,
            "bars start at 1"
        );
    }

    #[test]
    fn bar_ticks_land_on_bars_and_beats() {
        let fmt = TimeFormat::bars_beats(120.0, 4); // beat = 24000 frames at 48 kHz
        let zoomed_in = View {
            start: 0.0,
            frames_per_px: 100.0,
            amp: 1.0,
        };
        let t = time_ticks(&zoomed_in, 1000.0, 48_000, fmt, 90.0);
        let majors: Vec<&Tick> = t.iter().filter(|x| x.major).collect();
        assert_eq!(majors[1].frame, 24_000.0);
        assert_eq!(majors[1].label.as_deref(), Some("1:2"));
        let zoomed_out = View {
            start: 0.0,
            frames_per_px: 2000.0,
            amp: 1.0,
        };
        let t = time_ticks(&zoomed_out, 1000.0, 48_000, fmt, 90.0);
        let majors: Vec<&Tick> = t.iter().filter(|x| x.major).collect();
        assert_eq!(majors[1].frame % 96_000.0, 0.0, "majors on bar lines");
        assert!(majors[1].label.as_deref().is_some_and(|l| !l.contains(':')));
    }

    #[test]
    fn linear_units_label_correctly() {
        let g = amp_gridlines(AmpUnits::Percent, 1.0, 100.0, 20.0);
        assert_eq!(g[0].label, "100%");
        assert!(g.iter().any(|l| l.label == "50%"));
        let n = amp_gridlines(AmpUnits::Normalized, 1.0, 100.0, 20.0);
        assert!(n.iter().any(|l| l.label == "0.5"));
        let s = amp_gridlines(AmpUnits::Sample16, 1.0, 100.0, 20.0);
        assert_eq!(s[0].label, "32767");
    }

    #[test]
    fn ticks_have_labels_only_on_majors_and_are_increasing() {
        let t = time_ticks(&view(100.0), 1000.0, 48_000, TimeFormat::Time, 90.0);
        assert!(t.iter().any(|x| x.major) && t.iter().any(|x| !x.major));
        assert!(t.iter().all(|x| x.major == x.label.is_some()));
        assert!(t.windows(2).all(|w| w[0].frame < w[1].frame));
        let majors: Vec<&Tick> = t.iter().filter(|x| x.major).collect();
        let gap_px = (majors[1].frame - majors[0].frame) / 100.0;
        assert!(gap_px >= 90.0, "labels at least 90px apart, got {gap_px}");
    }

    #[test]
    fn label_formats() {
        assert_eq!(
            format_tick(48_000.0 * 125.0, 48_000, TimeFormat::Time, 5.0),
            "2:05"
        );
        assert_eq!(
            format_tick(48_000.0 * 1.25, 48_000, TimeFormat::Time, 0.05),
            "0:01.25"
        );
        assert_eq!(
            format_tick(24_000.0, 48_000, TimeFormat::Decimal, 0.1),
            "0.5"
        );
        assert_eq!(
            format_tick(44_100.0, 44_100, TimeFormat::Samples, 1.0),
            "44100"
        );
        assert_eq!(format_smpte(48_000.0 * 61.5, 48_000), "00:01:01:15");
    }

    #[test]
    fn sample_ticks_step_by_whole_samples_when_zoomed_in() {
        let t = time_ticks(&view(0.125), 800.0, 48_000, TimeFormat::Samples, 90.0);
        assert!(t.iter().filter(|x| x.major).all(|x| x.frame.fract() == 0.0));
        assert!(t.iter().filter(|x| x.major).count() >= 2);
    }

    #[test]
    fn smpte_never_ticks_finer_than_a_frame() {
        let t = time_ticks(&view(0.5), 1000.0, 48_000, TimeFormat::Smpte, 90.0);
        let majors: Vec<f64> = t.iter().filter(|x| x.major).map(|x| x.frame).collect();
        assert!(majors
            .windows(2)
            .all(|w| w[1] - w[0] >= 48_000.0 / SMPTE_FPS - 1e-6));
    }
}
