//! The macOS editor window: an `NSWindow` whose content view the plugin attaches to.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;

thread_local! {
    /// Window numbers of every open editor window, to tell whether the key window is one of ours.
    static OPEN: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
}

pub struct EditorWindow {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    number: isize,
    closed: bool,
}

impl EditorWindow {
    /// Open a window with a content area of `size` points. `parent_view` is the app's main window
    /// content view (from its window handle): the editor becomes its child, so it stays above it.
    pub fn open(
        title: &str,
        size: (f64, f64),
        resizable: bool,
        parent_view: Option<*mut c_void>,
    ) -> Result<EditorWindow, String> {
        let mtm =
            MainThreadMarker::new().ok_or("editor windows must be opened on the main thread")?;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size.0, size.1));
        let mut style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        // SAFETY: a plain `initWithContentRect:styleMask:backing:defer:` on a freshly allocated
        // window, on the main thread.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // We own the window (this struct holds the only reference); AppKit must not free it on close.
        // SAFETY: setting a flag on a window we own.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        let view = NSView::initWithFrame(mtm.alloc::<NSView>(), rect);
        window.setContentView(Some(&view));
        window.center();

        if let Some(p) = parent_view {
            // SAFETY: the caller passes the main window's content view pointer, which outlives
            // this call (we only read its window).
            let parent: &NSView = unsafe { &*(p as *const NSView) };
            if let Some(pw) = parent.window() {
                // SAFETY: both are live windows; ordering mode is a plain enum.
                unsafe { pw.addChildWindow_ordered(&window, NSWindowOrderingMode::Above) };
            }
        }
        window.makeKeyAndOrderFront(None);
        let number = window.windowNumber();
        OPEN.with(|o| o.borrow_mut().insert(number));
        Ok(EditorWindow {
            window,
            view,
            number,
            closed: false,
        })
    }

    /// The view to hand to the plugin as its parent (an `NSView*`).
    pub fn native_view(&self) -> *mut c_void {
        Retained::as_ptr(&self.view) as *mut c_void
    }

    /// Whether the window is still on screen (visible or minimized). False once the user closes it.
    pub fn is_open(&self) -> bool {
        !self.closed && (self.window.isVisible() || self.window.isMiniaturized())
    }

    pub fn is_key(&self) -> bool {
        !self.closed && self.window.isKeyWindow()
    }

    pub fn content_size(&self) -> (f64, f64) {
        let f = self.window.contentLayoutRect();
        (f.size.width, f.size.height)
    }

    pub fn set_content_size(&self, w: f64, h: f64) {
        self.window.setContentSize(NSSize::new(w, h));
    }

    pub fn front(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn window_number(&self) -> isize {
        self.number
    }

    /// Close and forget the window. Safe to call twice.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        OPEN.with(|o| o.borrow_mut().remove(&self.number));
        if let Some(parent) = self.window.parentWindow() {
            parent.removeChildWindow(&self.window);
        }
        self.window.orderOut(None);
        self.window.close();
    }
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// Whether the key window (the one receiving keystrokes) is a plugin editor, so that the app can
/// keep its own shortcuts (undo, copy, delete, space) away from the document while a plugin has focus.
pub fn any_editor_is_key() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let key = NSApplication::sharedApplication(mtm)
        .keyWindow()
        .map(|w| w.windowNumber());
    key.is_some_and(|n| OPEN.with(|o| o.borrow().contains(&n)))
}
