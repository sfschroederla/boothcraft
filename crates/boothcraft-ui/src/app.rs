use crate::effects_ui::{
    effects_by_category, ApplyJob, ApplyKind, EffectAction, EffectDialog, Preview, RackPanel,
};
use crate::generate_ui::Generator;
use crate::icons::{self, Icon};
use crate::loudness_ui::{LoudnessPanel, MatchLoudnessPanel};
use crate::markers_panel::{MarkerAction, MarkersPanel};
use crate::meters::Meters;
use crate::ops::{OpenFile, ZeroAdjust};
use crate::scale;
use crate::selview_panel::SelViewPanel;
use crate::view::format_time;
use crate::waveform;
use boothcraft_core::{Clipboards, Document, Rope, Selection};
use boothcraft_dsp::convert::Downmix;
use boothcraft_engine::{Engine, Recorder, Shared, Take};
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
    Markers,
    EffectsRack,
    PhaseMeter,
    Loudness,
    MatchLoudness,
    Selection,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Editor => "Editor",
            Tab::Files => "Files",
            Tab::History => "History",
            Tab::Levels => "Levels",
            Tab::Markers => "Markers",
            Tab::EffectsRack => "Effects Rack",
            Tab::PhaseMeter => "Phase Meter",
            Tab::Loudness => "Loudness",
            Tab::MatchLoudness => "Match Loudness",
            Tab::Selection => "Selection/View",
        }
    }
}

enum Dialog {
    InsertSilence(f64),

    Preferences {
        preroll: f64,
        postroll: f64,
    },
    SaveAs {
        path: PathBuf,
        opts: WriteOptions,
    },
    NewFile {
        rate: u32,
        channels: usize,
    },
    ConvertSampleType {
        rate: u32,
        channels: usize,
        downmix: Downmix,
    },
}

const SAMPLE_RATES: [u32; 11] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
];

fn depth_name(d: BitDepth) -> &'static str {
    match d {
        BitDepth::Int16 => "16-bit integer",
        BitDepth::Int24 => "24-bit integer",
        BitDepth::Float32 => "32-bit float",
    }
}

/// A recording in progress into file `file`, overwriting from `start`.
struct Recording {
    recorder: Recorder,
    take: Take,
    file: usize,
    start: usize,
    /// The document before recording began; the take is written over a copy of it.
    base: Document,
    /// Frames of the take already copied into the file's preview.
    shown: usize,
    scratch: Vec<f32>,
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
    /// Zoom requested by the transport bar, applied by the waveform on the next frame.
    pending_zoom: Option<waveform::ZoomCmd>,
    markers_panel: MarkersPanel,
    selview_panel: SelViewPanel,
    preroll_secs: f64,
    postroll_secs: f64,
    skip_selection: bool,
    /// Where playback was paused and the selection at that moment, so Play resumes there
    /// unless the user has since moved the cursor or selection.
    paused_at: Option<(usize, Selection)>,
    recording: Option<Recording>,
    untitled: usize,
    preview: Preview,
    rack_panel: RackPanel,
    effect_dialog: Option<EffectDialog>,
    apply_job: Option<ApplyJob>,
    generator: Option<Generator>,
    loudness: LoudnessPanel,
    match_panel: MatchLoudnessPanel,
    /// Further jobs waiting to run after the current Apply (Match Loudness over several files).
    apply_queue: std::collections::VecDeque<(usize, String, ApplyKind)>,
}

pub struct BoothcraftApp {
    dock: DockState<Tab>,
    state: State,
}

impl BoothcraftApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> BoothcraftApp {
        // Editor on top; Selection/View, Phase Meter and Levels side by side underneath. The split
        // fraction is the share kept by the *existing* node.
        let mut dock = DockState::new(vec![Tab::Editor]);
        let surface = dock.main_surface_mut();
        let [_editor, bottom] = surface.split_below(
            NodeIndex::root(),
            0.82,
            vec![Tab::Selection, Tab::Markers, Tab::EffectsRack],
        );
        let [_selection, rest] =
            surface.split_right(bottom, 0.2, vec![Tab::PhaseMeter, Tab::Loudness]);
        surface.split_right(rest, 0.25, vec![Tab::Levels]);

        let (engine, status) = match Engine::new() {
            Ok(e) => {
                let s = format!("Audio: {} @ {} Hz", e.device_name, e.sample_rate);
                (Some(e), s)
            }
            Err(e) => (None, format!("No audio output ({e}); editing still works")),
        };
        let mut engine = engine;
        let rack_ctl = engine.as_mut().and_then(Engine::take_rack_control);
        let tap = engine.as_mut().and_then(Engine::take_tap);
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
            pending_zoom: None,
            markers_panel: MarkersPanel::default(),
            selview_panel: SelViewPanel::default(),
            preroll_secs: 0.0,
            postroll_secs: 0.0,
            skip_selection: false,
            paused_at: None,
            recording: None,
            untitled: 0,
            preview: Preview::new(rack_ctl),
            rack_panel: RackPanel::default(),
            effect_dialog: None,
            apply_job: None,
            generator: None,
            loudness: LoudnessPanel::new(tap),
            match_panel: MatchLoudnessPanel::default(),
            apply_queue: Default::default(),
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

    /// Save to the file's own path with its last-used options, or ask for a path and options.
    fn save(&mut self, as_new: bool) {
        let Some(f) = self.files.get(self.active) else {
            return;
        };
        let existing = f
            .path
            .clone()
            .filter(|p| boothcraft_io::format_for_path(p).is_some());
        if let (false, Some(path), Some(opts)) = (as_new, existing.clone(), f.save_options) {
            self.write_file(path, opts);
            return;
        }
        let path = match (as_new, existing) {
            (false, Some(p)) => p,
            _ => {
                let stem = f
                    .name
                    .rsplit_once('.')
                    .map_or(f.name.as_str(), |x| x.0)
                    .to_string();
                let Some(p) = rfd::FileDialog::new()
                    .set_file_name(format!("{stem}.wav"))
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
        let depth = if format == Format::Wav {
            BitDepth::Float32
        } else {
            BitDepth::Int24
        };
        let opts = WriteOptions {
            dither: depth != BitDepth::Float32,
            ..WriteOptions::new(format, depth)
        };
        self.dialog = Some(Dialog::SaveAs { path, opts });
    }

    fn write_file(&mut self, path: PathBuf, opts: WriteOptions) {
        let Some(f) = self.files.get_mut(self.active) else {
            return;
        };
        match boothcraft_io::write(f.doc(), &path, &opts) {
            Ok(()) => {
                f.saved_at = f.history.cursor();
                f.name = path
                    .file_name()
                    .map_or_else(|| f.name.clone(), |n| n.to_string_lossy().into_owned());
                f.path = Some(path.clone());
                f.save_options = Some(opts);
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
        let sel = f.selection;
        if self.looping {
            shared.set_loop(true, sel.start, if sel.is_empty() { 0 } else { sel.end });
        } else {
            shared.set_loop(false, 0, 0);
        }
        shared.set_skip(self.skip_selection && !self.looping, sel.start, sel.end);
    }

    fn effect_action(&mut self, action: EffectAction) {
        match action {
            EffectAction::TogglePlayback => self.toggle_play(),
            EffectAction::Apply { label, kind } => {
                if self.apply_job.is_some() {
                    return;
                }
                if let Some(f) = self.files.get(self.active) {
                    if let Some(sh) = &self.shared {
                        sh.pause();
                    }
                    self.apply_job = Some(ApplyJob::start(self.active, f, label, kind));
                }
            }
        }
    }

    /// The open Generate dialog: on OK, insert into the active file (or a new one).
    fn generator_frame(&mut self, ctx: &egui::Context) {
        let Some(g) = &mut self.generator else {
            return;
        };
        let target = match self.files.get(self.active) {
            Some(f) if f.selection.is_empty() => format!("Inserts at the cursor in {}", f.name),
            Some(f) => format!("Replaces the selection in {}", f.name),
            None => "Creates a new 48 kHz stereo file".to_string(),
        };
        match g.show(ctx, &target) {
            Some(true) => {
                let g = self.generator.take().expect("generator present");
                if self.files.is_empty() {
                    self.new_file(48_000, 2);
                }
                let f = &mut self.files[self.active];
                let (label, samples) = g.render(f.doc().sample_rate);
                f.insert_audio(&label, &samples);
                self.status = format!(
                    "{label}: {:.2} s",
                    samples.len() as f64 / f.doc().sample_rate as f64
                );
            }
            Some(false) => self.generator = None,
            None => {}
        }
    }

    /// Per-frame effect plumbing: the open effect dialog and any Apply in progress.
    fn effects_frame(&mut self, ctx: &egui::Context) {
        self.preview.tick();
        self.loudness.tick();
        if self.apply_job.is_none() {
            if let Some((i, label, kind)) = self.apply_queue.pop_front() {
                if let Some(f) = self.files.get(i) {
                    self.apply_job = Some(ApplyJob::start_whole(i, f, label, kind));
                }
            }
        }
        let playing = self.shared.as_ref().is_some_and(|s| s.is_playing());
        if let Some(dlg) = &mut self.effect_dialog {
            let action = dlg.show(ctx, &mut self.preview, playing);
            if !dlg.open {
                self.effect_dialog = None;
            }
            if let Some(a) = action {
                self.effect_action(a);
            }
        }
        let Some(job) = &self.apply_job else {
            return;
        };
        match job.poll() {
            Some(result) => {
                let job = self.apply_job.take().expect("job present");
                match (result, self.files.get_mut(job.file)) {
                    (Some(chans), Some(f)) => {
                        job.commit(f, chans);
                        self.status = format!("Applied {}", job.label);
                    }
                    _ => self.status = format!("{} cancelled", job.label),
                }
            }
            None => {
                egui::Modal::new(egui::Id::new("apply_progress")).show(ctx, |ui| {
                    ui.label(format!("Applying {}…", job.label));
                    ui.add(
                        egui::ProgressBar::new(job.progress())
                            .show_percentage()
                            .desired_width(260.0),
                    );
                    if ui.button("Cancel").clicked() {
                        job.cancel();
                    }
                });
                ctx.request_repaint();
            }
        }
    }

    fn new_file(&mut self, rate: u32, channels: usize) {
        self.untitled += 1;
        let doc = Document::new(rate, vec![Rope::new(); channels.max(1)]);
        self.files.push(OpenFile::new(
            format!("Untitled {}", self.untitled),
            None,
            doc,
        ));
        self.set_active(self.files.len() - 1);
    }

    fn toggle_record(&mut self) {
        if self.recording.is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    /// Record over the active file from the cursor; with a selection, punch in over exactly the
    /// selection. With no file open, a new 48 kHz stereo file is created first.
    fn start_recording(&mut self) {
        if self.files.is_empty() {
            self.new_file(48_000, 2);
        }
        self.stop();
        let recorder = match Recorder::start() {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Cannot record: {e}");
                return;
            }
        };
        let f = &mut self.files[self.active];
        let (doc_rate, doc_ch) = (f.doc().sample_rate, f.doc().channel_count());
        let sel = f.selection;
        let limit = (!sel.is_empty()).then_some(sel.len());
        let take = Take::new(
            recorder.sample_rate,
            recorder.channels,
            doc_rate,
            doc_ch,
            limit,
        );
        let base = f.doc().clone();
        f.preview = Some(base.clone());
        self.status = format!(
            "Recording from {} ({} Hz, {} ch){}",
            recorder.device_name,
            recorder.sample_rate,
            recorder.channels,
            if limit.is_some() {
                ", punch-in over the selection"
            } else {
                ""
            }
        );
        self.recording = Some(Recording {
            recorder,
            take,
            file: self.active,
            start: sel.start,
            base,
            shown: 0,
            scratch: Vec::new(),
        });
    }

    /// Pull captured audio into the take and extend the live preview. Stops a finished punch-in.
    fn poll_recording(&mut self, ctx: &egui::Context) {
        let Some(rec) = &mut self.recording else {
            return;
        };
        rec.scratch.clear();
        rec.recorder.drain(&mut rec.scratch);
        rec.take.push_interleaved(&rec.scratch);
        let peaks = rec.take.take_peaks();
        self.meters.update(&peaks, ctx.input(|i| i.time));
        let new = rec.take.live_len();
        if let Some(f) = self.files.get_mut(rec.file) {
            if let Some(preview) = &mut f.preview {
                let chans: Vec<&[f32]> =
                    rec.take.live().iter().map(|c| &c[rec.shown..new]).collect();
                preview.overwrite(rec.start + rec.shown, &chans);
                f.reveal_request = Some(rec.start + new);
            }
        }
        rec.shown = new;
        if rec.recorder.dropped() > 0 {
            self.status = format!(
                "Recording: {} samples dropped (the app fell behind)",
                rec.recorder.dropped()
            );
        }
        let full = rec.take.is_full();
        ctx.request_repaint();
        if full {
            self.stop_recording();
        }
    }

    /// Finish the take (high-quality resampled) and commit it as one undoable "Record" step.
    fn stop_recording(&mut self) {
        let Some(mut rec) = self.recording.take() else {
            return;
        };
        rec.scratch.clear();
        rec.recorder.drain(&mut rec.scratch);
        rec.take.push_interleaved(&rec.scratch);
        let chans = rec.take.finish();
        let len = chans.first().map_or(0, Vec::len);
        let Some(f) = self.files.get_mut(rec.file) else {
            return;
        };
        f.preview = None;
        if len == 0 {
            self.status = "Recording stopped: nothing captured".into();
            return;
        }
        let mut doc = rec.base;
        let slices: Vec<&[f32]> = chans.iter().map(Vec::as_slice).collect();
        doc.overwrite(rec.start, &slices);
        f.commit("Record", doc);
        f.selection = Selection::new(rec.start, rec.start + len);
        self.synced = None;
        self.status = format!("Recorded {:.2} s", len as f64 / f.doc().sample_rate as f64);
    }

    /// Play or pause. Playing a selection starts at its start (minus pre-roll) and stops at its
    /// end (plus post-roll) unless looping. With Skip Selection on, the selection is played around:
    /// at least two seconds either side, jumping over the selected audio.
    fn toggle_play(&mut self) {
        let (Some(shared), Some(f)) = (self.shared.clone(), self.files.get(self.active)) else {
            return;
        };
        if shared.is_playing() {
            shared.pause();
            self.paused_at = Some((shared.position() as usize, f.selection));
            return;
        }
        let sel = f.selection;
        let (rate, frames) = (f.doc().sample_rate as f64, f.doc().frames());
        let secs = |s: f64| (s * rate) as usize;
        if let Some((at, paused_sel)) = self.paused_at.take() {
            if paused_sel == sel && at < frames {
                shared.seek(at);
                shared.play();
                return;
            }
        }
        let (pre, post) = if self.skip_selection && !sel.is_empty() {
            (self.preroll_secs.max(2.0), self.postroll_secs.max(2.0))
        } else {
            (self.preroll_secs, self.postroll_secs)
        };
        let start = sel
            .start
            .saturating_sub(secs(pre))
            .min(frames.saturating_sub(1));
        let stop = (!sel.is_empty() && !self.looping).then(|| (sel.end + secs(post)).min(frames));
        shared.set_stop_at(stop);
        shared.seek(start);
        shared.play();
    }

    fn stop(&mut self) {
        self.paused_at = None;
        if let Some(s) = &self.shared {
            s.stop();
        }
    }

    fn playhead(&self) -> f64 {
        self.shared.as_ref().map_or(0.0, |s| s.position())
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || self.apply_job.is_some() || self.generator.is_some() {
            return;
        }
        if self.recording.is_some() {
            // While recording only Space or Escape (stop) are live; edits would race the take.
            if ctx.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Space)
                    || i.consume_key(Modifiers::NONE, Key::Escape)
            }) {
                self.stop_recording();
            }
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
        let record = !ctx.egui_wants_keyboard_input() && keys(Modifiers::SHIFT, Key::Space);
        let space = !ctx.egui_wants_keyboard_input() && keys(Modifiers::NONE, Key::Space);
        let typing = ctx.egui_wants_keyboard_input();
        let home = !typing && keys(Modifiers::NONE, Key::Home);
        let end = !typing && keys(Modifiers::NONE, Key::End);
        let add_marker = !typing && keys(Modifiers::NONE, Key::M);
        let toggle_snap = !typing && keys(Modifiers::NONE, Key::S);
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
        if record {
            self.toggle_record();
        } else if space {
            self.toggle_play();
        }
        if toggle_snap {
            self.display.snap.enabled = !self.display.snap.enabled;
            self.status = format!(
                "Snapping {}",
                if self.display.snap.enabled {
                    "on"
                } else {
                    "off"
                }
            );
        }
        if add_marker {
            if let Some(f) = self.files.get_mut(self.active) {
                f.add_marker();
            }
        }
        if home {
            self.go_to(0);
        }
        if end {
            let frames = self.files.get(self.active).map_or(0, |f| f.doc().frames());
            self.go_to(frames);
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui, dock: &mut DockState<Tab>) {
        if self.recording.is_some() {
            ui.disable();
        }
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New…").clicked() {
                    self.dialog = Some(Dialog::NewFile {
                        rate: 48_000,
                        channels: 2,
                    });
                    ui.close();
                }
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
                ui.menu_button("Zero Crossings", |ui| {
                    for (label, mode) in ZeroAdjust::ALL {
                        if ui.button(label).clicked() {
                            f.adjust_zero_crossings(mode);
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Snapping", |ui| {
                    let snap = &mut self.display.snap;
                    ui.checkbox(&mut snap.enabled, "Snapping (S)");
                    ui.add_enabled_ui(snap.enabled, |ui| {
                        ui.checkbox(&mut snap.markers, "Snap to Markers");
                        ui.checkbox(&mut snap.ruler, "Snap to Ruler Ticks");
                        ui.checkbox(&mut snap.zero_crossings, "Snap to Zero Crossings");
                    });
                });
                ui.separator();
                if ui.button("Convert Sample Type…").clicked() {
                    let (rate, channels) = (f.doc().sample_rate, f.doc().channel_count().min(2));
                    self.dialog = Some(Dialog::ConvertSampleType {
                        rate,
                        channels,
                        downmix: Downmix::Mix,
                    });
                    ui.close();
                }
                if ui.button("Preferences…").clicked() {
                    self.dialog = Some(Dialog::Preferences {
                        preroll: self.preroll_secs,
                        postroll: self.postroll_secs,
                    });
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
                    if ui.button("Insert Silence…").clicked() {
                        self.dialog = Some(Dialog::InsertSilence(1.0));
                        ui.close();
                    }
                    ui.separator();
                    for (cat, list) in effects_by_category(false) {
                        ui.menu_button(cat.name(), |ui| {
                            for e in list {
                                let label = if e.is_realtime() {
                                    e.name.to_string()
                                } else {
                                    format!("{} (process)", e.name)
                                };
                                if ui.button(label).clicked() {
                                    self.effect_dialog = EffectDialog::new(e.id);
                                    ui.close();
                                }
                            }
                        });
                    }
                });
            });
            ui.menu_button("Generate", |ui| {
                if ui.button("Tones…").clicked() {
                    self.generator = Some(Generator::tone());
                    ui.close();
                }
                if ui.button("Noise…").clicked() {
                    self.generator = Some(Generator::noise());
                    ui.close();
                }
                if ui.button("DTMF Tones…").clicked() {
                    self.generator = Some(Generator::dtmf());
                    ui.close();
                }
                if ui.add_enabled(has, egui::Button::new("Silence…")).clicked() {
                    self.dialog = Some(Dialog::InsertSilence(1.0));
                    ui.close();
                }
            });
            ui.menu_button("Window", |ui| {
                for tab in [
                    Tab::Files,
                    Tab::History,
                    Tab::Selection,
                    Tab::Markers,
                    Tab::EffectsRack,
                    Tab::PhaseMeter,
                    Tab::Loudness,
                    Tab::MatchLoudness,
                    Tab::Levels,
                ] {
                    let open = dock.find_tab(&tab);
                    let mut checked = open.is_some();
                    if ui.checkbox(&mut checked, tab.title()).clicked() {
                        match open {
                            Some(path) => {
                                dock.remove_tab(path);
                            }
                            None => show_panel(dock, tab),
                        }
                    }
                }
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

            Dialog::Preferences { .. } => "Preferences",
            Dialog::SaveAs { .. } => "Save As",
            Dialog::ConvertSampleType { .. } => "Convert Sample Type",
            Dialog::NewFile { .. } => "New Audio File",
        };
        let doc_rate = self
            .files
            .get(self.active)
            .map_or(48_000, |f| f.doc().sample_rate);
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

                    Dialog::SaveAs { path, opts } => {
                        ui.label(egui::RichText::new(path.display().to_string()).weak());
                        egui::Grid::new("saveas").num_columns(2).show(ui, |ui| {
                            ui.label("Sample type");
                            egui::ComboBox::from_id_salt("depth")
                                .selected_text(depth_name(opts.depth))
                                .show_ui(ui, |ui| {
                                    for d in [BitDepth::Int16, BitDepth::Int24, BitDepth::Float32] {
                                        let allowed =
                                            d != BitDepth::Float32 || opts.format == Format::Wav;
                                        ui.add_enabled_ui(allowed, |ui| {
                                            ui.selectable_value(&mut opts.depth, d, depth_name(d))
                                        });
                                    }
                                });
                            ui.end_row();
                            ui.label("Sample rate");
                            let current = opts
                                .sample_rate
                                .map_or(format!("Keep ({doc_rate} Hz)"), |r| format!("{r} Hz"));
                            egui::ComboBox::from_id_salt("rate")
                                .selected_text(current)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut opts.sample_rate,
                                        None,
                                        format!("Keep ({doc_rate} Hz)"),
                                    );
                                    for r in SAMPLE_RATES {
                                        ui.selectable_value(
                                            &mut opts.sample_rate,
                                            Some(r),
                                            format!("{r} Hz"),
                                        );
                                    }
                                });
                            ui.end_row();
                            ui.label("Dither");
                            ui.add_enabled(
                                opts.depth != BitDepth::Float32,
                                egui::Checkbox::new(&mut opts.dither, "Triangular (TPDF)"),
                            );
                            ui.end_row();
                        });
                    }
                    Dialog::ConvertSampleType {
                        rate,
                        channels,
                        downmix,
                    } => {
                        egui::Grid::new("convert").num_columns(2).show(ui, |ui| {
                            ui.label("Sample rate");
                            egui::ComboBox::from_id_salt("cst_rate")
                                .selected_text(format!("{rate} Hz"))
                                .show_ui(ui, |ui| {
                                    for r in SAMPLE_RATES {
                                        ui.selectable_value(rate, r, format!("{r} Hz"));
                                    }
                                });
                            ui.end_row();
                            ui.label("Channels");
                            ui.horizontal(|ui| {
                                ui.radio_value(channels, 1, "Mono");
                                ui.radio_value(channels, 2, "Stereo");
                            });
                            ui.end_row();
                            ui.label("To mono");
                            ui.horizontal(|ui| {
                                ui.radio_value(downmix, Downmix::Mix, "Mix channels");
                                ui.radio_value(downmix, Downmix::First, "Left only");
                            });
                            ui.end_row();
                        });
                    }
                    Dialog::NewFile { rate, channels } => {
                        egui::Grid::new("newfile").num_columns(2).show(ui, |ui| {
                            ui.label("Sample rate");
                            egui::ComboBox::from_id_salt("new_rate")
                                .selected_text(format!("{rate} Hz"))
                                .show_ui(ui, |ui| {
                                    for r in SAMPLE_RATES {
                                        ui.selectable_value(rate, r, format!("{r} Hz"));
                                    }
                                });
                            ui.end_row();
                            ui.label("Channels");
                            ui.horizontal(|ui| {
                                ui.radio_value(channels, 1, "Mono");
                                ui.radio_value(channels, 2, "Stereo");
                            });
                            ui.end_row();
                        });
                    }
                    Dialog::Preferences { preroll, postroll } => {
                        egui::Grid::new("prefs").num_columns(2).show(ui, |ui| {
                            ui.label("Pre-roll (s)");
                            ui.add(egui::DragValue::new(preroll).range(0.0..=30.0).speed(0.05));
                            ui.end_row();
                            ui.label("Post-roll (s)");
                            ui.add(egui::DragValue::new(postroll).range(0.0..=30.0).speed(0.05));
                            ui.end_row();
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
            match self.dialog.take() {
                Some(Dialog::Preferences { preroll, postroll }) => {
                    self.preroll_secs = preroll;
                    self.postroll_secs = postroll;
                }
                Some(Dialog::SaveAs { path, opts }) => self.write_file(path, opts),
                Some(Dialog::NewFile { rate, channels }) => self.new_file(rate, channels),
                Some(d) => {
                    if let Some(f) = self.files.get_mut(self.active) {
                        match d {
                            Dialog::InsertSilence(s) => f.insert_silence(s),

                            Dialog::ConvertSampleType {
                                rate,
                                channels,
                                downmix,
                            } => {
                                f.convert_sample_type(rate, channels, downmix);
                                self.synced = None;
                            }
                            Dialog::Preferences { .. }
                            | Dialog::SaveAs { .. }
                            | Dialog::NewFile { .. } => {}
                        }
                    }
                }
                None => {}
            }
        } else if cancel {
            self.dialog = None;
        }
    }

    /// Timecode on the left, transport in the middle, zoom on the right.
    fn transport_row(&mut self, ui: &mut egui::Ui) {
        let playing = self.shared.as_ref().is_some_and(|s| s.is_playing());
        let has_file = !self.files.is_empty();
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), TRANSPORT_H),
            egui::Sense::hover(),
        );
        let group_w = |n: usize| n as f32 * 28.0 + n.saturating_sub(1) as f32 * 3.0;
        let centred = |w: f32| {
            egui::Rect::from_min_size(
                egui::pos2(rect.center().x - w / 2.0, rect.top()),
                egui::vec2(w, rect.height()),
            )
        };
        let layout = egui::Layout::left_to_right(egui::Align::Center);

        // Timecode.
        let code = self.files.get(self.active).map_or_else(
            || "0:00.000".to_string(),
            |f| scale::format_position(self.playhead(), f.doc().sample_rate, self.display.time),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(code)
                    .size(22.0)
                    .color(egui::Color32::from_rgb(96, 150, 255)),
            );
        });

        // Transport.
        let end = self.files.get(self.active).map_or(0, |f| f.doc().frames());
        let mut zoom = None;
        let mut scrub: Option<f64> = None;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(centred(group_w(13)))
                .layout(layout),
            |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.add_enabled_ui(has_file, |ui| {
                    if icons::button(ui, Icon::Stop, false, "Stop").clicked() {
                        if self.recording.is_some() {
                            self.stop_recording();
                        } else {
                            self.stop();
                        }
                    }
                    if icons::button(ui, Icon::Play, playing, "Play (Space)").clicked() && !playing
                    {
                        self.toggle_play();
                    }
                    if icons::button(ui, Icon::Pause, false, "Pause (Space)").clicked() && playing {
                        self.toggle_play();
                    }
                    if icons::button(ui, Icon::ToStart, false, "Go to start (Home)").clicked() {
                        self.go_to(0);
                    }
                    if icons::button(ui, Icon::Rewind, false, "Rewind (hold)")
                        .is_pointer_button_down_on()
                    {
                        scrub = Some(-1.0);
                    }
                    if icons::button(ui, Icon::FastForward, false, "Fast forward (hold)")
                        .is_pointer_button_down_on()
                    {
                        scrub = Some(1.0);
                    }
                    if icons::button(ui, Icon::ToEnd, false, "Go to end (End)").clicked() {
                        self.go_to(end);
                    }
                    if icons::button(ui, Icon::PrevMarker, false, "Previous marker").clicked() {
                        self.jump_marker(false);
                    }
                    if icons::button(ui, Icon::NextMarker, false, "Next marker").clicked() {
                        self.jump_marker(true);
                    }
                });
                let recording = self.recording.is_some();
                let tip = if recording {
                    "Stop recording (Space)"
                } else {
                    "Record from the cursor; punch in over a selection (Shift+Space)"
                };
                if icons::button(ui, Icon::Record, recording, tip).clicked() {
                    self.toggle_record();
                }
                ui.add_enabled_ui(has_file, |ui| {
                    let tip = if self.looping {
                        "Loop playback: on"
                    } else {
                        "Loop playback: off"
                    };
                    if icons::button(ui, Icon::Loop, self.looping, tip).clicked() {
                        self.looping = !self.looping;
                    }
                    let tip = if self.skip_selection {
                        "Skip selection: on"
                    } else {
                        "Skip selection: off"
                    };
                    if icons::button(ui, Icon::SkipSelection, self.skip_selection, tip).clicked() {
                        self.skip_selection = !self.skip_selection;
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
            },
        );

        // Zoom, right-aligned.
        let zoom_group = egui::Rect::from_min_size(
            egui::pos2(rect.right() - group_w(7) - 4.0, rect.top()),
            egui::vec2(group_w(7), rect.height()),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(zoom_group).layout(layout),
            |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.add_enabled_ui(has_file, |ui| {
                    for (icon, cmd, tip) in [
                        (
                            Icon::AmpZoomIn,
                            waveform::ZoomCmd::AmpIn,
                            "Zoom in amplitude",
                        ),
                        (
                            Icon::AmpZoomOut,
                            waveform::ZoomCmd::AmpOut,
                            "Zoom out amplitude",
                        ),
                        (
                            Icon::AmpReset,
                            waveform::ZoomCmd::AmpReset,
                            "Reset amplitude zoom",
                        ),
                        (Icon::TimeZoomIn, waveform::ZoomCmd::In, "Zoom in time"),
                        (Icon::TimeZoomOut, waveform::ZoomCmd::Out, "Zoom out time"),
                        (
                            Icon::ZoomSelection,
                            waveform::ZoomCmd::Selection,
                            "Zoom to selection",
                        ),
                        (Icon::ZoomFull, waveform::ZoomCmd::Full, "Zoom out full"),
                    ] {
                        if icons::button(ui, icon, false, tip).clicked() {
                            zoom = Some(cmd);
                        }
                    }
                });
            },
        );
        if zoom.is_some() {
            self.pending_zoom = zoom;
            ui.ctx().request_repaint();
        }
        // Holding rewind / fast-forward moves the playhead at 4x real time.
        if let Some(dir) = scrub {
            let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
            self.nudge(dir * 4.0 * dt);
            ui.ctx().request_repaint();
        }
    }

    /// Move to the next (or previous) marker relative to the playhead.
    fn jump_marker(&mut self, forward: bool) {
        let pos = self.playhead().round() as usize;
        let Some(f) = self.files.get_mut(self.active) else {
            return;
        };
        let target = if forward {
            f.doc().marker_after(pos)
        } else {
            f.doc().marker_before(pos)
        };
        if let Some(at) = target.and_then(|i| f.select_marker(i)) {
            if let Some(s) = &self.shared {
                s.seek(at);
            }
        }
    }

    /// Jump the playhead by `secs` without touching the selection.
    fn nudge(&mut self, secs: f64) {
        let (Some(shared), Some(f)) = (&self.shared, self.files.get_mut(self.active)) else {
            return;
        };
        let frames = f.doc().frames();
        let target = (shared.position() + secs * f.doc().sample_rate as f64)
            .clamp(0.0, frames as f64) as usize;
        shared.seek(target);
        f.reveal_request = Some(target);
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

const TRANSPORT_H: f32 = 36.0;

/// Re-open a closed panel next to the other non-editor panels, or in a new strip under the editor.
fn show_panel(dock: &mut DockState<Tab>, tab: Tab) {
    let target = dock
        .iter_all_tabs()
        .find(|(_, t)| **t != Tab::Editor)
        .map(|(p, _)| p);
    match target {
        Some(p) => {
            dock.set_focused_node_and_surface(egui_dock::NodePath {
                surface: p.surface,
                node: p.node,
            });
            dock.push_to_focused_leaf(tab);
        }
        None => {
            dock.main_surface_mut()
                .split_below(NodeIndex::root(), 0.82, vec![tab]);
        }
    }
}

struct Viewer<'a> {
    s: &'a mut State,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match (*tab, self.s.files.get(self.s.active)) {
            (Tab::Editor, Some(f)) => format!("Editor: {}", f.name).into(),
            _ => tab.title().into(),
        }
    }

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(tab.title())
    }

    fn is_closeable(&self, tab: &Tab) -> bool {
        *tab != Tab::Editor
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let s = &mut *self.s;
        match *tab {
            Tab::Editor => {
                let playhead = s.playhead();
                let playing = s.shared.as_ref().is_some_and(|sh| sh.is_playing());
                let follow = s.follow && playing;
                let zoom = s.pending_zoom.take();
                let wave_h = (ui.available_height() - TRANSPORT_H).max(60.0);
                let size = egui::vec2(ui.available_width(), wave_h);
                if let Some(f) = s.files.get_mut(s.active) {
                    let ev = ui
                        .allocate_ui(size, |ui| {
                            waveform::show(ui, f, playhead, follow, &mut s.display, zoom)
                        })
                        .inner;
                    if let (Some(at), Some(sh)) = (ev.seek, &s.shared) {
                        sh.seek(at);
                    }
                    if let Some(sh) = &s.shared {
                        sh.set_preview_gain(
                            ev.preview_gain_db.map_or(1.0, |db| 10f32.powf(db / 20.0)),
                        );
                    }
                } else {
                    ui.allocate_ui(size, |ui| {
                        ui.centered_and_justified(|ui| {
                            if ui
                                .button("Open an audio file…  (⌘O or drop a file here)")
                                .clicked()
                            {
                                s.pick_and_open();
                            }
                        });
                    });
                }
                s.transport_row(ui);
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
                s.meters.show_levels(ui, ch);
            }
            Tab::PhaseMeter => s.meters.show_phase(ui),
            Tab::Loudness => s.loudness.show(ui),
            Tab::MatchLoudness => {
                if let Some(req) = s.match_panel.show(ui, &s.files) {
                    if let Some(sh) = &s.shared {
                        sh.pause();
                    }
                    for i in req.files {
                        let label = format!("Match Loudness ({:.1} LUFS)", req.target);
                        s.apply_queue.push_back((
                            i,
                            label,
                            ApplyKind::MatchLoudness {
                                target: req.target,
                                ceiling: req.ceiling,
                            },
                        ));
                    }
                }
            }
            Tab::EffectsRack => {
                let has_file = !s.files.is_empty();
                if let Some(a) = s.rack_panel.show(ui, &mut s.preview, has_file) {
                    s.effect_action(a);
                }
            }
            Tab::Markers => {
                let fmt = s.display.time;
                if let Some(f) = s.files.get_mut(s.active) {
                    if let Some(MarkerAction::Seek(at)) = s.markers_panel.show(ui, f, fmt) {
                        if let Some(sh) = &s.shared {
                            sh.seek(at);
                        }
                    }
                } else {
                    ui.label(egui::RichText::new("No file open").weak());
                }
            }
            Tab::Selection => {
                let fmt = s.display.time;
                if let Some(f) = s.files.get_mut(s.active) {
                    s.selview_panel.show(ui, f, fmt);
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
        self.state.poll_recording(&ctx);
        let phase = self
            .state
            .shared
            .as_ref()
            .and_then(|sh| sh.phase_correlation());
        self.state.meters.update_phase(phase);

        egui::Panel::top("menu").show(ui, |ui| self.state.menu(ui, &mut self.dock));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(&self.state.status);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style().as_ref()))
                .show_inside(ui, &mut Viewer { s: &mut self.state });
        });
        self.state.dialogs(&ctx);
        self.state.effects_frame(&ctx);
        self.state.generator_frame(&ctx);
        self.state.sync_engine();
        if self.state.shared.as_ref().is_some_and(|s| s.is_playing()) {
            ctx.request_repaint();
        }
    }
}
