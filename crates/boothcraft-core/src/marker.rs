/// Whether a marker is a single point or a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    Cue,
    Range { len: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub name: String,
    pub at: usize,
    pub kind: MarkerKind,
}

impl Marker {
    pub fn cue(name: impl Into<String>, at: usize) -> Marker {
        Marker {
            name: name.into(),
            at,
            kind: MarkerKind::Cue,
        }
    }

    pub fn range(name: impl Into<String>, at: usize, len: usize) -> Marker {
        Marker {
            name: name.into(),
            at,
            kind: MarkerKind::Range { len },
        }
    }
}
