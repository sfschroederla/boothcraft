//! An in-memory `IBStream`, for plugin state going in and out.

#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

#[derive(Default)]
pub struct MemoryStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl MemoryStream {
    pub fn wrap(data: Vec<u8>) -> (ComWrapper<MemoryStream>, ComPtr<IBStream>) {
        let w = ComWrapper::new(MemoryStream {
            data: RefCell::new(data),
            pos: Cell::new(0),
        });
        let p = w
            .to_com_ptr::<IBStream>()
            .expect("MemoryStream is an IBStream");
        (w, p)
    }

    pub fn empty() -> (ComWrapper<MemoryStream>, ComPtr<IBStream>) {
        MemoryStream::wrap(Vec::new())
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.data.borrow().clone()
    }

    pub fn rewind(&self) {
        self.pos.set(0);
    }
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: i32, num_read: *mut i32) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let n = (num_bytes as usize).min(data.len() - pos);
        std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer as *mut u8, n);
        self.pos.set(pos + n);
        if !num_read.is_null() {
            *num_read = n as i32;
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, num_bytes: i32, num_written: *mut i32) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        let end = pos + num_bytes as usize;
        if data.len() < end {
            data.resize(end, 0);
        }
        std::ptr::copy_nonoverlapping(
            buffer as *const u8,
            data.as_mut_ptr().add(pos),
            num_bytes as usize,
        );
        self.pos.set(end);
        if !num_written.is_null() {
            *num_written = num_bytes;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let target = match mode {
            0 => pos,
            1 => self.pos.get() as i64 + pos,
            2 => len + pos,
            _ => return kInvalidArgument,
        };
        if target < 0 {
            return kInvalidArgument;
        }
        self.pos.set(target as usize);
        if !result.is_null() {
            *result = target;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        *pos = self.pos.get() as i64;
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_seek_read_round_trip() {
        let (w, p) = MemoryStream::empty();
        unsafe {
            let mut data = *b"hello world";
            let mut n = 0;
            assert_eq!(p.write(data.as_mut_ptr() as *mut _, 11, &mut n), kResultOk);
            assert_eq!(n, 11);
            let mut at = 0;
            assert_eq!(p.seek(6, 0, &mut at), kResultOk);
            let mut buf = [0u8; 16];
            assert_eq!(p.read(buf.as_mut_ptr() as *mut _, 16, &mut n), kResultOk);
            assert_eq!(&buf[..n as usize], b"world");
            assert_eq!(p.seek(-5, 2, &mut at), kResultOk);
            assert_eq!(at, 6);
            assert_eq!(p.seek(-100, 1, &mut at), kInvalidArgument);
        }
        assert_eq!(w.bytes(), b"hello world");
    }
}
