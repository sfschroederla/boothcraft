//! Document model for Boothcraft: immutable block storage, edits, undo, selection and markers.

pub mod block;
pub mod clipboard;
pub mod document;
pub mod history;
pub mod marker;
pub mod rope;
pub mod selection;

pub use block::{Block, Peak, BLOCK_FRAMES, PEAK_STRIDE};
pub use clipboard::Clipboards;
pub use document::{ChannelMask, Document};
pub use history::History;
pub use marker::{Marker, MarkerKind};
pub use rope::Rope;
pub use selection::Selection;
