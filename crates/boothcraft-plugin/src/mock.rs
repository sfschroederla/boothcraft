//! A fake plugin format for tests and development: `BOOTHCRAFT_PLUGINS=mock`.
//!
//! Plugins are named by uid:
//! - `gain`: one parameter, "Gain" in dB, as many channels as the host runs.
//! - `gain-stereo`: the same, but stereo only, whatever the host runs.
//! - `delay:N`: delays by N frames and reports it as latency (and tail); parameter "Level".
//! - `relatch`: after loading state, reports a latency change (a re-entrant callback).
//! - `nan`: outputs NaN.
//! - `async`: instantiates later, when [`MockFormat::deliver_deferred`] runs on the main thread.
//!
//! Scan candidates are files with extension `mock` in the extra paths; the file stem scripts what
//! probing does (`crash` aborts, `hang` never returns, `slow` takes a while, `empty` finds
//! nothing, `bad` fails, `gain` and `delay64` describe those plugins).

use crate::error::HostError;
use crate::main_thread::MainThread;
use crate::pending::Pending;
use crate::traits::*;
use boothcraft_dsp::param::db_to_gain;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Counters a test reads to check what the host did.
#[derive(Default)]
pub struct MockStats {
    pub live_instances: AtomicUsize,
    pub instantiations: AtomicUsize,
    pub activations: AtomicUsize,
    pub deactivations: AtomicUsize,
    /// Processors dropped on a thread that never claimed to be the main thread.
    pub off_main_drops: AtomicUsize,
}

#[derive(Clone)]
pub struct MockFormat {
    stats: Arc<MockStats>,
}

impl Default for MockFormat {
    fn default() -> Self {
        MockFormat::new()
    }
}

type Deferred = (
    Sender<Result<Box<dyn PluginInstance>, HostError>>,
    Box<dyn PluginInstance>,
);

thread_local! {
    static DEFERRED: RefCell<Vec<Deferred>> = const { RefCell::new(Vec::new()) };
}

pub fn descriptor(uid: &str) -> PluginDescriptor {
    PluginDescriptor {
        format: "mock".into(),
        uid: uid.into(),
        name: format!("Mock {uid}"),
        vendor: "Boothcraft".into(),
        version: "1.0".into(),
        path: PathBuf::from(format!("{uid}.mock")),
    }
}

impl MockFormat {
    pub fn new() -> MockFormat {
        MockFormat {
            stats: Arc::new(MockStats::default()),
        }
    }

    pub fn stats(&self) -> Arc<MockStats> {
        self.stats.clone()
    }

    /// Finish instantiations of `async` plugins started on this thread.
    pub fn deliver_deferred() {
        let items: Vec<Deferred> = DEFERRED.with(|d| std::mem::take(&mut *d.borrow_mut()));
        for (tx, inst) in items {
            let _ = tx.send(Ok(inst));
        }
    }

    fn make_instance(&self, d: &PluginDescriptor, host: HostContext) -> MockInstance {
        self.stats.instantiations.fetch_add(1, Relaxed);
        self.stats.live_instances.fetch_add(1, Relaxed);
        let (kind, latency) = match d.uid.as_str() {
            "gain-stereo" => (Kind::GainStereo, 0),
            "relatch" => (Kind::Relatch, 0),
            "nan" => (Kind::Nan, 0),
            u if u.starts_with("delay:") => (Kind::Delay, u[6..].parse().unwrap_or(32)),
            _ => (Kind::Gain, 0),
        };
        MockInstance {
            stats: self.stats.clone(),
            host,
            kind,
            level: if kind == Kind::Delay { 1.0 } else { 0.0 },
            latency,
        }
    }
}

impl PluginFormat for MockFormat {
    fn kind(&self) -> &'static str {
        "mock"
    }

    fn discover(&self, extra: &[PathBuf]) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = extra
            .iter()
            .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "mock"))
            .map(|path| Candidate { path, known: None })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    fn needs_probe(&self) -> bool {
        true
    }

    fn probe(&self, c: &Candidate) -> Result<Vec<PluginDescriptor>, HostError> {
        let stem = c.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        match stem {
            "crash" => std::process::abort(),
            "hang" => loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            },
            "empty" => Ok(Vec::new()),
            "bad" => Err(HostError::Load("not a plugin".into())),
            _ => {
                if stem.starts_with("slow") {
                    let ms = std::env::var("BC_MOCK_SLOW_MS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1500);
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
                let uid = match stem.strip_prefix("delay") {
                    Some(n) => format!("delay:{n}"),
                    None => "gain".to_string(),
                };
                let mut d = descriptor(&uid);
                d.name = format!("Mock {stem}");
                d.path = c.path.clone();
                Ok(vec![d])
            }
        }
    }

    fn instantiate(
        &self,
        d: &PluginDescriptor,
        host: HostContext,
        _mt: MainThread,
    ) -> Pending<Box<dyn PluginInstance>> {
        let inst: Box<dyn PluginInstance> = Box::new(self.make_instance(d, host));
        if d.uid == "async" {
            let (tx, pending) = Pending::channel();
            DEFERRED.with(|q| q.borrow_mut().push((tx, inst)));
            pending
        } else {
            Pending::ready(Ok(inst))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gain,
    GainStereo,
    Delay,
    Relatch,
    Nan,
}

struct MockInstance {
    stats: Arc<MockStats>,
    host: HostContext,
    kind: Kind,
    /// Gain in dB, or the delay's level.
    level: f32,
    latency: usize,
}

impl Drop for MockInstance {
    fn drop(&mut self) {
        self.stats.live_instances.fetch_sub(1, Relaxed);
    }
}

const MAGIC: &[u8; 4] = b"MOCK";

impl PluginInstance for MockInstance {
    fn params(&self) -> Vec<ParamInfo> {
        let delay = self.kind == Kind::Delay;
        vec![ParamInfo {
            id: 7,
            name: if delay { "Level" } else { "Gain" }.into(),
            unit: if delay { "" } else { "dB" }.into(),
            min: if delay { 0.0 } else { -60.0 },
            max: if delay { 1.0 } else { 24.0 },
            default: if delay { 1.0 } else { 0.0 },
            current: self.level,
            steps: 0,
            choices: Vec::new(),
            flags: 0,
        }]
    }

    fn set_param(&mut self, _mt: MainThread, id: u32, value: f32) {
        if id == 7 {
            self.level = value;
        }
    }

    fn param_text(&self, _id: u32, value: f32) -> String {
        format!("{value:.1}")
    }

    fn save_state(&mut self, _mt: MainThread, _kind: StateKind) -> Result<Vec<u8>, HostError> {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&self.level.to_le_bytes());
        Ok(v)
    }

    fn load_state(&mut self, _mt: MainThread, state: &[u8]) -> Result<(), HostError> {
        let bytes: [u8; 4] = state
            .strip_prefix(MAGIC)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| HostError::State("not a mock state".into()))?;
        self.level = f32::from_le_bytes(bytes);
        if self.kind == Kind::Relatch {
            // A real plugin may call back into the host from inside `load_state`.
            self.latency = 64;
            self.host.notify(HostEvent::LatencyChanged);
        }
        Ok(())
    }

    fn activate(
        &mut self,
        _mt: MainThread,
        cfg: &ProcessConfig,
    ) -> Result<(Box<dyn PluginProcessor>, Activated), HostError> {
        self.stats.activations.fetch_add(1, Relaxed);
        let ch = if self.kind == Kind::GainStereo {
            2
        } else {
            cfg.channels.clamp(1, 8)
        };
        let a = Activated {
            in_channels: ch,
            out_channels: ch,
            latency: self.latency,
            tail: if self.kind == Kind::Delay {
                self.latency
            } else {
                0
            },
        };
        Ok((
            Box::new(MockProcessor {
                stats: self.stats.clone(),
                kind: self.kind,
                level: self.level,
                line: vec![vec![0.0; self.latency.max(1)]; ch],
                at: 0,
                latency: self.latency,
            }),
            a,
        ))
    }

    fn deactivate(&mut self, _mt: MainThread, processor: Box<dyn PluginProcessor>) {
        self.stats.deactivations.fetch_add(1, Relaxed);
        drop(processor);
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn tail(&self) -> usize {
        if self.kind == Kind::Delay {
            self.latency
        } else {
            0
        }
    }
}

struct MockProcessor {
    stats: Arc<MockStats>,
    kind: Kind,
    level: f32,
    line: Vec<Vec<f32>>,
    at: usize,
    latency: usize,
}

impl Drop for MockProcessor {
    fn drop(&mut self) {
        if !MainThread::is_current() {
            self.stats.off_main_drops.fetch_add(1, Relaxed);
        }
    }
}

impl PluginProcessor for MockProcessor {
    fn process(&mut self, io: AudioIo<'_>, params: &ParamQueue) {
        for (id, v) in params.iter() {
            if id == 7 {
                self.level = v;
            }
        }
        let gain = match self.kind {
            Kind::Delay => self.level,
            _ => db_to_gain(self.level),
        };
        for (c, (inp, out)) in io.inputs.iter().zip(io.outputs.iter_mut()).enumerate() {
            match self.kind {
                Kind::Nan => out.fill(f32::NAN),
                Kind::Delay if self.latency > 0 => {
                    let n = self.line[c].len();
                    for i in 0..io.frames {
                        let slot = (self.at + i) % n;
                        out[i] = self.line[c][slot] * gain;
                        self.line[c][slot] = inp[i];
                    }
                }
                _ => {
                    for i in 0..io.frames {
                        out[i] = inp[i] * gain;
                    }
                }
            }
        }
        self.at = (self.at + io.frames) % self.line[0].len();
    }

    fn reset(&mut self) {
        self.line.iter_mut().for_each(|l| l.fill(0.0));
        self.at = 0;
    }
}
