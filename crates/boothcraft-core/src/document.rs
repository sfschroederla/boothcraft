use crate::clipboard::Clip;
use crate::marker::{Marker, MarkerKind};
use crate::rope::Rope;
use std::ops::Range;

/// Bitmask of channels an edit applies to (bit 0 = first channel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelMask(pub u32);

impl ChannelMask {
    pub const ALL: ChannelMask = ChannelMask(u32::MAX);

    pub fn has(self, ch: usize) -> bool {
        ch < 32 && self.0 & (1 << ch) != 0
    }

    pub fn is_all(self, channels: usize) -> bool {
        (0..channels).all(|c| self.has(c))
    }
}

/// A snapshot of an open audio file. Cloning is cheap; edits return new snapshots' worth of data
/// by mutating a clone, so callers push the result to a [`crate::History`].
#[derive(Clone, Debug)]
pub struct Document {
    pub sample_rate: u32,
    pub channels: Vec<Rope>,
    pub markers: Vec<Marker>,
}

impl Document {
    pub fn new(sample_rate: u32, channels: Vec<Rope>) -> Document {
        assert!(!channels.is_empty(), "document needs at least one channel");
        let len = channels[0].len();
        assert!(
            channels.iter().all(|c| c.len() == len),
            "channel lengths differ"
        );
        Document {
            sample_rate,
            channels,
            markers: Vec::new(),
        }
    }

    pub fn from_interleaved(sample_rate: u32, channels: usize, interleaved: &[f32]) -> Document {
        let frames = interleaved.len() / channels;
        let ropes = (0..channels)
            .map(|c| {
                let mono: Vec<f32> = (0..frames).map(|i| interleaved[i * channels + c]).collect();
                Rope::from_samples(&mono)
            })
            .collect();
        Document::new(sample_rate, ropes)
    }

    pub fn frames(&self) -> usize {
        self.channels[0].len()
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    pub fn to_interleaved(&self, range: Range<usize>) -> Vec<f32> {
        let n = range.end.min(self.frames()).saturating_sub(range.start);
        let chans: Vec<Vec<f32>> = self
            .channels
            .iter()
            .map(|c| c.to_vec(range.clone()))
            .collect();
        let mut out = Vec::with_capacity(n * chans.len());
        for i in 0..n {
            for c in &chans {
                out.push(c[i]);
            }
        }
        out
    }

    fn clamp(&self, r: Range<usize>) -> Range<usize> {
        let end = r.end.min(self.frames());
        r.start.min(end)..end
    }

    /// Copy the selected channels of `range`.
    pub fn copy(&self, range: Range<usize>, mask: ChannelMask) -> Clip {
        let r = self.clamp(range);
        let channels = self
            .channels
            .iter()
            .enumerate()
            .filter(|(i, _)| mask.has(*i))
            .map(|(_, c)| c.slice(r.clone()))
            .collect();
        Clip {
            sample_rate: self.sample_rate,
            channels,
        }
    }

    /// Remove `range`. When only some channels are masked, lengths must stay equal across channels,
    /// so the masked channels are silenced instead of shortened.
    pub fn delete(&mut self, range: Range<usize>, mask: ChannelMask) {
        let r = self.clamp(range);
        if r.is_empty() {
            return;
        }
        if mask.is_all(self.channel_count()) {
            for c in &mut self.channels {
                c.delete(r.clone());
            }
            for m in &mut self.markers {
                let old_end = match m.kind {
                    MarkerKind::Range { len } => m.at + len,
                    MarkerKind::Cue => m.at,
                };
                m.at = shift_after_delete(m.at, &r);
                if let MarkerKind::Range { len } = &mut m.kind {
                    *len = shift_after_delete(old_end, &r) - m.at;
                }
            }
        } else {
            self.silence(r, mask);
        }
    }

    pub fn crop(&mut self, range: Range<usize>) {
        let r = self.clamp(range);
        self.delete(r.end..self.frames(), ChannelMask::ALL);
        self.delete(0..r.start, ChannelMask::ALL);
    }

    /// Insert a clip at `at`. Mono clips are duplicated across channels; extra clip channels are dropped.
    pub fn paste(&mut self, at: usize, clip: &Clip) {
        let at = at.min(self.frames());
        let n = clip.frames();
        if n == 0 {
            return;
        }
        for (i, c) in self.channels.iter_mut().enumerate() {
            let src = clip
                .channels
                .get(i)
                .or_else(|| clip.channels.first())
                .expect("clip has channels");
            c.insert(at, src);
        }
        for m in &mut self.markers {
            if m.at >= at {
                m.at += n;
            }
        }
    }

    /// Sum a clip into the document starting at `at`, extending the file if needed.
    pub fn mix_paste(&mut self, at: usize, clip: &Clip, gain: f32) {
        let n = clip.frames();
        if n == 0 {
            return;
        }
        let at = at.min(self.frames());
        let need = (at + n).saturating_sub(self.frames());
        if need > 0 {
            for c in &mut self.channels {
                c.append(&Rope::silence(need));
            }
        }
        for (i, c) in self.channels.iter_mut().enumerate() {
            let src = clip
                .channels
                .get(i)
                .or_else(|| clip.channels.first())
                .expect("clip has channels")
                .to_vec(0..n);
            c.map_range(at..at + n, |dst| {
                for (d, s) in dst.iter_mut().zip(&src) {
                    *d += s * gain;
                }
            });
        }
    }

    pub fn insert_silence(&mut self, at: usize, frames: usize) {
        let clip = Clip {
            sample_rate: self.sample_rate,
            channels: vec![Rope::silence(frames)],
        };
        self.paste(at, &clip);
    }

    /// Apply `f(channel_index, samples)` in place over `range` on masked channels.
    pub fn apply(
        &mut self,
        range: Range<usize>,
        mask: ChannelMask,
        mut f: impl FnMut(usize, &mut [f32]),
    ) {
        let r = self.clamp(range);
        for (i, c) in self.channels.iter_mut().enumerate() {
            if mask.has(i) {
                c.map_range(r.clone(), |s| f(i, s));
            }
        }
    }

    pub fn silence(&mut self, range: Range<usize>, mask: ChannelMask) {
        self.apply(range, mask, |_, s| s.fill(0.0));
    }

    pub fn invert(&mut self, range: Range<usize>, mask: ChannelMask) {
        self.apply(range, mask, |_, s| s.iter_mut().for_each(|x| *x = -*x));
    }

    pub fn reverse(&mut self, range: Range<usize>, mask: ChannelMask) {
        self.apply(range, mask, |_, s| s.reverse());
    }

    /// Replace audio from `at` with `chans` (one slice per channel, equal lengths), extending the file
    /// if the new audio runs past the end. Used for recording over existing audio.
    pub fn overwrite(&mut self, at: usize, chans: &[&[f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        let at = at.min(self.frames());
        let overlap = n.min(self.frames() - at);
        for (c, rope) in self.channels.iter_mut().enumerate() {
            let src = chans.get(c).or(chans.first()).copied().unwrap_or(&[]);
            rope.map_range(at..at + overlap, |dst| dst.copy_from_slice(&src[..overlap]));
            rope.push_samples(&src[overlap..n]);
        }
    }

    /// Fade the first `len` frames up from silence.
    pub fn fade_in(&mut self, len: usize, shape: crate::FadeShape, mask: ChannelMask) {
        let len = len.min(self.frames());
        self.apply(0..len, mask, |_, s| {
            for (i, x) in s.iter_mut().enumerate() {
                *x *= crate::fade_gain(i as f32 / len as f32, shape);
            }
        });
    }

    /// Fade the last `len` frames down to silence.
    pub fn fade_out(&mut self, len: usize, shape: crate::FadeShape, mask: ChannelMask) {
        let n = self.frames();
        let len = len.min(n);
        self.apply(n - len..n, mask, |_, s| {
            for (i, x) in s.iter_mut().enumerate() {
                *x *= crate::fade_gain((len - 1 - i) as f32 / len as f32, shape);
            }
        });
    }

    pub fn amplify(&mut self, range: Range<usize>, mask: ChannelMask, gain: f32) {
        self.apply(range, mask, |_, s| s.iter_mut().for_each(|x| *x *= gain));
    }
}

/// Which way to search for a zero crossing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZeroDir {
    Left,
    Right,
    Nearest,
}

impl Document {
    /// The zero crossing of the summed enabled channels closest to `frame` in direction `dir`,
    /// searching at most `max_frames` away. A crossing resolves to whichever of its two samples is
    /// nearer zero; exact zeros count as crossings.
    pub fn zero_crossing(
        &self,
        frame: usize,
        dir: ZeroDir,
        mask: ChannelMask,
        max_frames: usize,
    ) -> Option<usize> {
        let n = self.frames();
        if n < 2 {
            return None;
        }
        let frame = frame.min(n - 1);
        let lo = if dir == ZeroDir::Right {
            frame
        } else {
            frame.saturating_sub(max_frames)
        };
        let hi = if dir == ZeroDir::Left {
            frame
        } else {
            (frame + max_frames).min(n - 1)
        };
        let (a, b) = (lo.saturating_sub(1), (hi + 1).min(n));
        let use_all = (0..self.channel_count()).all(|c| !mask.has(c));
        let mut sum = vec![0f32; b - a];
        for (c, rope) in self.channels.iter().enumerate() {
            if use_all || mask.has(c) {
                for (s, v) in sum.iter_mut().zip(rope.to_vec(a..b)) {
                    *s += v;
                }
            }
        }
        let mut best: Option<usize> = None;
        for i in a + 1..b {
            let (x, y) = (sum[i - 1 - a], sum[i - a]);
            let p = if y == 0.0 {
                i
            } else if (x < 0.0) != (y < 0.0) {
                if x.abs() <= y.abs() {
                    i - 1
                } else {
                    i
                }
            } else {
                continue;
            };
            let ok = match dir {
                ZeroDir::Left => p <= frame,
                ZeroDir::Right => p >= frame,
                ZeroDir::Nearest => true,
            };
            if ok && best.is_none_or(|b| p.abs_diff(frame) < b.abs_diff(frame)) {
                best = Some(p);
            }
        }
        best
    }
}

/// Marker editing. Markers are kept sorted by position, so indices are stable only until the next
/// marker edit; every method returns or takes the index in that sorted order.
impl Document {
    /// Insert a marker, clamped to the file, returning its index.
    pub fn add_marker(&mut self, mut m: Marker) -> usize {
        let frames = self.frames();
        m.at = m.at.min(frames);
        if let MarkerKind::Range { len } = &mut m.kind {
            *len = (*len).min(frames - m.at);
        }
        let i = self.markers.partition_point(|x| x.at <= m.at);
        self.markers.insert(i, m);
        i
    }

    pub fn remove_marker(&mut self, i: usize) -> Option<Marker> {
        (i < self.markers.len()).then(|| self.markers.remove(i))
    }

    pub fn rename_marker(&mut self, i: usize, name: impl Into<String>) {
        if let Some(m) = self.markers.get_mut(i) {
            m.name = name.into();
        }
    }

    /// Move marker `i` so it starts at `at` (ranges keep their length), returning its new index.
    pub fn move_marker(&mut self, i: usize, at: usize) -> Option<usize> {
        let m = self.remove_marker(i)?;
        Some(self.add_marker(Marker { at, ..m }))
    }

    /// The next default name, "Marker 01", "Marker 02", …
    pub fn next_marker_name(&self) -> String {
        let n = self
            .markers
            .iter()
            .filter_map(|m| m.name.strip_prefix("Marker ")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("Marker {:02}", n + 1)
    }

    /// Index of the first marker strictly after `frame`, and of the last strictly before it.
    pub fn marker_after(&self, frame: usize) -> Option<usize> {
        self.markers.iter().position(|m| m.at > frame)
    }

    pub fn marker_before(&self, frame: usize) -> Option<usize> {
        self.markers.iter().rposition(|m| m.at < frame)
    }
}

fn shift_after_delete(at: usize, deleted: &Range<usize>) -> usize {
    if at >= deleted.end {
        at - deleted.len()
    } else if at > deleted.start {
        deleted.start
    } else {
        at
    }
}
