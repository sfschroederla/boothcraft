use crate::view::db;
use egui::{Color32, Rect, Sense, Stroke, Ui, Vec2};

const FLOOR_DB: f32 = -60.0;
const HOLD_SECS: f64 = 1.5;
const FALL_DB_PER_SEC: f32 = 24.0;

/// Peak level bars with peak-hold ticks.
#[derive(Default)]
pub struct Meters {
    level_db: [f32; 8],
    hold_db: [f32; 8],
    hold_until: [f64; 8],
    last_t: f64,
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

    pub fn show(&self, ui: &mut Ui, channels: usize) {
        let channels = channels.clamp(1, 2);
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 18.0 * channels as f32 + 16.0),
            Sense::hover(),
        );
        let p = ui.painter_at(rect);
        let bar_h = 14.0;
        let to_x = |d: f32| rect.left() + (d - FLOOR_DB) / -FLOOR_DB * rect.width();
        for c in 0..channels {
            let top = rect.top() + c as f32 * 18.0;
            let bar = Rect::from_min_max(
                egui::pos2(rect.left(), top),
                egui::pos2(rect.right(), top + bar_h),
            );
            p.rect_filled(bar, 2.0, Color32::from_gray(25));
            let fill =
                Rect::from_min_max(bar.min, egui::pos2(to_x(self.level_db[c]), bar.bottom()));
            let color = if self.level_db[c] > -3.0 {
                Color32::from_rgb(230, 70, 60)
            } else if self.level_db[c] > -12.0 {
                Color32::from_rgb(230, 200, 60)
            } else {
                Color32::from_rgb(70, 200, 110)
            };
            p.rect_filled(fill, 2.0, color);
            let hx = to_x(self.hold_db[c]);
            p.line_segment(
                [egui::pos2(hx, top), egui::pos2(hx, top + bar_h)],
                Stroke::new(2.0, Color32::WHITE),
            );
        }
        for tick in [-60.0, -48.0, -36.0, -24.0, -12.0, -6.0, 0.0] {
            let x = to_x(tick);
            let y = rect.top() + channels as f32 * 18.0;
            p.text(
                egui::pos2(x, y),
                egui::Align2::CENTER_TOP,
                format!("{tick:.0}"),
                egui::FontId::proportional(9.0),
                Color32::GRAY,
            );
        }
    }
}
