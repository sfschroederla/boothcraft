//! Windows and Linux editor windows are not implemented yet (M5b, M5c); the generic parameter
//! controls are used there.

use std::ffi::c_void;

pub struct EditorWindow;

impl EditorWindow {
    pub fn open(
        _title: &str,
        _size: (f64, f64),
        _resizable: bool,
        _parent_view: Option<*mut c_void>,
    ) -> Result<EditorWindow, String> {
        Err("plugin editor windows are not available on this platform yet".into())
    }

    pub fn native_view(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    pub fn is_open(&self) -> bool {
        false
    }

    pub fn is_key(&self) -> bool {
        false
    }

    pub fn content_size(&self) -> (f64, f64) {
        (0.0, 0.0)
    }

    pub fn set_content_size(&self, _w: f64, _h: f64) {}

    pub fn front(&self) {}

    pub fn window_number(&self) -> isize {
        0
    }

    pub fn close(&mut self) {}
}

pub fn any_editor_is_key() -> bool {
    false
}
