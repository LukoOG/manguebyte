//! Finding the executable inside a macOS bundle.
//!
//! On macOS a plugin of either format is a bundle: a folder laid out as
//!
//! ```text
//! Name.clap/  or  Name.vst3/
//!   Contents/
//!     Info.plist        names the executable, among other things
//!     MacOS/
//!       Name            the shared library
//! ```
//!
//! Nothing here calls into the operating system's bundle functions, so the
//! same code runs, and is tested, on every platform: a folder is treated as
//! a bundle wherever it is found.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Most of an `Info.plist` that is read. The key this code looks for sits
/// near the top of a file that is normally a few hundred bytes long.
const MAX_PLIST_BYTES: u64 = 1 << 20;

/// Longest executable name accepted from an `Info.plist`.
const MAX_NAME_BYTES: usize = 255;

/// The executable of the bundle folder `path`, laid out as macOS has it, or
/// `None` if there is none to be found.
///
/// It is the file in `Contents/MacOS` that the bundle's `Info.plist` names;
/// failing that, the one named after the bundle; failing that, the only
/// file there is.
pub(crate) fn mac_executable(path: &Path) -> Option<PathBuf> {
    let contents = path.join("Contents");
    let folder = contents.join("MacOS");

    // The executable the bundle names for itself.
    if let Some(name) = read_limited(&contents.join("Info.plist"))
        .as_deref()
        .and_then(executable_name)
    {
        let candidate = folder.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // By convention the executable is named after the bundle.
    if let Some(stem) = path.file_stem() {
        let candidate = folder.join(stem);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Failing both, a folder that holds exactly one file leaves no doubt.
    only_file(&folder, None)
}

/// The one file in `folder`, if it holds exactly one. With `extension`,
/// only files that end in it are counted.
pub(crate) fn only_file(folder: &Path, extension: Option<&str>) -> Option<PathBuf> {
    let mut files = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_file())
        .filter(|candidate| {
            extension.is_none_or(|wanted| {
                candidate
                    .extension()
                    .is_some_and(|found| found.eq_ignore_ascii_case(wanted))
            })
        });
    match (files.next(), files.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Reads at most [`MAX_PLIST_BYTES`] of a file, or nothing if it cannot be
/// opened.
fn read_limited(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_PLIST_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// The value of `CFBundleExecutable` in an XML property list.
///
/// This is a search for one key and the string that follows it, not an XML
/// parser: that is all a bundle needs, and it keeps malformed input from
/// reaching anything complicated. A binary property list, a missing key or
/// a damaged file gives `None`, and the caller falls back on the bundle's
/// name.
///
/// A name that could lead outside the bundle's own folder is refused: it
/// must be a single path component.
pub(crate) fn executable_name(plist: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"<key>CFBundleExecutable</key>";
    const OPEN: &[u8] = b"<string>";
    const CLOSE: &[u8] = b"</string>";

    let after_key = &plist[find(plist, KEY)? + KEY.len()..];
    let blank = after_key
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    let value = after_key[blank..].strip_prefix(OPEN)?;
    let raw = &value[..find(value, CLOSE)?];
    if raw.len() > MAX_NAME_BYTES * 6 {
        return None;
    }

    let name = unescape(std::str::from_utf8(raw).ok()?)?;
    let safe = !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0']);
    safe.then_some(name)
}

/// Where `needle` first occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Replaces the five entities XML defines. Anything else that starts with
/// an ampersand is not a name this code will guess at.
fn unescape(text: &str) -> Option<String> {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        plain.push_str(&rest[..at]);
        let entity = &rest[at..];
        let (character, length) = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .iter()
        .find(|(name, _)| entity.starts_with(name))
        .map(|&(name, character)| (character, name.len()))?;
        plain.push(character);
        rest = &entity[length..];
    }
    plain.push_str(rest);
    Some(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Mangue Pluck</string>
    <key>CFBundleExecutable</key>
    <string>Mangue Pluck</string>
    <key>CFBundlePackageType</key>
    <string>BNDL</string>
</dict>
</plist>
"#;

    #[test]
    fn finds_the_executable_name() {
        assert_eq!(
            executable_name(PLIST.as_bytes()).as_deref(),
            Some("Mangue Pluck")
        );
        // The key and its value need not be on separate lines.
        let tight = b"<key>CFBundleExecutable</key><string>a&amp;b</string>";
        assert_eq!(executable_name(tight).as_deref(), Some("a&b"));
        let entities = b"<key>CFBundleExecutable</key>\t<string>&lt;&gt;&quot;&apos;</string>";
        assert_eq!(executable_name(entities).as_deref(), Some("<>\"'"));
    }

    #[test]
    fn every_truncation_of_a_plist_is_handled() {
        let whole = PLIST.as_bytes();
        // The name is known once the string that follows the key has closed.
        let key_at = PLIST.find("<key>CFBundleExecutable").unwrap();
        let complete_at = key_at + PLIST[key_at..].find("</string>").unwrap() + "</string>".len();
        for length in 0..=whole.len() {
            let name = executable_name(&whole[..length]);
            if length >= complete_at {
                assert_eq!(name.as_deref(), Some("Mangue Pluck"), "cut at {length}");
            } else {
                // Cut anywhere before the value closes, there is no name:
                // a partial one is never returned.
                assert_eq!(name, None, "cut at {length}");
            }
        }
    }

    #[test]
    fn every_single_byte_of_damage_is_handled() {
        let whole = PLIST.as_bytes();
        for index in 0..whole.len() {
            for damage in [0x00, 0xFF, b'<', b'&', b'/'] {
                let mut copy = whole.to_vec();
                copy[index] = damage;
                // The only requirement is that this returns: a name or none.
                if let Some(name) = executable_name(&copy) {
                    assert!(!name.is_empty() && !name.contains(['/', '\\', '\0']));
                }
            }
        }
    }

    #[test]
    fn names_that_leave_the_bundle_are_refused() {
        let with = |value: &str| {
            executable_name(
                format!("<key>CFBundleExecutable</key><string>{value}</string>").as_bytes(),
            )
        };
        assert_eq!(with("plugin").as_deref(), Some("plugin"));
        for hostile in [
            "",
            ".",
            "..",
            "../../../etc/passwd",
            "/bin/sh",
            "..\\evil",
            "C:evil",
            "a/b",
            "&unknown;",
            "&#47;bin",
        ] {
            assert_eq!(with(hostile), None, "accepted {hostile:?}");
        }
        assert_eq!(
            with(&"x".repeat(MAX_NAME_BYTES)).map(|name| name.len()),
            Some(255)
        );
        assert_eq!(with(&"x".repeat(MAX_NAME_BYTES + 1)), None);
        assert_eq!(with(&"&amp;".repeat(MAX_NAME_BYTES * 2)), None);
    }

    #[test]
    fn other_formats_and_other_keys_give_nothing() {
        assert_eq!(executable_name(b""), None);
        assert_eq!(executable_name(b"bplist00\xd1\x01\x02"), None);
        assert_eq!(executable_name(&[0xFF; 64]), None);
        // The value must be a string, directly after the key.
        assert_eq!(
            executable_name(b"<key>CFBundleExecutable</key><integer>3</integer>"),
            None
        );
        assert_eq!(
            executable_name(b"<key>CFBundleExecutable</key><string>never closed"),
            None
        );
        assert_eq!(
            executable_name(b"<key>CFBundleExecutable</key><string>\xff\xfe</string>"),
            None
        );
    }
}
