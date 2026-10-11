use crate::ops::OpenFile;
use crate::scale::{self, AmpUnits, TimeFormat};
use crate::view::{self, edge_drag_anchor, extend_anchor, View};
use boothcraft_core::{fade_gain, FadeShape, Selection, ZeroDir};
use egui::{pos2, vec2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, Ui};

const OVERVIEW_H: f32 = 44.0;
const RULER_H: f32 = 34.0;
/// Height of the marker band at the top of the ruler.
const MARKER_BAND_H: f32 = 12.0;
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
const CUE_MARKER: Color32 = Color32::from_rgb(235, 235, 235);
const RANGE_MARKER: Color32 = Color32::from_rgb(190, 150, 255);

/// Display preferences shared by every open file.
#[derive(Clone, Copy, Debug)]
pub struct DisplayOptions {
    pub amp: AmpUnits,
    pub time: TimeFormat,
    pub snap: SnapOptions,
    pub tempo_bpm: f32,
    pub beats_per_bar: u32,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        DisplayOptions {
            amp: AmpUnits::default(),
            time: TimeFormat::default(),
            snap: SnapOptions::default(),
            tempo_bpm: 120.0,
            beats_per_bar: 4,
        }
    }
}

/// What selection and marker drags snap to (Edit > Snapping).
#[derive(Clone, Copy, Debug)]
pub struct SnapOptions {
    pub enabled: bool,
    pub markers: bool,
    pub ruler: bool,
    pub zero_crossings: bool,
}

impl Default for SnapOptions {
    fn default() -> Self {
        SnapOptions {
            enabled: true,
            markers: true,
            ruler: false,
            zero_crossings: false,
        }
    }
}

/// Snap distance in pixels.
const SNAP_PX: f32 = 8.0;

/// Converts pointer positions to frames, applying the active snapping rules.
struct Snapper {
    candidates: Vec<f64>,
    zero_crossings: bool,
    left: f32,
    total: usize,
}

impl Snapper {
    /// `exclude` leaves one marker out, so a marker being dragged does not snap to itself.
    fn new(
        file: &OpenFile,
        opts: &DisplayOptions,
        ticks: &[scale::Tick],
        left: f32,
        exclude: Option<usize>,
    ) -> Snapper {
        let snap = opts.snap;
        let mut candidates = Vec::new();
        if snap.enabled && snap.markers {
            for (i, m) in file.doc().markers.iter().enumerate() {
                if Some(i) != exclude {
                    candidates.push(m.at as f64);
                    candidates.push(m.end() as f64);
                }
            }
        }
        if snap.enabled && snap.ruler {
            candidates.extend(ticks.iter().map(|t| t.frame));
        }
        Snapper {
            candidates,
            zero_crossings: snap.enabled && snap.zero_crossings,
            left,
            total: file.doc().frames(),
        }
    }

    fn frame(&self, file: &OpenFile, x: f32) -> usize {
        let v = &file.view;
        let raw = v.px_to_frame(x - self.left).max(0.0).min(self.total as f64);
        let tol = SNAP_PX as f64 * v.frames_per_px;
        let snapped = view::snap(raw, &self.candidates, tol);
        let f = (snapped.round() as usize).min(self.total);
        if snapped == raw && self.zero_crossings {
            return file
                .doc()
                .zero_crossing(f, ZeroDir::Nearest, file.mask, tol.ceil() as usize)
                .unwrap_or(f);
        }
        f
    }
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
    /// Gain in dB being previewed with the HUD knob (while dragging), applied to playback.
    pub preview_gain_db: Option<f32>,
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
    let input_ticks = scale::time_ticks(&file.view, width, rate, opts.time, LABEL_PX);
    handle_ruler_menu(ui, ruler, opts);
    let marker_drag = handle_markers(ui, ruler, file, opts, &input_ticks, &mut events);
    handle_strip(ui, strip, file, opts);
    handle_channel_buttons(ui, chan_col, file);
    let snapper = Snapper::new(file, opts, &input_ticks, lanes.left(), None);
    handle_input(ui, &resp, file, lanes, total, &snapper, &mut events);
    // Registered after the lanes so they sit on top of them for hit-testing.
    let fade_drag = handle_fades(ui, lanes, file);
    let hud_db = handle_hud(ui, lanes, file);
    events.preview_gain_db = (hud_db != 0.0).then_some(hud_db);
    if follow {
        file.view.reveal(playhead, total, width);
    }

    let view = file.view;
    let painter = ui.painter_at(outer);
    painter.rect_filled(lanes, 0.0, BACKGROUND);
    let ticks = scale::time_ticks(&view, width, rate, opts.time, LABEL_PX);
    draw_lanes(
        &painter,
        lanes,
        file,
        &view,
        opts,
        &ticks,
        10f32.powf(hud_db / 20.0),
    );
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
    draw_markers(&painter, ruler, lanes, file, &view, marker_drag);
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
    draw_fades(&painter, lanes, file, &view, fade_drag);
    draw_hud(&painter, lanes, hud_db);
    draw_overview(ui, &painter, overview, file, playhead);
    events
}

enum MarkerAction {
    Move(usize, usize),
    Select(usize),
    Delete(usize),
}

fn marker_flag_rect(ruler: Rect, x: f32) -> Rect {
    Rect::from_min_size(
        pos2(x - 1.0, ruler.top() + 1.0),
        vec2(8.0, MARKER_BAND_H - 2.0),
    )
}

/// Marker flags: click to jump (ranges select their span), drag to move, right-click to delete.
/// Returns the marker being dragged and its provisional position, for drawing.
fn handle_markers(
    ui: &Ui,
    ruler: Rect,
    file: &mut OpenFile,
    opts: &DisplayOptions,
    ticks: &[scale::Tick],
    ev: &mut WaveformEvents,
) -> Option<(usize, usize)> {
    let drag_id = ui.id().with("marker_drag");
    let view = file.view;
    let mut action = None;
    for (i, m) in file.doc().markers.iter().enumerate() {
        let x = ruler.left() + view.frame_to_px(m.at as f64);
        if x < ruler.left() - 8.0 || x > ruler.right() + 8.0 {
            continue;
        }
        let resp = ui.interact(
            marker_flag_rect(ruler, x).expand(3.0),
            ui.id().with(("marker", i)),
            Sense::click_and_drag(),
        );
        if resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            if let Some(p) = resp.interact_pointer_pos() {
                let f = Snapper::new(file, opts, ticks, ruler.left(), Some(i)).frame(file, p.x);
                ui.data_mut(|d| d.insert_temp(drag_id, (i, f)));
            }
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if resp.drag_stopped() {
            if let Some((j, f)) = ui.data(|d| d.get_temp::<(usize, usize)>(drag_id)) {
                action = Some(MarkerAction::Move(j, f));
            }
            ui.data_mut(|d| d.remove::<(usize, usize)>(drag_id));
        } else if resp.clicked() {
            action = Some(MarkerAction::Select(i));
        }
        resp.context_menu(|ui| {
            if ui.button("Delete marker").clicked() {
                action = Some(MarkerAction::Delete(i));
                ui.close();
            }
        });
        resp.on_hover_text(&m.name);
    }
    // Apply after the loop: every marker edit can reorder the list.
    match action {
        Some(MarkerAction::Move(i, f)) => file.move_marker(i, f),
        Some(MarkerAction::Select(i)) => ev.seek = file.select_marker(i),
        Some(MarkerAction::Delete(i)) => file.remove_marker(i),
        None => {}
    }
    ui.data(|d| d.get_temp::<(usize, usize)>(drag_id))
}

fn dashed_vline(p: &egui::Painter, x: f32, top: f32, bottom: f32, color: Color32) {
    let mut y = top;
    while y < bottom {
        p.line_segment(
            [pos2(x, y), pos2(x, (y + 4.0).min(bottom))],
            Stroke::new(1.0, color),
        );
        y += 8.0;
    }
}

fn draw_markers(
    p: &egui::Painter,
    ruler: Rect,
    lanes: Rect,
    file: &OpenFile,
    v: &View,
    drag: Option<(usize, usize)>,
) {
    let font = egui::FontId::proportional(9.5);
    for (i, m) in file.doc().markers.iter().enumerate() {
        let at = match drag {
            Some((j, f)) if j == i => f,
            _ => m.at,
        };
        let len = m.end() - m.at;
        let color = if m.is_range() {
            RANGE_MARKER
        } else {
            CUE_MARKER
        };
        let x = lanes.left() + v.frame_to_px(at as f64);
        let x2 = lanes.left() + v.frame_to_px((at + len) as f64);
        if x2 < lanes.left() - 8.0 || x > lanes.right() + 8.0 {
            continue;
        }
        if m.is_range() {
            let band = |top: f32, bottom: f32| {
                Rect::from_min_max(
                    pos2(x.max(lanes.left()), top),
                    pos2(x2.min(lanes.right()), bottom),
                )
            };
            p.rect_filled(
                band(lanes.top(), lanes.bottom()),
                0.0,
                color.gamma_multiply(0.08),
            );
            p.rect_filled(
                band(ruler.top(), ruler.top() + MARKER_BAND_H),
                0.0,
                color.gamma_multiply(0.3),
            );
            dashed_vline(
                p,
                x2,
                lanes.top(),
                lanes.bottom(),
                color.gamma_multiply(0.6),
            );
        }
        dashed_vline(p, x, lanes.top(), lanes.bottom(), color.gamma_multiply(0.6));
        p.line_segment(
            [pos2(x, ruler.top()), pos2(x, ruler.bottom())],
            Stroke::new(1.0, color),
        );
        p.rect_filled(marker_flag_rect(ruler, x), 1.0, color);
        p.text(
            pos2(x + 9.0, ruler.top() + MARKER_BAND_H / 2.0),
            egui::Align2::LEFT_CENTER,
            &m.name,
            font.clone(),
            color,
        );
    }
}

const FADE_HANDLE: f32 = 10.0;
const FADE_COLOR: Color32 = Color32::from_rgb(250, 210, 90);

/// A fade being dragged: which end, its length in frames, and its shape.
#[derive(Clone, Copy, Debug)]
struct FadeDrag {
    fade_in: bool,
    len: usize,
    shape: FadeShape,
}

fn fade_handle_rect(lanes: Rect, x: f32) -> Rect {
    let x = x.clamp(
        lanes.left() + FADE_HANDLE / 2.0 + 1.0,
        lanes.right() - FADE_HANDLE / 2.0 - 1.0,
    );
    Rect::from_center_size(
        pos2(x, lanes.top() + FADE_HANDLE / 2.0 + 3.0),
        vec2(FADE_HANDLE, FADE_HANDLE),
    )
}

/// The fade-in and fade-out handles sit at the file's start and end. Drag horizontally for length,
/// vertically for curve (up: logarithmic, down: exponential), hold Cmd/Ctrl for a cosine curve.
/// The fade is applied when the drag ends.
fn handle_fades(ui: &Ui, lanes: Rect, file: &mut OpenFile) -> Option<FadeDrag> {
    let drag_id = ui.id().with("fade_drag");
    let total = file.doc().frames();
    let v = file.view;
    let current: Option<FadeDrag> = ui.data(|d| d.get_temp(drag_id));
    for fade_in in [true, false] {
        let len = current
            .filter(|d| d.fade_in == fade_in)
            .map_or(0, |d| d.len);
        let frame = if fade_in { len } else { total - len.min(total) };
        let x = lanes.left() + v.frame_to_px(frame as f64);
        if x < lanes.left() - FADE_HANDLE || x > lanes.right() + FADE_HANDLE {
            continue;
        }
        let resp = ui.interact(
            fade_handle_rect(lanes, x),
            ui.id().with(("fade", fade_in)),
            Sense::drag(),
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            if let (Some(p), Some(origin)) = (
                resp.interact_pointer_pos(),
                ui.input(|i| i.pointer.press_origin()),
            ) {
                let f = (v.px_to_frame(p.x - lanes.left()).round().max(0.0) as usize).min(total);
                let len = if fade_in { f } else { total - f };
                let c = ((origin.y - p.y) / (lanes.height() / 2.0)).clamp(-1.0, 1.0);
                let shape = if ui.input(|i| i.modifiers.command) {
                    FadeShape::Cosine
                } else {
                    FadeShape::Power(c)
                };
                ui.data_mut(|d| {
                    d.insert_temp(
                        drag_id,
                        FadeDrag {
                            fade_in,
                            len,
                            shape,
                        },
                    )
                });
            }
        }
        if resp.drag_stopped() {
            if let Some(d) = ui.data(|d| d.get_temp::<FadeDrag>(drag_id)) {
                file.fade(d.fade_in, d.len, d.shape);
            }
            ui.data_mut(|d| d.remove::<FadeDrag>(drag_id));
        }
        resp.on_hover_text(if fade_in {
            "Fade In: drag right for length, up/down for curve, Cmd for cosine"
        } else {
            "Fade Out: drag left for length, up/down for curve, Cmd for cosine"
        });
    }
    ui.data(|d| d.get_temp(drag_id))
}

fn draw_fades(p: &egui::Painter, lanes: Rect, file: &OpenFile, v: &View, drag: Option<FadeDrag>) {
    let total = file.doc().frames();
    if let Some(d) = drag.filter(|d| d.len > 0) {
        let (from, to) = if d.fade_in {
            (0, d.len)
        } else {
            (total - d.len, total)
        };
        let (x0, x1) = (
            lanes.left() + v.frame_to_px(from as f64),
            lanes.left() + v.frame_to_px(to as f64),
        );
        let steps = ((x1 - x0).abs() as usize).clamp(2, 400);
        let pts: Vec<Pos2> = (0..=steps)
            .map(|k| {
                let t = k as f32 / steps as f32;
                let g = fade_gain(if d.fade_in { t } else { 1.0 - t }, d.shape);
                pos2(x0 + (x1 - x0) * t, lanes.bottom() - g * lanes.height())
            })
            .collect();
        let clipped = p.with_clip_rect(lanes);
        clipped.add(Shape::line(pts, Stroke::new(1.5, FADE_COLOR)));
    }
    for fade_in in [true, false] {
        let len = drag.filter(|d| d.fade_in == fade_in).map_or(0, |d| d.len);
        let frame = if fade_in { len } else { total - len.min(total) };
        let x = lanes.left() + v.frame_to_px(frame as f64);
        if x < lanes.left() - FADE_HANDLE || x > lanes.right() + FADE_HANDLE {
            continue;
        }
        let r = fade_handle_rect(lanes, x);
        p.rect_filled(r, 1.0, Color32::from_gray(70));
        p.rect_stroke(
            r,
            1.0,
            Stroke::new(1.0, Color32::from_gray(200)),
            egui::StrokeKind::Inside,
        );
    }
}

const HUD_RANGE_DB: f32 = 40.0;

fn hud_rect(lanes: Rect) -> Rect {
    Rect::from_min_size(lanes.left_top() + vec2(14.0, 24.0), vec2(96.0, 24.0))
}

/// The floating gain knob: drag (right/up raises) to preview, release to apply Amplify.
/// Returns the gain in dB currently being previewed (0 when idle).
fn handle_hud(ui: &Ui, lanes: Rect, file: &mut OpenFile) -> f32 {
    let id = ui.id().with("hud_db");
    let knob = Rect::from_center_size(
        pos2(hud_rect(lanes).left() + 14.0, hud_rect(lanes).center().y),
        vec2(20.0, 20.0),
    );
    let resp = ui.interact(knob, ui.id().with("hud_knob"), Sense::drag());
    let mut db: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        db = (db + (d.x - d.y) * 0.1).clamp(-HUD_RANGE_DB, HUD_RANGE_DB);
        ui.data_mut(|m| m.insert_temp(id, db));
    }
    if resp.drag_stopped() {
        let rounded = (db * 10.0).round() / 10.0;
        if rounded != 0.0 {
            file.amplify(rounded);
        }
        db = 0.0;
        ui.data_mut(|m| m.remove::<f32>(id));
    }
    resp.on_hover_text(
        "Adjust amplitude: drag to preview, release to apply to the selection (or whole file)",
    );
    db
}

fn draw_hud(p: &egui::Painter, lanes: Rect, db: f32) {
    let r = hud_rect(lanes);
    p.rect_filled(r, 4.0, Color32::from_black_alpha(170));
    p.rect_stroke(
        r,
        4.0,
        Stroke::new(1.0, Color32::from_gray(80)),
        egui::StrokeKind::Inside,
    );
    let c = pos2(r.left() + 14.0, r.center().y);
    p.circle_stroke(c, 8.0, Stroke::new(1.5, Color32::from_gray(200)));
    // -40 dB points to 7 o'clock, +40 dB to 5 o'clock.
    let angle = (db / HUD_RANGE_DB) * 135f32.to_radians() - std::f32::consts::FRAC_PI_2;
    p.line_segment(
        [c, c + vec2(angle.cos(), angle.sin()) * 7.0],
        Stroke::new(1.5, Color32::WHITE),
    );
    let color = if db == 0.0 {
        Color32::from_rgb(140, 170, 230)
    } else {
        FADE_COLOR
    };
    p.text(
        pos2(r.left() + 28.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{db:+.1} dB"),
        egui::FontId::proportional(11.0),
        color,
    );
}

/// Right-clicking the ruler picks the time format.
fn handle_ruler_menu(ui: &Ui, ruler: Rect, opts: &mut DisplayOptions) {
    let resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click());
    resp.context_menu(|ui| {
        for f in TimeFormat::ALL {
            if ui.radio(opts.time.same_kind(f), f.name()).clicked() {
                opts.time = match f {
                    TimeFormat::BarsBeats { .. } => {
                        TimeFormat::bars_beats(opts.tempo_bpm, opts.beats_per_bar)
                    }
                    other => other,
                };
                ui.close();
            }
        }
        if let TimeFormat::BarsBeats { .. } = opts.time {
            ui.separator();
            let tempo = ui
                .horizontal(|ui| {
                    ui.label("Tempo");
                    ui.add(
                        egui::DragValue::new(&mut opts.tempo_bpm)
                            .range(20.0..=300.0)
                            .speed(0.5)
                            .suffix(" BPM"),
                    )
                    .changed()
                })
                .inner;
            let beats = ui
                .horizontal(|ui| {
                    ui.label("Beats per bar");
                    ui.add(egui::DragValue::new(&mut opts.beats_per_bar).range(1..=16))
                        .changed()
                })
                .inner;
            if tempo || beats {
                opts.time = TimeFormat::bars_beats(opts.tempo_bpm, opts.beats_per_bar);
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
    snapper: &Snapper,
    ev: &mut WaveformEvents,
) {
    let width = lanes.width();
    let frame_at = |x: f32, file: &OpenFile| snapper.frame(file, x);
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
            let f = frame_at(p.x, file);
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
            file.selection = Selection::new(anchor, frame_at(p.x, file));
        }
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = frame_at(p.x, file);
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
                    pos2(x + 3.0, rect.top() + MARKER_BAND_H + 2.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::proportional(10.0),
                    Color32::LIGHT_GRAY,
                );
            }
        }
    }
    p.text(
        pos2(rect.left() + 3.0, rect.top() + MARKER_BAND_H + 2.0),
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
    preview_gain: f32,
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
        let scale = half * v.amp * preview_gain;
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
