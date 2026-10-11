//! The Markers panel: a table of markers with inline rename.

use crate::ops::OpenFile;
use crate::scale::{self, TimeFormat};
use egui::{Color32, RichText, Ui};

/// Per-panel UI state: the row being renamed and its edit buffer.
#[derive(Default)]
pub struct MarkersPanel {
    editing: Option<(usize, String)>,
    selected: Option<usize>,
}

/// What the panel asks the app to do after drawing.
pub enum MarkerAction {
    Seek(usize),
}

impl MarkersPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        file: &mut OpenFile,
        fmt: TimeFormat,
    ) -> Option<MarkerAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            if ui
                .button("Add")
                .on_hover_text("Add a marker at the cursor, or a range over the selection (M)")
                .clicked()
            {
                file.add_marker();
            }
            let can_delete = self.selected.is_some_and(|i| i < file.doc().markers.len());
            if ui
                .add_enabled(can_delete, egui::Button::new("Delete"))
                .clicked()
            {
                if let Some(i) = self.selected.take() {
                    file.remove_marker(i);
                }
            }
        });
        ui.separator();
        let rate = file.doc().sample_rate;
        let blue = Color32::from_rgb(96, 150, 255);
        let markers = file.doc().markers.clone();
        if markers.is_empty() {
            ui.label(RichText::new("No markers. Press M to add one at the cursor.").weak());
            return None;
        }
        let mut rename: Option<(usize, String)> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("markers_grid")
                .num_columns(5)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in ["Name", "Start", "End", "Duration", "Type"] {
                        ui.label(RichText::new(h).weak());
                    }
                    ui.end_row();
                    for (i, m) in markers.iter().enumerate() {
                        // Show the in-progress edit for this row, otherwise the stored name.
                        let mut text = match &self.editing {
                            Some((row, buf)) if *row == i => buf.clone(),
                            _ => m.name.clone(),
                        };
                        let resp =
                            ui.add(egui::TextEdit::singleline(&mut text).desired_width(120.0));
                        if resp.changed() {
                            self.editing = Some((i, text));
                        }
                        if resp.gained_focus() {
                            self.selected = Some(i);
                        }
                        if resp.lost_focus() {
                            if let Some(edit) = self.editing.take_if(|(row, _)| *row == i) {
                                rename = Some(edit);
                            }
                        }
                        let pos = |f: usize| {
                            RichText::new(scale::format_position(f as f64, rate, fmt)).color(blue)
                        };
                        let start = ui.selectable_label(self.selected == Some(i), pos(m.at));
                        ui.label(pos(m.end()));
                        ui.label(pos(m.end() - m.at));
                        ui.label(if m.is_range() { "Range" } else { "Cue" });
                        ui.end_row();
                        if start.clicked() {
                            self.selected = Some(i);
                            if let Some(at) = file.select_marker(i) {
                                action = Some(MarkerAction::Seek(at));
                            }
                        }
                    }
                });
        });
        if let Some((i, name)) = rename {
            file.rename_marker(i, name.trim());
        }
        action
    }
}
