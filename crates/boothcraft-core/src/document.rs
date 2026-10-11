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

    pub fn amplify(&mut self, range: Range<usize>, mask: ChannelMask, gain: f32) {
        self.apply(range, mask, |_, s| s.iter_mut().for_each(|x| *x *= gain));
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
