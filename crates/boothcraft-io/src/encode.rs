use crate::aiff::write_aiff;
use crate::{Error, Result};
use boothcraft_core::{Document, Rope};
use boothcraft_dsp::convert::Dither;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Wav,
    Aiff,
    Flac,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

impl BitDepth {
    fn bits(self) -> u16 {
        match self {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => 32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WriteOptions {
    pub format: Format,
    pub depth: BitDepth,
    /// Resample to this rate on export (`None` keeps the document's rate).
    pub sample_rate: Option<u32>,
    /// Add TPDF dither when writing integer samples.
    pub dither: bool,
}

impl WriteOptions {
    pub fn new(format: Format, depth: BitDepth) -> WriteOptions {
        WriteOptions {
            format,
            depth,
            sample_rate: None,
            dither: false,
        }
    }
}

pub fn write(doc: &Document, path: &Path, opts: &WriteOptions) -> Result<()> {
    // A courtesy for a caller that just finished a big batch of block creation (e.g. `convert`
    // decoding straight into writing again): give the spiller a chance to catch up first rather
    // than racing it through the whole encode.
    boothcraft_core::BlockStore::global().settle(std::time::Duration::from_secs(5));
    let resampled;
    let doc = match opts.sample_rate {
        Some(rate) if rate != doc.sample_rate => {
            resampled = boothcraft_dsp::convert::resample_document(doc, rate);
            &resampled
        }
        _ => doc,
    };
    let ch = doc.channel_count();
    let dither = (opts.dither && opts.depth != BitDepth::Float32).then(|| Dither::new(0x9E37_79B9));
    match opts.format {
        Format::Wav => {
            write_wav(path, doc, opts.depth, dither)?;
            crate::riff::append_markers(path, &doc.markers)
        }
        Format::Aiff => {
            if opts.depth == BitDepth::Float32 {
                return Err(Error::Encode(
                    "AIFF float is not supported; choose 16 or 24-bit".into(),
                ));
            }
            let mut w = BufWriter::new(File::create(path)?);
            let frames = doc.frames();
            let samples =
                InterleavedStream::new(&doc.channels, frames, opts.depth.bits() as u32, dither);
            write_aiff(
                &mut w,
                doc.sample_rate,
                ch,
                opts.depth.bits(),
                frames,
                samples,
            )
        }
        Format::Flac => write_flac(path, doc, opts.depth, dither),
    }
}

fn write_wav(path: &Path, doc: &Document, depth: BitDepth, dither: Option<Dither>) -> Result<()> {
    let spec = hound::WavSpec {
        channels: doc.channel_count() as u16,
        sample_rate: doc.sample_rate,
        bits_per_sample: depth.bits(),
        sample_format: if depth == BitDepth::Float32 {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let enc = |e: hound::Error| Error::Encode(e.to_string());
    let mut w = hound::WavWriter::create(path, spec).map_err(enc)?;
    let samples = InterleavedStream::new(&doc.channels, doc.frames(), depth.bits() as u32, dither);
    match depth {
        BitDepth::Float32 => samples
            .map(Ok)
            .try_for_each(|s: std::result::Result<f32, hound::Error>| w.write_sample(s?))
            .map_err(enc)?,
        d => {
            let max = (1i64 << (d.bits() - 1)) as f32;
            samples
                .map(|s| (s.clamp(-1.0, 1.0) * max).round().clamp(-max, max - 1.0) as i32)
                .try_for_each(|s| w.write_sample(s))
                .map_err(enc)?
        }
    }
    w.finalize().map_err(enc)
}

fn write_flac(path: &Path, doc: &Document, depth: BitDepth, dither: Option<Dither>) -> Result<()> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    let bits = match depth {
        BitDepth::Int16 => 16usize,
        BitDepth::Int24 => 24,
        BitDepth::Float32 => {
            return Err(Error::Encode(
                "FLAC cannot store 32-bit float; choose 16 or 24-bit".into(),
            ))
        }
    };
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| Error::Encode(format!("{e:?}")))?;
    let source = RopeSource {
        samples: InterleavedStream::new(&doc.channels, doc.frames(), bits as u32, dither),
        channels: doc.channel_count(),
        bits_per_sample: bits,
        sample_rate: doc.sample_rate as usize,
        total_frames: doc.frames(),
        scratch: Vec::new(),
    };
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| Error::Encode(format!("{e:?}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| Error::Encode(format!("{e:?}")))?;
    std::fs::write(path, sink.as_slice())?;
    Ok(())
}

/// Yields interleaved samples straight from a document's ropes, one chunk of frames at a time, so
/// no writer ever needs the whole file materialized as one buffer. Dither (if any) is applied per
/// chunk but advances continuously across chunks, matching how it would apply to one flat buffer.
struct InterleavedStream<'a> {
    channels: &'a [Rope],
    pos: usize,
    total: usize,
    bits: u32,
    dither: Option<Dither>,
    scratch: Vec<Vec<f32>>,
    chunk: Vec<f32>,
    chunk_pos: usize,
}

/// Frames pulled from the ropes per refill.
const CHUNK_FRAMES: usize = 8192;

impl<'a> InterleavedStream<'a> {
    fn new(channels: &'a [Rope], total: usize, bits: u32, dither: Option<Dither>) -> Self {
        InterleavedStream {
            channels,
            pos: 0,
            total,
            bits,
            dither,
            scratch: vec![vec![0.0; CHUNK_FRAMES]; channels.len()],
            chunk: Vec::new(),
            chunk_pos: 0,
        }
    }

    fn refill(&mut self) {
        self.chunk.clear();
        self.chunk_pos = 0;
        if self.pos >= self.total {
            return;
        }
        let n = (self.total - self.pos).min(CHUNK_FRAMES);
        for (c, rope) in self.channels.iter().enumerate() {
            rope.read(self.pos, &mut self.scratch[c][..n]);
        }
        self.chunk.resize(n * self.channels.len(), 0.0);
        for i in 0..n {
            for (c, scratch) in self.scratch.iter().enumerate() {
                self.chunk[i * self.channels.len() + c] = scratch[i];
            }
        }
        if let Some(d) = &mut self.dither {
            d.apply(&mut self.chunk, self.bits);
        }
        self.pos += n;
    }
}

impl Iterator for InterleavedStream<'_> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.chunk_pos >= self.chunk.len() {
            self.refill();
            if self.chunk.is_empty() {
                return None;
            }
        }
        let v = self.chunk[self.chunk_pos];
        self.chunk_pos += 1;
        Some(v)
    }
}

/// Adapts an [`InterleavedStream`] to `flacenc`'s [`flacenc::source::Source`], quantizing to
/// integer samples a block at a time instead of needing the whole file as one `i32` buffer.
struct RopeSource<'a> {
    samples: InterleavedStream<'a>,
    channels: usize,
    bits_per_sample: usize,
    sample_rate: usize,
    total_frames: usize,
    scratch: Vec<i32>,
}

impl flacenc::source::Source for RopeSource<'_> {
    fn channels(&self) -> usize {
        self.channels
    }

    fn bits_per_sample(&self) -> usize {
        self.bits_per_sample
    }

    fn sample_rate(&self) -> usize {
        self.sample_rate
    }

    fn len_hint(&self) -> Option<usize> {
        Some(self.total_frames)
    }

    fn read_samples<F: flacenc::source::Fill>(
        &mut self,
        block_size: usize,
        dest: &mut F,
    ) -> std::result::Result<usize, flacenc::error::SourceError> {
        let max = (1i64 << (self.bits_per_sample - 1)) as f32;
        self.scratch.clear();
        self.scratch.extend(
            (&mut self.samples)
                .take(block_size * self.channels)
                .map(|s| (s.clamp(-1.0, 1.0) * max).round().clamp(-max, max - 1.0) as i32),
        );
        let frames = self.scratch.len() / self.channels;
        if frames > 0 {
            dest.fill_interleaved(&self.scratch).map_err(|_| {
                flacenc::error::SourceError::by_reason(
                    flacenc::error::SourceErrorReason::InvalidBuffer,
                )
            })?;
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_stream_matches_a_flat_interleave_across_a_chunk_boundary() {
        let n = CHUNK_FRAMES * 2 + 100; // spans more than two refills
        let l: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let r: Vec<f32> = l.iter().map(|x| -x).collect();
        let channels = vec![Rope::from_samples(&l), Rope::from_samples(&r)];
        let got: Vec<f32> = InterleavedStream::new(&channels, n, 16, None).collect();
        assert_eq!(got.len(), n * 2);
        let expected: Vec<f32> = (0..n).flat_map(|i| [l[i], r[i]]).collect();
        assert_eq!(got, expected);
    }
}
