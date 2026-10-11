use crate::rope::Rope;

/// Audio held on a clipboard: the selected channels only.
#[derive(Clone, Debug)]
pub struct Clip {
    pub sample_rate: u32,
    pub channels: Vec<Rope>,
}

impl Clip {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Rope::len)
    }
}

/// Five independent clipboards, like Audition's.
#[derive(Debug, Default)]
pub struct Clipboards {
    slots: [Option<Clip>; Clipboards::COUNT],
    pub active: usize,
}

impl Clipboards {
    pub const COUNT: usize = 5;

    pub fn set(&mut self, slot: usize, clip: Clip) {
        self.slots[slot] = Some(clip);
    }

    pub fn get(&self, slot: usize) -> Option<&Clip> {
        self.slots.get(slot).and_then(Option::as_ref)
    }

    pub fn set_active(&mut self, clip: Clip) {
        let a = self.active;
        self.set(a, clip);
    }

    pub fn active_clip(&self) -> Option<&Clip> {
        self.get(self.active)
    }
}
