//! Typed errors for encoding, decoding and transport.

use std::fmt;
use std::io;

use crate::slip::SlipError;

/// Why a packet could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// The buffer is too small for the packet.
    BufferTooSmall {
        /// The number of bytes the packet takes.
        needed: usize,
    },
    /// An address does not begin with `/`.
    InvalidAddress,
    /// An address or a string contains a NUL character. An OSC string ends
    /// at its first NUL, so such a string cannot be sent.
    NulInString,
    /// A blob or a bundle element is larger than a 32-bit size can state.
    TooLarge,
    /// The array markers in a flat argument list do not pair up.
    UnbalancedArray,
    /// A bundle was closed that was never opened, or was still open when
    /// the encoder finished.
    UnbalancedBundle,
    /// More bundles are open inside each other than
    /// [`BundleEncoder`](crate::BundleEncoder) supports.
    TooDeep,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall { needed } => {
                write!(
                    f,
                    "the buffer is too small: the packet takes {needed} bytes"
                )
            }
            Self::InvalidAddress => f.write_str("an OSC address must begin with '/'"),
            Self::NulInString => f.write_str("an OSC string cannot contain a NUL character"),
            Self::TooLarge => f.write_str("a blob or bundle element is larger than 2 GiB"),
            Self::UnbalancedArray => f.write_str("array markers do not pair up"),
            Self::UnbalancedBundle => f.write_str("bundles were not opened and closed in pairs"),
            Self::TooDeep => f.write_str("too many bundles inside each other"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Why a packet could not be decoded.
///
/// Every `offset` is a byte position counted from the start of the packet
/// that was handed to the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The packet's length is not a multiple of four.
    Misaligned {
        /// The length that was given.
        len: usize,
    },
    /// A packet or bundle element has no contents.
    Empty {
        /// Where the contents should have begun.
        offset: usize,
    },
    /// The contents begin with neither `/` (a message) nor `#` (a bundle).
    UnknownPacket {
        /// The first byte that was found.
        first: u8,
        /// Where it was found.
        offset: usize,
    },
    /// The contents begin with `#` but not with `#bundle`.
    InvalidBundleHeader {
        /// Where the header begins.
        offset: usize,
    },
    /// The data ends in the middle of a value.
    Truncated {
        /// Where the data ends.
        offset: usize,
    },
    /// A string has no terminating NUL.
    UnterminatedString {
        /// Where the string begins.
        offset: usize,
    },
    /// A padding byte is not zero.
    NonZeroPadding {
        /// Where the padding begins.
        offset: usize,
    },
    /// A string is not valid UTF-8.
    InvalidString {
        /// Where the string begins.
        offset: usize,
    },
    /// A message has data after its address that is not a type tag string.
    MissingTypeTags {
        /// Where the type tag string should have begun.
        offset: usize,
    },
    /// The type tag string contains a tag this crate does not know.
    UnknownTypeTag {
        /// The tag.
        tag: u8,
        /// Its position in the type tag string, not counting the comma.
        index: usize,
    },
    /// The array markers in the type tag string do not pair up.
    UnbalancedArray,
    /// Arrays are nested more deeply than the limit allows.
    ArrayTooDeep {
        /// The limit that was applied.
        max: usize,
    },
    /// A blob's size is negative or reaches past the end of the message.
    InvalidBlobSize {
        /// The size as it was stated.
        size: i32,
        /// Where the blob begins.
        offset: usize,
    },
    /// A `c` argument is not a Unicode scalar value.
    InvalidChar {
        /// The value that was found.
        value: u32,
        /// Where it was found.
        offset: usize,
    },
    /// Bytes are left over after the last argument.
    TrailingData {
        /// Where the surplus begins.
        offset: usize,
    },
    /// A bundle element's size is negative, not a multiple of four, or
    /// reaches past the end of the bundle.
    InvalidElementSize {
        /// The size as it was stated.
        size: i32,
        /// Where the size was found.
        offset: usize,
    },
    /// Bundles are nested more deeply than the limit allows.
    BundleTooDeep {
        /// The limit that was applied.
        max: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Misaligned { len } => {
                write!(f, "packet length {len} is not a multiple of four")
            }
            Self::Empty { offset } => write!(f, "empty packet at byte {offset}"),
            Self::UnknownPacket { first, offset } => write!(
                f,
                "byte {offset} is {first:#04x}: neither a message nor a bundle"
            ),
            Self::InvalidBundleHeader { offset } => {
                write!(f, "malformed bundle header at byte {offset}")
            }
            Self::Truncated { offset } => write!(f, "data ends inside a value at byte {offset}"),
            Self::UnterminatedString { offset } => {
                write!(f, "string at byte {offset} has no terminating NUL")
            }
            Self::NonZeroPadding { offset } => {
                write!(f, "padding at byte {offset} is not zero")
            }
            Self::InvalidString { offset } => {
                write!(f, "string at byte {offset} is not valid UTF-8")
            }
            Self::MissingTypeTags { offset } => {
                write!(f, "no type tag string at byte {offset}")
            }
            Self::UnknownTypeTag { tag, index } => {
                write!(f, "unknown type tag {tag:#04x} at position {index}")
            }
            Self::UnbalancedArray => f.write_str("array markers do not pair up"),
            Self::ArrayTooDeep { max } => {
                write!(f, "arrays are nested more than {max} deep")
            }
            Self::InvalidBlobSize { size, offset } => {
                write!(
                    f,
                    "blob at byte {offset} states an impossible size of {size}"
                )
            }
            Self::InvalidChar { value, offset } => {
                write!(f, "character {value:#x} at byte {offset} is not valid")
            }
            Self::TrailingData { offset } => {
                write!(
                    f,
                    "unexpected data after the last argument at byte {offset}"
                )
            }
            Self::InvalidElementSize { size, offset } => write!(
                f,
                "bundle element at byte {offset} states an impossible size of {size}"
            ),
            Self::BundleTooDeep { max } => {
                write!(f, "bundles are nested more than {max} deep")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// What went wrong while sending or receiving.
#[derive(Debug)]
#[non_exhaustive]
pub enum OscError {
    /// The socket reported an error.
    Io(io::Error),
    /// A packet could not be encoded.
    Encode(EncodeError),
    /// What arrived is not a valid packet.
    Decode(DecodeError),
    /// A byte stream is not framed correctly.
    Frame(SlipError),
}

impl fmt::Display for OscError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Encode(error) => write!(f, "{error}"),
            Self::Decode(error) => write!(f, "{error}"),
            Self::Frame(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for OscError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Frame(error) => Some(error),
        }
    }
}

impl From<io::Error> for OscError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<EncodeError> for OscError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}

impl From<DecodeError> for OscError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<SlipError> for OscError {
    fn from(error: SlipError) -> Self {
        Self::Frame(error)
    }
}
