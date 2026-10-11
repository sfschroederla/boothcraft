use crate::block::{Block, Peak, BLOCK_FRAMES, PEAK_STRIDE};
use std::ops::Range;
use std::sync::Arc;

/// One channel of audio: a sequence of shared immutable blocks.
///
/// Cloning is O(blocks); edits copy only the blocks they touch.
#[derive(Clone, Debug, Default)]
pub struct Rope {
    blocks: Vec<Arc<Block>>,
    len: usize,
}

impl Rope {
    pub fn new() -> Rope {
        Rope::default()
    }

    pub fn from_samples(samples: &[f32]) -> Rope {
        let mut rope = Rope::new();
        rope.push_samples(samples);
        rope
    }

    pub fn silence(frames: usize) -> Rope {
        let zeros = vec![0.0; BLOCK_FRAMES.min(frames)];
        let mut rope = Rope::new();
        let mut left = frames;
        while left > 0 {
            let n = left.min(BLOCK_FRAMES);
            rope.push_block(Block::new(zeros[..n].to_vec()));
            left -= n;
        }
        rope
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn blocks(&self) -> &[Arc<Block>] {
        &self.blocks
    }

    pub fn push_block(&mut self, block: Arc<Block>) {
        if block.is_empty() {
            return;
        }
        self.len += block.len();
        self.blocks.push(block);
    }

    pub fn push_samples(&mut self, samples: &[f32]) {
        for chunk in samples.chunks(BLOCK_FRAMES) {
            self.push_block(Block::new(chunk.to_vec()));
        }
    }

    pub fn append(&mut self, other: &Rope) {
        for b in &other.blocks {
            self.push_block(b.clone());
        }
    }

    /// Frames `range` as a new rope, sharing whole blocks with `self`.
    pub fn slice(&self, range: Range<usize>) -> Rope {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        let mut out = Rope::new();
        let mut pos = 0;
        for b in &self.blocks {
            let (bs, be) = (pos, pos + b.len());
            pos = be;
            if be <= start {
                continue;
            }
            if bs >= end {
                break;
            }
            let (from, to) = (start.max(bs) - bs, end.min(be) - bs);
            if from == 0 && to == b.len() {
                out.push_block(b.clone());
            } else {
                out.push_block(Block::new(b.samples()[from..to].to_vec()));
            }
        }
        out
    }

    /// Remove `range`, returning the removed audio.
    pub fn delete(&mut self, range: Range<usize>) -> Rope {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        let removed = self.slice(start..end);
        let mut out = self.slice(0..start);
        out.append(&self.slice(end..self.len));
        *self = out;
        removed
    }

    /// Insert `other` at frame `at`.
    pub fn insert(&mut self, at: usize, other: &Rope) {
        let at = at.min(self.len);
        let mut out = self.slice(0..at);
        out.append(other);
        out.append(&self.slice(at..self.len));
        *self = out;
    }

    /// Replace `range` with the result of `f` applied to its samples (same length).
    pub fn map_range(&mut self, range: Range<usize>, f: impl FnOnce(&mut [f32])) {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        if start == end {
            return;
        }
        let mut buf = self.to_vec(start..end);
        f(&mut buf);
        let mut out = self.slice(0..start);
        out.push_samples(&buf);
        out.append(&self.slice(end..self.len));
        *self = out;
    }

    /// Copy `range` into a contiguous vector.
    pub fn to_vec(&self, range: Range<usize>) -> Vec<f32> {
        let mut v = vec![0.0; range.end.min(self.len).saturating_sub(range.start)];
        self.read(range.start, &mut v);
        v
    }

    /// Fill `out` from frame `start`; frames past the end are zero. Returns frames copied.
    pub fn read(&self, start: usize, out: &mut [f32]) -> usize {
        let mut pos = 0;
        let mut copied = 0;
        let end = start + out.len();
        for b in &self.blocks {
            let (bs, be) = (pos, pos + b.len());
            pos = be;
            if be <= start {
                continue;
            }
            if bs >= end {
                break;
            }
            let (from, to) = (start.max(bs) - bs, end.min(be) - bs);
            let dst = start.max(bs) - start;
            out[dst..dst + (to - from)].copy_from_slice(&b.samples()[from..to]);
            copied += to - from;
        }
        out[copied..].fill(0.0);
        copied
    }

    /// Summarise `range` into `columns` peaks. Uses block overviews when columns are wide.
    pub fn peaks(&self, range: Range<usize>, columns: usize) -> Vec<Peak> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        if columns == 0 || start == end {
            return vec![Peak::EMPTY; columns];
        }
        let span = end - start;
        let per_col = span as f64 / columns as f64;
        let use_overview = per_col >= (PEAK_STRIDE * 2) as f64;
        let mut cols: Vec<Option<(Peak, usize)>> = vec![None; columns];
        let mut add = |col: usize, p: Peak, n: usize| {
            let c = &mut cols[col.min(columns - 1)];
            *c = Some(match c.take() {
                Some((q, m)) => (Peak::merge(q, m, p, n), m + n),
                None => (p, n),
            });
        };
        let mut pos = 0;
        for b in &self.blocks {
            let (bs, be) = (pos, pos + b.len());
            pos = be;
            if be <= start {
                continue;
            }
            if bs >= end {
                break;
            }
            let (from, to) = (start.max(bs), end.min(be));
            if use_overview {
                let mut f = from;
                while f < to {
                    let cell = (f - bs) / PEAK_STRIDE;
                    let cell_end = (bs + (cell + 1) * PEAK_STRIDE).min(be).min(to);
                    let col = (((f - start) as f64) / per_col) as usize;
                    let whole = f == bs + cell * PEAK_STRIDE
                        && cell_end == (bs + (cell + 1) * PEAK_STRIDE).min(be);
                    let p = if whole {
                        b.peaks()[cell]
                    } else {
                        Peak::of(&b.samples()[f - bs..cell_end - bs])
                    };
                    add(col, p, cell_end - f);
                    f = cell_end;
                }
            } else {
                let mut f = from;
                while f < to {
                    let col = (((f - start) as f64) / per_col) as usize;
                    let col_end = (start as f64 + (col as f64 + 1.0) * per_col).ceil() as usize;
                    let stop = col_end.clamp(f + 1, to);
                    add(col, Peak::of(&b.samples()[f - bs..stop - bs]), stop - f);
                    f = stop;
                }
            }
        }
        cols.into_iter()
            .map(|c| c.map_or(Peak::EMPTY, |(p, _)| p))
            .collect()
    }
}
