#[cfg(not(target_os = "macos"))]
use crate::effects_ui::effects_by_category;
use crate::effects_ui::{ApplyJob, ApplyKind, EffectAction, EffectDialog, Preview, RackPanel};
use crate::generate_ui::Generator;
use crate::icons::{self, Icon};
use crate::loudness_ui::{LoudnessPanel, MatchLoudnessPanel};
use crate::markers_panel::{MarkerAction, MarkersPanel};
use crate::meters::Meters;
use crate::ops::OpenFile;
#[cfg(not(target_os = "macos"))]
use crate::ops::ZeroAdjust;
use crate::scale;
use crate::selview_panel::SelViewPanel;
use crate::spectral::ViewMode;
#[cfg(not(target_os = "macos"))]
use crate::spectral::{self, ColorMap, FreqAxis};
use crate::view::format_time;
use crate::waveform;
use boothcraft_core::{Clipboards, Document, Rope, Selection};
use boothcraft_dsp::convert::Downmix;
#[cfg(not(target_os = "macos"))]
use boothcraft_dsp::StftWindow;
use boothcraft_engine::{Engine, Recorder, Shared, Take};
use boothcraft_io::{BitDepth, Format, WriteOptions};
use egui::{Event, Key, KeyboardShortcut, Modifiers};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{mpsc, Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        memory_budget_mb: u64,
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
    /// Most-recently-opened first, capped at [`RECENT_FILES_MAX`]; persisted across launches.
    recent_files: Vec<PathBuf>,
    /// Files currently decoding on a worker thread (see `open`/`poll_open_jobs`); more than one
    /// can run at a time (e.g. picking several files at once, or clicking through Recent quickly).
    open_jobs: Vec<OpenJob>,
}

/// Longest "File > Open Recent" list kept.
const RECENT_FILES_MAX: usize = 10;

/// Decodes one file on a worker thread so opening it (however long that takes) never blocks the
/// UI thread, however it was requested: the file dialog, drag-and-drop, Open Recent, or a path on
/// the command line.
struct OpenJob {
    path: PathBuf,
    progress: Arc<AtomicU32>,
    rx: mpsc::Receiver<Result<Document, String>>,
}

impl OpenJob {
    fn spawn(path: PathBuf) -> OpenJob {
        let progress = Arc::new(AtomicU32::new(0));
        let p = progress.clone();
        let (tx, rx) = mpsc::channel();
        let read_path = path.clone();
        std::thread::spawn(move || {
            let result = boothcraft_io::read_with_progress(&read_path, |done, total| {
                if let Some(total) = total.filter(|&t| t > 0) {
                    p.store(((done as f64 / total as f64) * 1000.0) as u32, Relaxed);
                }
            })
            .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        OpenJob { path, progress, rx }
    }

    /// 0.0..=1.0; stays at 0 for the whole open if the container doesn't declare a frame count.
    fn progress(&self) -> f32 {
        self.progress.load(Relaxed) as f32 / 1000.0
    }

    fn poll(&self) -> Option<Result<Document, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("decode thread ended unexpectedly".into()))
            }
        }
    }
}

pub struct BoothcraftApp {
    dock: DockState<Tab>,
    state: State,
    /// Removes the disk-backed block store's segment directory when the app closes.
    _cleanup: boothcraft_core::CleanupGuard,
    #[cfg(target_os = "macos")]
    native_menu: crate::native_menu::NativeMenu,
}

/// On macOS, prefer the system's own San Francisco fonts over egui's bundled default (Ubuntu-Light),
/// so Boothcraft's text matches the rest of the OS. Reads the files the OS already has installed
/// at their standard location, rather than embedding/redistributing them; falls back silently to
/// egui's defaults (other platforms, or a future macOS that moves them) if they can't be read.
#[cfg(target_os = "macos")]
fn install_native_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let faces: [(&str, &str, egui::FontFamily); 2] = [
        (
            "SF Pro",
            "/System/Library/Fonts/SFNS.ttf",
            egui::FontFamily::Proportional,
        ),
        (
            "SF Mono",
            "/System/Library/Fonts/SFNSMono.ttf",
            egui::FontFamily::Monospace,
        ),
    ];
    for (name, path, family) in faces {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(data)));
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.to_owned());
    }
    ctx.set_fonts(fonts);
}

#[cfg(not(target_os = "macos"))]
fn install_native_fonts(_ctx: &egui::Context) {}

/// Storage keys for `eframe`'s persistence (only active with the "persistence" feature, which
/// both `boothcraft-app` and `boothcraft-ui` enable on `eframe`). Window position/size persist
/// automatically via `eframe`'s own mechanism; these two are ours.
const DOCK_STORAGE_KEY: &str = "boothcraft-dock";
const RECENT_FILES_STORAGE_KEY: &str = "boothcraft-recent-files";

/// The default panel layout: a full-height left column (Files above Effects Rack), and to its
/// right the editor on top with Selection/View + Markers, Phase Meter + Loudness and Levels
/// underneath. The split fraction is the share kept by the *existing* node for
/// `split_below`/`split_right`, but — despite the method docs implying otherwise — the share
/// given to the *new* node for `split_left`/`split_above` (confirmed against egui_dock 0.21's own
/// layout code).
fn default_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Editor]);
    let surface = dock.main_surface_mut();
    let [right, files] = surface.split_left(NodeIndex::root(), 1.0 / 3.0, vec![Tab::Files]);
    surface.split_below(files, 0.5, vec![Tab::EffectsRack]);
    let [_editor, bottom] = surface.split_below(right, 0.82, vec![Tab::Selection, Tab::Markers]);
    let [_selection, rest] = surface.split_right(bottom, 0.2, vec![Tab::PhaseMeter, Tab::Loudness]);
    surface.split_right(rest, 0.25, vec![Tab::Levels]);
    dock
}

impl BoothcraftApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> BoothcraftApp {
        install_native_fonts(&cc.egui_ctx);
        let cleanup = boothcraft_core::install_cleanup_guard();
        let dock = cc
            .storage
            .and_then(|s| eframe::get_value(s, DOCK_STORAGE_KEY))
            .unwrap_or_else(default_dock);
        let recent_files = cc
            .storage
            .and_then(|s| eframe::get_value(s, RECENT_FILES_STORAGE_KEY))
            .unwrap_or_default();

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
            recent_files,
            open_jobs: Vec::new(),
        };
        for p in paths {
            state.open(&p);
        }
        BoothcraftApp {
            dock,
            state,
            _cleanup: cleanup,
            #[cfg(target_os = "macos")]
            native_menu: crate::native_menu::install(),
        }
    }
}

impl State {
    /// Starts decoding `path` on a worker thread; see `poll_open_jobs` for where it lands once
    /// done. Never blocks, regardless of the file's size.
    fn open(&mut self, path: &Path) {
        let name = path
            .file_name()
            .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
        self.status = format!("Opening {name}…");
        self.open_jobs.push(OpenJob::spawn(path.to_path_buf()));
    }

    /// Picks up any finished (or failed) decode started by `open`, adding it as a new open file.
    /// Call once per frame.
    fn poll_open_jobs(&mut self, ctx: &egui::Context) {
        if self.open_jobs.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.open_jobs.len() {
            match self.open_jobs[i].poll() {
                None => i += 1,
                Some(result) => {
                    let job = self.open_jobs.remove(i);
                    let name = job
                        .path
                        .file_name()
                        .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
                    match result {
                        Ok(doc) => {
                            self.status = format!(
                                "Opened {name}: {} ch, {} Hz, {}",
                                doc.channel_count(),
                                doc.sample_rate,
                                format_time(doc.frames() as f64, doc.sample_rate)
                            );
                            self.files
                                .push(OpenFile::new(name, Some(job.path.clone()), doc));
                            self.set_active(self.files.len() - 1);
                            self.remember_recent(&job.path);
                        }
                        Err(e) => {
                            self.status = format!("Could not open {}: {e}", job.path.display())
                        }
                    }
                }
            }
        }
        if !self.open_jobs.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Move `path` to the front of the recent-files list (adding it if new), capped at
    /// [`RECENT_FILES_MAX`].
    fn remember_recent(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(RECENT_FILES_MAX);
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

    /// Close the open file at `i` (used by File > Close for the active file, and by the Files
    /// panel's right-click Close). Does nothing if `i` is out of range.
    fn close_file(&mut self, i: usize) {
        if i >= self.files.len() {
            return;
        }
        self.files.remove(i);
        if self.active >= i {
            self.active = self.active.saturating_sub(1);
        }
        self.synced = None;
        self.stop();
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
        // Lets the playback prefetch thread keep the selection start warm even while paused, in
        // case the user presses Play next.
        shared.set_selection_hint(sel.start);
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
        let toggle_view_mode = !typing && keys(Modifiers::SHIFT, Key::D);
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
        if toggle_view_mode {
            let spectral = &mut self.display.spectral;
            spectral.mode = if spectral.mode == ViewMode::Waveform {
                ViewMode::Frequency
            } else {
                ViewMode::Waveform
            };
            self.status = format!("View: {}", spectral.mode.name());
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

    /// Handle a click from the native macOS menu bar, mirroring exactly what the corresponding
    /// button closure in `menu()` does (that function is still used, unchanged, on other
    /// platforms; this is its native-menu equivalent).
    #[cfg(target_os = "macos")]
    fn dispatch_native_action(
        &mut self,
        dock: &mut DockState<Tab>,
        action: crate::native_menu::Action,
    ) {
        use crate::native_menu::Action;
        match action {
            Action::NewFile => {
                self.dialog = Some(Dialog::NewFile {
                    rate: 48_000,
                    channels: 2,
                });
            }
            Action::Open => self.pick_and_open(),
            Action::OpenRecent(i) => {
                if let Some(path) = self.recent_files.get(i).cloned() {
                    self.open(&path);
                }
            }
            Action::ClearRecent => self.recent_files.clear(),
            Action::Save => self.save(false),
            Action::SaveAs => self.save(true),
            Action::Close => {
                if self.active < self.files.len() {
                    self.files.remove(self.active);
                    self.active = self.active.saturating_sub(1);
                    self.synced = None;
                    self.stop();
                }
            }
            Action::Undo => {
                if let Some(f) = self.files.get_mut(self.active) {
                    f.undo();
                }
            }
            Action::Redo => {
                if let Some(f) = self.files.get_mut(self.active) {
                    f.redo();
                }
            }
            Action::Cut => {
                let clips = &mut self.clips;
                if let Some(f) = self.files.get_mut(self.active) {
                    f.cut(clips);
                }
            }
            Action::Copy => {
                let clips = &mut self.clips;
                if let Some(f) = self.files.get_mut(self.active) {
                    f.copy(clips);
                }
            }
            Action::Paste => {
                let clips = &mut self.clips;
                if let Some(f) = self.files.get_mut(self.active) {
                    f.paste(clips);
                }
            }
            Action::MixPaste => {
                let clips = &mut self.clips;
                if let Some(f) = self.files.get_mut(self.active) {
                    f.mix_paste(clips);
                }
            }
            Action::Delete => {
                if let Some(f) = self.files.get_mut(self.active) {
                    f.delete();
                }
            }
            Action::CropToSelection => {
                if let Some(f) = self.files.get_mut(self.active) {
                    f.crop();
                }
            }
            Action::SelectAll => {
                if let Some(f) = self.files.get_mut(self.active) {
                    f.select_all();
                }
            }
            Action::ZeroAdjust(mode) => {
                if let Some(f) = self.files.get_mut(self.active) {
                    f.adjust_zero_crossings(mode);
                }
            }
            Action::ToggleSnapEnabled => self.display.snap.enabled = !self.display.snap.enabled,
            Action::ToggleSnapMarkers => self.display.snap.markers = !self.display.snap.markers,
            Action::ToggleSnapRuler => self.display.snap.ruler = !self.display.snap.ruler,
            Action::ToggleSnapZeroCrossings => {
                self.display.snap.zero_crossings = !self.display.snap.zero_crossings;
            }
            Action::ConvertSampleType => {
                if let Some(f) = self.files.get(self.active) {
                    let (rate, channels) = (f.doc().sample_rate, f.doc().channel_count().min(2));
                    self.dialog = Some(Dialog::ConvertSampleType {
                        rate,
                        channels,
                        downmix: Downmix::Mix,
                    });
                }
            }
            Action::Preferences => {
                self.dialog = Some(Dialog::Preferences {
                    preroll: self.preroll_secs,
                    postroll: self.postroll_secs,
                    memory_budget_mb: boothcraft_core::BlockStore::global().budget_mb(),
                });
            }
            Action::SelectClipboard(i) => self.clips.active = i,
            Action::SetViewMode(mode) => self.display.spectral.mode = mode,
            Action::SetFftSize(size) => self.display.spectral.fft_size = size,
            Action::SetWindow(w) => self.display.spectral.window = w,
            Action::SetColorMap(m) => self.display.spectral.color_map = m,
            Action::SetFreqAxis(a) => self.display.spectral.freq_axis = a,
            Action::ResetLayout => *dock = default_dock(),
            Action::Invert => {
                if self.active < self.files.len() {
                    self.files[self.active].invert();
                }
            }
            Action::Reverse => {
                if self.active < self.files.len() {
                    self.files[self.active].reverse();
                }
            }
            Action::Silence => {
                if self.active < self.files.len() {
                    self.files[self.active].silence();
                }
            }
            Action::InsertSilence | Action::GenSilence => {
                self.dialog = Some(Dialog::InsertSilence(1.0));
            }
            Action::OpenEffect(id) => self.effect_dialog = EffectDialog::new(id),
            Action::GenTones => self.generator = Some(Generator::tone()),
            Action::GenNoise => self.generator = Some(Generator::noise()),
            Action::GenDtmf => self.generator = Some(Generator::dtmf()),
            Action::TogglePanel(i) => {
                if let Some(&tab) = WINDOW_TABS.get(i) {
                    match dock.find_tab(&tab) {
                        Some(path) => {
                            dock.remove_tab(path);
                        }
                        None => show_panel(dock, tab),
                    }
                }
            }
        }
    }

    /// The in-window menu bar, used on every platform except macOS (which gets a native menu bar
    /// instead — see `dispatch_native_action` and `native_menu`).
    #[cfg(not(target_os = "macos"))]
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
                ui.menu_button("Open Recent", |ui| {
                    if self.recent_files.is_empty() {
                        ui.add_enabled(false, egui::Button::new("No Recent Files"));
                    } else {
                        for path in self.recent_files.clone() {
                            let label = path.file_name().map_or_else(
                                || path.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            if ui
                                .button(label)
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                self.open(&path);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear Recent").clicked() {
                            self.recent_files.clear();
                            ui.close();
                        }
                    }
                });
                if ui.button("Save").clicked() {
                    self.save(false);
                    ui.close();
                }
                if ui.button("Save As…").clicked() {
                    self.save(true);
                    ui.close();
                }
                if ui.button("Close").clicked() {
                    self.close_file(self.active);
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
                        memory_budget_mb: boothcraft_core::BlockStore::global().budget_mb(),
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
            ui.menu_button("View", |ui| {
                for mode in ViewMode::ALL {
                    if ui
                        .radio(self.display.spectral.mode == mode, mode.name())
                        .clicked()
                    {
                        self.display.spectral.mode = mode;
                        ui.close();
                    }
                }
                ui.separator();
                ui.menu_button("Spectral Settings", |ui| {
                    let spectral = &mut self.display.spectral;
                    ui.horizontal(|ui| {
                        ui.label("FFT size:");
                        egui::ComboBox::new("spectral_fft_size", "")
                            .selected_text(spectral.fft_size.to_string())
                            .show_ui(ui, |ui| {
                                for size in spectral::FFT_SIZES {
                                    ui.selectable_value(
                                        &mut spectral.fft_size,
                                        size,
                                        size.to_string(),
                                    );
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Window:");
                        ui.radio_value(&mut spectral.window, StftWindow::Hann, "Hann");
                        ui.radio_value(
                            &mut spectral.window,
                            StftWindow::BlackmanHarris,
                            "Blackman-Harris",
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Colors:");
                        for m in ColorMap::ALL {
                            ui.radio_value(&mut spectral.color_map, m, m.name());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Frequency axis:");
                        for a in FreqAxis::ALL {
                            ui.radio_value(&mut spectral.freq_axis, a, a.name());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("dB range:");
                        ui.add(
                            egui::DragValue::new(&mut spectral.db_floor)
                                .range(-140.0..=-20.0)
                                .suffix(" dB"),
                        );
                    });
                });
                ui.separator();
                if ui.button("Reset Panel Layout").clicked() {
                    *dock = default_dock();
                    ui.close();
                }
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
                for tab in WINDOW_TABS {
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
                    Dialog::Preferences { preroll, postroll, memory_budget_mb } => {
                        egui::Grid::new("prefs").num_columns(2).show(ui, |ui| {
                            ui.label("Pre-roll (s)");
                            ui.add(egui::DragValue::new(preroll).range(0.0..=30.0).speed(0.05));
                            ui.end_row();
                            ui.label("Post-roll (s)");
                            ui.add(egui::DragValue::new(postroll).range(0.0..=30.0).speed(0.05));
                            ui.end_row();
                            ui.label("Memory budget (MB)").on_hover_text(
                                "Audio beyond this is spilled to a disk cache and reloaded as needed, so very long or large files stay editable.",
                            );
                            ui.add(egui::DragValue::new(memory_budget_mb).range(64..=65536).speed(16));
                            ui.end_row();
                            ui.label("Disk cache");
                            ui.label(
                                egui::RichText::new(boothcraft_core::BlockStore::global().cache_dir().display().to_string())
                                    .weak()
                                    .small(),
                            );
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
                Some(Dialog::Preferences {
                    preroll,
                    postroll,
                    memory_budget_mb,
                }) => {
                    self.preroll_secs = preroll;
                    self.postroll_secs = postroll;
                    boothcraft_core::BlockStore::global().set_budget_mb(memory_budget_mb);
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

/// The dockable, non-`Editor` panels offered by the Window menu, in menu order. On macOS this
/// doubles as the native menu's panel list (`native_menu::PANEL_LABELS`, same order).
const WINDOW_TABS: [Tab; 9] = [
    Tab::Files,
    Tab::History,
    Tab::Selection,
    Tab::Markers,
    Tab::EffectsRack,
    Tab::PhaseMeter,
    Tab::Loudness,
    Tab::MatchLoudness,
    Tab::Levels,
];

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
                                .button("Open an audio file…  (Cmd+O or drop a file here)")
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
                let mut close = None;
                for (i, f) in s.files.iter().enumerate() {
                    let label = format!("{}{}", f.name, if f.dirty() { " •" } else { "" });
                    let resp = ui.selectable_label(i == s.active, label);
                    if resp.clicked() {
                        pick = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Close").clicked() {
                            close = Some(i);
                            ui.close();
                        }
                    });
                }
                if let Some(i) = close {
                    s.close_file(i);
                } else if let Some(i) = pick {
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

        #[cfg(target_os = "macos")]
        {
            for action in self.native_menu.poll() {
                self.state.dispatch_native_action(&mut self.dock, action);
            }
            let panels_open: Vec<bool> = WINDOW_TABS
                .iter()
                .map(|tab| self.dock.find_tab(tab).is_some())
                .collect();
            let active = self.state.files.get(self.state.active);
            self.native_menu.sync(&crate::native_menu::SyncState {
                recording: self.state.recording.is_some(),
                has_file: !self.state.files.is_empty(),
                can_undo: active.is_some_and(|f| f.history.can_undo()),
                can_redo: active.is_some_and(|f| f.history.can_redo()),
                has_selection: active.is_some_and(|f| !f.selection.is_empty()),
                has_clip: self.state.clips.active_clip().is_some(),
                snap_enabled: self.state.display.snap.enabled,
                snap_markers: self.state.display.snap.markers,
                snap_ruler: self.state.display.snap.ruler,
                snap_zero_crossings: self.state.display.snap.zero_crossings,
                active_clipboard: self.state.clips.active,
                view_mode: self.state.display.spectral.mode,
                fft_size: self.state.display.spectral.fft_size,
                window: self.state.display.spectral.window,
                color_map: self.state.display.spectral.color_map,
                freq_axis: self.state.display.spectral.freq_axis,
                panels_open: &panels_open,
                recent_files: &self.state.recent_files,
            });
        }
        #[cfg(not(target_os = "macos"))]
        egui::Panel::top("menu").show(ui, |ui| self.state.menu(ui, &mut self.dock));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.state.status);
                for job in &self.state.open_jobs {
                    ui.add(
                        egui::ProgressBar::new(job.progress())
                            .desired_width(80.0)
                            .show_percentage(),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let store = boothcraft_core::BlockStore::global();
                    let mb = |b: u64| b / (1024 * 1024);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} MB resident, {} MB on disk",
                            mb(store.resident_bytes()),
                            mb(store.disk_bytes())
                        ))
                        .weak()
                        .small(),
                    );
                });
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style().as_ref()))
                .show_inside(ui, &mut Viewer { s: &mut self.state });
        });
        self.state.dialogs(&ctx);
        self.state.poll_open_jobs(&ctx);
        self.state.effects_frame(&ctx);
        self.state.generator_frame(&ctx);
        self.state.sync_engine();
        if self.state.shared.as_ref().is_some_and(|s| s.is_playing()) {
            ctx.request_repaint();
        }
    }

    /// Persists panel placement and the recent-files list; window position/size are handled by
    /// `eframe` itself. Only takes effect with the "persistence" feature enabled on `eframe`
    /// (it is, for both `boothcraft-app` and `boothcraft-ui`).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, DOCK_STORAGE_KEY, &self.dock);
        eframe::set_value(storage, RECENT_FILES_STORAGE_KEY, &self.state.recent_files);
    }
}
