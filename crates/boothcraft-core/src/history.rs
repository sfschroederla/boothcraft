use crate::document::Document;

struct Entry {
    label: String,
    doc: Document,
}

/// Linear undo history of whole-document snapshots (cheap, since blocks are shared).
pub struct History {
    entries: Vec<Entry>,
    cursor: usize,
}

impl History {
    pub fn new(initial: Document) -> History {
        History {
            entries: vec![Entry {
                label: "Original".into(),
                doc: initial,
            }],
            cursor: 0,
        }
    }

    pub fn current(&self) -> &Document {
        &self.entries[self.cursor].doc
    }

    /// Record a new state, discarding any redo branch.
    pub fn commit(&mut self, label: impl Into<String>, doc: Document) {
        self.entries.truncate(self.cursor + 1);
        self.entries.push(Entry {
            label: label.into(),
            doc,
        });
        self.cursor += 1;
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn undo(&mut self) -> bool {
        if self.can_undo() {
            self.cursor -= 1;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.can_redo() {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    /// Jump to any entry (clicking in the History panel).
    pub fn jump(&mut self, index: usize) {
        self.cursor = index.min(self.entries.len() - 1);
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.label.as_str())
    }
}
