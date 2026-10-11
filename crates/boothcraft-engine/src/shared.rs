use arc_swap::ArcSwapOption;
use boothcraft_core::Document;
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
}

impl Renderer {
    pub fn new(shared: Arc<Shared>) -> Renderer {
        Renderer {
            shared,
            scratch: Vec::new(),
        }
    }

    /// Render interleaved `out` (`out_channels` wide) at `out_rate`. Allocation only happens the
    /// first time a document with more channels than before is seen.
    pub fn render(&mut self, out: &mut [f32], out_channels: usize, out_rate: u32) {
        out.fill(0.0);
        let s = &*self.shared;
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
        let looping = s.looping.load(Relaxed);
        let (mut ls, mut le) = (
            s.loop_start.load(Relaxed) as usize,
            s.loop_end.load(Relaxed) as usize,
        );
        if le <= ls || le > frames {
            ls = 0;
            le = frames;
        }
        let end = if looping { le } else { frames } as f64;
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
                if pos >= end {
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
                        let v = b[i] + (b[i + 1] - b[i]) * frac;
                        out[(produced + done) * out_channels + oc] = v;
                        if oc < MAX_METERS {
                            peaks[oc] = peaks[oc].max(v.abs());
                        }
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
