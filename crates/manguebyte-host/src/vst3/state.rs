//! A plugin's whole state as one run of bytes.
//!
//! A VST 3 plugin keeps state in two places: its component, which is what
//! makes the sound, and its controller, which is what a person sees. A
//! session has to store both, and has to hand each back to the object it
//! came from. This module packs the two into one run of bytes, with the
//! class they belong to, so that a caller stores one thing:
//!
//! ```text
//! 8 bytes    "MBVST3", a zero byte, the layout version (1)
//! 16 bytes   the class identifier, as four big-endian 32-bit numbers
//! 8 bytes    length of the component's state, little-endian
//! ...        the component's state
//! 8 bytes    length of the controller's state, little-endian
//! ...        the controller's state
//! ```
//!
//! The layout is this host's own. It is not the `.vstpreset` file format.

use super::ClassId;

/// What every state made by this host starts with.
const MAGIC: [u8; 8] = *b"MBVST3\0\x01";

/// Bytes before the component's state.
const HEADER_BYTES: usize = MAGIC.len() + 16 + 8;

/// The parts of a state, borrowed from the bytes they were read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Parts<'a> {
    pub(crate) class: ClassId,
    pub(crate) component: &'a [u8],
    pub(crate) controller: &'a [u8],
}

/// Packs the two halves of a plugin's state.
pub(crate) fn encode(class: ClassId, component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_BYTES + component.len() + 8 + controller.len());
    bytes.extend_from_slice(&MAGIC);
    for word in class.words() {
        bytes.extend_from_slice(&word.to_be_bytes());
    }
    bytes.extend_from_slice(&(component.len() as u64).to_le_bytes());
    bytes.extend_from_slice(component);
    bytes.extend_from_slice(&(controller.len() as u64).to_le_bytes());
    bytes.extend_from_slice(controller);
    bytes
}

/// Takes `count` bytes off the front of `rest`.
fn take<'a>(rest: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    let (front, back) = rest.split_at_checked(count)?;
    *rest = back;
    Some(front)
}

/// Takes a length off the front of `rest`, then that many bytes.
fn take_counted<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let length = u64::from_le_bytes(take(rest, 8)?.try_into().ok()?);
    // A length the bytes cannot hold is refused before anything is done
    // with it, so nothing is ever allocated or skipped on its say-so.
    take(rest, usize::try_from(length).ok()?)
}

/// Unpacks a state, or says what is wrong with it. Does not allocate.
pub(crate) fn decode(bytes: &[u8]) -> Result<Parts<'_>, &'static str> {
    let mut rest = bytes;
    if take(&mut rest, MAGIC.len()) != Some(&MAGIC[..]) {
        return Err("the bytes are not a state saved by this host");
    }
    let class = take(&mut rest, 16).ok_or("the state ends inside its class identifier")?;
    let mut words = [0u32; 4];
    for (word, chunk) in words.iter_mut().zip(class.chunks_exact(4)) {
        *word = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let component = take_counted(&mut rest).ok_or("the state ends inside the component's part")?;
    let controller =
        take_counted(&mut rest).ok_or("the state ends inside the controller's part")?;
    if !rest.is_empty() {
        return Err("the state has bytes after its end");
    }
    Ok(Parts {
        class: ClassId::from_words(words),
        component,
        controller,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS: ClassId =
        ClassId::from_words([0xE3AC_906E, 0x0FE4_AF33, 0x3672_CEE1, 0x1FA3_4823]);

    #[test]
    fn a_state_is_laid_out_byte_for_byte_as_documented() {
        let bytes = encode(CLASS, b"abc", b"de");
        let expected: Vec<u8> = [
            &b"MBVST3\0\x01"[..],
            &[
                0xE3, 0xAC, 0x90, 0x6E, 0x0F, 0xE4, 0xAF, 0x33, 0x36, 0x72, 0xCE, 0xE1, 0x1F, 0xA3,
                0x48, 0x23,
            ],
            &[3, 0, 0, 0, 0, 0, 0, 0],
            b"abc",
            &[2, 0, 0, 0, 0, 0, 0, 0],
            b"de",
        ]
        .concat();
        assert_eq!(bytes, expected);
        assert_eq!(
            decode(&bytes),
            Ok(Parts {
                class: CLASS,
                component: b"abc",
                controller: b"de",
            })
        );
        // Either half, or both, may be empty.
        let empty = encode(CLASS, b"", b"");
        assert_eq!(empty.len(), 40);
        let parts = decode(&empty).unwrap();
        assert!(parts.component.is_empty() && parts.controller.is_empty());
    }

    #[test]
    fn every_truncation_is_an_error() {
        let whole = encode(CLASS, &[7; 33], &[9; 5]);
        assert!(decode(&whole).is_ok());
        for length in 0..whole.len() {
            assert!(decode(&whole[..length]).is_err(), "cut at {length}");
        }
        // And so is anything after the end.
        let mut longer = whole.clone();
        longer.push(0);
        assert_eq!(decode(&longer), Err("the state has bytes after its end"));
    }

    #[test]
    fn every_single_byte_of_damage_is_refused_or_read_within_the_bytes() {
        let whole = encode(CLASS, &[7; 33], &[9; 5]);
        for index in 0..whole.len() {
            for damage in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
                let mut copy = whole.clone();
                if copy[index] == damage {
                    continue;
                }
                copy[index] = damage;
                match decode(&copy) {
                    // Damage to the magic or to a length is always noticed.
                    Err(_) => {}
                    // Damage to the class or to the content cannot be: it
                    // gives another class, or other content, of the same
                    // lengths.
                    Ok(parts) => {
                        assert!(index >= 8, "a damaged magic was accepted");
                        assert_eq!(
                            (parts.component.len(), parts.controller.len()),
                            (33, 5),
                            "damage at {index} changed a length and was accepted"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_length_that_lies_costs_nothing() {
        // A state that claims sixteen exabytes of component.
        let mut lying = Vec::new();
        lying.extend_from_slice(&MAGIC);
        lying.extend_from_slice(&[0; 16]);
        lying.extend_from_slice(&u64::MAX.to_le_bytes());
        lying.extend_from_slice(b"short");
        assert_eq!(
            decode(&lying),
            Err("the state ends inside the component's part")
        );
        // And one whose controller part claims more than is left.
        let mut lying = encode(CLASS, b"ok", b"");
        let at = lying.len() - 8;
        lying[at..].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            decode(&lying),
            Err("the state ends inside the controller's part")
        );
        assert_eq!(
            decode(b"something else entirely"),
            Err("the bytes are not a state saved by this host")
        );
    }
}
