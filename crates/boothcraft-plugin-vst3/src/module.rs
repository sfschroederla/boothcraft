//! Loading a `.vst3` bundle and reading its plugin factory.
//!
//! Modules are never unloaded during a session: many plugins keep threads or static state that
//! does not survive being unloaded, and a plugin the user adds twice shares one module.

#![allow(unsafe_code)]

use boothcraft_plugin::HostError;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use vst3::Steinberg::*;
use vst3::{ComPtr, Interface};

pub struct Module {
    pub factory: ComPtr<IPluginFactory>,
}

thread_local! {
    // Loading happens on the main thread (and on the scan worker's only thread), so a
    // thread-local registry is enough and keeps non-`Send` COM pointers out of statics.
    static LOADED: RefCell<HashMap<PathBuf, Rc<Module>>> = RefCell::new(HashMap::new());
}

/// Load (or reuse) the module at `path`.
pub fn load(path: &Path) -> Result<Rc<Module>, HostError> {
    if let Some(m) = LOADED.with(|l| l.borrow().get(path).cloned()) {
        return Ok(m);
    }
    let factory = platform::open(path)?;
    let module = Rc::new(Module { factory });
    LOADED.with(|l| l.borrow_mut().insert(path.to_path_buf(), module.clone()));
    Ok(module)
}

impl Module {
    pub fn class_count(&self) -> i32 {
        unsafe { self.factory.countClasses() }
    }

    /// Hand the factory our host context (before any `createInstance`), if it asks for one.
    pub fn set_host_context(&self, host: *mut FUnknown) {
        if let Some(f3) = self.factory.cast::<IPluginFactory3>() {
            unsafe {
                f3.setHostContext(host);
            }
        }
    }

    /// Create an object of class `cid` as interface `I`.
    pub fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut obj: *mut std::ffi::c_void = std::ptr::null_mut();
        let iid = I::IID;
        let r = unsafe {
            self.factory
                .createInstance(cid.as_ptr(), iid.as_ptr() as *const _, &mut obj)
        };
        if r != kResultOk {
            return None;
        }
        unsafe { ComPtr::from_raw(obj as *mut I) }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use core_foundation::bundle::CFBundle;
    use core_foundation::string::CFString;
    use core_foundation::url::CFURL;
    use core_foundation_sys::base::CFTypeRef;
    use core_foundation_sys::bundle::CFBundleLoadExecutable;

    type BundleEntry = unsafe extern "C" fn(CFTypeRef) -> bool;
    type GetFactory = unsafe extern "C" fn() -> *mut IPluginFactory;

    pub fn open(path: &Path) -> Result<ComPtr<IPluginFactory>, HostError> {
        let load_err = |why: &str| HostError::Load(format!("{}: {why}", path.display()));
        let url = CFURL::from_path(path, true).ok_or_else(|| load_err("not a valid path"))?;
        let bundle = CFBundle::new(url).ok_or_else(|| load_err("not a bundle"))?;
        use core_foundation::base::TCFType;
        let loaded = unsafe { CFBundleLoadExecutable(bundle.as_concrete_TypeRef()) };
        if loaded == 0 {
            return Err(load_err("could not load the executable"));
        }
        let func = |name: &str| bundle.function_pointer_for_name(CFString::new(name));
        let mut entry = func("bundleEntry");
        if entry.is_null() {
            entry = func("BundleEntry");
        }
        if !entry.is_null() {
            let entry: BundleEntry = unsafe { std::mem::transmute(entry) };
            if !unsafe { entry(bundle.as_concrete_TypeRef() as CFTypeRef) } {
                return Err(load_err("bundleEntry failed"));
            }
        }
        let get = func("GetPluginFactory");
        if get.is_null() {
            return Err(load_err("no GetPluginFactory"));
        }
        let get: GetFactory = unsafe { std::mem::transmute(get) };
        // The bundle (and so the loaded code) must outlive everything made from it.
        std::mem::forget(bundle);
        unsafe { ComPtr::from_raw(get()) }.ok_or_else(|| load_err("no plugin factory"))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;

    /// The shared object inside a bundle directory, or `path` itself when it is a plain file.
    fn binary(path: &Path) -> Option<PathBuf> {
        if path.is_file() {
            return Some(path.to_path_buf());
        }
        let stem = path.file_stem()?.to_owned();
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        let (dir, ext) = if cfg!(windows) {
            (format!("{arch}-win"), "vst3")
        } else {
            (format!("{arch}-linux"), "so")
        };
        let p = path
            .join("Contents")
            .join(dir)
            .join(Path::new(&stem).with_extension(ext));
        p.is_file().then_some(p)
    }

    pub fn open(path: &Path) -> Result<ComPtr<IPluginFactory>, HostError> {
        let load_err = |why: String| HostError::Load(format!("{}: {why}", path.display()));
        let bin = binary(path).ok_or_else(|| load_err("no module binary found".into()))?;
        let lib = unsafe { libloading::Library::new(&bin) }.map_err(|e| load_err(e.to_string()))?;
        let (init, get_name): (&[&[u8]], &[u8]) = if cfg!(windows) {
            (&[b"InitDll\0"], b"GetPluginFactory\0")
        } else {
            (&[b"ModuleEntry\0"], b"GetPluginFactory\0")
        };
        for name in init {
            if let Ok(f) =
                unsafe { lib.get::<unsafe extern "system" fn(*mut std::ffi::c_void) -> bool>(name) }
            {
                let handle = lib_handle(&lib);
                if !unsafe { f(handle) } {
                    return Err(load_err("module entry failed".into()));
                }
            }
        }
        let get: libloading::Symbol<GetFactory> =
            unsafe { lib.get(get_name) }.map_err(|e| load_err(e.to_string()))?;
        let factory = unsafe { get() };
        let factory = unsafe { ComPtr::from_raw(factory) }
            .ok_or_else(|| load_err("no plugin factory".into()))?;
        // Never unload.
        std::mem::forget(lib);
        Ok(factory)
    }

    /// The handle `ModuleEntry`/`InitDll` expect: the library handle on Linux, the module on
    /// Windows. `libloading` keeps it private, so this passes null, which every shipping plugin
    /// accepts (it is only used for resource lookup).
    fn lib_handle(_lib: &libloading::Library) -> *mut std::ffi::c_void {
        std::ptr::null_mut()
    }
}
