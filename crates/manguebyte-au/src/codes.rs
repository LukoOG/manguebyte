//! The four-character codes and the version number an Audio Unit is known
//! by.
//!
//! An Audio Unit is found by three codes: its type, which says what kind of
//! unit it is, its subtype, and its manufacturer. Hosts store the three in
//! sessions, so they must never change once a version has shipped. Like the
//! VST 3 class id, they are worked out from the plugin's id, by hashing,
//! and are the same on every machine.

use manguebyte_plugin::{PluginInfo, PluginKind};

/// A four-character code, as Apple's `OSType` writes it: four bytes, the
/// first in the high byte.
pub type FourCharCode = [u8; 4];

/// The code as the number Apple's interfaces take.
pub const fn as_os_type(code: FourCharCode) -> u32 {
    u32::from_be_bytes(code)
}

/// The component type for a kind of plugin.
///
/// | Kind | Type | |
/// |---|---|---|
/// | Instrument | `aumu` | a music device: notes in, audio out |
/// | Effect, analyzer | `aufx` | an effect: audio in, audio out |
/// | Note effect | `aumf` | a music effect: audio and notes in, audio out |
///
/// A note effect that makes notes for other plugins would be a MIDI
/// processor (`aumi`), which needs MIDI output; the wrapper has none yet.
pub const fn component_type(kind: PluginKind) -> FourCharCode {
    match kind {
        PluginKind::Instrument => *b"aumu",
        PluginKind::Effect | PluginKind::Analyzer => *b"aufx",
        PluginKind::NoteEffect => *b"aumf",
    }
}

/// 32-bit FNV-1a (Fowler, Noll and Vo), from a given starting point.
const fn fnv1a_32(bytes: &[u8], basis: u32) -> u32 {
    let mut hash = basis;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        index += 1;
    }
    hash
}

const LOWER: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
const UPPER: &[u8; 26] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER_AND_DIGITS: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Four characters from a hash: the first from `first`, the others from
/// lower-case letters and digits.
const fn spell(mut hash: u32, first: &[u8; 26]) -> FourCharCode {
    let mut code = [0u8; 4];
    code[0] = first[(hash % 26) as usize];
    hash /= 26;
    let mut index = 1;
    while index < 4 {
        code[index] = LOWER_AND_DIGITS[(hash % 36) as usize];
        hash /= 36;
        index += 1;
    }
    code
}

/// The part of a reverse-domain id before its last dot: the vendor's part,
/// `org.manguebyte` of `org.manguebyte.pluck`. The whole id if it has no
/// dot.
const fn vendor_part(id: &[u8]) -> usize {
    let mut end = id.len();
    while end > 0 {
        end -= 1;
        if id[end] == b'.' {
            return end;
        }
    }
    id.len()
}

/// The subtype of the plugin with this id: a lower-case letter, then three
/// lower-case letters or digits, from a hash of the whole id.
///
/// ```
/// use manguebyte_au::subtype_code;
///
/// let code = subtype_code("org.manguebyte.pluck");
/// assert_eq!(code, subtype_code("org.manguebyte.pluck"));
/// assert_ne!(code, subtype_code("org.manguebyte.duck"));
/// assert!(code[0].is_ascii_lowercase());
/// ```
pub const fn subtype_code(id: &str) -> FourCharCode {
    spell(fnv1a_32(id.as_bytes(), 0x811C_9DC5), LOWER)
}

/// The manufacturer of the plugin with this id: an upper-case letter, then
/// three lower-case letters or digits, from a hash of the vendor's part of
/// the id, so that every plugin of one vendor has the same manufacturer.
/// Apple keeps the codes made of lower-case letters alone for itself, and
/// the upper-case first letter keeps clear of them.
///
/// ```
/// use manguebyte_au::manufacturer_code;
///
/// let code = manufacturer_code("org.manguebyte.pluck");
/// assert_eq!(code, manufacturer_code("org.manguebyte.duck"));
/// assert_ne!(code, manufacturer_code("com.example.pluck"));
/// assert!(code[0].is_ascii_uppercase());
/// ```
pub const fn manufacturer_code(id: &str) -> FourCharCode {
    let bytes = id.as_bytes();
    let end = vendor_part(bytes);
    let (vendor, _) = bytes.split_at(end);
    // Another starting point than the subtype's, so that the two codes of a
    // one-word id differ.
    spell(fnv1a_32(vendor, 0x050C_5D1F), UPPER)
}

/// The three codes of a plugin, in the order `auval` takes them.
pub const fn codes(info: &PluginInfo) -> [FourCharCode; 3] {
    [
        component_type(info.kind),
        subtype_code(info.id),
        manufacturer_code(info.id),
    ]
}

/// The code as text, for `Info.plist` and messages. Bytes that are not
/// printable ASCII, which the codes made here never have, show as `?`.
pub fn code_text(code: FourCharCode) -> String {
    code.iter()
        .map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '?'
            }
        })
        .collect()
}

/// The version as an Audio Unit gives it: `0xMMMMmmbb`, major in the high
/// sixteen bits, then minor and patch in eight each, as Apple's
/// `AudioComponents` entries and `kAudioUnitProperty_ClassInfo` take it.
/// Numbers that do not fit are held to the largest that does; a missing or
/// unreadable one is zero; a pre-release or build suffix is ignored.
///
/// ```
/// use manguebyte_au::version_number;
///
/// assert_eq!(version_number("1.2.3"), 0x0001_0203);
/// assert_eq!(version_number("0.1.0"), 0x0000_0100);
/// assert_eq!(version_number("2.0.0-beta.1"), 0x0002_0000);
/// assert_eq!(version_number("70000.300.7"), 0xFFFF_FF07);
/// ```
pub fn version_number(version: &str) -> u32 {
    let release = version.split(['-', '+']).next().unwrap_or_default();
    let mut numbers = release.split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            0
        } else {
            digits.parse::<u64>().unwrap_or(u64::MAX)
        }
    });
    let major = numbers.next().unwrap_or(0).min(0xFFFF) as u32;
    let minor = numbers.next().unwrap_or(0).min(0xFF) as u32;
    let patch = numbers.next().unwrap_or(0).min(0xFF) as u32;
    (major << 16) | (minor << 8) | patch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_printable_and_kept_apart() {
        let ids = [
            "org.manguebyte.pluck",
            "org.manguebyte.duck",
            "org.manguebyte.test.keys",
            "com.example.gain",
            "gain",
            "",
            "a.b.c.d.e",
            "ünïcode.ïd",
        ];
        let mut subtypes = Vec::new();
        for id in ids {
            let subtype = subtype_code(id);
            let manufacturer = manufacturer_code(id);
            for code in [subtype, manufacturer] {
                assert!(code.iter().all(|byte| byte.is_ascii_alphanumeric()), "{id}");
                assert_eq!(code_text(code).len(), 4);
            }
            assert!(subtype[0].is_ascii_lowercase());
            assert!(manufacturer[0].is_ascii_uppercase());
            assert!(!subtypes.contains(&subtype), "{id}");
            subtypes.push(subtype);
        }
        // Worked out at compile time, the same as at run time.
        const PLUCK: FourCharCode = subtype_code("org.manguebyte.pluck");
        assert_eq!(PLUCK, subtype_code("org.manguebyte.pluck"));
        // The vendor's part decides the manufacturer.
        assert_eq!(vendor_part(b"org.manguebyte.pluck"), 14);
        assert_eq!(vendor_part(b"plain"), 5);
        assert_eq!(
            manufacturer_code("org.manguebyte.a"),
            manufacturer_code("org.manguebyte.b")
        );
    }

    #[test]
    fn the_codes_of_the_example_plugins_never_change() {
        // Hosts keep these in sessions: a change here breaks every session
        // that uses the plugins.
        assert_eq!(code_text(subtype_code("org.manguebyte.pluck")), "h4yl");
        assert_eq!(code_text(subtype_code("org.manguebyte.duck")), "l4tw");
        assert_eq!(code_text(manufacturer_code("org.manguebyte.duck")), "Ogvj");
        assert_eq!(as_os_type(*b"aufx"), 0x6175_6678);
    }

    #[test]
    fn kinds_have_their_types() {
        assert_eq!(component_type(PluginKind::Instrument), *b"aumu");
        assert_eq!(component_type(PluginKind::Effect), *b"aufx");
        assert_eq!(component_type(PluginKind::Analyzer), *b"aufx");
        assert_eq!(component_type(PluginKind::NoteEffect), *b"aumf");
        assert_eq!(version_number(""), 0);
        assert_eq!(version_number("x.y"), 0);
        assert_eq!(version_number("3"), 0x0003_0000);
        assert_eq!(version_number("1.2.3+build.9"), 0x0001_0203);
    }
}
