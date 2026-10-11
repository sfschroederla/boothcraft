//! Plugin state in the `.vstpreset` chunk layout.
//!
//! ```text
//! "VST3" | version i32 = 1 | class id (32 ASCII hex) | list offset i64
//! chunk data ... | "List" | count i32 | { id[4], offset i64, size i64 } * count
//! ```
//! with a `Comp` chunk (the component's state) and a `Cont` chunk (the controller's). Slots store
//! this blob, and `.vstpreset` files from other hosts load through the same reader.
//!
//! A third chunk, `BCed`, is ours: parameter edits made in the generic UI that the component has
//! not yet absorbed (it only learns of an edit through `process`). Other hosts ignore the chunk.

use boothcraft_plugin::HostError;

const MAGIC: &[u8; 4] = b"VST3";
const HEADER: usize = 4 + 4 + 32 + 8;

#[derive(Debug, Default, PartialEq)]
pub struct Parts {
    pub component: Vec<u8>,
    pub controller: Option<Vec<u8>>,
    /// Edits not yet in the component's state: (parameter id, our plain value).
    pub edits: Vec<(u32, f32)>,
}

pub fn write(class_id_hex: &str, parts: &Parts) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1i32.to_le_bytes());
    let mut id = class_id_hex.as_bytes().to_vec();
    id.resize(32, b'0');
    out.extend_from_slice(&id[..32]);
    out.extend_from_slice(&0i64.to_le_bytes()); // list offset, patched below
    let mut entries: Vec<([u8; 4], i64, i64)> = Vec::new();
    let mut chunk = |tag: &[u8; 4], data: &[u8], out: &mut Vec<u8>| {
        entries.push((*tag, out.len() as i64, data.len() as i64));
        out.extend_from_slice(data);
    };
    chunk(b"Comp", &parts.component, &mut out);
    if let Some(c) = &parts.controller {
        chunk(b"Cont", c, &mut out);
    }
    if !parts.edits.is_empty() {
        let mut e = (parts.edits.len() as u32).to_le_bytes().to_vec();
        for (id, v) in &parts.edits {
            e.extend_from_slice(&id.to_le_bytes());
            e.extend_from_slice(&v.to_le_bytes());
        }
        chunk(b"BCed", &e, &mut out);
    }
    let list_at = out.len() as i64;
    out.extend_from_slice(b"List");
    out.extend_from_slice(&(entries.len() as i32).to_le_bytes());
    for (tag, off, size) in entries {
        out.extend_from_slice(&tag);
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    out[HEADER - 8..HEADER].copy_from_slice(&list_at.to_le_bytes());
    out
}

pub fn read(bytes: &[u8]) -> Result<Parts, HostError> {
    let bad = |why: &str| HostError::State(format!("not a VST3 state blob: {why}"));
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return Err(bad("missing header"));
    }
    let i64_at = |at: usize| -> Result<i64, HostError> {
        bytes
            .get(at..at + 8)
            .map(|b| i64::from_le_bytes(b.try_into().expect("8 bytes")))
            .ok_or_else(|| bad("truncated"))
    };
    let list = usize::try_from(i64_at(HEADER - 8)?).map_err(|_| bad("bad list offset"))?;
    if bytes.get(list..list + 4) != Some(b"List") {
        return Err(bad("missing chunk list"));
    }
    let count = bytes
        .get(list + 4..list + 8)
        .map(|b| i32::from_le_bytes(b.try_into().expect("4 bytes")))
        .ok_or_else(|| bad("truncated"))?;
    let mut parts = Parts::default();
    let mut have_comp = false;
    for k in 0..count.max(0) as usize {
        let at = list + 8 + k * 20;
        let tag = bytes.get(at..at + 4).ok_or_else(|| bad("truncated list"))?;
        let off = usize::try_from(i64_at(at + 4)?).map_err(|_| bad("bad chunk offset"))?;
        let size = usize::try_from(i64_at(at + 12)?).map_err(|_| bad("bad chunk size"))?;
        let data = bytes
            .get(off..off.checked_add(size).ok_or_else(|| bad("bad chunk"))?)
            .ok_or_else(|| bad("chunk outside file"))?;
        match tag {
            b"Comp" => {
                parts.component = data.to_vec();
                have_comp = true;
            }
            b"Cont" => parts.controller = Some(data.to_vec()),
            b"BCed" => parts.edits = read_edits(data),
            _ => {}
        }
    }
    if !have_comp {
        return Err(bad("no component chunk"));
    }
    Ok(parts)
}

/// Tolerant of a damaged chunk: whatever whole entries it holds.
fn read_edits(data: &[u8]) -> Vec<(u32, f32)> {
    let Some(n) = data
        .get(..4)
        .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
    else {
        return Vec::new();
    };
    data[4..]
        .as_chunks::<8>()
        .0
        .iter()
        .take(n as usize)
        .map(|c| {
            (
                u32::from_le_bytes(c[..4].try_into().expect("4 bytes")),
                f32::from_le_bytes(c[4..].try_into().expect("4 bytes")),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_both_chunks() {
        let parts = Parts {
            component: (0..=255).collect(),
            controller: Some(vec![9, 8, 7]),
            edits: vec![(7, 0.5), (u32::MAX, -2.0)],
        };
        let blob = write(&"AB".repeat(16), &parts);
        assert_eq!(&blob[..4], b"VST3");
        assert_eq!(read(&blob).unwrap(), parts);
        let only = Parts {
            component: vec![1],
            controller: None,
            edits: Vec::new(),
        };
        assert_eq!(read(&write("0", &only)).unwrap(), only);
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert!(read(b"").is_err());
        assert!(read(b"VST3 not really").is_err());
        let mut blob = write(&"00".repeat(16), &Parts::default());
        let n = blob.len();
        blob.truncate(n - 3);
        assert!(read(&blob).is_err());
        // A corrupt chunk size must not read out of bounds.
        let mut blob = write(
            &"00".repeat(16),
            &Parts {
                component: vec![1, 2],
                controller: None,
                edits: Vec::new(),
            },
        );
        let list = i64::from_le_bytes(blob[HEADER - 8..HEADER].try_into().unwrap()) as usize;
        blob[list + 8 + 12..list + 8 + 20].copy_from_slice(&i64::MAX.to_le_bytes());
        assert!(read(&blob).is_err());
    }
}
