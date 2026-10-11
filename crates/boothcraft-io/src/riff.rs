//! WAV marker chunks: `cue ` points with `LIST/adtl` labels (`labl`) and region lengths (`ltxt`).
//!
//! `hound` cannot write extra chunks, so markers are appended after the audio is finalized and the
//! RIFF size is patched.

use crate::Result;
use boothcraft_core::{Marker, MarkerKind};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Iterate `(fourcc, body)` sub-chunks of a RIFF-style byte range.
fn chunks(mut b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if b.len() < 8 {
            return None;
        }
        let id: [u8; 4] = b[0..4].try_into().ok()?;
        let len = u32_at(b, 4)? as usize;
        let body = b.get(8..8 + len.min(b.len() - 8))?;
        let advance = (8 + len + (len & 1)).min(b.len());
        b = &b[advance..];
        Some((id, body))
    })
}

fn c_string(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Parse markers from the bytes of a whole WAV file. Unknown or malformed chunks are ignored.
pub fn parse_markers(file: &[u8]) -> Vec<Marker> {
    if file.len() < 12 || &file[0..4] != b"RIFF" || &file[8..12] != b"WAVE" {
        return Vec::new();
    }
    let mut points: Vec<(u32, u32)> = Vec::new();
    let mut labels: HashMap<u32, String> = HashMap::new();
    let mut lengths: HashMap<u32, u32> = HashMap::new();
    for (id, body) in chunks(&file[12..]) {
        match &id {
            b"cue " => {
                let n = u32_at(body, 0).unwrap_or(0) as usize;
                for k in 0..n {
                    let e = 4 + k * 24;
                    if let (Some(cue_id), Some(offset)) = (u32_at(body, e), u32_at(body, e + 20)) {
                        points.push((cue_id, offset));
                    }
                }
            }
            b"LIST" if body.get(0..4) == Some(b"adtl") => {
                for (sub, sb) in chunks(&body[4..]) {
                    let Some(cue_id) = u32_at(sb, 0) else {
                        continue;
                    };
                    match &sub {
                        b"labl" => {
                            labels.insert(cue_id, c_string(&sb[4..]));
                        }
                        b"ltxt" => {
                            if let Some(len) = u32_at(sb, 4) {
                                lengths.insert(cue_id, len);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<Marker> = points
        .into_iter()
        .enumerate()
        .map(|(k, (cue_id, at))| {
            let name = labels
                .remove(&cue_id)
                .unwrap_or_else(|| format!("Marker {:02}", k + 1));
            let kind = match lengths.get(&cue_id) {
                Some(&len) if len > 0 => MarkerKind::Range { len: len as usize },
                _ => MarkerKind::Cue,
            };
            Marker {
                name,
                at: at as usize,
                kind,
            }
        })
        .collect();
    out.sort_by_key(|m| m.at);
    out
}

fn chunk(id: &[u8; 4], body: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

/// Encode `cue ` and `LIST/adtl` chunks for `markers`. Positions beyond 32 bits are skipped.
pub fn encode_markers(markers: &[Marker]) -> Vec<u8> {
    let usable: Vec<&Marker> = markers
        .iter()
        .filter(|m| m.end() <= u32::MAX as usize)
        .collect();
    if usable.is_empty() {
        return Vec::new();
    }
    let mut cue = (usable.len() as u32).to_le_bytes().to_vec();
    let mut adtl = b"adtl".to_vec();
    for (k, m) in usable.iter().enumerate() {
        let id = k as u32 + 1;
        let at = m.at as u32;
        for v in [id, at] {
            cue.extend_from_slice(&v.to_le_bytes());
        }
        cue.extend_from_slice(b"data");
        for v in [0u32, 0, at] {
            cue.extend_from_slice(&v.to_le_bytes());
        }
        let mut labl = id.to_le_bytes().to_vec();
        labl.extend_from_slice(m.name.as_bytes());
        labl.push(0);
        chunk(b"labl", &labl, &mut adtl);
        if let MarkerKind::Range { len } = m.kind {
            let mut ltxt = id.to_le_bytes().to_vec();
            ltxt.extend_from_slice(&(len as u32).to_le_bytes());
            ltxt.extend_from_slice(b"rgn ");
            ltxt.extend_from_slice(&[0u8; 8]);
            chunk(b"ltxt", &ltxt, &mut adtl);
        }
    }
    let mut out = Vec::new();
    chunk(b"cue ", &cue, &mut out);
    chunk(b"LIST", &adtl, &mut out);
    out
}

/// Read markers from a WAV file on disk.
pub fn read_markers(path: &Path) -> Result<Vec<Marker>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    Ok(parse_markers(&bytes))
}

/// Append marker chunks to a finished WAV file and fix the RIFF size.
pub fn append_markers(path: &Path, markers: &[Marker]) -> Result<()> {
    let extra = encode_markers(markers);
    if extra.is_empty() {
        return Ok(());
    }
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let mut len = f.seek(SeekFrom::End(0))?;
    if len % 2 == 1 {
        f.write_all(&[0])?;
        len += 1;
    }
    f.write_all(&extra)?;
    let riff_size = (len + extra.len() as u64 - 8) as u32;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&riff_size.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_parse_roundtrip() {
        let markers = vec![
            Marker::cue("Intro", 10),
            Marker::range("Verse", 500, 250),
            Marker::cue("odd", 7),
        ];
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend(encode_markers(&markers));
        let back = parse_markers(&wav);
        assert_eq!(back.len(), 3);
        assert_eq!(back[0], Marker::cue("odd", 7));
        assert_eq!(back[1], Marker::cue("Intro", 10));
        assert_eq!(back[2], Marker::range("Verse", 500, 250));
    }

    #[test]
    fn garbage_is_ignored() {
        assert!(parse_markers(b"not a wav file at all").is_empty());
        let mut wav = b"RIFF\0\0\0\0WAVEcue \xff\xff\xff\x7f".to_vec();
        wav.extend_from_slice(&[1, 2, 3]);
        assert!(parse_markers(&wav).is_empty());
    }
}
