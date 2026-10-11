use boothcraft_core::Selection;

/// Horizontal view of a document: which frame is at the left edge and how many frames per pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub start: f64,
    pub frames_per_px: f64,
    /// Vertical (amplitude) zoom shared by all channels; 1.0 shows full scale.
    pub amp: f32,
}

pub const AMP_MIN: f32 = 0.25;
pub const AMP_MAX: f32 = 256.0;

impl View {
    pub fn fit(total_frames: usize, width_px: f32) -> View {
        View {
            start: 0.0,
            frames_per_px: (total_frames as f64 / width_px.max(1.0) as f64).max(MIN_FPP),
            amp: 1.0,
        }
    }

    /// Fit the whole file horizontally, keeping the amplitude zoom.
    pub fn fit_horizontal(&mut self, total_frames: usize, width_px: f32) {
        *self = View {
            amp: self.amp,
            ..View::fit(total_frames, width_px)
        };
    }

    /// Show `range` with a little margin on each side.
    pub fn zoom_to_range(
        &mut self,
        range: std::ops::Range<usize>,
        total_frames: usize,
        width_px: f32,
    ) {
        let len = (range.end - range.start).max(1) as f64;
        let max_fpp = (total_frames as f64 / width_px.max(1.0) as f64).max(MIN_FPP);
        self.frames_per_px = (len * 1.1 / width_px.max(1.0) as f64).clamp(MIN_FPP, max_fpp);
        let centre = (range.start + range.end) as f64 / 2.0;
        self.start = centre - self.visible_frames(width_px) / 2.0;
        self.clamp(total_frames, width_px);
    }

    pub fn zoom_amp(&mut self, factor: f32) {
        self.amp = (self.amp * factor).clamp(AMP_MIN, AMP_MAX);
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

/// If a press at pixel `x` (relative to the view's left edge) is within `thresh` pixels of an edge of
/// a non-empty selection, return the frame of the *opposite* edge, which stays fixed while dragging.
pub fn edge_drag_anchor(sel: Selection, view: &View, x: f32, thresh: f32) -> Option<usize> {
    if sel.is_empty() {
        return None;
    }
    let ds = (view.frame_to_px(sel.start as f64) - x).abs();
    let de = (view.frame_to_px(sel.end as f64) - x).abs();
    if ds.min(de) > thresh {
        None
    } else if ds <= de {
        Some(sel.end)
    } else {
        Some(sel.start)
    }
}

/// Anchor for shift-extending a selection to `at`: the edge farther from `at` stays put.
pub fn extend_anchor(sel: Selection, at: usize) -> usize {
    if at.abs_diff(sel.start) <= at.abs_diff(sel.end) {
        sel.end
    } else {
        sel.start
    }
}

/// The candidate nearest to `frame` if it lies within `tol_frames`, otherwise `frame` unchanged.
pub fn snap(frame: f64, candidates: &[f64], tol_frames: f64) -> f64 {
    candidates
        .iter()
        .copied()
        .filter(|c| (c - frame).abs() <= tol_frames)
        .min_by(|a, b| (a - frame).abs().total_cmp(&(b - frame).abs()))
        .unwrap_or(frame)
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
            amp: 1.0,
        };
        v.pan_px(-50.0, 1000, 100.0);
        assert_eq!(v.start, 0.0);
        v.pan_px(5000.0, 1000, 100.0);
        assert_eq!(v.start, 900.0);
    }

    #[test]
    fn zoom_to_range_centres_selection() {
        let mut v = View::fit(1_000_000, 1000.0);
        v.zoom_to_range(100_000..200_000, 1_000_000, 1000.0);
        let (l, r) = (v.frame_to_px(100_000.0), v.frame_to_px(200_000.0));
        assert!(
            l > 0.0 && r < 1000.0,
            "selection visible with margin: {l}..{r}"
        );
        assert!(((l + r) / 2.0 - 500.0).abs() < 1.0);
    }

    #[test]
    fn amp_zoom_is_bounded() {
        let mut v = View::fit(1000, 100.0);
        for _ in 0..20 {
            v.zoom_amp(2.0);
        }
        assert_eq!(v.amp, AMP_MAX);
        for _ in 0..40 {
            v.zoom_amp(0.5);
        }
        assert_eq!(v.amp, AMP_MIN);
    }

    #[test]
    fn edge_pick_and_extend() {
        let v = View {
            start: 0.0,
            frames_per_px: 1.0,
            amp: 1.0,
        };
        let sel = Selection::new(100, 300);
        assert_eq!(edge_drag_anchor(sel, &v, 102.0, 5.0), Some(300));
        assert_eq!(edge_drag_anchor(sel, &v, 297.0, 5.0), Some(100));
        assert_eq!(edge_drag_anchor(sel, &v, 200.0, 5.0), None);
        assert_eq!(
            edge_drag_anchor(Selection::cursor(100), &v, 100.0, 5.0),
            None
        );
        assert_eq!(extend_anchor(sel, 350), 100);
        assert_eq!(extend_anchor(sel, 50), 300);
        assert_eq!(extend_anchor(Selection::cursor(200), 500), 200);
    }

    #[test]
    fn snap_picks_nearest_candidate_in_range() {
        let c = [100.0, 130.0, 500.0];
        assert_eq!(snap(118.0, &c, 20.0), 130.0);
        assert_eq!(snap(112.0, &c, 20.0), 100.0);
        assert_eq!(snap(300.0, &c, 20.0), 300.0, "nothing in range");
        assert_eq!(snap(300.0, &[], 20.0), 300.0);
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(48_000.0 * 61.5, 48_000), "1:01.500");
        assert_eq!(format_time(48_000.0 * 3661.0, 48_000), "1:01:01.000");
    }
}
