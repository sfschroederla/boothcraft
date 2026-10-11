//! The host's `IParameterChanges` objects, pooled so the audio thread never allocates.
//!
//! One set is created when a plugin is activated. Each block the processor fills the input set
//! from the queued parameter changes; the plugin may also write to the output set.

#![allow(unsafe_code)]

use std::cell::Cell;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper};

const POINTS: usize = 8;

/// One parameter's changes within a block.
pub struct ValueQueue {
    id: Cell<u32>,
    len: Cell<usize>,
    points: [Cell<(i32, f64)>; POINTS],
}

impl Class for ValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ValueQueue {
    fn new() -> ValueQueue {
        ValueQueue {
            id: Cell::new(0),
            len: Cell::new(0),
            points: std::array::from_fn(|_| Cell::new((0, 0.0))),
        }
    }
}

impl IParamValueQueueTrait for ValueQueue {
    unsafe fn getParameterId(&self) -> u32 {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.len.get() as i32
    }

    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> tresult {
        if index < 0
            || index as usize >= self.len.get()
            || sample_offset.is_null()
            || value.is_null()
        {
            return kInvalidArgument;
        }
        let (o, v) = self.points[index as usize].get();
        *sample_offset = o;
        *value = v;
        kResultOk
    }

    unsafe fn addPoint(&self, sample_offset: i32, value: f64, index: *mut i32) -> tresult {
        let n = self.len.get();
        if n == POINTS {
            return kResultFalse;
        }
        self.points[n].set((sample_offset, value));
        self.len.set(n + 1);
        if !index.is_null() {
            *index = n as i32;
        }
        kResultOk
    }
}

pub struct Changes {
    queues: Vec<ComWrapper<ValueQueue>>,
    used: Cell<usize>,
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl Changes {
    /// A set with room for `capacity` parameters changing in one block.
    pub fn new(capacity: usize) -> ComWrapper<Changes> {
        ComWrapper::new(Changes {
            queues: (0..capacity)
                .map(|_| ComWrapper::new(ValueQueue::new()))
                .collect(),
            used: Cell::new(0),
        })
    }

    pub fn clear(&self) {
        for q in &self.queues[..self.used.get()] {
            q.len.set(0);
        }
        self.used.set(0);
    }

    /// Record `value` (normalized) for parameter `id` at the start of the block. Returns false
    /// if the pool is full.
    pub fn push(&self, id: u32, value: f64) -> bool {
        let used = self.used.get();
        if let Some(q) = self.queues[..used].iter().find(|q| q.id.get() == id) {
            // A second change in one block replaces the first at the same position.
            q.len.set(0);
            q.points[0].set((0, value));
            q.len.set(1);
            return true;
        }
        match self.queues.get(used) {
            Some(q) => {
                q.id.set(id);
                q.points[0].set((0, value));
                q.len.set(1);
                self.used.set(used + 1);
                true
            }
            None => false,
        }
    }

    /// The last value the plugin wrote for `id`, if any (what output parameters will use).
    #[cfg(test)]
    pub fn written(&self, id: u32) -> Option<f64> {
        self.queues[..self.used.get()]
            .iter()
            .find(|q| q.id.get() == id && q.len.get() > 0)
            .map(|q| q.points[q.len.get() - 1].get().1)
    }
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> i32 {
        self.used.get() as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.used.get() {
            return std::ptr::null_mut();
        }
        // Not owned by the caller: the object lives as long as this set.
        self.queues[index as usize]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }

    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        let id = *id;
        let used = self.used.get();
        let at = match self.queues[..used].iter().position(|q| q.id.get() == id) {
            Some(i) => i,
            None => match self.queues.get(used) {
                Some(q) => {
                    q.id.set(id);
                    q.len.set(0);
                    self.used.set(used + 1);
                    used
                }
                None => return std::ptr::null_mut(),
            },
        };
        if !index.is_null() {
            *index = at as i32;
        }
        self.queues[at]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_replaces_and_pool_is_bounded() {
        let c = Changes::new(2);
        assert!(c.push(10, 0.25) && c.push(11, 0.5) && c.push(10, 0.75));
        assert!(!c.push(12, 1.0), "pool full");
        assert_eq!(c.written(10), Some(0.75));
        unsafe {
            assert_eq!(c.getParameterCount(), 2);
            let q = c.getParameterData(0);
            assert!(!q.is_null());
        }
        c.clear();
        assert_eq!(c.written(10), None);
        assert!(c.push(12, 1.0));
    }
}
