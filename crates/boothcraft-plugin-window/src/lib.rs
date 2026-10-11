//! Native windows that host plugin editors.
//!
//! A plugin draws its own interface into a native view we give it. On macOS this is an `NSWindow`
//! whose content view is handed to the plugin, owned by (a child of) the app's main window so it
//! floats above it. Everything here must be called on the main thread.

// This crate calls into AppKit, so it cannot forbid unsafe code (see Cargo.toml).
#![allow(unsafe_code)]

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::{any_editor_is_key, EditorWindow};

#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "macos"))]
pub use other::{any_editor_is_key, EditorWindow};
