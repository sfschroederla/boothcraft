//! Generic parameter editors, driven by each effect's `ParamSpec`s.

use boothcraft_dsp::{Curve, ParamSpec};
use egui::Ui;

/// Draw one row per parameter. Returns `(index, value)` for every parameter the user changed.
pub fn edit_params(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    specs: &[ParamSpec],
    values: &mut [f32],
) -> Vec<(usize, f32)> {
    let mut changed = Vec::new();
    egui::Grid::new(id_salt)
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            for (i, (spec, v)) in specs.iter().zip(values.iter_mut()).enumerate() {
                ui.label(spec.name);
                let before = *v;
                if spec.choices == ["Off", "On"] {
                    let mut on = *v >= 0.5;
                    if ui.checkbox(&mut on, "").changed() {
                        *v = on as u8 as f32;
                    }
                } else if !spec.choices.is_empty() {
                    let mut idx = v.round() as usize;
                    egui::ComboBox::from_id_salt((spec.id, i))
                        .selected_text(spec.format(*v))
                        .show_ui(ui, |ui| {
                            for (k, c) in spec.choices.iter().enumerate() {
                                ui.selectable_value(&mut idx, k, *c);
                            }
                        });
                    *v = idx as f32;
                } else {
                    ui.add(
                        egui::Slider::new(v, spec.min..=spec.max)
                            .logarithmic(spec.curve == Curve::Log)
                            .suffix(spec.unit.suffix())
                            .clamping(egui::SliderClamping::Always),
                    );
                }
                if *v != before {
                    changed.push((i, *v));
                }
                ui.end_row();
            }
        });
    changed
}

/// Breakpoint editor for Fade/Gain Envelope. Click empty space to add a point, drag to move,
/// double-click (or right-click) a point to remove it; the end points only move vertically.
/// Returns true when the curve changed.
pub fn envelope_editor(
    ui: &mut Ui,
    env: &mut boothcraft_dsp::Envelope,
    smooth: bool,
    db: bool,
) -> bool {
    use egui::{pos2, vec2, Color32, Rect, Sense, Shape, Stroke};
    let size = vec2(ui.available_width().max(360.0), 170.0);
    let (rect, bg) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter_at(rect);
    let (min, max) = (env.min, env.max);
    let to_screen = |t: f32, v: f32| {
        pos2(
            rect.left() + t * rect.width(),
            rect.bottom() - (v - min) / (max - min) * rect.height(),
        )
    };
    let from_screen = |q: egui::Pos2| {
        let t = ((q.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let v = (min + (rect.bottom() - q.y) / rect.height() * (max - min)).clamp(min, max);
        (t, v)
    };
    p.rect_filled(rect, 3.0, Color32::from_gray(16));
    let grid = Stroke::new(1.0, Color32::from_gray(40));
    let font = egui::FontId::proportional(9.5);
    let marks: Vec<f32> = if db {
        vec![max, 0.0, -12.0, -24.0, min]
    } else {
        vec![1.0, 0.75, 0.5, 0.25, 0.0]
    };
    for m in marks {
        let y = to_screen(0.0, m).y;
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            if db && m == 0.0 {
                Stroke::new(1.0, Color32::from_gray(80))
            } else {
                grid
            },
        );
        let label = if db {
            format!("{m:+.0} dB")
        } else {
            format!("{:.0}%", m * 100.0)
        };
        p.text(
            pos2(
                rect.left() + 3.0,
                y.clamp(rect.top() + 6.0, rect.bottom() - 6.0),
            ),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            Color32::GRAY,
        );
    }
    let curve: Vec<egui::Pos2> = (0..=240)
        .map(|k| {
            let t = k as f32 / 240.0;
            to_screen(t, env.eval(t, smooth))
        })
        .collect();
    p.add(Shape::line(
        curve,
        Stroke::new(1.5, Color32::from_rgb(250, 210, 90)),
    ));

    let mut changed = false;
    let mut remove = None;
    let last = env.points.len() - 1;
    for i in 0..env.points.len() {
        let (t, v) = env.points[i];
        let c = to_screen(t, v);
        let resp = ui.interact(
            Rect::from_center_size(c, vec2(12.0, 12.0)),
            bg.id.with(("pt", i)),
            Sense::click_and_drag(),
        );
        if resp.dragged() {
            if let Some(q) = resp.interact_pointer_pos() {
                let (nt, nv) = from_screen(q);
                let t = if i == 0 {
                    0.0
                } else if i == last {
                    1.0
                } else {
                    nt.clamp(env.points[i - 1].0 + 1e-4, env.points[i + 1].0 - 1e-4)
                };
                env.points[i] = (t, nv);
                changed = true;
            }
        }
        if (resp.double_clicked() || resp.secondary_clicked()) && i != 0 && i != last {
            remove = Some(i);
        }
        let hot = resp.hovered() || resp.dragged();
        p.circle_filled(c, if hot { 5.0 } else { 4.0 }, Color32::WHITE);
        let shown = if db {
            format!("{v:+.1} dB")
        } else {
            format!("{:.0}%", v * 100.0)
        };
        resp.on_hover_text(shown);
    }
    if let Some(i) = remove {
        env.points.remove(i);
        changed = true;
    } else if bg.clicked() {
        if let Some(q) = bg.interact_pointer_pos() {
            let (t, _) = from_screen(q);
            let v = env.eval(t, smooth);
            env.points.push((t, v));
            changed = true;
        }
    }
    if changed {
        env.normalize();
    }
    changed
}

/// The editor for an effect: a custom one where it helps (EQs), else the generic parameter list.
pub fn effect_editor(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug + Copy,
    effect_id: &str,
    specs: &[ParamSpec],
    values: &mut [f32],
) -> Vec<(usize, f32)> {
    match effect_id {
        "parametric_eq" => {
            let mut changed = parametric_eq_graph(ui, values);
            egui::CollapsingHeader::new("Band settings")
                .id_salt((id_salt, "bands"))
                .show(ui, |ui| {
                    changed.extend(edit_params(ui, id_salt, specs, values));
                });
            changed
        }
        id if id.starts_with("graphic_eq") => graphic_eq_sliders(ui, specs, values),
        _ => edit_params(ui, id_salt, specs, values),
    }
}

const GRAPH_RATE: f32 = 48_000.0;
const GRAPH_DB: f32 = 24.0;

fn freq_to_x(rect: egui::Rect, f: f32) -> f32 {
    rect.left() + (f / 20.0).log10() / 3.0 * rect.width()
}

fn x_to_freq(rect: egui::Rect, x: f32) -> f32 {
    20.0 * 10f32.powf(((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * 3.0)
}

/// A draggable band on the EQ graph: parameter indices for on, frequency, gain and Q, plus a label.
type EqNode = (usize, usize, Option<usize>, Option<usize>, String);

/// Frequency-response graph with draggable band nodes.
fn parametric_eq_graph(ui: &mut Ui, values: &mut [f32]) -> Vec<(usize, f32)> {
    use boothcraft_dsp::effects::parametric_eq::{idx, response_db, PEAKS};
    use egui::{pos2, vec2, Color32, Rect, Sense, Shape, Stroke};
    let size = vec2(ui.available_width().max(420.0), 190.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    let db_y = |db: f32| rect.center().y - db / GRAPH_DB * rect.height() / 2.0;
    p.rect_filled(rect, 3.0, Color32::from_gray(14));
    let font = egui::FontId::proportional(9.5);
    for f in [
        20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0,
    ] {
        let x = freq_to_x(rect, f);
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, Color32::from_gray(34)),
        );
        let label = if f >= 1000.0 {
            format!("{}k", f / 1000.0)
        } else {
            format!("{f}")
        };
        p.text(
            pos2(x + 2.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            font.clone(),
            Color32::GRAY,
        );
    }
    for db in [-18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0] {
        let y = db_y(db);
        let c = if db == 0.0 {
            Color32::from_gray(70)
        } else {
            Color32::from_gray(34)
        };
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, c),
        );
        p.text(
            pos2(rect.left() + 2.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{db:+.0}"),
            font.clone(),
            Color32::GRAY,
        );
    }
    let curve: Vec<egui::Pos2> = (0..=300)
        .map(|k| {
            let f = 20.0 * 10f32.powf(k as f32 / 300.0 * 3.0);
            pos2(
                freq_to_x(rect, f),
                db_y(response_db(values, GRAPH_RATE, f).clamp(-GRAPH_DB, GRAPH_DB)),
            )
        })
        .collect();
    p.add(Shape::line(
        curve,
        Stroke::new(2.0, Color32::from_rgb(90, 200, 160)),
    ));

    let mut nodes: Vec<EqNode> = vec![
        (idx::HP_ON, idx::HP_FREQ, None, None, "HP".into()),
        (
            idx::LS_ON,
            idx::LS_FREQ,
            Some(idx::LS_GAIN),
            None,
            "L".into(),
        ),
    ];
    for k in 0..PEAKS {
        let b = idx::PEAK + 4 * k;
        nodes.push((b, b + 1, Some(b + 2), Some(b + 3), format!("{}", k + 1)));
    }
    nodes.push((
        idx::HS_ON,
        idx::HS_FREQ,
        Some(idx::HS_GAIN),
        None,
        "H".into(),
    ));
    nodes.push((idx::LP_ON, idx::LP_FREQ, None, None, "LP".into()));

    let mut changed = Vec::new();
    for (on, f_i, g_i, q_i, label) in nodes {
        if values[on] < 0.5 {
            continue;
        }
        let gain = g_i.map_or(0.0, |g| values[g]);
        let c = pos2(freq_to_x(rect, values[f_i]), db_y(gain));
        let resp = ui.interact(
            Rect::from_center_size(c, vec2(16.0, 16.0)),
            ui.id().with(("eq_node", f_i)),
            Sense::drag(),
        );
        if resp.dragged() {
            if let Some(q) = resp.interact_pointer_pos() {
                values[f_i] = x_to_freq(rect, q.x).clamp(20.0, 20_000.0);
                changed.push((f_i, values[f_i]));
                if let Some(g) = g_i {
                    values[g] = ((rect.center().y - q.y) / (rect.height() / 2.0) * GRAPH_DB)
                        .clamp(-GRAPH_DB, GRAPH_DB);
                    changed.push((g, values[g]));
                }
            }
        }
        if let Some(qi) = q_i {
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    values[qi] = (values[qi] * (1.0 + scroll * 0.01)).clamp(0.1, 16.0);
                    changed.push((qi, values[qi]));
                }
            }
        }
        let hot = resp.hovered() || resp.dragged();
        p.circle_filled(
            c,
            if hot { 8.0 } else { 7.0 },
            Color32::from_rgb(40, 70, 120),
        );
        p.circle_stroke(
            c,
            if hot { 8.0 } else { 7.0 },
            Stroke::new(1.5, Color32::WHITE),
        );
        p.text(
            c,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(9.0),
            Color32::WHITE,
        );
        let tip = match (g_i, q_i) {
            (Some(g), Some(q)) => format!(
                "{:.0} Hz, {:+.1} dB, Q {:.2} (scroll for Q)",
                values[f_i], values[g], values[q]
            ),
            (Some(g), None) => format!("{:.0} Hz, {:+.1} dB", values[f_i], values[g]),
            _ => format!("{:.0} Hz", values[f_i]),
        };
        resp.on_hover_text(tip);
    }
    changed
}

/// Graphic EQ: one vertical slider per band, then master gain.
fn graphic_eq_sliders(ui: &mut Ui, specs: &[ParamSpec], values: &mut [f32]) -> Vec<(usize, f32)> {
    let mut changed = Vec::new();
    let bands = specs.len() - 1;
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal(|ui| {
            for i in 0..bands {
                ui.vertical(|ui| {
                    ui.set_width(30.0);
                    let before = values[i];
                    ui.add(
                        egui::Slider::new(&mut values[i], -20.0..=20.0)
                            .vertical()
                            .show_value(false),
                    );
                    ui.label(egui::RichText::new(format!("{:+.0}", values[i])).small());
                    ui.label(
                        egui::RichText::new(specs[i].name.replace(" Hz", "").replace(" kHz", "k"))
                            .small()
                            .weak(),
                    );
                    if values[i] != before {
                        changed.push((i, values[i]));
                    }
                    if ui.small_button("0").on_hover_text("Reset band").clicked() {
                        values[i] = 0.0;
                        changed.push((i, 0.0));
                    }
                });
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("Master Gain");
        let before = values[bands];
        ui.add(egui::Slider::new(&mut values[bands], -20.0..=20.0).suffix(" dB"));
        if values[bands] != before {
            changed.push((bands, values[bands]));
        }
    });
    changed
}
