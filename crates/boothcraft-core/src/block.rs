use std::sync::Arc;

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

/// An immutable run of mono samples with a precomputed peak overview.
#[derive(Debug)]
pub struct Block {
    data: Vec<f32>,
    peaks: Vec<Peak>,
}

impl Block {
    pub fn new(data: Vec<f32>) -> Arc<Block> {
        assert!(data.len() <= BLOCK_FRAMES, "block too large");
        let peaks = data.chunks(PEAK_STRIDE).map(Peak::of).collect();
        Arc::new(Block { data, peaks })
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn samples(&self) -> &[f32] {
        &self.data
    }

    /// Overview entries, one per [`PEAK_STRIDE`] frames (the last may cover fewer).
    pub fn peaks(&self) -> &[Peak] {
        &self.peaks
    }
}
