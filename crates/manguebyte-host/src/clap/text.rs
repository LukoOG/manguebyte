//! Reading text a plugin hands over, without trusting its length.
//!
//! A C string is as long as its author says it is. A plugin that forgets the
//! terminating NUL, or that hands over a megabyte of text where a name was
//! expected, must not make the host read or allocate without limit, so every
//! function here stops after a fixed number of bytes.

use std::ffi::{CStr, c_char};

/// Reads a C string, stopping at its end or after `limit` bytes.
///
/// Bytes that are not UTF-8 are replaced, so the result is always text.
/// Allocates at most `limit` bytes before the conversion.
///
/// # Safety
///
/// `pointer` must be null, or point to memory that can be read up to and
/// including its first NUL byte, or for `limit` bytes if that is shorter.
pub(crate) unsafe fn bounded_text(pointer: *const c_char, limit: usize) -> String {
    let mut bytes = Vec::new();
    if !pointer.is_null() {
        for index in 0..limit {
            // SAFETY: no byte before this one was NUL and `index` is below
            // `limit`, so the contract makes this byte readable.
            let byte = unsafe { pointer.add(index).read() } as u8;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Copies a C string into `out`, stopping at its end or when `out` is full,
/// and returns how many bytes were copied. Does not allocate.
///
/// # Safety
///
/// `pointer` must be null, or point to memory that can be read up to and
/// including its first NUL byte, or for `out.len()` bytes if that is
/// shorter.
pub(crate) unsafe fn copy_bounded(pointer: *const c_char, out: &mut [u8]) -> usize {
    if pointer.is_null() {
        return 0;
    }
    for (index, slot) in out.iter_mut().enumerate() {
        // SAFETY: no byte before this one was NUL and `index` is below
        // `out.len()`, so the contract makes this byte readable.
        let byte = unsafe { pointer.add(index).read() } as u8;
        if byte == 0 {
            return index;
        }
        *slot = byte;
    }
    out.len()
}

/// Whether the C string at `pointer` is exactly `expected`.
///
/// Reads no further than the first byte that differs, so it never runs past
/// the end of a shorter string.
///
/// # Safety
///
/// `pointer` must be null or point to a NUL-terminated string.
pub(crate) unsafe fn text_is(pointer: *const c_char, expected: &CStr) -> bool {
    if pointer.is_null() {
        return false;
    }
    for (index, &wanted) in expected.to_bytes_with_nul().iter().enumerate() {
        // SAFETY: every earlier byte matched a byte of `expected` that was
        // not its terminator, so the string at `pointer` has not ended yet
        // and this byte is part of it.
        if unsafe { pointer.add(index).read() } as u8 != wanted {
            return false;
        }
    }
    true
}

/// The text in a fixed-size field: up to its first NUL, or all of it when
/// the plugin left the NUL out.
pub(crate) fn field_text(field: &[c_char]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .map(|&unit| unit as u8)
        .take_while(|&byte| byte != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &[u8]) -> Vec<c_char> {
        text.iter().map(|&byte| byte as c_char).collect()
    }

    #[test]
    fn text_stops_at_the_terminator_or_at_the_limit() {
        let whole = units(b"mangue\0beat");
        // SAFETY: the buffer is NUL-terminated within the bytes it owns.
        unsafe {
            assert_eq!(bounded_text(whole.as_ptr(), 64), "mangue");
            assert_eq!(bounded_text(whole.as_ptr(), 3), "man");
            assert_eq!(bounded_text(whole.as_ptr(), 0), "");
            assert_eq!(bounded_text(std::ptr::null(), 64), "");
        }
        // No terminator at all: exactly `limit` bytes are read and no more.
        let open = units(b"abcdef");
        // SAFETY: the limit is the length of the buffer.
        assert_eq!(unsafe { bounded_text(open.as_ptr(), open.len()) }, "abcdef");
    }

    #[test]
    fn bytes_that_are_not_text_are_replaced_not_refused() {
        let broken = units(&[b'o', b'k', 0xFF, 0xC3, 0]);
        // SAFETY: the buffer is NUL-terminated.
        let text = unsafe { bounded_text(broken.as_ptr(), 64) };
        assert!(text.starts_with("ok"));
        assert!(text.contains('\u{FFFD}'));
        assert_eq!(field_text(&units(&[0xE2, 0x82])), "\u{FFFD}");
    }

    #[test]
    fn copying_never_writes_past_the_buffer() {
        let source = units(b"twelve bytes\0");
        let mut small = [0u8; 5];
        // SAFETY: the source is NUL-terminated.
        unsafe {
            assert_eq!(copy_bounded(source.as_ptr(), &mut small), 5);
            assert_eq!(&small, b"twelv");
            let mut large = [0u8; 32];
            assert_eq!(copy_bounded(source.as_ptr(), &mut large), 12);
            assert_eq!(&large[..12], b"twelve bytes");
            assert_eq!(copy_bounded(std::ptr::null(), &mut large), 0);
            assert_eq!(copy_bounded(source.as_ptr(), &mut []), 0);
        }
    }

    #[test]
    fn comparison_is_exact() {
        let id = units(b"clap.params\0");
        let longer = units(b"clap.params.more\0");
        let shorter = units(b"clap\0");
        // SAFETY: every buffer is NUL-terminated.
        unsafe {
            assert!(text_is(id.as_ptr(), c"clap.params"));
            assert!(!text_is(longer.as_ptr(), c"clap.params"));
            assert!(!text_is(shorter.as_ptr(), c"clap.params"));
            assert!(!text_is(std::ptr::null(), c"clap.params"));
            assert!(text_is(units(b"\0").as_ptr(), c""));
        }
    }

    #[test]
    fn a_field_without_a_terminator_is_read_whole() {
        assert_eq!(field_text(&units(b"Cutoff\0garbage")), "Cutoff");
        assert_eq!(field_text(&units(b"full")), "full");
        assert_eq!(field_text(&[]), "");
    }
}
