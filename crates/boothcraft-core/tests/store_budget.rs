//! Exercises the disk-backed block store under real memory pressure. Kept to a single test in its
//! own process (cargo gives each `tests/*.rs` file its own binary) so the shared global store
//! isn't polluted by, or doesn't pollute, any other test.

use boothcraft_core::{BlockStore, Rope};
use std::time::{Duration, Instant};

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn budget_is_enforced_and_data_survives_spill_and_drop() {
    let store = BlockStore::global();
    store.set_budget_mb(8);
    let budget_bytes = store.budget_mb() * 1024 * 1024;

    // Comfortably more than one 256MB segment, so at least one segment gets sealed (the writer
    // rolls past it) rather than staying the active segment for the rest of the test. A segment
    // can only be deleted once sealed; see the `active_segment_is_not_deleted_*` unit test in
    // `store.rs` for why the still-active segment is deliberately exempt (no compaction there).
    let frames = 70_000_000usize; // ~280MB of mono f32 audio.
    let rope = Rope::from_samples(&vec![0.5f32; frames]);

    // A little slack for the handful of blocks that can still be mid-flight when we sample.
    let slack = 4 * 1024 * 1024;
    let settled = wait_until(Duration::from_secs(20), || {
        store.resident_bytes() <= budget_bytes + slack
    });
    assert!(
        settled,
        "resident bytes {} did not settle under budget {budget_bytes}",
        store.resident_bytes()
    );
    let spilled_disk_bytes = store.disk_bytes();
    assert!(
        spilled_disk_bytes > 0,
        "most of the audio should have spilled to disk"
    );

    // Reading back (including spilled blocks, which reload transparently) must still be correct.
    let front = rope.to_vec(0..1_000);
    let back = rope.to_vec(frames - 1_000..frames);
    assert!(front.iter().all(|&x| x == 0.5));
    assert!(back.iter().all(|&x| x == 0.5));

    // Dropping every block frees the sealed segment(s)' disk space; only the segment the writer
    // was last appending to is left (for a later rollover to reclaim), so most — not necessarily
    // all — of it should come back. This can lag drop() slightly: the spiller thread's registry
    // scan transiently clones a `Weak<Block>` upgrade while deciding whether to spill it, which
    // can delay (never prevent) that block's own final drop by up to one spill interval.
    drop(rope);
    let reclaimed = wait_until(Duration::from_secs(5), || {
        store.disk_bytes() < spilled_disk_bytes / 2
    });
    assert!(
        reclaimed,
        "expected most disk space back: was {spilled_disk_bytes}, now {}",
        store.disk_bytes()
    );
}
