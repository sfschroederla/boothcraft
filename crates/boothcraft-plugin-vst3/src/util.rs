//! Conversions between VST3's fixed-size C strings and ids and Rust types.

#![allow(unsafe_code)]

use std::ffi::c_char;
use vst3::Steinberg::TUID;

/// A class id as 32 upper-case hex digits (the bytes in memory order), the form stored in racks.
pub fn tuid_hex(t: &TUID) -> String {
    t.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

pub fn tuid_from_hex(s: &str) -> Option<TUID> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()? as c_char;
    }
    Some(out)
}

/// A NUL-terminated (or full) `char8` array.
pub fn c_string(a: &[c_char]) -> String {
    let bytes: Vec<u8> = a
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A NUL-terminated (or full) UTF-16 array (`String128`).
pub fn utf16_string(a: &[u16]) -> String {
    let n = a.iter().position(|c| *c == 0).unwrap_or(a.len());
    String::from_utf16_lossy(&a[..n])
}

/// Write `s` into `dst` as NUL-terminated UTF-16, truncating if needed.
pub fn write_utf16(s: &str, dst: &mut [u16]) {
    let mut n = 0;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()) {
        *d = c;
        n += 1;
    }
    if n < dst.len() {
        dst[n] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let t: TUID = [1, -2, 3, -4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, -16];
        let h = tuid_hex(&t);
        assert_eq!(h.len(), 32);
        assert_eq!(tuid_from_hex(&h), Some(t));
        assert_eq!(tuid_from_hex("zz"), None);
        assert_eq!(tuid_from_hex(&"G".repeat(32)), None);
    }

    #[test]
    fn utf16_round_trips_and_truncates() {
        let mut buf = [0xFFFFu16; 6];
        write_utf16("héllo wörld", &mut buf);
        assert_eq!(utf16_string(&buf), "héllo");
        assert_eq!(buf[5], 0);
    }
}
