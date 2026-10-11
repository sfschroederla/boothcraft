use crate::effect::OfflineEffect;
use crate::param::{db_to_gain, defaults, ParamSpec, Unit};

static SPECS: [ParamSpec; 3] = [
    ParamSpec::new("target", "Normalize To", Unit::Db, -40.0, 0.0, -0.1),
    ParamSpec::toggle("linked", "Normalize All Channels Equally", true),
    ParamSpec::toggle("dc", "DC Bias Adjust", false),
];

/// Scale so the loudest sample hits the target peak. Linked: one gain for all channels.
pub struct Normalize {
    values: Vec<f32>,
}

impl Normalize {
    pub fn new() -> Normalize {
        Normalize {
            values: defaults(&SPECS),
        }
    }
}

impl Default for Normalize {
    fn default() -> Self {
        Normalize::new()
    }
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

impl OfflineEffect for Normalize {
    fn specs(&self) -> &'static [ParamSpec] {
        &SPECS
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn process_offline(&mut self, chans: &mut [Vec<f32>], _sample_rate: f32) {
        let target = db_to_gain(self.values[0]);
        if self.values[2] >= 0.5 {
            for c in chans.iter_mut().filter(|c| !c.is_empty()) {
                let mean = (c.iter().map(|&x| x as f64).sum::<f64>() / c.len() as f64) as f32;
                c.iter_mut().for_each(|x| *x -= mean);
            }
        }
        let scale = |c: &mut Vec<f32>, p: f32| {
            if p > 0.0 {
                let g = target / p;
                c.iter_mut().for_each(|x| *x *= g);
            }
        };
        if self.values[1] >= 0.5 {
            let p = chans.iter().map(|c| peak(c)).fold(0.0, f32::max);
            chans.iter_mut().for_each(|c| scale(c, p));
        } else {
            for c in chans.iter_mut() {
                let p = peak(c);
                scale(c, p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_and_per_channel() {
        let mut n = Normalize::new();
        n.values = vec![-6.0, 1.0, 0.0];
        let mut chans = vec![vec![0.1, -0.25], vec![0.05, 0.0]];
        n.process_offline(&mut chans, 48_000.0);
        assert!((peak(&chans[0]) - db_to_gain(-6.0)).abs() < 1e-6);
        assert!(
            (chans[1][0] / chans[0][0] - 0.5).abs() < 1e-6,
            "linked keeps the balance"
        );

        n.values = vec![0.0, 0.0, 0.0];
        let mut chans = vec![vec![0.1, -0.25], vec![0.05, 0.0]];
        n.process_offline(&mut chans, 48_000.0);
        assert!((peak(&chans[0]) - 1.0).abs() < 1e-6 && (peak(&chans[1]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dc_removed_and_silence_safe() {
        let mut n = Normalize::new();
        n.values = vec![0.0, 1.0, 1.0];
        let mut chans = vec![vec![0.6, 0.4, 0.6, 0.4], vec![0.0; 4]];
        n.process_offline(&mut chans, 48_000.0);
        let mean: f32 = chans[0].iter().sum::<f32>() / 4.0;
        assert!(mean.abs() < 1e-6);
        assert!((peak(&chans[0]) - 1.0).abs() < 1e-6);
        assert!(
            chans[1].iter().all(|x| *x == 0.0),
            "silence stays silent, no NaN"
        );
    }
}
