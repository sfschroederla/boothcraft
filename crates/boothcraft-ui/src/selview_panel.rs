//! The Selection/View panel: start, end and duration of the selection and of the visible range,
//! each editable by typing a position in the current time format.

use crate::ops::OpenFile;
use crate::scale::{self, TimeFormat};
use crate::view::MIN_FPP;
use boothcraft_core::Selection;
use egui::{Color32, RichText, Ui};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    SelStart,
    SelEnd,
    SelDuration,
    ViewStart,
    ViewEnd,
    ViewDuration,
}

/// Apply a typed value (in frames) to one field. The opposite edge stays put where possible.
pub fn apply_field(file: &mut OpenFile, field: Field, frames: f64) {
    let total = file.doc().frames();
    let v = (frames.round().max(0.0) as usize).min(total);
    let sel = file.selection;
    let width = file.view_width_px;
    match field {
        Field::SelStart => file.selection = Selection::new(v, sel.end.max(v)),
        Field::SelEnd => file.selection = Selection::new(sel.start.min(v), v),
        Field::SelDuration => {
            file.selection = Selection::new(sel.start, (sel.start + v).min(total))
        }
        Field::ViewStart => file.view.start = frames,
        Field::ViewEnd if frames > file.view.start && width > 0.0 => {
            file.view.frames_per_px = ((frames - file.view.start) / width as f64).max(MIN_FPP)
        }
        Field::ViewDuration if frames > 0.0 && width > 0.0 => {
            file.view.frames_per_px = (frames / width as f64).max(MIN_FPP)
        }
        Field::ViewEnd | Field::ViewDuration => {}
    }
    match field {
        Field::SelStart | Field::SelEnd | Field::SelDuration => {
            file.reveal_request = Some(file.selection.start)
        }
        _ if width > 0.0 => {
            let max_fpp = (total as f64 / width as f64).max(MIN_FPP);
            file.view.frames_per_px = file.view.frames_per_px.min(max_fpp);
            file.view.clamp(total, width);
        }
        _ => {}
    }
}

#[derive(Default)]
pub struct SelViewPanel {
    editing: Option<(Field, String)>,
}

impl SelViewPanel {
    pub fn show(&mut self, ui: &mut Ui, file: &mut OpenFile, fmt: TimeFormat) {
        let (rate, total) = (file.doc().sample_rate, file.doc().frames());
        let sel = file.selection;
        let view_start = file.view.start.max(0.0);
        let view_end =
            (view_start + file.view.visible_frames(file.view_width_px)).min(total as f64);
        let mut commit: Option<(Field, f64)> = None;
        egui::Grid::new("selgrid")
            .num_columns(4)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label("");
                for h in ["Start", "End", "Duration"] {
                    ui.label(RichText::new(h).weak());
                }
                ui.end_row();
                let rows = [
                    (
                        "Selection",
                        [
                            (Field::SelStart, sel.start as f64),
                            (Field::SelEnd, sel.end as f64),
                            (Field::SelDuration, sel.len() as f64),
                        ],
                    ),
                    (
                        "View",
                        [
                            (Field::ViewStart, view_start),
                            (Field::ViewEnd, view_end),
                            (Field::ViewDuration, view_end - view_start),
                        ],
                    ),
                ];
                for (label, cells) in rows {
                    ui.label(label);
                    for (field, value) in cells {
                        if let Some(c) = self.cell(ui, field, value, rate, fmt) {
                            commit = Some(c);
                        }
                    }
                    ui.end_row();
                }
            });
        if let Some((field, frames)) = commit {
            apply_field(file, field, frames);
        }
    }

    /// One editable cell. Returns the parsed value when the user commits a valid entry.
    fn cell(
        &mut self,
        ui: &mut Ui,
        field: Field,
        value: f64,
        rate: u32,
        fmt: TimeFormat,
    ) -> Option<(Field, f64)> {
        let mut text = match &self.editing {
            Some((f, buf)) if *f == field => buf.clone(),
            _ => scale::format_position(value, rate, fmt),
        };
        let valid = scale::parse_position(&text, rate, fmt).is_some();
        let color = if valid {
            Color32::from_rgb(96, 150, 255)
        } else {
            Color32::from_rgb(230, 90, 80)
        };
        let resp = ui.add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(78.0)
                .text_color(color)
                .frame(egui::Frame::NONE),
        );
        if resp.changed() {
            self.editing = Some((field, text));
        }
        if resp.lost_focus() {
            let edit = self.editing.take_if(|(f, _)| *f == field)?;
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                return None;
            }
            return scale::parse_position(&edit.1, rate, fmt).map(|v| (field, v));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boothcraft_core::{Document, Rope};

    fn file() -> OpenFile {
        let mut f = OpenFile::new(
            "t".into(),
            None,
            Document::new(48_000, vec![Rope::from_samples(&vec![0.0; 48_000])]),
        );
        f.view_width_px = 1000.0;
        f.view.frames_per_px = 48.0;
        f
    }

    #[test]
    fn selection_fields_keep_the_other_edge() {
        let mut f = file();
        f.selection = Selection::new(1000, 2000);
        apply_field(&mut f, Field::SelStart, 500.0);
        assert_eq!(f.selection, Selection::new(500, 2000));
        apply_field(&mut f, Field::SelEnd, 3000.0);
        assert_eq!(f.selection, Selection::new(500, 3000));
        apply_field(&mut f, Field::SelDuration, 100.0);
        assert_eq!(f.selection, Selection::new(500, 600));
        apply_field(&mut f, Field::SelStart, 900.0);
        assert_eq!(
            f.selection,
            Selection::new(900, 900),
            "start past end collapses to a cursor"
        );
        apply_field(&mut f, Field::SelDuration, 1e9);
        assert_eq!(f.selection.end, 48_000, "clamped to the file");
    }

    #[test]
    fn view_fields_zoom_and_scroll() {
        let mut f = file();
        f.view.frames_per_px = 10.0;
        apply_field(&mut f, Field::ViewStart, 2000.0);
        assert_eq!(f.view.start, 2000.0);
        apply_field(&mut f, Field::ViewEnd, 7000.0);
        assert_eq!(f.view.frames_per_px, 5.0);
        apply_field(&mut f, Field::ViewDuration, 1e9);
        assert_eq!(
            f.view.frames_per_px, 48.0,
            "cannot zoom out past the whole file"
        );
        assert_eq!(f.view.start, 0.0);
    }
}
