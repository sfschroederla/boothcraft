use crate::effect::Effect;
use crate::param::{db_to_gain, defaults, ParamSpec, Unit};

static SPECS: [ParamSpec; 4] = [
    ParamSpec::new("ceiling", "Maximum Amplitude", Unit::Db, -20.0, 0.0, -0.1),
    ParamSpec::new("boost", "Input Boost", Unit::Db, 0.0, 24.0, 0.0),
    ParamSpec::new("lookahead", "Look-ahead Time", Unit::Ms, 0.5, 20.0, 5.0).log(),
    ParamSpec::new("release", "Release Time", Unit::Ms, 10.0, 1000.0, 80.0).log(),
];

const MAX_LOOKAHEAD_MS: f32 = 20.0;

/// Look-ahead brick-wall limiter.
///
/// For a window `W` (the look-ahead), the required gain `t = min(1, ceiling/|x|)` is held at its
/// minimum over the last `W` samples, released smoothly, then averaged over `W` samples; the audio
/// is delayed by `W - 1`. Every output sample's gain is an average of values no larger than its
/// own required gain, so the output never exceeds the ceiling, and gain changes are smooth.
pub struct Limiter {
    values: Vec<f32>,
    rate: f32,
    channels: usize,
    window: usize,
    release: f32,
    /// Delayed audio, one ring per channel (capacity = max window).
    delay: Vec<Vec<f32>>,
    /// Required gain history for the sliding minimum.
    req: Vec<f32>,
    /// Indices into `req` forming a monotonic deque (ring of `deque_len` entries from `deque_head`).
    deque: Vec<usize>,
    deque_head: usize,
    deque_len: usize,
    /// Released gains for the box average, and their running sum.
    box_buf: Vec<f32>,
    box_sum: f64,
    released: f32,
    pos: usize,
    cap: usize,
}

impl Limiter {
    pub fn new() -> Limiter {
        Limiter {
            values: defaults(&SPECS),
            rate: 48_000.0,
            channels: 0,
            window: 1,
            release: 0.0,
            delay: Vec::new(),
            req: Vec::new(),
            deque: Vec::new(),
            deque_head: 0,
            deque_len: 0,
            box_buf: Vec::new(),
            box_sum: 0.0,
            released: 1.0,
            pos: 0,
            cap: 1,
        }
    }

    fn window_for(&self) -> usize {
        ((self.values[2] * 0.001 * self.rate).round() as usize).clamp(1, self.cap)
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter::new()
    }
}

impl Effect for Limiter {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        self.channels = channels;
        self.cap = (MAX_LOOKAHEAD_MS * 0.001 * sample_rate).ceil() as usize + 1;
        self.delay = vec![vec![0.0; self.cap]; channels];
        self.req = vec![1.0; self.cap];
        self.deque = vec![0; self.cap];
        self.box_buf = vec![1.0; self.cap];
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.window = self.window_for();
        self.delay.iter_mut().for_each(|d| d.fill(0.0));
        self.req.fill(1.0);
        self.box_buf.fill(1.0);
        self.box_sum = self.window as f64;
        self.deque_head = 0;
        self.deque_len = 0;
        self.released = 1.0;
        self.pos = 0;
    }

    fn update(&mut self) {
        self.release = crate::effects::compressor::time_coeff(self.rate, self.values[3]);
        let w = self.window_for();
        if w != self.window && !self.delay.is_empty() {
            // A new look-ahead changes the latency; start clean rather than mis-align.
            self.reset();
        }
    }

    fn latency(&self) -> usize {
        self.window_for() - 1
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        if self.delay.is_empty() {
            return;
        }
        let ceiling = db_to_gain(self.values[0]);
        let boost = db_to_gain(self.values[1]);
        let (w, cap) = (self.window, self.cap);
        let n = buf.first().map_or(0, |c| c.len());
        for i in 0..n {
            let p = self.pos;
            // Store boosted input and its required gain.
            let mut peak = 0f32;
            for (d, ch) in self.delay.iter_mut().zip(buf.iter()) {
                let x = ch[i] * boost;
                d[p % cap] = x;
                peak = peak.max(x.abs());
            }
            let need = if peak > ceiling { ceiling / peak } else { 1.0 };
            self.req[p % cap] = need;
            // Sliding minimum over the last `w` required gains (monotonic deque of positions).
            while self.deque_len > 0 {
                let back = self.deque[(self.deque_head + self.deque_len - 1) % cap];
                if self.req[back % cap] >= need {
                    self.deque_len -= 1;
                } else {
                    break;
                }
            }
            self.deque[(self.deque_head + self.deque_len) % cap] = p;
            self.deque_len += 1;
            while self.deque[self.deque_head % cap] + w <= p {
                self.deque_head = (self.deque_head + 1) % cap;
                self.deque_len -= 1;
            }
            let held = self.req[self.deque[self.deque_head % cap] % cap];
            // Instant attack to the held minimum, smooth release back up.
            self.released = if held < self.released {
                held
            } else {
                held + (self.released - held) * self.release
            };
            // Box average over the last `w` released gains.
            let slot = p % w;
            self.box_sum += self.released as f64 - self.box_buf[slot] as f64;
            self.box_buf[slot] = self.released;
            let gain = (self.box_sum / w as f64) as f32;
            // Output the sample from `w - 1` steps ago.
            let out_idx = (p + cap - (w - 1)) % cap;
            for (d, ch) in self.delay.iter().zip(buf.iter_mut()) {
                ch[i] = d[out_idx] * gain;
            }
            self.pos = p + 1;
            if self.pos.is_multiple_of(65_536) {
                // Re-sum occasionally so floating-point drift cannot accumulate.
                self.box_sum = self.box_buf[..w].iter().map(|&g| g as f64).sum();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize) -> Vec<f32> {
        let mut x = 12345u32;
        (0..n)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let burst = if (i / 4000) % 3 == 0 { 4.0 } else { 0.3 };
                (x as f32 / u32::MAX as f32 - 0.5) * burst
            })
            .collect()
    }

    #[test]
    fn never_exceeds_the_ceiling() {
        for lookahead in [0.5, 5.0, 20.0] {
            let mut l = Limiter::new();
            l.set_all(&[-3.0, 12.0, lookahead, 50.0]);
            l.prepare(48_000.0, 2, 1024);
            let (mut a, mut b) = (
                noise(60_000),
                noise(60_000).into_iter().rev().collect::<Vec<_>>(),
            );
            for (ca, cb) in a.chunks_mut(1024).zip(b.chunks_mut(1024)) {
                l.process(&mut [ca, cb]);
            }
            let ceiling = db_to_gain(-3.0) * (1.0 + 1e-5);
            let worst = a.iter().chain(&b).fold(0f32, |m, x| m.max(x.abs()));
            assert!(
                worst <= ceiling,
                "lookahead {lookahead}: {worst} > {ceiling}"
            );
        }
    }

    #[test]
    fn quiet_audio_is_only_delayed() {
        let mut l = Limiter::new();
        l.prepare(48_000.0, 1, 512);
        let d = l.latency();
        assert_eq!(d, 239, "5 ms at 48 kHz, minus one");
        let input: Vec<f32> = (0..5000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut x = input.clone();
        l.process(&mut [&mut x[..]]);
        for i in d..5000 {
            assert!((x[i] - input[i - d]).abs() < 1e-6);
        }
    }
}
