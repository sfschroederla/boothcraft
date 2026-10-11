//! The objects a plugin sees of the host: the application object, messages, and the component
//! handler. Every callback arrives re-entrantly and possibly from another thread, so each only
//! enqueues an event (see [`boothcraft_plugin::HostContext`]) and is wrapped in `catch_unwind`:
//! a panic crossing the FFI boundary would abort the process.

#![allow(unsafe_code)]

use crate::util::write_utf16;
use boothcraft_plugin::{HostContext, HostEvent};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

fn guarded(f: impl FnOnce() -> tresult) -> tresult {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(kInternalError)
}

// ---- IHostApplication ------------------------------------------------------------------------

pub struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        write_utf16("Boothcraft", &mut *name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        guarded(|| {
            if cid.is_null() || obj.is_null() {
                return kInvalidArgument;
            }
            let id: [u8; 16] = std::mem::transmute(*cid);
            if id == IMessage::IID {
                let m = ComWrapper::new(HostMessage::default());
                if let Some(p) = m.to_com_ptr::<IMessage>() {
                    *obj = p.into_raw() as *mut c_void;
                    return kResultOk;
                }
            } else if id == IAttributeList::IID {
                let a = ComWrapper::new(HostAttributes::default());
                if let Some(p) = a.to_com_ptr::<IAttributeList>() {
                    *obj = p.into_raw() as *mut c_void;
                    return kResultOk;
                }
            }
            *obj = std::ptr::null_mut();
            kResultFalse
        })
    }
}

// ---- IMessage / IAttributeList ---------------------------------------------------------------

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

#[derive(Default)]
pub struct HostAttributes {
    map: RefCell<HashMap<String, Attr>>,
}

impl Class for HostAttributes {
    type Interfaces = (IAttributeList,);
}

unsafe fn key(id: AttrID) -> Option<String> {
    (!id.is_null()).then(|| CStr::from_ptr(id).to_string_lossy().into_owned())
}

impl IAttributeListTrait for HostAttributes {
    unsafe fn setInt(&self, id: AttrID, value: i64) -> tresult {
        match key(id) {
            Some(k) => {
                self.map.borrow_mut().insert(k, Attr::Int(value));
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut i64) -> tresult {
        match (key(id), value.is_null()) {
            (Some(k), false) => match self.map.borrow().get(&k) {
                Some(Attr::Int(v)) => {
                    *value = *v;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        match key(id) {
            Some(k) => {
                self.map.borrow_mut().insert(k, Attr::Float(value));
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        match (key(id), value.is_null()) {
            (Some(k), false) => match self.map.borrow().get(&k) {
                Some(Attr::Float(v)) => {
                    *value = *v;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        match (key(id), string.is_null()) {
            (Some(k), false) => {
                let mut v = Vec::new();
                let mut p = string;
                while *p != 0 {
                    v.push(*p);
                    p = p.add(1);
                }
                self.map.borrow_mut().insert(k, Attr::Str(v));
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: u32) -> tresult {
        match (key(id), string.is_null()) {
            (Some(k), false) => match self.map.borrow().get(&k) {
                Some(Attr::Str(v)) => {
                    let cap = (size_in_bytes as usize / 2).saturating_sub(1);
                    let n = v.len().min(cap);
                    std::ptr::copy_nonoverlapping(v.as_ptr(), string, n);
                    *string.add(n) = 0;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: u32) -> tresult {
        match (key(id), data.is_null()) {
            (Some(k), false) => {
                let v =
                    std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize).to_vec();
                self.map.borrow_mut().insert(k, Attr::Bin(v));
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        match (key(id), data.is_null() || size_in_bytes.is_null()) {
            (Some(k), false) => match self.map.borrow().get(&k) {
                Some(Attr::Bin(v)) => {
                    // The pointer stays valid until the attribute is replaced or the list dies,
                    // which is the contract the SDK's own host classes give.
                    *data = v.as_ptr() as *const c_void;
                    *size_in_bytes = v.len() as u32;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }
}

#[derive(Default)]
pub struct HostMessage {
    id: RefCell<CString>,
    attrs: ComWrapperAttrs,
}

struct ComWrapperAttrs(ComWrapper<HostAttributes>);

impl Default for ComWrapperAttrs {
    fn default() -> Self {
        ComWrapperAttrs(ComWrapper::new(HostAttributes::default()))
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.borrow().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if !id.is_null() {
            *self.id.borrow_mut() = CStr::from_ptr(id).to_owned();
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Owned by the message; the caller does not release it.
        self.attrs
            .0
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

// ---- IComponentHandler -----------------------------------------------------------------------

/// Receives edits and restart requests from a plugin's controller.
pub struct ComponentHandler {
    ctx: HostContext,
    /// Step counts by parameter id, to turn a normalized value back into our plain value.
    steps: Arc<Mutex<HashMap<u32, u32>>>,
}

impl ComponentHandler {
    pub fn new(ctx: HostContext, steps: Arc<Mutex<HashMap<u32, u32>>>) -> ComponentHandler {
        ComponentHandler { ctx, steps }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: u32) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: u32, value_normalized: f64) -> tresult {
        guarded(|| {
            let steps = self
                .steps
                .lock()
                .ok()
                .and_then(|m| m.get(&id).copied())
                .unwrap_or(0);
            let value = if steps > 0 {
                value_normalized * steps as f64
            } else {
                value_normalized
            };
            self.ctx.notify(HostEvent::ParamEdited {
                id,
                value: value as f32,
            });
            kResultOk
        })
    }

    unsafe fn endEdit(&self, _id: u32) -> tresult {
        // The end of a gesture is when the state worth remembering has settled.
        guarded(|| {
            self.ctx.notify(HostEvent::StateDirty);
            kResultOk
        })
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        guarded(|| {
            const RELOAD: i32 = RestartFlags_::kReloadComponent;
            const IO: i32 = RestartFlags_::kIoChanged;
            const VALUES: i32 = RestartFlags_::kParamValuesChanged;
            const LATENCY: i32 = RestartFlags_::kLatencyChanged;
            const TITLES: i32 = RestartFlags_::kParamTitlesChanged;
            if flags & (RELOAD | IO | LATENCY) != 0 {
                self.ctx.notify(HostEvent::LatencyChanged);
            }
            if flags & (RELOAD | TITLES) != 0 {
                self.ctx.notify(HostEvent::ParamsChanged);
            }
            if flags & VALUES != 0 {
                self.ctx.notify(HostEvent::StateDirty);
            }
            kResultOk
        })
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, _state: u8) -> tresult {
        guarded(|| {
            self.ctx.notify(HostEvent::StateDirty);
            kResultOk
        })
    }

    unsafe fn requestOpenEditor(&self, _name: *const c_char) -> tresult {
        kNotImplemented
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// Keep `ComPtr` in scope for callers building handler pointers.
pub fn handler_ptr(h: &ComWrapper<ComponentHandler>) -> Option<ComPtr<IComponentHandler>> {
    h.to_com_ptr::<IComponentHandler>()
}
