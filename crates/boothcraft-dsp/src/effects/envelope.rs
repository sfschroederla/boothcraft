use crate::effect::{Envelope, OfflineEffect};
use crate::param::{db_to_gain, defaults, ParamSpec};

static SPECS: [ParamSpec; 1] = [ParamSpec::toggle("spline", "Spline Curves", false)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Points are linear levels, 0..=1 (attenuation only).
    Fade,
    /// Points are gains in dB.
    Gain,
}

/// Fade Envelope and Gain Envelope: a breakpoint curve drawn over the selection.
pub struct EnvelopeEffect {
    values: Vec<f32>,
    env: Envelope,
    mode: Mode,
}

impl EnvelopeEffect {
    pub fn fade() -> EnvelopeEffect {
        EnvelopeEffect {
            values: defaults(&SPECS),
            env: Envelope::flat(1.0, 0.0, 1.0),
            mode: Mode::Fade,
        }
    }

    pub fn gain() -> EnvelopeEffect {
        EnvelopeEffect {
            values: defaults(&SPECS),
            env: Envelope::flat(0.0, -40.0, 20.0),
            mode: Mode::Gain,
        }
    }
}

impl OfflineEffect for EnvelopeEffect {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn envelope(&self) -> Option<&Envelope> {
        Some(&self.env)
    }

    fn envelope_mut(&mut self) -> Option<&mut Envelope> {
        Some(&mut self.env)
    }

    fn process_offline(&mut self, chans: &mut [Vec<f32>], _sample_rate: f32) {
        self.env.normalize();
        let smooth = self.values[0] >= 0.5;
        for c in chans.iter_mut() {
            let n = c.len();
            let last = n.saturating_sub(1).max(1) as f32;
            for (i, x) in c.iter_mut().enumerate() {
                let v = self.env.eval(i as f32 / last, smooth);
                *x *= if self.mode == Mode::Gain {
                    db_to_gain(v)
                } else {
                    v
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_envelope_follows_points() {
        let mut e = EnvelopeEffect::fade();
        e.env.points = vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)];
        let mut chans = vec![vec![1.0; 101]];
        e.process_offline(&mut chans, 48_000.0);
        assert_eq!(chans[0][0], 0.0);
        assert!((chans[0][25] - 0.5).abs() < 1e-6);
        assert!((chans[0][50] - 1.0).abs() < 1e-6);
        assert_eq!(chans[0][100], 0.0);
    }

    #[test]
    fn gain_envelope_is_in_db_and_spline_is_smooth() {
        let mut e = EnvelopeEffect::gain();
        e.env.points = vec![(0.0, 0.0), (1.0, -6.0)];
        e.values[0] = 1.0;
        let mut chans = vec![vec![1.0; 1001]];
        e.process_offline(&mut chans, 48_000.0);
        assert!((chans[0][1000] - db_to_gain(-6.0)).abs() < 1e-5);
        assert!((chans[0][1] - 1.0).abs() < 1e-4, "spline starts flat");
    }

    #[test]
    fn normalize_pins_endpoints_and_clamps() {
        let mut env = Envelope {
            points: vec![(0.7, 5.0), (0.2, -1.0), (0.4, 0.5)],
            min: 0.0,
            max: 1.0,
        };
        env.normalize();
        assert_eq!(env.points, vec![(0.0, 0.0), (0.4, 0.5), (1.0, 1.0)]);
    }
}
