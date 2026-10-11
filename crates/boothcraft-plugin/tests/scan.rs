//! Scanning in child processes, against a worker that crashes, hangs and dawdles on command.
#![cfg(feature = "mock")]

use boothcraft_plugin::cache::{BlockReason, Outcome};
use boothcraft_plugin::mock::MockFormat;
use boothcraft_plugin::scan::scan;
use boothcraft_plugin::{PluginFormat, ScanCache, ScanConfig, WorkerCommand};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bc-scan-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(timeout_ms: u64, retry_ms: u64) -> ScanConfig {
    let mut cfg = ScanConfig::new(WorkerCommand {
        program: PathBuf::from(env!("CARGO_BIN_EXE_bc-scan-worker-mock")),
        prefix: Vec::new(),
    });
    cfg.timeout = Duration::from_millis(timeout_ms);
    cfg.retry_timeout = Duration::from_millis(retry_ms);
    cfg
}

fn formats() -> Vec<Box<dyn PluginFormat>> {
    vec![Box::new(MockFormat::new())]
}

fn touch(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, name).unwrap();
    p
}

#[test]
fn crashes_block_immediately_and_good_plugins_survive() {
    let dir = temp("crash");
    for n in [
        "gain.mock",
        "delay64.mock",
        "crash.mock",
        "empty.mock",
        "bad.mock",
    ] {
        touch(&dir, n);
    }
    let mut cache = ScanCache::default();
    let report = scan(
        &formats(),
        &mut cache,
        std::slice::from_ref(&dir),
        &config(10_000, 10_000),
    );

    assert_eq!(report.found, 2);
    assert_eq!(report.blocked, vec![dir.join("crash.mock")]);
    let invalid: Vec<_> = report
        .invalid
        .iter()
        .map(|(p, _)| p.file_name().unwrap())
        .collect();
    assert!(invalid.contains(&"empty.mock".as_ref()) && invalid.contains(&"bad.mock".as_ref()));
    let (path, reason) = cache.blocked().remove(0);
    assert_eq!(path, dir.join("crash.mock"));
    assert!(matches!(reason, BlockReason::Crash(_)));
    let uids: Vec<_> = cache.descriptors().map(|d| d.uid.clone()).collect();
    assert_eq!(uids, vec!["delay:64", "gain"]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_plugin_that_crashes_while_exiting_is_not_blocked() {
    // Many real plugins scan fine and then crash in their shutdown code.
    let dir = temp("exitcrash");
    touch(&dir, "exitcrash.mock");
    touch(&dir, "chatty.mock"); // prints junk to stdout around the reply
    let mut cache = ScanCache::default();
    let report = scan(
        &formats(),
        &mut cache,
        std::slice::from_ref(&dir),
        &config(10_000, 10_000),
    );
    assert!(report.blocked.is_empty(), "blocked: {:?}", report.blocked);
    assert_eq!(report.found, 2);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_slow_plugin_is_retried_alone_and_not_blocked() {
    let dir = temp("slow");
    touch(&dir, "slow.mock"); // takes ~1.5 s to probe
    let mut cache = ScanCache::default();
    // 400 ms is not enough, but the retry gets 10 s.
    let report = scan(
        &formats(),
        &mut cache,
        std::slice::from_ref(&dir),
        &config(400, 10_000),
    );
    assert_eq!(report.found, 1);
    assert!(report.blocked.is_empty(), "a first timeout must not block");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn two_timeouts_block() {
    let dir = temp("hang");
    touch(&dir, "hang.mock");
    let mut cache = ScanCache::default();
    let started = Instant::now();
    let report = scan(
        &formats(),
        &mut cache,
        std::slice::from_ref(&dir),
        &config(300, 600),
    );
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "tried twice"
    );
    assert_eq!(report.blocked, vec![dir.join("hang.mock")]);
    assert_eq!(cache.blocked()[0].1, &BlockReason::Timeout);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unchanged_files_are_not_probed_again_and_blocks_clear_on_change() {
    let dir = temp("cache");
    touch(&dir, "gain.mock");
    let crash = touch(&dir, "crash.mock");
    let mut cache = ScanCache::default();
    let cfg = config(10_000, 10_000);
    let first = scan(&formats(), &mut cache, std::slice::from_ref(&dir), &cfg);
    assert_eq!((first.probed, first.cached), (2, 0));

    let second = scan(&formats(), &mut cache, std::slice::from_ref(&dir), &cfg);
    assert_eq!((second.probed, second.cached), (0, 2), "nothing re-probed");
    assert_eq!(second.blocked, vec![crash.clone()], "the block persists");

    // The cache survives a restart.
    cache.save(&dir).unwrap();
    assert_eq!(ScanCache::load(&dir), cache);

    // Replacing the file (a new version) lifts the block: it is probed again.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&crash, "a different, longer file").unwrap();
    let third = scan(&formats(), &mut cache, std::slice::from_ref(&dir), &cfg);
    assert_eq!(third.probed, 1);

    // Or the user can unblock it explicitly.
    cache.unblock(&crash);
    assert!(cache.blocked().is_empty());
    assert!(cache
        .entries
        .values()
        .any(|e| matches!(e.outcome, Outcome::Ok(_))));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn manager_scans_in_the_background_and_manages_the_results() {
    use boothcraft_plugin::{MainThread, PluginManager};
    let dir = temp("manager");
    touch(&dir, "gain.mock");
    touch(&dir, "delay64.mock");
    let crash = touch(&dir, "crash.mock");

    let mut m = PluginManager::new(
        MainThread::init(),
        formats(),
        None,
        std::sync::Arc::new(|| {}),
    );
    let bundled = m.catalog().len(); // the mock format ships a few plugins of its own
    m.settings.extra_paths = vec![dir.clone()];
    assert!(
        !m.settings.scan_standard_locations,
        "standard folders are off by default"
    );

    m.start_scan(config(10_000, 10_000));
    assert!(m.is_scanning());
    let report = loop {
        if let Some(r) = m.pump().scan_finished {
            break r;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!m.is_scanning());
    assert_eq!(m.scan_progress(), (3, 3), "every file was counted");
    assert_eq!(report.found, 2);
    assert_eq!(report.blocked, vec![crash.clone()]);

    // Scanned plugins join the catalog (the mock's own bundled ones share uids with these).
    assert!(m.catalog().len() >= bundled);
    let id = m
        .catalog()
        .all()
        .find(|d| d.uid == "delay:64")
        .unwrap()
        .effect_id();

    // Switching one off removes it from menus but keeps it listed for switching back on.
    m.set_enabled(&id, false);
    assert!(m.catalog().get(&id).is_none());
    assert!(m.catalog().known().any(|d| d.effect_id() == id));
    m.set_enabled(&id, true);
    assert!(m.catalog().get(&id).is_some());

    // The blocked file is reported, and unblocking lets the next scan try it again.
    assert_eq!(m.blocked().len(), 1);
    m.unblock(&crash);
    assert!(m.blocked().is_empty());
    m.start_scan(config(10_000, 10_000));
    while m.pump().scan_finished.is_none() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        m.scan_progress(),
        (1, 1),
        "only the unblocked file was probed again"
    );
    std::fs::remove_dir_all(dir).ok();
}
