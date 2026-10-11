//! Real-time audio engine: output device, transport and metering.
//!
//! The audio callback never locks, allocates or blocks. The UI talks to it through atomics and an
//! [`arc_swap::ArcSwapOption`] holding the current document snapshot.

mod device;
mod rackctl;
mod recorder;
mod shared;
mod take;

pub use device::{Engine, EngineError, Tap};
pub use rackctl::{rack_channel, RackControl, RackLink, RackMsg};
pub use recorder::Recorder;
pub use shared::{Renderer, Shared};
pub use take::Take;
