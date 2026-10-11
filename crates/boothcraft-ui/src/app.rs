use crate::icons::{self, Icon};
use crate::meters::Meters;
use crate::ops::OpenFile;
use crate::view::format_time;
use crate::waveform;
use boothcraft_core::{Clipboards, Selection};
use boothcraft_engine::{Engine, Shared};
use boothcraft_io::{BitDepth, Format, WriteOptions};
use egui::{Event, Key, KeyboardShortcut, Modifiers};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Editor,
    Files,
    History,
    Levels,
    Selection,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Editor => "Editor",
            Tab::Files => "Files",
            Tab::History => "History",
            Tab::Levels => "Levels",
            Tab::Selection => "Selection/View",
        }
    }
}

enum Dialog {
    InsertSilence(f64),
    Amplify(f32),
}

struct State {
    files: Vec<OpenFile>,
    active: usize,
    /// Held to keep the output stream alive.
    _engine: Option<Engine>,
    shared: Option<Arc<Shared>>,
    synced: Option<(usize, u64)>,
    clips: Clipboards,
    meters: Meters,
    status: String,
    follow: bool,
    looping: bool,
    dialog: Option<Dialog>,
    display: waveform::DisplayOptions,
}

pub struct BoothcraftApp {
    dock: DockState<Tab>,
    state: State,
}

impl BoothcraftApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> BoothcraftApp {
        let mut dock = DockState::new(vec![Tab::Editor]);
        let surface = dock.main_surface_mut();
        let [_main, left] =
            surface.split_left(NodeIndex::root(), 0.18, vec![Tab::Files, Tab::History]);
        surface.split_below(left, 0.55, vec![Tab::Selection]);
        surface.split_below(NodeIndex::root(), 0.82, vec![Tab::Levels]);

        let (engine, status) = match Engine::new() {
            Ok(e) => {
                let s = format!("Audio: {} @ {} Hz", e.device_name, e.sample_rate);
                (Some(e), s)
            }
            Err(e) => (None, format!("No audio output ({e}); editing still works")),
        };
        let shared = engine.as_ref().map(Engine::shared);
        let mut state = State {
            files: Vec::new(),
            active: 0,
            _engine: engine,
            shared,
            synced: None,
            clips: Clipboards::default(),
            meters: Meters::default(),
            status,
            follow: true,
            looping: false,
            dialog: None,
            display: waveform::DisplayOptions::default(),
        };
        for p in paths {
            state.open(&p);
        }
        BoothcraftApp { dock, state }
    }
}

impl State {
    fn open(&mut self, path: &Path) {
        match boothcraft_io::read(path) {
            Ok(doc) => {
                let name = path
                    .file_name()
                    .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
                self.status = format!(
                    "Opened {name}: {} ch, {} Hz, {}",
                    doc.channel_count(),
                    doc.sample_rate,
                    format_time(doc.frames() as f64, doc.sample_rate)
                );
                self.files
                    .push(OpenFile::new(name, Some(path.to_path_buf()), doc));
                self.set_active(self.files.len() - 1);
            }
            Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
        }
    }

    fn set_active(&mut self, i: usize) {
        if i != self.active {
            if let Some(s) = &self.shared {
                s.pause();
            }
        }
        self.active = i;
        self.synced = None;
    }

    fn file(&mut self) -> Option<&mut OpenFile> {
        self.files.get_mut(self.active)
    }

    fn pick_and_open(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Audio",
                &["wav", "aif", "aiff", "flac", "mp3", "ogg", "m4a", "aac"],
            )
            .pick_files()
        {
            for p in paths {
                self.open(&p);
            }
        }
    }

    fn save(&mut self, as_new: bool) {
        let Some(f) = self.files.get(self.active) else {
            return;
        };
        let existing = f
            .path
            .clone()
            .filter(|p| boothcraft_io::format_for_path(p).is_some());
        let path = match (as_new, existing) {
            (false, Some(p)) => p,
            _ => {
                let Some(p) = rfd::FileDialog::new()
                    .set_file_name(format!(
                        "{}.wav",
                        f.name.rsplit_once('.').map_or(f.name.as_str(), |x| x.0)
                    ))
                    .add_filter("WAV", &["wav"])
                    .add_filter("AIFF", &["aiff", "aif"])
                    .add_filter("FLAC", &["flac"])
                    .save_file()
                else {
                    return;
                };
                p
            }
        };
        let Some(format) = boothcraft_io::format_for_path(&path) else {
            self.status = "Save as .wav, .aiff or .flac".into();
            return;
        };
        let opts = WriteOptions {
            format,
            depth: if format == Format::Wav {
                BitDepth::Float32
            } else {
                BitDepth::Int24
            },
        };
        let f = &mut self.files[self.active];
        match boothcraft_io::write(f.doc(), &path, &opts) {
            Ok(()) => {
                f.saved_at = f.history.cursor();
                f.name = path
                    .file_name()
                    .map_or_else(|| f.name.clone(), |n| n.to_string_lossy().into_owned());
                f.path = Some(path.clone());
                self.status = format!("Saved {}", path.display());
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// Push the active document to the audio engine when it has changed.
    fn sync_engine(&mut self) {
        let (Some(shared), Some(f)) = (&self.shared, self.files.get(self.active)) else {
            return;
        };
        if self.synced != Some((self.active, f.revision)) {
            shared.set_document(Arc::new(f.doc().clone()));
            self.synced = Some((self.active, f.revision));
        }
        if self.looping {
            let sel = f.selection;
            shared.set_loop(true, sel.start, if sel.is_empty() { 0 } else { sel.end });
        } else {
            shared.set_loop(false, 0, 0);
        }
    }

    fn toggle_play(&mut self) {
        let (Some(shared), Some(f)) = (self.shared.clone(), self.files.get(self.active)) else {
            return;
        };
        if shared.is_playing() {
            shared.pause();
        } else {
            let sel = f.selection;
            let frames = f.doc().frames();
            let pos = shared.position() as usize;
            // Start at the cursor/selection start, unless resuming mid-file after a pause.
            if !sel.is_empty() && !(sel.start..sel.end).contains(&pos) || pos >= frames {
                shared.seek(sel.start.min(frames.saturating_sub(1)));
            } else if sel.is_empty()
                && pos != sel.start
                && !self.looping
                && shared.position() == 0.0
            {
                shared.seek(sel.start);
            }
            shared.play();
        }
    }

    fn stop(&mut self) {
        if let Some(s) = &self.shared {
            s.stop();
        }
    }

    fn playhead(&self) -> f64 {
        self.shared.as_ref().map_or(0.0, |s| s.position())
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        let cmd = Modifiers::COMMAND;
        let keys = |m: Modifiers, k: Key| {
            ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
        };
        if keys(cmd, Key::O) {
            self.pick_and_open();
        }
        if keys(cmd | Modifiers::SHIFT, Key::S) {
            self.save(true);
        } else if keys(cmd, Key::S) {
            self.save(false);
        }
        if keys(cmd | Modifiers::SHIFT, Key::Z) || keys(cmd, Key::Y) {
            if let Some(f) = self.file() {
                f.redo();
            }
        } else if keys(cmd, Key::Z) {
            if let Some(f) = self.file() {
                f.undo();
            }
        }
        let select_all = keys(cmd, Key::A);
        let del = keys(Modifiers::NONE, Key::Delete) || keys(Modifiers::NONE, Key::Backspace);
        let (cut, copy, paste) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, Event::Cut)),
                i.events.iter().any(|e| matches!(e, Event::Copy)),
                i.events.iter().any(|e| matches!(e, Event::Paste(_))),
            )
        });
        let space = !ctx.egui_wants_keyboard_input() && keys(Modifiers::NONE, Key::Space);
        let typing = ctx.egui_wants_keyboard_input();
        let home = !typing && keys(Modifiers::NONE, Key::Home);
        let end = !typing && keys(Modifiers::NONE, Key::End);
        let clips = &mut self.clips;
        if let Some(f) = self.files.get_mut(self.active) {
            if select_all {
                f.select_all();
            }
            if del {
                f.delete();
            }
            if cut {
                f.cut(clips);
            }
            if copy {
                f.copy(clips);
            }
            if paste {
                f.paste(clips);
            }
        }
        if space {
            self.toggle_play();
        }
        if home {
            self.go_to(0);
        }
        if end {
            let frames = self.files.get(self.active).map_or(0, |f| f.doc().frames());
            self.go_to(frames);
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open…").clicked() {
                    self.pick_and_open();
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    self.save(false);
                    ui.close();
                }
                if ui.button("Save As…").clicked() {
                    self.save(true);
                    ui.close();
                }
                if ui.button("Close").clicked() {
                    if self.active < self.files.len() {
                        self.files.remove(self.active);
                        self.active = self.active.saturating_sub(1);
                        self.synced = None;
                        self.stop();
                    }
                    ui.close();
                }
            });
            let has = !self.files.is_empty();
            ui.menu_button("Edit", |ui| {
                let (can_undo, can_redo, has_sel) =
                    self.files
                        .get(self.active)
                        .map_or((false, false, false), |f| {
                            (
                                f.history.can_undo(),
                                f.history.can_redo(),
                                !f.selection.is_empty(),
                            )
                        });
                let clips = &mut self.clips;
                let has_clip = clips.active_clip().is_some();
                let Some(f) = self.files.get_mut(self.active) else {
                    ui.add_enabled(false, egui::Button::new("Open a file to edit"));
                    return;
                };
                let item = |ui: &mut egui::Ui, label: &str, enabled: bool| {
                    ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                };
                if item(ui, "Undo", can_undo) {
                    f.undo();
                    ui.close();
                }
                if item(ui, "Redo", can_redo) {
                    f.redo();
                    ui.close();
                }
                ui.separator();
                if item(ui, "Cut", has_sel) {
                    f.cut(clips);
                    ui.close();
                }
                if item(ui, "Copy", has_sel) {
                    f.copy(clips);
                    ui.close();
                }
                if item(ui, "Paste", has_clip) {
                    f.paste(clips);
                    ui.close();
                }
                if item(ui, "Mix Paste", has_clip) {
                    f.mix_paste(clips);
                    ui.close();
                }
                if item(ui, "Delete", has_sel) {
                    f.delete();
                    ui.close();
                }
                if item(ui, "Crop to Selection", has_sel) {
                    f.crop();
                    ui.close();
                }
                ui.separator();
                if item(ui, "Select All", true) {
                    f.select_all();
                    ui.close();
                }
                ui.separator();
                ui.menu_button("Clipboard", |ui| {
                    for i in 0..Clipboards::COUNT {
                        if ui
                            .radio(clips.active == i, format!("Clipboard {}", i + 1))
                            .clicked()
                        {
                            clips.active = i;
                            ui.close();
                        }
                    }
                });
            });
            ui.add_enabled_ui(has, |ui| {
                ui.menu_button("Effects", |ui| {
                    if ui.button("Invert").clicked() {
                        self.files[self.active].invert();
                        ui.close();
                    }
                    if ui.button("Reverse").clicked() {
                        self.files[self.active].reverse();
                        ui.close();
                    }
                    if ui.button("Silence").clicked() {
                        self.files[self.active].silence();
                        ui.close();
                    }
                    if ui.button("Amplify…").clicked() {
                        self.dialog = Some(Dialog::Amplify(3.0));
                        ui.close();
                    }
                    if ui.button("Insert Silence…").clicked() {
                        self.dialog = Some(Dialog::InsertSilence(1.0));
                        ui.close();
                    }
                });
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        let title = match dialog {
            Dialog::InsertSilence(_) => "Insert Silence",
            Dialog::Amplify(_) => "Amplify",
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                match dialog {
                    Dialog::InsertSilence(secs) => {
                        ui.horizontal(|ui| {
                            ui.label("Duration (s)");
                            ui.add(egui::DragValue::new(secs).range(0.001..=3600.0).speed(0.05));
                        });
                    }
                    Dialog::Amplify(db) => {
                        ui.horizontal(|ui| {
                            ui.label("Gain (dB)");
                            ui.add(egui::DragValue::new(db).range(-96.0..=48.0).speed(0.1));
                        });
                    }
                }
                ui.horizontal(|ui| {
                    apply = ui.button("Apply").clicked() || ui.input(|i| i.key_pressed(Key::Enter));
                    cancel =
                        ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
                });
            });
        if apply {
            let d = self.dialog.take();
            if let (Some(d), Some(f)) = (d, self.files.get_mut(self.active)) {
                match d {
                    Dialog::InsertSilence(s) => f.insert_silence(s),
                    Dialog::Amplify(db) => f.amplify(db),
                }
            }
        } else if cancel {
            self.dialog = None;
        }
    }

    fn transport_bar(&mut self, ui: &mut egui::Ui) {
        let playing = self.shared.as_ref().is_some_and(|s| s.is_playing());
        let has_file = !self.files.is_empty();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(has_file, |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                if icons::button(ui, Icon::ToStart, false, "Go to start (Home)").clicked() {
                    self.go_to(0);
                }
                let (icon, tip) = if playing {
                    (Icon::Pause, "Pause (Space)")
                } else {
                    (Icon::Play, "Play (Space)")
                };
                if icons::button(ui, icon, playing, tip).clicked() {
                    self.toggle_play();
                }
                if icons::button(ui, Icon::Stop, false, "Stop").clicked() {
                    self.stop();
                }
                let end = self.files.get(self.active).map_or(0, |f| f.doc().frames());
                if icons::button(ui, Icon::ToEnd, false, "Go to end (End)").clicked() {
                    self.go_to(end);
                }
                ui.add_space(6.0);
                let tip = if self.looping {
                    "Loop playback: on"
                } else {
                    "Loop playback: off"
                };
                if icons::button(ui, Icon::Loop, self.looping, tip).clicked() {
                    self.looping = !self.looping;
                }
                let tip = if self.follow {
                    "Follow playhead: on"
                } else {
                    "Follow playhead: off"
                };
                if icons::button(ui, Icon::Follow, self.follow, tip).clicked() {
                    self.follow = !self.follow;
                }
            });
            ui.add_space(10.0);
            if let Some(f) = self.files.get(self.active) {
                let pos = self.playhead();
                ui.monospace(format!(
                    "{} / {}",
                    format_time(pos, f.doc().sample_rate),
                    format_time(f.doc().frames() as f64, f.doc().sample_rate)
                ));
            }
        });
    }

    /// Move the playhead and cursor to `frame`, clearing any selection.
    fn go_to(&mut self, frame: usize) {
        let Some(f) = self.files.get_mut(self.active) else {
            return;
        };
        let frame = frame.min(f.doc().frames());
        f.selection = Selection::cursor(frame);
        f.reveal_request = Some(frame);
        if let Some(s) = &self.shared {
            s.seek(frame);
        }
    }
}

struct Viewer<'a> {
    s: &'a mut State,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(tab.title())
    }

    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let s = &mut *self.s;
        match *tab {
            Tab::Editor => {
                s.transport_bar(ui);
                let playhead = s.playhead();
                let playing = s.shared.as_ref().is_some_and(|sh| sh.is_playing());
                let follow = s.follow && playing;
                let zoom = waveform::toolbar(ui, &mut s.display, !s.files.is_empty());
                if let Some(f) = s.files.get_mut(s.active) {
                    let ev = waveform::show(ui, f, playhead, follow, &s.display, zoom);
                    if let (Some(at), Some(sh)) = (ev.seek, &s.shared) {
                        sh.seek(at);
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        if ui
                            .button("Open an audio file…  (⌘O or drop a file here)")
                            .clicked()
                        {
                            s.pick_and_open();
                        }
                    });
                }
            }
            Tab::Files => {
                let mut pick = None;
                for (i, f) in s.files.iter().enumerate() {
                    let label = format!("{}{}", f.name, if f.dirty() { " •" } else { "" });
                    if ui.selectable_label(i == s.active, label).clicked() {
                        pick = Some(i);
                    }
                }
                if let Some(i) = pick {
                    s.set_active(i);
                }
            }
            Tab::History => {
                if let Some(f) = s.files.get_mut(s.active) {
                    let cur = f.history.cursor();
                    let labels: Vec<String> = f.history.labels().map(String::from).collect();
                    let mut jump = None;
                    for (i, l) in labels.iter().enumerate() {
                        let text = if i > cur {
                            egui::RichText::new(l).weak()
                        } else {
                            egui::RichText::new(l)
                        };
                        if ui.selectable_label(i == cur, text).clicked() {
                            jump = Some(i);
                        }
                    }
                    if let Some(i) = jump {
                        f.history.jump(i);
                        f.revision += 1;
                    }
                }
            }
            Tab::Levels => {
                let mut peaks = [0f32; 8];
                if let Some(sh) = &s.shared {
                    sh.take_peaks(&mut peaks);
                }
                let now = ui.input(|i| i.time);
                s.meters.update(&peaks, now);
                let ch = s.files.get(s.active).map_or(2, |f| f.doc().channel_count());
                s.meters.show(ui, ch);
            }
            Tab::Selection => {
                if let Some(f) = s.files.get_mut(s.active) {
                    let rate = f.doc().sample_rate;
                    let sel: Selection = f.selection;
                    egui::Grid::new("selgrid").num_columns(2).show(ui, |ui| {
                        ui.label("Start");
                        ui.monospace(format_time(sel.start as f64, rate));
                        ui.end_row();
                        ui.label("End");
                        ui.monospace(format_time(sel.end as f64, rate));
                        ui.end_row();
                        ui.label("Length");
                        ui.monospace(format_time(sel.len() as f64, rate));
                        ui.end_row();
                        ui.label("Samples");
                        ui.monospace(sel.len().to_string());
                        ui.end_row();
                    });
                    ui.separator();
                    ui.label("Edit channels");
                    for c in 0..f.doc().channel_count().min(8) {
                        let mut on = f.mask.has(c);
                        if ui
                            .checkbox(
                                &mut on,
                                if c == 0 {
                                    "Left / 1".to_string()
                                } else if c == 1 {
                                    "Right / 2".to_string()
                                } else {
                                    format!("Channel {}", c + 1)
                                },
                            )
                            .changed()
                        {
                            f.mask.0 = if on {
                                f.mask.0 | (1 << c)
                            } else {
                                f.mask.0 & !(1 << c)
                            };
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for BoothcraftApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            self.state.open(&p);
        }
        self.state.handle_shortcuts(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.state.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(&self.state.status);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style().as_ref()))
                .show_inside(ui, &mut Viewer { s: &mut self.state });
        });
        self.state.dialogs(&ctx);
        self.state.sync_engine();
        if self.state.shared.as_ref().is_some_and(|s| s.is_playing()) {
            ctx.request_repaint();
        }
    }
}
