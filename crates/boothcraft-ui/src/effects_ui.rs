//! The Effects Rack panel, single-effect dialogs, real-time preview and background Apply.

use crate::ops::OpenFile;
use crate::param_ui::{effect_editor, envelope_editor};
use boothcraft_core::{ChannelMask, Rope, Selection};
use boothcraft_dsp::preset::{self, Preset};
use boothcraft_dsp::rack::{render_rope, render_rope_with, OFFLINE_BLOCK, SLOTS};
use boothcraft_dsp::registry::{self, Category, EFFECTS};
use boothcraft_dsp::{
    Envelope, ParamSpec, Rack, RackConfig, RackDesc, SlotDesc, SlotError, SlotId,
};
use boothcraft_engine::{RackControl, RackMsg};
use boothcraft_plugin::PluginManager;
use egui::{RichText, Ui};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{mpsc, Arc};

/// Who is using the engine's single preview rack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Rack,
    Dialog,
}

/// Wraps the engine's preview-rack control: installs prepared racks and forwards tweaks.
pub struct Preview {
    ctl: Option<RackControl>,
    owner: Option<Owner>,
    /// Channels of the playing document. The preview rack runs on these (up to the device's
    /// count), before they are mapped onto the outputs, so it matches what Apply produces.
    doc_channels: usize,
    /// The rack last installed, to rebuild it if the document's channel count changes.
    installed: Option<RackDesc>,
    /// Hosted plugins, if enabled. Lives here because every preview rack goes through it.
    pub plugins: Option<PluginManager>,
    /// Plugin slots of the installed rack that could not be built, by slot index.
    pub build_errors: Vec<(usize, SlotError)>,
}

impl Preview {
    pub fn new(ctl: Option<RackControl>, plugins: Option<PluginManager>) -> Preview {
        Preview {
            ctl,
            owner: None,
            doc_channels: 2,
            installed: None,
            plugins,
            build_errors: Vec::new(),
        }
    }

    /// Tell the preview how many channels the active document has.
    pub fn set_doc_channels(&mut self, channels: usize) {
        let channels = channels.max(1);
        if channels != self.doc_channels {
            self.doc_channels = channels;
            if let (Some(_), Some(desc)) = (self.owner, self.installed.clone()) {
                self.install(&desc);
            }
        }
    }

    pub fn available(&self) -> bool {
        self.ctl.is_some()
    }

    pub fn is_active(&self, owner: Owner) -> bool {
        self.owner == Some(owner)
    }

    pub fn start(&mut self, owner: Owner, desc: &RackDesc) {
        self.owner = Some(owner);
        self.install(desc);
        self.send(RackMsg::Power(true));
    }

    pub fn stop(&mut self, owner: Owner) {
        if self.is_active(owner) {
            self.send(RackMsg::Power(false));
            self.send(RackMsg::Clear);
            if let Some(m) = &mut self.plugins {
                m.preview_cleared();
            }
            self.build_errors.clear();
            self.installed = None;
            self.owner = None;
        }
    }

    /// Rebuild after a structural change (effect added, removed, moved, preset loaded).
    pub fn reinstall(&mut self, owner: Owner, desc: &RackDesc) {
        if self.is_active(owner) {
            self.install(desc);
        }
    }

    pub fn param(&mut self, owner: Owner, slot: usize, index: usize, value: f32) {
        if self.is_active(owner) {
            self.send(RackMsg::Param {
                slot: slot as u8,
                index: index as u16,
                value,
            });
        }
    }

    pub fn bypass(&mut self, owner: Owner, slot: usize, on: bool) {
        if self.is_active(owner) {
            self.send(RackMsg::Bypass {
                slot: slot as u8,
                on,
            });
        }
    }

    pub fn gains(&mut self, owner: Owner, desc: &RackDesc) {
        if self.is_active(owner) {
            self.send(RackMsg::Gains {
                input_db: desc.input_gain_db,
                output_db: desc.output_gain_db,
                mix: desc.mix,
            });
        }
    }

    /// Take back racks the audio thread has handed back (plugins return to their host rather
    /// than being dropped), and act on what plugins asked for. Call once per frame.
    pub fn tick(&mut self) {
        if let Some(c) = &mut self.ctl {
            match &mut self.plugins {
                Some(m) => {
                    for rack in c.drain_trash() {
                        m.retire(rack);
                    }
                }
                None => c.collect_garbage(),
            }
        }
        let rebuild = self.plugins.as_mut().is_some_and(|m| m.pump().rebuild);
        if rebuild && self.owner.is_some() {
            if let Some(desc) = self.installed.clone() {
                self.install(&desc);
            }
        }
    }

    fn install(&mut self, desc: &RackDesc) {
        let Some(c) = &mut self.ctl else {
            return;
        };
        let channels = self.doc_channels.min(c.channels).max(1);
        match &mut self.plugins {
            Some(m) => {
                let cfg = RackConfig {
                    sample_rate: c.sample_rate as f32,
                    channels,
                    max_block: 1024,
                };
                let b = m.build_preview(desc, &cfg);
                c.send(RackMsg::Rebuild {
                    rack: b.rack,
                    carry: b.carry,
                });
                self.build_errors = b.errors;
            }
            None => {
                let rack = Rack::new(desc, c.sample_rate as f32, channels, 1024);
                c.send(RackMsg::Install(Box::new(rack)));
            }
        }
        self.installed = Some(desc.clone());
    }

    /// Load `slot`'s plugin so its parameters can be shown and edited. `Err` says why not.
    pub fn ensure_loaded(&mut self, slot: &SlotDesc) -> Result<(), SlotError> {
        match &mut self.plugins {
            Some(m) => m.ensure_loaded(slot),
            None => Err(SlotError::Missing(slot.effect.clone())),
        }
    }

    /// The loaded plugin's parameter specs and its current values.
    pub fn plugin_params(&self, slot: SlotId) -> Option<(Vec<ParamSpec>, Vec<f32>)> {
        let m = self.plugins.as_ref()?;
        let (specs, _) = m.specs(slot)?;
        let values = m.params(slot)?.iter().map(|p| p.current).collect();
        Some((specs.to_vec(), values))
    }

    /// A generic-UI edit of a plugin parameter (by index): the plugin's controller hears it now,
    /// and the audio thread's processor too when this owner's preview is on.
    pub fn plugin_param(
        &mut self,
        owner: Owner,
        slot_index: usize,
        slot: SlotId,
        index: usize,
        value: f32,
    ) {
        if let Some(m) = &mut self.plugins {
            let id = m.specs(slot).and_then(|(_, ids)| ids.get(index).copied());
            if let Some(id) = id {
                m.edit_param(slot, id, value);
            }
        }
        self.param(owner, slot_index, index, value);
    }

    /// Record every plugin's real state into `desc` (before saving a rack or applying it).
    pub fn snapshot(&mut self, desc: &mut RackDesc) {
        if let Some(m) = &mut self.plugins {
            m.snapshot(desc);
        }
    }

    /// Build the Apply rack for a rack with plugins: new offline instances, primed, on this (the
    /// main) thread. Hand the result to an [`ApplyJob`] and give it back through
    /// [`Preview::finish_apply`] afterwards. Never skips a slot that failed to build.
    pub fn prepare_apply(
        &mut self,
        desc: &mut RackDesc,
        sample_rate: f32,
        channels: usize,
    ) -> Result<Box<Rack>, String> {
        let m = self.plugins.as_mut().ok_or("plugins are not enabled")?;
        m.snapshot(desc);
        let cfg = RackConfig {
            sample_rate,
            channels,
            max_block: OFFLINE_BLOCK,
        };
        match m.prepare_offline(desc, &cfg).poll() {
            Some(Ok((rack, errors))) if errors.is_empty() => Ok(rack),
            Some(Ok((rack, errors))) => {
                m.finish_offline(rack);
                let why: Vec<String> = errors
                    .iter()
                    .map(|(i, e)| format!("slot {}: {e}", i + 1))
                    .collect();
                Err(why.join("; "))
            }
            Some(Err(e)) => Err(e.to_string()),
            None => Err("a plugin is still loading; try again in a moment".into()),
        }
    }

    /// Tear down an Apply rack on the main thread.
    pub fn finish_apply(&mut self, rack: Box<Rack>) {
        if let Some(m) = &mut self.plugins {
            m.finish_offline(rack);
        }
    }

    /// Keep only plugins still wanted by a rack the UI holds; the rest are torn down.
    pub fn sweep_plugins(&mut self, keep: &std::collections::HashSet<SlotId>) {
        if let Some(m) = &mut self.plugins {
            m.sweep(keep);
        }
    }

    /// App shutdown: silence the preview rack, collect everything the audio thread returns,
    /// then release plugins on this thread before the engine goes away.
    pub fn shutdown(&mut self) {
        let rack_playing = self.installed.is_some();
        self.send(RackMsg::Power(false));
        self.send(RackMsg::Clear);
        let mut retired = Vec::new();
        if let Some(c) = &mut self.ctl {
            retired.extend(c.drain_trash());
            // The audio callback takes the Clear on its next period; wait briefly for the rack
            // it was playing (about 500 ms at most, and not at all if none was).
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while rack_playing && retired.is_empty() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
                retired.extend(c.drain_trash());
            }
        }
        if let Some(m) = &mut self.plugins {
            m.shutdown(retired);
        }
    }

    fn send(&mut self, msg: RackMsg) {
        if let Some(c) = &mut self.ctl {
            c.send(msg);
        }
    }
}

/// Categories in menu order, each with its effects (real-time only when `realtime_only`).
pub fn effects_by_category(
    realtime_only: bool,
) -> Vec<(Category, Vec<&'static registry::EffectInfo>)> {
    let mut out: Vec<(Category, Vec<&'static registry::EffectInfo>)> = Vec::new();
    for e in EFFECTS.iter().filter(|e| !realtime_only || e.is_realtime()) {
        match out.iter_mut().find(|(c, _)| *c == e.category) {
            Some((_, list)) => list.push(e),
            None => out.push((e.category, vec![e])),
        }
    }
    out
}

/// What applying an effect means.
pub enum ApplyKind {
    Rack(RackDesc),
    /// A rack already built on the main thread (it holds plugins). Comes back through
    /// [`ApplyJob::take_rack`] to be torn down there.
    Prepared(Box<Rack>),
    Offline {
        id: &'static str,
        values: Vec<f32>,
        envelope: Option<Envelope>,
    },
    /// Whole file, all channels, to a loudness target with optional peak limiting.
    MatchLoudness {
        target: f64,
        ceiling: Option<f32>,
    },
}

/// What an [`ApplyJob`] produced, ready to be spliced or copied back into the document.
pub enum ApplyResult {
    /// The whole selection, pre-materialized as flat per-channel buffers (the offline-effect and
    /// Match Loudness paths, which need the whole range in memory for their own analysis anyway).
    Flat(Vec<Vec<f32>>),
    /// The whole selection as ropes, built up and processed in chunks without ever materializing
    /// as one buffer (the real-time rack path, so a selection spanning most or all of a large,
    /// disk-backed file doesn't need a second full-size in-memory copy).
    Ropes(Vec<Rope>),
}

/// An effect being rendered over a file's selection on a worker thread.
pub struct ApplyJob {
    pub file: usize,
    pub label: String,
    range: Range<usize>,
    channels: Vec<usize>,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<ApplyMsg>,
    /// The prepared rack, once the worker returns it.
    returned_rack: std::cell::RefCell<Option<Box<Rack>>>,
}

/// What the worker sends back: the audio (`None` = cancelled or failed) and any prepared rack.
struct ApplyMsg {
    result: Option<ApplyResult>,
    rack: Option<Box<Rack>>,
}

impl ApplyJob {
    /// Render the selection (or the whole file) of the enabled channels.
    pub fn start(file_index: usize, file: &OpenFile, label: String, kind: ApplyKind) -> ApplyJob {
        let doc = file.doc();
        let range = if file.selection.is_empty() {
            0..doc.frames()
        } else {
            file.selection.range()
        };
        let channels = Self::enabled_channels(file);
        Self::spawn(file_index, file, label, kind, range, channels)
    }

    /// The channels an Apply on this file renders: those enabled for editing.
    pub fn enabled_channels(file: &OpenFile) -> Vec<usize> {
        (0..file.doc().channel_count())
            .filter(|c| file.mask.has(*c))
            .collect()
    }

    /// Render the whole file on every channel (Match Loudness).
    pub fn start_whole(
        file_index: usize,
        file: &OpenFile,
        label: String,
        kind: ApplyKind,
    ) -> ApplyJob {
        let doc = file.doc();
        Self::spawn(
            file_index,
            file,
            label,
            kind,
            0..doc.frames(),
            (0..doc.channel_count()).collect(),
        )
    }

    fn spawn(
        file_index: usize,
        file: &OpenFile,
        label: String,
        kind: ApplyKind,
        range: Range<usize>,
        channels: Vec<usize>,
    ) -> ApplyJob {
        let doc = file.doc();
        let rate = doc.sample_rate as f32;
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (p, c) = (progress.clone(), cancel.clone());

        // The rack path reads and writes directly through ropes in chunks (see `render_rope`), so
        // a selection spanning most or all of a large file is never copied into one flat buffer.
        // Cloning a rope is cheap (it shares blocks structurally; O(blocks), not O(samples)).
        // The other kinds need their whole range as flat buffers anyway for their own analysis
        // (a peak scan, an LUFS pass, …), so they keep materializing it up front.
        if matches!(kind, ApplyKind::Rack(_) | ApplyKind::Prepared(_)) {
            let ropes: Vec<Rope> = channels
                .iter()
                .map(|&ch| doc.channels[ch].clone())
                .collect();
            let render_range = range.clone();
            std::thread::spawn(move || {
                let msg = match kind {
                    ApplyKind::Rack(desc) => ApplyMsg {
                        result: render_rope(&desc, &ropes, render_range, rate, &p, &c)
                            .map(ApplyResult::Ropes),
                        rack: None,
                    },
                    ApplyKind::Prepared(mut rack) => ApplyMsg {
                        result: render_rope_with(&mut rack, &ropes, render_range, &p, &c)
                            .map(ApplyResult::Ropes),
                        // The plugins inside must be torn down on the main thread.
                        rack: Some(rack),
                    },
                    _ => unreachable!("checked above"),
                };
                let _ = tx.send(msg);
            });
            return ApplyJob {
                file: file_index,
                label,
                range,
                channels,
                progress,
                cancel,
                rx,
                returned_rack: Default::default(),
            };
        }

        let mut chans: Vec<Vec<f32>> = channels
            .iter()
            .map(|&c| doc.channels[c].to_vec(range.clone()))
            .collect();
        std::thread::spawn(move || {
            let ok = match kind {
                ApplyKind::Rack(_) | ApplyKind::Prepared(_) => unreachable!("handled above"),
                ApplyKind::MatchLoudness { target, ceiling } => {
                    let applied = boothcraft_dsp::loudness::match_loudness(
                        &mut chans,
                        rate as u32,
                        target,
                        ceiling,
                    );
                    p.store(1000, Relaxed);
                    applied.is_some()
                }
                ApplyKind::Offline {
                    id,
                    values,
                    envelope,
                } => match registry::create_offline(id) {
                    Some(mut fx) => {
                        let specs = fx.specs();
                        for ((v, s), new) in fx.values_mut().iter_mut().zip(specs).zip(&values) {
                            *v = s.clamp(*new);
                        }
                        if let (Some(env), Some(dst)) = (envelope, fx.envelope_mut()) {
                            *dst = env;
                        }
                        fx.process_offline(&mut chans, rate);
                        p.store(1000, Relaxed);
                        true
                    }
                    None => false,
                },
            };
            let _ = tx.send(ApplyMsg {
                result: ok.then_some(ApplyResult::Flat(chans)),
                rack: None,
            });
        });
        ApplyJob {
            file: file_index,
            label,
            range,
            channels,
            progress,
            cancel,
            rx,
            returned_rack: Default::default(),
        }
    }

    pub fn progress(&self) -> f32 {
        self.progress.load(Relaxed) as f32 / 1000.0
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    /// `Some(result)` once the worker has finished (`None` inside means cancelled or failed).
    pub fn poll(&self) -> Option<Option<ApplyResult>> {
        match self.rx.try_recv() {
            Ok(msg) => {
                *self.returned_rack.borrow_mut() = msg.rack;
                Some(msg.result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(None),
        }
    }

    /// The prepared rack, once `poll` has reported the job finished, for the main thread to
    /// hand to [`Preview::finish_apply`].
    pub fn take_rack(&self) -> Option<Box<Rack>> {
        self.returned_rack.borrow_mut().take()
    }

    /// Write the rendered audio back as one undoable step.
    pub fn commit(&self, file: &mut OpenFile, result: ApplyResult) {
        let mut d = file.doc().clone();
        match result {
            ApplyResult::Flat(result) => {
                let mut mask = 0u32;
                for &c in &self.channels {
                    mask |= 1 << c;
                }
                let channels = &self.channels;
                d.apply(self.range.clone(), ChannelMask(mask), |c, s| {
                    if let Some(k) = channels.iter().position(|&x| x == c) {
                        let src = &result[k];
                        let n = s.len().min(src.len());
                        s[..n].copy_from_slice(&src[..n]);
                    }
                });
            }
            ApplyResult::Ropes(ropes) => {
                // Splice the processed audio in directly: cheap, structural-sharing edits on the
                // affected channels rather than a `map_range`-style whole-range copy. Markers need
                // no adjustment, since the replacement is exactly as long as what it replaces.
                for (&c, replacement) in self.channels.iter().zip(&ropes) {
                    d.channels[c].delete(self.range.clone());
                    d.channels[c].insert(self.range.start, replacement);
                }
            }
        }
        file.commit(&self.label, d);
        file.selection =
            Selection::new(self.range.start, self.range.end).clamp(file.doc().frames());
    }
}

/// What a panel or dialog asks the app to do.
pub enum EffectAction {
    Apply {
        label: String,
        kind: ApplyKind,
    },
    /// Start or stop playback for preview.
    TogglePlayback,
}

/// The Effects Rack panel.
#[derive(Default)]
pub struct RackPanel {
    pub desc: RackDesc,
    selected: Option<usize>,
}

impl RackPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        preview: &mut Preview,
        has_file: bool,
    ) -> Option<EffectAction> {
        let mut action = None;
        let mut structural = false;
        ui.horizontal(|ui| {
            let mut on = preview.is_active(Owner::Rack);
            let tip = if preview.available() {
                "Hear the rack while playing"
            } else {
                "No audio output device"
            };
            if ui
                .add_enabled(
                    preview.available(),
                    egui::Checkbox::new(&mut on, "Power (preview)"),
                )
                .on_hover_text(tip)
                .changed()
            {
                if on {
                    preview.stop(Owner::Dialog);
                    preview.start(Owner::Rack, &self.desc);
                } else {
                    preview.stop(Owner::Rack);
                }
            }
            ui.menu_button("Rack Presets", |ui| {
                if ui.button("Save Rack…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Boothcraft rack", &["json"])
                        .set_file_name("rack.json")
                        .save_file()
                    {
                        preview.snapshot(&mut self.desc);
                        let _ = preset::save_rack(&path, &self.desc);
                    }
                    ui.close();
                }
                if ui.button("Load Rack…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Boothcraft rack", &["json"])
                        .pick_file()
                    {
                        if let Ok(mut d) = preset::load_rack(&path) {
                            d.slots.resize(SLOTS, None);
                            self.desc = d;
                            structural = true;
                        }
                    }
                    ui.close();
                }
                if ui.button("Clear Rack").clicked() {
                    self.desc = RackDesc::default();
                    structural = true;
                    ui.close();
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("rack_slots")
            .max_height(ui.available_height() * 0.5)
            .show(ui, |ui| {
                for i in 0..SLOTS {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{:>2}", i + 1)).monospace().weak());
                        match &mut self.desc.slots[i] {
                            Some(slot) => {
                                let name = registry::info(&slot.effect)
                                    .map(|e| e.name)
                                    .or(slot.plugin.as_ref().map(|p| p.name.as_str()))
                                    .unwrap_or(slot.effect.as_str());
                                let mut bypassed = slot.bypass;
                                if ui
                                    .checkbox(&mut bypassed, "")
                                    .on_hover_text("Bypass")
                                    .changed()
                                {
                                    slot.bypass = bypassed;
                                    preview.bypass(Owner::Rack, i, bypassed);
                                }
                                let text = if slot.bypass {
                                    RichText::new(name).weak().strikethrough()
                                } else {
                                    RichText::new(name)
                                };
                                if ui
                                    .selectable_label(self.selected == Some(i), text)
                                    .clicked()
                                {
                                    self.selected = Some(i);
                                }
                                if slot.plugin.is_some() {
                                    if let Some((badge, why)) = plugin_badge(preview, i, slot) {
                                        ui.label(RichText::new(badge).small().weak())
                                            .on_hover_text(why);
                                    }
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("Remove")
                                            .on_hover_text("Remove")
                                            .clicked()
                                        {
                                            self.desc.slots[i] = None;
                                            structural = true;
                                        }
                                        if i + 1 < SLOTS
                                            && ui
                                                .small_button("Down")
                                                .on_hover_text("Move down")
                                                .clicked()
                                        {
                                            self.desc.slots.swap(i, i + 1);
                                            self.selected = Some(i + 1);
                                            structural = true;
                                        }
                                        if i > 0
                                            && ui
                                                .small_button("Up")
                                                .on_hover_text("Move up")
                                                .clicked()
                                        {
                                            self.desc.slots.swap(i, i - 1);
                                            self.selected = Some(i - 1);
                                            structural = true;
                                        }
                                    },
                                );
                            }
                            None => {
                                ui.menu_button(RichText::new("+ Add effect").weak(), |ui| {
                                    if let Some(m) = &preview.plugins {
                                        if !m.catalog().is_empty() {
                                            ui.menu_button("Plugins", |ui| {
                                                for (vendor, list) in m.catalog().by_vendor() {
                                                    ui.menu_button(vendor, |ui| {
                                                        for d in list {
                                                            if ui.button(&d.name).clicked() {
                                                                self.desc.slots[i] =
                                                                    Some(m.new_slot(d));
                                                                self.selected = Some(i);
                                                                structural = true;
                                                                ui.close();
                                                            }
                                                        }
                                                    });
                                                }
                                            });
                                            ui.separator();
                                        }
                                    }
                                    for (cat, list) in effects_by_category(true) {
                                        ui.menu_button(cat.name(), |ui| {
                                            for e in list {
                                                if ui.button(e.name).clicked() {
                                                    self.desc.slots[i] = SlotDesc::new(e.id);
                                                    self.selected = Some(i);
                                                    structural = true;
                                                    ui.close();
                                                }
                                            }
                                        });
                                    }
                                });
                            }
                        }
                    });
                }
            });
        ui.separator();
        if let Some(i) = self.selected {
            if let Some(slot) = &mut self.desc.slots[i] {
                if slot.plugin.is_some() {
                    plugin_editor(ui, preview, i, slot);
                } else if let (Some(info), Some(specs)) =
                    (registry::info(&slot.effect), registry::specs(&slot.effect))
                {
                    ui.label(RichText::new(info.name).strong());
                    for (k, v) in effect_editor(
                        ui,
                        ("rack_params", i),
                        &slot.effect,
                        specs,
                        &mut slot.values,
                    ) {
                        preview.param(Owner::Rack, i, k, v);
                    }
                    ui.separator();
                }
            }
        }
        let gains_before = (
            self.desc.input_gain_db,
            self.desc.output_gain_db,
            self.desc.mix,
        );
        egui::Grid::new("rack_gains").num_columns(2).show(ui, |ui| {
            ui.label("Input");
            ui.add(egui::Slider::new(&mut self.desc.input_gain_db, -40.0..=24.0).suffix(" dB"));
            ui.end_row();
            ui.label("Output");
            ui.add(egui::Slider::new(&mut self.desc.output_gain_db, -40.0..=24.0).suffix(" dB"));
            ui.end_row();
            ui.label("Mix (wet)");
            let mut pct = self.desc.mix * 100.0;
            if ui
                .add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" %"))
                .changed()
            {
                self.desc.mix = pct / 100.0;
            }
            ui.end_row();
        });
        if gains_before
            != (
                self.desc.input_gain_db,
                self.desc.output_gain_db,
                self.desc.mix,
            )
        {
            preview.gains(Owner::Rack, &self.desc);
        }
        ui.horizontal(|ui| {
            let can = has_file && !self.desc.is_empty();
            if ui
                .add_enabled(can, egui::Button::new("Apply"))
                .on_hover_text("Render the rack into the selection (or the whole file)")
                .clicked()
            {
                action = Some(EffectAction::Apply {
                    label: "Effects Rack".into(),
                    kind: ApplyKind::Rack(self.desc.clone()),
                });
            }
            if ui
                .add_enabled(
                    has_file && preview.is_active(Owner::Rack),
                    egui::Button::new("Play / Stop"),
                )
                .clicked()
            {
                action = Some(EffectAction::TogglePlayback);
            }
        });
        if structural {
            preview.reinstall(Owner::Rack, &self.desc);
        }
        action
    }
}

/// A short status for a plugin slot that is not (yet) usable, with the reason for the tooltip.
fn plugin_badge(
    preview: &mut Preview,
    index: usize,
    slot: &SlotDesc,
) -> Option<(&'static str, String)> {
    let err = match preview.ensure_loaded(slot) {
        Ok(()) => preview
            .build_errors
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, e)| e.clone())?,
        Err(e) => e,
    };
    let badge = match &err {
        SlotError::Missing(_) => "missing",
        SlotError::NotReady => "loading…",
        SlotError::UnsupportedRate(_) => "unsupported rate",
        _ => "failed",
    };
    Some((badge, err.to_string()))
}

/// The generic parameter editor for a plugin slot: parameters come from the plugin itself.
fn plugin_editor(ui: &mut Ui, preview: &mut Preview, index: usize, slot: &mut SlotDesc) {
    let name = slot
        .plugin
        .as_ref()
        .map_or("Plugin", |p| p.name.as_str())
        .to_string();
    ui.label(RichText::new(name).strong());
    if let Err(e) = preview.ensure_loaded(slot) {
        ui.label(RichText::new(e.to_string()).weak());
        ui.separator();
        return;
    }
    let Some((specs, current)) = preview.plugin_params(slot.id) else {
        return;
    };
    if slot.values.len() != specs.len() {
        slot.values = current;
    }
    let slot_id = slot.id;
    for (k, v) in effect_editor(
        ui,
        ("rack_params", index),
        &slot.effect,
        &specs,
        &mut slot.values,
    ) {
        preview.plugin_param(Owner::Rack, index, slot_id, k, v);
    }
    ui.separator();
}

/// A single effect opened from the Effects menu.
pub struct EffectDialog {
    pub id: &'static str,
    name: &'static str,
    /// Real-time effects are edited as a one-slot rack, so preview works the same way.
    desc: Option<RackDesc>,
    offline_values: Vec<f32>,
    envelope: Option<Envelope>,
    presets: Vec<Preset>,
    preset_name: String,
    pub open: bool,
}

impl EffectDialog {
    pub fn new(id: &'static str) -> Option<EffectDialog> {
        let info = registry::info(id)?;
        let (desc, offline_values, envelope) = if info.is_realtime() {
            (Some(RackDesc::single(id)?), Vec::new(), None)
        } else {
            let fx = registry::create_offline(id)?;
            (None, fx.values().to_vec(), fx.envelope().cloned())
        };
        let presets = preset::root()
            .map(|r| preset::load(&r, id))
            .unwrap_or_default();
        Some(EffectDialog {
            id,
            name: info.name,
            desc,
            offline_values,
            envelope,
            presets,
            preset_name: String::new(),
            open: true,
        })
    }

    fn specs(&self) -> &'static [boothcraft_dsp::ParamSpec] {
        registry::specs(self.id).unwrap_or(&[])
    }

    fn values_mut(&mut self) -> &mut Vec<f32> {
        match &mut self.desc {
            Some(d) => &mut d.slots[0].as_mut().expect("single-slot rack").values,
            None => &mut self.offline_values,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        preview: &mut Preview,
        playing: bool,
    ) -> Option<EffectAction> {
        let mut action = None;
        let mut open = self.open;
        let specs = self.specs();
        egui::Window::new(self.name)
            .id(egui::Id::new(("effect_dialog", self.id)))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                let mut reload = false;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("presets")
                        .selected_text("Presets")
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(false, "(Default)").clicked() {
                                *self.values_mut() = specs.iter().map(|s| s.default).collect();
                                reload = true;
                            }
                            for p in self.presets.clone() {
                                if ui.selectable_label(false, &p.name).clicked() {
                                    *self.values_mut() = p.values_for(specs);
                                    self.preset_name = p.name.clone();
                                    reload = true;
                                }
                            }
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.preset_name)
                            .hint_text("Preset name")
                            .desired_width(110.0),
                    );
                    let can_save = !self.preset_name.trim().is_empty() && preset::root().is_some();
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .clicked()
                    {
                        if let Some(root) = preset::root() {
                            let values = self.values_mut().clone();
                            let p = Preset::capture(self.preset_name.trim(), specs, &values);
                            if preset::save(&root, self.id, &p).is_ok() {
                                self.presets = preset::load(&root, self.id);
                            }
                        }
                    }
                    let exists = self
                        .presets
                        .iter()
                        .any(|p| p.name == self.preset_name.trim());
                    if ui
                        .add_enabled(exists, egui::Button::new("Delete"))
                        .clicked()
                    {
                        if let Some(root) = preset::root() {
                            let _ = preset::delete(&root, self.id, self.preset_name.trim());
                            self.presets = preset::load(&root, self.id);
                        }
                    }
                });
                ui.separator();
                let id = self.id;
                let changes =
                    effect_editor(ui, ("dialog_params", id), id, specs, self.values_mut());
                for (k, v) in changes {
                    preview.param(Owner::Dialog, 0, k, v);
                }
                let smooth = self.offline_values.first().is_some_and(|v| *v >= 0.5);
                if let Some(env) = &mut self.envelope {
                    let db = env.min < 0.0;
                    ui.horizontal(|ui| {
                        ui.label("Shape:");
                        let (lo, unity) = (env.min, if db { 0.0 } else { 1.0 });
                        let shapes: [(&str, Vec<(f32, f32)>); 4] = [
                            ("Flat", vec![(0.0, unity), (1.0, unity)]),
                            (
                                "Fade In",
                                vec![(0.0, if db { lo } else { 0.0 }), (1.0, unity)],
                            ),
                            (
                                "Fade Out",
                                vec![(0.0, unity), (1.0, if db { lo } else { 0.0 })],
                            ),
                            (
                                "Dip",
                                vec![
                                    (0.0, unity),
                                    (0.2, if db { -12.0 } else { 0.25 }),
                                    (0.8, if db { -12.0 } else { 0.25 }),
                                    (1.0, unity),
                                ],
                            ),
                        ];

                        for (name, pts) in shapes {
                            if ui.small_button(name).clicked() {
                                env.points = pts;
                                env.normalize();
                            }
                        }
                    });
                    envelope_editor(ui, env, smooth, db);
                    ui.label(
                        RichText::new(
                            "Click to add a point, drag to move, double-click to remove.",
                        )
                        .weak()
                        .small(),
                    );
                }
                if reload {
                    if let Some(d) = &self.desc {
                        preview.reinstall(Owner::Dialog, d);
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if let Some(d) = &self.desc {
                        let previewing = preview.is_active(Owner::Dialog) && playing;
                        let label = if previewing {
                            "Stop Preview"
                        } else {
                            "Preview"
                        };
                        if ui
                            .add_enabled(preview.available(), egui::Button::new(label))
                            .clicked()
                        {
                            if previewing {
                                preview.stop(Owner::Dialog);
                            } else {
                                preview.stop(Owner::Rack);
                                preview.start(Owner::Dialog, d);
                            }
                            action = Some(EffectAction::TogglePlayback);
                        }
                    }
                    if ui.button("Apply").clicked() {
                        let kind = match &self.desc {
                            Some(d) => ApplyKind::Rack(d.clone()),
                            None => ApplyKind::Offline {
                                id: self.id,
                                values: self.offline_values.clone(),
                                envelope: self.envelope.clone(),
                            },
                        };
                        action = Some(EffectAction::Apply {
                            label: self.name.to_string(),
                            kind,
                        });
                        self.open = false;
                    }
                });
            });
        if !open {
            self.open = false;
        }
        if !self.open {
            preview.stop(Owner::Dialog);
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boothcraft_core::{Document, Rope};

    fn wait(job: &ApplyJob) -> Option<ApplyResult> {
        for _ in 0..500 {
            if let Some(r) = job.poll() {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("apply job did not finish");
    }

    #[test]
    fn apply_touches_only_the_selection_and_enabled_channels() {
        let ch = vec![0.25f32; 10_000];
        let mut f = OpenFile::new(
            "t".into(),
            None,
            Document::new(
                48_000,
                vec![Rope::from_samples(&ch), Rope::from_samples(&ch)],
            ),
        );
        f.selection = Selection::new(2_000, 6_000);
        f.mask = ChannelMask(0b01);
        let mut desc = RackDesc::single("amplify").unwrap();
        desc.slots[0].as_mut().unwrap().values[0] = 20.0 * 2f32.log10();
        let job = ApplyJob::start(0, &f, "Amplify".into(), ApplyKind::Rack(desc));
        let result = wait(&job).expect("not cancelled");
        job.commit(&mut f, result);
        let left = f.doc().channels[0].to_vec(0..10_000);
        let right = f.doc().channels[1].to_vec(0..10_000);
        assert_eq!(left[1_999], 0.25);
        assert!((left[4_000] - 0.5).abs() < 1e-5);
        assert_eq!(left[6_000], 0.25);
        assert!(
            right.iter().all(|&x| x == 0.25),
            "disabled channel untouched"
        );
        assert_eq!(f.history.labels().last(), Some("Amplify"));
        f.undo();
        assert_eq!(f.doc().channels[0].to_vec(4_000..4_001), vec![0.25]);
    }

    #[test]
    fn cancelled_apply_changes_nothing() {
        let mut f = OpenFile::new(
            "t".into(),
            None,
            Document::new(48_000, vec![Rope::from_samples(&vec![0.1; 2_000_000])]),
        );
        f.selection = Selection::default();
        let job = ApplyJob::start(
            0,
            &f,
            "Amplify".into(),
            ApplyKind::Rack(RackDesc::single("amplify").unwrap()),
        );
        job.cancel();
        assert!(wait(&job).is_none());
    }

    #[test]
    fn offline_apply_with_values_and_envelope() {
        let mut f = OpenFile::new(
            "t".into(),
            None,
            Document::new(48_000, vec![Rope::from_samples(&vec![0.1; 1001])]),
        );
        let job = ApplyJob::start(
            0,
            &f,
            "Normalize".into(),
            ApplyKind::Offline {
                id: "normalize",
                values: vec![-6.0, 1.0, 0.0],
                envelope: None,
            },
        );
        let r = wait(&job).unwrap();
        job.commit(&mut f, r);
        assert!((f.doc().channels[0].to_vec(0..1)[0] - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6);

        let env = Envelope {
            points: vec![(0.0, 0.0), (1.0, 1.0)],
            min: 0.0,
            max: 1.0,
        };
        let job = ApplyJob::start(
            0,
            &f,
            "Fade Envelope".into(),
            ApplyKind::Offline {
                id: "fade_envelope",
                values: vec![0.0],
                envelope: Some(env),
            },
        );
        let r = wait(&job).unwrap();
        job.commit(&mut f, r);
        let v = f.doc().channels[0].to_vec(0..1001);
        assert_eq!(v[0], 0.0);
        assert!((v[500] / v[1000] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn match_loudness_job_uses_the_whole_file_and_every_channel() {
        let tone: Vec<f32> = (0..96_000)
            .map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / 48_000.0).sin() * 0.05)
            .collect();
        let mut f = OpenFile::new(
            "t".into(),
            None,
            Document::new(
                48_000,
                vec![Rope::from_samples(&tone), Rope::from_samples(&tone)],
            ),
        );
        f.selection = Selection::new(10, 20);
        f.mask = ChannelMask(0b01);
        let job = ApplyJob::start_whole(
            0,
            &f,
            "Match Loudness".into(),
            ApplyKind::MatchLoudness {
                target: -20.0,
                ceiling: None,
            },
        );
        let r = wait(&job).unwrap();
        job.commit(&mut f, r);
        let n = f.doc().frames();
        let chans: Vec<Vec<f32>> = f.doc().channels.iter().map(|c| c.to_vec(0..n)).collect();
        let m = boothcraft_dsp::loudness::measure(&chans, 48_000);
        assert!((m.integrated + 20.0).abs() < 0.1, "{}", m.integrated);
        assert_eq!(
            chans[0], chans[1],
            "both channels processed despite the channel mask"
        );
    }

    #[test]
    fn effects_menu_lists_every_effect_once() {
        let all: usize = effects_by_category(false)
            .iter()
            .map(|(_, l)| l.len())
            .sum();
        assert_eq!(all, EFFECTS.len());
    }
}
