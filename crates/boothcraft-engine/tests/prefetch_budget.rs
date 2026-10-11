//! Kept in its own process (cargo gives each `tests/*.rs` file its own binary): it lowers the
//! process-wide block store's memory budget, which would otherwise race with any other test that
//! also depends on real eviction timing or an exact resident-byte reading.

use boothcraft_core::{BlockStore, Document, Rope};
use boothcraft_engine::{Prefetcher, Shared};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn keeps_the_playhead_window_resident_under_memory_pressure() {
    let store = BlockStore::global();
    store.set_budget_mb(1);

    let shared = Shared::new();
    let frames = 2_000_000usize; // ~8MB, well over the 1MB budget
    let v: Vec<f32> = (0..frames).map(|i| i as f32).collect();
    let doc = Arc::new(Document::new(48_000, vec![Rope::from_samples(&v)]));
    shared.set_document(doc.clone());

    // Park the playhead somewhere the background spiller, left alone, would have no reason to
    // favour: without prefetch, LRU would evict it along with everything else untouched.
    let target = 1_000_000;
    shared.seek(target);
    let prefetcher = Prefetcher::spawn(shared.clone());

    let resident_at_target = (0..200).find_map(|_| {
        std::thread::sleep(Duration::from_millis(20));
        let mut out = [0.0; 4];
        doc.channels[0]
            .read_resident(target, &mut out)
            .then_some(out)
    });
    drop(prefetcher);

    let out = resident_at_target.expect("prefetch kept the playhead window resident in time");
    assert_eq!(
        out,
        [
            target as f32,
            target as f32 + 1.0,
            target as f32 + 2.0,
            target as f32 + 3.0
        ]
    );
}
