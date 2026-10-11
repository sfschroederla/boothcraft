//! A loaded VST3 plugin: component, controller and the connection between them.

#![allow(unsafe_code)]

use crate::host::{handler_ptr, ComponentHandler, HostApplication};
use crate::module::Module;
use crate::params::Changes;
use crate::processor::{new_context, Vst3Processor, MAX_BUSES};
use crate::state::{self, Parts};
use crate::stream::MemoryStream;
use crate::util::{tuid_from_hex, tuid_hex, utf16_string};
use boothcraft_dsp::param::flags;
use boothcraft_plugin::{
    Activated, HostContext, HostError, MainThread, ParamInfo, PluginDescriptor, PluginInstance,
    PluginProcessor, ProcessConfig, StateKind,
};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

/// Stepped parameters with more steps than this are shown as continuous sliders.
const MAX_LABELLED_STEPS: i32 = 64;
/// Parameters that can change within one block (the pool size for the input changes).
const CHANGES_CAPACITY: usize = 128;
/// A "tail" longer than this many seconds is clamped (plugins may report infinity).
const MAX_TAIL_SECS: f64 = 30.0;

struct ParamMeta {
    id: u32,
    title: String,
    units: String,
    steps: i32,
    default: f64,
    flags: i32,
    /// Labels for stepped parameters, when the plugin provides them.
    choices: Vec<String>,
}

impl ParamMeta {
    /// Multiplier from our plain value to the normalized one.
    fn scale(&self) -> f64 {
        if self.choices.is_empty() {
            1.0
        } else {
            1.0 / self.steps as f64
        }
    }
}

pub struct Vst3Instance {
    _module: Rc<Module>,
    class_id: TUID,
    class_hex: String,
    _host: ComWrapper<HostApplication>,
    host_ptr: ComPtr<IHostApplication>,
    handler: ComWrapper<ComponentHandler>,
    component: ComPtr<IComponent>,
    audio: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    separate_controller: bool,
    connected: bool,
    params: Vec<ParamMeta>,
    steps: Arc<Mutex<HashMap<u32, u32>>>,
    activated: bool,
    /// Edits the component's own state does not yet carry, by parameter id.
    edits: BTreeMap<u32, f32>,
}

fn ok(r: tresult) -> bool {
    r == kResultOk
}

impl Vst3Instance {
    pub fn create(
        module: Rc<Module>,
        desc: &PluginDescriptor,
        ctx: HostContext,
    ) -> Result<Vst3Instance, HostError> {
        let load = |why: &str| HostError::Load(format!("{}: {why}", desc.name));
        let cid = tuid_from_hex(&desc.uid).ok_or_else(|| load("bad class id"))?;
        let host = ComWrapper::new(HostApplication);
        let host_ptr = host
            .to_com_ptr::<IHostApplication>()
            .ok_or_else(|| load("no host object"))?;
        let host_unknown = host_ptr.as_ptr() as *mut FUnknown;
        module.set_host_context(host_unknown);

        let component = module
            .create::<IComponent>(&cid)
            .ok_or_else(|| load("could not create the component"))?;
        if !ok(unsafe { component.initialize(host_unknown) }) {
            return Err(load("the component refused to initialize"));
        }
        let finish = |component: &ComPtr<IComponent>, e: HostError| {
            unsafe { component.terminate() };
            e
        };
        let Some(audio) = component.cast::<IAudioProcessor>() else {
            return Err(finish(&component, load("not an audio processor")));
        };

        // A single-component plugin is its own controller; otherwise create the controller.
        let (controller, separate) = match component.cast::<IEditController>() {
            Some(c) => (c, false),
            None => {
                let mut cc: TUID = [0; 16];
                if !ok(unsafe { component.getControllerClassId(&mut cc) }) {
                    return Err(finish(&component, load("no edit controller")));
                }
                let Some(c) = module.create::<IEditController>(&cc) else {
                    return Err(finish(&component, load("could not create the controller")));
                };
                if !ok(unsafe { c.initialize(host_unknown) }) {
                    return Err(finish(
                        &component,
                        load("the controller refused to initialize"),
                    ));
                }
                (c, true)
            }
        };

        let steps = Arc::new(Mutex::new(HashMap::new()));
        let handler = ComWrapper::new(ComponentHandler::new(ctx, steps.clone()));
        if let Some(h) = handler_ptr(&handler) {
            unsafe { controller.setComponentHandler(h.as_ptr()) };
        }

        // Connect the two halves so they can message each other.
        let mut connected = false;
        if separate {
            if let (Some(a), Some(b)) = (
                component.cast::<IConnectionPoint>(),
                controller.cast::<IConnectionPoint>(),
            ) {
                let r1 = unsafe { a.connect(b.as_ptr()) };
                let r2 = unsafe { b.connect(a.as_ptr()) };
                connected = ok(r1) && ok(r2);
            }
        }

        let mut inst = Vst3Instance {
            _module: module,
            class_id: cid,
            class_hex: tuid_hex(&cid),
            _host: host,
            host_ptr,
            handler,
            component,
            audio,
            controller,
            separate_controller: separate,
            connected,
            params: Vec::new(),
            steps,
            activated: false,
            edits: BTreeMap::new(),
        };
        // Give the controller the component's initial state, as hosts do.
        if separate {
            let (w, p) = MemoryStream::empty();
            if ok(unsafe { inst.component.getState(p.as_ptr()) }) {
                w.rewind();
                unsafe { inst.controller.setComponentState(p.as_ptr()) };
            }
        }
        inst.read_params();
        Ok(inst)
    }

    fn read_params(&mut self) {
        let n = unsafe { self.controller.getParameterCount() };
        let mut params = Vec::new();
        let mut steps = HashMap::new();
        for i in 0..n {
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            if !ok(unsafe { self.controller.getParameterInfo(i, &mut info) }) {
                continue;
            }
            let mut choices = Vec::new();
            if info.stepCount > 0 && info.stepCount <= MAX_LABELLED_STEPS {
                for k in 0..=info.stepCount {
                    let v = k as f64 / info.stepCount as f64;
                    let mut s: String128 = [0; 128];
                    let label =
                        if ok(unsafe { self.controller.getParamStringByValue(info.id, v, &mut s) })
                        {
                            utf16_string(&s)
                        } else {
                            String::new()
                        };
                    choices.push(if label.is_empty() {
                        format!("{k}")
                    } else {
                        label
                    });
                }
            }
            if info.stepCount > 0 {
                steps.insert(
                    info.id,
                    if choices.is_empty() {
                        0
                    } else {
                        info.stepCount as u32
                    },
                );
            }
            params.push(ParamMeta {
                id: info.id,
                title: utf16_string(&info.title),
                units: utf16_string(&info.units),
                steps: info.stepCount,
                default: info.defaultNormalizedValue,
                flags: info.flags,
                choices,
            });
        }
        if let Ok(mut m) = self.steps.lock() {
            *m = steps;
        }
        self.params = params;
    }

    fn meta(&self, id: u32) -> Option<&ParamMeta> {
        self.params.iter().find(|p| p.id == id)
    }

    fn to_normalized(&self, id: u32, value: f32) -> f64 {
        let k = self.meta(id).map_or(1.0, ParamMeta::scale);
        (value as f64 * k).clamp(0.0, 1.0)
    }

    fn bus_count(&self, dir: BusDirection) -> i32 {
        unsafe {
            self.component
                .getBusCount(MediaTypes_::kAudio as MediaType, dir)
        }
    }
}

impl PluginInstance for Vst3Instance {
    fn params(&self) -> Vec<ParamInfo> {
        self.params
            .iter()
            .map(|m| {
                let labelled = !m.choices.is_empty();
                let norm = unsafe { self.controller.getParamNormalized(m.id) };
                let k = if labelled { m.steps as f64 } else { 1.0 };
                let mut f = 0u8;
                if m.flags & ParameterInfo_::ParameterFlags_::kIsHidden != 0
                    || m.flags & ParameterInfo_::ParameterFlags_::kIsProgramChange != 0
                {
                    f |= flags::HIDDEN;
                }
                if m.flags & ParameterInfo_::ParameterFlags_::kIsReadOnly != 0 {
                    f |= flags::READ_ONLY;
                }
                if m.flags & ParameterInfo_::ParameterFlags_::kIsBypass != 0 {
                    f |= flags::BYPASS;
                }
                ParamInfo {
                    id: m.id,
                    name: m.title.clone(),
                    unit: m.units.clone(),
                    min: 0.0,
                    max: k as f32,
                    default: (m.default * k) as f32,
                    current: (norm * k) as f32,
                    steps: m.steps.max(0) as u32,
                    choices: m.choices.clone(),
                    flags: f,
                }
            })
            .collect()
    }

    fn set_param(&mut self, _mt: MainThread, id: u32, value: f32) {
        let n = self.to_normalized(id, value);
        unsafe { self.controller.setParamNormalized(id, n) };
        // The component only learns of this through `process`, so remember it.
        self.edits.insert(id, value);
    }

    fn pending_params(&self) -> Vec<(u32, f32)> {
        self.edits.iter().map(|(k, v)| (*k, *v)).collect()
    }

    fn param_text(&self, id: u32, value: f32) -> String {
        let n = self.to_normalized(id, value);
        let mut s: String128 = [0; 128];
        if ok(unsafe { self.controller.getParamStringByValue(id, n, &mut s) }) {
            utf16_string(&s)
        } else {
            format!("{value:.2}")
        }
    }

    fn save_state(&mut self, _mt: MainThread, _kind: StateKind) -> Result<Vec<u8>, HostError> {
        let (w, p) = MemoryStream::empty();
        if !ok(unsafe { self.component.getState(p.as_ptr()) }) {
            return Err(HostError::State(
                "the plugin could not save its state".into(),
            ));
        }
        let component = w.bytes();
        let controller = if self.separate_controller {
            let (cw, cp) = MemoryStream::empty();
            ok(unsafe { self.controller.getState(cp.as_ptr()) }).then(|| cw.bytes())
        } else {
            None
        };
        Ok(state::write(
            &self.class_hex,
            &Parts {
                component,
                controller,
                edits: self.pending_params(),
            },
        ))
    }

    fn load_state(&mut self, _mt: MainThread, bytes: &[u8]) -> Result<(), HostError> {
        let parts = state::read(bytes)?;
        self.edits.clear();
        let (w, p) = MemoryStream::wrap(parts.component);
        if !ok(unsafe { self.component.setState(p.as_ptr()) }) {
            return Err(HostError::State(
                "the plugin rejected its saved state".into(),
            ));
        }
        if self.separate_controller {
            w.rewind();
            unsafe { self.controller.setComponentState(p.as_ptr()) };
            if let Some(c) = parts.controller {
                let (_cw, cp) = MemoryStream::wrap(c);
                unsafe { self.controller.setState(cp.as_ptr()) };
            }
        }
        // Re-apply edits the component's state did not carry.
        for (id, v) in parts.edits {
            let n = self.to_normalized(id, v);
            unsafe { self.controller.setParamNormalized(id, n) };
            self.edits.insert(id, v);
        }
        Ok(())
    }

    fn activate(
        &mut self,
        _mt: MainThread,
        cfg: &ProcessConfig,
    ) -> Result<(Box<dyn PluginProcessor>, Activated), HostError> {
        let fail = |why: String| HostError::Activate(why);
        if !ok(unsafe {
            self.audio
                .canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32)
        }) {
            return Err(HostError::Unsupported("no 32-bit processing".into()));
        }
        let (n_in, n_out) = (
            self.bus_count(BusDirections_::kInput as BusDirection),
            self.bus_count(BusDirections_::kOutput as BusDirection),
        );
        if n_in < 1 || n_out < 1 {
            return Err(HostError::Unsupported(
                "not an effect (needs audio in and out)".into(),
            ));
        }
        if n_in as usize > MAX_BUSES || n_out as usize > MAX_BUSES {
            return Err(HostError::Unsupported("too many audio buses".into()));
        }

        // Ask for mono or stereo on the main buses and keep whatever the others already use.
        let wanted = if cfg.channels <= 1 {
            SpeakerArr::kMono
        } else {
            SpeakerArr::kStereo
        };
        let arrangements = |dir: BusDirection, n: i32| -> Vec<SpeakerArrangement> {
            (0..n)
                .map(|i| {
                    if i == 0 {
                        return wanted;
                    }
                    let mut a: SpeakerArrangement = 0;
                    if ok(unsafe { self.audio.getBusArrangement(dir, i, &mut a) }) {
                        a
                    } else {
                        SpeakerArr::kStereo
                    }
                })
                .collect()
        };
        let mut ins = arrangements(BusDirections_::kInput as BusDirection, n_in);
        let mut outs = arrangements(BusDirections_::kOutput as BusDirection, n_out);
        // A refusal is normal (a stereo-only plugin): read back what it will actually use.
        unsafe {
            self.audio
                .setBusArrangements(ins.as_mut_ptr(), n_in, outs.as_mut_ptr(), n_out);
        }
        let channels = |dir: BusDirection| -> Result<usize, HostError> {
            let mut a: SpeakerArrangement = 0;
            if !ok(unsafe { self.audio.getBusArrangement(dir, 0, &mut a) }) {
                return Err(fail("could not read the bus arrangement".into()));
            }
            if a >> 63 != 0 || a == 0 {
                return Err(HostError::Unsupported("ambisonic or empty bus".into()));
            }
            Ok(a.count_ones() as usize)
        };
        let in_ch = channels(BusDirections_::kInput as BusDirection)?;
        let out_ch = channels(BusDirections_::kOutput as BusDirection)?;
        if in_ch > 8 || out_ch > 8 {
            return Err(HostError::Unsupported("more than 8 channels".into()));
        }

        // Only the main buses run; sidechains and extra outputs stay off.
        for (dir, n) in [
            (BusDirections_::kInput as BusDirection, n_in),
            (BusDirections_::kOutput as BusDirection, n_out),
        ] {
            for i in 0..n {
                unsafe {
                    self.component.activateBus(
                        MediaTypes_::kAudio as MediaType,
                        dir,
                        i,
                        (i == 0) as u8,
                    );
                }
            }
        }

        let mode = if cfg.offline {
            ProcessModes_::kOffline
        } else {
            ProcessModes_::kRealtime
        } as i32;
        let mut setup = ProcessSetup {
            processMode: mode,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: cfg.max_block as i32,
            sampleRate: cfg.sample_rate as f64,
        };
        if !ok(unsafe { self.audio.setupProcessing(&mut setup) }) {
            return Err(fail("setupProcessing failed".into()));
        }
        if !ok(unsafe { self.component.setActive(1) }) {
            return Err(fail("setActive failed".into()));
        }
        unsafe { self.audio.setProcessing(1) };
        self.activated = true;

        let changes_in = Changes::new(CHANGES_CAPACITY);
        let changes_out = Changes::new(CHANGES_CAPACITY);
        let (Some(changes_in_ptr), Some(changes_out_ptr)) = (
            changes_in.to_com_ptr::<IParameterChanges>(),
            changes_out.to_com_ptr::<IParameterChanges>(),
        ) else {
            return Err(fail("could not create parameter change objects".into()));
        };
        let processor = Vst3Processor {
            audio: self.audio.clone(),
            _component: self.component.clone(),
            changes_in,
            changes_in_ptr,
            changes_out,
            changes_out_ptr,
            scale: self.params.iter().map(|p| (p.id, p.scale())).collect(),
            mode,
            in_buses: n_in as usize,
            out_buses: n_out as usize,
            context: new_context(cfg.sample_rate as f64),
        };
        let activated = Activated {
            in_channels: in_ch,
            out_channels: out_ch,
            latency: self.latency(),
            tail: self.tail_for(cfg.sample_rate as f64),
        };
        Ok((Box::new(processor), activated))
    }

    fn deactivate(&mut self, _mt: MainThread, processor: Box<dyn PluginProcessor>) {
        if self.activated {
            unsafe {
                self.audio.setProcessing(0);
                self.component.setActive(0);
            }
            self.activated = false;
        }
        drop(processor);
    }

    fn latency(&self) -> usize {
        unsafe { self.audio.getLatencySamples() as usize }
    }

    fn tail(&self) -> usize {
        self.tail_for(48_000.0)
    }
}

impl Vst3Instance {
    fn tail_for(&self, sample_rate: f64) -> usize {
        let t = unsafe { self.audio.getTailSamples() };
        let cap = (MAX_TAIL_SECS * sample_rate) as usize;
        if t == kInfiniteTail {
            cap
        } else {
            (t as usize).min(cap)
        }
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        // Always on the main thread (the manager drops instances there).
        unsafe {
            if self.activated {
                self.audio.setProcessing(0);
                self.component.setActive(0);
            }
            if self.connected {
                if let (Some(a), Some(b)) = (
                    self.component.cast::<IConnectionPoint>(),
                    self.controller.cast::<IConnectionPoint>(),
                ) {
                    a.disconnect(b.as_ptr());
                    b.disconnect(a.as_ptr());
                }
            }
            self.controller.setComponentHandler(std::ptr::null_mut());
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
        let _ = (&self.handler, &self.host_ptr, &self.class_id);
    }
}
