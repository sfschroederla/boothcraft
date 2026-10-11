use crate::ops::OpenFile;
use crate::scale::{self, AmpUnits, TimeFormat};
use crate::view::{edge_drag_anchor, extend_anchor, View};
use boothcraft_core::Selection;
use egui::{pos2, vec2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, Ui};

const OVERVIEW_H: f32 = 44.0;
const RULER_H: f32 = 22.0;
/// Width of the amplitude scale column to the right of the lanes.
const SCALE_W: f32 = 44.0;
/// Width of the L/R channel button column at the far right.
const CHAN_W: f32 = 26.0;
const EDGE_GRAB_PX: f32 = 6.0;
const LABEL_PX: f32 = 90.0;
/// Ruler ticks closer than this to the left edge are unlabelled, leaving room for the format name.
const CORNER_PX: f32 = 36.0;

const BACKGROUND: Color32 = Color32::from_rgb(0, 0, 0);
const GRID: Color32 = Color32::from_rgb(14, 48, 24);
const WAVE: Color32 = Color32::from_rgb(66, 201, 165);
const WAVE_RMS: Color32 = Color32::from_rgb(44, 150, 125);
const OVERVIEW_WAVE: Color32 = Color32::from_rgb(120, 215, 170);
const PLAYHEAD: Color32 = Color32::from_rgb(90, 150, 255);
const SELECTION: Color32 = Color32::from_rgba_premultiplied(60, 100, 160, 90);
const SELECTION_EDGE: Color32 = Color32::from_rgb(130, 170, 240);
const PANEL_BG: Color32 = Color32::from_rgb(34, 34, 34);

/// Display preferences shared by every open file.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayOptions {
    pub amp: AmpUnits,
    pub time: TimeFormat,
}

/// Zoom actions requested from the transport bar.
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
///
/// Layout, top to bottom: overview bar, time ruler, channel lanes. The dB scale and L/R channel
/// buttons run down the right-hand side.
pub fn show(
    ui: &mut Ui,
    file: &mut OpenFile,
    playhead: f64,
    follow: bool,
    opts: &mut DisplayOptions,
    zoom_cmd: Option<ZoomCmd>,
) -> WaveformEvents {
    let mut events = WaveformEvents::default();
    let total = file.doc().frames();
    let rate = file.doc().sample_rate;
    let avail = ui.available_size();
    let (outer, _) = ui.allocate_exact_size(avail, Sense::hover());
    let right_w = SCALE_W + CHAN_W;
    if total == 0 || outer.width() < right_w + 60.0 || outer.height() < OVERVIEW_H + RULER_H + 40.0
    {
        return events;
    }
    let width = outer.width() - right_w;
    file.view_width_px = width;
    if file.view.frames_per_px <= 0.0
        || (file.view.frames_per_px * width as f64) > total as f64 * 64.0
    {
        file.view.fit_horizontal(total, width);
    }
    if let Some(cmd) = zoom_cmd {
        apply_zoom(file, cmd, total, width);
    }
    if let Some(frame) = file.reveal_request.take() {
        file.view.reveal(frame as f64, total, width);
    }
    file.view.clamp(total, width);

    let overview = Rect::from_min_size(outer.min, vec2(width, OVERVIEW_H));
    let ruler = Rect::from_min_size(
        pos2(outer.left(), overview.bottom() + 4.0),
        vec2(width, RULER_H),
    );
    let lanes = Rect::from_min_max(
        pos2(outer.left(), ruler.bottom()),
        pos2(outer.left() + width, outer.bottom() - 2.0),
    );
    let strip = Rect::from_min_max(
        pos2(lanes.right(), lanes.top()),
        pos2(lanes.right() + SCALE_W, lanes.bottom()),
    );
    let chan_col = Rect::from_min_max(
        pos2(strip.right(), lanes.top()),
        pos2(outer.right(), lanes.bottom()),
    );

    let resp = ui.interact(lanes, ui.id().with("lanes"), Sense::click_and_drag());
    handle_ruler_menu(ui, ruler, opts);
    handle_strip(ui, strip, file, opts);
    handle_channel_buttons(ui, chan_col, file);
    handle_input(ui, &resp, file, lanes, total, &mut events);
    if follow {
        file.view.reveal(playhead, total, width);
    }

    let view = file.view;
    let painter = ui.painter_at(outer);
    painter.rect_filled(lanes, 0.0, BACKGROUND);
    let ticks = scale::time_ticks(&view, width, rate, opts.time, LABEL_PX);
    draw_lanes(&painter, lanes, file, &view, opts, &ticks);
    draw_strip(&painter, strip, file, &view, opts);
    draw_channel_buttons(&painter, chan_col, file);
    draw_ruler(&painter, ruler, &view, &ticks, opts);
    painter.text(
        pos2(strip.left() + 6.0, ruler.bottom() - 3.0),
        egui::Align2::LEFT_BOTTOM,
        opts.amp.short(),
        egui::FontId::proportional(10.0),
        Color32::LIGHT_GRAY,
    );
    draw_selection(&painter, lanes, file.selection, &view);
    let x = lanes.left() + view.frame_to_px(playhead);
    if x >= lanes.left() && x <= lanes.right() {
        painter.line_segment(
            [pos2(x, ruler.bottom() - 6.0), pos2(x, lanes.bottom())],
            Stroke::new(1.0, PLAYHEAD),
        );
        painter.add(Shape::convex_polygon(
            vec![
                pos2(x - 5.0, ruler.bottom() - 9.0),
                pos2(x + 5.0, ruler.bottom() - 9.0),
                pos2(x, ruler.bottom() - 2.0),
            ],
            PLAYHEAD,
            Stroke::NONE,
        ));
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

/// Right-clicking the ruler picks the time format.
fn handle_ruler_menu(ui: &Ui, ruler: Rect, opts: &mut DisplayOptions) {
    let resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click());
    resp.context_menu(|ui| {
        for f in TimeFormat::ALL {
            if ui.radio_value(&mut opts.time, f, f.name()).clicked() {
                ui.close();
            }
        }
    });
}

/// Dragging the amplitude scale zooms vertically; double-click resets; right-click picks the units.
fn handle_strip(ui: &Ui, strip: Rect, file: &mut OpenFile, opts: &mut DisplayOptions) {
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
    resp.context_menu(|ui| {
        for u in AmpUnits::ALL {
            if ui.radio_value(&mut opts.amp, u, u.name()).clicked() {
                ui.close();
            }
        }
    });
}

fn channel_button_rect(col: Rect, lane: Rect) -> Rect {
    Rect::from_center_size(pos2(col.center().x, lane.center().y), vec2(18.0, 16.0))
}

/// Clicking a channel's L/R button enables or disables edits on it (at least one stays enabled).
fn handle_channel_buttons(ui: &Ui, col: Rect, file: &mut OpenFile) {
    let n = file.doc().channel_count();
    for c in 0..n.min(8) {
        let r = channel_button_rect(col, lane_rect(col, c, n));
        let resp = ui.interact(r, ui.id().with(("chan", c)), Sense::click());
        if resp.clicked() {
            let toggled = file.mask.0 ^ (1 << c);
            if (0..n).any(|i| toggled & (1 << i) != 0) {
                file.mask.0 = toggled;
            }
        }
        resp.on_hover_text(if file.mask.has(c) {
            "Channel enabled for editing (click to disable)"
        } else {
            "Channel disabled (click to enable)"
        });
    }
}

fn draw_channel_buttons(p: &egui::Painter, col: Rect, file: &OpenFile) {
    let n = file.doc().channel_count();
    for c in 0..n.min(8) {
        let r = channel_button_rect(col, lane_rect(col, c, n));
        let on = file.mask.has(c);
        if on {
            p.rect_filled(r, 2.0, Color32::from_rgb(52, 72, 118));
        }
        p.rect_stroke(
            r,
            2.0,
            Stroke::new(1.0, Color32::from_gray(if on { 120 } else { 70 })),
            egui::StrokeKind::Inside,
        );
        p.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            channel_label(c, n),
            egui::FontId::proportional(10.0),
            Color32::from_gray(if on { 225 } else { 120 }),
        );
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

fn draw_ruler(
    p: &egui::Painter,
    rect: Rect,
    v: &View,
    ticks: &[scale::Tick],
    opts: &DisplayOptions,
) {
    p.rect_filled(rect, 0.0, PANEL_BG);
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
            if x - rect.left() >= CORNER_PX {
                p.text(
                    pos2(x + 3.0, rect.top() + 3.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::proportional(10.0),
                    Color32::LIGHT_GRAY,
                );
            }
        }
    }
    p.text(
        pos2(rect.left() + 3.0, rect.top() + 3.0),
        egui::Align2::LEFT_TOP,
        opts.time.short(),
        egui::FontId::proportional(10.0),
        Color32::from_rgb(140, 170, 230),
    );
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

/// Amplitude labels and tick marks beside each lane (left-aligned, just right of the waveform).
fn draw_strip(p: &egui::Painter, strip: Rect, file: &OpenFile, v: &View, opts: &DisplayOptions) {
    p.rect_filled(strip, 0.0, PANEL_BG);
    let n = file.doc().channel_count();
    let font = egui::FontId::proportional(9.5);
    for c in 0..n {
        let lane = lane_rect(strip, c, n);
        let (mid, half) = (lane.center().y, lane.height() * 0.5 - 2.0);
        let clipped = p.with_clip_rect(lane);
        let dim = if file.mask.has(c) { 1.0 } else { 0.5 };
        let text = Color32::from_gray(190).gamma_multiply(dim);
        let put = |y: f32, label: &str| {
            let ly = y.clamp(lane.top() + 6.0, lane.bottom() - 6.0);
            clipped.line_segment(
                [pos2(strip.left(), y), pos2(strip.left() + 4.0, y)],
                Stroke::new(1.0, Color32::GRAY),
            );
            clipped.text(
                pos2(strip.left() + 7.0, ly),
                egui::Align2::LEFT_CENTER,
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
                Stroke::new(1.0, GRID),
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
                    Stroke::new(1.0, GRID),
                );
            }
        }
        p.line_segment(
            [pos2(lane.left(), mid), pos2(lane.right(), mid)],
            Stroke::new(1.0, Color32::from_rgb(26, 80, 44)),
        );
        if c > 0 {
            p.line_segment(
                [lane.left_top(), lane.right_top()],
                Stroke::new(1.0, Color32::from_gray(70)),
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

/// Whole-file overview above the ruler, one band per channel; drag or click to move the view.
fn draw_overview(ui: &mut Ui, p: &egui::Painter, rect: Rect, file: &mut OpenFile, playhead: f64) {
    let doc = file.doc();
    let total = doc.frames();
    let n = doc.channel_count();
    p.rect_filled(rect, 2.0, Color32::from_rgb(16, 16, 16));
    let cols = rect.width().floor().max(1.0) as usize;
    let inner = rect.shrink2(vec2(0.0, 2.0));
    for (c, rope) in doc.channels.iter().enumerate() {
        let band = Rect::from_min_size(
            pos2(
                inner.left(),
                inner.top() + c as f32 * inner.height() / n as f32,
            ),
            vec2(inner.width(), inner.height() / n as f32),
        );
        let (mid, half) = (band.center().y, band.height() * 0.5 - 1.0);
        for (i, pk) in rope.peaks(0..total, cols).iter().enumerate() {
            let x = rect.left() + i as f32 + 0.5;
            p.line_segment(
                [
                    pos2(x, mid - pk.max.clamp(-1.0, 1.0) * half),
                    pos2(x, mid - pk.min.clamp(-1.0, 1.0) * half + 1.0),
                ],
                Stroke::new(1.0, OVERVIEW_WAVE),
            );
        }
    }
    let v = file.view;
    let px_per_frame = rect.width() as f64 / total as f64;
    let vis_l = rect.left() + (v.start * px_per_frame) as f32;
    let vis_w = (v.visible_frames(rect.width()) * px_per_frame).min(rect.width() as f64) as f32;
    let window = Rect::from_min_size(pos2(vis_l, rect.top()), vec2(vis_w.max(3.0), rect.height()));
    p.rect_stroke(
        window,
        2.0,
        Stroke::new(1.5, Color32::from_rgb(190, 195, 210)),
        egui::StrokeKind::Inside,
    );
    let px = rect.left() + (playhead * px_per_frame) as f32;
    p.line_segment(
        [pos2(px, rect.top()), pos2(px, rect.bottom())],
        Stroke::new(1.0, PLAYHEAD),
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
