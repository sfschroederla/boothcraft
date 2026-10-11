//! Generate > Tones / Noise / DTMF dialogs.

use boothcraft_dsp::generate::{self, NoiseColor, Waveform};
use egui::Ui;

pub enum Generator {
    Tone {
        wave: Waveform,
        start: f32,
        end: f32,
        sweep: bool,
        amp_db: f32,
        secs: f64,
    },
    Noise {
        color: NoiseColor,
        amp_db: f32,
        secs: f64,
    },
    Dtmf {
        digits: String,
        tone_ms: f32,
        gap_ms: f32,
        amp_db: f32,
    },
}

impl Generator {
    pub fn tone() -> Generator {
        Generator::Tone {
            wave: Waveform::Sine,
            start: 440.0,
            end: 880.0,
            sweep: false,
            amp_db: -6.0,
            secs: 5.0,
        }
    }

    pub fn noise() -> Generator {
        Generator::Noise {
            color: NoiseColor::Pink,
            amp_db: -12.0,
            secs: 5.0,
        }
    }

    pub fn dtmf() -> Generator {
        Generator::Dtmf {
            digits: "5551234".into(),
            tone_ms: 100.0,
            gap_ms: 80.0,
            amp_db: -6.0,
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Generator::Tone { .. } => "Generate Tones",
            Generator::Noise { .. } => "Generate Noise",
            Generator::Dtmf { .. } => "Generate DTMF Tones",
        }
    }

    /// The audio this generator produces at `rate`, with a history label.
    pub fn render(&self, rate: u32) -> (String, Vec<f32>) {
        match self {
            Generator::Tone {
                wave,
                start,
                end,
                sweep,
                amp_db,
                secs,
            } => {
                let end = if *sweep { *end } else { *start };
                (
                    "Generate Tones".into(),
                    generate::tone(*wave, *start, end, *amp_db, *secs, rate),
                )
            }
            Generator::Noise {
                color,
                amp_db,
                secs,
            } => (
                "Generate Noise".into(),
                generate::noise(*color, *amp_db, *secs, rate, 0x2545_F491),
            ),
            Generator::Dtmf {
                digits,
                tone_ms,
                gap_ms,
                amp_db,
            } => (
                "Generate DTMF".into(),
                generate::dtmf(digits, *tone_ms, *gap_ms, *amp_db, rate),
            ),
        }
    }

    fn fields(&mut self, ui: &mut Ui) {
        egui::Grid::new("gen_fields")
            .num_columns(2)
            .show(ui, |ui| match self {
                Generator::Tone {
                    wave,
                    start,
                    end,
                    sweep,
                    amp_db,
                    secs,
                } => {
                    ui.label("Waveform");
                    egui::ComboBox::from_id_salt("gen_wave")
                        .selected_text(wave.name())
                        .show_ui(ui, |ui| {
                            for w in Waveform::ALL {
                                ui.selectable_value(wave, w, w.name());
                            }
                        });
                    ui.end_row();
                    ui.label(if *sweep {
                        "Start frequency"
                    } else {
                        "Frequency"
                    });
                    ui.add(
                        egui::DragValue::new(start)
                            .range(1.0..=24_000.0)
                            .speed(1.0)
                            .suffix(" Hz"),
                    );
                    ui.end_row();
                    ui.label("Sweep");
                    ui.checkbox(sweep, "");
                    ui.end_row();
                    if *sweep {
                        ui.label("End frequency");
                        ui.add(
                            egui::DragValue::new(end)
                                .range(1.0..=24_000.0)
                                .speed(1.0)
                                .suffix(" Hz"),
                        );
                        ui.end_row();
                    }
                    ui.label("Amplitude");
                    ui.add(
                        egui::DragValue::new(amp_db)
                            .range(-96.0..=0.0)
                            .speed(0.1)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Duration");
                    ui.add(
                        egui::DragValue::new(secs)
                            .range(0.01..=3600.0)
                            .speed(0.05)
                            .suffix(" s"),
                    );
                    ui.end_row();
                }
                Generator::Noise {
                    color,
                    amp_db,
                    secs,
                } => {
                    ui.label("Color");
                    egui::ComboBox::from_id_salt("gen_color")
                        .selected_text(color.name())
                        .show_ui(ui, |ui| {
                            for c in NoiseColor::ALL {
                                ui.selectable_value(color, c, c.name());
                            }
                        });
                    ui.end_row();
                    ui.label("Peak level");
                    ui.add(
                        egui::DragValue::new(amp_db)
                            .range(-96.0..=0.0)
                            .speed(0.1)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                    ui.label("Duration");
                    ui.add(
                        egui::DragValue::new(secs)
                            .range(0.01..=3600.0)
                            .speed(0.05)
                            .suffix(" s"),
                    );
                    ui.end_row();
                }
                Generator::Dtmf {
                    digits,
                    tone_ms,
                    gap_ms,
                    amp_db,
                } => {
                    ui.label("Dial string");
                    ui.text_edit_singleline(digits);
                    ui.end_row();
                    ui.label("Tone length");
                    ui.add(
                        egui::DragValue::new(tone_ms)
                            .range(10.0..=2000.0)
                            .speed(1.0)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    ui.label("Break length");
                    ui.add(
                        egui::DragValue::new(gap_ms)
                            .range(0.0..=2000.0)
                            .speed(1.0)
                            .suffix(" ms"),
                    );
                    ui.end_row();
                    ui.label("Amplitude");
                    ui.add(
                        egui::DragValue::new(amp_db)
                            .range(-96.0..=0.0)
                            .speed(0.1)
                            .suffix(" dB"),
                    );
                    ui.end_row();
                }
            });
    }

    /// Draw the dialog. Returns `Some(true)` on OK, `Some(false)` on Cancel.
    pub fn show(&mut self, ctx: &egui::Context, target: &str) -> Option<bool> {
        let mut result = None;
        egui::Window::new(self.title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                self.fields(ui);
                ui.label(egui::RichText::new(target).weak().small());
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        result = Some(false);
                    }
                });
            });
        result
    }
}
