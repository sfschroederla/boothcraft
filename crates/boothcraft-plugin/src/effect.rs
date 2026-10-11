//! A plugin as a rack effect.

use crate::traits::{Activated, AudioIo, ParamInfo, ParamQueue, PluginProcessor};
use boothcraft_dsp::param::{intern, intern_list, Curve, ParamSpec, Unit};
use boothcraft_dsp::{Effect, SlotId};
use std::any::Any;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

/// Status the audio thread reports to the UI.
pub const STATUS_BAD_OUTPUT: u32 = 1;

pub type Status = Arc<AtomicU32>;

/// The processor handed back through [`Effect::retire`].
pub struct RetiredProcessor(pub Box<dyn PluginProcessor>);

const MAX_CH: usize = 8;

pub struct PluginEffect {
    slot: SlotId,
    processor: Option<Box<dyn PluginProcessor>>,
    specs: Vec<ParamSpec>,
    ids: Vec<u32>,
    values: Vec<f32>,
    queue: ParamQueue,
    latency: usize,
    tail: usize,
    in_ch: usize,
    out_ch: usize,
    max_block: usize,
    in_buf: Vec<Vec<f32>>,
    out_buf: Vec<Vec<f32>>,
    status: Status,
}

/// Build the spec list for a plugin's parameters, interning strings so memory is bounded by the
/// number of distinct names however often plugins are instantiated.
pub fn specs_from(params: &[ParamInfo]) -> (Vec<ParamSpec>, Vec<u32>) {
    let specs = params
        .iter()
        .map(|p| {
            let unit = if p.unit.is_empty() {
                Unit::None
            } else {
                Unit::Custom(intern(&format!(" {}", p.unit)))
            };
            let mut s = ParamSpec::new(
                intern(&p.id.to_string()),
                intern(&p.name),
                unit,
                p.min,
                p.max,
                p.default,
            );
            s.curve = Curve::Linear;
            s.steps = p.steps;
            s.flags = p.flags;
            if !p.choices.is_empty() {
                s.choices = intern_list(&p.choices);
            }
            s
        })
        .collect();
    (specs, params.iter().map(|p| p.id).collect())
}

impl PluginEffect {
    pub fn new(
        slot: SlotId,
        processor: Box<dyn PluginProcessor>,
        params: &[ParamInfo],
        activated: Activated,
        max_block: usize,
        status: Status,
    ) -> PluginEffect {
        let (specs, ids) = specs_from(params);
        let ch = |n: usize| vec![vec![0.0; max_block]; n.clamp(1, MAX_CH)];
        PluginEffect {
            slot,
            processor: Some(processor),
            specs,
            ids,
            values: params.iter().map(|p| p.current).collect(),
            queue: ParamQueue::new(),
            latency: activated.latency,
            tail: activated.tail,
            in_ch: activated.in_channels.clamp(1, MAX_CH),
            out_ch: activated.out_channels.clamp(1, MAX_CH),
            max_block,
            in_buf: ch(activated.in_channels),
            out_buf: ch(activated.out_channels),
            status,
        }
    }

    /// Queue a change by the format's own parameter id, for the next block.
    pub fn queue_by_id(&mut self, id: u32, value: f32) {
        self.queue.push(id, value);
    }

    pub fn set_latency(&mut self, frames: usize) {
        self.latency = frames;
    }

    pub fn slot(&self) -> SlotId {
        self.slot
    }

    /// Run one block through the plugin with the channel adapter.
    fn run_block(&mut self, buf: &mut [&mut [f32]], at: usize, len: usize) {
        let host = buf.len().min(MAX_CH);
        // Plugin inputs: host channels in order; a plugin with more inputs than the host has
        // (mono into stereo-only) gets the last host channel repeated.
        for c in 0..self.in_ch {
            let src = &buf[c.min(host - 1)][at..at + len];
            self.in_buf[c][..len].copy_from_slice(src);
        }
        let Some(proc) = self.processor.as_mut() else {
            return;
        };
        {
            let mut ins: [&[f32]; MAX_CH] = [&[]; MAX_CH];
            for (d, s) in ins.iter_mut().zip(&self.in_buf).take(self.in_ch) {
                *d = &s[..len];
            }
            let mut outs: [&mut [f32]; MAX_CH] = Default::default();
            for (d, s) in outs
                .iter_mut()
                .zip(self.out_buf.iter_mut())
                .take(self.out_ch)
            {
                *d = &mut s[..len];
            }
            proc.process(
                AudioIo {
                    inputs: &ins[..self.in_ch],
                    outputs: &mut outs[..self.out_ch],
                    frames: len,
                },
                &self.queue,
            );
        }
        self.queue.clear();
        // A plugin that produces NaN or infinity must not poison the rest of the chain.
        let bad = self.out_buf[..self.out_ch]
            .iter()
            .any(|ch| ch[..len].iter().any(|x| !x.is_finite()));
        if bad {
            self.status.fetch_or(STATUS_BAD_OUTPUT, Relaxed);
            return; // leave the block dry
        }
        if host == 1 && self.out_ch >= 2 {
            // Mono host through a stereo plugin: fold the outputs back down.
            for i in 0..len {
                let sum: f32 = (0..self.out_ch).map(|c| self.out_buf[c][i]).sum();
                buf[0][at + i] = sum / self.out_ch as f32;
            }
        } else {
            // Channels beyond the plugin's outputs stay dry.
            for (c, ch) in buf.iter_mut().enumerate().take(host.min(self.out_ch)) {
                ch[at..at + len].copy_from_slice(&self.out_buf[c][..len]);
            }
        }
    }
}

impl Effect for PluginEffect {
    fn specs(&self) -> &[ParamSpec] {
        &self.specs
    }

    fn values(&self) -> &[f32] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [f32] {
        &mut self.values
    }

    fn prepare(&mut self, _: f32, _: usize, _: usize) {
        // Already set up by the host when the processor was activated.
    }

    fn reset(&mut self) {
        if let Some(p) = self.processor.as_mut() {
            p.reset();
        }
    }

    fn process(&mut self, buf: &mut [&mut [f32]]) {
        let Some(frames) = buf.first().map(|c| c.len()) else {
            return;
        };
        if self.processor.is_none() {
            return; // not ready: audio passes through
        }
        let mut at = 0;
        while at < frames {
            let len = (frames - at).min(self.max_block);
            self.run_block(buf, at, len);
            at += len;
        }
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn tail(&self) -> usize {
        self.tail
    }

    /// Queue the change for the processor. (The plugin's controller side is updated by the
    /// `PluginManager` on the main thread.)
    fn set_param(&mut self, index: usize, value: f32) {
        if let (Some(spec), Some(&id)) = (self.specs.get(index), self.ids.get(index)) {
            let v = spec.clamp(value);
            self.values[index] = v;
            self.queue.push(id, v);
        }
    }

    fn set_all(&mut self, values: &[f32]) {
        for (i, v) in values.iter().enumerate().take(self.specs.len()) {
            self.set_param(i, *v);
        }
    }

    fn retire(&mut self) -> Option<(SlotId, Box<dyn Any + Send>)> {
        let p = self.processor.take()?;
        Some((self.slot, Box::new(RetiredProcessor(p))))
    }
}
