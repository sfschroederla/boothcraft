//! A single-component VST3 effect for testing a host.
//!
//! It delays its input by a number of frames set by its one parameter, "Latency" (0..1 maps to
//! 0..=256 frames, default 64), and reports exactly that as its latency. When the parameter
//! changes it calls `restartComponent(kLatencyChanged)`, as a real plugin does; the new delay takes
//! effect when the host re-activates it. State is the parameter as a little-endian f64.
//!
//! Adapted from the `vst3` crate's `gain` example (MIT OR Apache-2.0).

#![allow(
    unsafe_code,
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals
)]
// The SDK's enum constants are `u32` on some platforms and `i32` on others, so casts that are
// redundant here are needed elsewhere.
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{ptr, slice};
use vst3::{uid, Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

const CID: TUID = uid(0x426F6F74, 0x68437261, 0x66744C61, 0x74656E31);
const MAX_LATENCY: f64 = 256.0;
const DEFAULT_PARAM: f64 = 0.25;
const NAME: &str = "Boothcraft Latency Test";

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c = CString::new(src).unwrap_or_default();
    for (d, s) in dst.iter_mut().zip(c.as_bytes_with_nul()) {
        *d = *s as c_char;
    }
    if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut n = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s as TChar;
        n += 1;
    }
    if n < dst.len() {
        dst[n] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn latency_of(param: f64) -> usize {
    (param.clamp(0.0, 1.0) * MAX_LATENCY).round() as usize
}

struct LatencyPlugin {
    /// The parameter, normalized (f64 bits). Written by the controller side, read by both.
    param: AtomicU64,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    ring: RefCell<Vec<Vec<f32>>>,
    at: Cell<usize>,
    /// The delay in use, fixed when the plugin is activated.
    latched: Cell<usize>,
    active: Cell<bool>,
    arrangement: Cell<SpeakerArrangement>,
}

impl Class for LatencyPlugin {
    type Interfaces = (
        IComponent,
        IAudioProcessor,
        IEditController,
        IProcessContextRequirements,
    );
}

impl LatencyPlugin {
    fn new() -> LatencyPlugin {
        LatencyPlugin {
            param: AtomicU64::new(DEFAULT_PARAM.to_bits()),
            handler: RefCell::new(None),
            ring: RefCell::new(Vec::new()),
            at: Cell::new(0),
            latched: Cell::new(latency_of(DEFAULT_PARAM)),
            active: Cell::new(false),
            arrangement: Cell::new(SpeakerArr::kStereo),
        }
    }

    fn param(&self) -> f64 {
        f64::from_bits(self.param.load(Ordering::Relaxed))
    }

    fn channels(&self) -> i32 {
        self.arrangement.get().count_ones() as i32
    }
}

impl IPluginBaseTrait for LatencyPlugin {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for LatencyPlugin {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        kNotImplemented // a single component is its own controller
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        if media_type as MediaTypes == MediaTypes_::kAudio && (dir as BusDirections) < 2 {
            1
        } else {
            0
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type as MediaTypes != MediaTypes_::kAudio || index != 0 || dir as BusDirections > 1
        {
            return kInvalidArgument;
        }
        let bus = &mut *bus;
        bus.mediaType = MediaTypes_::kAudio as MediaType;
        bus.direction = dir;
        bus.channelCount = self.channels();
        copy_wstring(
            if dir as BusDirections == BusDirections_::kInput {
                "Input"
            } else {
                "Output"
            },
            &mut bus.name,
        );
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _in_info: *mut RoutingInfo,
        _out_info: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _t: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state != 0 {
            // Latch the delay now; a later parameter change waits for the next activation.
            let n = latency_of(self.param());
            self.latched.set(n);
            *self.ring.borrow_mut() = vec![vec![0.0; n.max(1)]; 2];
            self.at.set(0);
        }
        self.active.set(state != 0);
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(s) = ComRef::from_raw(state) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 8];
        let mut n = 0;
        if s.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut n) != kResultOk || n != 8 {
            return kResultFalse;
        }
        self.param.store(
            f64::from_le_bytes(bytes).clamp(0.0, 1.0).to_bits(),
            Ordering::Relaxed,
        );
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(s) = ComRef::from_raw(state) else {
            return kInvalidArgument;
        };
        let mut bytes = self.param().to_le_bytes();
        let mut n = 0;
        s.write(bytes.as_mut_ptr() as *mut c_void, 8, &mut n)
    }
}

impl IAudioProcessorTrait for LatencyPlugin {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins != 1 || num_outs != 1 || *inputs != *outputs {
            return kResultFalse;
        }
        if *inputs == SpeakerArr::kMono || *inputs == SpeakerArr::kStereo {
            self.arrangement.set(*inputs);
            return kResultTrue;
        }
        kResultFalse
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        *arr = self.arrangement.get();
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size as SymbolicSampleSizes == SymbolicSampleSizes_::kSample32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        // What the running delay really is while active; what it will be once activated otherwise.
        if self.active.get() {
            self.latched.get() as u32
        } else {
            latency_of(self.param()) as u32
        }
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let d = &*data;
        if let Some(changes) = ComRef::from_raw(d.inputParameterChanges) {
            for i in 0..changes.getParameterCount() {
                if let Some(q) = ComRef::from_raw(changes.getParameterData(i)) {
                    let n = q.getPointCount();
                    if q.getParameterId() == 0 && n > 0 {
                        let (mut off, mut v) = (0, 0.0);
                        if q.getPoint(n - 1, &mut off, &mut v) == kResultOk {
                            self.param
                                .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        if d.numInputs != 1 || d.numOutputs != 1 {
            return kResultOk;
        }
        let (ib, ob) = (&*d.inputs, &*d.outputs);
        let ch = (ib.numChannels.min(ob.numChannels)).clamp(0, 2) as usize;
        let n = d.numSamples as usize;
        let lat = self.latched.get();
        let mut ring = self.ring.borrow_mut();
        for c in 0..ch {
            let input = slice::from_raw_parts(*ib.__field0.channelBuffers32.add(c), n);
            let output = slice::from_raw_parts_mut(*ob.__field0.channelBuffers32.add(c), n);
            if lat == 0 || ring.len() <= c {
                output.copy_from_slice(input);
                continue;
            }
            let mut at = self.at.get();
            for i in 0..n {
                output[i] = ring[c][at];
                ring[c][at] = input[i];
                at = (at + 1) % lat;
            }
            if c + 1 == ch {
                self.at.set(at);
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for LatencyPlugin {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

impl IEditControllerTrait for LatencyPlugin {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kNotImplemented
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        1
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.id = 0;
        copy_wstring("Latency", &mut info.title);
        copy_wstring("Latency", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = DEFAULT_PARAM;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 {
            return kInvalidArgument;
        }
        copy_wstring(&format!("{} samples", latency_of(value)), &mut *string);
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        _id: u32,
        _string: *mut TChar,
        _value: *mut f64,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, value: f64) -> f64 {
        value * MAX_LATENCY
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, plain: f64) -> f64 {
        plain / MAX_LATENCY
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        if id == 0 {
            self.param()
        } else {
            0.0
        }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id != 0 {
            return kInvalidArgument;
        }
        let before = latency_of(self.param());
        self.param
            .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        if latency_of(value) != before {
            if let Some(h) = self.handler.borrow().as_ref() {
                // Re-entrant on purpose: a real plugin calls back from inside the host's call.
                h.restartComponent(RestartFlags_::kLatencyChanged);
            }
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.borrow_mut() = ComRef::from_raw(handler).map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = &mut *info;
        copy_cstring("Boothcraft Tests", &mut info.vendor);
        copy_cstring("https://example.invalid", &mut info.url);
        copy_cstring("tests@example.invalid", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        1
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(NAME, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        if *(cid as *const TUID) != CID {
            return kInvalidArgument;
        }
        let p = ComWrapper::new(LatencyPlugin::new())
            .to_com_ptr::<FUnknown>()
            .unwrap();
        let raw = p.as_ptr();
        ((*(*raw).vtbl).queryInterface)(raw, iid as *mut TUID, obj)
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(NAME, &mut info.name);
        info.classFlags = 0;
        copy_cstring("Fx", &mut info.subCategories);
        copy_cstring("Boothcraft Tests", &mut info.vendor);
        copy_cstring("1.0.0", &mut info.version);
        copy_cstring("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "C" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "C" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleExit() -> bool {
    true
}

#[no_mangle]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .unwrap()
        .into_raw()
}
