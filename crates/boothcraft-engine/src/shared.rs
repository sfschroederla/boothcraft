use crate::rackctl::{RackLink, RackMsg};
use arc_swap::ArcSwapOption;
use boothcraft_core::Document;
use boothcraft_dsp::Rack;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

const MAX_METERS: usize = 8;
const NO_SEEK: i64 = -1;
/// Source frames fetched per inner chunk; bounds the scratch buffers.
const CHUNK: usize = 2048;

/// State shared between the UI thread and the audio callback.
pub struct Shared {
    pub doc: ArcSwapOption<Document>,
    position: AtomicU64,
    pending_seek: AtomicI64,
    playing: AtomicBool,
    looping: AtomicBool,
    loop_start: AtomicU64,
    loop_end: AtomicU64,
    peaks: [AtomicU32; MAX_METERS],
    /// Stereo correlation of the last rendered block, as f32 bits; NaN when silent or not playing.
    phase: AtomicU32,
    /// Linear gain applied to playback while the HUD gain knob is being dragged.
    preview_gain: AtomicU32,
    /// Playback stops here when not looping (`u64::MAX` = end of file). Used for selection + post-roll.
    stop_at: AtomicU64,
    skipping: AtomicBool,
    skip_start: AtomicU64,
    skip_end: AtomicU64,
}

impl Shared {
    pub fn new() -> Arc<Shared> {
        Arc::new(Shared {
            doc: ArcSwapOption::empty(),
            position: AtomicU64::new(0f64.to_bits()),
            pending_seek: AtomicI64::new(NO_SEEK),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
            peaks: Default::default(),
            phase: AtomicU32::new(f32::NAN.to_bits()),
            preview_gain: AtomicU32::new(1f32.to_bits()),
            stop_at: AtomicU64::new(u64::MAX),
            skipping: AtomicBool::new(false),
            skip_start: AtomicU64::new(0),
            skip_end: AtomicU64::new(0),
        })
    }

    pub fn set_document(&self, doc: Arc<Document>) {
        self.doc.store(Some(doc));
    }

    pub fn play(&self) {
        self.playing.store(true, Relaxed);
    }

    pub fn pause(&self) {
        self.playing.store(false, Relaxed);
    }

    pub fn stop(&self) {
        self.playing.store(false, Relaxed);
        self.seek(0);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Relaxed)
    }

    pub fn seek(&self, frame: usize) {
        self.pending_seek.store(frame as i64, Relaxed);
        // Reflect immediately for UI reads while paused (the callback may not run).
        self.position.store((frame as f64).to_bits(), Relaxed);
    }

    pub fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Relaxed))
    }

    /// Loop over `[start, end)`; `end == 0` or `end <= start` means the whole file.
    pub fn set_loop(&self, enabled: bool, start: usize, end: usize) {
        self.loop_start.store(start as u64, Relaxed);
        self.loop_end.store(end as u64, Relaxed);
        self.looping.store(enabled, Relaxed);
    }

    /// Stop playback at `frame` (when not looping), or play to the end of the file with `None`.
    pub fn set_stop_at(&self, frame: Option<usize>) {
        self.stop_at
            .store(frame.map_or(u64::MAX, |f| f as u64), Relaxed);
    }

    /// Jump over `[start, end)` during playback ("Skip Selection"). Ignored when the range is empty.
    pub fn set_skip(&self, enabled: bool, start: usize, end: usize) {
        self.skip_start.store(start as u64, Relaxed);
        self.skip_end.store(end as u64, Relaxed);
        self.skipping.store(enabled && end > start, Relaxed);
    }

    /// Scale playback by `gain` (linear) until set back to 1.0.
    pub fn set_preview_gain(&self, gain: f32) {
        self.preview_gain.store(gain.to_bits(), Relaxed);
    }

    /// Phase correlation between the first two output channels, from -1 (out of phase) to +1
    /// (identical), or `None` when nothing is playing.
    pub fn phase_correlation(&self) -> Option<f32> {
        let v = f32::from_bits(self.phase.load(Relaxed));
        (!v.is_nan()).then_some(v)
    }

    /// Peak since the last call, per output channel, resetting the meters.
    pub fn take_peaks(&self, out: &mut [f32]) {
        for (o, p) in out.iter_mut().zip(&self.peaks) {
            *o = f32::from_bits(p.swap(0, Relaxed));
        }
    }
}

/// Fills output buffers from the shared state. Owned by the audio callback.
pub struct Renderer {
    shared: Arc<Shared>,
    scratch: Vec<Vec<f32>>,
    link: Option<RackLink>,
    rack: Option<Box<Rack>>,
    rack_power: bool,
    /// Copy of everything played, for loudness metering on the UI thread.
    tap: Option<rtrb::Producer<f32>>,
}

impl Renderer {
    pub fn new(shared: Arc<Shared>) -> Renderer {
        Renderer {
            shared,
            scratch: Vec::new(),
            link: None,
            rack: None,
            rack_power: false,
            tap: None,
        }
    }

    /// Render interleaved `out` (`out_channels` wide) at `out_rate`. Allocation only happens the
    /// first time a document with more channels than before is seen.
    /// Send a copy of the rendered output (interleaved) into `tap`. Blocks that do not fit are dropped.
    pub fn attach_tap(&mut self, tap: rtrb::Producer<f32>) {
        self.tap = Some(tap);
    }

    /// Accept preview-rack messages from the UI (see [`crate::rack_channel`]).
    pub fn attach_rack(&mut self, link: RackLink) {
        self.link = Some(link);
    }

    fn poll_rack(&mut self) {
        let Some(link) = &mut self.link else {
            return;
        };
        while let Ok(msg) = link.rx.pop() {
            let retired = match msg {
                RackMsg::Install(rack) => self.rack.replace(rack),
                RackMsg::Clear => self.rack.take(),
                RackMsg::Param { slot, index, value } => {
                    if let Some(r) = &mut self.rack {
                        r.set_param(slot as usize, index as usize, value);
                    }
                    None
                }
                RackMsg::Bypass { slot, on } => {
                    if let Some(r) = &mut self.rack {
                        r.set_bypass(slot as usize, on);
                    }
                    None
                }
                RackMsg::Gains {
                    input_db,
                    output_db,
                    mix,
                } => {
                    if let Some(r) = &mut self.rack {
                        r.set_gains(input_db, output_db, mix);
                    }
                    None
                }
                RackMsg::Power(on) => {
                    self.rack_power = on;
                    None
                }
            };
            if let Some(old) = retired {
                // Hand the old rack back for the UI thread to free. If that queue is somehow full,
                // leak it rather than deallocate on the audio thread.
                if let Err(rtrb::PushError::Full(old)) = link.trash.push(old) {
                    std::mem::forget(old);
                }
            }
        }
    }

    pub fn render(&mut self, out: &mut [f32], out_channels: usize, out_rate: u32) {
        self.poll_rack();
        out.fill(0.0);
        let s = &*self.shared;
        s.phase.store(f32::NAN.to_bits(), Relaxed);
        let seek = s.pending_seek.swap(NO_SEEK, Relaxed);
        let mut pos = if seek >= 0 { seek as f64 } else { s.position() };
        let guard = s.doc.load();
        let (Some(doc), true) = (guard.as_ref(), s.playing.load(Relaxed)) else {
            if seek >= 0 {
                s.position.store(pos.to_bits(), Relaxed);
            }
            return;
        };
        let doc_ch = doc.channel_count();
        while self.scratch.len() < doc_ch {
            self.scratch.push(vec![0.0; CHUNK + 4]);
        }
        let frames = doc.frames();
        let step = doc.sample_rate as f64 / out_rate as f64;
        let gain = f32::from_bits(s.preview_gain.load(Relaxed));
        let looping = s.looping.load(Relaxed);
        let (mut ls, mut le) = (
            s.loop_start.load(Relaxed) as usize,
            s.loop_end.load(Relaxed) as usize,
        );
        if le <= ls || le > frames {
            ls = 0;
            le = frames;
        }
        let stop_at = s.stop_at.load(Relaxed).min(frames as u64) as usize;
        let end = if looping { le } else { stop_at } as f64;
        let skipping = s.skipping.load(Relaxed);
        let (ss, se) = (
            s.skip_start.load(Relaxed) as f64,
            s.skip_end.load(Relaxed) as f64,
        );
        let mut peaks = [0f32; MAX_METERS];
        let total_out = out.len() / out_channels;
        let mut produced = 0;
        let mut ended = false;
        while produced < total_out {
            if pos >= end {
                if looping && le > ls {
                    pos = ls as f64 + (pos - end);
                } else {
                    ended = true;
                    break;
                }
            }
            if skipping && pos >= ss && pos < se {
                pos = se;
                continue;
            }
            // Never run across the start of a skipped range.
            let limit = if skipping && pos < ss {
                ss.min(end)
            } else {
                end
            };
            // Fetch the source window covering the next run of output frames.
            let base = pos.floor() as usize;
            let run = (((CHUNK as f64 - 2.0) / step) as usize)
                .min(total_out - produced)
                .max(1);
            let need = ((run as f64 * step).ceil() as usize + 2).min(CHUNK + 4);
            for (c, buf) in self.scratch.iter_mut().enumerate().take(doc_ch) {
                doc.channels[c].read(base, &mut buf[..need]);
            }
            let mut done = 0;
            while done < run {
                if pos >= limit {
                    break;
                }
                let rel = pos - base as f64;
                let i = rel.floor() as usize;
                let frac = (rel - i as f64) as f32;
                for oc in 0..out_channels {
                    let sc = if doc_ch == 1 {
                        Some(0)
                    } else if oc < doc_ch {
                        Some(oc)
                    } else {
                        None
                    };
                    if let Some(sc) = sc {
                        let b = &self.scratch[sc];
                        let v = (b[i] + (b[i + 1] - b[i]) * frac) * gain;
                        out[(produced + done) * out_channels + oc] = v;
                    }
                }
                pos += step;
                done += 1;
            }
            produced += done;
            if done == 0 {
                break;
            }
        }
        let written = produced * out_channels;
        if self.rack_power {
            if let Some(rack) = self.rack.as_mut().filter(|r| r.channels() == out_channels) {
                rack.process_interleaved(&mut out[..written]);
            }
        }
        if let Some(tap) = &mut self.tap {
            if let Ok(mut chunk) = tap.write_chunk(written) {
                let (a, b) = chunk.as_mut_slices();
                let split = a.len();
                a.copy_from_slice(&out[..split]);
                b.copy_from_slice(&out[split..written]);
                chunk.commit_all();
            }
        }
        for frame in out[..written].chunks_exact(out_channels) {
            for (p, v) in peaks.iter_mut().zip(frame) {
                *p = p.max(v.abs());
            }
        }
        if out_channels >= 2 {
            let (mut ll, mut rr, mut lr) = (0f64, 0f64, 0f64);
            for frame in out[..produced * out_channels].chunks_exact(out_channels) {
                let (l, r) = (frame[0] as f64, frame[1] as f64);
                ll += l * l;
                rr += r * r;
                lr += l * r;
            }
            if ll > 1e-12 && rr > 1e-12 {
                s.phase
                    .store(((lr / (ll * rr).sqrt()) as f32).to_bits(), Relaxed);
            }
        }
        for (p, v) in s.peaks.iter().zip(peaks) {
            let prev = f32::from_bits(p.load(Relaxed));
            p.store(prev.max(v).to_bits(), Relaxed);
        }
        if ended {
            s.playing.store(false, Relaxed);
            pos = end;
        } else if looping && pos >= end && le > ls {
            pos = ls as f64 + (pos - end);
        }
        s.position.store(pos.to_bits(), Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boothcraft_core::Rope;

    fn ramp_doc(frames: usize, rate: u32) -> Arc<Document> {
        let v: Vec<f32> = (0..frames).map(|i| i as f32 / frames as f32).collect();
        Arc::new(Document::new(rate, vec![Rope::from_samples(&v)]))
    }

    #[test]
    fn plays_then_stops_at_end() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 600 * 2];
        r.render(&mut out, 2, 48_000);
        assert!(sh.is_playing());
        assert_eq!(
            out[2 * 100],
            out[2 * 100 + 1],
            "mono duplicated to both channels"
        );
        assert!((out[2 * 100] - 0.1).abs() < 1e-6);
        r.render(&mut out, 2, 48_000);
        assert!(!sh.is_playing());
        assert_eq!(sh.position(), 1000.0);
        assert!(out[2 * 450..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn loop_wraps_and_seek_works() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.set_loop(true, 200, 400);
        sh.seek(300);
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 300];
        r.render(&mut out, 1, 48_000);
        assert!(sh.is_playing());
        assert!((out[0] - 0.3).abs() < 1e-6);
        assert!(
            (out[100] - 0.2).abs() < 1e-6,
            "wrapped to loop start, got {}",
            out[100]
        );
        assert!(sh.position() >= 200.0 && sh.position() < 400.0);
    }

    #[test]
    fn resamples_by_rate_ratio() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(48_000, 48_000));
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 4410];
        r.render(&mut out, 1, 44_100);
        let expect = 4410.0 * (48_000.0 / 44_100.0);
        assert!(
            (sh.position() - expect).abs() < 2.0,
            "position {}",
            sh.position()
        );
    }

    #[test]
    fn phase_correlation_tracks_polarity() {
        let rate = 48_000;
        let sine: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let inverted: Vec<f32> = sine.iter().map(|x| -x).collect();
        let play = |chans: Vec<Vec<f32>>| {
            let sh = Shared::new();
            let ropes = chans.iter().map(|c| Rope::from_samples(c)).collect();
            sh.set_document(Arc::new(Document::new(rate, ropes)));
            assert_eq!(sh.phase_correlation(), None, "no reading before playing");
            sh.play();
            let mut r = Renderer::new(sh.clone());
            let mut out = vec![0.0; 1000 * 2];
            r.render(&mut out, 2, rate);
            sh.phase_correlation().unwrap()
        };
        assert!((play(vec![sine.clone(), sine.clone()]) - 1.0).abs() < 1e-4);
        assert!((play(vec![sine.clone(), inverted]) + 1.0).abs() < 1e-4);
        assert!(
            (play(vec![sine]) - 1.0).abs() < 1e-4,
            "mono plays to both sides"
        );
    }

    #[test]
    fn preview_gain_scales_output() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.set_preview_gain(0.5);
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 500];
        r.render(&mut out, 1, 48_000);
        assert!(
            (out[200] - 0.1).abs() < 1e-6,
            "0.2 halved, got {}",
            out[200]
        );
    }

    #[test]
    fn skip_range_is_jumped_over() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.set_skip(true, 200, 400);
        sh.seek(100);
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 300];
        r.render(&mut out, 1, 48_000);
        assert!((out[99] - 0.199).abs() < 1e-6);
        assert!(
            (out[100] - 0.4).abs() < 1e-6,
            "jumped to 400, got {}",
            out[100]
        );
        assert_eq!(sh.position(), 600.0);
    }

    #[test]
    fn stop_at_ends_playback_early() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.set_stop_at(Some(300));
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 500];
        r.render(&mut out, 1, 48_000);
        assert!(!sh.is_playing());
        assert_eq!(sh.position(), 300.0);
        assert!(out[300..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn preview_rack_processes_output_and_old_racks_come_back() {
        use boothcraft_dsp::RackDesc;
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.play();
        let (mut ctl, link) = crate::rack_channel(48_000, 1);
        let mut r = Renderer::new(sh.clone());
        r.attach_rack(link);
        let mut desc = RackDesc::single("amplify").unwrap();
        desc.slots[0].as_mut().unwrap().values[0] = 20.0 * 2f32.log10();
        ctl.send(RackMsg::Install(Box::new(Rack::new(
            &desc, 48_000.0, 1, 256,
        ))));
        ctl.send(RackMsg::Power(true));
        let mut out = vec![0.0; 300];
        r.render(&mut out, 1, 48_000);
        assert!((out[200] - 0.4).abs() < 1e-5, "doubled, got {}", out[200]);
        ctl.send(RackMsg::Power(false));
        r.render(&mut out, 1, 48_000);
        assert!(
            (out[0] - 0.3).abs() < 1e-5,
            "bypassed when off, got {}",
            out[0]
        );
        ctl.send(RackMsg::Install(Box::new(Rack::new(
            &RackDesc::default(),
            48_000.0,
            1,
            256,
        ))));
        r.render(&mut out, 1, 48_000);
        assert!(
            ctl.trash.pop().is_ok(),
            "replaced rack handed back to the UI thread"
        );
    }

    #[test]
    fn tap_receives_what_was_played() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.play();
        let (tx, mut rx) = rtrb::RingBuffer::new(4096);
        let mut r = Renderer::new(sh.clone());
        r.attach_tap(tx);
        let mut out = vec![0.0; 2 * 300];
        r.render(&mut out, 2, 48_000);
        let got: Vec<f32> = std::iter::from_fn(|| rx.pop().ok()).collect();
        assert_eq!(got, out);
    }

    #[test]
    fn meters_report_and_reset() {
        let sh = Shared::new();
        sh.set_document(ramp_doc(1000, 48_000));
        sh.play();
        let mut r = Renderer::new(sh.clone());
        let mut out = vec![0.0; 1000];
        r.render(&mut out, 1, 48_000);
        let mut p = [0.0; 2];
        sh.take_peaks(&mut p);
        assert!(p[0] > 0.9);
        sh.take_peaks(&mut p);
        assert_eq!(p[0], 0.0);
    }
}
