//! Plugin editors: each plugin's own interface in a native window, opened from the rack.

use boothcraft_dsp::{SlotDesc, SlotId};
use boothcraft_plugin::{NativeParent, PluginEditor, PluginManager};
use boothcraft_plugin_window::EditorWindow;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

struct Session {
    window: EditorWindow,
    editor: Box<dyn PluginEditor>,
    /// The content size last given to (or reported by) the window, to notice the user resizing it.
    known: (f64, f64),
}

#[derive(Default)]
pub struct PluginEditors {
    sessions: HashMap<SlotId, Session>,
    /// The app's main window content view, so editors float above it. Set every frame.
    pub parent_view: Option<*mut c_void>,
    /// Why a slot has no editor window (shown instead of it, with the generic controls).
    unavailable: HashMap<SlotId, String>,
}

impl PluginEditors {
    pub fn is_open(&self, slot: SlotId) -> bool {
        self.sessions.contains_key(&slot)
    }

    /// The native window number of the slot's open editor (for tooling that targets one window).
    pub fn window_number(&self, slot: SlotId) -> Option<isize> {
        self.sessions.get(&slot).map(|s| s.window.window_number())
    }

    /// A one-line description of an open editor, for developer tooling.
    pub fn describe(&self, slot: SlotId) -> Option<String> {
        let s = self.sessions.get(&slot)?;
        let (pw, ph) = s.editor.size();
        let (cw, ch) = s.window.content_size();
        Some(format!(
            "plugin size {pw}x{ph}, window content {cw:.0}x{ch:.0}, open={}, resizable={}",
            s.window.is_open(),
            s.editor.resizable()
        ))
    }

    /// Why this slot's interface could not be opened, if it could not.
    pub fn unavailable(&self, slot: SlotId) -> Option<&str> {
        self.unavailable.get(&slot).map(String::as_str)
    }

    /// Open (or bring forward) the plugin's own interface for `slot`.
    pub fn open(&mut self, plugins: &mut PluginManager, slot: &SlotDesc, title: &str) {
        if let Some(s) = self.sessions.get(&slot.id) {
            s.window.front();
            return;
        }
        let result = plugins
            .ensure_loaded(slot)
            .map_err(|e| e.to_string())
            .and_then(|()| Self::start(plugins, slot.id, title, self.parent_view));
        match result {
            Ok(s) => {
                self.unavailable.remove(&slot.id);
                self.sessions.insert(slot.id, s);
            }
            Err(why) => {
                self.unavailable.insert(slot.id, why);
            }
        }
    }

    fn start(
        plugins: &mut PluginManager,
        slot: SlotId,
        title: &str,
        parent: Option<*mut c_void>,
    ) -> Result<Session, String> {
        let mut editor = plugins
            .create_editor(slot)
            .map_err(|e| e.to_string())?
            .ok_or("this plugin has no interface of its own")?;
        let (w, h) = editor.size();
        let size = (w as f64, h as f64);
        let mut window = EditorWindow::open(title, size, editor.resizable(), parent)?;
        if let Err(e) = editor.attach(NativeParent::NSView(window.native_view())) {
            window.close();
            return Err(e.to_string());
        }
        // Some plugins settle on a different size once attached.
        let known = window.content_size();
        Ok(Session {
            window,
            editor,
            known,
        })
    }

    /// Per-frame upkeep: forget windows the user closed, and keep window and plugin the same size.
    pub fn frame(&mut self) {
        let mut gone = Vec::new();
        for (slot, s) in &mut self.sessions {
            if !s.window.is_open() {
                gone.push(*slot);
                continue;
            }
            // The plugin asked for a new size: resize the window, then tell the plugin.
            if let Some((w, h)) = s.editor.take_resize_request() {
                s.window.set_content_size(w as f64, h as f64);
                let (w, h) = s.editor.set_size(w, h);
                s.known = (w as f64, h as f64);
                s.window.set_content_size(s.known.0, s.known.1);
                continue;
            }
            // The user dragged the window edge.
            let now = s.window.content_size();
            if (now.0 - s.known.0).abs() > 0.5 || (now.1 - s.known.1).abs() > 0.5 {
                let (w, h) = s
                    .editor
                    .set_size(now.0.round() as u32, now.1.round() as u32);
                s.known = (w as f64, h as f64);
                if (s.known.0 - now.0).abs() > 0.5 || (s.known.1 - now.1).abs() > 0.5 {
                    s.window.set_content_size(s.known.0, s.known.1);
                }
            }
        }
        for slot in gone {
            self.close(slot);
        }
    }

    /// Close one editor. The plugin's view is removed first, while its instance is still alive.
    pub fn close(&mut self, slot: SlotId) {
        if let Some(mut s) = self.sessions.remove(&slot) {
            s.editor.close();
            s.window.close();
            // Dropped here: editor before window, both before the instance can be released.
        }
    }

    /// Close every editor whose slot is no longer in any rack the UI holds.
    pub fn close_except(&mut self, keep: &HashSet<SlotId>) {
        let stale: Vec<SlotId> = self
            .sessions
            .keys()
            .filter(|k| !keep.contains(k))
            .copied()
            .collect();
        for s in stale {
            self.close(s);
        }
        self.unavailable.retain(|k, _| keep.contains(k));
    }

    pub fn close_all(&mut self) {
        let all: Vec<SlotId> = self.sessions.keys().copied().collect();
        for s in all {
            self.close(s);
        }
    }

    /// Whether a plugin editor currently has keyboard focus.
    pub fn any_key(&self) -> bool {
        self.sessions.values().any(|s| s.window.is_key())
    }

    /// Close whichever editor has keyboard focus (Cmd+W). Returns whether there was one.
    pub fn close_key(&mut self) -> bool {
        let key = self
            .sessions
            .iter()
            .find(|(_, s)| s.window.is_key())
            .map(|(k, _)| *k);
        key.map(|k| self.close(k)).is_some()
    }
}
