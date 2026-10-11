use crate::ops::OpenFile;
use crate::scale::{self, AmpUnits, TimeFormat};
use crate::view::{edge_drag_anchor, extend_anchor, View};
use boothcraft_core::Selection;
use egui::{pos2, vec2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, Ui};

const RULER_H: f32 = 22.0;
const OVERVIEW_H: f32 = 36.0;
/// Width of the amplitude scale strip at the left of the lanes.
const STRIP_W: f32 = 52.0;
const EDGE_GRAB_PX: f32 = 6.0;
const LABEL_PX: f32 = 90.0;

const WAVE: Color32 = Color32::from_rgb(110, 200, 140);
const WAVE_RMS: Color32 = Color32::from_rgb(60, 140, 90);
const SELECTION: Color32 = Color32::from_rgba_premultiplied(60, 100, 160, 90);
const SELECTION_EDGE: Color32 = Color32::from_rgb(130, 170, 240);

/// Display preferences shared by every open file.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayOptions {
    pub amp: AmpUnits,
    pub time: TimeFormat,
}

/// Zoom actions requested from the toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomCmd {
    In,
    Out,
    Selection,
    Full,
    AmpIn,
    AmpOut,
    AmpReset,
}

#[derive(Default)]
pub struct WaveformEvents {
    /// Set when the user clicked to a new cursor position (frames).
    pub seek: Option<usize>,
}

/// Zoom buttons and display-format pickers, drawn above the waveform.
pub fn toolbar(ui: &mut Ui, opts: &mut DisplayOptions, enabled: bool) -> Option<ZoomCmd> {
    let mut cmd = None;
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(enabled, |ui| {
            ui.label("Zoom");
            for (label, c, tip) in [
                ("In", ZoomCmd::In, "Zoom in (time)"),
                ("Out", ZoomCmd::Out, "Zoom out (time)"),
                ("Selection", ZoomCmd::Selection, "Zoom to selection"),
                ("Full", ZoomCmd::Full, "Show the whole file"),
            ] {
                if ui.button(label).on_hover_text(tip).clicked() {
                    cmd = Some(c);
                }
            }
            ui.separator();
            ui.label("Amplitude");
            for (label, c, tip) in [
                ("+", ZoomCmd::AmpIn, "Zoom in (amplitude)"),
                ("−", ZoomCmd::AmpOut, "Zoom out (amplitude)"),
                ("1:1", ZoomCmd::AmpReset, "Reset amplitude zoom"),
            ] {
                if ui.button(label).on_hover_text(tip).clicked() {
                    cmd = Some(c);
                }
            }
            ui.separator();
            egui::ComboBox::from_id_salt("amp_units")
                .selected_text(opts.amp.name())
                .show_ui(ui, |ui| {
                    for u in AmpUnits::ALL {
                        ui.selectable_value(&mut opts.amp, u, u.name());
                    }
                });
            egui::ComboBox::from_id_salt("time_format")
                .selected_text(opts.time.name())
                .show_ui(ui, |ui| {
                    for f in TimeFormat::ALL {
                        ui.selectable_value(&mut opts.time, f, f.name());
                    }
                });
        });
    });
    cmd
}

fn apply_zoom(file: &mut OpenFile, cmd: ZoomCmd, total: usize, width: f32) {
    let sel = file.selection;
    let v = &mut file.view;
    match cmd {
        ZoomCmd::In => v.zoom(2.0, width / 2.0, total, width),
        ZoomCmd::Out => v.zoom(0.5, width / 2.0, total, width),
        ZoomCmd::Selection if !sel.is_empty() => v.zoom_to_range(sel.range(), total, width),
        ZoomCmd::Selection => {}
        ZoomCmd::Full => v.fit_horizontal(total, width),
        ZoomCmd::AmpIn => v.zoom_amp(2.0),
        ZoomCmd::AmpOut => v.zoom_amp(0.5),
        ZoomCmd::AmpReset => v.amp = 1.0,
    }
}

/// Draw the editor for `file` and handle selection, zoom and scroll.
pub fn show(
    ui: &mut Ui,
    file: &mut OpenFile,
    playhead: f64,
    follow: bool,
    opts: &DisplayOptions,
    zoom_cmd: Option<ZoomCmd>,
) -> WaveformEvents {
    let mut events = WaveformEvents::default();
    let total = file.doc().frames();
    let rate = file.doc().sample_rate;
    let avail = ui.available_size();
    let (outer, _) = ui.allocate_exact_size(avail, Sense::hover());
    if total == 0 || outer.width() < STRIP_W + 40.0 || outer.height() < 80.0 {
        return events;
    }
    let body_left = outer.left() + STRIP_W;
    let width = outer.right() - body_left;
    if file.view.frames_per_px <= 0.0
        || (file.view.frames_per_px * width as f64) > total as f64 * 64.0
    {
        file.view.fit_horizontal(total, width);
    }
    if let Some(cmd) = zoom_cmd {
        apply_zoom(file, cmd, total, width);
    }
    file.view.clamp(total, width);

    let ruler = Rect::from_min_size(pos2(body_left, outer.top()), vec2(width, RULER_H));
    let overview = Rect::from_min_size(
        pos2(body_left, outer.bottom() - OVERVIEW_H),
        vec2(width, OVERVIEW_H),
    );
    let lanes = Rect::from_min_max(
        pos2(body_left, ruler.bottom()),
        pos2(outer.right(), overview.top() - 4.0),
    );
    let strip = Rect::from_min_max(
        pos2(outer.left(), lanes.top()),
        pos2(body_left, lanes.bottom()),
    );

    let resp = ui.interact(lanes, ui.id().with("lanes"), Sense::click_and_drag());
    handle_strip(ui, strip, file);
    handle_input(ui, &resp, file, lanes, total, &mut events);
    if follow {
        file.view.reveal(playhead, total, width);
    }

    let view = file.view;
    let painter = ui.painter_at(outer);
    painter.rect_filled(lanes, 0.0, Color32::from_gray(18));
    let ticks = scale::time_ticks(&view, width, rate, opts.time, LABEL_PX);
    draw_lanes(&painter, lanes, file, &view, opts, &ticks);
    draw_strip(&painter, strip, file, &view, opts);
    draw_ruler(&painter, ruler, &view, &ticks);
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

/// Dragging the amplitude scale zooms vertically; double-click resets.
fn handle_strip(ui: &Ui, strip: Rect, file: &mut OpenFile) {
    let resp = ui.interact(strip, ui.id().with("amp_strip"), Sense::click_and_drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        file.view.zoom_amp((-resp.drag_delta().y * 0.01).exp());
    }
    if resp.double_clicked() {
        file.view.amp = 1.0;
    }
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
    let frame_at =
        |x: f32, v: &View| (v.px_to_frame(x - lanes.left()).round().max(0.0) as usize).min(total);
    let shift = ui.input(|i| i.modifiers.shift);

    if let Some(p) = resp.hover_pos() {
        if edge_drag_anchor(file.selection, &file.view, p.x - lanes.left(), EDGE_GRAB_PX).is_some()
        {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }
    if resp.drag_started() {
        // The pointer has already moved past the drag threshold; the press origin is where the user aimed.
        if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
            let f = frame_at(p.x, &file.view);
            let anchor = if shift {
                extend_anchor(file.selection, f)
            } else {
                edge_drag_anchor(file.selection, &file.view, p.x - lanes.left(), EDGE_GRAB_PX)
                    .unwrap_or(f)
            };
            ui.data_mut(|d| d.insert_temp(resp.id, anchor));
            file.selection = Selection::new(anchor, f);
        }
    }
    if resp.dragged() {
        if let (Some(p), Some(anchor)) = (
            resp.interact_pointer_pos(),
            ui.data(|d| d.get_temp::<usize>(resp.id)),
        ) {
            file.selection = Selection::new(anchor, frame_at(p.x, &file.view));
        }
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = frame_at(p.x, &file.view);
            if shift {
                file.selection = Selection::new(extend_anchor(file.selection, f), f);
            } else {
                file.selection = Selection::cursor(f);
                ev.seek = Some(f);
            }
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
    let (lx, rx) = (
        lanes.left() + v.frame_to_px(sel.start as f64),
        lanes.left() + v.frame_to_px(sel.end as f64),
    );
    let (l, r) = (
        lx.clamp(lanes.left(), lanes.right()),
        rx.clamp(lanes.left(), lanes.right()),
    );
    if r > l {
        p.rect_filled(
            Rect::from_min_max(pos2(l, lanes.top()), pos2(r, lanes.bottom())),
            0.0,
            SELECTION,
        );
    }
    for x in [lx, rx] {
        if x >= lanes.left() && x <= lanes.right() {
            p.line_segment(
                [pos2(x, lanes.top()), pos2(x, lanes.bottom())],
                Stroke::new(1.0, SELECTION_EDGE),
            );
        }
    }
}

fn draw_ruler(p: &egui::Painter, rect: Rect, v: &View, ticks: &[scale::Tick]) {
    p.rect_filled(rect, 0.0, Color32::from_gray(32));
    for t in ticks {
        let x = rect.left() + v.frame_to_px(t.frame);
        if x < rect.left() || x > rect.right() {
            continue;
        }
        let h = if t.major { 9.0 } else { 4.0 };
        p.line_segment(
            [pos2(x, rect.bottom() - h), pos2(x, rect.bottom())],
            Stroke::new(1.0, Color32::GRAY),
        );
        if let Some(label) = &t.label {
            p.text(
                pos2(x + 3.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(10.0),
                Color32::LIGHT_GRAY,
            );
        }
    }
}

fn lane_rect(lanes: Rect, c: usize, n: usize) -> Rect {
    let h = lanes.height() / n as f32;
    Rect::from_min_size(
        pos2(lanes.left(), lanes.top() + c as f32 * h),
        vec2(lanes.width(), h),
    )
}

fn channel_label(c: usize, n: usize) -> String {
    match (n, c) {
        (2, 0) => "L".into(),
        (2, 1) => "R".into(),
        _ => (c + 1).to_string(),
    }
}

/// Amplitude labels and tick marks beside each lane.
fn draw_strip(p: &egui::Painter, strip: Rect, file: &OpenFile, v: &View, opts: &DisplayOptions) {
    p.rect_filled(strip, 0.0, Color32::from_gray(26));
    let doc = file.doc();
    let n = doc.channel_count();
    let font = egui::FontId::proportional(9.5);
    for c in 0..n {
        let lane = lane_rect(strip, c, n);
        let (mid, half) = (lane.center().y, lane.height() * 0.5 - 2.0);
        let clipped = p.with_clip_rect(lane);
        let dim = if file.mask.has(c) { 1.0 } else { 0.4 };
        let text = Color32::from_gray(170).gamma_multiply(dim);
        let put = |y: f32, label: &str| {
            let ly = y.clamp(lane.top() + 6.0, lane.bottom() - 6.0);
            clipped.line_segment(
                [pos2(strip.right() - 4.0, y), pos2(strip.right(), y)],
                Stroke::new(1.0, Color32::GRAY),
            );
            clipped.text(
                pos2(strip.right() - 7.0, ly),
                egui::Align2::RIGHT_CENTER,
                label,
                font.clone(),
                text,
            );
        };
        for g in scale::amp_gridlines(opts.amp, v.amp, half, 18.0) {
            let dy = g.value * v.amp * half;
            put(mid - dy, &g.label);
            put(mid + dy, &g.label);
        }
        put(mid, scale::zero_label(opts.amp));
        if c > 0 {
            p.line_segment(
                [lane.left_top(), lane.right_top()],
                Stroke::new(1.0, Color32::from_gray(60)),
            );
        }
        p.text(
            lane.left_top() + vec2(5.0, 3.0),
            egui::Align2::LEFT_TOP,
            channel_label(c, n),
            egui::FontId::proportional(11.0),
            Color32::from_gray(200).gamma_multiply(dim),
        );
    }
}

fn draw_lanes(
    p: &egui::Painter,
    lanes: Rect,
    file: &OpenFile,
    v: &View,
    opts: &DisplayOptions,
    ticks: &[scale::Tick],
) {
    let doc = file.doc();
    let n = doc.channel_count();
    let cols = lanes.width().floor() as usize;
    let start = v.start.max(0.0) as usize;
    let end = ((v.start + v.visible_frames(lanes.width())).ceil() as usize).min(doc.frames());

    // Vertical gridlines under the waveform follow the ruler's major ticks.
    for t in ticks.iter().filter(|t| t.major) {
        let x = lanes.left() + v.frame_to_px(t.frame);
        if x > lanes.left() && x < lanes.right() {
            p.line_segment(
                [pos2(x, lanes.top()), pos2(x, lanes.bottom())],
                Stroke::new(1.0, Color32::from_gray(30)),
            );
        }
    }

    for (c, rope) in doc.channels.iter().enumerate() {
        let lane = lane_rect(lanes, c, n);
        let p = p.with_clip_rect(lane);
        let mid = lane.center().y;
        let half = lane.height() * 0.5 - 2.0;
        let scale = half * v.amp;
        let dim = if file.mask.has(c) { 1.0 } else { 0.35 };
        for g in scale::amp_gridlines(opts.amp, v.amp, half, 18.0) {
            for y in [mid - g.value * scale, mid + g.value * scale] {
                p.line_segment(
                    [pos2(lane.left(), y), pos2(lane.right(), y)],
                    Stroke::new(1.0, Color32::from_gray(32)),
                );
            }
        }
        p.line_segment(
            [pos2(lane.left(), mid), pos2(lane.right(), mid)],
            Stroke::new(1.0, Color32::from_gray(55)),
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
                        mid - s.clamp(-4.0, 4.0) * scale,
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
        let want = cols.min(((end - start) as f64 / v.frames_per_px).ceil().max(1.0) as usize);
        let peaks = rope.peaks(start..end, want);
        let col_w = (end - start) as f64 / peaks.len() as f64 / v.frames_per_px;
        let x0 = lanes.left() + v.frame_to_px(start as f64);
        for (i, pk) in peaks.iter().enumerate() {
            let x = x0 + (i as f64 * col_w) as f32 + 0.5;
            let (top, bot) = (
                mid - pk.max.clamp(-4.0, 4.0) * scale,
                mid - pk.min.clamp(-4.0, 4.0) * scale,
            );
            let w = col_w.max(1.0) as f32;
            p.line_segment(
                [pos2(x, top), pos2(x, bot.max(top + 1.0))],
                Stroke::new(w, WAVE.gamma_multiply(dim)),
            );
            let r = pk.rms.min(4.0) * scale;
            p.line_segment(
                [pos2(x, mid - r), pos2(x, mid + r)],
                Stroke::new(w, WAVE_RMS.gamma_multiply(dim)),
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
            [pos2(x, mid - hi * half), pos2(x, mid - lo * half + 1.0)],
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
