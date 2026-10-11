//! Effect parameter descriptions and smoothing.

/// What a parameter's number means, for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    Db,
    Hz,
    Ms,
    Percent,
    Ratio,
    Seconds,
}

impl Unit {
    pub fn suffix(self) -> &'static str {
        match self {
            Unit::None => "",
            Unit::Db => " dB",
            Unit::Hz => " Hz",
            Unit::Ms => " ms",
            Unit::Percent => " %",
            Unit::Ratio => ":1",
            Unit::Seconds => " s",
        }
    }
}

/// How a slider maps to the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// Logarithmic (for frequencies and times); requires `min > 0`.
    Log,
}

/// A parameter's identity, range and default. `choices` non-empty means the value is an index
/// into it (a menu, such as filter type).
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub unit: Unit,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub curve: Curve,
    pub choices: &'static [&'static str],
}

impl ParamSpec {
    pub const fn new(
        id: &'static str,
        name: &'static str,
        unit: Unit,
        min: f32,
        max: f32,
        default: f32,
    ) -> ParamSpec {
        ParamSpec {
            id,
            name,
            unit,
            min,
            max,
            default,
            curve: Curve::Linear,
            choices: &[],
        }
    }

    pub const fn log(mut self) -> ParamSpec {
        self.curve = Curve::Log;
        self
    }

    /// A menu parameter; the value is an index into `choices`.
    pub const fn choice(
        id: &'static str,
        name: &'static str,
        choices: &'static [&'static str],
        default: usize,
    ) -> ParamSpec {
        ParamSpec {
            id,
            name,
            unit: Unit::None,
            min: 0.0,
            max: (choices.len() - 1) as f32,
            default: default as f32,
            curve: Curve::Linear,
            choices,
        }
    }

    /// An on/off switch.
    pub const fn toggle(id: &'static str, name: &'static str, default: bool) -> ParamSpec {
        ParamSpec::choice(id, name, &["Off", "On"], default as usize)
    }

    pub fn clamp(&self, v: f32) -> f32 {
        let v = if v.is_finite() {
            v.clamp(self.min, self.max)
        } else {
            self.default
        };
        if self.choices.is_empty() {
            v
        } else {
            v.round()
        }
    }

    /// Map a 0..=1 slider position to a value, honouring the curve.
    pub fn from_normalized(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self.curve {
            Curve::Linear => self.min + (self.max - self.min) * t,
            Curve::Log => self.min * (self.max / self.min).powf(t),
        }
    }

    pub fn to_normalized(&self, v: f32) -> f32 {
        let v = self.clamp(v);
        match self.curve {
            Curve::Linear => (v - self.min) / (self.max - self.min),
            Curve::Log => (v / self.min).ln() / (self.max / self.min).ln(),
        }
    }

    pub fn format(&self, v: f32) -> String {
        if let Some(c) = self
            .choices
            .get(v.round() as usize)
            .filter(|_| !self.choices.is_empty())
        {
            return (*c).to_string();
        }
        let digits = if v.abs() >= 100.0 {
            0
        } else if v.abs() >= 10.0 {
            1
        } else {
            2
        };
        format!("{v:.digits$}{}", self.unit.suffix())
    }
}

/// Leak a generated name or id once, for parameter lists built at startup (e.g. EQ bands).
pub fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Default values for a parameter list.
pub fn defaults(specs: &[ParamSpec]) -> Vec<f32> {
    specs.iter().map(|s| s.default).collect()
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-12).log10()
}

/// One-pole smoothing toward a target, to avoid zipper noise when parameters move.
#[derive(Clone, Copy, Debug)]
pub struct Smoothed {
    current: f32,
    target: f32,
    coeff: f32,
}

impl Smoothed {
    pub fn new(value: f32) -> Smoothed {
        Smoothed {
            current: value,
            target: value,
            coeff: 0.0,
        }
    }

    /// Reach ~63% of a change in `ms` milliseconds.
    pub fn prepare(&mut self, sample_rate: f32, ms: f32) {
        self.coeff = (-1.0 / (ms * 0.001 * sample_rate).max(1.0)).exp();
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump straight to the target (after reset, or for offline rendering).
    pub fn snap(&mut self) {
        self.current = self.target;
    }

    #[inline]
    pub fn tick(&mut self) -> f32 {
        self.current = self.target + (self.current - self.target) * self.coeff;
        if (self.current - self.target).abs() < 1e-7 {
            self.current = self.target;
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_mapping_round_trips() {
        let s = ParamSpec::new("f", "Freq", Unit::Hz, 20.0, 20_000.0, 1000.0).log();
        assert!((s.from_normalized(0.5) - 632.46).abs() < 0.1);
        for v in [20.0, 440.0, 1000.0, 20_000.0] {
            assert!((s.from_normalized(s.to_normalized(v)) - v).abs() / v < 1e-4);
        }
    }

    #[test]
    fn choices_clamp_and_format() {
        let s = ParamSpec::choice("t", "Type", &["Low", "High"], 0);
        assert_eq!(s.clamp(0.7), 1.0);
        assert_eq!(s.clamp(f32::NAN), 0.0);
        assert_eq!(s.format(1.0), "High");
        assert_eq!(
            ParamSpec::new("g", "Gain", Unit::Db, -40.0, 40.0, 0.0).format(-3.25),
            "-3.25 dB"
        );
    }

    #[test]
    fn smoothing_converges() {
        let mut s = Smoothed::new(0.0);
        s.prepare(48_000.0, 10.0);
        s.set(1.0);
        let mut v = 0.0;
        for _ in 0..48_00 {
            v = s.tick();
        }
        assert!((v - 1.0).abs() < 1e-3);
    }
}
