use crate::view::db;
use egui::{pos2, vec2, Color32, Rect, Sense, Stroke, Ui};

const FLOOR_DB: f32 = -72.0;
const HOLD_SECS: f64 = 1.5;
const FALL_DB_PER_SEC: f32 = 24.0;
const BAR_BG: Color32 = Color32::from_rgb(22, 22, 22);

/// Peak level bars with peak-hold ticks.
#[derive(Default)]
pub struct Meters {
    level_db: [f32; 8],
    hold_db: [f32; 8],
    hold_until: [f64; 8],
    last_t: f64,
    phase: Option<f32>,
}

impl Meters {
    pub fn update(&mut self, peaks: &[f32], now: f64) {
        let dt = (now - self.last_t).clamp(0.0, 0.25) as f32;
        self.last_t = now;
        for (i, &p) in peaks.iter().enumerate().take(8) {
            let d = db(p).max(FLOOR_DB);
            self.level_db[i] = if d > self.level_db[i] {
                d
            } else {
                (self.level_db[i] - FALL_DB_PER_SEC * dt)
                    .max(d)
                    .max(FLOOR_DB)
            };
            if d >= self.hold_db[i] {
                self.hold_db[i] = d;
                self.hold_until[i] = now + HOLD_SECS;
            } else if now > self.hold_until[i] {
                self.hold_db[i] = self.level_db[i];
            }
        }
    }

    pub fn update_phase(&mut self, correlation: Option<f32>) {
        self.phase = match (correlation, self.phase) {
            (Some(c), Some(prev)) => Some(prev + (c - prev) * 0.35),
            (Some(c), None) => Some(c),
            (None, _) => None,
        };
    }

    /// Horizontal bars (one per channel) over a dB scale, filling the available width.
    pub fn show_levels(&self, ui: &mut Ui, channels: usize) {
        let channels = channels.clamp(1, 2);
        let scale_h = 14.0;
        let avail = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(vec2(avail.x, avail.y.max(30.0)), Sense::hover());
        let p = ui.painter_at(rect);
        let label_w = 22.0;
        let bars = Rect::from_min_max(
            pos2(rect.left() + label_w, rect.top() + 2.0),
            pos2(rect.right() - 6.0, rect.bottom() - scale_h),
        );
        let to_x = |d: f32| bars.left() + (d - FLOOR_DB) / -FLOOR_DB * bars.width();
        let bar_h = (bars.height() / channels as f32 - 2.0).clamp(6.0, 40.0);
        for c in 0..channels {
            let top = bars.top() + c as f32 * (bar_h + 2.0);
            let bar = Rect::from_min_size(pos2(bars.left(), top), vec2(bars.width(), bar_h));
            p.rect_filled(bar, 0.0, BAR_BG);
            let level = self.level_db[c];
            // Segmented fill: thin dark gaps over a solid bar, like a hardware meter.
            let fill_right = to_x(level);
            for (from, to, color) in [
                (FLOOR_DB, -18.0, Color32::from_rgb(70, 200, 110)),
                (-18.0, -6.0, Color32::from_rgb(220, 200, 60)),
                (-6.0, 0.0, Color32::from_rgb(230, 70, 60)),
            ] {
                let (x0, x1) = (to_x(from), to_x(to).min(fill_right));
                if x1 > x0 {
                    p.rect_filled(
                        Rect::from_min_max(pos2(x0, top), pos2(x1, top + bar_h)),
                        0.0,
                        color,
                    );
                }
            }
            let mut x = bars.left();
            while x < bars.right() {
                p.line_segment(
                    [pos2(x, top), pos2(x, top + bar_h)],
                    Stroke::new(1.0, BAR_BG),
                );
                x += 3.0;
            }
            let hx = to_x(self.hold_db[c]);
            if self.hold_db[c] > FLOOR_DB + 0.5 {
                p.line_segment(
                    [pos2(hx, top), pos2(hx, top + bar_h)],
                    Stroke::new(2.0, Color32::from_rgb(255, 230, 80)),
                );
            }
        }
        let font = egui::FontId::proportional(9.0);
        p.text(
            pos2(rect.left() + 2.0, rect.bottom() - 1.0),
            egui::Align2::LEFT_BOTTOM,
            "dB",
            font.clone(),
            Color32::GRAY,
        );
        let mut tick = FLOOR_DB + 3.0;
        while tick <= 0.01 {
            let x = to_x(tick);
            p.text(
                pos2(x, rect.bottom() - 1.0),
                egui::Align2::CENTER_BOTTOM,
                format!("{tick:.0}"),
                font.clone(),
                Color32::GRAY,
            );
            tick += 3.0;
        }
    }

    /// Phase correlation bar from -1 (red) through 0 (yellow) to +1 (green), with a position marker.
    pub fn show_phase(&self, ui: &mut Ui) {
        let avail = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(vec2(avail.x, avail.y.max(30.0)), Sense::hover());
        let p = ui.painter_at(rect);
        let bar = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.top() + 4.0),
            pos2(rect.right() - 8.0, rect.bottom() - 16.0),
        );
        let segments = 90;
        for i in 0..segments {
            let t = i as f32 / (segments - 1) as f32;
            let x0 = bar.left() + bar.width() * i as f32 / segments as f32;
            let x1 = bar.left() + bar.width() * (i + 1) as f32 / segments as f32;
            p.rect_filled(
                Rect::from_min_max(pos2(x0, bar.top()), pos2(x1 + 0.5, bar.bottom())),
                0.0,
                phase_color(t),
            );
        }
        let font = egui::FontId::proportional(9.0);
        for k in -4..=5 {
            let v = k as f32 * 0.2;
            let x = bar.left() + (v + 1.0) / 2.0 * bar.width();
            p.line_segment(
                [pos2(x, bar.bottom()), pos2(x, bar.bottom() + 3.0)],
                Stroke::new(1.0, Color32::GRAY),
            );
            p.text(
                pos2(x, rect.bottom() - 1.0),
                egui::Align2::CENTER_BOTTOM,
                format!("{v:.1}"),
                font.clone(),
                Color32::GRAY,
            );
        }
        if let Some(c) = self.phase {
            let x = bar.left() + (c.clamp(-1.0, 1.0) + 1.0) / 2.0 * bar.width();
            p.line_segment(
                [pos2(x, bar.top() - 2.0), pos2(x, bar.bottom())],
                Stroke::new(2.5, Color32::WHITE),
            );
        }
    }
}

/// Red at -1, yellow-green around 0, green at +1 (`t` in 0..=1).
fn phase_color(t: f32) -> Color32 {
    let lerp = |a: f32, b: f32, k: f32| a + (b - a) * k;
    let (r, g, b) = if t < 0.5 {
        let k = t / 0.5;
        (
            lerp(180.0, 190.0, k),
            lerp(30.0, 175.0, k),
            lerp(30.0, 60.0, k),
        )
    } else {
        let k = (t - 0.5) / 0.5;
        (
            lerp(190.0, 40.0, k),
            lerp(175.0, 175.0, k),
            lerp(60.0, 30.0, k),
        )
    };
    Color32::from_rgb(r as u8, g as u8, b as u8)
}
