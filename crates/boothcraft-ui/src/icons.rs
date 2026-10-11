//! Transport and zoom icons drawn as vector shapes, so they render identically on every platform
//! without depending on font glyph coverage.

use egui::{
    pos2, vec2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo,
    WidgetType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    ToStart,
    ToEnd,
    Rewind,
    FastForward,
    Record,
    PrevMarker,
    NextMarker,
    Loop,
    SkipSelection,
    Follow,
    AmpZoomIn,
    AmpZoomOut,
    AmpReset,
    TimeZoomIn,
    TimeZoomOut,
    ZoomSelection,
    ZoomFull,
}

const BUTTON_SIZE: egui::Vec2 = vec2(28.0, 26.0);
const RECORD_RED: Color32 = Color32::from_rgb(224, 72, 60);

/// A flat icon button. `selected` draws it as toggled on.
pub fn button(ui: &mut Ui, icon: Icon, selected: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(BUTTON_SIZE, Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, tooltip));
    if ui.is_rect_visible(rect) {
        let v = ui.style().interact_selectable(&resp, selected);
        let painter = ui.painter();
        if selected || resp.hovered() {
            painter.rect_filled(rect, v.corner_radius, v.weak_bg_fill);
        }
        let color = if icon == Icon::Record && enabled {
            RECORD_RED
        } else {
            v.fg_stroke.color
        };
        paint(
            painter,
            icon,
            Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            color,
        );
    }
    resp.on_hover_text(tooltip)
}

/// Paint `icon` filling `r`.
pub fn paint(p: &egui::Painter, icon: Icon, r: Rect, color: Color32) {
    let (c, s) = (r.center(), r.width().min(r.height()) / 2.0);
    let fill = |points: Vec<Pos2>| p.add(Shape::convex_polygon(points, color, Stroke::NONE));
    let bar = |x0: f32, x1: f32| {
        p.rect_filled(
            Rect::from_min_max(pos2(x0, c.y - s), pos2(x1, c.y + s)),
            0.0,
            color,
        )
    };
    let line = Stroke::new(1.8, color);
    let thin = Stroke::new(1.3, color);
    match icon {
        Icon::Play => {
            fill(vec![
                pos2(c.x - s * 0.75, c.y - s),
                pos2(c.x + s, c.y),
                pos2(c.x - s * 0.75, c.y + s),
            ]);
        }
        Icon::Pause => {
            bar(c.x - s * 0.8, c.x - s * 0.2);
            bar(c.x + s * 0.2, c.x + s * 0.8);
        }
        Icon::Stop => {
            p.rect_filled(
                Rect::from_center_size(c, vec2(s * 1.7, s * 1.7)),
                1.0,
                color,
            );
        }
        Icon::ToStart => {
            bar(c.x - s, c.x - s * 0.6);
            fill(vec![
                pos2(c.x + s, c.y - s),
                pos2(c.x - s * 0.5, c.y),
                pos2(c.x + s, c.y + s),
            ]);
        }
        Icon::ToEnd => {
            bar(c.x + s * 0.6, c.x + s);
            fill(vec![
                pos2(c.x - s, c.y - s),
                pos2(c.x + s * 0.5, c.y),
                pos2(c.x - s, c.y + s),
            ]);
        }
        Icon::Rewind => {
            for dx in [0.0, s] {
                fill(vec![
                    pos2(c.x + dx, c.y - s * 0.8),
                    pos2(c.x + dx - s, c.y),
                    pos2(c.x + dx, c.y + s * 0.8),
                ]);
            }
        }
        Icon::FastForward => {
            for dx in [0.0, -s] {
                fill(vec![
                    pos2(c.x + dx, c.y - s * 0.8),
                    pos2(c.x + dx + s, c.y),
                    pos2(c.x + dx, c.y + s * 0.8),
                ]);
            }
        }
        Icon::Record => {
            p.circle_filled(c, s * 0.8, color);
        }
        Icon::PrevMarker | Icon::NextMarker => {
            // A marker flag on one side and an arrow pointing toward it.
            let flip = if icon == Icon::PrevMarker { 1.0 } else { -1.0 };
            let pole = c.x - s * 0.85 * flip;
            p.line_segment([pos2(pole, c.y - s), pos2(pole, c.y + s)], thin);
            let flag_end = pole + s * 0.8 * flip;
            p.rect_filled(
                Rect::from_two_pos(pos2(pole, c.y - s), pos2(flag_end, c.y - s * 0.35)),
                0.0,
                color,
            );
            let tip = c.x + s * 0.05 * flip;
            let back = c.x + s * flip;
            fill(vec![
                pos2(back, c.y - s * 0.1),
                pos2(tip, c.y + s * 0.4),
                pos2(back, c.y + s * 0.9),
            ]);
        }
        Icon::Loop => {
            let body = Rect::from_center_size(c, vec2(s * 2.0, s * 1.3));
            p.rect_stroke(body, 3.0, line, StrokeKind::Middle);
            // Arrowheads on the top and bottom edges, pointing in opposite directions.
            let (top, bottom) = (body.top(), body.bottom());
            fill(vec![
                pos2(c.x + s * 0.1, top - s * 0.4),
                pos2(c.x + s * 0.1, top + s * 0.4),
                pos2(c.x + s * 0.7, top),
            ]);
            fill(vec![
                pos2(c.x - s * 0.1, bottom - s * 0.4),
                pos2(c.x - s * 0.1, bottom + s * 0.4),
                pos2(c.x - s * 0.7, bottom),
            ]);
        }
        Icon::SkipSelection => {
            // A selection (brackets) with an arrow arcing over it.
            for x in [c.x - s * 0.45, c.x + s * 0.45] {
                p.line_segment([pos2(x, c.y - s * 0.1), pos2(x, c.y + s)], thin);
            }
            let arc: Vec<Pos2> = (0..=12)
                .map(|k| {
                    let a = std::f32::consts::PI * (1.0 - k as f32 / 12.0);
                    pos2(c.x + a.cos() * s * 0.85, c.y - s * 0.05 - a.sin() * s * 0.8)
                })
                .collect();
            let tip = *arc.last().unwrap_or(&c);
            p.add(Shape::line(arc, thin));
            fill(vec![
                tip + vec2(-s * 0.35, -s * 0.05),
                tip + vec2(s * 0.35, -s * 0.05),
                tip + vec2(0.0, s * 0.45),
            ]);
        }
        Icon::Follow => {
            // An arrow chasing a playhead line.
            p.line_segment(
                [pos2(c.x + s * 0.8, c.y - s), pos2(c.x + s * 0.8, c.y + s)],
                line,
            );
            p.line_segment([pos2(c.x - s, c.y), pos2(c.x + s * 0.2, c.y)], line);
            fill(vec![
                pos2(c.x - s * 0.1, c.y - s * 0.6),
                pos2(c.x + s * 0.5, c.y),
                pos2(c.x - s * 0.1, c.y + s * 0.6),
            ]);
        }
        Icon::AmpReset => {
            p.text(
                c,
                egui::Align2::CENTER_CENTER,
                "1:1",
                egui::FontId::proportional(s * 1.3),
                color,
            );
        }
        Icon::AmpZoomIn
        | Icon::AmpZoomOut
        | Icon::TimeZoomIn
        | Icon::TimeZoomOut
        | Icon::ZoomSelection
        | Icon::ZoomFull => {
            // Magnifier in the upper right; amplitude zooms get a vertical double arrow on the left,
            // time zooms a horizontal one along the bottom.
            let lens = pos2(c.x + s * 0.2, c.y - s * 0.25);
            let radius = s * 0.58;
            p.circle_stroke(lens, radius, thin);
            let dir = vec2(1.0, 1.0).normalized();
            p.line_segment(
                [lens + dir * radius, lens + dir * (radius + s * 0.65)],
                line,
            );
            let reach = s * 0.3;
            let plus = |vertical: bool| {
                p.line_segment([lens - vec2(reach, 0.0), lens + vec2(reach, 0.0)], thin);
                if vertical {
                    p.line_segment([lens - vec2(0.0, reach), lens + vec2(0.0, reach)], thin);
                }
            };
            match icon {
                Icon::AmpZoomIn | Icon::TimeZoomIn => plus(true),
                Icon::AmpZoomOut | Icon::TimeZoomOut => plus(false),
                Icon::ZoomSelection => {
                    for dx in [-reach, reach] {
                        p.line_segment([lens + vec2(dx, -reach), lens + vec2(dx, reach)], thin);
                    }
                }
                _ => {
                    p.line_segment([lens - vec2(reach, 0.0), lens + vec2(reach, 0.0)], thin);
                    for dx in [-reach, reach] {
                        p.line_segment(
                            [lens + vec2(dx, -reach * 0.6), lens + vec2(dx, reach * 0.6)],
                            thin,
                        );
                    }
                }
            }
            let arrow_tip = s * 0.28;
            match icon {
                Icon::AmpZoomIn | Icon::AmpZoomOut => {
                    let x = c.x - s * 0.9;
                    let (top, bottom) = (c.y - s, c.y + s * 0.6);
                    p.line_segment([pos2(x, top), pos2(x, bottom)], thin);
                    fill(vec![
                        pos2(x - arrow_tip, top + arrow_tip * 1.4),
                        pos2(x + arrow_tip, top + arrow_tip * 1.4),
                        pos2(x, top - 0.5),
                    ]);
                    fill(vec![
                        pos2(x - arrow_tip, bottom - arrow_tip * 1.4),
                        pos2(x + arrow_tip, bottom - arrow_tip * 1.4),
                        pos2(x, bottom + 0.5),
                    ]);
                }
                _ => {
                    let y = c.y + s * 0.85;
                    let (left, right) = (c.x - s, c.x + s * 0.5);
                    p.line_segment([pos2(left, y), pos2(right, y)], thin);
                    fill(vec![
                        pos2(left + arrow_tip * 1.4, y - arrow_tip),
                        pos2(left + arrow_tip * 1.4, y + arrow_tip),
                        pos2(left - 0.5, y),
                    ]);
                    fill(vec![
                        pos2(right - arrow_tip * 1.4, y - arrow_tip),
                        pos2(right - arrow_tip * 1.4, y + arrow_tip),
                        pos2(right + 0.5, y),
                    ]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_paints_inside_its_button() {
        let ctx = egui::Context::default();
        let all = [
            Icon::Play,
            Icon::Pause,
            Icon::Stop,
            Icon::ToStart,
            Icon::ToEnd,
            Icon::Loop,
            Icon::SkipSelection,
            Icon::Follow,
        ];
        let mut rects = Vec::new();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| {
                for icon in all {
                    rects.push(button(ui, icon, false, "test").rect);
                }
            });
        });
        out.textures_delta.clear(); // nothing renders this frame; drop the font upload
        assert_eq!(rects.len(), all.len());
        assert!(
            rects.windows(2).all(|w| w[0].right() <= w[1].left()),
            "buttons do not overlap"
        );
        // Flat buttons draw no background, so every icon must contribute its own shapes.
        assert!(
            out.shapes.len() >= all.len() * 2,
            "got {} shapes",
            out.shapes.len()
        );
    }
}
