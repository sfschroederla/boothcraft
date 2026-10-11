use crate::effect::Effect;
use crate::param::{defaults, ParamSpec, Smoothed, Unit};

static SPECS: [ParamSpec; 4] = [
    ParamSpec::new(
        "l_from_l",
        "Left from Left",
        Unit::Percent,
        -200.0,
        200.0,
        100.0,
    ),
    ParamSpec::new(
        "l_from_r",
        "Left from Right",
        Unit::Percent,
        -200.0,
        200.0,
        0.0,
    ),
    ParamSpec::new(
        "r_from_l",
        "Right from Left",
        Unit::Percent,
        -200.0,
        200.0,
        0.0,
    ),
    ParamSpec::new(
        "r_from_r",
        "Right from Right",
        Unit::Percent,
        -200.0,
        200.0,
        100.0,
    ),
];

/// Remix the first two channels as a 2x2 matrix (extra channels pass through; mono files use
/// Left from Left as a gain).
pub struct ChannelMixer {
    values: Vec<f32>,
    m: [Smoothed; 4],
}

impl ChannelMixer {
    pub fn new() -> ChannelMixer {
        let mut c = ChannelMixer {
            values: defaults(&SPECS),
            m: [Smoothed::new(0.0); 4],
        };
        c.update();
        c.reset();
        c
    }
}

impl Default for ChannelMixer {
    fn default() -> Self {
        ChannelMixer::new()
    }
}

impl Effect for ChannelMixer {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, sample_rate: f32, _channels: usize, _max_block: usize) {
        for m in &mut self.m {
            m.prepare(sample_rate, 20.0);
        }
    }

    fn reset(&mut self) {
        for m in &mut self.m {
            m.snap();
        }
    }

    fn update(&mut self) {
        for (m, v) in self.m.iter_mut().zip(&self.values) {
            m.set(v / 100.0);
        }
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        for i in 0..n {
            let k = [
                self.m[0].tick(),
                self.m[1].tick(),
                self.m[2].tick(),
                self.m[3].tick(),
            ];
            match buf {
                [l, r, ..] => {
                    let (a, b) = (l[i], r[i]);
                    l[i] = a * k[0] + b * k[1];
                    r[i] = a * k[2] + b * k[3];
                }
                [m] => m[i] *= k[0],
                [] => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_channels() {
        let mut c = ChannelMixer::new();
        c.set_all(&[0.0, 100.0, 100.0, 0.0]);
        c.prepare(48_000.0, 2, 64);
        c.reset();
        let (mut l, mut r) = (vec![1.0; 8], vec![-1.0; 8]);
        c.process(&mut [&mut l[..], &mut r[..]]);
        assert_eq!((l[7], r[7]), (-1.0, 1.0));
    }
}
