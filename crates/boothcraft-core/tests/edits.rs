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
