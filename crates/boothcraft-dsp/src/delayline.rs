//! A ring-buffer delay with fractional (linear-interpolated) reads, and a simple LFO.

/// Single-channel delay line. Allocate with [`DelayLine::new`] off the audio thread.
#[derive(Clone, Debug, Default)]
pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    /// Room for delays up to `max_samples`.
    pub fn new(max_samples: usize) -> DelayLine {
        DelayLine {
            buf: vec![0.0; max_samples + 2],
            pos: 0,
        }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }

    pub fn max_delay(&self) -> f32 {
        self.buf.len().saturating_sub(2) as f32
    }

    pub fn push(&mut self, x: f32) {
        if self.buf.is_empty() {
            return;
        }
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.buf.len();
    }

    /// The sample written `delay` samples ago (1.0 = the previous push), interpolated.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let n = self.buf.len();
        if n == 0 {
            return 0.0;
        }
        let d = delay.clamp(1.0, self.max_delay());
        let i = d.floor();
        let frac = d - i;
        let a = self.buf[(self.pos + n - i as usize) % n];
        let b = self.buf[(self.pos + n - i as usize - 1) % n];
        a + (b - a) * frac
    }
}

/// Sine LFO with a phase offset, advanced one sample at a time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lfo {
    phase: f32,
    inc: f32,
}

impl Lfo {
    pub fn set_rate(&mut self, hz: f32, sample_rate: f32) {
        self.inc = hz / sample_rate;
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// Value in -1..=1 at `offset` cycles ahead of the current phase, then advance.
    #[inline]
    pub fn tick(&mut self, offset: f32) -> f32 {
        let v = ((self.phase + offset) * std::f32::consts::TAU).sin();
        self.phase = (self.phase + self.inc).fract();
        v
    }

    /// Same as `tick` without advancing (for extra voices or channels on the same sample).
    #[inline]
    pub fn peek(&self, offset: f32) -> f32 {
        ((self.phase + offset) * std::f32::consts::TAU).sin()
    }

    pub fn advance(&mut self) {
        self.phase = (self.phase + self.inc).fract();
    }
}

/// One-pole low-pass, used to darken feedback paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    a: f32,
    z: f32,
}

impl OnePole {
    pub fn set(&mut self, cutoff: f32, sample_rate: f32) {
        self.a = (-std::f32::consts::TAU * cutoff.min(sample_rate * 0.49) / sample_rate).exp();
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        self.z = x + (self.z - x) * self.a;
        self.z
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_fractional_reads() {
        let mut d = DelayLine::new(10);
        for x in [1.0, 2.0, 3.0, 4.0] {
            d.push(x);
        }
        assert_eq!(d.read(1.0), 4.0);
        assert_eq!(d.read(3.0), 2.0);
        assert_eq!(d.read(1.5), 3.5);
    }
}
