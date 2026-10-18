//! Base 64 as RFC 4648 section 4 defines it: the standard alphabet, with
//! padding. The WebSocket handshake needs it for `Sec-WebSocket-Key` and
//! `Sec-WebSocket-Accept` (RFC 6455 section 4).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in base 64, padded with `=` to a multiple of four characters.
pub fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        let digits = chunk.len() + 1;
        for index in 0..4 {
            if index < digits {
                let sextet = (word >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(ALPHABET[sextet as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

fn value_of(character: u8) -> Option<u32> {
    let value = match character {
        b'A'..=b'Z' => character - b'A',
        b'a'..=b'z' => character - b'a' + 26,
        b'0'..=b'9' => character - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

/// The bytes `text` encodes, or `None` if it is not canonical padded base
/// 64: a multiple of four characters of the alphabet, `=` only at the end,
/// and no bits set beyond the last byte (RFC 4648 section 3.5).
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if text.len() % 4 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let quads = text.len() / 4;
    for (index, quad) in text.chunks(4).enumerate() {
        let last = index + 1 == quads;
        let padding = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut word = 0u32;
        for &character in &quad[..4 - padding] {
            word = (word << 6) | value_of(character)?;
        }
        word <<= 6 * padding as u32;
        let produced = 3 - padding;
        // The bits past the last byte must be zero.
        if padding > 0 && word & ((1 << (8 * padding)) - 1) != 0 {
            return None;
        }
        for byte in 0..produced {
            bytes.push((word >> (16 - 8 * byte)) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 section 10.
    const VECTORS: [(&str, &str); 7] = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn the_test_vectors_of_rfc_4648() {
        for (plain, coded) in VECTORS {
            assert_eq!(encode(plain.as_bytes()), coded);
            assert_eq!(decode(coded).unwrap(), plain.as_bytes());
        }
    }

    #[test]
    fn every_byte_value_round_trips() {
        let bytes: Vec<u8> = (0..=255).collect();
        for length in 0..bytes.len() {
            let coded = encode(&bytes[..length]);
            assert_eq!(decode(&coded).unwrap(), &bytes[..length]);
        }
        assert_eq!(&encode(&[0xfb, 0xff])[..3], "+/8");
    }

    #[test]
    fn what_is_not_canonical_is_refused() {
        for bad in [
            "Zg", "Zg=", "Z===", "Zg==Zg==", "Zh==", "Zm9=", "Zm9v!A==", "Zm 9", "====",
        ] {
            assert_eq!(decode(bad), None, "{bad}");
        }
    }
}
