//! A VST3 plugin's own interface (`IPlugView`) and the host's `IPlugFrame`.

#![allow(unsafe_code)]

use boothcraft_plugin::{HostError, NativeParent, PluginEditor};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

type Request = Arc<Mutex<Option<(u32, u32)>>>;

/// What the plugin calls back on to ask for a different size.
pub struct PlugFrame {
    request: Request,
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        // Only record it: the host resizes its window on its own turn, then calls `onSize`.
        catch_unwind(AssertUnwindSafe(|| {
            if new_size.is_null() {
                return kInvalidArgument;
            }
            let r = *new_size;
            let size = (
                (r.right - r.left).max(1) as u32,
                (r.bottom - r.top).max(1) as u32,
            );
            if let Ok(mut g) = self.request.lock() {
                *g = Some(size);
            }
            kResultOk
        }))
        .unwrap_or(kInternalError)
    }
}

pub struct Vst3Editor {
    view: ComPtr<IPlugView>,
    frame: ComWrapper<PlugFrame>,
    request: Request,
    size: (u32, u32),
    resizable: bool,
    attached: bool,
}

fn rect(w: u32, h: u32) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: w as i32,
        bottom: h as i32,
    }
}

impl Vst3Editor {
    /// Wrap a freshly created view; `None` if the plugin cannot draw on this platform.
    pub fn new(view: ComPtr<IPlugView>) -> Option<Vst3Editor> {
        let platform: &[u8] = if cfg!(target_os = "macos") {
            b"NSView\0"
        } else if cfg!(windows) {
            b"HWND\0"
        } else {
            b"X11EmbedWindowID\0"
        };
        if unsafe { view.isPlatformTypeSupported(platform.as_ptr() as *const _) } != kResultTrue {
            return None;
        }
        let mut r = rect(0, 0);
        if unsafe { view.getSize(&mut r) } != kResultOk {
            return None;
        }
        let resizable = unsafe { view.canResize() } == kResultTrue;
        let request: Request = Arc::new(Mutex::new(None));
        Some(Vst3Editor {
            view,
            frame: ComWrapper::new(PlugFrame {
                request: request.clone(),
            }),
            request,
            size: (
                (r.right - r.left).max(1) as u32,
                (r.bottom - r.top).max(1) as u32,
            ),
            resizable,
            attached: false,
        })
    }
}

impl PluginEditor for Vst3Editor {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn resizable(&self) -> bool {
        self.resizable
    }

    fn attach(&mut self, parent: NativeParent) -> Result<(), HostError> {
        let (ptr, kind): (*mut c_void, &[u8]) = match parent {
            NativeParent::NSView(p) => (p, b"NSView\0"),
            NativeParent::Hwnd(p) => (p, b"HWND\0"),
            NativeParent::X11(id) => (id as usize as *mut c_void, b"X11EmbedWindowID\0"),
        };
        if let Some(f) = self.frame.to_com_ptr::<IPlugFrame>() {
            unsafe { self.view.setFrame(f.as_ptr()) };
        }
        let r = unsafe { self.view.attached(ptr, kind.as_ptr() as *const _) };
        if r != kResultOk {
            unsafe { self.view.setFrame(std::ptr::null_mut()) };
            return Err(HostError::Load(
                "the plugin could not attach its interface".into(),
            ));
        }
        self.attached = true;
        Ok(())
    }

    fn set_size(&mut self, width: u32, height: u32) -> (u32, u32) {
        let mut r = rect(width, height);
        if self.resizable {
            unsafe { self.view.checkSizeConstraint(&mut r) };
        }
        unsafe { self.view.onSize(&mut r) };
        let size = (
            (r.right - r.left).max(1) as u32,
            (r.bottom - r.top).max(1) as u32,
        );
        self.size = size;
        size
    }

    fn take_resize_request(&mut self) -> Option<(u32, u32)> {
        self.request.lock().ok().and_then(|mut g| g.take())
    }

    fn close(&mut self) {
        if self.attached {
            unsafe {
                self.view.removed();
                self.view.setFrame(std::ptr::null_mut());
            }
            self.attached = false;
        }
    }
}

impl Drop for Vst3Editor {
    fn drop(&mut self) {
        self.close();
    }
}
