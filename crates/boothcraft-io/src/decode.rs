use crate::{Error, Result};
use boothcraft_core::{BlockStore, Document, Rope, BLOCK_FRAMES};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decode any supported file into a document.
pub fn read(path: &Path) -> Result<Document> {
    read_with_progress(path, |_, _| {})
}

/// Like [`read`], but calls `progress(frames_decoded_so_far, declared_frames)` after each decoded
/// packet, so a caller can show a progress indicator for a long file (`declared_frames` is `None`
/// when the container doesn't report a length up front).
///
/// Each packet is pushed straight into the per-channel block store as it's decoded, rather than
/// collecting the whole file into one buffer first: memory use stays bounded by the block store's
/// budget throughout, the same as during playback, instead of needing roughly the whole
/// (uncompressed) file size again just to open it.
pub fn read_with_progress(
    path: &Path,
    mut progress: impl FnMut(usize, Option<usize>),
) -> Result<Document> {
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
    let declared_frames = track.codec_params.n_frames.map(|n| n as usize);
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    let mut ropes: Vec<Rope> = Vec::new();
    // Accumulates decoded packets per channel until there's at least a full block's worth, then
    // flushes in one `push_samples` call. `push_samples` tops up a rope's last (partial) block by
    // rebuilding it, an O(block size) cost; calling it once per small decoded packet, rather than
    // in batches, would pay that cost on nearly every packet and turn filling one block into
    // O(block size squared / packet size) work.
    let mut pending: Vec<Vec<f32>> = Vec::new();
    const FLUSH_THRESHOLD: usize = BLOCK_FRAMES * 4;
    let mut frames_so_far = 0usize;
    // Rate-limits throttle checks to roughly the spiller's own cadence: calling `throttle` (which
    // can wait) on every packet of a file with many small packets would otherwise start a fresh
    // wait window each time, compounding into a far longer total stall than any single call's own
    // timeout suggests, without actually letting the spiller make more progress per check.
    let mut last_throttle = std::time::Instant::now();

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
                let ch = *channels.get_or_insert(spec.channels.count());
                if ch == 0 {
                    continue;
                }
                let buf =
                    sbuf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
                buf.copy_interleaved_ref(decoded);
                let interleaved = buf.samples();
                let frames = interleaved.len() / ch;

                if ropes.is_empty() {
                    ropes = (0..ch).map(|_| Rope::new()).collect();
                    pending = (0..ch).map(|_| Vec::new()).collect();
                }
                // De-interleave into each channel's pending buffer; flushed in batches below.
                for (c, buf) in pending.iter_mut().enumerate() {
                    buf.extend(interleaved.iter().copied().skip(c).step_by(ch));
                }
                if pending[0].len() >= FLUSH_THRESHOLD {
                    for (rope, buf) in ropes.iter_mut().zip(pending.iter_mut()) {
                        rope.push_samples(buf);
                        buf.clear();
                    }
                }
                frames_so_far += frames;
                progress(frames_so_far, declared_frames);
                // Keeps decoding from racing arbitrarily far ahead of the spiller: a cheap no-op
                // once resident is within a couple times the budget (normal lag, not worth
                // pausing for), rate-limited so a file with many small packets doesn't turn this
                // into near-continuous waiting.
                if last_throttle.elapsed() >= std::time::Duration::from_millis(200) {
                    BlockStore::global().throttle(2, std::time::Duration::from_secs(2));
                    last_throttle = std::time::Instant::now();
                }
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
    if ropes.is_empty() {
        // A track was present but nothing came out of it (e.g. zero decodable packets); produce
        // an empty-but-valid document rather than failing.
        ropes = (0..ch).map(|_| Rope::new()).collect();
    } else {
        for (rope, buf) in ropes.iter_mut().zip(&pending) {
            rope.push_samples(buf);
        }
    }
    // Codecs may decode a longer final block than the container declares; trim to the declared
    // length exactly, same as the whole-buffer path this replaced.
    if let Some(n) = declared_frames {
        for rope in &mut ropes {
            if rope.len() > n {
                rope.delete(n..rope.len());
            }
        }
    }
    let mut doc = Document::new(rate, ropes);
    let is_wav = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("wave"));
    if is_wav {
        // Markers are best-effort: a malformed chunk must not stop the audio from opening.
        for m in crate::riff::read_markers(path).unwrap_or_default() {
            doc.add_marker(m);
        }
    }
    // A fast decode (reading is far cheaper than the file was to encode) can otherwise finish
    // well before the background spiller gets a chance to bring a big file back under budget.
    BlockStore::global().settle(std::time::Duration::from_secs(5));
    Ok(doc)
}
