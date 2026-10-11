//! The audio-thread half of an activated plugin.

#![allow(unsafe_code)]

use crate::params::Changes;
use boothcraft_plugin::{AudioIo, ParamQueue, PluginProcessor};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_ as Ctx;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

/// Most audio buses a plugin may declare (inputs or outputs); beyond that it is refused.
pub const MAX_BUSES: usize = 8;
const MAX_CH: usize = 8;

pub struct Vst3Processor {
    pub(crate) audio: ComPtr<IAudioProcessor>,
    /// Keeps the component alive for as long as the processor can run.
    pub(crate) _component: ComPtr<IComponent>,
    pub(crate) changes_in: ComWrapper<Changes>,
    pub(crate) changes_in_ptr: ComPtr<IParameterChanges>,
    pub(crate) changes_out: ComWrapper<Changes>,
    pub(crate) changes_out_ptr: ComPtr<IParameterChanges>,
    /// Parameter id → factor from our plain value to the normalized one (1 / steps, or 1).
    pub(crate) scale: Vec<(u32, f64)>,
    pub(crate) mode: i32,
    pub(crate) in_buses: usize,
    pub(crate) out_buses: usize,
    pub(crate) context: ProcessContext,
}

// SAFETY: a VST3 plugin's `process` runs on whichever thread the host chooses, one at a time and
// never concurrently with itself; the host is the only holder of this object, and it is only
// ever used from one thread at a time (the audio thread once installed in a rack). The COM
// objects inside use `Arc` reference counts, which are atomic.
unsafe impl Send for Vst3Processor {}

impl PluginProcessor for Vst3Processor {
    fn process(&mut self, io: AudioIo<'_>, params: &ParamQueue) {
        if io.frames == 0 || io.inputs.len() > MAX_CH || io.outputs.len() > MAX_CH {
            return;
        }
        self.changes_in.clear();
        self.changes_out.clear();
        for (id, v) in params.iter() {
            let k = self
                .scale
                .iter()
                .find(|(i, _)| *i == id)
                .map_or(1.0, |(_, k)| *k);
            self.changes_in.push(id, (v as f64 * k).clamp(0.0, 1.0));
        }

        let mut in_ptrs: [*mut f32; MAX_CH] = [std::ptr::null_mut(); MAX_CH];
        let mut out_ptrs: [*mut f32; MAX_CH] = [std::ptr::null_mut(); MAX_CH];
        for (p, c) in in_ptrs.iter_mut().zip(io.inputs) {
            // Inputs are read-only by contract; the API just types the pointer as mutable.
            *p = c.as_ptr() as *mut f32;
        }
        for (p, c) in out_ptrs.iter_mut().zip(io.outputs.iter_mut()) {
            *p = c.as_mut_ptr();
        }
        let empty = AudioBusBuffers {
            numChannels: 0,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: std::ptr::null_mut(),
            },
        };
        let mut ins = [empty; MAX_BUSES];
        let mut outs = [empty; MAX_BUSES];
        ins[0] = AudioBusBuffers {
            numChannels: io.inputs.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: in_ptrs.as_mut_ptr(),
            },
        };
        outs[0] = AudioBusBuffers {
            numChannels: io.outputs.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: out_ptrs.as_mut_ptr(),
            },
        };
        let mut data = ProcessData {
            processMode: self.mode,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: io.frames as i32,
            numInputs: self.in_buses as i32,
            numOutputs: self.out_buses as i32,
            inputs: ins.as_mut_ptr(),
            outputs: outs.as_mut_ptr(),
            inputParameterChanges: self.changes_in_ptr.as_ptr(),
            outputParameterChanges: self.changes_out_ptr.as_ptr(),
            inputEvents: std::ptr::null_mut(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut self.context,
        };
        let r = unsafe { self.audio.process(&mut data) };
        self.context.projectTimeSamples += io.frames as i64;
        self.context.continousTimeSamples += io.frames as i64;
        if r != kResultOk {
            for o in io.outputs.iter_mut() {
                o.fill(0.0);
            }
        }
    }

    fn reset(&mut self) {
        // The recommended way to clear a plugin's internal state.
        unsafe {
            self.audio.setProcessing(0);
            self.audio.setProcessing(1);
        }
        self.context.projectTimeSamples = 0;
        self.context.continousTimeSamples = 0;
    }
}

pub fn new_context(sample_rate: f64) -> ProcessContext {
    ProcessContext {
        state: (Ctx::kPlaying | Ctx::kTempoValid | Ctx::kTimeSigValid) as u32,
        sampleRate: sample_rate,
        projectTimeSamples: 0,
        systemTime: 0,
        continousTimeSamples: 0,
        projectTimeMusic: 0.0,
        barPositionMusic: 0.0,
        cycleStartMusic: 0.0,
        cycleEndMusic: 0.0,
        tempo: 120.0,
        timeSigNumerator: 4,
        timeSigDenominator: 4,
        chord: Chord {
            keyNote: 0,
            rootNote: 0,
            chordMask: 0,
        },
        smpteOffsetSubframes: 0,
        frameRate: FrameRate {
            framesPerSecond: 0,
            flags: 0,
        },
        samplesToNextClock: 0,
    }
}
