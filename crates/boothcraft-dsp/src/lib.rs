//! Signal processing for Boothcraft: format conversion, effects, the Effects Rack and presets.

pub mod biquad;
pub mod convert;
pub mod delayline;
pub mod effect;
pub mod effects;
pub mod generate;
pub mod loudness;
pub mod param;
pub mod preset;
pub mod rack;
pub mod registry;

pub use effect::{Effect, Envelope, OfflineEffect};
pub use param::{Curve, ParamSpec, Unit};
pub use rack::{Rack, RackDesc, SlotDesc};
