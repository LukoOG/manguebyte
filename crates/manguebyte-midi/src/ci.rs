//! MIDI Capability Inquiry (MIDI-CI): devices that find each other and
//! agree on what they can do.
//!
//! MIDI-CI is a conversation in universal system exclusive messages
//! between an *initiator*, which asks, and a *responder*, which answers.
//! Each side names itself with a random 28-bit [`Muid`]. The conversation
//! covers:
//!
//! - **Discovery**: who is there, made by whom, able to do what, and how
//!   large a system exclusive message it can take.
//! - **Profile configuration**: turning on and off *profiles*, agreed sets
//!   of behaviour (a drawbar organ, MPE, a mixer), on a channel, a group or
//!   a whole function block.
//! - **Property exchange**: getting and setting a device's properties,
//!   JSON documents carried in chunks, such as its list of programs.
//! - **Process inquiry**: asking a device what state its MIDI is in.
//! - **Protocol negotiation**, deprecated by version 1.2 in favour of the
//!   UMP stream messages of [`ump`](crate::ump): read and written here for
//!   older devices, never started.
//!
//! This module is the message layer: [`parse`] reads one message from the
//! bytes of a system exclusive message, and [`encode`] writes one. The
//! [`agent`] module holds a small state machine that answers and asks on
//! an application's behalf, and [`property`] cuts property data into
//! chunks and puts it back together. None of it does I/O: the bytes go
//! out and come in through whatever carries system exclusive data, such
//! as a port of `manguebyte-devices` or [`ump::sysex7_packets`].
//!
//! Written from "MIDI Capability Inquiry (MIDI-CI)" version 1.2 (M2-101-UM,
//! the MIDI Association and AMEI, 2023), "Common Rules for MIDI-CI
//! Profiles" (M2-102-U), and "Common Rules for MIDI-CI Property
//! Exchange" version 1.1 (M2-103-UM), from the published documents and the
//! author's knowledge of them. It has not been checked against another
//! implementation or a conformance suite.
//!
//! [`ump::sysex7_packets`]: crate::ump::sysex7_packets
//!
//! ```
//! use manguebyte_midi::ci::{self, Address, Categories, Header, Message, Muid};
//! use manguebyte_midi::ump::DeviceIdentity;
//!
//! let identity = DeviceIdentity {
//!     manufacturer: [0x7D, 0, 0],
//!     family: [1, 0],
//!     model: [2, 0],
//!     revision: [0, 0, 1, 0],
//! };
//! let discovery = Message::Discovery {
//!     identity,
//!     categories: Categories::PROFILES.with(Categories::PROPERTIES),
//!     max_sysex: 512,
//!     output_path: 0,
//! };
//! let header = Header::new(Address::FunctionBlock, Muid::new(0x123_4567).unwrap(), Muid::BROADCAST);
//! let mut bytes = Vec::new();
//! ci::encode(&header, &discovery, &mut bytes).unwrap();
//! // A universal system exclusive message, without the 0xF0 and 0xF7.
//! assert_eq!(&bytes[..4], &[0x7E, 0x7F, 0x0D, 0x70]);
//!
//! let (read_header, read) = ci::parse(&bytes).unwrap();
//! assert_eq!(read_header, header);
//! assert_eq!(read, discovery);
//! ```
//!
//! # Limits
//!
//! The JSON of property exchange is carried as bytes and never parsed
//! here. Profile details inquiries are read and written but the agent
//! answers them with a NAK. Protocol negotiation is read and written and
//! the agent answers it with a NAK, as a version 1.2 responder may.

use core::fmt;

use crate::ump::DeviceIdentity;

pub mod agent;
pub mod property;

/// The MIDI-CI version this module writes: 1.2.
pub const VERSION: u8 = 0x02;

/// The universal system exclusive id for non-real-time messages, and the
/// sub-id that marks MIDI-CI.
const UNIVERSAL_NON_REAL_TIME: u8 = 0x7E;
const SUB_ID_MIDI_CI: u8 = 0x0D;

/// Sub-ID#2, the message type.
pub mod sub_id {
    /// Initiate protocol negotiation (deprecated in 1.2).
    pub const INITIATE_PROTOCOL_NEGOTIATION: u8 = 0x10;
    /// Reply to initiate protocol negotiation (deprecated in 1.2).
    pub const INITIATE_PROTOCOL_NEGOTIATION_REPLY: u8 = 0x11;
    /// Set new selected protocol (deprecated in 1.2).
    pub const SET_NEW_PROTOCOL: u8 = 0x12;
    /// Test new protocol, initiator to responder (deprecated in 1.2).
    pub const TEST_NEW_PROTOCOL_TO_RESPONDER: u8 = 0x13;
    /// Test new protocol, responder to initiator (deprecated in 1.2).
    pub const TEST_NEW_PROTOCOL_TO_INITIATOR: u8 = 0x14;
    /// Confirmation that the new protocol is established (deprecated in
    /// 1.2).
    pub const CONFIRM_NEW_PROTOCOL: u8 = 0x15;
    /// Profile inquiry.
    pub const PROFILE_INQUIRY: u8 = 0x20;
    /// Reply to profile inquiry.
    pub const PROFILE_INQUIRY_REPLY: u8 = 0x21;
    /// Set profile on.
    pub const SET_PROFILE_ON: u8 = 0x22;
    /// Set profile off.
    pub const SET_PROFILE_OFF: u8 = 0x23;
    /// Profile enabled report.
    pub const PROFILE_ENABLED: u8 = 0x24;
    /// Profile disabled report.
    pub const PROFILE_DISABLED: u8 = 0x25;
    /// Profile added report.
    pub const PROFILE_ADDED: u8 = 0x26;
    /// Profile removed report.
    pub const PROFILE_REMOVED: u8 = 0x27;
    /// Profile details inquiry.
    pub const PROFILE_DETAILS_INQUIRY: u8 = 0x28;
    /// Reply to profile details inquiry.
    pub const PROFILE_DETAILS_REPLY: u8 = 0x29;
    /// Profile specific data.
    pub const PROFILE_SPECIFIC_DATA: u8 = 0x2F;
    /// Inquiry: property exchange capabilities.
    pub const PROPERTY_CAPABILITIES: u8 = 0x30;
    /// Reply to property exchange capabilities.
    pub const PROPERTY_CAPABILITIES_REPLY: u8 = 0x31;
    /// Inquiry: get property data.
    pub const GET_PROPERTY: u8 = 0x34;
    /// Reply to get property data.
    pub const GET_PROPERTY_REPLY: u8 = 0x35;
    /// Inquiry: set property data.
    pub const SET_PROPERTY: u8 = 0x36;
    /// Reply to set property data.
    pub const SET_PROPERTY_REPLY: u8 = 0x37;
    /// Subscription.
    pub const SUBSCRIPTION: u8 = 0x38;
    /// Reply to subscription.
    pub const SUBSCRIPTION_REPLY: u8 = 0x39;
    /// Notify.
    pub const NOTIFY: u8 = 0x3F;
    /// Inquiry: process inquiry capabilities.
    pub const PROCESS_CAPABILITIES: u8 = 0x40;
    /// Reply to process inquiry capabilities.
    pub const PROCESS_CAPABILITIES_REPLY: u8 = 0x41;
    /// Inquiry: MIDI message report.
    pub const MESSAGE_REPORT: u8 = 0x42;
    /// Reply to MIDI message report.
    pub const MESSAGE_REPORT_REPLY: u8 = 0x43;
    /// End of MIDI message report.
    pub const MESSAGE_REPORT_END: u8 = 0x44;
    /// Discovery.
    pub const DISCOVERY: u8 = 0x70;
    /// Reply to discovery.
    pub const DISCOVERY_REPLY: u8 = 0x71;
    /// Inquiry: endpoint information.
    pub const ENDPOINT_INQUIRY: u8 = 0x72;
    /// Reply to endpoint information.
    pub const ENDPOINT_REPLY: u8 = 0x73;
    /// Acknowledgement.
    pub const ACK: u8 = 0x7D;
    /// Invalidate MUID.
    pub const INVALIDATE_MUID: u8 = 0x7E;
    /// Negative acknowledgement.
    pub const NAK: u8 = 0x7F;
}

/// NAK status codes.
pub mod nak {
    /// Something else went wrong; the text says what.
    pub const OTHER: u8 = 0x00;
    /// The message is not supported.
    pub const NOT_SUPPORTED: u8 = 0x01;
    /// This MIDI-CI version is not supported.
    pub const VERSION_NOT_SUPPORTED: u8 = 0x02;
    /// The channel, group or function block is not in use.
    pub const ADDRESS_NOT_IN_USE: u8 = 0x03;
    /// The profile is not supported here.
    pub const PROFILE_NOT_SUPPORTED: u8 = 0x04;
    /// The inquiry is ended.
    pub const TERMINATE: u8 = 0x20;
    /// A property exchange chunk came out of sequence.
    pub const CHUNK_OUT_OF_SEQUENCE: u8 = 0x21;
    /// An error happened; trying again may work.
    pub const RETRY: u8 = 0x40;
    /// The message was malformed.
    pub const MALFORMED: u8 = 0x41;
    /// Something timed out.
    pub const TIMEOUT: u8 = 0x42;
    /// Something timed out; trying again may work.
    pub const TIMEOUT_RETRY: u8 = 0x43;
}

/// Why bytes could not be read, or a message written, as MIDI-CI.
///
/// More reasons may be added, so a `match` on this type needs a wildcard
/// arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CiError {
    /// The bytes are not a MIDI-CI message: not universal non-real-time
    /// system exclusive with sub-id 0x0D.
    NotMidiCi,
    /// The message ends before a field it must have.
    Truncated,
    /// A byte has its high bit set, which system exclusive data may not.
    NotSevenBit,
    /// The device id byte names neither a channel, a group nor a function
    /// block.
    InvalidAddress(u8),
    /// The message type is not one this module knows.
    UnknownMessage(u8),
    /// A field holds a value the format does not allow, or a value too
    /// large for its field when writing.
    Invalid(&'static str),
}

impl fmt::Display for CiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotMidiCi => f.write_str("not a MIDI-CI message"),
            Self::Truncated => f.write_str("the MIDI-CI message is cut short"),
            Self::NotSevenBit => f.write_str("a system exclusive byte is above 127"),
            Self::InvalidAddress(byte) => write!(f, "device id {byte:#04x} is not an address"),
            Self::UnknownMessage(sub_id) => write!(f, "unknown MIDI-CI message {sub_id:#04x}"),
            Self::Invalid(why) => write!(f, "invalid MIDI-CI field: {why}"),
        }
    }
}

impl std::error::Error for CiError {}

/// A MIDI-CI unique identifier: 28 random bits that name one side of the
/// conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Muid(u32);

impl Muid {
    /// The MUID that addresses every device.
    pub const BROADCAST: Self = Self(0x0FFF_FFFF);

    /// The first of the MUIDs reserved for later use, which a device may
    /// not take: `0x0FFF_FF00` to `0x0FFF_FFFE`.
    const FIRST_RESERVED: u32 = 0x0FFF_FF00;

    /// A MUID, or `None` if `value` has more than 28 bits.
    pub const fn new(value: u32) -> Option<Self> {
        if value > 0x0FFF_FFFF {
            None
        } else {
            Some(Self(value))
        }
    }

    /// A MUID a device may take as its own, made from `seed`: the seed
    /// mixed with the SplitMix64 finaliser (Steele, Lea and Flood, "Fast
    /// splittable pseudorandom number generators", 2014), kept out of the
    /// broadcast and reserved values. Seed it with something that differs
    /// between devices and runs, such as the time and a serial number.
    pub const fn from_seed(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let value = (z as u32) & 0x0FFF_FFFF;
        if value >= Self::FIRST_RESERVED {
            // Out of the top 256 values into the ordinary ones.
            Self(value - 0x100)
        } else {
            Self(value)
        }
    }

    /// The 28-bit value.
    pub const fn value(self) -> u32 {
        self.0
    }

    /// Whether this is [`BROADCAST`](Self::BROADCAST).
    pub const fn is_broadcast(self) -> bool {
        self.0 == Self::BROADCAST.0
    }

    /// Whether a device may take this MUID as its own: neither broadcast
    /// nor reserved.
    pub const fn is_assignable(self) -> bool {
        self.0 < Self::FIRST_RESERVED
    }
}

/// What a message is addressed to: the device id byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Address {
    /// One MIDI channel, `0..=15`.
    Channel(u8),
    /// The group the message travels on.
    Group,
    /// The whole function block (in MIDI-CI 1.1, the "MIDI port").
    FunctionBlock,
}

impl Address {
    fn byte(self) -> u8 {
        match self {
            Self::Channel(channel) => channel & 0x0F,
            Self::Group => 0x7E,
            Self::FunctionBlock => 0x7F,
        }
    }

    fn from_byte(byte: u8) -> Result<Self, CiError> {
        match byte {
            0x00..=0x0F => Ok(Self::Channel(byte)),
            0x7E => Ok(Self::Group),
            0x7F => Ok(Self::FunctionBlock),
            other => Err(CiError::InvalidAddress(other)),
        }
    }
}

/// The part every MIDI-CI message starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header {
    /// What the message is addressed to.
    pub address: Address,
    /// The MIDI-CI message version of the sender: 1 for 1.1, 2 for 1.2.
    pub version: u8,
    /// Who sent it.
    pub source: Muid,
    /// Who it is for, or [`Muid::BROADCAST`].
    pub destination: Muid,
}

impl Header {
    /// A header of the version this module writes.
    pub const fn new(address: Address, source: Muid, destination: Muid) -> Self {
        Self {
            address,
            version: VERSION,
            source,
            destination,
        }
    }
}

/// The categories of MIDI-CI a device supports, as discovery announces
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Categories(u8);

impl Categories {
    /// None.
    pub const NONE: Self = Self(0);
    /// Protocol negotiation (deprecated in 1.2).
    pub const PROTOCOL_NEGOTIATION: Self = Self(0x02);
    /// Profile configuration.
    pub const PROFILES: Self = Self(0x04);
    /// Property exchange.
    pub const PROPERTIES: Self = Self(0x08);
    /// Process inquiry.
    pub const PROCESS_INQUIRY: Self = Self(0x10);

    /// The categories as the byte of the message.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Categories from the byte of a message. Reserved bits are kept.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0x7F)
    }

    /// These categories and `other`.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every category of `other` is among these.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// A profile's identity: five bytes (M2-102-U). A profile the
/// MIDI Association defines starts with `0x7E`; a manufacturer's starts
/// with its system exclusive id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProfileId(pub [u8; 5]);

impl ProfileId {
    /// A profile the MIDI Association defines: its bank, number, version
    /// and level.
    pub const fn standard(bank: u8, number: u8, version: u8, level: u8) -> Self {
        Self([
            0x7E,
            bank & 0x7F,
            number & 0x7F,
            version & 0x7F,
            level & 0x7F,
        ])
    }

    /// Whether the MIDI Association defines the profile.
    pub const fn is_standard(&self) -> bool {
        self.0[0] == 0x7E
    }
}

/// A list of profiles inside a message: five bytes each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProfileList<'a>(&'a [u8]);

impl<'a> ProfileList<'a> {
    /// A list over these bytes, or `None` unless they are whole profiles
    /// of 7-bit bytes.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        (bytes.len() % 5 == 0 && seven_bit(bytes)).then_some(Self(bytes))
    }

    /// The empty list.
    pub const EMPTY: ProfileList<'static> = ProfileList(&[]);

    /// How many profiles the list holds.
    pub fn len(&self) -> usize {
        self.0.len() / 5
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The profiles.
    pub fn iter(&self) -> impl Iterator<Item = ProfileId> + 'a {
        self.0.chunks_exact(5).map(|bytes| {
            let mut id = [0; 5];
            id.copy_from_slice(bytes);
            ProfileId(id)
        })
    }

    /// The bytes, five per profile.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0
    }
}

/// A protocol as protocol negotiation names it: type, version, extensions
/// and two reserved bytes (MIDI-CI 1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Protocol(pub [u8; 5]);

/// The body shared by ACK and NAK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Acknowledgement<'a> {
    /// The message type being answered.
    pub original: u8,
    /// A status code; for a NAK, one of [`nak`].
    pub status: u8,
    /// More about the status.
    pub status_data: u8,
    /// Five bytes whose meaning depends on the message being answered.
    pub details: [u8; 5],
    /// Text for people, or JSON, in 7-bit bytes.
    pub text: &'a [u8],
}

impl Acknowledgement<'static> {
    /// A NAK with this status and no text.
    pub const fn nak(original: u8, status: u8) -> Self {
        Self {
            original,
            status,
            status_data: 0,
            details: [0; 5],
            text: &[],
        }
    }
}

/// Which of the property exchange messages carries a
/// [`PropertyChunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyKind {
    /// Inquiry: get property data.
    Get,
    /// Reply to get property data.
    GetReply,
    /// Inquiry: set property data.
    Set,
    /// Reply to set property data.
    SetReply,
    /// Subscription.
    Subscribe,
    /// Reply to subscription.
    SubscribeReply,
    /// Notify.
    Notify,
}

impl PropertyKind {
    /// The message type.
    pub const fn sub_id(self) -> u8 {
        match self {
            Self::Get => sub_id::GET_PROPERTY,
            Self::GetReply => sub_id::GET_PROPERTY_REPLY,
            Self::Set => sub_id::SET_PROPERTY,
            Self::SetReply => sub_id::SET_PROPERTY_REPLY,
            Self::Subscribe => sub_id::SUBSCRIPTION,
            Self::SubscribeReply => sub_id::SUBSCRIPTION_REPLY,
            Self::Notify => sub_id::NOTIFY,
        }
    }

    /// The reply to this inquiry, or `None` for a reply or a notify.
    pub const fn reply(self) -> Option<Self> {
        match self {
            Self::Get => Some(Self::GetReply),
            Self::Set => Some(Self::SetReply),
            Self::Subscribe => Some(Self::SubscribeReply),
            _ => None,
        }
    }

    /// Whether this is a reply.
    pub const fn is_reply(self) -> bool {
        matches!(self, Self::GetReply | Self::SetReply | Self::SubscribeReply)
    }

    fn from_sub_id(sub_id: u8) -> Option<Self> {
        Some(match sub_id {
            sub_id::GET_PROPERTY => Self::Get,
            sub_id::GET_PROPERTY_REPLY => Self::GetReply,
            sub_id::SET_PROPERTY => Self::Set,
            sub_id::SET_PROPERTY_REPLY => Self::SetReply,
            sub_id::SUBSCRIPTION => Self::Subscribe,
            sub_id::SUBSCRIPTION_REPLY => Self::SubscribeReply,
            sub_id::NOTIFY => Self::Notify,
            _ => return None,
        })
    }
}

/// One chunk of a property exchange message (M2-103-UM).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropertyChunk<'a> {
    /// Ties a reply to its inquiry, `0..=127`.
    pub request_id: u8,
    /// The JSON header. Only the first chunk carries it; later ones have
    /// an empty header.
    pub header: &'a [u8],
    /// How many chunks the data is in, `1..=16383`, or zero while the
    /// sender does not know yet.
    pub chunks: u16,
    /// Which chunk this is, counting from 1.
    pub chunk: u16,
    /// This chunk's part of the property data.
    pub data: &'a [u8],
}

/// A MIDI-CI message, without its [`Header`].
///
/// Fields that borrow, such as property data, point into the bytes the
/// message was read from. Values are as the message carries them; where
/// a field is only in newer versions of the specification, reading an
/// older message gives it the value the specification says it has there.
///
/// More messages may be added, so a `match` on this type needs a wildcard
/// arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Message<'a> {
    /// An initiator announces itself and asks who is there.
    Discovery {
        /// Who made the device.
        identity: DeviceIdentity,
        /// What it supports.
        categories: Categories,
        /// The largest system exclusive message it can receive, in bytes.
        max_sysex: u32,
        /// Which of its outputs this went out on, for the reply to name.
        /// Zero before version 1.2.
        output_path: u8,
    },
    /// A responder answers [`Discovery`](Self::Discovery).
    DiscoveryReply {
        /// Who made the device.
        identity: DeviceIdentity,
        /// What it supports.
        categories: Categories,
        /// The largest system exclusive message it can receive.
        max_sysex: u32,
        /// The output path of the discovery being answered.
        output_path: u8,
        /// The function block the responder is, or `0x7F` for none.
        function_block: u8,
    },
    /// Asks for one piece of information about the endpoint.
    EndpointInquiry {
        /// What is asked for: `0x00` is the product instance id.
        status: u8,
    },
    /// Answers [`EndpointInquiry`](Self::EndpointInquiry).
    EndpointReply {
        /// What was asked for.
        status: u8,
        /// The information, 7-bit.
        data: &'a [u8],
    },
    /// A MUID is no longer in use.
    InvalidateMuid {
        /// The MUID that is gone.
        target: Muid,
    },
    /// The message named in it was received and acted on.
    Ack(Acknowledgement<'a>),
    /// The message named in it could not be acted on.
    Nak(Acknowledgement<'a>),
    /// Protocol negotiation, deprecated: the protocols the initiator
    /// supports, preferred first.
    InitiateProtocolNegotiation {
        /// The initiator's authority level.
        authority: u8,
        /// Five bytes per protocol.
        protocols: &'a [u8],
    },
    /// Protocol negotiation, deprecated: the protocols the responder
    /// supports.
    InitiateProtocolNegotiationReply {
        /// The responder's authority level.
        authority: u8,
        /// Five bytes per protocol.
        protocols: &'a [u8],
    },
    /// Protocol negotiation, deprecated: switch to this protocol.
    SetNewProtocol {
        /// The authority level.
        authority: u8,
        /// The new protocol.
        protocol: Protocol,
    },
    /// Protocol negotiation, deprecated: a test message in the new
    /// protocol, `to_responder` from the initiator, otherwise back.
    TestNewProtocol {
        /// Which way the test goes.
        to_responder: bool,
        /// The authority level.
        authority: u8,
    },
    /// Protocol negotiation, deprecated: the new protocol works.
    ConfirmNewProtocol {
        /// The authority level.
        authority: u8,
    },
    /// Which profiles are there, on the address.
    ProfileInquiry,
    /// Answers [`ProfileInquiry`](Self::ProfileInquiry).
    ProfileInquiryReply {
        /// The profiles that are on.
        enabled: ProfileList<'a>,
        /// The profiles that could be turned on.
        disabled: ProfileList<'a>,
    },
    /// Turn a profile on.
    SetProfileOn {
        /// The profile.
        profile: ProfileId,
        /// On how many channels, for a profile that spans several; zero
        /// for the profile's own default (and before version 1.2).
        channels: u16,
    },
    /// Turn a profile off.
    SetProfileOff {
        /// The profile.
        profile: ProfileId,
    },
    /// A profile is on.
    ProfileEnabled {
        /// The profile.
        profile: ProfileId,
        /// On how many channels; zero for one, or before version 1.2.
        channels: u16,
    },
    /// A profile is off.
    ProfileDisabled {
        /// The profile.
        profile: ProfileId,
        /// On how many channels it was on.
        channels: u16,
    },
    /// A profile became available.
    ProfileAdded {
        /// The profile.
        profile: ProfileId,
    },
    /// A profile is no longer available.
    ProfileRemoved {
        /// The profile.
        profile: ProfileId,
    },
    /// Asks for details of a profile.
    ProfileDetailsInquiry {
        /// The profile.
        profile: ProfileId,
        /// What is asked about; the profile's specification defines it.
        target: u8,
    },
    /// Answers [`ProfileDetailsInquiry`](Self::ProfileDetailsInquiry).
    ProfileDetailsReply {
        /// The profile.
        profile: ProfileId,
        /// What was asked about.
        target: u8,
        /// The details.
        data: &'a [u8],
    },
    /// Data whose meaning the profile defines.
    ProfileSpecificData {
        /// The profile.
        profile: ProfileId,
        /// The data, 7-bit.
        data: &'a [u8],
    },
    /// How many property exchange inquiries can be under way at once, and
    /// which version of property exchange is spoken.
    PropertyCapabilities {
        /// Simultaneous inquiries supported, at least 1.
        simultaneous: u8,
        /// Property exchange major version; zero before MIDI-CI 1.2.
        major: u8,
        /// Property exchange minor version.
        minor: u8,
    },
    /// Answers [`PropertyCapabilities`](Self::PropertyCapabilities).
    PropertyCapabilitiesReply {
        /// Simultaneous inquiries supported, at least 1.
        simultaneous: u8,
        /// Property exchange major version.
        major: u8,
        /// Property exchange minor version.
        minor: u8,
    },
    /// One chunk of a property exchange inquiry, reply or notification.
    Property(PropertyKind, PropertyChunk<'a>),
    /// Which process inquiries are supported.
    ProcessCapabilities,
    /// Answers [`ProcessCapabilities`](Self::ProcessCapabilities).
    ProcessCapabilitiesReply {
        /// One bit per supported inquiry: bit 0 is the MIDI message report.
        features: u8,
    },
    /// Asks the responder to send the MIDI messages that describe its
    /// state.
    MessageReport(MessageReport),
    /// What the responder will send in answer to
    /// [`MessageReport`](Self::MessageReport).
    MessageReportReply(MessageReport),
    /// The responder has sent its report.
    MessageReportEnd,
}

/// What a MIDI message report covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MessageReport {
    /// `0x00`: no data; `0x01`: only values not at their defaults;
    /// `0x7F`: everything.
    pub data_control: u8,
    /// Bitmap of system messages.
    pub system: u8,
    /// Bitmap of channel controller messages.
    pub channel_controllers: u8,
    /// Bitmap of note data messages.
    pub note_data: u8,
}

fn seven_bit(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte < 0x80)
}

/// Reads fields from the body of a message.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], CiError> {
        if self.bytes.len() < count {
            return Err(CiError::Truncated);
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, CiError> {
        Ok(self.take(1)?[0])
    }

    /// A byte that only newer versions have, or `default`.
    fn byte_or(&mut self, default: u8) -> u8 {
        self.byte().unwrap_or(default)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CiError> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    /// Fourteen bits in two bytes, least significant first.
    fn u14(&mut self) -> Result<u16, CiError> {
        let [low, high] = self.array()?;
        Ok(u16::from(low) | (u16::from(high) << 7))
    }

    fn u14_or(&mut self, default: u16) -> u16 {
        if self.bytes.len() >= 2 {
            self.u14().unwrap_or(default)
        } else {
            default
        }
    }

    /// Twenty-eight bits in four bytes, least significant first.
    fn u28(&mut self) -> Result<u32, CiError> {
        let bytes: [u8; 4] = self.array()?;
        Ok(bytes
            .iter()
            .rev()
            .fold(0, |value, byte| (value << 7) | u32::from(*byte)))
    }

    fn profile(&mut self) -> Result<ProfileId, CiError> {
        Ok(ProfileId(self.array()?))
    }

    /// A 14-bit count of `size`-byte items, then the items.
    fn counted(&mut self, size: usize) -> Result<&'a [u8], CiError> {
        let count = usize::from(self.u14()?);
        self.take(count * size)
    }

    /// A 14-bit length, then that many bytes.
    fn sized(&mut self) -> Result<&'a [u8], CiError> {
        let length = usize::from(self.u14()?);
        self.take(length)
    }
}

/// Reads one MIDI-CI message from the bytes of a system exclusive
/// message, with or without the `0xF0` and `0xF7` around it.
///
/// Hostile input is safe: nothing panics, nothing is allocated, and the
/// result borrows from `bytes`. Bytes after the last field a message
/// defines are ignored, as the specification asks of a receiver, so that
/// newer messages can be read; fields that older versions lack take their
/// defaults.
pub fn parse(bytes: &[u8]) -> Result<(Header, Message<'_>), CiError> {
    let bytes = bytes.strip_prefix(&[0xF0]).unwrap_or(bytes);
    let bytes = bytes.strip_suffix(&[0xF7]).unwrap_or(bytes);
    if !seven_bit(bytes) {
        return Err(CiError::NotSevenBit);
    }
    let mut reader = Reader { bytes };
    let [universal, device, sub_1, sub_2, version] =
        reader.array().map_err(|_| CiError::NotMidiCi)?;
    if universal != UNIVERSAL_NON_REAL_TIME || sub_1 != SUB_ID_MIDI_CI {
        return Err(CiError::NotMidiCi);
    }
    let address = Address::from_byte(device)?;
    let source = Muid(reader.u28()?);
    let destination = Muid(reader.u28()?);
    let header = Header {
        address,
        version,
        source,
        destination,
    };
    let message = parse_body(sub_2, &mut reader)?;
    Ok((header, message))
}

fn parse_body<'a>(sub: u8, reader: &mut Reader<'a>) -> Result<Message<'a>, CiError> {
    use sub_id::*;
    let identity = |reader: &mut Reader<'a>| -> Result<DeviceIdentity, CiError> {
        Ok(DeviceIdentity {
            manufacturer: reader.array()?,
            family: reader.array()?,
            model: reader.array()?,
            revision: reader.array()?,
        })
    };
    let acknowledgement = |reader: &mut Reader<'a>| -> Result<Acknowledgement<'a>, CiError> {
        // A NAK of version 1.1 has no body.
        if reader.bytes.is_empty() {
            return Ok(Acknowledgement::nak(0, nak::OTHER));
        }
        Ok(Acknowledgement {
            original: reader.byte()?,
            status: reader.byte()?,
            status_data: reader.byte()?,
            details: reader.array()?,
            text: reader.sized()?,
        })
    };
    let capabilities = |reader: &mut Reader<'a>| -> Result<(u8, u8, u8), CiError> {
        Ok((reader.byte()?, reader.byte_or(0), reader.byte_or(0)))
    };
    let report = |reader: &mut Reader<'a>| -> Result<MessageReport, CiError> {
        let data_control = reader.byte()?;
        let system = reader.byte()?;
        let _reserved = reader.byte()?;
        Ok(MessageReport {
            data_control,
            system,
            channel_controllers: reader.byte()?,
            note_data: reader.byte()?,
        })
    };
    Ok(match sub {
        DISCOVERY => Message::Discovery {
            identity: identity(reader)?,
            categories: Categories::from_bits(reader.byte()?),
            max_sysex: reader.u28()?,
            output_path: reader.byte_or(0),
        },
        DISCOVERY_REPLY => Message::DiscoveryReply {
            identity: identity(reader)?,
            categories: Categories::from_bits(reader.byte()?),
            max_sysex: reader.u28()?,
            output_path: reader.byte_or(0),
            function_block: reader.byte_or(0x7F),
        },
        ENDPOINT_INQUIRY => Message::EndpointInquiry {
            status: reader.byte()?,
        },
        ENDPOINT_REPLY => Message::EndpointReply {
            status: reader.byte()?,
            data: reader.sized()?,
        },
        INVALIDATE_MUID => Message::InvalidateMuid {
            target: Muid(reader.u28()?),
        },
        ACK => Message::Ack(acknowledgement(reader)?),
        NAK => Message::Nak(acknowledgement(reader)?),
        INITIATE_PROTOCOL_NEGOTIATION | INITIATE_PROTOCOL_NEGOTIATION_REPLY => {
            let authority = reader.byte()?;
            let count = usize::from(reader.byte()?);
            let protocols = reader.take(count * 5)?;
            if sub == INITIATE_PROTOCOL_NEGOTIATION {
                Message::InitiateProtocolNegotiation {
                    authority,
                    protocols,
                }
            } else {
                Message::InitiateProtocolNegotiationReply {
                    authority,
                    protocols,
                }
            }
        }
        SET_NEW_PROTOCOL => Message::SetNewProtocol {
            authority: reader.byte()?,
            protocol: Protocol(reader.array()?),
        },
        TEST_NEW_PROTOCOL_TO_RESPONDER | TEST_NEW_PROTOCOL_TO_INITIATOR => {
            let authority = reader.byte()?;
            // Forty-eight bytes counting up from zero (MIDI-CI 1.1):
            // anything else did not survive the new protocol.
            let data = reader.take(48)?;
            if data
                .iter()
                .enumerate()
                .any(|(index, byte)| usize::from(*byte) != index)
            {
                return Err(CiError::Invalid("protocol test data"));
            }
            Message::TestNewProtocol {
                to_responder: sub == TEST_NEW_PROTOCOL_TO_RESPONDER,
                authority,
            }
        }
        CONFIRM_NEW_PROTOCOL => Message::ConfirmNewProtocol {
            authority: reader.byte()?,
        },
        PROFILE_INQUIRY => Message::ProfileInquiry,
        PROFILE_INQUIRY_REPLY => Message::ProfileInquiryReply {
            enabled: ProfileList(reader.counted(5)?),
            disabled: ProfileList(reader.counted(5)?),
        },
        SET_PROFILE_ON => Message::SetProfileOn {
            profile: reader.profile()?,
            channels: reader.u14_or(0),
        },
        SET_PROFILE_OFF => Message::SetProfileOff {
            profile: reader.profile()?,
        },
        PROFILE_ENABLED => Message::ProfileEnabled {
            profile: reader.profile()?,
            channels: reader.u14_or(0),
        },
        PROFILE_DISABLED => Message::ProfileDisabled {
            profile: reader.profile()?,
            channels: reader.u14_or(0),
        },
        PROFILE_ADDED => Message::ProfileAdded {
            profile: reader.profile()?,
        },
        PROFILE_REMOVED => Message::ProfileRemoved {
            profile: reader.profile()?,
        },
        PROFILE_DETAILS_INQUIRY => Message::ProfileDetailsInquiry {
            profile: reader.profile()?,
            target: reader.byte()?,
        },
        PROFILE_DETAILS_REPLY => Message::ProfileDetailsReply {
            profile: reader.profile()?,
            target: reader.byte()?,
            data: reader.sized()?,
        },
        PROFILE_SPECIFIC_DATA => {
            let profile = reader.profile()?;
            let length = reader.u28()? as usize;
            Message::ProfileSpecificData {
                profile,
                data: reader.take(length)?,
            }
        }
        PROPERTY_CAPABILITIES => {
            let (simultaneous, major, minor) = capabilities(reader)?;
            Message::PropertyCapabilities {
                simultaneous,
                major,
                minor,
            }
        }
        PROPERTY_CAPABILITIES_REPLY => {
            let (simultaneous, major, minor) = capabilities(reader)?;
            Message::PropertyCapabilitiesReply {
                simultaneous,
                major,
                minor,
            }
        }
        PROCESS_CAPABILITIES => Message::ProcessCapabilities,
        PROCESS_CAPABILITIES_REPLY => Message::ProcessCapabilitiesReply {
            features: reader.byte()?,
        },
        MESSAGE_REPORT => Message::MessageReport(report(reader)?),
        MESSAGE_REPORT_REPLY => Message::MessageReportReply(report(reader)?),
        MESSAGE_REPORT_END => Message::MessageReportEnd,
        other => {
            let Some(kind) = PropertyKind::from_sub_id(other) else {
                return Err(CiError::UnknownMessage(other));
            };
            let request_id = reader.byte()?;
            let header = reader.sized()?;
            let chunks = reader.u14()?;
            let chunk = reader.u14()?;
            let data = reader.sized()?;
            if chunk == 0 || (chunks != 0 && chunk > chunks) {
                return Err(CiError::Invalid("chunk number"));
            }
            Message::Property(
                kind,
                PropertyChunk {
                    request_id,
                    header,
                    chunks,
                    chunk,
                    data,
                },
            )
        }
    })
}

/// The message type of a message.
pub fn sub_id_of(message: &Message<'_>) -> u8 {
    use sub_id::*;
    match message {
        Message::Discovery { .. } => DISCOVERY,
        Message::DiscoveryReply { .. } => DISCOVERY_REPLY,
        Message::EndpointInquiry { .. } => ENDPOINT_INQUIRY,
        Message::EndpointReply { .. } => ENDPOINT_REPLY,
        Message::InvalidateMuid { .. } => INVALIDATE_MUID,
        Message::Ack(_) => ACK,
        Message::Nak(_) => NAK,
        Message::InitiateProtocolNegotiation { .. } => INITIATE_PROTOCOL_NEGOTIATION,
        Message::InitiateProtocolNegotiationReply { .. } => INITIATE_PROTOCOL_NEGOTIATION_REPLY,
        Message::SetNewProtocol { .. } => SET_NEW_PROTOCOL,
        Message::TestNewProtocol { to_responder, .. } => {
            if *to_responder {
                TEST_NEW_PROTOCOL_TO_RESPONDER
            } else {
                TEST_NEW_PROTOCOL_TO_INITIATOR
            }
        }
        Message::ConfirmNewProtocol { .. } => CONFIRM_NEW_PROTOCOL,
        Message::ProfileInquiry => PROFILE_INQUIRY,
        Message::ProfileInquiryReply { .. } => PROFILE_INQUIRY_REPLY,
        Message::SetProfileOn { .. } => SET_PROFILE_ON,
        Message::SetProfileOff { .. } => SET_PROFILE_OFF,
        Message::ProfileEnabled { .. } => PROFILE_ENABLED,
        Message::ProfileDisabled { .. } => PROFILE_DISABLED,
        Message::ProfileAdded { .. } => PROFILE_ADDED,
        Message::ProfileRemoved { .. } => PROFILE_REMOVED,
        Message::ProfileDetailsInquiry { .. } => PROFILE_DETAILS_INQUIRY,
        Message::ProfileDetailsReply { .. } => PROFILE_DETAILS_REPLY,
        Message::ProfileSpecificData { .. } => PROFILE_SPECIFIC_DATA,
        Message::PropertyCapabilities { .. } => PROPERTY_CAPABILITIES,
        Message::PropertyCapabilitiesReply { .. } => PROPERTY_CAPABILITIES_REPLY,
        Message::Property(kind, _) => kind.sub_id(),
        Message::ProcessCapabilities => PROCESS_CAPABILITIES,
        Message::ProcessCapabilitiesReply { .. } => PROCESS_CAPABILITIES_REPLY,
        Message::MessageReport(_) => MESSAGE_REPORT,
        Message::MessageReportReply(_) => MESSAGE_REPORT_REPLY,
        Message::MessageReportEnd => MESSAGE_REPORT_END,
    }
}

/// Writes fields of a message.
struct Writer<'o> {
    out: &'o mut Vec<u8>,
}

impl Writer<'_> {
    fn byte(&mut self, byte: u8) -> Result<(), CiError> {
        if byte > 0x7F {
            return Err(CiError::NotSevenBit);
        }
        self.out.push(byte);
        Ok(())
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), CiError> {
        if !seven_bit(bytes) {
            return Err(CiError::NotSevenBit);
        }
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    fn u14(&mut self, value: usize, what: &'static str) -> Result<(), CiError> {
        if value > 0x3FFF {
            return Err(CiError::Invalid(what));
        }
        self.out.push((value & 0x7F) as u8);
        self.out.push((value >> 7) as u8);
        Ok(())
    }

    fn u28(&mut self, value: u32, what: &'static str) -> Result<(), CiError> {
        if value > 0x0FFF_FFFF {
            return Err(CiError::Invalid(what));
        }
        for shift in [0, 7, 14, 21] {
            self.out.push(((value >> shift) & 0x7F) as u8);
        }
        Ok(())
    }

    fn sized(&mut self, bytes: &[u8], what: &'static str) -> Result<(), CiError> {
        self.u14(bytes.len(), what)?;
        self.bytes(bytes)
    }

    fn identity(&mut self, identity: &DeviceIdentity) -> Result<(), CiError> {
        self.bytes(&identity.manufacturer)?;
        self.bytes(&identity.family)?;
        self.bytes(&identity.model)?;
        self.bytes(&identity.revision)
    }

    fn acknowledgement(&mut self, ack: &Acknowledgement<'_>) -> Result<(), CiError> {
        self.byte(ack.original)?;
        self.byte(ack.status)?;
        self.byte(ack.status_data)?;
        self.bytes(&ack.details)?;
        self.sized(ack.text, "acknowledgement text longer than 16383 bytes")
    }

    fn report(&mut self, report: &MessageReport) -> Result<(), CiError> {
        self.byte(report.data_control)?;
        self.byte(report.system)?;
        self.byte(0)?;
        self.byte(report.channel_controllers)?;
        self.byte(report.note_data)
    }
}

/// Writes one MIDI-CI message, appending it to `out`: the bytes of a
/// system exclusive message without the `0xF0` and `0xF7` around it, as
/// [`StreamParser`](crate::StreamParser) delivers them and
/// [`ump::sysex7_packets`](crate::ump::sysex7_packets) takes them. Use
/// [`encode_sysex`] for a MIDI 1.0 port, which wants them.
///
/// Fails, leaving `out` as it was, if a byte would be above 127 or a field
/// does not fit: a list longer than 16383 entries, for instance.
pub fn encode(header: &Header, message: &Message<'_>, out: &mut Vec<u8>) -> Result<(), CiError> {
    let start = out.len();
    let result = encode_into(header, message, &mut Writer { out });
    if result.is_err() {
        out.truncate(start);
    }
    result
}

/// [`encode`], with the `0xF0` and `0xF7` that a MIDI 1.0 port needs.
pub fn encode_sysex(
    header: &Header,
    message: &Message<'_>,
    out: &mut Vec<u8>,
) -> Result<(), CiError> {
    let start = out.len();
    out.push(0xF0);
    if let Err(error) = encode(header, message, out) {
        out.truncate(start);
        return Err(error);
    }
    out.push(0xF7);
    Ok(())
}

fn encode_into(header: &Header, message: &Message<'_>, w: &mut Writer<'_>) -> Result<(), CiError> {
    w.byte(UNIVERSAL_NON_REAL_TIME)?;
    w.byte(header.address.byte())?;
    w.byte(SUB_ID_MIDI_CI)?;
    w.byte(sub_id_of(message))?;
    w.byte(header.version)?;
    w.u28(header.source.0, "source MUID")?;
    w.u28(header.destination.0, "destination MUID")?;
    match *message {
        Message::Discovery {
            identity,
            categories,
            max_sysex,
            output_path,
        } => {
            w.identity(&identity)?;
            w.byte(categories.bits())?;
            w.u28(max_sysex, "maximum system exclusive size")?;
            w.byte(output_path)?;
        }
        Message::DiscoveryReply {
            identity,
            categories,
            max_sysex,
            output_path,
            function_block,
        } => {
            w.identity(&identity)?;
            w.byte(categories.bits())?;
            w.u28(max_sysex, "maximum system exclusive size")?;
            w.byte(output_path)?;
            w.byte(function_block)?;
        }
        Message::EndpointInquiry { status } => w.byte(status)?,
        Message::EndpointReply { status, data } => {
            w.byte(status)?;
            w.sized(data, "endpoint information longer than 16383 bytes")?;
        }
        Message::InvalidateMuid { target } => w.u28(target.0, "target MUID")?,
        Message::Ack(ack) | Message::Nak(ack) => w.acknowledgement(&ack)?,
        Message::InitiateProtocolNegotiation {
            authority,
            protocols,
        }
        | Message::InitiateProtocolNegotiationReply {
            authority,
            protocols,
        } => {
            if protocols.len() % 5 != 0 || protocols.len() / 5 > 0x7F {
                return Err(CiError::Invalid("protocol list"));
            }
            w.byte(authority)?;
            w.byte((protocols.len() / 5) as u8)?;
            w.bytes(protocols)?;
        }
        Message::SetNewProtocol {
            authority,
            protocol,
        } => {
            w.byte(authority)?;
            w.bytes(&protocol.0)?;
        }
        Message::TestNewProtocol { authority, .. } => {
            w.byte(authority)?;
            for byte in 0..48 {
                w.byte(byte)?;
            }
        }
        Message::ConfirmNewProtocol { authority } => w.byte(authority)?,
        Message::ProfileInquiry | Message::ProcessCapabilities | Message::MessageReportEnd => {}
        Message::ProfileInquiryReply { enabled, disabled } => {
            for list in [enabled, disabled] {
                w.u14(list.len(), "more than 16383 profiles")?;
                w.bytes(list.as_bytes())?;
            }
        }
        Message::SetProfileOn { profile, channels }
        | Message::ProfileEnabled { profile, channels }
        | Message::ProfileDisabled { profile, channels } => {
            w.bytes(&profile.0)?;
            w.u14(usize::from(channels), "more than 16383 channels")?;
        }
        Message::SetProfileOff { profile } => {
            w.bytes(&profile.0)?;
            // Two reserved bytes since version 1.2.
            w.u14(0, "")?;
        }
        Message::ProfileAdded { profile } | Message::ProfileRemoved { profile } => {
            w.bytes(&profile.0)?;
        }
        Message::ProfileDetailsInquiry { profile, target } => {
            w.bytes(&profile.0)?;
            w.byte(target)?;
        }
        Message::ProfileDetailsReply {
            profile,
            target,
            data,
        } => {
            w.bytes(&profile.0)?;
            w.byte(target)?;
            w.sized(data, "profile details longer than 16383 bytes")?;
        }
        Message::ProfileSpecificData { profile, data } => {
            w.bytes(&profile.0)?;
            let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
            w.u28(length, "profile data longer than 2^28 bytes")?;
            w.bytes(data)?;
        }
        Message::PropertyCapabilities {
            simultaneous,
            major,
            minor,
        }
        | Message::PropertyCapabilitiesReply {
            simultaneous,
            major,
            minor,
        } => {
            w.byte(simultaneous)?;
            w.byte(major)?;
            w.byte(minor)?;
        }
        Message::Property(_, chunk) => {
            if chunk.chunk == 0 || (chunk.chunks != 0 && chunk.chunk > chunk.chunks) {
                return Err(CiError::Invalid("chunk number"));
            }
            w.byte(chunk.request_id)?;
            w.sized(chunk.header, "property header longer than 16383 bytes")?;
            w.u14(usize::from(chunk.chunks), "more than 16383 chunks")?;
            w.u14(usize::from(chunk.chunk), "more than 16383 chunks")?;
            w.sized(chunk.data, "property chunk longer than 16383 bytes")?;
        }
        Message::ProcessCapabilitiesReply { features } => w.byte(features)?,
        Message::MessageReport(report) | Message::MessageReportReply(report) => {
            w.report(&report)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Muid = Muid(0x0123_4567);
    const YOU: Muid = Muid(0x0765_4321);

    fn identity() -> DeviceIdentity {
        DeviceIdentity {
            manufacturer: [0x00, 0x21, 0x09],
            family: [0x10, 0x00],
            model: [0x02, 0x01],
            revision: [0x01, 0x02, 0x03, 0x04],
        }
    }

    fn round_trip(header: Header, message: Message<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode(&header, &message, &mut bytes).unwrap();
        assert_eq!(parse(&bytes), Ok((header, message)), "{message:?}");
        let mut framed = Vec::new();
        encode_sysex(&header, &message, &mut framed).unwrap();
        assert_eq!(framed.first(), Some(&0xF0));
        assert_eq!(framed.last(), Some(&0xF7));
        assert_eq!(&framed[1..framed.len() - 1], bytes.as_slice());
        assert_eq!(parse(&framed), Ok((header, message)));
        bytes
    }

    #[test]
    fn discovery_is_written_as_the_specification_lays_it_out() {
        let header = Header::new(Address::FunctionBlock, ME, Muid::BROADCAST);
        let bytes = round_trip(
            header,
            Message::Discovery {
                identity: identity(),
                categories: Categories::PROFILES.with(Categories::PROPERTIES),
                max_sysex: 0x10_0000,
                output_path: 3,
            },
        );
        assert_eq!(
            bytes,
            [
                0x7E, 0x7F, 0x0D, 0x70, 0x02, // universal, function block, CI, discovery, 1.2
                0x67, 0x0A, 0x0D, 0x09, // 0x0123_4567, 7 bits at a time, low first
                0x7F, 0x7F, 0x7F, 0x7F, // broadcast
                0x00, 0x21, 0x09, 0x10, 0x00, 0x02, 0x01, 0x01, 0x02, 0x03, 0x04, // identity
                0x0C, // profiles and properties
                0x00, 0x00, 0x40, 0x00, // 2^20
                0x03, // output path
            ]
        );
    }

    #[test]
    fn every_message_round_trips() {
        let header = Header::new(Address::Channel(5), ME, YOU);
        let organ = ProfileId::standard(0x21, 0x01, 0x01, 0x01);
        let two = [1, 2, 3, 4, 5, 0x7E, 0, 1, 0, 1];
        let chunk = PropertyChunk {
            request_id: 9,
            header: br#"{"resource":"ProgramList"}"#,
            chunks: 3,
            chunk: 2,
            data: b"[]",
        };
        let messages = [
            Message::DiscoveryReply {
                identity: identity(),
                categories: Categories::PROCESS_INQUIRY,
                max_sysex: 512,
                output_path: 1,
                function_block: 0x7F,
            },
            Message::EndpointInquiry { status: 0 },
            Message::EndpointReply {
                status: 0,
                data: b"SN-1234",
            },
            Message::InvalidateMuid { target: YOU },
            Message::Ack(Acknowledgement {
                original: sub_id::SET_PROFILE_ON,
                status: 0,
                status_data: 1,
                details: [1, 2, 3, 4, 5],
                text: b"fine",
            }),
            Message::Nak(Acknowledgement::nak(
                sub_id::GET_PROPERTY,
                nak::NOT_SUPPORTED,
            )),
            Message::InitiateProtocolNegotiation {
                authority: 0x10,
                protocols: &[1, 0, 0, 0, 0, 2, 0, 0, 0, 0],
            },
            Message::InitiateProtocolNegotiationReply {
                authority: 0x20,
                protocols: &[2, 0, 0, 0, 0],
            },
            Message::SetNewProtocol {
                authority: 0x10,
                protocol: Protocol([2, 0, 0, 0, 0]),
            },
            Message::TestNewProtocol {
                to_responder: true,
                authority: 0x10,
            },
            Message::TestNewProtocol {
                to_responder: false,
                authority: 0x10,
            },
            Message::ConfirmNewProtocol { authority: 0x10 },
            Message::ProfileInquiry,
            Message::ProfileInquiryReply {
                enabled: ProfileList::new(&two[..5]).unwrap(),
                disabled: ProfileList::new(&two).unwrap(),
            },
            Message::SetProfileOn {
                profile: organ,
                channels: 3,
            },
            Message::SetProfileOff { profile: organ },
            Message::ProfileEnabled {
                profile: organ,
                channels: 1,
            },
            Message::ProfileDisabled {
                profile: organ,
                channels: 0,
            },
            Message::ProfileAdded { profile: organ },
            Message::ProfileRemoved { profile: organ },
            Message::ProfileDetailsInquiry {
                profile: organ,
                target: 1,
            },
            Message::ProfileDetailsReply {
                profile: organ,
                target: 1,
                data: &[1, 2],
            },
            Message::ProfileSpecificData {
                profile: organ,
                data: &[0x10, 0x20, 0x30],
            },
            Message::PropertyCapabilities {
                simultaneous: 4,
                major: 0,
                minor: 1,
            },
            Message::PropertyCapabilitiesReply {
                simultaneous: 1,
                major: 0,
                minor: 1,
            },
            Message::Property(PropertyKind::Get, chunk),
            Message::Property(PropertyKind::GetReply, chunk),
            Message::Property(PropertyKind::Set, chunk),
            Message::Property(PropertyKind::SetReply, chunk),
            Message::Property(PropertyKind::Subscribe, chunk),
            Message::Property(PropertyKind::SubscribeReply, chunk),
            Message::Property(PropertyKind::Notify, chunk),
            Message::ProcessCapabilities,
            Message::ProcessCapabilitiesReply { features: 1 },
            Message::MessageReport(MessageReport {
                data_control: 0x7F,
                system: 0x07,
                channel_controllers: 0x3F,
                note_data: 0x1F,
            }),
            Message::MessageReportReply(MessageReport::default()),
            Message::MessageReportEnd,
        ];
        for message in messages {
            round_trip(header, message);
            round_trip(Header::new(Address::Group, YOU, ME), message);
        }
    }

    #[test]
    fn older_messages_read_with_the_defaults_of_their_version() {
        // A discovery of version 1.1: no output path.
        let mut bytes = Vec::new();
        let header = Header {
            version: 1,
            ..Header::new(Address::FunctionBlock, ME, Muid::BROADCAST)
        };
        encode(
            &header,
            &Message::DiscoveryReply {
                identity: identity(),
                categories: Categories::PROFILES,
                max_sysex: 256,
                output_path: 0,
                function_block: 0x7F,
            },
            &mut bytes,
        )
        .unwrap();
        bytes.truncate(bytes.len() - 2);
        let (_, message) = parse(&bytes).unwrap();
        assert!(matches!(
            message,
            Message::DiscoveryReply {
                output_path: 0,
                function_block: 0x7F,
                ..
            }
        ));
        // A NAK of version 1.1 has no body at all.
        let mut nak = Vec::new();
        encode(&header, &Message::Nak(Acknowledgement::nak(0, 0)), &mut nak).unwrap();
        nak.truncate(13);
        assert!(matches!(parse(&nak), Ok((_, Message::Nak(_)))));
        // And newer messages may have bytes this version does not know.
        let mut longer = Vec::new();
        let on = Message::SetProfileOn {
            profile: ProfileId::standard(0x21, 1, 1, 1),
            channels: 2,
        };
        encode(&header, &on, &mut longer).unwrap();
        longer.extend_from_slice(&[1, 2, 3]);
        assert_eq!(parse(&longer).unwrap().1, on);
    }

    #[test]
    fn what_cannot_be_written_is_refused_and_leaves_the_output_alone() {
        let header = Header::new(Address::Channel(0), ME, YOU);
        let mut out = vec![1, 2, 3];
        let not_seven_bit = Message::EndpointReply {
            status: 0,
            data: &[0x80],
        };
        assert_eq!(
            encode(&header, &not_seven_bit, &mut out),
            Err(CiError::NotSevenBit)
        );
        let too_long = vec![0; 0x4000];
        let chunk = PropertyChunk {
            request_id: 0,
            header: &too_long,
            chunks: 1,
            chunk: 1,
            data: &[],
        };
        assert!(
            encode(
                &header,
                &Message::Property(PropertyKind::Get, chunk),
                &mut out
            )
            .is_err()
        );
        let past = PropertyChunk {
            chunk: 2,
            header: &[],
            ..chunk
        };
        assert!(
            encode(
                &header,
                &Message::Property(PropertyKind::Get, past),
                &mut out
            )
            .is_err()
        );
        let bad_muid = Header {
            source: Muid(0x1000_0000),
            ..header
        };
        assert!(encode(&bad_muid, &Message::ProfileInquiry, &mut out).is_err());
        assert!(encode_sysex(&header, &not_seven_bit, &mut out).is_err());
        assert_eq!(out, [1, 2, 3]);
    }

    #[test]
    fn what_is_not_midi_ci_is_told_apart() {
        assert_eq!(parse(&[]), Err(CiError::NotMidiCi));
        // A device inquiry: universal, but not MIDI-CI.
        assert_eq!(parse(&[0x7E, 0x7F, 0x06, 0x01]), Err(CiError::NotMidiCi));
        assert_eq!(parse(&[0xF0, 0x80, 0xF7]), Err(CiError::NotSevenBit));
        let mut bytes = Vec::new();
        encode(
            &Header::new(Address::Channel(1), ME, YOU),
            &Message::ProfileInquiry,
            &mut bytes,
        )
        .unwrap();
        bytes[1] = 0x20;
        assert_eq!(parse(&bytes), Err(CiError::InvalidAddress(0x20)));
        bytes[1] = 0x01;
        bytes[3] = 0x50;
        assert_eq!(parse(&bytes), Err(CiError::UnknownMessage(0x50)));
        bytes[3] = sub_id::SET_PROFILE_ON;
        assert_eq!(parse(&bytes), Err(CiError::Truncated));
    }

    #[test]
    fn muids_from_seeds_are_always_assignable() {
        let mut seen = std::collections::HashSet::new();
        for seed in 0..10_000u64 {
            let muid = Muid::from_seed(seed);
            assert!(muid.is_assignable() && !muid.is_broadcast());
            seen.insert(muid);
        }
        assert!(seen.len() > 9_990, "{} distinct", seen.len());
        assert_eq!(Muid::new(0x1000_0000), None);
        assert!(!Muid::new(0x0FFF_FF00).unwrap().is_assignable());
    }

    #[test]
    fn hostile_bytes_never_panic() {
        // Every truncation and many corruptions of every message.
        let header = Header::new(Address::Channel(3), ME, YOU);
        let mut corpus = Vec::new();
        let chunk = PropertyChunk {
            request_id: 1,
            header: b"{}",
            chunks: 2,
            chunk: 1,
            data: b"abc",
        };
        for message in [
            Message::Discovery {
                identity: identity(),
                categories: Categories::PROFILES,
                max_sysex: 99,
                output_path: 0,
            },
            Message::ProfileInquiryReply {
                enabled: ProfileList::new(&[1, 2, 3, 4, 5]).unwrap(),
                disabled: ProfileList::EMPTY,
            },
            Message::Property(PropertyKind::Set, chunk),
            Message::ProfileSpecificData {
                profile: ProfileId::standard(1, 2, 3, 4),
                data: &[1; 20],
            },
            Message::Nak(Acknowledgement::nak(1, 2)),
            Message::TestNewProtocol {
                to_responder: true,
                authority: 1,
            },
        ] {
            let mut bytes = Vec::new();
            encode(&header, &message, &mut bytes).unwrap();
            corpus.push(bytes);
        }
        let mut state = 0x2545_F491_u32;
        for bytes in &corpus {
            for cut in 0..=bytes.len() {
                let _ = parse(&bytes[..cut]);
            }
            for _ in 0..2_000 {
                let mut bytes = bytes.clone();
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let at = state as usize % bytes.len();
                bytes[at] = (state >> 8) as u8 & 0x7F;
                if let Ok((_, Message::ProfileInquiryReply { enabled, disabled })) = parse(&bytes) {
                    assert!(enabled.iter().count() == enabled.len());
                    assert!(disabled.len() * 5 <= bytes.len());
                }
            }
        }
    }
}
