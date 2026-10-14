//! Text and identifiers as VST 3 writes them, read without trusting them.
//!
//! VST 3 hands text over in fixed fields: bytes for a factory's older
//! structs, UTF-16 units for nearly everything else. A field need not hold
//! a terminator and need not hold valid text, so every function here stops
//! at the end of the field or at a fixed limit, and replaces what is not
//! text instead of refusing it.

use std::ffi::c_char;
use std::fmt;
use std::str::FromStr;

use vst3::Steinberg::TUID;

/// The text in a fixed field of bytes: up to its first NUL, or all of it
/// when the plugin left the NUL out.
pub(crate) fn narrow_text(field: &[c_char]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .map(|&unit| unit as u8)
        .take_while(|&byte| byte != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The text in a fixed field of UTF-16 units: up to its first zero, or all
/// of it when the plugin left the zero out. A surrogate without its pair is
/// replaced.
pub(crate) fn wide_text(field: &[u16]) -> String {
    let length = field
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(field.len());
    String::from_utf16_lossy(&field[..length])
}

/// Reads zero-terminated UTF-16 text, stopping at its end or after `limit`
/// units. Allocates at most `limit` units before the conversion.
///
/// # Safety
///
/// `pointer` must be null, or point to units that can be read up to and
/// including the first zero, or for `limit` units if that is shorter.
pub(crate) unsafe fn bounded_wide(pointer: *const u16, limit: usize) -> Vec<u16> {
    let mut units = Vec::new();
    if !pointer.is_null() && pointer.is_aligned() {
        for index in 0..limit {
            // SAFETY: no unit before this one was zero and `index` is below
            // `limit`, so the contract makes this unit readable.
            let unit = unsafe { pointer.add(index).read() };
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
    }
    units
}

/// Reads a C string, stopping at its end or after `limit` bytes.
///
/// # Safety
///
/// `pointer` must be null, or point to memory that can be read up to and
/// including its first NUL byte, or for `limit` bytes if that is shorter.
pub(crate) unsafe fn bounded_narrow(pointer: *const c_char, limit: usize) -> Vec<u8> {
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
    bytes
}

/// Copies `text` into a fixed buffer of UTF-16 units, ending it with a zero.
///
/// Text that does not fit is cut, and never in the middle of a character
/// that takes two units.
pub(crate) fn copy_wide(text: &str, out: &mut [u16]) {
    let Some(room) = out.len().checked_sub(1) else {
        return;
    };
    let mut length = 0;
    for unit in text.encode_utf16().take(room) {
        out[length] = unit;
        length += 1;
    }
    // A leading surrogate left without its pair is not a character.
    if length > 0 && (0xD800..0xDC00).contains(&out[length - 1]) {
        length -= 1;
    }
    out[length] = 0;
}

/// `text` as zero-terminated UTF-16, without any zero it contained.
pub(crate) fn wide_units(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .filter(|&unit| unit != 0)
        .chain([0])
        .collect()
}

/// The 16-byte identifier of a class in a VST 3 module: the number a
/// session stores to find a plugin again.
///
/// VST 3 defines the identifier as four 32-bit numbers and lays their bytes
/// out differently on Windows than elsewhere. The numbers are what stays
/// the same from one system to the next, so they are what this type shows
/// and reads as text: 32 hexadecimal digits, as hosts print them.
///
/// ```
/// use manguebyte_host::vst3::ClassId;
///
/// let id: ClassId = "E3AC906E0FE4AF333672CEE11FA34823".parse().unwrap();
/// assert_eq!(id.words(), [0xE3AC_906E, 0x0FE4_AF33, 0x3672_CEE1, 0x1FA3_4823]);
/// assert_eq!(id.to_string(), "E3AC906E0FE4AF333672CEE11FA34823");
/// assert_eq!(ClassId::from_words(id.words()), id);
/// assert!("not an id".parse::<ClassId>().is_err());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClassId([u8; 16]);

impl ClassId {
    /// The identifier whose bytes, as a module on this system lays them
    /// out in memory, are `bytes`.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The identifier's bytes as a module on this system lays them out in
    /// memory. They differ between Windows and other systems; the
    /// [`words`](Self::words) do not.
    pub const fn bytes(&self) -> [u8; 16] {
        self.0
    }

    /// The identifier made of these four numbers.
    pub const fn from_words(words: [u32; 4]) -> Self {
        let [a, b, c, d] = words;
        let tuid = vst3::uid(a, b, c, d);
        let mut bytes = [0u8; 16];
        let mut index = 0;
        while index < 16 {
            bytes[index] = tuid[index] as u8;
            index += 1;
        }
        Self(bytes)
    }

    /// The four numbers the identifier is made of, which are the same on
    /// every system.
    pub const fn words(&self) -> [u32; 4] {
        /// Four bytes, most significant first, as one number.
        const fn big(b: &[u8; 16], at: usize) -> u32 {
            u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
        }
        let b = &self.0;
        if cfg!(target_os = "windows") {
            // The layout of a Windows GUID: one 32-bit and two 16-bit
            // numbers, least significant byte first, then eight bytes as
            // they come.
            [
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                u32::from_be_bytes([b[5], b[4], b[7], b[6]]),
                big(b, 8),
                big(b, 12),
            ]
        } else {
            [big(b, 0), big(b, 4), big(b, 8), big(b, 12)]
        }
    }

    /// The identifier as the 16 characters the binary interface passes.
    pub(crate) fn tuid(&self) -> TUID {
        self.0.map(|byte| byte as c_char)
    }

    /// The identifier a module wrote into one of its structs.
    pub(crate) fn from_tuid(tuid: &TUID) -> Self {
        Self(tuid.map(|unit| unit as u8))
    }

    /// Whether every byte is zero, which no class has for an identifier.
    pub(crate) fn is_zero(&self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Display for ClassId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.words();
        write!(f, "{a:08X}{b:08X}{c:08X}{d:08X}")
    }
}

impl fmt::Debug for ClassId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClassId({self})")
    }
}

/// The text is not a class identifier: 32 hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseClassIdError;

impl fmt::Display for ParseClassIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a class identifier is 32 hexadecimal digits")
    }
}

impl std::error::Error for ParseClassIdError {}

impl FromStr for ClassId {
    type Err = ParseClassIdError;

    /// Reads 32 hexadecimal digits, in either case.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.as_bytes();
        if digits.len() != 32 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(ParseClassIdError);
        }
        let mut words = [0u32; 4];
        for (word, chunk) in words.iter_mut().zip(digits.chunks(8)) {
            // Eight ASCII hexadecimal digits are valid text and fit.
            let chunk = std::str::from_utf8(chunk).map_err(|_| ParseClassIdError)?;
            *word = u32::from_str_radix(chunk, 16).map_err(|_| ParseClassIdError)?;
        }
        Ok(Self::from_words(words))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &[u8]) -> Vec<c_char> {
        text.iter().map(|&byte| byte as c_char).collect()
    }

    #[test]
    fn a_field_is_read_to_its_terminator_or_its_end() {
        assert_eq!(narrow_text(&units(b"Fx|Delay\0junk")), "Fx|Delay");
        assert_eq!(narrow_text(&units(b"full")), "full");
        assert_eq!(narrow_text(&[]), "");
        assert!(narrow_text(&units(&[b'o', b'k', 0xFF])).starts_with("ok"));

        let wide: Vec<u16> = "Olá\0lixo".encode_utf16().collect();
        assert_eq!(wide_text(&wide), "Olá");
        let open: Vec<u16> = "full".encode_utf16().collect();
        assert_eq!(wide_text(&open), "full");
        assert_eq!(wide_text(&[]), "");
        // Half of a character that takes two units is not a character.
        assert_eq!(wide_text(&[0x61, 0xD83E]), "a\u{FFFD}");
    }

    #[test]
    fn text_behind_a_pointer_stops_at_the_terminator_or_at_the_limit() {
        let wide: Vec<u16> = "mangue\0beat".encode_utf16().collect();
        let narrow = units(b"mangue\0beat");
        // SAFETY: both buffers hold a terminator within the units they own,
        // and the limits passed for the open ones are their lengths.
        unsafe {
            assert_eq!(bounded_wide(wide.as_ptr(), 64).len(), 6);
            assert_eq!(bounded_wide(wide.as_ptr(), 3).len(), 3);
            assert!(bounded_wide(std::ptr::null(), 64).is_empty());
            assert_eq!(bounded_narrow(narrow.as_ptr(), 64), b"mangue");
            assert_eq!(bounded_narrow(narrow.as_ptr(), 2), b"ma");
            assert!(bounded_narrow(std::ptr::null(), 64).is_empty());
            let open: Vec<u16> = "abcdef".encode_utf16().collect();
            assert_eq!(bounded_wide(open.as_ptr(), open.len()).len(), 6);
        }
    }

    #[test]
    fn text_for_the_plugin_is_terminated_and_cut_at_a_character() {
        let wide = |text: &str, room: usize| {
            let mut out = vec![0xFFFF; room];
            copy_wide(text, &mut out);
            out
        };
        assert_eq!(wide("Host", 8)[..5], [72, 111, 115, 116, 0]);
        assert_eq!(wide("Host", 3), [72, 111, 0]);
        assert_eq!(wide("Host", 1), [0]);
        assert!(wide("Host", 0).is_empty());
        assert_eq!(wide("a\u{1F980}", 3), [97, 0, 0xFFFF]);
        assert_eq!(wide_units("a\0b"), [97, 98, 0]);
        assert_eq!(wide_units(""), [0]);
    }

    #[test]
    fn a_class_id_is_the_same_numbers_on_every_system() {
        let words = [0xE3AC_906E, 0x0FE4_AF33, 0x3672_CEE1, 0x1FA3_4823];
        let id = ClassId::from_words(words);
        assert_eq!(id.words(), words);
        assert_eq!(id.to_string(), "E3AC906E0FE4AF333672CEE11FA34823");
        assert_eq!(
            format!("{id:?}"),
            "ClassId(E3AC906E0FE4AF333672CEE11FA34823)"
        );
        assert_eq!("e3ac906e0fe4af333672cee11fa34823".parse(), Ok(id));
        assert_eq!(ClassId::from_bytes(id.bytes()), id);
        assert_eq!(ClassId::from_tuid(&id.tuid()), id);

        // The bytes are in the order each system's modules use.
        let expected: [u8; 16] = if cfg!(target_os = "windows") {
            [
                0x6E, 0x90, 0xAC, 0xE3, 0xE4, 0x0F, 0x33, 0xAF, 0x36, 0x72, 0xCE, 0xE1, 0x1F, 0xA3,
                0x48, 0x23,
            ]
        } else {
            [
                0xE3, 0xAC, 0x90, 0x6E, 0x0F, 0xE4, 0xAF, 0x33, 0x36, 0x72, 0xCE, 0xE1, 0x1F, 0xA3,
                0x48, 0x23,
            ]
        };
        assert_eq!(id.bytes(), expected);

        // Every word survives the trip, whatever its bits.
        for words in [[0; 4], [u32::MAX; 4], [1, 2, 3, 4], [0x0102_0304; 4]] {
            assert_eq!(ClassId::from_words(words).words(), words);
        }
        assert!(ClassId::from_words([0; 4]).is_zero() && !id.is_zero());
    }

    #[test]
    fn text_that_is_not_a_class_id_is_refused() {
        for text in [
            "",
            "E3AC906E0FE4AF333672CEE11FA3482",
            "E3AC906E0FE4AF333672CEE11FA348233",
            "E3AC906E0FE4AF333672CEE11FA3482G",
            "+3AC906E0FE4AF333672CEE11FA34823",
            "E3AC906E-0FE4AF33-3672CEE1-1FA348",
            "\u{e9}3AC906E0FE4AF333672CEE11FA3482",
        ] {
            assert_eq!(text.parse::<ClassId>(), Err(ParseClassIdError), "{text:?}");
        }
    }
}
