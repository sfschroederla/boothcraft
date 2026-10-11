use boothcraft_core::*;
use proptest::prelude::*;

fn ramp(n: usize) -> Vec<f32> {
    (0..n).map(|i| i as f32).collect()
}

fn doc(n: usize) -> Document {
    Document::new(
        48_000,
        vec![Rope::from_samples(&ramp(n)), Rope::from_samples(&ramp(n))],
    )
}

fn all(d: &Document) -> Vec<f32> {
    d.channels[0].to_vec(0..d.frames())
}

#[test]
fn delete_cut_paste_roundtrip() {
    let mut d = doc(200_000);
    let before = all(&d);
    let clip = d.copy(1000..150_000, ChannelMask::ALL);
    d.delete(1000..150_000, ChannelMask::ALL);
    assert_eq!(d.frames(), 200_000 - 149_000);
    d.paste(1000, &clip);
    assert_eq!(all(&d), before);
}

#[test]
fn single_channel_delete_silences() {
    let mut d = doc(1000);
    d.delete(100..200, ChannelMask(1));
    assert_eq!(d.frames(), 1000);
    assert!(d.channels[0].to_vec(100..200).iter().all(|&x| x == 0.0));
    assert_eq!(d.channels[1].to_vec(100..200)[0], 100.0);
}

#[test]
fn reverse_twice_is_identity_and_invert_cancels() {
    let mut d = doc(70_000);
    let before = all(&d);
    d.reverse(10..69_000, ChannelMask::ALL);
    d.reverse(10..69_000, ChannelMask::ALL);
    assert_eq!(all(&d), before);
    let clip = d.copy(0..70_000, ChannelMask::ALL);
    d.invert(0..70_000, ChannelMask::ALL);
    d.mix_paste(0, &clip, 1.0);
    assert!(all(&d).iter().all(|&x| x == 0.0));
}

#[test]
fn crop_and_markers() {
    let mut d = doc(1000);
    d.markers.push(Marker::cue("a", 500));
    d.markers.push(Marker::range("r", 100, 300));
    d.delete(200..400, ChannelMask::ALL);
    assert_eq!(d.markers[0].at, 300);
    assert_eq!(d.markers[1].kind, MarkerKind::Range { len: 100 });
    d.crop(100..300);
    assert_eq!(d.frames(), 200);
}

#[test]
fn peaks_match_samples() {
    let r = Rope::from_samples(&ramp(300_000));
    let p = r.peaks(0..300_000, 100);
    assert_eq!(p.len(), 100);
    assert_eq!(p[0].min, 0.0);
    assert_eq!(p[99].max, 299_999.0);
    let fine = r.peaks(10..20, 10);
    assert_eq!(fine[3].max, 13.0);
}

#[test]
fn history_undo_redo() {
    let mut h = History::new(doc(10));
    let mut d = h.current().clone();
    d.delete(0..5, ChannelMask::ALL);
    h.commit("Delete", d);
    assert_eq!(h.current().frames(), 5);
    assert!(h.undo());
    assert_eq!(h.current().frames(), 10);
    assert!(h.redo());
    assert_eq!(h.current().frames(), 5);
}

proptest! {
    #[test]
    fn rope_delete_insert_matches_vec(n in 1usize..200_000, a in 0usize..200_000, b in 0usize..200_000, at in 0usize..200_000) {
        let src = ramp(n);
        let (a, b) = (a.min(n), b.min(n));
        let (s, e) = (a.min(b), a.max(b));
        let mut rope = Rope::from_samples(&src);
        let mut model = src.clone();
        let removed = rope.delete(s..e);
        let removed_model: Vec<f32> = model.drain(s..e).collect();
        prop_assert_eq!(removed.to_vec(0..removed.len()), removed_model.clone());
        prop_assert_eq!(rope.to_vec(0..rope.len()), model.clone());
        let at = at.min(model.len());
        rope.insert(at, &removed);
        let tail = model.split_off(at);
        model.extend(removed_model);
        model.extend(tail);
        prop_assert_eq!(rope.to_vec(0..rope.len()), model);
    }

    #[test]
    fn edit_then_undo_restores(n in 1usize..100_000, a in 0usize..100_000, b in 0usize..100_000) {
        let (a, b) = (a.min(n), b.min(n));
        let (s, e) = (a.min(b), a.max(b));
        let mut h = History::new(doc(n));
        let before = all(h.current());
        let mut d = h.current().clone();
        d.delete(s..e, ChannelMask::ALL);
        d.reverse(0..d.frames(), ChannelMask::ALL);
        h.commit("edit", d);
        h.undo();
        prop_assert_eq!(all(h.current()), before);
    }
}

#[test]
fn markers_stay_sorted_and_clamped() {
    let mut d = doc(1000);
    assert_eq!(d.add_marker(Marker::cue(d.next_marker_name(), 500)), 0);
    assert_eq!(d.add_marker(Marker::cue(d.next_marker_name(), 100)), 0);
    assert_eq!(d.add_marker(Marker::range("r", 900, 500)), 2);
    assert_eq!(
        d.markers[2].kind,
        MarkerKind::Range { len: 100 },
        "range clamped to file end"
    );
    assert_eq!(d.markers[1].name, "Marker 01");
    assert_eq!(d.next_marker_name(), "Marker 03");
    let i = d.move_marker(0, 950).unwrap();
    assert_eq!(i, 2);
    assert!(d.markers.windows(2).all(|w| w[0].at <= w[1].at));
    assert_eq!(d.marker_after(500), Some(1));
    assert_eq!(d.marker_before(500), None);
    assert_eq!(d.marker_before(901), Some(1));
    d.rename_marker(0, "Intro");
    assert_eq!(d.markers[0].name, "Intro");
    assert!(d.remove_marker(5).is_none());
    assert_eq!(d.remove_marker(0).unwrap().name, "Intro");
}

#[test]
fn zero_crossings_of_a_sine() {
    // Period 100 frames: crossings at 0, 50, 100, ...
    let v: Vec<f32> = (0..1000)
        .map(|i| (i as f32 * std::f32::consts::TAU / 100.0).sin())
        .collect();
    let d = Document::new(48_000, vec![Rope::from_samples(&v)]);
    let all = ChannelMask::ALL;
    assert_eq!(d.zero_crossing(120, ZeroDir::Left, all, 1000), Some(100));
    assert_eq!(d.zero_crossing(120, ZeroDir::Right, all, 1000), Some(150));
    assert_eq!(d.zero_crossing(140, ZeroDir::Nearest, all, 1000), Some(150));
    assert_eq!(
        d.zero_crossing(120, ZeroDir::Right, all, 10),
        None,
        "search window respected"
    );
}

#[test]
fn zero_crossings_use_enabled_channels_only() {
    let pos = vec![0.5f32; 100];
    let mut flip = vec![0.5f32; 100];
    flip[60..].fill(-0.5);
    let d = Document::new(
        48_000,
        vec![Rope::from_samples(&pos), Rope::from_samples(&flip)],
    );
    assert_eq!(
        d.zero_crossing(10, ZeroDir::Right, ChannelMask(0b01), 100),
        None
    );
    assert_eq!(
        d.zero_crossing(10, ZeroDir::Right, ChannelMask(0b10), 100),
        Some(59)
    );
    // Summed, the second half is exactly zero, which counts as a crossing.
    assert_eq!(
        d.zero_crossing(10, ZeroDir::Right, ChannelMask::ALL, 100),
        Some(60)
    );
}

#[test]
fn fades_touch_only_their_ends() {
    let mut d = Document::new(48_000, vec![Rope::from_samples(&vec![1.0; 1000])]);
    d.fade_in(100, FadeShape::Power(0.0), ChannelMask::ALL);
    d.fade_out(200, FadeShape::Cosine, ChannelMask::ALL);
    let v = d.channels[0].to_vec(0..1000);
    assert_eq!(v[0], 0.0);
    assert!((v[50] - 0.5).abs() < 1e-6);
    assert_eq!(v[100..800], vec![1.0; 700][..]);
    assert!(v[999] < 0.001, "ends at silence, got {}", v[999]);
    assert!(v[800] > 0.99);
}

#[test]
fn small_appends_coalesce_into_full_blocks() {
    let mut r = Rope::new();
    for _ in 0..1000 {
        r.push_samples(&[0.5; 700]);
    }
    assert_eq!(r.len(), 700_000);
    assert!(
        r.blocks().len() <= 700_000 / BLOCK_FRAMES + 1,
        "{} blocks",
        r.blocks().len()
    );
    assert!(r.blocks()[..r.blocks().len() - 1]
        .iter()
        .all(|b| b.len() == BLOCK_FRAMES));
}

#[test]
fn overwrite_replaces_and_extends() {
    let mut d = doc(100);
    let new = vec![-1.0f32; 30];
    d.overwrite(80, &[&new, &new]);
    assert_eq!(d.frames(), 110);
    let v = d.channels[1].to_vec(0..110);
    assert_eq!(v[79], 79.0);
    assert!(v[80..110].iter().all(|&x| x == -1.0));
    d.overwrite(10, &[&new[..5]]);
    assert_eq!(d.frames(), 110, "inside the file: no extension");
    assert_eq!(
        d.channels[1].to_vec(10..16),
        vec![-1.0, -1.0, -1.0, -1.0, -1.0, 15.0],
        "mono source fills every channel"
    );
}
