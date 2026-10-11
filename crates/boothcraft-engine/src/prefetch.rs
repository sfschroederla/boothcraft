//! Keeps the blocks the renderer is about to need resident, so the real-time audio callback can
//! use [`boothcraft_core::Rope::read_resident`] and never has to block reloading them from disk
//! (see [`crate::Shared::underruns`] for what happens when prefetch doesn't keep up).

use crate::shared::Shared;
use boothcraft_core::Rope;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

/// How far ahead of the playhead to keep resident.
const LOOKAHEAD_SECS: f64 = 10.0;
/// How far behind the playhead to keep resident, for a small rewind or scrub.
const LOOKBEHIND_SECS: f64 = 1.0;
/// A second's worth of audio is kept warm at the loop point and the selection start, in case
/// playback jumps there next.
const HINT_WINDOW_SECS: f64 = 1.0;
const TICK: Duration = Duration::from_millis(20);

/// Owns a background thread that periodically pins the blocks the renderer is likely to read
/// next. Stops the thread when dropped.
pub struct Prefetcher {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Prefetcher {
    pub fn spawn(shared: Arc<Shared>) -> Prefetcher {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = std::thread::spawn(move || run(shared, stop_flag));
        Prefetcher {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run(shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    // Holds every block's resident data for the windows below, so nothing in them can be evicted
    // out from under the renderer between one tick and the next.
    let mut pinned: Vec<Arc<Vec<f32>>> = Vec::new();
    while !stop.load(Relaxed) {
        std::thread::sleep(TICK);
        pinned.clear();
        let guard = shared.doc.load();
        let Some(doc) = guard.as_ref() else { continue };
        let rate = doc.sample_rate as f64;
        let frames = doc.frames();
        let pos = shared.position();
        let (loop_start, _) = shared.loop_bounds();
        let sel_start = shared.selection_hint();
        let windows = [
            window(
                pos - LOOKBEHIND_SECS * rate,
                pos + LOOKAHEAD_SECS * rate,
                frames,
            ),
            window(
                loop_start as f64,
                loop_start as f64 + HINT_WINDOW_SECS * rate,
                frames,
            ),
            window(
                sel_start as f64,
                sel_start as f64 + HINT_WINDOW_SECS * rate,
                frames,
            ),
        ];
        for range in windows {
            if range.is_empty() {
                continue;
            }
            for ch in &doc.channels {
                pin_range(ch, range.clone(), &mut pinned);
            }
        }
    }
}

fn window(start: f64, end: f64, frames: usize) -> Range<usize> {
    let start = (start.max(0.0) as usize).min(frames);
    let end = (end.max(0.0) as usize).min(frames);
    start..end
}

/// Load (if needed) and hold every block of `rope` overlapping `range`.
fn pin_range(rope: &Rope, range: Range<usize>, pinned: &mut Vec<Arc<Vec<f32>>>) {
    let mut pos = 0usize;
    for b in rope.blocks() {
        let (bs, be) = (pos, pos + b.len());
        pos = be;
        if be <= range.start {
            continue;
        }
        if bs >= range.end {
            break;
        }
        pinned.push(b.data());
    }
}

// See `tests/prefetch_budget.rs` for the "keeps the playhead window resident under memory
// pressure" test: it depends on the real (process-wide) block store's memory budget, so it needs
// its own process rather than sharing this binary's global store with every other unit test here.
