use crate::store::{BlockStore, Location};
use arc_swap::ArcSwapOption;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};

/// Maximum frames in one block.
pub const BLOCK_FRAMES: usize = 65_536;
/// Frames summarised by one [`Peak`] entry in a block's overview.
pub const PEAK_STRIDE: usize = 256;

/// Min/max/RMS summary of a run of samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

impl Peak {
    pub const EMPTY: Peak = Peak {
        min: 0.0,
        max: 0.0,
        rms: 0.0,
    };

    pub fn of(samples: &[f32]) -> Peak {
        if samples.is_empty() {
            return Peak::EMPTY;
        }
        let (mut min, mut max, mut sq) = (f32::MAX, f32::MIN, 0.0f64);
        for &s in samples {
            min = min.min(s);
            max = max.max(s);
            sq += (s as f64) * (s as f64);
        }
        Peak {
            min,
            max,
            rms: (sq / samples.len() as f64).sqrt() as f32,
        }
    }

    /// Combine two summaries; `rms` is weighted by the given frame counts.
    pub fn merge(a: Peak, a_len: usize, b: Peak, b_len: usize) -> Peak {
        let total = (a_len + b_len).max(1) as f64;
        let sq = (a.rms as f64).powi(2) * a_len as f64 + (b.rms as f64).powi(2) * b_len as f64;
        Peak {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
            rms: (sq / total).sqrt() as f32,
        }
    }
}

fn resident_bytes(frames: usize) -> u64 {
    (frames * std::mem::size_of::<f32>()) as u64
}

/// An immutable run of mono samples with a precomputed peak overview.
///
/// Sample data can be spilled to disk under memory pressure (see
/// [`BlockStore`](crate::store::BlockStore)) and reloaded on demand; the peak overview always
/// stays resident, since it is a small fraction (about 1/85th, at 3 `f32`s per
/// [`PEAK_STRIDE`] frames) of the audio it summarises.
pub struct Block {
    len: usize,
    peaks: Vec<Peak>,
    /// Peak over the whole block, so a fully zoomed-out overview need not visit every entry of
    /// `peaks`.
    summary: Peak,
    resident: ArcSwapOption<Vec<f32>>,
    disk: OnceLock<Location>,
    last_use: AtomicU64,
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block")
            .field("len", &self.len)
            .field("resident", &self.resident.load().is_some())
            .finish_non_exhaustive()
    }
}

impl Block {
    pub fn new(data: Vec<f32>) -> Arc<Block> {
        assert!(data.len() <= BLOCK_FRAMES, "block too large");
        let peaks: Vec<Peak> = data.chunks(PEAK_STRIDE).map(Peak::of).collect();
        let summary = Peak::of(&data);
        let len = data.len();
        let store = BlockStore::global();
        let block = Arc::new(Block {
            len,
            peaks,
            summary,
            resident: ArcSwapOption::from_pointee(data),
            disk: OnceLock::new(),
            last_use: AtomicU64::new(store.tick()),
        });
        store.note_resident(resident_bytes(len));
        store.register(&block);
        block
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Overview entries, one per [`PEAK_STRIDE`] frames (the last may cover fewer).
    pub fn peaks(&self) -> &[Peak] {
        &self.peaks
    }

    /// Peak over the whole block.
    pub fn summary(&self) -> Peak {
        self.summary
    }

    /// This block's samples, loading them from disk if they were spilled.
    ///
    /// This can block on file I/O: call it only from the UI thread or a worker thread, never from
    /// the real-time audio callback (use [`Block::try_data`] there instead).
    pub fn data(&self) -> Arc<Vec<f32>> {
        self.touch();
        if let Some(v) = self.resident.load_full() {
            return v;
        }
        let loc = *self
            .disk
            .get()
            .expect("a non-resident block always has a disk location");
        let data = Arc::new(BlockStore::global().read(loc));
        self.resident.store(Some(data.clone()));
        BlockStore::global().note_resident(resident_bytes(self.len));
        data
    }

    /// This block's samples if they are already resident in memory, without touching disk.
    ///
    /// Lock-free and wait-free (an atomic load plus, at most, an atomic counter increment), so
    /// this is safe to call from the real-time audio callback.
    pub fn try_data(&self) -> Option<Arc<Vec<f32>>> {
        let v = self.resident.load_full();
        if v.is_some() {
            self.touch();
        }
        v
    }

    fn touch(&self) {
        self.last_use.store(BlockStore::global().tick(), Relaxed);
    }

    pub(crate) fn last_use(&self) -> u64 {
        self.last_use.load(Relaxed)
    }

    /// Whether this block's resident copy could be dropped right now: it must already be
    /// resident, and nothing beyond the block's own reference to it may be holding a clone (an
    /// in-flight reader), so eviction cannot free memory still in use.
    pub(crate) fn is_spillable(&self) -> bool {
        self.resident
            .load_full()
            .is_some_and(|v| Arc::strong_count(&v) <= 2)
    }

    /// Write this block's data to disk if it hasn't been already, then drop the resident copy.
    pub(crate) fn spill(&self, store: &BlockStore) {
        if self.disk.get().is_none() {
            let Some(data) = self.resident.load_full() else {
                return;
            };
            let loc = store.write(&data);
            // Lost a race with another spill pass: the data is identical either way.
            let _ = self.disk.set(loc);
        }
        if self.resident.swap(None).is_some() {
            store.note_evicted(resident_bytes(self.len));
        }
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // A block that was never spilled never occupied disk space, so there is nothing to free.
        if let Some(&loc) = self.disk.get() {
            BlockStore::global().block_dropped(loc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_data_never_reloads_but_data_transparently_does() {
        let store = BlockStore::global();
        let block = Block::new(vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(
            *block.try_data().expect("freshly created block is resident"),
            vec![1.0, 2.0, 3.0, 4.0]
        );

        block.spill(store); // force an eviction now instead of waiting for the background spiller
        assert!(
            block.try_data().is_none(),
            "evicted block must not be reported as resident"
        );

        assert_eq!(
            *block.data(),
            vec![1.0, 2.0, 3.0, 4.0],
            "data() reloads from disk"
        );
        assert!(block.try_data().is_some(), "resident again after data()");
    }

    #[test]
    fn spilling_twice_does_not_rewrite_to_disk() {
        let store = BlockStore::global();
        let block = Block::new(vec![5.0; 100]);
        block.spill(store);
        let loc_first = *block.disk.get().unwrap();
        // Residency was restored by `data()`; spilling again must reuse the same location rather
        // than writing the (identical) bytes out a second time.
        let _ = block.data();
        block.spill(store);
        let loc_second = *block.disk.get().unwrap();
        assert_eq!(loc_first, loc_second);
    }
}
