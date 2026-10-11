//! Real-time preview through an Effects Rack.
//!
//! The UI builds and prepares a [`Rack`] (allocating) and sends it to the audio thread through a
//! lock-free queue. The audio thread hands the rack it replaces back on a second queue, so the UI
//! thread, not the audio callback, frees it. Parameter tweaks travel as small messages.

use boothcraft_dsp::Rack;

pub enum RackMsg {
    Install(Box<Rack>),
    /// Install `rack`, first moving live effects from the current rack into it: slot `i` takes
    /// the old rack's slot `carry[i]` (`CARRY_NONE` = nothing). Used for plugin instances, which
    /// must survive edits and preview toggles instead of being re-created. Pointer moves only.
    Rebuild {
        rack: Box<Rack>,
        carry: [u8; boothcraft_dsp::rack::SLOTS],
    },
    Clear,
    Param {
        slot: u8,
        index: u16,
        value: f32,
    },
    Bypass {
        slot: u8,
        on: bool,
    },
    Gains {
        input_db: f32,
        output_db: f32,
        mix: f32,
    },
    Power(bool),
}

/// UI-side handle.
pub struct RackControl {
    tx: rtrb::Producer<RackMsg>,
    pub(crate) trash: rtrb::Consumer<Box<Rack>>,
    /// Rate and channel count a preview rack must be prepared for.
    pub sample_rate: u32,
    pub channels: usize,
}

impl RackControl {
    /// Queue a message; returns false if the audio thread has fallen behind and the queue is full.
    /// Retired racks are not freed here: the caller collects them each frame, either with
    /// [`RackControl::collect_garbage`] or, when plugins are hosted, [`RackControl::drain_trash`].
    pub fn send(&mut self, msg: RackMsg) -> bool {
        self.tx.push(msg).is_ok()
    }

    /// Drop racks the audio thread has finished with. Only for callers without plugins; a plugin
    /// host uses [`RackControl::drain_trash`] so retired plugin processors are parked, not dropped.
    pub fn collect_garbage(&mut self) {
        while self.trash.pop().is_ok() {}
    }

    /// Racks the audio thread has finished with, for the caller to retire on the UI thread.
    pub fn drain_trash(&mut self) -> Vec<Box<Rack>> {
        std::iter::from_fn(|| self.trash.pop().ok()).collect()
    }
}

/// Audio-thread side, owned by the renderer.
pub struct RackLink {
    pub(crate) rx: rtrb::Consumer<RackMsg>,
    pub(crate) trash: rtrb::Producer<Box<Rack>>,
}

pub fn rack_channel(sample_rate: u32, channels: usize) -> (RackControl, RackLink) {
    let (tx, rx) = rtrb::RingBuffer::new(1024);
    let (trash_tx, trash_rx) = rtrb::RingBuffer::new(64);
    (
        RackControl {
            tx,
            trash: trash_rx,
            sample_rate,
            channels,
        },
        RackLink {
            rx,
            trash: trash_tx,
        },
    )
}
