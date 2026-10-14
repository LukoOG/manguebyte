//! Open Sound Control (OSC) for the Manguebyte audio framework.
//!
//! OSC is how control surfaces, sequencers, lighting desks and other
//! programs tell an audio program what to do, usually over a local network.
//! This crate implements OSC 1.0 and the parts of OSC 1.1 that are in use:
//!
//! | Part | Where |
//! |---|---|
//! | Messages, bundles and every argument type | [`OscMessage`], [`OscBundle`], [`OscPacket`], [`OscArg`], [`OscTime`] |
//! | Encoding into a buffer the caller supplies | [`OscPacket::encode`], [`encode_message`], [`BundleEncoder`] |
//! | Strict decoding, owned or borrowed | [`OscPacket::decode`], [`OscPacketRef::parse`] |
//! | Address patterns, with the `//` wildcard of 1.1 | [`pattern`], [`Dispatcher`] |
//! | UDP: unicast, broadcast and multicast | [`UdpSender`], [`UdpReceiver`] |
//! | Streams: the SLIP framing of 1.1, and TCP | [`slip`], [`TcpSender`], [`TcpReceiver`] |
//! | Delivery to the audio thread, on the exact sample | [`bridge`] |
//!
//! # Example
//!
//! Encode a message, and read it back the way a receiver would:
//!
//! ```
//! use manguebyte_osc::{OscArg, OscMessage, OscPacket};
//!
//! let message = OscMessage::new("/oscillator/4/frequency").with(440.0f32);
//! let bytes = message.to_bytes()?;
//! assert_eq!(bytes.len(), 32);
//!
//! let OscPacket::Message(received) = OscPacket::decode(&bytes)? else {
//!     unreachable!()
//! };
//! assert_eq!(received.address, "/oscillator/4/frequency");
//! assert_eq!(received.args, [OscArg::Float(440.0)]);
//! # Ok::<(), manguebyte_osc::OscError>(())
//! ```
//!
//! # Threads
//!
//! Sockets block, and decoding into owned values allocates, so neither
//! belongs on the audio thread. The [`bridge`] is the way across: a network
//! thread checks each packet and puts small events on a wait-free queue,
//! and the audio thread takes them off with the frame each one falls on.
//! Bundles with time tags land on the sample their time tag names.
//!
//! What may be called on the audio thread says so in its documentation:
//! [`BridgeReader::read`], [`OscPacketRef::parse`] and the iterators it
//! gives, [`encode_message`], [`BundleEncoder`], and [`OscTime::now`].
//!
//! # Untrusted input
//!
//! A packet from the network can contain anything. Decoding checks
//! everything the specification states and bounds the rest: a packet that
//! is damaged or built to do harm produces a [`DecodeError`], never a
//! panic, and never more memory than is proportional to its own length.
//! Matching an address pattern takes time bounded by the pattern's length
//! times the address's, whatever the pattern contains. That product is
//! still large for a long pattern tried against many addresses, so the
//! [`bridge`] also limits the matching work one packet may cause, and
//! refuses a packet that would need more: see
//! [`BridgeConfig::match_budget`].
//!
//! # Sources
//!
//! - M. Wright, *The Open Sound Control 1.0 Specification*, 2002.
//! - A. Freed and A. Schmeder, *Features and Future of Open Sound Control
//!   version 1.1 for NIME*, 2009: the `//` wildcard, the stream framing and
//!   the argument types beyond the first four.
//! - RFC 1055, *A Nonstandard for Transmission of IP Datagrams over Serial
//!   Lines: SLIP*.
//! - RFC 5905 and RFC 4330, for the NTP timestamp format a time tag uses
//!   and for reading it after the year 2036.

#![forbid(unsafe_code)]

mod agenda;
pub mod bridge;
mod decode;
mod dispatch;
mod encode;
mod error;
pub mod pattern;
pub mod slip;
mod tcp;
mod time;
mod types;
mod udp;

pub use bridge::{
    BridgeConfig, BridgeCounts, BridgeReader, BridgeStats, BridgeThread, BridgeWriter, OscEvent,
};
pub use decode::{DecodeLimits, OscArgs, OscBundleRef, OscElements, OscMessageRef, OscPacketRef};
pub use dispatch::Dispatcher;
pub use encode::{BundleEncoder, encode_message, encoded_message_len};
pub use error::{DecodeError, EncodeError, OscError};
pub use pattern::{AddressError, PatternError};
pub use slip::{SlipDecoder, SlipError};
pub use tcp::{TcpReceiver, TcpSender};
pub use time::OscTime;
pub use types::{OscArg, OscArgRef, OscBundle, OscColor, OscMessage, OscMidi, OscPacket};
pub use udp::{UdpReceiver, UdpSender};
