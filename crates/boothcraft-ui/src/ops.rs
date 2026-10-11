//! Edit operations on an open file, shared by menus and shortcuts.

use boothcraft_core::{ChannelMask, Clipboards, Document, History, Selection};

/// An open file with its edit history and view state.
pub struct OpenFile {
    pub name: String,
    pub path: Option<std::path::PathBuf>,
    pub history: History,
    pub selection: Selection,
    pub mask: ChannelMask,
    pub view: crate::view::View,
    /// History index at last save, for the dirty marker.
    pub saved_at: usize,
    /// Bumped on every change to the visible document, so the engine knows when to resync.
    pub revision: u64,
    /// A frame the waveform should scroll into view on its next draw.
    pub reveal_request: Option<usize>,
}

impl OpenFile {
    pub fn new(name: String, path: Option<std::path::PathBuf>, doc: Document) -> OpenFile {
        OpenFile {
            name,
            path,
            history: History::new(doc),
            selection: Selection::default(),
            mask: ChannelMask::ALL,
            view: crate::view::View {
                start: 0.0,
                frames_per_px: 1.0,
                amp: 1.0,
            },
            saved_at: 0,
            revision: 0,
            reveal_request: None,
        }
    }

    pub fn doc(&self) -> &Document {
        self.history.current()
    }

    pub fn dirty(&self) -> bool {
        self.history.cursor() != self.saved_at
    }

    /// Range an effect-like edit applies to: the selection, or everything if there is none.
    fn effect_range(&self) -> std::ops::Range<usize> {
        if self.selection.is_empty() {
            0..self.doc().frames()
        } else {
            self.selection.range()
        }
    }

    fn commit(&mut self, label: &str, doc: Document) {
        self.history.commit(label, doc);
        self.revision += 1;
        self.selection = self.selection.clamp(self.history.current().frames());
    }

    pub fn select_all(&mut self) {
        self.selection = Selection::new(0, self.doc().frames());
    }

    pub fn copy(&self, clips: &mut Clipboards) {
        if !self.selection.is_empty() {
            clips.set_active(self.doc().copy(self.selection.range(), self.mask));
        }
    }

    pub fn delete(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let mut d = self.doc().clone();
        d.delete(self.selection.range(), self.mask);
        if self.mask.is_all(d.channel_count()) {
            self.selection = Selection::cursor(self.selection.start);
        }
        self.commit("Delete", d);
    }

    pub fn cut(&mut self, clips: &mut Clipboards) {
        if self.selection.is_empty() {
            return;
        }
        self.copy(clips);
        self.delete();
    }

    /// Paste at the cursor, replacing the selection if there is one.
    pub fn paste(&mut self, clips: &Clipboards) {
        let Some(clip) = clips.active_clip() else {
            return;
        };
        let mut d = self.doc().clone();
        let at = self.selection.start;
        if !self.selection.is_empty() {
            d.delete(self.selection.range(), ChannelMask::ALL);
        }
        d.paste(at, clip);
        self.selection = Selection::new(at, at + clip.frames());
        self.commit("Paste", d);
    }

    pub fn mix_paste(&mut self, clips: &Clipboards) {
        let Some(clip) = clips.active_clip() else {
            return;
        };
        let mut d = self.doc().clone();
        let at = self.selection.start;
        d.mix_paste(at, clip, 1.0);
        self.selection = Selection::new(at, at + clip.frames());
        self.commit("Mix Paste", d);
    }

    pub fn crop(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let mut d = self.doc().clone();
        d.crop(self.selection.range());
        self.selection = Selection::new(0, d.frames());
        self.commit("Crop", d);
    }

    pub fn insert_silence(&mut self, seconds: f64) {
        let mut d = self.doc().clone();
        let n = (seconds * d.sample_rate as f64).round() as usize;
        let at = self.selection.start;
        d.insert_silence(at, n);
        self.selection = Selection::new(at, at + n);
        self.commit("Insert Silence", d);
    }

    pub fn silence(&mut self) {
        let mut d = self.doc().clone();
        d.silence(self.effect_range(), self.mask);
        self.commit("Silence", d);
    }

    pub fn invert(&mut self) {
        let mut d = self.doc().clone();
        d.invert(self.effect_range(), self.mask);
        self.commit("Invert", d);
    }

    pub fn reverse(&mut self) {
        let mut d = self.doc().clone();
        d.reverse(self.effect_range(), self.mask);
        self.commit("Reverse", d);
    }

    pub fn amplify(&mut self, db: f32) {
        let mut d = self.doc().clone();
        d.amplify(self.effect_range(), self.mask, 10f32.powf(db / 20.0));
        self.commit(&format!("Amplify {db:+.1} dB"), d);
    }

    pub fn undo(&mut self) {
        self.history.undo();
        self.revision += 1;
        self.selection = self.selection.clamp(self.doc().frames());
    }

    pub fn redo(&mut self) {
        self.history.redo();
        self.revision += 1;
        self.selection = self.selection.clamp(self.doc().frames());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boothcraft_core::Rope;

    fn file() -> OpenFile {
        let v: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        OpenFile::new(
            "t".into(),
            None,
            Document::new(48_000, vec![Rope::from_samples(&v)]),
        )
    }

    #[test]
    fn cut_paste_undo() {
        let mut f = file();
        let mut clips = Clipboards::default();
        f.selection = Selection::new(100, 300);
        f.cut(&mut clips);
        assert_eq!(f.doc().frames(), 800);
        assert!(f.dirty());
        f.selection = Selection::cursor(0);
        f.paste(&clips);
        assert_eq!(f.doc().frames(), 1000);
        assert_eq!(f.doc().channels[0].to_vec(0..1)[0], 100.0);
        f.undo();
        f.undo();
        assert_eq!(f.doc().frames(), 1000);
        assert!(!f.dirty());
    }

    #[test]
    fn effects_default_to_whole_file() {
        let mut f = file();
        f.invert();
        assert_eq!(f.doc().channels[0].to_vec(5..6)[0], -5.0);
        f.selection = Selection::new(0, 10);
        f.silence();
        assert_eq!(f.doc().channels[0].to_vec(5..6)[0], 0.0);
        assert_eq!(f.doc().channels[0].to_vec(20..21)[0], -20.0);
    }
}
