//! Fade shapes for the on-waveform fade handles.

/// Shape of a fade. `Power(c)`: `c > 0` is logarithmic (fast rise), `c < 0` exponential (slow rise),
/// `0` linear; `c` is clamped to `-1..=1`. `Cosine` is an S-curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FadeShape {
    Power(f32),
    Cosine,
}

/// Fade-in gain at position `t` in `0..=1` (0 → silent, 1 → full level).
pub fn fade_gain(t: f32, shape: FadeShape) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match shape {
        FadeShape::Cosine => 0.5 - 0.5 * (std::f32::consts::PI * t).cos(),
        FadeShape::Power(c) => {
            let c = c.clamp(-1.0, 1.0);
            let k = 1.0 + 4.0 * c.abs();
            if c >= 0.0 {
                1.0 - (1.0 - t).powf(k)
            } else {
                t.powf(k)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_monotonic() {
        for shape in [
            FadeShape::Power(-1.0),
            FadeShape::Power(0.0),
            FadeShape::Power(0.6),
            FadeShape::Cosine,
        ] {
            assert_eq!(fade_gain(0.0, shape), 0.0, "{shape:?}");
            assert!((fade_gain(1.0, shape) - 1.0).abs() < 1e-6, "{shape:?}");
            let mut prev = 0.0;
            for i in 1..=100 {
                let g = fade_gain(i as f32 / 100.0, shape);
                assert!(g >= prev, "{shape:?} not monotonic at {i}");
                prev = g;
            }
        }
        assert_eq!(fade_gain(0.5, FadeShape::Power(0.0)), 0.5);
        assert!(
            fade_gain(0.5, FadeShape::Power(1.0)) > 0.9,
            "log rises fast"
        );
        assert!(
            fade_gain(0.5, FadeShape::Power(-1.0)) < 0.1,
            "exp rises slowly"
        );
    }
}
