use crate::ops::OpenFile;
use crate::view::{format_time, View};
use boothcraft_core::Selection;
use egui::{pos2, vec2, Color32, Pos2, Rect, Sense, Shape, Stroke, Ui};

const RULER_H: f32 = 22.0;
const OVERVIEW_H: f32 = 36.0;

const WAVE: Color32 = Color32::from_rgb(110, 200, 140);
const WAVE_RMS: Color32 = Color32::from_rgb(60, 140, 90);
const SELECTION: Color32 = Color32::from_rgba_premultiplied(60, 100, 160, 90);

#[derive(Default)]
pub struct WaveformEvents {
    /// Set when the user clicked/dragged to a new cursor position (frames).
    pub seek: Option<usize>,
}

/// Draw the editor for `file` and handle selection, zoom and scroll.
pub fn show(ui: &mut Ui, file: &mut OpenFile, playhead: f64, follow: bool) -> WaveformEvents {
    let mut events = WaveformEvents::default();
    let total = file.doc().frames();
    let rate = file.doc().sample_rate;
    let avail = ui.available_size();
    let (outer, _) = ui.allocate_exact_size(avail, Sense::hover());
    if total == 0 || outer.width() < 20.0 || outer.height() < 80.0 {
        return events;
    }
    let width = outer.width();
    if file.view.frames_per_px <= 0.0
        || (file.view.frames_per_px * width as f64) > total as f64 * 64.0
    {
        file.view = View::fit(total, width);
    }
    file.view.clamp(total, width);

    let ruler = Rect::from_min_size(outer.min, vec2(width, RULER_H));
    let overview = Rect::from_min_size(
        pos2(outer.left(), outer.bottom() - OVERVIEW_H),
        vec2(width, OVERVIEW_H),
    );
    let lanes = Rect::from_min_max(
        pos2(outer.left(), ruler.bottom()),
        pos2(outer.right(), overview.top() - 4.0),
    );

    let resp = ui.interact(lanes, ui.id().with("lanes"), Sense::click_and_drag());
    handle_input(ui, &resp, file, lanes, total, &mut events);
    if follow {
        file.view.reveal(playhead, total, width);
    }

    let view = file.view;
    let painter = ui.painter_at(outer);
    painter.rect_filled(lanes, 0.0, Color32::from_gray(18));
    draw_ruler(&painter, ruler, &view, rate);
    draw_lanes(&painter, lanes, file, &view);
    draw_selection(&painter, lanes, file.selection, &view);
    let x = lanes.left() + view.frame_to_px(playhead);
    if x >= lanes.left() && x <= lanes.right() {
        painter.line_segment(
            [pos2(x, ruler.top()), pos2(x, lanes.bottom())],
            Stroke::new(1.5, Color32::from_rgb(255, 210, 80)),
        );
    }
    if file.selection.is_empty() {
        let cx = lanes.left() + view.frame_to_px(file.selection.start as f64);
        painter.line_segment(
            [pos2(cx, lanes.top()), pos2(cx, lanes.bottom())],
            Stroke::new(1.0, Color32::from_white_alpha(120)),
        );
    }
    draw_overview(ui, &painter, overview, file, playhead);
    events
}

fn handle_input(
    ui: &Ui,
    resp: &egui::Response,
    file: &mut OpenFile,
    lanes: Rect,
    total: usize,
    ev: &mut WaveformEvents,
) {
    let width = lanes.width();
    let frame_at = |p: Pos2, v: &View| {
        (v.px_to_frame(p.x - lanes.left()).round().max(0.0) as usize).min(total)
    };

    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = frame_at(p, &file.view);
            ui.data_mut(|d| d.insert_temp(resp.id, f));
            file.selection = Selection::cursor(f);
        }
    }
    if resp.dragged() {
        if let (Some(p), Some(anchor)) = (
            resp.interact_pointer_pos(),
            ui.data(|d| d.get_temp::<usize>(resp.id)),
        ) {
            file.selection = Selection::new(anchor, frame_at(p, &file.view));
        }
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = frame_at(p, &file.view);
            file.selection = Selection::cursor(f);
            ev.seek = Some(f);
        }
    }
    if resp.hovered() {
        let (zoom, scroll, cmd) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.command));
        if let Some(hover) = resp.hover_pos() {
            let anchor = hover.x - lanes.left();
            if zoom != 1.0 {
                file.view.zoom(zoom as f64, anchor, total, width);
            } else if cmd && scroll.y != 0.0 {
                file.view.zoom(
                    (1.0 + scroll.y as f64 * 0.01).max(0.1),
                    anchor,
                    total,
                    width,
                );
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                let d = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                file.view.pan_px(-d, total, width);
            }
        }
    }
}

fn draw_selection(p: &egui::Painter, lanes: Rect, sel: Selection, v: &View) {
    if sel.is_empty() {
        return;
    }
    let l = (lanes.left() + v.frame_to_px(sel.start as f64)).clamp(lanes.left(), lanes.right());
    let r = (lanes.left() + v.frame_to_px(sel.end as f64)).clamp(lanes.left(), lanes.right());
    if r > l {
        p.rect_filled(
            Rect::from_min_max(pos2(l, lanes.top()), pos2(r, lanes.bottom())),
            0.0,
            SELECTION,
        );
    }
}

fn nice_step(min_secs: f64) -> f64 {
    let mut step = 0.001;
    loop {
        for m in [1.0, 2.0, 5.0] {
            if step * m >= min_secs {
                return step * m;
            }
        }
        step *= 10.0;
    }
}

fn draw_ruler(p: &egui::Painter, rect: Rect, v: &View, rate: u32) {
    p.rect_filled(rect, 0.0, Color32::from_gray(32));
    let secs_per_px = v.frames_per_px / rate as f64;
    let step = nice_step(secs_per_px * 90.0);
    let first = (v.start / rate as f64 / step).floor() as i64;
    let last = ((v.start + v.visible_frames(rect.width())) / rate as f64 / step).ceil() as i64;
    for i in first..=last {
        let frame = i as f64 * step * rate as f64;
        let x = rect.left() + v.frame_to_px(frame);
        if x < rect.left() || x > rect.right() {
            continue;
        }
        p.line_segment(
            [pos2(x, rect.bottom() - 7.0), pos2(x, rect.bottom())],
            Stroke::new(1.0, Color32::GRAY),
        );
        p.text(
            pos2(x + 3.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            format_time(frame, rate),
            egui::FontId::proportional(10.0),
            Color32::LIGHT_GRAY,
        );
    }
}

fn draw_lanes(p: &egui::Painter, lanes: Rect, file: &OpenFile, v: &View) {
    let doc = file.doc();
    let n = doc.channel_count();
    let lane_h = lanes.height() / n as f32;
    let cols = lanes.width().floor() as usize;
    let start = v.start.max(0.0) as usize;
    let end = ((v.start + v.visible_frames(lanes.width())).ceil() as usize).min(doc.frames());
    for (c, rope) in doc.channels.iter().enumerate() {
        let lane = Rect::from_min_size(
            pos2(lanes.left(), lanes.top() + c as f32 * lane_h),
            vec2(lanes.width(), lane_h),
        );
        let mid = lane.center().y;
        let half = lane_h * 0.5 - 2.0;
        let dim = if file.mask.has(c) { 1.0 } else { 0.35 };
        p.line_segment(
            [pos2(lane.left(), mid), pos2(lane.right(), mid)],
            Stroke::new(1.0, Color32::from_gray(50)),
        );
        if c > 0 {
            p.line_segment(
                [lane.left_top(), lane.right_top()],
                Stroke::new(1.0, Color32::from_gray(60)),
            );
        }
        if v.frames_per_px < 1.0 {
            let samples = rope.to_vec(start..end);
            let pts: Vec<Pos2> = samples
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    pos2(
                        lanes.left() + v.frame_to_px((start + i) as f64),
                        mid - s.clamp(-1.0, 1.0) * half,
                    )
                })
                .collect();
            p.add(Shape::line(
                pts.clone(),
                Stroke::new(1.0, WAVE.gamma_multiply(dim)),
            ));
            if v.frames_per_px < 0.25 {
                for pt in pts {
                    p.circle_filled(pt, 2.0, WAVE.gamma_multiply(dim));
                }
            }
            continue;
        }
        // Columns map 1:1 to pixels, so bucket the visible range by pixel.
        let peaks = rope.peaks(
            start..end,
            cols.min(((end - start) as f64 / v.frames_per_px).ceil().max(1.0) as usize),
        );
        let col_w = (end - start) as f64 / peaks.len() as f64 / v.frames_per_px;
        let x0 = lanes.left() + v.frame_to_px(start as f64);
        for (i, pk) in peaks.iter().enumerate() {
            let x = x0 + (i as f64 * col_w) as f32 + 0.5;
            let (top, bot) = (
                mid - pk.max.clamp(-1.0, 1.0) * half,
                mid - pk.min.clamp(-1.0, 1.0) * half,
            );
            p.line_segment(
                [pos2(x, top), pos2(x, bot.max(top + 1.0))],
                Stroke::new(col_w.max(1.0) as f32, WAVE.gamma_multiply(dim)),
            );
            let r = pk.rms.min(1.0) * half;
            p.line_segment(
                [pos2(x, mid - r), pos2(x, mid + r)],
                Stroke::new(col_w.max(1.0) as f32, WAVE_RMS.gamma_multiply(dim)),
            );
        }
    }
}

fn draw_overview(ui: &mut Ui, p: &egui::Painter, rect: Rect, file: &mut OpenFile, playhead: f64) {
    let doc = file.doc();
    let total = doc.frames();
    p.rect_filled(rect, 2.0, Color32::from_gray(24));
    let cols = rect.width().floor().max(1.0) as usize;
    let mid = rect.center().y;
    let half = rect.height() * 0.5 - 2.0;
    let mut merged = vec![(0f32, 0f32); cols];
    for rope in &doc.channels {
        for (m, pk) in merged.iter_mut().zip(rope.peaks(0..total, cols)) {
            m.0 = m.0.min(pk.min);
            m.1 = m.1.max(pk.max);
        }
    }
    for (i, (lo, hi)) in merged.iter().enumerate() {
        let x = rect.left() + i as f32 + 0.5;
        p.line_segment(
            [
                pos2(x, mid - hi * half),
                pos2(x, mid - lo * half - 0.0 + 1.0),
            ],
            Stroke::new(1.0, Color32::from_rgb(80, 150, 110)),
        );
    }
    let v = file.view;
    let px_per_frame = rect.width() as f64 / total as f64;
    let vis_l = rect.left() + (v.start * px_per_frame) as f32;
    let vis_w = (v.visible_frames(rect.width()) * px_per_frame).min(rect.width() as f64) as f32;
    let window = Rect::from_min_size(pos2(vis_l, rect.top()), vec2(vis_w.max(3.0), rect.height()));
    p.rect_stroke(
        window,
        2.0,
        Stroke::new(1.5, Color32::from_rgb(200, 200, 220)),
        egui::StrokeKind::Inside,
    );
    let px = rect.left() + (playhead * px_per_frame) as f32;
    p.line_segment(
        [pos2(px, rect.top()), pos2(px, rect.bottom())],
        Stroke::new(1.0, Color32::from_rgb(255, 210, 80)),
    );

    let resp = ui.interact(rect, ui.id().with("overview"), Sense::click_and_drag());
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let centre = (pos.x - rect.left()) as f64 / px_per_frame;
            let vis = file.view.visible_frames(rect.width());
            file.view.start = centre - vis / 2.0;
            file.view.clamp(total, rect.width());
        }
    }
}
