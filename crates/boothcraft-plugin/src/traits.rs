//! The format-neutral host interface. VST3, AudioUnit and (later) CLAP implement these.
//!
//! The split follows CLAP's model, which VST3 and AU map onto too: a [`PluginInstance`] lives on
//! the main thread and is never `Send`; [`PluginInstance::activate`] hands back a movable
//! [`PluginProcessor`] for the audio thread, and [`PluginInstance::deactivate`] takes it back, so
//! the types forbid deactivating while processing.

use crate::error::HostError;
use crate::main_thread::MainThread;
use crate::pending::Pending;
use boothcraft_dsp::{PluginRef, SlotId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// A plugin found by scanning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// "vst3", "au", later "clap".
    pub format: String,
    /// Format-specific unique id (VST3 class id in hex, AU `type-subtype-manufacturer`).
    pub uid: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// The bundle or file it was found in.
    pub path: PathBuf,
}

impl PluginDescriptor {
    pub fn plugin_ref(&self) -> PluginRef {
        PluginRef {
            format: self.format.clone(),
            uid: self.uid.clone(),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
        }
    }

    pub fn effect_id(&self) -> String {
        format!("{}:{}", self.format, self.uid)
    }
}

/// A file a format might load plugins from. Finding one never loads plugin code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// Formats that need no probe (AU) already know what is inside.
    pub known: Option<Vec<PluginDescriptor>>,
}

/// One parameter, in plain (display-range) values.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    /// The format's own id, stable across sessions.
    pub id: u32,
    pub name: String,
    /// Unit label without a leading space ("dB", "Hz"); empty for none.
    pub unit: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub current: f32,
    /// Discrete steps between `min` and `max` (0 = continuous).
    pub steps: u32,
    /// Menu labels for a stepped parameter, if the plugin provides them.
    pub choices: Vec<String>,
    /// Bitwise OR of `boothcraft_dsp::param::flags`.
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessConfig {
    pub sample_rate: f32,
    pub max_block: usize,
    /// Channels the host rack runs; the plugin may negotiate fewer or more.
    pub channels: usize,
    /// Offline (Apply) rather than real-time rendering.
    pub offline: bool,
}

/// What the plugin agreed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activated {
    pub in_channels: usize,
    pub out_channels: usize,
    pub latency: usize,
    pub tail: usize,
}

/// How state is saved: a slot or Apply uses the plugin's full document state; a preset may use
/// a lighter form where the format distinguishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateKind {
    Document,
    Preset,
}

/// Something that happened on the plugin's side, delivered to the main thread.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// The plugin's latency changed; the rack must be re-activated to pick it up.
    LatencyChanged,
    /// The plugin's parameter list changed.
    ParamsChanged,
    /// Its state changed outside our own edits (a preset loaded in its editor).
    StateDirty,
    /// A parameter was edited in the plugin's own editor.
    ParamEdited { id: u32, value: f32 },
}

/// Callbacks a plugin makes into the host. They arrive re-entrantly and from any thread, so they
/// only enqueue: nothing is handled inside the callback.
#[derive(Clone)]
pub struct HostContext {
    pub slot: SlotId,
    events: Sender<(SlotId, HostEvent)>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl HostContext {
    pub fn new(
        slot: SlotId,
        events: Sender<(SlotId, HostEvent)>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> HostContext {
        HostContext {
            slot,
            events,
            repaint,
        }
    }

    pub fn notify(&self, event: HostEvent) {
        let _ = self.events.send((self.slot, event));
        (self.repaint)();
    }
}

pub trait PluginFormat: Send + Sync {
    fn kind(&self) -> &'static str;

    /// Files to consider: `extra` folders, plus the format's standard install locations when
    /// `include_standard` is set. Never loads plugin code.
    fn discover(&self, extra: &[PathBuf], include_standard: bool) -> Vec<Candidate>;

    /// Plugins that need no scanning because the format ships them (the mock format). Registered
    /// in the catalog at startup.
    fn bundled(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }

    /// Called once in a scan worker process before it probes anything (e.g. to keep it out of the
    /// Dock on macOS).
    fn init_scan_worker(&self) {}

    /// End a scan worker process once its reply has been written. Plugins' own shutdown code
    /// often crashes (static destructors, background threads), and nothing it does matters now,
    /// so a format can end the process without running it. The default exits normally.
    fn exit_scan_worker(&self, code: i32) -> ! {
        std::process::exit(code)
    }

    /// Whether `probe` must run in a scan worker. VST3 true (it loads the module); AU false.
    fn needs_probe(&self) -> bool;

    /// Load the file and list what it contains. Runs only in a scan worker child process.
    fn probe(&self, candidate: &Candidate) -> Result<Vec<PluginDescriptor>, HostError>;

    /// Create an instance. Main thread only; may complete later.
    fn instantiate(
        &self,
        descriptor: &PluginDescriptor,
        host: HostContext,
        mt: MainThread,
    ) -> Pending<Box<dyn PluginInstance>>;
}

pub trait PluginInstance {
    fn params(&self) -> Vec<ParamInfo>;
    fn set_param(&mut self, mt: MainThread, id: u32, value: f32);
    /// Display text for a value, as the plugin itself would show it.
    fn param_text(&self, id: u32, value: f32) -> String;
    fn save_state(&mut self, mt: MainThread, kind: StateKind) -> Result<Vec<u8>, HostError>;
    /// Replace everything the plugin remembers. Parameter values afterwards come from the plugin.
    fn load_state(&mut self, mt: MainThread, state: &[u8]) -> Result<(), HostError>;
    fn activate(
        &mut self,
        mt: MainThread,
        cfg: &ProcessConfig,
    ) -> Result<(Box<dyn PluginProcessor>, Activated), HostError>;
    fn deactivate(&mut self, mt: MainThread, processor: Box<dyn PluginProcessor>);
    /// Parameter values the processor must be told about because the saved state alone does not
    /// carry them (VST3 only moves an edit into the component through `process`). The host
    /// queues these into every new processor.
    fn pending_params(&self) -> Vec<(u32, f32)> {
        Vec::new()
    }
    /// Current latency in frames (may change after activation or processing).
    fn latency(&self) -> usize;
    fn tail(&self) -> usize;
}

/// Planar audio for one block: `inputs` and `outputs` have the plugin's negotiated channel counts.
pub struct AudioIo<'a> {
    pub inputs: &'a [&'a [f32]],
    pub outputs: &'a mut [&'a mut [f32]],
    pub frames: usize,
}

/// Parameter changes to apply at the start of a block, coalesced to the latest value per id.
pub struct ParamQueue {
    items: [(u32, f32); Self::CAP],
    len: usize,
}

impl ParamQueue {
    pub const CAP: usize = 128;

    pub fn new() -> ParamQueue {
        ParamQueue {
            items: [(0, 0.0); Self::CAP],
            len: 0,
        }
    }

    /// Queue a change, replacing an earlier one for the same parameter. Returns false if full.
    pub fn push(&mut self, id: u32, value: f32) -> bool {
        if let Some(it) = self.items[..self.len].iter_mut().find(|(i, _)| *i == id) {
            it.1 = value;
            return true;
        }
        if self.len == Self::CAP {
            return false;
        }
        self.items[self.len] = (id, value);
        self.len += 1;
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, f32)> + '_ {
        self.items[..self.len].iter().copied()
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for ParamQueue {
    fn default() -> Self {
        ParamQueue::new()
    }
}

pub trait PluginProcessor: Send {
    /// Audio thread: must not allocate, lock or block.
    fn process(&mut self, io: AudioIo<'_>, params: &ParamQueue);
    /// Clear delay lines and tails (a seek, or Apply starting).
    fn reset(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_queue_coalesces_and_caps() {
        let mut q = ParamQueue::new();
        assert!(q.push(1, 0.1) && q.push(2, 0.2) && q.push(1, 0.9));
        assert_eq!(q.iter().collect::<Vec<_>>(), vec![(1, 0.9), (2, 0.2)]);
        q.clear();
        for i in 0..ParamQueue::CAP as u32 {
            assert!(q.push(i, 0.0));
        }
        assert!(
            !q.push(9999, 1.0),
            "full queue drops rather than allocating"
        );
        assert!(q.push(5, 1.0), "but still coalesces");
        assert_eq!(q.len(), ParamQueue::CAP);
    }
}
