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
}

impl TimeFormat {
    pub const ALL: [TimeFormat; 4] = [
        TimeFormat::Time,
        TimeFormat::Decimal,
        TimeFormat::Samples,
        TimeFormat::Smpte,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TimeFormat::Time => "Minutes:Seconds",
            TimeFormat::Decimal => "Decimal seconds",
            TimeFormat::Samples => "Samples",
            TimeFormat::Smpte => "SMPTE 30 fps",
        }
    }
}

pub const SMPTE_FPS: f64 = 30.0;

/// One horizontal amplitude gridline. `value` is a linear amplitude in `0..=1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Gridline {
    pub value: f32,
    pub label: String,
}

const DB_LEVELS: [f32; 12] = [
    0.0, -3.0, -6.0, -9.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0, -72.0, -96.0,
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
                accept(
                    10f32.powf(db / 20.0),
                    format!("{db:.0}").replace('-', "−"),
                    &mut out,
                );
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
    }
}

fn fraction_digits(step_secs: f64) -> usize {
    if step_secs >= 1.0 {
        0
    } else {
        ((-step_secs.log10()).ceil() as usize).clamp(1, 3)
    }
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
