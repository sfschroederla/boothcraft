//! Disk-backed storage for audio blocks, shared by the whole process.
//!
//! Each [`Block`](crate::block::Block) keeps its peak overview in RAM but can spill its sample
//! data to an append-only segment file once a memory budget is exceeded. Blocks are immutable, so
//! a block is written to disk at most once; evicting it again later just drops the resident copy.

use crate::block::Block;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

/// Where a spilled block's samples live on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    segment: u32,
    offset: u64,
    frames: u32,
}

/// Segment files are capped at this size; a write that would exceed it starts a new segment.
const SEGMENT_CAP: u64 = 256 * 1024 * 1024;
const DEFAULT_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
/// A process's segment directory is considered abandoned (and removed) once its heartbeat is
/// older than this.
const STALE_AFTER: Duration = Duration::from_secs(300);
const SPILL_INTERVAL: Duration = Duration::from_millis(200);

struct Writer {
    segment: u32,
    file: File,
    offset: u64,
}

impl Writer {
    fn open(dir: &Path, segment: u32) -> Writer {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(segment_name(segment)))
            .expect("create block segment file");
        Writer {
            segment,
            file,
            offset: 0,
        }
    }
}

/// Process-wide block storage: a resident-byte budget enforced by writing least-recently-used
/// blocks to disk.
pub struct BlockStore {
    dir: PathBuf,
    writer: Mutex<Writer>,
    readers: Mutex<HashMap<u32, File>>,
    registry: Mutex<Vec<Weak<Block>>>,
    /// Number of still-live blocks whose data lives in each segment, so a segment can be deleted
    /// once nothing references it any more. There is no compaction of partially-live segments.
    live_counts: Mutex<HashMap<u32, u32>>,
    /// The segment the writer is currently appending to. Even if its live count reaches zero, it
    /// must not be deleted while still active: a later write would land in the (still open, but
    /// now unlinked) file, and the delete would hide that data from any later read of it, since
    /// the read path opens the segment by path, not by the write's file handle.
    active_segment: AtomicU32,
    resident_bytes: AtomicU64,
    budget_bytes: AtomicU64,
    clock: AtomicU64,
}

impl BlockStore {
    /// The process-wide store. Created, with its spiller thread started, on first use.
    pub fn global() -> &'static BlockStore {
        static STORE: OnceLock<BlockStore> = OnceLock::new();
        let store = STORE.get_or_init(BlockStore::init);
        // A second `OnceLock` guards the spiller thread so exactly one gets spawned even if two
        // threads race to initialize `STORE` above (one blocks inside `get_or_init` and observes
        // the already-built store, so both would otherwise think they're "first").
        static SPILLER: OnceLock<()> = OnceLock::new();
        SPILLER.get_or_init(|| store.spawn_spiller());
        store
    }

    fn init() -> BlockStore {
        let base = base_dir();
        let _ = fs::create_dir_all(&base);
        cleanup_stale(&base);
        let dir = base.join(std::process::id().to_string());
        let _ = fs::create_dir_all(&dir);
        touch_heartbeat(&dir);
        let writer = Writer::open(&dir, 0);
        BlockStore {
            dir,
            writer: Mutex::new(writer),
            readers: Mutex::new(HashMap::new()),
            registry: Mutex::new(Vec::new()),
            live_counts: Mutex::new(HashMap::new()),
            active_segment: AtomicU32::new(0),
            resident_bytes: AtomicU64::new(0),
            budget_bytes: AtomicU64::new(DEFAULT_BUDGET_BYTES),
            clock: AtomicU64::new(0),
        }
    }

    /// Directory this process's segment files live in.
    pub fn cache_dir(&self) -> &Path {
        &self.dir
    }

    pub fn set_budget_mb(&self, mb: u64) {
        self.budget_bytes.store(mb.max(1) * 1024 * 1024, Relaxed);
    }

    pub fn budget_mb(&self) -> u64 {
        self.budget_bytes.load(Relaxed) / (1024 * 1024)
    }

    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes.load(Relaxed)
    }

    /// Waits (briefly — a courtesy, not a guarantee) for the background spiller to bring resident
    /// bytes back at or under budget, returning early once it does, or after `timeout`, whichever
    /// comes first.
    ///
    /// The spiller runs on its own timer and reacts to memory pressure after the fact, which is
    /// fine for an interactive session (there's always another 200ms along). A short batch
    /// operation — decoding a big file and then immediately re-encoding it, say — can otherwise
    /// finish well before the spiller gets a chance to catch up, briefly holding far more than the
    /// budget. Call this between such phases to give it that chance.
    pub fn settle(&self, timeout: Duration) {
        let start = std::time::Instant::now();
        while self.resident_bytes() > self.budget_bytes.load(Relaxed) {
            if start.elapsed() > timeout {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Like [`BlockStore::settle`], but only waits once resident bytes exceed `budget * slack`,
    /// rather than the instant they exceed the budget itself (which, given the spiller's own
    /// 200ms-or-so cadence, is normal and not worth pausing for). Meant to be called often from a
    /// tight production loop — decoding a file packet by packet, say — as a cheap single atomic
    /// load in the common case, self-throttling production against a slow spiller instead of
    /// racing arbitrarily far ahead of it.
    pub fn throttle(&self, slack: u64, timeout: Duration) {
        if self.resident_bytes() > self.budget_bytes.load(Relaxed).saturating_mul(slack.max(1)) {
            self.settle(timeout);
        }
    }

    /// Total size of this process's segment files on disk right now.
    pub fn disk_bytes(&self) -> u64 {
        fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "seg"))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }

    /// A logical clock for LRU ordering; cheap and wait-free, safe to call from the audio thread.
    pub(crate) fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Relaxed)
    }

    pub(crate) fn note_resident(&self, bytes: u64) {
        self.resident_bytes.fetch_add(bytes, Relaxed);
    }

    pub(crate) fn note_evicted(&self, bytes: u64) {
        self.resident_bytes.fetch_sub(bytes, Relaxed);
    }

    pub(crate) fn register(&self, block: &Arc<Block>) {
        self.registry.lock().unwrap().push(Arc::downgrade(block));
    }

    pub(crate) fn write(&self, data: &[f32]) -> Location {
        let bytes = f32_to_bytes(data);
        let mut w = self.writer.lock().unwrap();
        if w.offset + bytes.len() as u64 > SEGMENT_CAP {
            let sealed = w.segment;
            *w = Writer::open(&self.dir, w.segment + 1);
            self.active_segment.store(w.segment, Relaxed);
            self.seal_segment(sealed);
        }
        let _ = w.file.write_all(&bytes);
        let loc = Location {
            segment: w.segment,
            offset: w.offset,
            frames: data.len() as u32,
        };
        w.offset += bytes.len() as u64;
        drop(w);
        *self
            .live_counts
            .lock()
            .unwrap()
            .entry(loc.segment)
            .or_insert(0) += 1;
        loc
    }

    /// Called when a block whose data lives at `loc` is dropped. Deletes the segment file once no
    /// live block references it any more, unless it is still the writer's active segment (see
    /// [`BlockStore::active_segment`]), in which case `seal_segment` will delete it once the
    /// writer moves on.
    pub(crate) fn block_dropped(&self, loc: Location) {
        let mut counts = self.live_counts.lock().unwrap();
        let Some(n) = counts.get_mut(&loc.segment) else {
            return;
        };
        *n -= 1;
        if *n > 0 || loc.segment == self.active_segment.load(Relaxed) {
            return;
        }
        counts.remove(&loc.segment);
        drop(counts);
        self.delete_segment(loc.segment);
    }

    /// Called once a segment will never be written to again (the writer has moved past it). If it
    /// already has no live blocks — every one written to it was already dropped while it was still
    /// active, so `block_dropped` deliberately left it in place — delete it now.
    fn seal_segment(&self, segment: u32) {
        let mut counts = self.live_counts.lock().unwrap();
        if !matches!(counts.get(&segment), Some(&0) | None) {
            return;
        }
        counts.remove(&segment);
        drop(counts);
        self.delete_segment(segment);
    }

    fn delete_segment(&self, segment: u32) {
        self.readers.lock().unwrap().remove(&segment);
        let _ = fs::remove_file(self.dir.join(segment_name(segment)));
    }

    pub(crate) fn read(&self, loc: Location) -> Vec<f32> {
        let zeros = || vec![0.0; loc.frames as usize];
        let mut open = self.readers.lock().unwrap();
        let file = match open.entry(loc.segment) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                match File::open(self.dir.join(segment_name(loc.segment))) {
                    Ok(f) => e.insert(f),
                    Err(_) => return zeros(),
                }
            }
        };
        let mut buf = vec![0u8; loc.frames as usize * 4];
        if file.seek(SeekFrom::Start(loc.offset)).is_err() || file.read_exact(&mut buf).is_err() {
            return zeros();
        }
        bytes_to_f32(&buf)
    }

    /// Spawn the background thread that enforces the resident-byte budget. Takes `&'static self`
    /// so the thread can run for the process's lifetime without an `Arc`.
    fn spawn_spiller(&'static self) {
        std::thread::spawn(move || loop {
            std::thread::sleep(SPILL_INTERVAL);
            touch_heartbeat(&self.dir);
            self.spill_pass();
        });
    }

    fn spill_pass(&self) {
        let budget = self.budget_bytes.load(Relaxed);
        if self.resident_bytes.load(Relaxed) <= budget {
            return;
        }
        // Collect candidates while pruning dead (dropped) blocks from the registry.
        let mut candidates: Vec<(u64, Arc<Block>)> = Vec::new();
        self.registry.lock().unwrap().retain(|w| match w.upgrade() {
            Some(b) => {
                if b.is_spillable() {
                    candidates.push((b.last_use(), b.clone()));
                }
                true
            }
            None => false,
        });
        candidates.sort_by_key(|(t, _)| *t);
        for (_, block) in candidates {
            if self.resident_bytes.load(Relaxed) <= budget {
                break;
            }
            block.spill(self);
        }
    }

    /// Remove this process's segment directory. Intended to run once, near process exit; see
    /// [`install_cleanup_guard`].
    fn cleanup(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Removes the block store's on-disk directory when dropped. Hold one of these for the lifetime
/// of `main` in every binary that opens audio files, so temporary segment files don't pile up.
#[must_use]
pub struct CleanupGuard(());

/// Ensures the block store is initialized and arranges for its directory to be removed when the
/// returned guard is dropped (normally, at the end of `main`).
pub fn install_cleanup_guard() -> CleanupGuard {
    BlockStore::global();
    CleanupGuard(())
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        BlockStore::global().cleanup();
    }
}

fn base_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "Boothcraft")
        .map(|d| d.cache_dir().join("blocks"))
        .unwrap_or_else(|| std::env::temp_dir().join("boothcraft-blocks"))
}

fn segment_name(id: u32) -> String {
    format!("{id}.seg")
}

fn heartbeat_path(dir: &Path) -> PathBuf {
    dir.join(".heartbeat")
}

fn touch_heartbeat(dir: &Path) {
    let _ = fs::write(heartbeat_path(dir), []);
}

/// Remove subdirectories whose heartbeat is missing or old: these belong to a process that
/// crashed or was killed before it could clean up after itself.
fn cleanup_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let stale = match fs::metadata(heartbeat_path(&path)).and_then(|m| m.modified()) {
            Ok(t) => now
                .duration_since(t)
                .map(|age| age > STALE_AFTER)
                .unwrap_or(true),
            Err(_) => true,
        };
        if stale {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

fn f32_to_bytes(data: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4);
    for &x in data {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&c| f32::from_le_bytes(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store rooted in a fresh temp directory, isolated from the real process-wide one, so
    /// tests can exercise spill/reload without interfering with each other.
    fn test_store() -> BlockStore {
        let dir = std::env::temp_dir().join(format!("boothcraft-store-test-{}", uid()));
        let _ = fs::create_dir_all(&dir);
        BlockStore {
            dir: dir.clone(),
            writer: Mutex::new(Writer::open(&dir, 0)),
            readers: Mutex::new(HashMap::new()),
            registry: Mutex::new(Vec::new()),
            live_counts: Mutex::new(HashMap::new()),
            active_segment: AtomicU32::new(0),
            resident_bytes: AtomicU64::new(0),
            budget_bytes: AtomicU64::new(DEFAULT_BUDGET_BYTES),
            clock: AtomicU64::new(0),
        }
    }

    fn uid() -> u64 {
        use std::sync::atomic::AtomicU64 as A;
        static N: A = A::new(0);
        N.fetch_add(1, Relaxed) ^ (std::process::id() as u64) << 32
    }

    #[test]
    fn write_then_read_is_bit_exact() {
        let store = test_store();
        let data: Vec<f32> = (0..70_000).map(|i| (i as f32 * 0.0001).sin()).collect();
        let loc = store.write(&data);
        assert_eq!(loc.frames as usize, data.len());
        let back = store.read(loc);
        assert_eq!(back, data);
        fs::remove_dir_all(&store.dir).ok();
    }

    #[test]
    fn writes_roll_over_to_a_new_segment_past_the_cap() {
        let store = test_store();
        let big = vec![1.0f32; (SEGMENT_CAP / 4) as usize];
        let loc1 = store.write(&big);
        let loc2 = store.write(&[2.0, 3.0]);
        assert_eq!(loc1.segment, 0);
        assert_eq!(loc2.segment, 1, "second write did not fit in segment 0");
        assert_eq!(store.read(loc2), vec![2.0, 3.0]);
        fs::remove_dir_all(&store.dir).ok();
    }

    #[test]
    fn segment_file_is_deleted_once_sealed_and_its_last_block_is_dropped() {
        let store = test_store();
        let loc_a = store.write(&[1.0, 2.0]);
        let loc_b = store.write(&[3.0, 4.0]);
        assert_eq!(
            loc_a.segment, loc_b.segment,
            "both small writes share segment 0"
        );
        let path = store.dir.join(segment_name(loc_a.segment));
        assert!(path.exists());

        store.block_dropped(loc_a);
        assert!(path.exists(), "segment b is still live");

        // Force a rollover to segment 1 so segment 0 is sealed (no longer written to).
        let big = vec![0.0f32; (SEGMENT_CAP / 4) as usize];
        let loc_c = store.write(&big);
        assert_eq!(loc_c.segment, loc_a.segment + 1);
        assert!(
            path.exists(),
            "block b is still live in the now-sealed segment"
        );

        store.block_dropped(loc_b);
        assert!(!path.exists(), "sealed, and no live blocks left in it");
        fs::remove_dir_all(&store.dir).ok();
    }

    /// Regression test: a segment must never be deleted while the writer could still append to
    /// it, even if every block written to it so far has already been dropped. Deleting it out from
    /// under the open write handle would let a later write silently land in an unlinked file that
    /// a subsequent read, which opens the segment by path, could then never find.
    #[test]
    fn active_segment_is_not_deleted_even_at_zero_live_blocks() {
        let store = test_store();
        let loc_a = store.write(&[1.0, 2.0, 3.0, 4.0]);
        let path = store.dir.join(segment_name(loc_a.segment));

        store.block_dropped(loc_a);
        assert!(path.exists(), "still the writer's active segment");

        // A later write into that same (still active) segment must still work and read back
        // correctly — this is exactly the scenario the bug broke.
        let loc_b = store.write(&[5.0, 6.0]);
        assert_eq!(loc_b.segment, loc_a.segment);
        assert_eq!(store.read(loc_b), vec![5.0, 6.0]);
        fs::remove_dir_all(&store.dir).ok();
    }

    #[test]
    fn stale_directories_are_removed_but_fresh_ones_survive() {
        // `cleanup_stale` treats a missing heartbeat file as immediately stale, so that alone
        // (rather than backdating an mtime) is enough to exercise both branches.
        let base = std::env::temp_dir().join(format!("boothcraft-cleanup-test-{}", uid()));
        fs::create_dir_all(&base).unwrap();
        let fresh = base.join("123");
        let stale = base.join("456");
        fs::create_dir_all(&fresh).unwrap();
        fs::create_dir_all(&stale).unwrap();
        touch_heartbeat(&fresh);

        cleanup_stale(&base);
        assert!(fresh.exists(), "has a fresh heartbeat");
        assert!(!stale.exists(), "no heartbeat file at all");
        fs::remove_dir_all(&base).ok();
    }
}
