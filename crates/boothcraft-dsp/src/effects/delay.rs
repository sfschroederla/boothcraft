use crate::delayline::{DelayLine, OnePole};
use crate::effect::Effect;
use crate::param::{defaults, ParamSpec, Smoothed, Unit};

const MAX_MS: f32 = 2000.0;

static DELAY_SPECS: [ParamSpec; 4] = [
    ParamSpec::new("left", "Left Delay", Unit::Ms, 0.0, MAX_MS, 300.0),
    ParamSpec::new("right", "Right Delay", Unit::Ms, 0.0, MAX_MS, 300.0),
    ParamSpec::new("mix", "Mix", Unit::Percent, 0.0, 100.0, 50.0),
    ParamSpec::toggle("invert", "Invert Delayed", false),
];

static ECHO_SPECS: [ParamSpec; 6] = [
    ParamSpec::new("left", "Left Delay", Unit::Ms, 1.0, MAX_MS, 400.0),
    ParamSpec::new("right", "Right Delay", Unit::Ms, 1.0, MAX_MS, 400.0),
    ParamSpec::new("feedback", "Feedback", Unit::Percent, 0.0, 95.0, 40.0),
    ParamSpec::new("bounce", "Echo Bounce", Unit::Percent, 0.0, 100.0, 0.0),
    ParamSpec::new("damping", "Damping", Unit::Hz, 500.0, 20_000.0, 8000.0).log(),
    ParamSpec::new("mix", "Mix", Unit::Percent, 0.0, 100.0, 40.0),
];

const MODES: &[&str] = &["Tape", "Tape/Tube", "Analog"];

static ANALOG_SPECS: [ParamSpec; 7] = [
    ParamSpec::choice("mode", "Mode", MODES, 0),
    ParamSpec::new("dry", "Dry Out", Unit::Percent, 0.0, 200.0, 100.0),
    ParamSpec::new("wet", "Wet Out", Unit::Percent, 0.0, 200.0, 50.0),
    ParamSpec::new("delay", "Delay", Unit::Ms, 1.0, MAX_MS, 350.0),
    ParamSpec::new("feedback", "Feedback", Unit::Percent, 0.0, 100.0, 50.0),
    ParamSpec::new("trash", "Trash", Unit::Percent, 0.0, 100.0, 20.0),
    ParamSpec::new("spread", "Spread", Unit::Percent, 0.0, 100.0, 30.0),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Delay,
    Echo,
    Analog,
}

/// Delay, Echo and Analog Delay share their machinery: one delay line per channel, smoothed delay
/// times (so changes glide instead of clicking), and an optional damped, saturated feedback path.
pub struct DelayEffect {
    kind: Kind,
    specs: &'static [ParamSpec],
    values: Vec<f32>,
    rate: f32,
    lines: Vec<DelayLine>,
    damp: Vec<OnePole>,
    times: [Smoothed; 2],
    feedback_state: Vec<f32>,
}

impl DelayEffect {
    fn with(kind: Kind, specs: &'static [ParamSpec]) -> DelayEffect {
        DelayEffect {
            kind,
            specs,
            values: defaults(specs),
            rate: 48_000.0,
            lines: Vec::new(),
            damp: Vec::new(),
            times: [Smoothed::new(0.0); 2],
            feedback_state: Vec::new(),
        }
    }

    pub fn delay() -> DelayEffect {
        DelayEffect::with(Kind::Delay, &DELAY_SPECS)
    }

    pub fn echo() -> DelayEffect {
        DelayEffect::with(Kind::Echo, &ECHO_SPECS)
    }

    pub fn analog() -> DelayEffect {
        DelayEffect::with(Kind::Analog, &ANALOG_SPECS)
    }

    /// Delay time in samples for channel side `c` (0 = left).
    fn target_time(&self, c: usize) -> f32 {
        let ms = match self.kind {
            Kind::Delay | Kind::Echo => self.values[c.min(1)],
            // Spread lengthens the right side by up to 20%.
            Kind::Analog => {
                self.values[3]
                    * if c == 1 {
                        1.0 + self.values[6] / 500.0
                    } else {
                        1.0
                    }
            }
        };
        (ms * 0.001 * self.rate).max(1.0)
    }
}

impl Effect for DelayEffect {
    fn specs(&self) -> &'static [ParamSpec] {
        self.specs
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, channels: usize, _max_block: usize) {
        self.rate = sample_rate;
        let max = (MAX_MS * 1.25 * 0.001 * sample_rate) as usize + 4;
        self.lines = vec![DelayLine::new(max); channels];
        self.damp = vec![OnePole::default(); channels];
        self.feedback_state = vec![0.0; channels];
        for t in &mut self.times {
            t.prepare(sample_rate, 60.0);
        }
        self.update();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.damp.iter_mut().for_each(OnePole::reset);
        self.feedback_state.fill(0.0);
        for t in &mut self.times {
            t.snap();
        }
    }

    fn update(&mut self) {
        for c in 0..2 {
            let t = self.target_time(c);
            self.times[c].set(t);
        }
        let cutoff = match self.kind {
            Kind::Echo => self.values[4],
            Kind::Analog => [6500.0, 4500.0, 3000.0][self.values[0] as usize % 3],
            Kind::Delay => 20_000.0,
        };
        let rate = self.rate;
        self.damp.iter_mut().for_each(|d| d.set(cutoff, rate));
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        let chans = buf.len();
        let v = &self.values;
        for i in 0..n {
            let t = [self.times[0].tick(), self.times[1].tick()];
            // Read every channel's delayed signal first, so cross-feedback (bounce) is symmetric.
            for c in 0..chans {
                self.feedback_state[c] = self.lines[c].read(t[c % 2]);
            }
            for (c, ch) in buf.iter_mut().enumerate() {
                let x = ch[i];
                let delayed = self.feedback_state[c];
                let (into_line, out) = match self.kind {
                    Kind::Delay => {
                        let mix = v[2] / 100.0;
                        let wet = if v[3] >= 0.5 { -delayed } else { delayed };
                        (x, x * (1.0 - mix) + wet * mix)
                    }
                    Kind::Echo => {
                        let (fb, bounce, mix) = (v[2] / 100.0, v[3] / 100.0, v[5] / 100.0);
                        let other = self.feedback_state[(c + 1) % chans];
                        let recirc = self.damp[c].tick(delayed * (1.0 - bounce) + other * bounce);
                        (x + recirc * fb, x * (1.0 - mix) + delayed * mix)
                    }
                    Kind::Analog => {
                        let (dry, wet, fb, trash) =
                            (v[1] / 100.0, v[2] / 100.0, v[4] / 100.0, v[5] / 100.0);
                        let drive = 1.0 + trash * 6.0;
                        let shaped = (self.damp[c].tick(delayed) * drive).tanh() / drive.tanh();
                        (x + shaped * fb.min(0.98), x * dry + delayed * wet)
                    }
                };
                self.lines[c].push(into_line);
                ch[i] = out;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse_response(mut e: DelayEffect, n: usize) -> Vec<f32> {
        e.prepare(1000.0, 1, n);
        e.reset();
        let mut x = vec![0.0; n];
        x[0] = 1.0;
        e.process(&mut [&mut x[..]]);
        x
    }

    #[test]
    fn delay_places_the_copy() {
        let mut e = DelayEffect::delay();
        e.set_all(&[100.0, 100.0, 50.0, 0.0]);
        let y = impulse_response(e, 300);
        assert_eq!(y[0], 0.5);
        assert!(
            (y[100] - 0.5).abs() < 1e-6,
            "copy at 100 ms (= 100 samples at 1 kHz): {}",
            y[100]
        );
        assert_eq!(y[200], 0.0, "no feedback");
    }

    #[test]
    fn echo_decays_by_feedback() {
        let mut e = DelayEffect::echo();
        e.set_all(&[50.0, 50.0, 50.0, 0.0, 20_000.0, 100.0]);
        let y = impulse_response(e, 400);
        assert!((y[50] - 1.0).abs() < 0.05, "{}", y[50]);
        assert!(
            (y[100] / y[50] - 0.5).abs() < 0.1,
            "each echo half the last: {} {}",
            y[50],
            y[100]
        );
        assert!(y[150] < y[100]);
    }
}
