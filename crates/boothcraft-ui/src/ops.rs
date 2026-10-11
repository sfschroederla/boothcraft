//! Edit operations on an open file, shared by menus and shortcuts.

use boothcraft_core::{
    ChannelMask, Clipboards, Document, FadeShape, History, Marker, Selection, ZeroDir,
};

/// Edit > Zero Crossings commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZeroAdjust {
    Inward,
    Outward,
    LeftToLeft,
    LeftToRight,
    RightToLeft,
    RightToRight,
}

impl ZeroAdjust {
    pub const ALL: [(&'static str, ZeroAdjust); 6] = [
        ("Adjust Selection Inward", ZeroAdjust::Inward),
        ("Adjust Selection Outward", ZeroAdjust::Outward),
        ("Adjust Left Side to Left", ZeroAdjust::LeftToLeft),
        ("Adjust Left Side to Right", ZeroAdjust::LeftToRight),
        ("Adjust Right Side to Left", ZeroAdjust::RightToLeft),
        ("Adjust Right Side to Right", ZeroAdjust::RightToRight),
    ];
}

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
    /// Width in pixels of the waveform area at its last draw, so panels can report the visible range.
    pub view_width_px: f32,
    /// Options used for the last save, reused by plain Save.
    pub save_options: Option<boothcraft_io::WriteOptions>,
    /// Live, uncommitted audio shown while recording.
    pub preview: Option<Document>,
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
            view_width_px: 0.0,
            save_options: None,
            preview: None,
        }
    }

    /// The document as displayed: a recording in progress if there is one, else the history's current state.
    pub fn doc(&self) -> &Document {
        self.preview
            .as_ref()
            .unwrap_or_else(|| self.history.current())
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

    pub fn commit(&mut self, label: &str, doc: Document) {
        self.preview = None;
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

    /// Insert generated mono audio at the cursor (over the selection if there is one), on every channel.
    pub fn insert_audio(&mut self, label: &str, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut d = self.doc().clone();
        let at = self.selection.start;
        if !self.selection.is_empty() {
            d.delete(self.selection.range(), ChannelMask::ALL);
        }
        let clip = boothcraft_core::clipboard::Clip {
            sample_rate: d.sample_rate,
            channels: vec![boothcraft_core::Rope::from_samples(samples)],
        };
        d.paste(at, &clip);
        self.selection = Selection::new(at, at + samples.len());
        self.commit(label, d);
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

    /// Change sample rate and/or channel count as one undoable edit.
    pub fn convert_sample_type(
        &mut self,
        rate: u32,
        channels: usize,
        downmix: boothcraft_dsp::convert::Downmix,
    ) {
        let d = boothcraft_dsp::convert::resample_document(self.doc(), rate);
        let d = boothcraft_dsp::convert::convert_channels(&d, channels, downmix);
        self.mask = ChannelMask::ALL;
        self.selection = Selection::cursor(0);
        self.view.frames_per_px = 0.0; // refit on next draw
        self.commit("Convert Sample Type", d);
    }

    /// Fade in from the file start, or out to the file end, over `len` frames.
    pub fn fade(&mut self, fade_in: bool, len: usize, shape: FadeShape) {
        if len == 0 {
            return;
        }
        let mut d = self.doc().clone();
        if fade_in {
            d.fade_in(len, shape, self.mask);
        } else {
            d.fade_out(len, shape, self.mask);
        }
        self.commit(if fade_in { "Fade In" } else { "Fade Out" }, d);
    }

    pub fn amplify(&mut self, db: f32) {
        let mut d = self.doc().clone();
        d.amplify(self.effect_range(), self.mask, 10f32.powf(db / 20.0));
        self.commit(&format!("Amplify {db:+.1} dB"), d);
    }

    /// Move selection edges to nearby zero crossings (searching up to one second away).
    /// With only a cursor, it moves to the nearest crossing.
    pub fn adjust_zero_crossings(&mut self, mode: ZeroAdjust) {
        let d = self.doc();
        let (mask, max) = (self.mask, d.sample_rate as usize);
        let find = |f: usize, dir: ZeroDir| d.zero_crossing(f, dir, mask, max).unwrap_or(f);
        let (s, e) = (self.selection.start, self.selection.end);
        if self.selection.is_empty() {
            let at = find(s, ZeroDir::Nearest);
            self.selection = Selection::cursor(at);
            return;
        }
        use ZeroAdjust::*;
        let (ns, ne) = match mode {
            Inward => (find(s, ZeroDir::Right), find(e, ZeroDir::Left)),
            Outward => (find(s, ZeroDir::Left), find(e, ZeroDir::Right)),
            LeftToLeft => (find(s, ZeroDir::Left), e),
            LeftToRight => (find(s, ZeroDir::Right), e),
            RightToLeft => (s, find(e, ZeroDir::Left)),
            RightToRight => (s, find(e, ZeroDir::Right)),
        };
        // Moving inward past each other leaves nothing sensible to select; keep the original.
        if ns <= ne {
            self.selection = Selection::new(ns, ne);
        }
    }

    /// Add a cue at the cursor, or a range marker covering the selection.
    pub fn add_marker(&mut self) {
        let mut d = self.doc().clone();
        let name = d.next_marker_name();
        let sel = self.selection;
        let m = if sel.is_empty() {
            Marker::cue(name, sel.start)
        } else {
            Marker::range(name, sel.start, sel.len())
        };
        d.add_marker(m);
        self.commit("Add Marker", d);
    }

    pub fn remove_marker(&mut self, i: usize) {
        let mut d = self.doc().clone();
        if d.remove_marker(i).is_some() {
            self.commit("Delete Marker", d);
        }
    }

    pub fn rename_marker(&mut self, i: usize, name: &str) {
        if self.doc().markers.get(i).is_none_or(|m| m.name == name) {
            return;
        }
        let mut d = self.doc().clone();
        d.rename_marker(i, name);
        self.commit("Rename Marker", d);
    }

    pub fn move_marker(&mut self, i: usize, at: usize) {
        if self.doc().markers.get(i).is_none_or(|m| m.at == at) {
            return;
        }
        let mut d = self.doc().clone();
        d.move_marker(i, at);
        self.commit("Move Marker", d);
    }

    /// Put the cursor on a cue marker, or select a range marker. Returns the frame to seek to.
    pub fn select_marker(&mut self, i: usize) -> Option<usize> {
        let (at, end) = self.doc().markers.get(i).map(|m| (m.at, m.end()))?;
        self.selection = Selection::new(at, end);
        self.reveal_request = Some(at);
        Some(at)
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
    fn marker_ops_are_undoable() {
        let mut f = file();
        f.selection = Selection::cursor(100);
        f.add_marker();
        f.selection = Selection::new(200, 400);
        f.add_marker();
        assert_eq!(f.doc().markers.len(), 2);
        assert!(f.doc().markers[1].is_range());
        f.rename_marker(0, "Kick");
        f.rename_marker(0, "Kick"); // no-op: no extra history entry
        f.move_marker(0, 900);
        assert_eq!(f.doc().markers[1].name, "Kick");
        assert_eq!(f.select_marker(0), Some(200));
        assert_eq!(f.selection, Selection::new(200, 400));
        f.undo();
        assert_eq!(f.doc().markers[0].name, "Kick");
        f.undo();
        f.undo();
        f.undo();
        assert!(f.doc().markers.is_empty());
    }

    #[test]
    fn zero_crossing_adjustments() {
        // Period 100 frames: crossings at multiples of 50.
        let v: Vec<f32> = (0..1000)
            .map(|i| (i as f32 * std::f32::consts::TAU / 100.0).sin())
            .collect();
        let mut f = OpenFile::new(
            "z".into(),
            None,
            Document::new(48_000, vec![Rope::from_samples(&v)]),
        );
        f.selection = Selection::new(120, 380);
        f.adjust_zero_crossings(ZeroAdjust::Inward);
        assert_eq!(f.selection, Selection::new(150, 350));
        f.selection = Selection::new(120, 380);
        f.adjust_zero_crossings(ZeroAdjust::Outward);
        assert_eq!(f.selection, Selection::new(100, 400));
        f.selection = Selection::new(120, 380);
        f.adjust_zero_crossings(ZeroAdjust::RightToRight);
        assert_eq!(f.selection, Selection::new(120, 400));
        f.selection = Selection::cursor(140);
        f.adjust_zero_crossings(ZeroAdjust::Inward);
        assert_eq!(f.selection, Selection::cursor(150));
    }

    #[test]
    fn convert_sample_type_is_undoable() {
        let mut f = file();
        f.convert_sample_type(24_000, 2, boothcraft_dsp::convert::Downmix::Mix);
        assert_eq!(f.doc().sample_rate, 24_000);
        assert_eq!(f.doc().channel_count(), 2);
        assert_eq!(f.doc().frames(), 500);
        f.undo();
        assert_eq!(f.doc().sample_rate, 48_000);
        assert_eq!(f.doc().channel_count(), 1);
    }

    #[test]
    fn insert_audio_replaces_selection() {
        let mut f = file();
        f.selection = Selection::new(100, 200);
        f.insert_audio("Generate Tones", &[9.0; 50]);
        assert_eq!(f.doc().frames(), 950);
        assert_eq!(f.selection, Selection::new(100, 150));
        assert_eq!(f.doc().channels[0].to_vec(149..151), vec![9.0, 200.0]);
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
