/// Horizontal view of a document: which frame is at the left edge and how many frames per pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub start: f64,
    pub frames_per_px: f64,
}

impl View {
    pub fn fit(total_frames: usize, width_px: f32) -> View {
        View {
            start: 0.0,
            frames_per_px: (total_frames as f64 / width_px.max(1.0) as f64).max(MIN_FPP),
        }
    }

    pub fn px_to_frame(&self, px: f32) -> f64 {
        self.start + px as f64 * self.frames_per_px
    }

    pub fn frame_to_px(&self, frame: f64) -> f32 {
        ((frame - self.start) / self.frames_per_px) as f32
    }

    pub fn visible_frames(&self, width_px: f32) -> f64 {
        width_px as f64 * self.frames_per_px
    }

    /// Zoom by `factor` (>1 zooms in) keeping the frame under `anchor_px` fixed.
    pub fn zoom(&mut self, factor: f64, anchor_px: f32, total_frames: usize, width_px: f32) {
        let anchor = self.px_to_frame(anchor_px);
        let max_fpp = (total_frames as f64 / width_px.max(1.0) as f64).max(MIN_FPP);
        self.frames_per_px = (self.frames_per_px / factor).clamp(MIN_FPP, max_fpp);
        self.start = anchor - anchor_px as f64 * self.frames_per_px;
        self.clamp(total_frames, width_px);
    }

    pub fn pan_px(&mut self, dx: f32, total_frames: usize, width_px: f32) {
        self.start += dx as f64 * self.frames_per_px;
        self.clamp(total_frames, width_px);
    }

    /// Keep `frame` visible, scrolling the minimum amount.
    pub fn reveal(&mut self, frame: f64, total_frames: usize, width_px: f32) {
        let vis = self.visible_frames(width_px);
        if frame < self.start {
            self.start = frame;
        } else if frame > self.start + vis {
            self.start = frame - vis;
        }
        self.clamp(total_frames, width_px);
    }

    pub fn clamp(&mut self, total_frames: usize, width_px: f32) {
        let vis = self.visible_frames(width_px);
        let max_start = (total_frames as f64 - vis).max(0.0);
        self.start = self.start.clamp(0.0, max_start);
    }
}

/// One sample per 1/8 pixel is as far as it is useful to zoom in.
pub const MIN_FPP: f64 = 0.125;

/// Format a frame position as `m:ss.mmm` (or `h:mm:ss.mmm`).
pub fn format_time(frame: f64, sample_rate: u32) -> String {
    let total_ms = (frame / sample_rate as f64 * 1000.0).round().max(0.0) as u64;
    let (ms, s, m, h) = (
        total_ms % 1000,
        total_ms / 1000 % 60,
        total_ms / 60_000 % 60,
        total_ms / 3_600_000,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

pub fn db(linear: f32) -> f32 {
    20.0 * linear.max(1e-6).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut v = View::fit(1_000_000, 1000.0);
        v.zoom(4.0, 250.0, 1_000_000, 1000.0);
        let before = View::fit(1_000_000, 1000.0).px_to_frame(250.0);
        assert!((v.px_to_frame(250.0) - before).abs() < 1.0);
        assert!(v.frames_per_px < 1000.0);
    }

    #[test]
    fn zoom_out_is_bounded_by_fit() {
        let mut v = View::fit(1000, 100.0);
        v.zoom(0.001, 0.0, 1000, 100.0);
        assert_eq!(v.frames_per_px, 10.0);
        assert_eq!(v.start, 0.0);
    }

    #[test]
    fn pan_clamps_to_ends() {
        let mut v = View {
            start: 0.0,
            frames_per_px: 1.0,
        };
        v.pan_px(-50.0, 1000, 100.0);
        assert_eq!(v.start, 0.0);
        v.pan_px(5000.0, 1000, 100.0);
        assert_eq!(v.start, 900.0);
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(48_000.0 * 61.5, 48_000), "1:01.500");
        assert_eq!(format_time(48_000.0 * 3661.0, 48_000), "1:01:01.000");
    }
}
