//! Finding VST3 bundles, reading what they contain, and creating instances.

#![allow(unsafe_code)]

use crate::instance::Vst3Instance;
use crate::module;
use crate::util::{c_string, tuid_hex};
use boothcraft_plugin::{
    Candidate, HostContext, HostError, MainThread, Pending, PluginDescriptor, PluginFormat,
    PluginInstance,
};
use std::path::{Path, PathBuf};
use vst3::Steinberg::*;

/// The VST3 plugin format.
#[derive(Default)]
pub struct Vst3Format;

impl Vst3Format {
    pub fn new() -> Vst3Format {
        Vst3Format
    }
}

/// Where VST3 plugins are installed by default on this platform.
fn standard_locations() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        v.extend(home.map(|h| h.join("Library/Audio/Plug-Ins/VST3")));
    } else if cfg!(windows) {
        v.extend(std::env::var_os("COMMONPROGRAMFILES").map(|p| PathBuf::from(p).join("VST3")));
        v.extend(
            std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Programs/Common/VST3")),
        );
    } else {
        v.extend(home.map(|h| h.join(".vst3")));
        v.push(PathBuf::from("/usr/lib/vst3"));
        v.push(PathBuf::from("/usr/local/lib/vst3"));
    }
    v
}

/// Every `.vst3` under `root`, a few folders deep. Never looks inside a bundle.
fn find_bundles(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "vst3") {
            out.push(p);
        } else if depth > 0 && p.is_dir() {
            find_bundles(&p, depth - 1, out);
        }
    }
}

/// Make this process a background ("UI element") application: no Dock icon, no menu bar.
#[cfg(target_os = "macos")]
fn hide_from_dock() {
    #[repr(C)]
    struct ProcessSerialNumber {
        high: u32,
        low: u32,
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn TransformProcessType(psn: *const ProcessSerialNumber, kind: u32) -> i32;
    }
    const K_CURRENT_PROCESS: u32 = 2;
    const TO_UI_ELEMENT_APPLICATION: u32 = 4;
    let psn = ProcessSerialNumber {
        high: 0,
        low: K_CURRENT_PROCESS,
    };
    // SAFETY: plain C call with a valid, live pointer; a failure just leaves the Dock icon.
    unsafe {
        TransformProcessType(&psn, TO_UI_ELEMENT_APPLICATION);
    }
}

impl PluginFormat for Vst3Format {
    fn kind(&self) -> &'static str {
        "vst3"
    }

    fn discover(&self, extra: &[PathBuf], include_standard: bool) -> Vec<Candidate> {
        let mut found = Vec::new();
        let standard = if include_standard {
            standard_locations()
        } else {
            Vec::new()
        };
        for root in standard.iter().chain(extra) {
            find_bundles(root, 3, &mut found);
        }
        found.sort();
        found.dedup();
        found
            .into_iter()
            .map(|path| Candidate { path, known: None })
            .collect()
    }

    fn init_scan_worker(&self) {
        // The worker is a copy of the app's executable; without this each one would bounce a
        // Dock icon while it probes a plugin.
        #[cfg(target_os = "macos")]
        hide_from_dock();
    }

    fn needs_probe(&self) -> bool {
        true
    }

    fn probe(&self, c: &Candidate) -> Result<Vec<PluginDescriptor>, HostError> {
        let module = module::load(&c.path)?;
        let factory = &module.factory;
        let mut factory_vendor = String::new();
        let mut finfo: PFactoryInfo = unsafe { std::mem::zeroed() };
        if unsafe { factory.getFactoryInfo(&mut finfo) } == kResultOk {
            factory_vendor = c_string(&finfo.vendor);
        }
        let f2 = factory.cast::<IPluginFactory2>();
        let mut out = Vec::new();
        for i in 0..module.class_count() {
            let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
            let have = match &f2 {
                Some(f2) => {
                    let r = unsafe { f2.getClassInfo2(i, &mut info) };
                    r == kResultOk
                }
                None => false,
            };
            if !have {
                // An older factory: no sub-categories to tell effects from instruments. List the
                // audio classes anyway; activation refuses one that is not an effect.
                let mut plain: PClassInfo = unsafe { std::mem::zeroed() };
                if unsafe { factory.getClassInfo(i, &mut plain) } != kResultOk
                    || c_string(&plain.category) != "Audio Module Class"
                {
                    continue;
                }
                out.push(PluginDescriptor {
                    format: "vst3".into(),
                    uid: tuid_hex(&plain.cid),
                    name: c_string(&plain.name),
                    vendor: factory_vendor.clone(),
                    version: String::new(),
                    path: c.path.clone(),
                });
                continue;
            }
            if c_string(&info.category) != "Audio Module Class" {
                continue;
            }
            let subs = c_string(&info.subCategories);
            let is_fx = subs.split('|').any(|s| s == "Fx");
            let is_instrument = subs.split('|').any(|s| s == "Instrument");
            if !is_fx || is_instrument {
                continue;
            }
            let vendor = match c_string(&info.vendor) {
                v if v.is_empty() => factory_vendor.clone(),
                v => v,
            };
            out.push(PluginDescriptor {
                format: "vst3".into(),
                uid: tuid_hex(&info.cid),
                name: c_string(&info.name),
                vendor,
                version: c_string(&info.version),
                path: c.path.clone(),
            });
        }
        Ok(out)
    }

    fn instantiate(
        &self,
        d: &PluginDescriptor,
        host: HostContext,
        _mt: MainThread,
    ) -> Pending<Box<dyn PluginInstance>> {
        let made = module::load(&d.path)
            .and_then(|m| Vst3Instance::create(m, d, host))
            .map(|i| Box::new(i) as Box<dyn PluginInstance>);
        Pending::ready(made)
    }
}
