use crate::{Error, Result};
use boothcraft_core::Document;
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decode any supported file into a float document.
pub fn read(path: &Path) -> Result<Document> {
    let file = File::open(path)?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(e.to_string()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| Error::Decode("no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let mut sample_rate = track.codec_params.sample_rate;
    let mut channels = track.codec_params.channels.map(|c| c.count());
    let declared_frames = track.codec_params.n_frames;
    let mut interleaved: Vec<f32> = Vec::new();
    let mut sbuf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(Error::Decode(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                sample_rate.get_or_insert(spec.rate);
                channels.get_or_insert(spec.channels.count());
                let buf =
                    sbuf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
                buf.copy_interleaved_ref(decoded);
                interleaved.extend_from_slice(buf.samples());
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(Error::Decode(e.to_string())),
        }
    }

    let (rate, ch) = match (sample_rate, channels) {
        (Some(r), Some(c)) if c > 0 => (r, c),
        _ => {
            return Err(Error::Decode(
                "could not determine sample rate or channels".into(),
            ))
        }
    };
    // Codecs may emit a full final block; the container's declared length is authoritative.
    if let Some(n) = declared_frames {
        interleaved.truncate((n as usize).saturating_mul(ch).min(interleaved.len()));
    }
    Ok(Document::from_interleaved(rate, ch, &interleaved))
}
