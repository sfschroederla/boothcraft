//! Kept in its own process (cargo gives each `tests/*.rs` file its own binary): it lowers the
//! process-wide block store's memory budget, which would otherwise race with any other test that
//! also depends on real eviction timing or an exact resident-byte reading.

use boothcraft_core::{BlockStore, Document, Rope};
use boothcraft_engine::{Renderer, Shared};
use std::sync::Arc;
use std::time::Duration;

fn ramp_doc(frames: usize, rate: u32) -> Arc<Document> {
    let v: Vec<f32> = (0..frames).map(|i| i as f32).collect();
    Arc::new(Document::new(rate, vec![Rope::from_samples(&v)]))
}

#[test]
fn evicted_audio_renders_silence_and_counts_an_underrun_instead_of_blocking() {
    let store = BlockStore::global();
    store.set_budget_mb(1);
    let sh = Shared::new();
    let doc = ramp_doc(2_000_000, 48_000); // ~8MB, well over the 1MB budget
    sh.set_document(doc.clone());
    assert_eq!(sh.underruns(), 0);

    // Wait for the background spiller to evict frame 0's block specifically (LRU evicts the
    // oldest — here, earliest-created, i.e. earliest-in-file — blocks first), rather than relying
    // on a global resident-byte threshold. Each check that finds it still resident touches it (any
    // successful read legitimately counts as "recently used"), so polling faster than the
    // spiller's own interval would keep re-marking it fresh and could starve it indefinitely;
    // each attempt here waits out a full interval undisturbed first.
    let mut probe = [0.0f32; 4];
    let evicted = (0..10).any(|_| {
        std::thread::sleep(Duration::from_millis(500));
        !doc.channels[0].read_resident(0, &mut probe)
    });
    assert!(evicted, "background spiller did not evict frame 0 in time");

    sh.play();
    let mut r = Renderer::new(sh.clone());
    let mut out = vec![0.0; 1000];
    r.render(&mut out, 1, 48_000);
    assert!(
        out.iter().all(|&x| x == 0.0),
        "silence while the evicted block reloads"
    );
    assert!(sh.underruns() > 0);
    assert!(
        sh.position() > 0.0,
        "still advances normally despite the gap"
    );
}
