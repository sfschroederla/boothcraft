use crate::effect::Effect;
use crate::param::{db_to_gain, defaults, ParamSpec, Smoothed, Unit};

static SPECS: [ParamSpec; 3] = [
    ParamSpec::new("left", "Left Gain", Unit::Db, -40.0, 40.0, 0.0),
    ParamSpec::new("right", "Right Gain", Unit::Db, -40.0, 40.0, 0.0),
    ParamSpec::toggle("link", "Link Sliders", true),
];

/// Gain per side. With Link on, the left gain applies to every channel. Odd channels (right, and
/// so on in multichannel files) follow Right Gain when unlinked.
pub struct Amplify {
    values: Vec<f32>,
    gains: [Smoothed; 2],
}

impl Amplify {
    pub fn new() -> Amplify {
        let mut a = Amplify {
            values: defaults(&SPECS),
            gains: [Smoothed::new(1.0); 2],
        };
        a.update();
        a.reset();
        a
    }
}

impl Default for Amplify {
    fn default() -> Self {
        Amplify::new()
    }
}

impl Effect for Amplify {
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
        for g in &mut self.gains {
            g.prepare(sample_rate, 20.0);
        }
    }

    fn reset(&mut self) {
        for g in &mut self.gains {
            g.snap();
        }
    }

    fn update(&mut self) {
        let linked = self.values[2] >= 0.5;
        let left = db_to_gain(self.values[0]);
        let right = if linked {
            left
        } else {
            db_to_gain(self.values[1])
        };
        self.gains[0].set(left);
        self.gains[1].set(right);
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let n = buf.first().map_or(0, |c| c.len());
        for i in 0..n {
            let g = [self.gains[0].tick(), self.gains[1].tick()];
            for (c, ch) in buf.iter_mut().enumerate() {
                ch[i] *= g[c % 2];
            }
        }
    }
}
