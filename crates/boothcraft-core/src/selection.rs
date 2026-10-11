use std::ops::Range;

/// A time selection in frames. `start == end` is a cursor position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl Selection {
    pub fn new(a: usize, b: usize) -> Selection {
        Selection {
            start: a.min(b),
            end: a.max(b),
        }
    }

    pub fn cursor(at: usize) -> Selection {
        Selection { start: at, end: at }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    pub fn clamp(&self, frames: usize) -> Selection {
        Selection {
            start: self.start.min(frames),
            end: self.end.min(frames),
        }
    }
}
