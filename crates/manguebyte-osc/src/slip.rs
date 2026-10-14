//! Framing packets in a byte stream.
//!
//! UDP delivers one packet per datagram. A byte stream, such as TCP or a
//! serial line, has no boundaries, so each packet has to be marked. OSC 1.1
//! does it with SLIP (RFC 1055), with one addition: the END byte goes
//! before each packet as well as after it, so that line noise between two
//! packets cannot be taken for the start of the second.
//!
//! This module is the codec alone: bytes in, bytes out, no sockets.
//! [`TcpSender`](crate::TcpSender) and [`TcpReceiver`](crate::TcpReceiver)
//! put it on a connection.
//!
//! OSC 1.0 marked packets on a stream differently, with a 32-bit size
//! before each one, and some programs still do. That older framing is not
//! implemented.
//!
//! ```
//! use manguebyte_osc::slip::{self, SlipDecoder};
//!
//! let packet = [0x2f, 0x61, 0xc0, 0x00];
//! let mut framed = Vec::new();
//! slip::encode_to_vec(&packet, &mut framed);
//! assert_eq!(framed, [0xc0, 0x2f, 0x61, 0xdb, 0xdc, 0x00, 0xc0]);
//!
//! let mut decoder = SlipDecoder::new(1_024);
//! let mut packets = Vec::new();
//! decoder.feed_all(&framed, |frame| packets.push(frame.unwrap().to_vec()));
//! assert_eq!(packets, [packet.to_vec()]);
//! ```

use std::fmt;

use crate::error::EncodeError;

/// Marks the beginning and the end of a packet.
pub const END: u8 = 0xC0;
/// Announces that the next byte stands for an END or an ESC in the data.
pub const ESC: u8 = 0xDB;
/// After an ESC: the data byte was an END.
pub const ESC_END: u8 = 0xDC;
/// After an ESC: the data byte was an ESC.
pub const ESC_ESC: u8 = 0xDD;

/// What is wrong with a stream of framed packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SlipError {
    /// An ESC was followed by a byte that cannot follow one.
    InvalidEscape {
        /// The byte that was found.
        byte: u8,
    },
    /// A packet is longer than the decoder was built to hold.
    FrameTooLong {
        /// The longest packet the decoder holds.
        max: usize,
    },
}

impl fmt::Display for SlipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEscape { byte } => {
                write!(f, "byte {byte:#04x} cannot follow a SLIP escape")
            }
            Self::FrameTooLong { max } => {
                write!(f, "a framed packet is longer than {max} bytes")
            }
        }
    }
}

impl std::error::Error for SlipError {}

/// Bytes `packet` takes once framed: an END on each side, and one more for
/// every byte that has to be escaped.
pub fn encoded_len(packet: &[u8]) -> usize {
    let escapes = packet
        .iter()
        .filter(|&&byte| byte == END || byte == ESC)
        .count();
    packet.len() + escapes + 2
}

fn write_framed(packet: &[u8], mut put: impl FnMut(u8)) {
    put(END);
    for &byte in packet {
        match byte {
            END => {
                put(ESC);
                put(ESC_END);
            }
            ESC => {
                put(ESC);
                put(ESC_ESC);
            }
            other => put(other),
        }
    }
    put(END);
}

/// Frames `packet` into `out` and returns the number of bytes written.
/// Does not allocate.
///
/// # Errors
///
/// [`EncodeError::BufferTooSmall`], with the size needed, if `out` is
/// shorter than [`encoded_len`] says. Nothing is written in that case.
pub fn encode(packet: &[u8], out: &mut [u8]) -> Result<usize, EncodeError> {
    let needed = encoded_len(packet);
    let target = out
        .get_mut(..needed)
        .ok_or(EncodeError::BufferTooSmall { needed })?;
    let mut slots = target.iter_mut();
    write_framed(packet, |byte| {
        if let Some(slot) = slots.next() {
            *slot = byte;
        }
    });
    Ok(needed)
}

/// Appends `packet`, framed, to `out`. Allocates only if the spare capacity
/// of `out` is smaller than [`encoded_len`] says.
pub fn encode_to_vec(packet: &[u8], out: &mut Vec<u8>) {
    out.reserve(encoded_len(packet));
    write_framed(packet, |byte| out.push(byte));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Inside a packet, or between two.
    Data,
    /// The byte before was an ESC.
    Escape,
    /// The packet being read is damaged. Its remaining bytes are dropped,
    /// up to the next END.
    Skip,
}

/// Takes framed packets out of a byte stream, however the stream is cut
/// into pieces.
///
/// The decoder holds one packet at a time in a buffer that is allocated
/// when it is built and never grows, so a sender cannot make it take more
/// memory than that. Feeding it does not allocate.
#[derive(Debug, Clone)]
pub struct SlipDecoder {
    frame: Vec<u8>,
    max: usize,
    state: State,
    /// `frame` holds a whole packet that has been handed out, and is to be
    /// emptied before the next byte is read.
    complete: bool,
}

impl SlipDecoder {
    /// A decoder for packets of up to `max_frame_len` bytes. Allocates
    /// that many bytes.
    pub fn new(max_frame_len: usize) -> Self {
        Self {
            frame: Vec::with_capacity(max_frame_len),
            max: max_frame_len,
            state: State::Data,
            complete: false,
        }
    }

    /// Reads bytes from the front of `input` until a packet ends, something
    /// is wrong, or the input runs out.
    ///
    /// Returns how many bytes were read, and what happened:
    ///
    /// - `None`: all of `input` was read and no packet has ended yet.
    /// - `Some(Ok(()))`: a packet ended. It is in [`frame`](Self::frame)
    ///   until the next call. Feed the rest of `input` again.
    /// - `Some(Err(_))`: a packet was damaged or too long, and is dropped.
    ///   Decoding picks up at the next END; feed the rest of `input` again.
    ///
    /// An END that ends an empty packet is ignored, which is how the
    /// doubled END between two packets is read.
    pub fn feed(&mut self, input: &[u8]) -> (usize, Option<Result<(), SlipError>>) {
        if self.complete {
            self.frame.clear();
            self.complete = false;
        }
        for (index, &byte) in input.iter().enumerate() {
            let read = index + 1;
            let outcome = match (self.state, byte) {
                (State::Skip, END) => {
                    self.state = State::Data;
                    Ok(false)
                }
                (State::Skip, _) => Ok(false),
                (State::Escape, ESC_END) => self.put(END),
                (State::Escape, ESC_ESC) => self.put(ESC),
                (State::Escape, _) => {
                    // An END here still ends the damaged packet; any other
                    // byte leaves the rest of it to be skipped.
                    self.state = if byte == END {
                        State::Data
                    } else {
                        State::Skip
                    };
                    self.frame.clear();
                    Err(SlipError::InvalidEscape { byte })
                }
                (State::Data, END) => Ok(!self.frame.is_empty()),
                (State::Data, ESC) => {
                    self.state = State::Escape;
                    Ok(false)
                }
                (State::Data, _) => self.put(byte),
            };
            match outcome {
                Ok(false) => {}
                Ok(true) => {
                    self.complete = true;
                    return (read, Some(Ok(())));
                }
                Err(error) => return (read, Some(Err(error))),
            }
        }
        (input.len(), None)
    }

    /// Adds a byte of data to the packet being read. Never says a packet
    /// ended.
    fn put(&mut self, byte: u8) -> Result<bool, SlipError> {
        self.state = State::Data;
        if self.frame.len() >= self.max {
            self.state = State::Skip;
            self.frame.clear();
            return Err(SlipError::FrameTooLong { max: self.max });
        }
        // Below `max`, which is the capacity: this does not allocate.
        self.frame.push(byte);
        Ok(false)
    }

    /// The packet that the last call to [`feed`](Self::feed) completed, or
    /// an empty slice if it completed none.
    pub fn frame(&self) -> &[u8] {
        if self.complete { &self.frame } else { &[] }
    }

    /// Reads all of `input` and calls `handle` for every packet that ends
    /// in it and for every error.
    pub fn feed_all(&mut self, mut input: &[u8], mut handle: impl FnMut(Result<&[u8], SlipError>)) {
        while !input.is_empty() {
            let (read, outcome) = self.feed(input);
            input = input.get(read..).unwrap_or(&[]);
            match outcome {
                Some(Ok(())) => handle(Ok(self.frame())),
                Some(Err(error)) => handle(Err(error)),
                None => {}
            }
        }
    }

    /// Forgets any packet that is partly read, for use when a connection is
    /// replaced by another.
    pub fn reset(&mut self) {
        self.frame.clear();
        self.state = State::Data;
        self.complete = false;
    }
}
