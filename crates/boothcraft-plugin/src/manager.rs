//! The host's main-thread owner of every plugin instance.
//!
//! Instances never leave the main thread. Each live slot is activated once; its processor moves to
//! the audio thread inside a rack, and comes back (retired) when that rack is replaced. Instances
//! outlive rack rebuilds, so editing the rack or toggling preview does not re-create plugins (and
//! their licence dialogs). A slot is resolved, in order, by: carrying the processor over from the
//! rack already playing, reusing a parked processor, activating an inactive instance, or
//! instantiating.

use crate::cache::ScanCache;
use crate::catalog::Catalog;
use crate::crash::Inflight;
use crate::effect::{specs_from, PluginEffect, RetiredProcessor, Status, STATUS_BAD_OUTPUT};
use crate::main_thread::MainThread;
use crate::pending::Pending;
use crate::scan::{self, ScanConfig, ScanReport};
use crate::settings::PluginSettings;
use crate::traits::{
    Activated, AudioIo, HostContext, HostEvent, ParamInfo, ParamQueue, PluginDescriptor,
    PluginFormat, PluginInstance, PluginProcessor, ProcessConfig, StateKind,
};
use boothcraft_dsp::rack::{Carried, SLOTS};
use boothcraft_dsp::{
    Effect, ParamSpec, PluginRef, PluginState, Rack, RackConfig, RackDesc, SlotDesc, SlotError,
    SlotId, SlotSource, CARRY_NONE,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Where a live slot's processor is, once activated.
struct Active {
    cfg: ProcessConfig,
    activated: Activated,
    /// `Some` while parked here; `None` while it is inside a rack on the audio thread.
    processor: Option<Box<dyn PluginProcessor>>,
}

struct Live {
    instance: Option<Box<dyn PluginInstance>>,
    pending: Option<Pending<Box<dyn PluginInstance>>>,
    state_loaded: bool,
    /// The plugin's parameters as specs, with the format's id for each, built once per load.
    specs: Option<(Vec<ParamSpec>, Vec<u32>)>,
    active: Option<Active>,
    /// A throwaway instance for Apply: torn down as soon as its processor comes back.
    offline: bool,
    /// Torn down as soon as its processor is back (the UI no longer holds its slot).
    doomed: bool,
    /// The plugin asked to be re-activated (latency or parameter list changed).
    restart_pending: bool,
    /// Wanted in a rack but its processor was still busy in the old one.
    rebuild_after_retire: bool,
    status: Status,
}

/// What happened since the last pump, for the UI to act on.
#[derive(Debug, Default, PartialEq)]
pub struct Pump {
    /// The preview rack should be rebuilt (a plugin restarted or became ready).
    pub rebuild: bool,
    /// Parameters edited inside a plugin's own editor: (slot, parameter id, plain value).
    pub param_edits: Vec<(SlotId, u32, f32)>,
    /// Slots whose plugin state changed on its own side.
    pub state_dirty: Vec<SlotId>,
    pub scan_finished: Option<ScanReport>,
}

/// A rack built for Apply, and any slots that could not be built.
pub type OfflineBuild = (Box<Rack>, Vec<(usize, SlotError)>);

/// A rack built for preview, plus how to install it without re-creating plugins.
pub struct PreviewBuild {
    pub rack: Box<Rack>,
    /// `carry[i]` = slot of the rack currently playing whose live plugin slot `i` takes over.
    pub carry: [u8; SLOTS],
    pub errors: Vec<(usize, SlotError)>,
}

pub struct PluginManager {
    mt: MainThread,
    formats: Arc<Vec<Box<dyn PluginFormat>>>,
    dir: Option<PathBuf>,
    pub settings: PluginSettings,
    cache: ScanCache,
    catalog: Catalog,
    live: HashMap<SlotId, Live>,
    /// The plugin slots of the preview rack on the audio thread, by slot index.
    installed: Vec<Option<(SlotId, ProcessConfig)>>,
    carry_now: HashSet<SlotId>,
    offline_mode: bool,
    events_tx: Sender<(SlotId, HostEvent)>,
    events_rx: Receiver<(SlotId, HostEvent)>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    inflight: Option<Inflight>,
    /// Plugins that were loaded when the previous session ended unexpectedly.
    pub unclean: Vec<PluginRef>,
    rebuild_wanted: bool,
    scan_job: Option<Pending<(ScanCache, ScanReport)>>,
    /// Instances deliberately leaked at shutdown because their processor never came back.
    pub leaked: usize,
}

impl PluginManager {
    /// `dir` is where settings, the scan cache and the crash marker live (`None` keeps everything
    /// in memory). `repaint` wakes the UI when a plugin calls back.
    pub fn new(
        mt: MainThread,
        formats: Vec<Box<dyn PluginFormat>>,
        dir: Option<PathBuf>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> PluginManager {
        let settings = dir.as_deref().map(PluginSettings::load).unwrap_or_default();
        let cache = dir.as_deref().map(ScanCache::load).unwrap_or_default();
        let mut catalog = Catalog::default();
        catalog.refresh(&cache, &settings.disabled);
        for d in formats.iter().flat_map(|f| f.bundled()) {
            catalog.insert(d);
        }
        let (inflight, unclean) = match dir.as_deref() {
            Some(d) => {
                let (i, u) = Inflight::start(d);
                (Some(i), u)
            }
            None => (None, Vec::new()),
        };
        let (events_tx, events_rx) = channel();
        PluginManager {
            mt,
            formats: Arc::new(formats),
            dir,
            settings,
            cache,
            catalog,
            live: HashMap::new(),
            installed: Vec::new(),
            carry_now: HashSet::new(),
            offline_mode: false,
            events_tx,
            events_rx,
            repaint,
            inflight,
            unclean,
            rebuild_wanted: false,
            scan_job: None,
            leaked: 0,
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn cache(&self) -> &ScanCache {
        &self.cache
    }

    /// Add a plugin to the catalog without scanning (tests, the mock format, `--plugin` paths).
    pub fn register(&mut self, d: PluginDescriptor) {
        self.catalog.insert(d);
    }

    pub fn formats(&self) -> &[Box<dyn PluginFormat>] {
        &self.formats
    }

    // ---- scanning -------------------------------------------------------------------------

    /// Scan in the background; the result arrives through [`PluginManager::pump`].
    pub fn start_scan(&mut self, cfg: ScanConfig) {
        if self.scan_job.is_some() {
            return;
        }
        let (tx, pending) = Pending::channel();
        let formats = self.formats.clone();
        let mut cache = self.cache.clone();
        let extra = self.settings.extra_paths.clone();
        std::thread::spawn(move || {
            let report = scan::scan(&formats, &mut cache, &extra, &cfg);
            let _ = tx.send(Ok((cache, report)));
        });
        self.scan_job = Some(pending);
    }

    pub fn is_scanning(&self) -> bool {
        self.scan_job.is_some()
    }

    /// Scan and wait (the CLI).
    pub fn scan_blocking(&mut self, cfg: &ScanConfig) -> ScanReport {
        let report = scan::scan(
            &self.formats,
            &mut self.cache,
            &self.settings.extra_paths,
            cfg,
        );
        self.after_scan();
        report
    }

    fn after_scan(&mut self) {
        if let Some(d) = &self.dir {
            let _ = self.cache.save(d);
        }
        self.refresh_catalog();
    }

    fn refresh_catalog(&mut self) {
        self.catalog.refresh(&self.cache, &self.settings.disabled);
    }

    /// Turn plugins off (after a crash, or from the Plugin Manager) and remember it.
    pub fn disable(&mut self, effect_ids: impl IntoIterator<Item = String>) {
        self.settings.disabled.extend(effect_ids);
        if let Some(d) = &self.dir {
            let _ = self.settings.save(d);
        }
        self.refresh_catalog();
    }

    // ---- events ---------------------------------------------------------------------------

    /// Handle what plugins have asked for since the last call. Call once per UI frame, never from
    /// inside a plugin callback: those only queue (they arrive re-entrantly and from any thread).
    pub fn pump(&mut self) -> Pump {
        let mut out = Pump::default();
        while let Ok((slot, event)) = self.events_rx.try_recv() {
            match event {
                HostEvent::LatencyChanged | HostEvent::ParamsChanged => {
                    if let Some(l) = self.live.get_mut(&slot) {
                        l.restart_pending = true;
                        if event == HostEvent::ParamsChanged {
                            l.specs = None;
                        }
                        self.rebuild_wanted = true;
                    }
                }
                HostEvent::StateDirty => out.state_dirty.push(slot),
                HostEvent::ParamEdited { id, value } => out.param_edits.push((slot, id, value)),
            }
        }
        if let Some(job) = &mut self.scan_job {
            if let Some(res) = job.poll() {
                self.scan_job = None;
                if let Ok((cache, report)) = res {
                    self.cache = cache;
                    self.after_scan();
                    out.scan_finished = Some(report);
                }
            }
        }
        out.rebuild = std::mem::take(&mut self.rebuild_wanted);
        out
    }

    // ---- building racks -------------------------------------------------------------------

    /// Build the preview rack for `desc`. Plugin slots already playing (same slot id, same
    /// configuration) are marked in `carry` and left to the audio thread to move across.
    pub fn build_preview(&mut self, desc: &RackDesc, cfg: &RackConfig) -> PreviewBuild {
        let pcfg = process_config(cfg, false);
        let mut carry = [CARRY_NONE; SLOTS];
        self.carry_now.clear();
        for (i, slot) in desc.slots.iter().enumerate().take(SLOTS) {
            let Some(slot) = slot.as_ref().filter(|s| s.plugin.is_some()) else {
                continue;
            };
            let from = self.installed.iter().position(|e| {
                e.as_ref()
                    .is_some_and(|(id, c)| *id == slot.id && *c == pcfg)
            });
            let playing = self.live.get(&slot.id).is_some_and(|l| {
                !l.restart_pending
                    && l.active
                        .as_ref()
                        .is_some_and(|a| a.processor.is_none() && a.cfg == pcfg)
            });
            if let (Some(from), true) = (from, playing) {
                carry[i] = from as u8;
                self.carry_now.insert(slot.id);
            }
        }
        self.offline_mode = false;
        let (rack, errors) = Rack::build(desc, cfg, self);
        self.carry_now.clear();
        self.installed = desc
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let s = s.as_ref().filter(|s| s.plugin.is_some())?;
                let ok = !errors.iter().any(|(e, _)| *e == i);
                ok.then_some((s.id, pcfg))
            })
            .collect();
        PreviewBuild {
            rack: Box::new(rack),
            carry,
            errors,
        }
    }

    /// Forget the playing rack (preview switched off).
    pub fn preview_cleared(&mut self) {
        self.installed.clear();
    }

    /// Build a rack for Apply. Every plugin slot gets a new offline instance, created and
    /// activated here on the main thread at the document's rate, in offline mode, with the live
    /// instance's state and its latency primed. Hand the finished rack to [`finish_offline`].
    ///
    /// [`finish_offline`]: PluginManager::finish_offline
    pub fn prepare_offline(&mut self, desc: &RackDesc, cfg: &RackConfig) -> Pending<OfflineBuild> {
        let mut d = desc.clone();
        for slot in d.slots.iter_mut().flatten().filter(|s| s.plugin.is_some()) {
            // The live instance's current state beats whatever the slot last saved.
            if let Some(state) = self.save_live_state(slot.id) {
                slot.state = Some(state);
            }
            slot.id = SlotId::fresh();
        }
        self.offline_mode = true;
        let (rack, errors) = Rack::build(&d, cfg, self);
        self.offline_mode = false;
        Pending::ready(Ok((Box::new(rack), errors)))
    }

    /// Tear down an Apply rack: processors go back to their instances, which are terminated.
    pub fn finish_offline(&mut self, rack: Box<Rack>) {
        self.retire(rack);
    }

    /// Take back what a rack the audio thread has finished with (or an Apply rack) was holding.
    #[allow(clippy::boxed_local)] // callers hold the `Box` the audio thread handed back
    pub fn retire(&mut self, rack: Box<Rack>) {
        for (_, mut effect) in rack.into_effects() {
            if let Some((slot, any)) = effect.retire() {
                if let Ok(r) = any.downcast::<RetiredProcessor>() {
                    self.park(slot, r.0);
                }
            }
        }
    }

    fn park(&mut self, slot: SlotId, processor: Box<dyn PluginProcessor>) {
        let Some(live) = self.live.get_mut(&slot) else {
            return;
        };
        let Some(active) = &mut live.active else {
            return;
        };
        active.processor = Some(processor);
        if live.rebuild_after_retire || live.restart_pending {
            live.rebuild_after_retire = false;
            self.rebuild_wanted = true;
        }
        if live.doomed {
            self.terminate(slot);
        }
    }

    // ---- slots ----------------------------------------------------------------------------

    fn save_live_state(&mut self, slot: SlotId) -> Option<PluginState> {
        let mt = self.mt;
        let live = self.live.get_mut(&slot).filter(|l| !l.offline)?;
        let bytes = live
            .instance
            .as_mut()?
            .save_state(mt, StateKind::Document)
            .ok()?;
        Some(PluginState(bytes))
    }

    /// Record every live plugin's state and parameter values into `desc`, so saving a preset or
    /// running Apply uses what the plugin really holds (including edits made in its own editor).
    pub fn snapshot(&mut self, desc: &mut RackDesc) {
        for slot in desc.slots.iter_mut().flatten() {
            if slot.plugin.is_none() {
                continue;
            }
            if let Some(state) = self.save_live_state(slot.id) {
                slot.state = Some(state);
            }
            if let Some(inst) = self.live.get(&slot.id).and_then(|l| l.instance.as_ref()) {
                slot.values = inst.params().iter().map(|p| p.current).collect();
            }
        }
    }

    /// A new slot for `descriptor`, ready to put in a rack.
    pub fn new_slot(&self, descriptor: &PluginDescriptor) -> SlotDesc {
        SlotDesc::for_plugin(descriptor.plugin_ref(), Vec::new(), None)
    }

    /// The plugin's parameters as it currently holds them, once the slot has loaded.
    pub fn params(&self, slot: SlotId) -> Option<Vec<ParamInfo>> {
        Some(self.live.get(&slot)?.instance.as_ref()?.params())
    }

    pub fn param_text(&self, slot: SlotId, id: u32, value: f32) -> Option<String> {
        Some(
            self.live
                .get(&slot)?
                .instance
                .as_ref()?
                .param_text(id, value),
        )
    }

    /// A generic-UI edit: updates the plugin's controller side now. The caller also sends
    /// `RackMsg::Param` so the processor on the audio thread hears it.
    pub fn edit_param(&mut self, slot: SlotId, id: u32, value: f32) {
        let mt = self.mt;
        if let Some(inst) = self.live.get_mut(&slot).and_then(|l| l.instance.as_mut()) {
            inst.set_param(mt, id, value);
        }
    }

    /// Whether the plugin's output went bad (NaN or infinity) since the last call.
    pub fn take_bad_output(&self, slot: SlotId) -> bool {
        self.live.get(&slot).is_some_and(|l| {
            l.status.fetch_and(!STATUS_BAD_OUTPUT, Relaxed) & STATUS_BAD_OUTPUT != 0
        })
    }

    pub fn is_loaded(&self, slot: SlotId) -> bool {
        self.live.get(&slot).is_some_and(|l| l.instance.is_some())
    }

    /// Instances that are no longer in any rack the UI holds are torn down (as soon as their
    /// processors are back). Pass every slot id still wanted.
    pub fn sweep(&mut self, keep: &HashSet<SlotId>) {
        let gone: Vec<SlotId> = self
            .live
            .iter()
            .filter(|(id, l)| !l.offline && !keep.contains(id))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            let parked = self
                .live
                .get(&id)
                .is_some_and(|l| l.active.as_ref().is_none_or(|a| a.processor.is_some()));
            if parked {
                self.terminate(id);
            } else if let Some(l) = self.live.get_mut(&id) {
                l.doomed = true;
            }
        }
    }

    fn terminate(&mut self, slot: SlotId) {
        let mt = self.mt;
        let Some(mut live) = self.live.remove(&slot) else {
            return;
        };
        if let (Some(inst), Some(active)) = (live.instance.as_mut(), live.active.as_mut()) {
            if let Some(proc) = active.processor.take() {
                inst.deactivate(mt, proc);
            }
        }
        // Dropping the instance here, on the main thread, ends it.
    }

    /// App shutdown, after the audio thread has been told to drop its rack and the trash has been
    /// drained into `retired`. Instances whose processors never came back are leaked rather than
    /// risk releasing a plugin from the wrong thread.
    pub fn shutdown(&mut self, retired: Vec<Box<Rack>>) {
        for rack in retired {
            self.retire(rack);
        }
        let ids: Vec<SlotId> = self.live.keys().copied().collect();
        for id in ids {
            let parked = self
                .live
                .get(&id)
                .is_some_and(|l| l.active.as_ref().is_none_or(|a| a.processor.is_some()));
            if parked {
                self.terminate(id);
            } else if let Some(l) = self.live.remove(&id) {
                self.leaked += 1;
                std::mem::forget(l);
            }
        }
        if let Some(i) = self.inflight.take() {
            i.finish();
        }
    }

    // ---- creating effects -----------------------------------------------------------------

    fn instance_for(
        &mut self,
        slot: &SlotDesc,
        descriptor: &PluginDescriptor,
    ) -> Result<(), SlotError> {
        if !self.live.contains_key(&slot.id) {
            let formats = self.formats.clone();
            let format = formats
                .iter()
                .find(|f| f.kind() == descriptor.format)
                .ok_or_else(|| SlotError::Missing(descriptor.effect_id()))?;
            // Recorded before loading: a crash inside the plugin is what the marker is for.
            if let Some(i) = &mut self.inflight {
                i.note_loading(&descriptor.plugin_ref());
            }
            let host = HostContext::new(slot.id, self.events_tx.clone(), self.repaint.clone());
            let pending = format.instantiate(descriptor, host, self.mt);
            self.live.insert(
                slot.id,
                Live {
                    instance: None,
                    pending: Some(pending),
                    state_loaded: false,
                    specs: None,
                    active: None,
                    offline: self.offline_mode,
                    doomed: self.offline_mode,
                    restart_pending: false,
                    rebuild_after_retire: false,
                    status: Arc::new(AtomicU32::new(0)),
                },
            );
        }
        let live = self.live.get_mut(&slot.id).expect("just inserted");
        if let Some(p) = &mut live.pending {
            match p.poll() {
                None => return Err(SlotError::NotReady),
                Some(Ok(inst)) => {
                    live.instance = Some(inst);
                    live.pending = None;
                }
                Some(Err(e)) => {
                    self.live.remove(&slot.id);
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    /// Instantiate the slot's plugin if needed and load its saved state, without activating it.
    fn load_slot(&mut self, slot: &SlotDesc) -> Result<(), SlotError> {
        let plugin = slot
            .plugin
            .as_ref()
            .ok_or_else(|| SlotError::Unknown(slot.effect.clone()))?;
        let id = plugin.effect_id();
        let descriptor = self
            .catalog
            .get(&id)
            .cloned()
            .ok_or(SlotError::Missing(id))?;
        self.instance_for(slot, &descriptor)?;

        let mt = self.mt;
        let live = self.live.get_mut(&slot.id).expect("instance_for made it");
        let inst = live.instance.as_mut().expect("instance_for loaded it");
        if !live.state_loaded {
            // The saved state is authoritative; `values` only fills in when there is none.
            match &slot.state {
                Some(state) => inst.load_state(mt, &state.0).map_err(SlotError::from)?,
                None => {
                    let params = inst.params();
                    if slot.values.len() == params.len() {
                        for (p, v) in params.iter().zip(&slot.values) {
                            inst.set_param(mt, p.id, *v);
                        }
                    }
                }
            }
            live.state_loaded = true;
        }
        if live.specs.is_none() {
            live.specs = Some(specs_from(&inst.params()));
        }
        Ok(())
    }

    /// Load a slot's plugin ahead of any rack, so the UI can show and edit its parameters (and
    /// those edits reach the plugin even with preview off). Cheap once loaded; `NotReady` while an
    /// asynchronous load is still running.
    pub fn ensure_loaded(&mut self, slot: &SlotDesc) -> Result<(), SlotError> {
        let loaded = self
            .live
            .get(&slot.id)
            .is_some_and(|l| l.state_loaded && l.instance.is_some() && l.specs.is_some());
        if loaded {
            return Ok(());
        }
        self.load_slot(slot)
    }

    /// The loaded plugin's parameter specs and, for each, the format's parameter id.
    pub fn specs(&self, slot: SlotId) -> Option<(&[ParamSpec], &[u32])> {
        let (s, ids) = self.live.get(&slot)?.specs.as_ref()?;
        Some((s, ids))
    }

    fn make_plugin(
        &mut self,
        slot: &SlotDesc,
        cfg: &RackConfig,
    ) -> Result<Box<dyn Effect>, SlotError> {
        if self.carry_now.contains(&slot.id) {
            return Ok(Box::new(Carried));
        }
        self.load_slot(slot)?;

        let mt = self.mt;
        let pcfg = process_config(cfg, self.offline_mode);
        let live = self.live.get_mut(&slot.id).expect("load_slot made it");
        let inst = live.instance.as_mut().expect("load_slot loaded it");

        let mut parked = None;
        if let Some(active) = &mut live.active {
            if active.processor.is_none() {
                // Still playing in the old rack: take it back first, then build again.
                live.rebuild_after_retire = true;
                return Err(SlotError::NotReady);
            }
            if active.cfg == pcfg && !live.restart_pending {
                parked = active.processor.take();
            } else if let Some(p) = active.processor.take() {
                inst.deactivate(mt, p);
                live.active = None;
                live.restart_pending = false;
            }
        }

        let (processor, mut activated) = match parked {
            Some(p) => (
                p,
                live.active
                    .as_ref()
                    .expect("parked implies active")
                    .activated,
            ),
            None => {
                let (mut p, mut a) = inst.activate(mt, &pcfg).map_err(SlotError::from)?;
                if pcfg.offline {
                    // Some plugins report their latency only after processing: run silence
                    // through, drop the output, and ask again.
                    prime(p.as_mut(), &a, pcfg.max_block);
                    a.latency = inst.latency();
                    a.tail = inst.tail();
                }
                (p, a)
            }
        };
        activated.latency = inst.latency();
        let params = inst.params();
        live.active = Some(Active {
            cfg: pcfg,
            activated,
            processor: None,
        });
        Ok(Box::new(PluginEffect::new(
            slot.id,
            processor,
            &params,
            activated,
            pcfg.max_block,
            live.status.clone(),
        )))
    }
}

fn process_config(cfg: &RackConfig, offline: bool) -> ProcessConfig {
    ProcessConfig {
        sample_rate: cfg.sample_rate,
        max_block: cfg.max_block,
        channels: cfg.channels,
        offline,
    }
}

/// Process one block of silence and throw the result away.
fn prime(p: &mut dyn PluginProcessor, a: &Activated, block: usize) {
    let ins: Vec<Vec<f32>> = vec![vec![0.0; block]; a.in_channels];
    let mut outs: Vec<Vec<f32>> = vec![vec![0.0; block]; a.out_channels];
    let in_refs: Vec<&[f32]> = ins.iter().map(Vec::as_slice).collect();
    let mut out_refs: Vec<&mut [f32]> = outs.iter_mut().map(Vec::as_mut_slice).collect();
    p.process(
        AudioIo {
            inputs: &in_refs,
            outputs: &mut out_refs,
            frames: block,
        },
        &ParamQueue::new(),
    );
    p.reset();
}

impl SlotSource for PluginManager {
    fn make(
        &mut self,
        index: usize,
        slot: &SlotDesc,
        cfg: &RackConfig,
    ) -> Result<Box<dyn Effect>, SlotError> {
        if slot.plugin.is_some() || boothcraft_dsp::registry::is_plugin_id(&slot.effect) {
            self.make_plugin(slot, cfg)
        } else {
            boothcraft_dsp::BuiltinSource.make(index, slot, cfg)
        }
    }
}
