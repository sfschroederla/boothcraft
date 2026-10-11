use crate::{Error, Result};
use std::io::Write;

/// IEEE 754 80-bit extended encoding of an integer sample rate.
fn extended(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let exp = 31 - rate.leading_zeros();
    let mantissa = (rate as u64) << (63 - exp);
    out[0..2].copy_from_slice(&((16383 + exp) as u16).to_be_bytes());
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

/// Write big-endian integer PCM AIFF. `samples` yields exactly `frames * channels` interleaved
/// values already in `[-1, 1]`, pulled on demand rather than needing them all in memory at once.
pub fn write_aiff<W: Write>(
    w: &mut W,
    rate: u32,
    channels: usize,
    bits: u16,
    frames: usize,
    samples: impl Iterator<Item = f32>,
) -> Result<()> {
    if !matches!(bits, 8 | 16 | 24 | 32) {
        return Err(Error::Encode(format!("AIFF does not support {bits}-bit")));
    }
    let bytes = (bits / 8) as usize;
    let data_len = frames * channels * bytes;
    let pad = data_len % 2;
    let form_len = 4 + (8 + 18) + (8 + 8 + data_len + pad);
    w.write_all(b"FORM")?;
    w.write_all(&(form_len as u32).to_be_bytes())?;
    w.write_all(b"AIFF")?;
    w.write_all(b"COMM")?;
    w.write_all(&18u32.to_be_bytes())?;
    w.write_all(&(channels as u16).to_be_bytes())?;
    w.write_all(&(frames as u32).to_be_bytes())?;
    w.write_all(&bits.to_be_bytes())?;
    w.write_all(&extended(rate))?;
    w.write_all(b"SSND")?;
    w.write_all(&((data_len + 8) as u32).to_be_bytes())?;
    w.write_all(&[0u8; 8])?;
    let max = (1i64 << (bits - 1)) as f32;
    let mut written = 0usize;
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * max).round().clamp(-max, max - 1.0) as i32;
        let be = v.to_be_bytes();
        w.write_all(&be[4 - bytes..])?;
        written += 1;
    }
    debug_assert_eq!(
        written,
        frames * channels,
        "the sample iterator must yield exactly frames * channels values"
    );
    if pad == 1 {
        w.write_all(&[0])?;
    }
    Ok(())
}
