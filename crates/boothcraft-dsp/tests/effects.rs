//! Checks every registered real-time effect against the rules all effects must follow.

use assert_no_alloc::{assert_no_alloc, AllocDisabler};
use boothcraft_dsp::registry::{self, EFFECTS};
use boothcraft_dsp::Effect;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const RATE: f32 = 48_000.0;
const MAX_BLOCK: usize = 512;

/// Deterministic test signal: a chirp plus noise, with an impulse and some silence.
fn signal(n: usize, seed: u32) -> Vec<f32> {
    let mut x = seed;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let noise = (x as f32 / u32::MAX as f32 - 0.5) * 0.2;
            let t = i as f32 / RATE;
            let chirp = (std::f32::consts::TAU * (100.0 + 2000.0 * t) * t).sin() * 0.5;
            if i == 1000 {
                1.0
            } else if (20_000..24_000).contains(&i) {
                0.0
            } else {
                chirp + noise
            }
        })
        .collect()
}

/// Give every parameter a non-default value so more code paths run.
fn exercise_params(e: &mut dyn Effect) {
    let specs = e.specs().to_vec();
    for (i, s) in specs.iter().enumerate() {
        e.set_param(i, s.from_normalized(0.37 + 0.05 * (i % 5) as f32));
    }
}

fn run(e: &mut dyn Effect, input: &[Vec<f32>], sizes: &mut dyn FnMut() -> usize) -> Vec<Vec<f32>> {
    let mut out = input.to_vec();
    let n = input[0].len();
    let mut at = 0;
    while at < n {
        let len = sizes().clamp(1, MAX_BLOCK).min(n - at);
        let (a, b) = out.split_at_mut(1);
        let mut slices: [&mut [f32]; 2] = [&mut a[0][at..at + len], &mut b[0][at..at + len]];
        assert_no_alloc(|| e.process(&mut slices));
        at += len;
    }
    out
}

fn fresh(id: &str) -> Box<dyn Effect> {
    let mut e = registry::create(id).unwrap();
    exercise_params(e.as_mut());
    e.prepare(RATE, 2, MAX_BLOCK);
    e.reset();
    e
}

#[test]
fn block_size_does_not_change_the_output() {
    let input = vec![signal(48_000, 1), signal(48_000, 2)];
    for info in EFFECTS.iter().filter(|e| e.is_realtime()) {
        let fixed = run(fresh(info.id).as_mut(), &input, &mut || MAX_BLOCK);
        let mut x = 99u32;
        let random = run(fresh(info.id).as_mut(), &input, &mut || {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            1 + (x >> 8) as usize % MAX_BLOCK
        });
        for (c, (a, b)) in fixed.iter().zip(&random).enumerate() {
            let worst = a
                .iter()
                .zip(b)
                .map(|(p, q)| (p - q).abs())
                .fold(0.0, f32::max);
            assert!(
                worst < 1e-5,
                "{}: channel {c} differs by {worst} with random block sizes",
                info.id
            );
        }
    }
}

#[test]
fn output_stays_finite() {
    let loud: Vec<f32> = (0..20_000)
        .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    let inputs = [
        vec![vec![0.0; 20_000]; 2],
        vec![loud.clone(), loud],
        vec![signal(20_000, 3), signal(20_000, 4)],
    ];
    for info in EFFECTS.iter().filter(|e| e.is_realtime()) {
        for input in &inputs {
            let out = run(fresh(info.id).as_mut(), input, &mut || 256);
            assert!(
                out.iter().flatten().all(|x| x.is_finite()),
                "{} produced NaN/inf",
                info.id
            );
        }
    }
}

#[test]
fn every_effect_has_sane_specs() {
    for info in EFFECTS {
        let (specs, values) = match &info.kind {
            registry::Kind::Realtime(make) => {
                let e = make();
                (e.specs().to_vec(), e.values().to_vec())
            }
            registry::Kind::Offline(make) => {
                let e = make();
                (e.specs().to_vec(), e.values().to_vec())
            }
        };
        assert_eq!(specs.len(), values.len(), "{}", info.id);
        let mut ids = std::collections::HashSet::new();
        for (s, v) in specs.iter().zip(&values) {
            assert!(ids.insert(s.id), "{}: duplicate param id {}", info.id, s.id);
            assert!(
                s.min < s.max && (s.min..=s.max).contains(&s.default),
                "{}: {} range",
                info.id,
                s.id
            );
            assert_eq!(*v, s.default, "{}: {} starts at default", info.id, s.id);
            if s.curve == boothcraft_dsp::Curve::Log {
                assert!(s.min > 0.0, "{}: log param {} needs min > 0", info.id, s.id);
            }
        }
    }
}

#[test]
fn eqs_at_neutral_settings_pass_audio_through() {
    let input = vec![signal(10_000, 5), signal(10_000, 6)];
    for id in [
        "parametric_eq",
        "graphic_eq_10",
        "graphic_eq_20",
        "graphic_eq_30",
    ] {
        let mut e = registry::create(id).unwrap();
        e.prepare(RATE, 2, MAX_BLOCK);
        e.reset();
        let out = run(e.as_mut(), &input, &mut || MAX_BLOCK);
        let worst = out
            .iter()
            .flatten()
            .zip(input.iter().flatten())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-6, "{id} is not transparent at defaults: {worst}");
    }
}

#[test]
fn parametric_band_response_matches_graph() {
    use boothcraft_dsp::effects::parametric_eq::{idx, response_db};
    let mut e = registry::create("parametric_eq").unwrap();
    let b = idx::PEAK + 4 * 2; // band 3
    e.set_param(b + 1, 1000.0);
    e.set_param(b + 2, 9.0);
    e.set_param(b + 3, 2.0);
    assert!((response_db(e.values(), RATE, 1000.0) - 9.0).abs() < 0.2);
    e.prepare(RATE, 1, 4096);
    e.reset();
    let mut sine: Vec<f32> = (0..48_000)
        .map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / RATE).sin() * 0.1)
        .collect();
    e.process(&mut [&mut sine[..]]);
    let peak = sine[30_000..].iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!((20.0 * (peak / 0.1).log10() - 9.0).abs() < 0.3);
}

#[test]
fn notch_removes_hum() {
    let mut e = registry::create("notch").unwrap();
    e.set_param(2, -60.0);
    e.prepare(RATE, 1, 4096);
    e.reset();
    let mut hum: Vec<f32> = (0..96_000)
        .map(|i| (i as f32 * std::f32::consts::TAU * 60.0 / RATE).sin())
        .collect();
    e.process(&mut [&mut hum[..]]);
    let peak = hum[72_000..].iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak < 0.01, "60 Hz left at {peak}");
}

#[test]
fn parameter_changes_do_not_allocate() {
    for info in EFFECTS.iter().filter(|e| e.is_realtime()) {
        let mut e = registry::create(info.id).unwrap();
        e.prepare(RATE, 2, MAX_BLOCK);
        e.reset();
        let specs = e.specs().to_vec();
        for (i, s) in specs.iter().enumerate() {
            for t in [0.0, 0.5, 1.0] {
                let v = s.from_normalized(t);
                assert_no_alloc(|| e.set_param(i, v));
            }
        }
        // Make sure the result still processes.
        let mut a = signal(MAX_BLOCK, 9);
        let mut b = signal(MAX_BLOCK, 10);
        assert_no_alloc(|| e.process(&mut [&mut a[..], &mut b[..]]));
        assert!(a.iter().chain(&b).all(|x| x.is_finite()), "{}", info.id);
    }
}
