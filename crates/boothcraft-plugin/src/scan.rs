//! Plugin scanning, always in child processes.
//!
//! Loading a plugin to read its description can crash or hang, so the app never does it itself.
//! It runs `current_exe --plugin-scan-worker <format> <path>` for each file, up to four at a time,
//! with a timeout. A crash blocks the file at once; a timeout is retried alone with a longer
//! limit and blocks only if that times out too. Results go to the [`ScanCache`].

use crate::cache::{BlockReason, Outcome, ScanCache};
use crate::error::HostError;
use crate::traits::{Candidate, PluginDescriptor, PluginFormat};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const WORKER_FLAG: &str = "--plugin-scan-worker";

/// How to start a scan worker.
#[derive(Clone, Debug)]
pub struct WorkerCommand {
    pub program: PathBuf,
    /// Arguments placed before the worker flag (none for the real executable).
    pub prefix: Vec<OsString>,
}

impl WorkerCommand {
    /// This same executable, in its hidden worker mode.
    pub fn current_exe() -> std::io::Result<WorkerCommand> {
        Ok(WorkerCommand {
            program: std::env::current_exe()?,
            prefix: Vec::new(),
        })
    }

    fn spawn(&self, format: &str, path: &Path) -> std::io::Result<Child> {
        Command::new(&self.program)
            .args(&self.prefix)
            .arg(WORKER_FLAG)
            .arg(format)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }
}

/// How far a scan has got, readable from the UI while it runs.
#[derive(Debug, Default)]
pub struct ScanProgress {
    /// Plugin files that need probing in this scan.
    pub total: AtomicUsize,
    /// Of those, how many have finished.
    pub done: AtomicUsize,
}

impl ScanProgress {
    pub fn get(&self) -> (usize, usize) {
        (self.done.load(Relaxed), self.total.load(Relaxed))
    }
}

#[derive(Clone, Debug)]
pub struct ScanConfig {
    pub worker: WorkerCommand,
    pub timeout: Duration,
    pub retry_timeout: Duration,
    pub parallel: usize,
    /// Also search each format's standard install locations (the default). Off, only the extra
    /// folders given are searched.
    pub standard_locations: bool,
    pub progress: Arc<ScanProgress>,
}

impl ScanConfig {
    pub fn new(worker: WorkerCommand) -> ScanConfig {
        ScanConfig {
            worker,
            timeout: Duration::from_secs(30),
            retry_timeout: Duration::from_secs(120),
            parallel: 4,
            standard_locations: true,
            progress: Arc::new(ScanProgress::default()),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub probed: usize,
    pub cached: usize,
    pub found: usize,
    pub blocked: Vec<PathBuf>,
    pub invalid: Vec<(PathBuf, String)>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum WorkerReply {
    Ok { descriptors: Vec<PluginDescriptor> },
    Error { message: String },
}

/// Hidden mode of the app and CLI executables: `--plugin-scan-worker <format> <path>`. Call this
/// first in `main`, before argument parsing. In worker mode it probes one file, prints the result
/// as JSON and exits; otherwise it returns.
pub fn maybe_run_scan_worker(formats: &[Box<dyn PluginFormat>]) {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).map(|a| a == WORKER_FLAG) != Some(true) {
        return;
    }
    let reply = match (args.get(2), args.get(3)) {
        (Some(kind), Some(path)) => {
            let kind = kind.to_string_lossy();
            match formats.iter().find(|f| f.kind() == kind) {
                Some(f) => {
                    f.init_scan_worker();
                    let cand = Candidate {
                        path: PathBuf::from(path),
                        known: None,
                    };
                    match f.probe(&cand) {
                        Ok(descriptors) => WorkerReply::Ok { descriptors },
                        Err(e) => WorkerReply::Error {
                            message: e.to_string(),
                        },
                    }
                }
                None => WorkerReply::Error {
                    message: format!("no plugin format {kind:?}"),
                },
            }
        }
        _ => WorkerReply::Error {
            message: "usage: --plugin-scan-worker <format> <path>".into(),
        },
    };
    println!("{}", serde_json::to_string(&reply).unwrap_or_default());
    std::process::exit(0);
}

enum Probe {
    Done(Vec<PluginDescriptor>),
    Invalid(String),
    Crashed(String),
    TimedOut,
}

struct Running {
    index: usize,
    child: Child,
    reader: JoinHandle<Vec<u8>>,
    started: Instant,
}

fn start(cfg: &ScanConfig, format: &str, index: usize, path: &Path) -> Result<Running, Probe> {
    let mut child = cfg
        .worker
        .spawn(format, path)
        .map_err(|e| Probe::Crashed(format!("could not start scan worker: {e}")))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    // Read on a thread so a chatty worker can never fill the pipe and stall.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    Ok(Running {
        index,
        child,
        reader,
        started: Instant::now(),
    })
}

fn finish(run: Running, status: std::process::ExitStatus) -> Probe {
    let out = run.reader.join().unwrap_or_default();
    if !status.success() {
        return Probe::Crashed(format!("scan worker exited with {status}"));
    }
    match serde_json::from_slice::<WorkerReply>(&out) {
        Ok(WorkerReply::Ok { descriptors }) => Probe::Done(descriptors),
        Ok(WorkerReply::Error { message }) => Probe::Invalid(message),
        Err(e) => Probe::Invalid(format!("unreadable scan worker reply: {e}")),
    }
}

/// Probe `jobs` (index, path) with up to `parallel` workers at once and `timeout` each.
fn run_pool(
    cfg: &ScanConfig,
    format: &str,
    jobs: Vec<(usize, PathBuf)>,
    parallel: usize,
    timeout: Duration,
    count_progress: bool,
) -> Vec<(usize, Probe)> {
    let mut queue: VecDeque<_> = jobs.into();
    let mut running: Vec<Running> = Vec::new();
    let mut results = Vec::new();
    while !queue.is_empty() || !running.is_empty() {
        while running.len() < parallel.max(1) {
            let Some((index, path)) = queue.pop_front() else {
                break;
            };
            match start(cfg, format, index, &path) {
                Ok(r) => running.push(r),
                Err(p) => {
                    results.push((index, p));
                    if count_progress {
                        cfg.progress.done.fetch_add(1, Relaxed);
                    }
                }
            }
        }
        let mut i = 0;
        while i < running.len() {
            match running[i].child.try_wait() {
                Ok(Some(status)) => {
                    let run = running.swap_remove(i);
                    let index = run.index;
                    results.push((index, finish(run, status)));
                    if count_progress {
                        cfg.progress.done.fetch_add(1, Relaxed);
                    }
                }
                Ok(None) if running[i].started.elapsed() > timeout => {
                    let mut run = running.swap_remove(i);
                    let _ = run.child.kill();
                    let _ = run.child.wait();
                    let _ = run.reader.join();
                    results.push((run.index, Probe::TimedOut));
                    // A timeout is retried, so it only counts once that retry has finished.
                }
                Ok(None) => i += 1,
                Err(e) => {
                    let run = running.swap_remove(i);
                    results.push((run.index, Probe::Crashed(format!("wait failed: {e}"))));
                    if count_progress {
                        cfg.progress.done.fetch_add(1, Relaxed);
                    }
                }
            }
        }
        if !running.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    results
}

/// Scan every format, updating `cache`. Files unchanged since their cached result are skipped.
pub fn scan(
    formats: &[Box<dyn PluginFormat>],
    cache: &mut ScanCache,
    extra: &[PathBuf],
    cfg: &ScanConfig,
) -> ScanReport {
    cfg.progress.total.store(0, Relaxed);
    cfg.progress.done.store(0, Relaxed);
    let mut report = ScanReport::default();
    for format in formats {
        let kind = format.kind();
        let mut todo: Vec<Candidate> = Vec::new();
        for cand in format.discover(extra, cfg.standard_locations) {
            if !format.needs_probe() {
                // Nothing to load: the candidate already says what it holds.
                if let Some(known) = cand.known {
                    report.found += known.len();
                    cache.record(kind, &cand.path, Outcome::Ok(known));
                }
                continue;
            }
            match cache.fresh(kind, &cand.path) {
                Some(e) => {
                    report.cached += 1;
                    match &e.outcome {
                        Outcome::Ok(d) => report.found += d.len(),
                        Outcome::Blocked(_) => report.blocked.push(cand.path.clone()),
                        Outcome::Invalid(_) => {}
                    }
                }
                None => todo.push(cand),
            }
        }
        let jobs: Vec<(usize, PathBuf)> = todo
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.path.clone()))
            .collect();
        report.probed += jobs.len();
        cfg.progress.total.fetch_add(jobs.len(), Relaxed);
        let mut results = run_pool(cfg, kind, jobs, cfg.parallel, cfg.timeout, true);

        // Timeouts may just mean a busy machine: retry them one at a time, with longer to finish.
        let retry: Vec<(usize, PathBuf)> = results
            .iter()
            .filter(|(_, p)| matches!(p, Probe::TimedOut))
            .map(|(i, _)| (*i, todo[*i].path.clone()))
            .collect();
        if !retry.is_empty() {
            results.retain(|(_, p)| !matches!(p, Probe::TimedOut));
            let retried = run_pool(cfg, kind, retry, 1, cfg.retry_timeout, false);
            cfg.progress.done.fetch_add(retried.len(), Relaxed);
            results.extend(retried);
        }

        for (i, probe) in results {
            let path = &todo[i].path;
            let outcome = match probe {
                Probe::Done(d) if d.is_empty() => Outcome::Invalid("no effects".into()),
                Probe::Done(d) => {
                    report.found += d.len();
                    Outcome::Ok(d)
                }
                Probe::Invalid(why) => Outcome::Invalid(why),
                Probe::Crashed(why) => Outcome::Blocked(BlockReason::Crash(why)),
                Probe::TimedOut => Outcome::Blocked(BlockReason::Timeout),
            };
            match &outcome {
                Outcome::Blocked(_) => report.blocked.push(path.clone()),
                Outcome::Invalid(why) => report.invalid.push((path.clone(), why.clone())),
                Outcome::Ok(_) => {}
            }
            cache.record(kind, path, outcome);
        }
    }
    report
}

impl From<serde_json::Error> for HostError {
    fn from(e: serde_json::Error) -> Self {
        HostError::Other(e.to_string())
    }
}
