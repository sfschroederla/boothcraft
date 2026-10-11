//! VST3 plugin hosting.
//!
//! Implements `boothcraft_plugin`'s format traits on top of the `vst3` bindings. Everything that
//! calls into a plugin is here; the rest of the app stays free of unsafe code.

// The SDK's enum constants are `u32` on some platforms and `i32` on others, so casts that are
// redundant on one platform are needed on another.
#![allow(clippy::unnecessary_cast)]

mod editor;
mod format;
mod host;
mod instance;
mod module;
mod params;
mod processor;
mod state;
mod stream;
mod util;

pub use format::Vst3Format;
