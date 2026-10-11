//! The effect interfaces.

use crate::param::ParamSpec;

/// A real-time effect: usable in the Effects Rack, for live preview and for offline Apply.
///
/// `process` runs on the audio thread: it must not allocate, lock or block. Buffers are planar
/// (one slice per channel, all the same length, never longer than `max_block` from `prepare`).
/// `Drop` never runs on the audio thread: the engine hands retired racks back to the UI thread.
///
/// Built-in effects return a `'static` slice; plugin effects return specs owned by the instance.
pub trait Effect: Send {
    fn specs(&self) -> &[ParamSpec];
    fn values(&self) -> &[f32];
    fn values_mut(&mut self) -> &mut [f32];

    /// Allocate state for this rate and channel count. Called off the audio thread.
    fn prepare(&mut self, sample_rate: f32, channels: usize, max_block: usize);
    /// Clear delay lines, envelopes and filters, and jump smoothers to their targets.
    fn reset(&mut self);
    fn process(&mut self, buf: &mut [&mut [f32]]);

    /// Recompute derived state after parameter values change.
    fn update(&mut self) {}

    /// Delay introduced by the effect, in frames (e.g. limiter lookahead).
    fn latency(&self) -> usize {
        0
    }

    /// Frames of output that follow the input ending (reverb and delay tails), if known.
    fn tail(&self) -> usize {
        0
    }

    /// Called on the UI thread, never the audio thread, when the rack holding this effect is
    /// being torn down. An effect that owns something that must be released on the main thread
    /// (a plugin's processor) hands it over, tagged with its slot, instead of dropping it.
    fn retire(&mut self) -> Option<(crate::plugin_ref::SlotId, Box<dyn std::any::Any + Send>)> {
        None
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if let Some(spec) = self.specs().get(index) {
            let v = spec.clamp(value);
            self.values_mut()[index] = v;
            self.update();
        }
    }

    fn param(&self, index: usize) -> f32 {
        self.values().get(index).copied().unwrap_or(0.0)
    }

    /// Set every parameter at once (presets), updating once.
    fn set_all(&mut self, values: &[f32]) {
        let n = self.specs().len().min(values.len());
        for (i, new) in values.iter().enumerate().take(n) {
            let spec = self.specs()[i];
            self.values_mut()[i] = spec.clamp(*new);
        }
        self.update();
    }
}

/// An effect that needs the whole selection at once (e.g. Normalize looks at the peak first).
/// Not available in the rack; applied from its own dialog.
pub trait OfflineEffect: Send {
    fn specs(&self) -> &'static [ParamSpec];
    fn values(&self) -> &[f32];
    fn values_mut(&mut self) -> &mut [f32];
    /// Process the selected range of each enabled channel in place.
    fn process_offline(&mut self, chans: &mut [Vec<f32>], sample_rate: f32);

    /// Envelope effects expose their breakpoints for the curve editor.
    fn envelope(&self) -> Option<&Envelope> {
        None
    }

    fn envelope_mut(&mut self) -> Option<&mut Envelope> {
        None
    }
}

/// A breakpoint curve over the selection: `(position 0..=1, value)` pairs sorted by position, with
/// fixed endpoints at 0 and 1. Values are in `min..=max` (linear level, or dB for gain envelopes).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    pub points: Vec<(f32, f32)>,
    pub min: f32,
    pub max: f32,
}

impl Envelope {
    pub fn flat(value: f32, min: f32, max: f32) -> Envelope {
        Envelope {
            points: vec![(0.0, value), (1.0, value)],
            min,
            max,
        }
    }

    /// Keep points sorted, in range, and the endpoints pinned at 0 and 1.
    pub fn normalize(&mut self) {
        for p in &mut self.points {
            p.0 = p.0.clamp(0.0, 1.0);
            p.1 = p.1.clamp(self.min, self.max);
        }
        self.points.sort_by(|a, b| a.0.total_cmp(&b.0));
        if self.points.len() < 2 {
            let v = self.points.first().map_or(self.max, |p| p.1);
            self.points = vec![(0.0, v), (1.0, v)];
        }
        let last = self.points.len() - 1;
        self.points[0].0 = 0.0;
        self.points[last].0 = 1.0;
    }

    /// Value at position `t`; `smooth` uses an S-curve between points instead of straight lines.
    pub fn eval(&self, t: f32, smooth: bool) -> f32 {
        let pts = &self.points;
        let i = pts.partition_point(|p| p.0 <= t).clamp(1, pts.len() - 1);
        let (a, b) = (pts[i - 1], pts[i]);
        let span = (b.0 - a.0).max(1e-9);
        let mut k = ((t - a.0) / span).clamp(0.0, 1.0);
        if smooth {
            k = k * k * (3.0 - 2.0 * k);
        }
        a.1 + (b.1 - a.1) * k
    }
}
