//! Reading and writing audio files.

use boothcraft_core::Document;
use std::path::Path;

mod aiff;
mod decode;
mod encode;

pub use decode::read;
pub use encode::{write, BitDepth, Format, WriteOptions};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported or corrupt audio: {0}")]
    Decode(String),
    #[error("cannot write: {0}")]
    Encode(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Format implied by a file extension.
pub fn format_for_path(path: &Path) -> Option<Format> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "wav" | "wave" => Some(Format::Wav),
        "aif" | "aiff" => Some(Format::Aiff),
        "flac" => Some(Format::Flac),
        _ => None,
    }
}

/// Convenience: decode `src`, encode to `dst` using the extension of `dst`.
pub fn convert(src: &Path, dst: &Path, depth: BitDepth) -> Result<Document> {
    let doc = read(src)?;
    let format =
        format_for_path(dst).ok_or_else(|| Error::Encode("unknown output extension".into()))?;
    write(&doc, dst, &WriteOptions { format, depth })?;
    Ok(doc)
}
