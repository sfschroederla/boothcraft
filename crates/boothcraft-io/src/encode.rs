use crate::aiff::write_aiff;
use crate::{Error, Result};
use boothcraft_core::Document;
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
    let resampled;
    let doc = match opts.sample_rate {
        Some(rate) if rate != doc.sample_rate => {
            resampled = boothcraft_dsp::convert::resample_document(doc, rate);
            &resampled
        }
        _ => doc,
    };
    let mut samples = doc.to_interleaved(0..doc.frames());
    if opts.dither && opts.depth != BitDepth::Float32 {
        boothcraft_dsp::convert::Dither::new(0x9E37_79B9)
            .apply(&mut samples, opts.depth.bits() as u32);
    }
    let ch = doc.channel_count();
    match opts.format {
        Format::Wav => {
            write_wav(path, doc.sample_rate, ch, opts.depth, &samples)?;
            crate::riff::append_markers(path, &doc.markers)
        }
        Format::Aiff => {
            if opts.depth == BitDepth::Float32 {
                return Err(Error::Encode(
                    "AIFF float is not supported; choose 16 or 24-bit".into(),
                ));
            }
            let mut w = BufWriter::new(File::create(path)?);
            write_aiff(&mut w, doc.sample_rate, ch, opts.depth.bits(), &samples)
        }
        Format::Flac => write_flac(path, doc.sample_rate, ch, opts.depth, &samples),
    }
}

fn write_wav(path: &Path, rate: u32, ch: usize, depth: BitDepth, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: ch as u16,
        sample_rate: rate,
        bits_per_sample: depth.bits(),
        sample_format: if depth == BitDepth::Float32 {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let enc = |e: hound::Error| Error::Encode(e.to_string());
    let mut w = hound::WavWriter::create(path, spec).map_err(enc)?;
    match depth {
        BitDepth::Float32 => samples
            .iter()
            .try_for_each(|&s| w.write_sample(s))
            .map_err(enc)?,
        d => {
            let max = (1i64 << (d.bits() - 1)) as f32;
            samples
                .iter()
                .try_for_each(|&s| {
                    w.write_sample((s.clamp(-1.0, 1.0) * max).round().clamp(-max, max - 1.0) as i32)
                })
                .map_err(enc)?
        }
    }
    w.finalize().map_err(enc)
}

fn write_flac(path: &Path, rate: u32, ch: usize, depth: BitDepth, samples: &[f32]) -> Result<()> {
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
    let max = (1i64 << (bits - 1)) as f32;
    let ints: Vec<i32> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * max).round().clamp(-max, max - 1.0) as i32)
        .collect();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| Error::Encode(format!("{e:?}")))?;
    let source = flacenc::source::MemSource::from_samples(&ints, ch, bits, rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| Error::Encode(format!("{e:?}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| Error::Encode(format!("{e:?}")))?;
    std::fs::write(path, sink.as_slice())?;
    Ok(())
}
