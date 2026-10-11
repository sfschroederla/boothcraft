use boothcraft_core::{Document, Rope};
use boothcraft_io::*;

fn sine(frames: usize, rate: u32) -> Document {
    let l: Vec<f32> = (0..frames)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
        .collect();
    let r: Vec<f32> = l.iter().map(|x| -x).collect();
    Document::new(rate, vec![Rope::from_samples(&l), Rope::from_samples(&r)])
}

fn max_err(a: &Document, b: &Document) -> f32 {
    a.to_interleaved(0..a.frames())
        .iter()
        .zip(b.to_interleaved(0..b.frames()))
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

fn roundtrip(name: &str, fmt: Format, depth: BitDepth, tol: f32) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    let doc = sine(100_000, 44_100);
    write(&doc, &path, &WriteOptions { format: fmt, depth }).unwrap();
    let back = read(&path).unwrap();
    assert_eq!(back.sample_rate, 44_100);
    assert_eq!(back.channel_count(), 2);
    assert_eq!(back.frames(), doc.frames());
    assert!(
        max_err(&doc, &back) <= tol,
        "error {}",
        max_err(&doc, &back)
    );
}

#[test]
fn wav_16() {
    roundtrip("a.wav", Format::Wav, BitDepth::Int16, 1.0 / 32768.0);
}
#[test]
fn wav_24() {
    roundtrip("a.wav", Format::Wav, BitDepth::Int24, 1.0 / 8_388_608.0);
}
#[test]
fn wav_float_is_exact() {
    roundtrip("a.wav", Format::Wav, BitDepth::Float32, 0.0);
}
#[test]
fn aiff_16() {
    roundtrip("a.aiff", Format::Aiff, BitDepth::Int16, 1.0 / 32768.0);
}
#[test]
fn aiff_24() {
    roundtrip("a.aif", Format::Aiff, BitDepth::Int24, 1.0 / 8_388_608.0);
}
#[test]
fn flac_16() {
    roundtrip("a.flac", Format::Flac, BitDepth::Int16, 1.0 / 32768.0);
}
#[test]
fn flac_24() {
    roundtrip("a.flac", Format::Flac, BitDepth::Int24, 1.0 / 8_388_608.0);
}
