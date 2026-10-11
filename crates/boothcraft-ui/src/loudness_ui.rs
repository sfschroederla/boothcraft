//! The Loudness meter panel and the Match Loudness panel.

use crate::ops::OpenFile;
use boothcraft_dsp::loudness::{measure, LiveMeter, Measurement};
use boothcraft_engine::Tap;
use egui::{RichText, Ui};
use std::collections::HashMap;

fn lufs(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.1}")
    } else {
        "—".into()
    }
}

/// Live momentary / short-term / integrated loudness of the output.
pub struct LoudnessPanel {
    tap: Option<Tap>,
    meter: Option<LiveMeter>,
    scratch: Vec<f32>,
}

impl LoudnessPanel {
    pub fn new(tap: Option<Tap>) -> LoudnessPanel {
        let meter = tap
            .as_ref()
            .map(|t| LiveMeter::new(t.channels, t.sample_rate));
        LoudnessPanel {
            tap,
            meter,
            scratch: Vec::new(),
        }
    }

    /// Feed newly played audio into the meter. Call every frame, even when the panel is hidden,
    /// so the tap never fills up.
    pub fn tick(&mut self) {
        if let (Some(tap), Some(meter)) = (&mut self.tap, &mut self.meter) {
            self.scratch.clear();
            tap.drain(&mut self.scratch);
            meter.push(&self.scratch);
        }
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let Some(meter) = &mut self.meter else {
            ui.label(RichText::new("No audio output device").weak());
            return;
        };
        let r = meter.reading();
        egui::Grid::new("loudness")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (name, v, unit) in [
                    ("Momentary", r.momentary, "LUFS"),
                    ("Short-term", r.short_term, "LUFS"),
                    ("Integrated", r.integrated, "LUFS"),
                    ("True peak", r.true_peak_db, "dBTP"),
                ] {
                    ui.label(name);
                    ui.label(
                        RichText::new(format!("{} {unit}", lufs(v)))
                            .size(16.0)
                            .color(egui::Color32::from_rgb(96, 150, 255)),
                    );
                    ui.end_row();
                }
            });
        if ui
            .button("Reset")
            .on_hover_text("Start integrating from now")
            .clicked()
        {
            meter.reset();
        }
        ui.label(
            RichText::new("Measures whatever is playing (ITU-R BS.1770).")
                .weak()
                .small(),
        );
    }
}

const TARGETS: [(&str, f64); 4] = [
    ("EBU R128 (-23 LUFS)", -23.0),
    ("ATSC A/85 (-24 LUFS)", -24.0),
    ("Podcast (-16 LUFS)", -16.0),
    ("Streaming (-14 LUFS)", -14.0),
];

/// Measure open files and bring the chosen ones to a loudness target.
pub struct MatchLoudnessPanel {
    target: f64,
    limit: bool,
    ceiling: f32,
    chosen: HashMap<usize, bool>,
    /// Measurements keyed by file index, valid for the recorded revision.
    measured: HashMap<usize, (u64, Measurement)>,
}

impl Default for MatchLoudnessPanel {
    fn default() -> Self {
        MatchLoudnessPanel {
            target: -16.0,
            limit: true,
            ceiling: -1.0,
            chosen: HashMap::new(),
            measured: HashMap::new(),
        }
    }
}

/// What the panel asks the app to do.
pub struct MatchRequest {
    pub files: Vec<usize>,
    pub target: f64,
    pub ceiling: Option<f32>,
}

fn whole(file: &OpenFile) -> Vec<Vec<f32>> {
    let n = file.doc().frames();
    file.doc().channels.iter().map(|c| c.to_vec(0..n)).collect()
}

impl MatchLoudnessPanel {
    pub fn show(&mut self, ui: &mut Ui, files: &[OpenFile]) -> Option<MatchRequest> {
        let mut request = None;
        ui.horizontal(|ui| {
            ui.label("Target");
            let current = TARGETS
                .iter()
                .find(|(_, v)| *v == self.target)
                .map_or("Custom", |(n, _)| n);
            egui::ComboBox::from_id_salt("ml_target")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (name, v) in TARGETS {
                        ui.selectable_value(&mut self.target, v, name);
                    }
                });
            ui.add(
                egui::DragValue::new(&mut self.target)
                    .range(-60.0..=0.0)
                    .speed(0.1)
                    .suffix(" LUFS"),
            );
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.limit, "Limit peaks to");
            ui.add_enabled(
                self.limit,
                egui::DragValue::new(&mut self.ceiling)
                    .range(-20.0..=0.0)
                    .speed(0.1)
                    .suffix(" dB"),
            );
        });
        ui.separator();
        if files.is_empty() {
            ui.label(RichText::new("Open files to measure them.").weak());
            return None;
        }
        egui::Grid::new("ml_files")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new("File").weak());
                ui.label(RichText::new("Loudness").weak());
                ui.label(RichText::new("True peak").weak());
                ui.end_row();
                for (i, f) in files.iter().enumerate() {
                    let on = self.chosen.entry(i).or_insert(true);
                    ui.checkbox(on, "");
                    ui.label(&f.name);
                    match self.measured.get(&i).filter(|(rev, _)| *rev == f.revision) {
                        Some((_, m)) => {
                            ui.label(format!("{} LUFS", lufs(m.integrated)));
                            ui.label(format!("{} dBTP", lufs(m.true_peak_db)));
                        }
                        None => {
                            ui.label(RichText::new("not measured").weak());
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
        let picked: Vec<usize> = (0..files.len())
            .filter(|i| self.chosen.get(i).copied().unwrap_or(true))
            .collect();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!picked.is_empty(), egui::Button::new("Measure"))
                .clicked()
            {
                for &i in &picked {
                    let f = &files[i];
                    self.measured
                        .insert(i, (f.revision, measure(&whole(f), f.doc().sample_rate)));
                }
            }
            if ui
                .add_enabled(!picked.is_empty(), egui::Button::new("Run"))
                .on_hover_text("Bring each checked file to the target (one undo step per file)")
                .clicked()
            {
                request = Some(MatchRequest {
                    files: picked.clone(),
                    target: self.target,
                    ceiling: self.limit.then_some(self.ceiling),
                });
            }
        });
        request
    }
}
