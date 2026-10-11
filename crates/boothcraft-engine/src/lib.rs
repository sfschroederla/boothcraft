//! Real-time audio engine: output device, transport and metering.
//!
//! The audio callback never locks, allocates or blocks. The UI talks to it through atomics and an
//! [`arc_swap::ArcSwapOption`] holding the current document snapshot.

mod device;
mod shared;

pub use device::{Engine, EngineError};
pub use shared::{Renderer, Shared};
