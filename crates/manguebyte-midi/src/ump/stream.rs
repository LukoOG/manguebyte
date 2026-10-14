//! UMP stream messages: message type `0xF`, section 7.1 of the
//! specification. Two endpoints use them to find out about each other and
//! to agree on a protocol.

use super::{Packet, Part, Pieces, UmpError, bytes16, octet, seven, until_nul};

/// Bits of the filter in [`StreamMessage::EndpointDiscovery`]: which
/// notifications the sender wants in reply (figure 12).
pub mod endpoint_filter {
    /// Ask for [`EndpointInfo`](super::StreamMessage::EndpointInfo).
    pub const ENDPOINT_INFO: u8 = 0x01;
    /// Ask for [`DeviceIdentity`](super::StreamMessage::DeviceIdentity).
    pub const DEVICE_IDENTITY: u8 = 0x02;
    /// Ask for [`EndpointName`](super::StreamMessage::EndpointName).
    pub const ENDPOINT_NAME: u8 = 0x04;
    /// Ask for
    /// [`ProductInstanceId`](super::StreamMessage::ProductInstanceId).
    pub const PRODUCT_INSTANCE_ID: u8 = 0x08;
    /// Ask for [`StreamConfigurationNotification`].
    ///
    /// [`StreamConfigurationNotification`]: super::StreamMessage::StreamConfigurationNotification
    pub const STREAM_CONFIGURATION: u8 = 0x10;
    /// Ask for everything.
    pub const ALL: u8 = 0x1F;
}

/// Bits of the filter in [`StreamMessage::FunctionBlockDiscovery`]
/// (figure 21).
pub mod function_block_filter {
    /// Ask for
    /// [`FunctionBlockInfo`](super::StreamMessage::FunctionBlockInfo).
    pub const INFO: u8 = 0x01;
    /// Ask for
    /// [`FunctionBlockName`](super::StreamMessage::FunctionBlockName).
    pub const NAME: u8 = 0x02;
    /// Ask for both.
    pub const ALL: u8 = 0x03;
}

/// Values of [`StreamConfiguration::protocol`].
pub mod protocol {
    /// The MIDI 1.0 protocol: 7-bit channel voice messages in packets.
    pub const MIDI_1: u8 = 0x01;
    /// The MIDI 2.0 protocol.
    pub const MIDI_2: u8 = 0x02;
}

/// A UMP stream message (section 7.1). These are addressed to the endpoint
/// as a whole and have no group.
///
/// Most statuses are reserved, so more variants may be added and a `match`
/// on this type needs a wildcard arm.
///
/// ```
/// use manguebyte_midi::ump::{endpoint_filter, Message, StreamMessage};
///
/// // "Tell me who you are and which protocol you are using."
/// let discovery = StreamMessage::EndpointDiscovery {
///     version_major: 1,
///     version_minor: 1,
///     filter: endpoint_filter::ENDPOINT_INFO | endpoint_filter::STREAM_CONFIGURATION,
/// };
/// let packet = Message::Stream(discovery).to_packet();
/// assert_eq!(packet.words(), &[0xF000_0101, 0x0000_0011, 0, 0]);
/// assert_eq!(packet.message(), Ok(Message::Stream(discovery)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StreamMessage {
    /// Asks the other endpoint to describe itself (section 7.1.1).
    EndpointDiscovery {
        /// Major version of the UMP format the sender implements: 1 for
        /// version 1.1.
        version_major: u8,
        /// Minor version: 1 for version 1.1.
        version_minor: u8,
        /// Which notifications are wanted: [`endpoint_filter`] bits.
        filter: u8,
    },
    /// What an endpoint can do (section 7.1.2).
    EndpointInfo(EndpointInfo),
    /// Who made the device and which model it is (section 7.1.3).
    DeviceIdentity(DeviceIdentity),
    /// Up to fourteen bytes of the endpoint's name, in UTF-8
    /// (section 7.1.4). The whole name is at most 98 bytes.
    EndpointName(StreamText<14>),
    /// Up to fourteen bytes of the product instance id, normally the
    /// serial number, in printable ASCII (section 7.1.5). The whole id is
    /// at most 42 bytes.
    ProductInstanceId(StreamText<14>),
    /// Asks the other endpoint to use a protocol and to send or accept
    /// jitter-reduction timestamps (section 7.1.6.2).
    StreamConfigurationRequest(StreamConfiguration),
    /// The protocol and timestamps an endpoint is using now
    /// (section 7.1.6.3).
    StreamConfigurationNotification(StreamConfiguration),
    /// Asks for the description of function blocks (section 7.1.7).
    FunctionBlockDiscovery {
        /// The block asked about, `0..=31`, or `0xFF` for all of them.
        block: u8,
        /// What is wanted: [`function_block_filter`] bits.
        filter: u8,
    },
    /// What one function block is (section 7.1.8).
    FunctionBlockInfo(FunctionBlockInfo),
    /// Up to thirteen bytes of a function block's name, in UTF-8
    /// (section 7.1.9).
    FunctionBlockName {
        /// The block being named, `0..=31`.
        block: u8,
        /// This packet's piece of the name.
        name: StreamText<13>,
    },
    /// The first event of the sequence data in a MIDI clip file
    /// (section 7.1.10).
    StartOfClip,
    /// The last event of the sequence data in a MIDI clip file
    /// (section 7.1.11).
    EndOfClip,
}

/// What an endpoint can do (figure 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointInfo {
    /// Major version of the UMP format the endpoint implements.
    pub version_major: u8,
    /// Minor version of the UMP format the endpoint implements.
    pub version_minor: u8,
    /// The endpoint promises not to change its function blocks.
    pub static_function_blocks: bool,
    /// How many function blocks the endpoint has, `0..=32`.
    pub function_blocks: u8,
    /// The endpoint can use the MIDI 2.0 protocol.
    pub midi2: bool,
    /// The endpoint can use the MIDI 1.0 protocol.
    pub midi1: bool,
    /// The endpoint can receive jitter-reduction timestamps.
    pub receives_jr_timestamps: bool,
    /// The endpoint can send jitter-reduction timestamps.
    pub sends_jr_timestamps: bool,
}

/// Who made a device and which model it is: the same data as the reply to
/// a MIDI 1.0 device inquiry (figure 14). Every byte is 7-bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceIdentity {
    /// The manufacturer's system exclusive id. A one-byte id is followed
    /// by two zeros.
    pub manufacturer: [u8; 3],
    /// The device family, least significant byte first.
    pub family: [u8; 2],
    /// The model within the family, least significant byte first.
    pub model: [u8; 2],
    /// The software revision level.
    pub revision: [u8; 4],
}

/// A protocol and the use of jitter-reduction timestamps (figures 18 and
/// 19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamConfiguration {
    /// [`protocol::MIDI_1`] or [`protocol::MIDI_2`].
    pub protocol: u8,
    /// In a request: the receiver of this message should expect
    /// timestamps. In a notification: its sender expects them.
    pub receives_jr_timestamps: bool,
    /// In a request: the receiver of this message should send timestamps.
    /// In a notification: its sender sends them.
    pub sends_jr_timestamps: bool,
}

/// What one function block is (figure 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionBlockInfo {
    /// Whether the block is in use at the moment.
    pub active: bool,
    /// The block's number, `0..=31`.
    pub block: u8,
    /// What a user interface should present the block as: 1 for a
    /// receiver, 2 for a sender, 3 for both, 0 if not declared.
    pub ui_hint: u8,
    /// Whether the block stands for a MIDI 1.0 port: 0 for no, 1 for yes,
    /// 2 for yes and limited to the speed of a MIDI 1.0 cable.
    pub midi1: u8,
    /// Which way messages flow: 1 for into the block only, 2 for out of it
    /// only, 3 for both.
    pub direction: u8,
    /// The lowest group that belongs to the block, `0..=15`.
    pub first_group: u8,
    /// How many groups the block spans, `1..=16`.
    pub groups_spanned: u8,
    /// The version of MIDI-CI messages the block supports, or 0 for none
    /// or unknown.
    pub midi_ci_version: u8,
    /// How many system exclusive 8 streams the block can receive at once.
    pub max_sysex8_streams: u8,
}

/// One packet's piece of a name or identifier: up to `N` bytes.
///
/// A name longer than one packet is cut at any byte, so one piece need not
/// be valid UTF-8 on its own. Collect the pieces with an
/// [`Assembler`](super::Assembler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamText<const N: usize> {
    /// Which piece of the whole text this is.
    pub part: Part,
    bytes: [u8; N],
}

impl<const N: usize> StreamText<N> {
    /// A piece holding `text`, padded with zeros. Fails if `text` is
    /// longer than `N` bytes.
    pub fn new(part: Part, text: &[u8]) -> Result<Self, UmpError> {
        let mut bytes = [0u8; N];
        match bytes.get_mut(..text.len()) {
            Some(target) => target.copy_from_slice(text),
            None => return Err(UmpError::Invalid("text too long for one packet")),
        }
        Ok(Self { part, bytes })
    }

    /// The text in this packet: the bytes up to the first zero, which
    /// marks the end of the name.
    pub fn text(&self) -> &[u8] {
        until_nul(&self.bytes)
    }

    /// All the bytes, with the padding.
    pub fn bytes(&self) -> &[u8; N] {
        &self.bytes
    }
}

/// The longest endpoint name: seven packets of fourteen bytes.
const MAX_ENDPOINT_NAME: usize = 98;
/// The longest product instance id: three packets of fourteen bytes.
const MAX_PRODUCT_INSTANCE_ID: usize = 42;
/// The longest function block name that fits the seven packets a name may
/// take: seven times thirteen bytes.
const MAX_FUNCTION_BLOCK_NAME: usize = 91;

/// Cuts an endpoint name into stream packets of fourteen bytes.
///
/// Fails if the name is longer than 98 bytes or contains a zero byte,
/// which the format uses as its end marker. Does not allocate.
pub fn endpoint_name_packets(name: &str) -> Result<impl Iterator<Item = Packet> + '_, UmpError> {
    check_text(name.as_bytes(), MAX_ENDPOINT_NAME)?;
    Ok(Pieces::new(name.as_bytes(), 14).map(|(part, piece)| {
        let mut bytes = [0u8; 14];
        bytes[..piece.len()].copy_from_slice(piece);
        encode(&StreamMessage::EndpointName(StreamText { part, bytes }))
    }))
}

/// Cuts a product instance id into stream packets of fourteen bytes.
///
/// Fails if the id is longer than 42 bytes or holds anything but printable
/// ASCII, codes 32 to 126. Does not allocate.
pub fn product_instance_id_packets(
    id: &str,
) -> Result<impl Iterator<Item = Packet> + '_, UmpError> {
    check_text(id.as_bytes(), MAX_PRODUCT_INSTANCE_ID)?;
    if !id.bytes().all(|byte| (32..=126).contains(&byte)) {
        return Err(UmpError::Invalid(
            "product instance id is not printable ASCII",
        ));
    }
    Ok(Pieces::new(id.as_bytes(), 14).map(|(part, piece)| {
        let mut bytes = [0u8; 14];
        bytes[..piece.len()].copy_from_slice(piece);
        encode(&StreamMessage::ProductInstanceId(StreamText {
            part,
            bytes,
        }))
    }))
}

/// Cuts the name of function block `block` into stream packets of thirteen
/// bytes.
///
/// Fails if the name contains a zero byte or is longer than 91 bytes. The
/// specification's text allows 98 bytes but also at most seven packets,
/// which hold 91; this function keeps to the seven packets. Does not
/// allocate.
pub fn function_block_name_packets(
    block: u8,
    name: &str,
) -> Result<impl Iterator<Item = Packet> + '_, UmpError> {
    check_text(name.as_bytes(), MAX_FUNCTION_BLOCK_NAME)?;
    Ok(Pieces::new(name.as_bytes(), 13).map(move |(part, piece)| {
        let mut bytes = [0u8; 13];
        bytes[..piece.len()].copy_from_slice(piece);
        encode(&StreamMessage::FunctionBlockName {
            block,
            name: StreamText { part, bytes },
        })
    }))
}

fn check_text(bytes: &[u8], longest: usize) -> Result<(), UmpError> {
    if bytes.len() > longest {
        Err(UmpError::Invalid("text longer than the format allows"))
    } else if bytes.contains(&0) {
        Err(UmpError::Invalid("text contains a zero byte"))
    } else {
        Ok(())
    }
}

const ENDPOINT_DISCOVERY: u16 = 0x000;
const ENDPOINT_INFO: u16 = 0x001;
const DEVICE_IDENTITY: u16 = 0x002;
const ENDPOINT_NAME: u16 = 0x003;
const PRODUCT_INSTANCE_ID: u16 = 0x004;
const STREAM_CONFIGURATION_REQUEST: u16 = 0x005;
const STREAM_CONFIGURATION_NOTIFICATION: u16 = 0x006;
const FUNCTION_BLOCK_DISCOVERY: u16 = 0x010;
const FUNCTION_BLOCK_INFO: u16 = 0x011;
const FUNCTION_BLOCK_NAME: u16 = 0x012;
const START_OF_CLIP: u16 = 0x020;
const END_OF_CLIP: u16 = 0x021;

pub(super) fn encode(message: &StreamMessage) -> Packet {
    // Figure 10: message type, two bits of form, ten bits of status, then
    // fourteen bytes of data.
    let mut bytes = [0u8; 16];
    let (form, status) = match *message {
        StreamMessage::EndpointDiscovery {
            version_major,
            version_minor,
            filter,
        } => {
            bytes[2] = version_major;
            bytes[3] = version_minor;
            bytes[7] = filter & endpoint_filter::ALL;
            (Part::Complete, ENDPOINT_DISCOVERY)
        }
        StreamMessage::EndpointInfo(info) => {
            bytes[2] = info.version_major;
            bytes[3] = info.version_minor;
            bytes[4] = (u8::from(info.static_function_blocks) << 7) | (info.function_blocks & 0x7F);
            bytes[6] = (u8::from(info.midi2) << 1) | u8::from(info.midi1);
            bytes[7] =
                (u8::from(info.receives_jr_timestamps) << 1) | u8::from(info.sends_jr_timestamps);
            (Part::Complete, ENDPOINT_INFO)
        }
        StreamMessage::DeviceIdentity(identity) => {
            let fields = identity
                .manufacturer
                .iter()
                .chain(&identity.family)
                .chain(&identity.model)
                .chain(&identity.revision);
            for (byte, field) in bytes[5..].iter_mut().zip(fields) {
                *byte = field & 0x7F;
            }
            (Part::Complete, DEVICE_IDENTITY)
        }
        StreamMessage::EndpointName(text) => {
            bytes[2..].copy_from_slice(&text.bytes);
            (text.part, ENDPOINT_NAME)
        }
        StreamMessage::ProductInstanceId(text) => {
            bytes[2..].copy_from_slice(&text.bytes);
            (text.part, PRODUCT_INSTANCE_ID)
        }
        StreamMessage::StreamConfigurationRequest(configuration) => {
            encode_configuration(&mut bytes, configuration);
            (Part::Complete, STREAM_CONFIGURATION_REQUEST)
        }
        StreamMessage::StreamConfigurationNotification(configuration) => {
            encode_configuration(&mut bytes, configuration);
            (Part::Complete, STREAM_CONFIGURATION_NOTIFICATION)
        }
        StreamMessage::FunctionBlockDiscovery { block, filter } => {
            bytes[2] = block;
            bytes[3] = filter & function_block_filter::ALL;
            (Part::Complete, FUNCTION_BLOCK_DISCOVERY)
        }
        StreamMessage::FunctionBlockInfo(info) => {
            bytes[2] = (u8::from(info.active) << 7) | (info.block & 0x7F);
            bytes[3] =
                ((info.ui_hint & 0x3) << 4) | ((info.midi1 & 0x3) << 2) | (info.direction & 0x3);
            bytes[4] = info.first_group;
            bytes[5] = info.groups_spanned;
            bytes[6] = info.midi_ci_version;
            bytes[7] = info.max_sysex8_streams;
            (Part::Complete, FUNCTION_BLOCK_INFO)
        }
        StreamMessage::FunctionBlockName { block, name } => {
            bytes[2] = block;
            bytes[3..].copy_from_slice(&name.bytes);
            (name.part, FUNCTION_BLOCK_NAME)
        }
        StreamMessage::StartOfClip => (Part::Complete, START_OF_CLIP),
        StreamMessage::EndOfClip => (Part::Complete, END_OF_CLIP),
    };
    let top = 0xF000 | ((form.bits() as u16) << 10) | status;
    bytes[..2].copy_from_slice(&top.to_be_bytes());
    Packet::from_bytes16(bytes)
}

fn encode_configuration(bytes: &mut [u8; 16], configuration: StreamConfiguration) {
    bytes[2] = configuration.protocol;
    bytes[3] = (u8::from(configuration.receives_jr_timestamps) << 1)
        | u8::from(configuration.sends_jr_timestamps);
}

fn decode_configuration(bytes: &[u8; 16]) -> StreamConfiguration {
    StreamConfiguration {
        protocol: bytes[2],
        receives_jr_timestamps: bytes[3] & 0x02 != 0,
        sends_jr_timestamps: bytes[3] & 0x01 != 0,
    }
}

pub(super) fn decode(words: &[u32; 4]) -> Result<StreamMessage, UmpError> {
    let w0 = words[0];
    let part = Part::from_bits(w0 >> 26);
    let status = ((w0 >> 16) & 0x03FF) as u16;
    let bytes = bytes16(words);
    let message = match status {
        ENDPOINT_NAME => {
            let mut text = [0u8; 14];
            text.copy_from_slice(&bytes[2..]);
            return Ok(StreamMessage::EndpointName(StreamText {
                part,
                bytes: text,
            }));
        }
        PRODUCT_INSTANCE_ID => {
            let mut text = [0u8; 14];
            text.copy_from_slice(&bytes[2..]);
            return Ok(StreamMessage::ProductInstanceId(StreamText {
                part,
                bytes: text,
            }));
        }
        FUNCTION_BLOCK_NAME => {
            let mut text = [0u8; 13];
            text.copy_from_slice(&bytes[3..]);
            return Ok(StreamMessage::FunctionBlockName {
                block: bytes[2],
                name: StreamText { part, bytes: text },
            });
        }
        ENDPOINT_DISCOVERY => StreamMessage::EndpointDiscovery {
            version_major: bytes[2],
            version_minor: bytes[3],
            filter: bytes[7] & endpoint_filter::ALL,
        },
        ENDPOINT_INFO => StreamMessage::EndpointInfo(EndpointInfo {
            version_major: bytes[2],
            version_minor: bytes[3],
            static_function_blocks: bytes[4] & 0x80 != 0,
            function_blocks: bytes[4] & 0x7F,
            midi2: bytes[6] & 0x02 != 0,
            midi1: bytes[6] & 0x01 != 0,
            receives_jr_timestamps: bytes[7] & 0x02 != 0,
            sends_jr_timestamps: bytes[7] & 0x01 != 0,
        }),
        DEVICE_IDENTITY => StreamMessage::DeviceIdentity(DeviceIdentity {
            manufacturer: [seven(words[1], 16), seven(words[1], 8), seven(words[1], 0)],
            family: [seven(words[2], 24), seven(words[2], 16)],
            model: [seven(words[2], 8), seven(words[2], 0)],
            revision: [
                seven(words[3], 24),
                seven(words[3], 16),
                seven(words[3], 8),
                seven(words[3], 0),
            ],
        }),
        STREAM_CONFIGURATION_REQUEST => {
            StreamMessage::StreamConfigurationRequest(decode_configuration(&bytes))
        }
        STREAM_CONFIGURATION_NOTIFICATION => {
            StreamMessage::StreamConfigurationNotification(decode_configuration(&bytes))
        }
        FUNCTION_BLOCK_DISCOVERY => StreamMessage::FunctionBlockDiscovery {
            block: bytes[2],
            filter: bytes[3] & function_block_filter::ALL,
        },
        FUNCTION_BLOCK_INFO => StreamMessage::FunctionBlockInfo(FunctionBlockInfo {
            active: bytes[2] & 0x80 != 0,
            block: bytes[2] & 0x7F,
            ui_hint: (bytes[3] >> 4) & 0x3,
            midi1: (bytes[3] >> 2) & 0x3,
            direction: bytes[3] & 0x3,
            first_group: octet(words[1], 24),
            groups_spanned: octet(words[1], 16),
            midi_ci_version: octet(words[1], 8),
            max_sysex8_streams: octet(words[1], 0),
        }),
        START_OF_CLIP => StreamMessage::StartOfClip,
        END_OF_CLIP => StreamMessage::EndOfClip,
        _ => {
            return Err(UmpError::UnknownStatus {
                message_type: 0xF,
                status,
            });
        }
    };
    // Everything but the three texts is defined as a single packet.
    if part != Part::Complete {
        return Err(UmpError::Invalid(
            "a stream message of fixed size spread over several packets",
        ));
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::super::{Assembler, Message};
    use super::*;

    fn words(message: StreamMessage) -> [u32; 4] {
        let packet = Message::Stream(message).to_packet();
        assert_eq!(packet.word_count(), 4);
        assert_eq!(packet.message_type(), 0xF);
        assert_eq!(packet.group(), None);
        assert_eq!(packet.message(), Ok(Message::Stream(message)), "{packet:?}");
        [
            packet.words()[0],
            packet.words()[1],
            packet.words()[2],
            packet.words()[3],
        ]
    }

    #[test]
    fn discovery_and_endpoint_info_match_their_figures() {
        // Figure 11 and the filter bits of figure 12: e, d, n, i, s from
        // the least significant bit.
        assert_eq!(
            words(StreamMessage::EndpointDiscovery {
                version_major: 1,
                version_minor: 1,
                filter: endpoint_filter::ALL,
            }),
            [0xF000_0101, 0x0000_001F, 0, 0]
        );
        assert_eq!(endpoint_filter::ENDPOINT_INFO, 0b0_0001);
        assert_eq!(endpoint_filter::DEVICE_IDENTITY, 0b0_0010);
        assert_eq!(endpoint_filter::ENDPOINT_NAME, 0b0_0100);
        assert_eq!(endpoint_filter::PRODUCT_INSTANCE_ID, 0b0_1000);
        assert_eq!(endpoint_filter::STREAM_CONFIGURATION, 0b1_0000);

        // Figure 13: static bit, seven bits of block count, then m2 and
        // m1 in bits 9 and 8 and the two timestamp bits at the bottom.
        let info = |change: fn(&mut EndpointInfo)| {
            let mut info = EndpointInfo {
                version_major: 1,
                version_minor: 1,
                static_function_blocks: false,
                function_blocks: 0,
                midi2: false,
                midi1: false,
                receives_jr_timestamps: false,
                sends_jr_timestamps: false,
            };
            change(&mut info);
            words(StreamMessage::EndpointInfo(info))
        };
        assert_eq!(info(|_| {}), [0xF001_0101, 0, 0, 0]);
        assert_eq!(info(|i| i.static_function_blocks = true)[1], 0x8000_0000);
        assert_eq!(info(|i| i.function_blocks = 32)[1], 0x2000_0000);
        assert_eq!(info(|i| i.midi2 = true)[1], 0x0000_0200);
        assert_eq!(info(|i| i.midi1 = true)[1], 0x0000_0100);
        assert_eq!(info(|i| i.receives_jr_timestamps = true)[1], 0x0000_0002);
        assert_eq!(info(|i| i.sends_jr_timestamps = true)[1], 0x0000_0001);
    }

    #[test]
    fn device_identity_matches_its_figure() {
        // Figure 14.
        assert_eq!(
            words(StreamMessage::DeviceIdentity(DeviceIdentity {
                manufacturer: [0x00, 0x21, 0x09],
                family: [0x01, 0x02],
                model: [0x03, 0x04],
                revision: [0x05, 0x06, 0x07, 0x7F],
            })),
            [0xF002_0000, 0x0000_2109, 0x0102_0304, 0x0506_077F]
        );
        // Each byte sits under a reserved bit, which is ignored.
        assert_eq!(
            Packet::from_words(&[0xF002_FFFF, 0xFF80_A189, 0x8182_8384, 0x8586_87FF])
                .unwrap()
                .message(),
            Ok(Message::Stream(StreamMessage::DeviceIdentity(
                DeviceIdentity {
                    manufacturer: [0x00, 0x21, 0x09],
                    family: [0x01, 0x02],
                    model: [0x03, 0x04],
                    revision: [0x05, 0x06, 0x07, 0x7F],
                }
            )))
        );
    }

    #[test]
    fn stream_configuration_matches_its_figures() {
        // Figures 18 and 19.
        let configuration = StreamConfiguration {
            protocol: protocol::MIDI_2,
            receives_jr_timestamps: true,
            sends_jr_timestamps: false,
        };
        assert_eq!(
            words(StreamMessage::StreamConfigurationRequest(configuration)),
            [0xF005_0202, 0, 0, 0]
        );
        let configuration = StreamConfiguration {
            protocol: protocol::MIDI_1,
            receives_jr_timestamps: false,
            sends_jr_timestamps: true,
        };
        assert_eq!(
            words(StreamMessage::StreamConfigurationNotification(
                configuration
            )),
            [0xF006_0101, 0, 0, 0]
        );
    }

    #[test]
    fn function_block_messages_match_their_figures() {
        // Figure 20.
        assert_eq!(
            words(StreamMessage::FunctionBlockDiscovery {
                block: 0xFF,
                filter: function_block_filter::ALL,
            }),
            [0xF010_FF03, 0, 0, 0]
        );
        assert_eq!(function_block_filter::INFO, 0b01);
        assert_eq!(function_block_filter::NAME, 0b10);

        // Figure 22: active bit and block number, then reserved, user
        // interface hint, MIDI 1.0 and direction, two bits each.
        assert_eq!(
            words(StreamMessage::FunctionBlockInfo(FunctionBlockInfo {
                active: true,
                block: 31,
                ui_hint: 0b10,
                midi1: 0b01,
                direction: 0b11,
                first_group: 4,
                groups_spanned: 16,
                midi_ci_version: 2,
                max_sysex8_streams: 255,
            })),
            [0xF011_9F27, 0x0410_02FF, 0, 0]
        );
        assert_eq!(words(StreamMessage::StartOfClip), [0xF020_0000, 0, 0, 0]);
        assert_eq!(words(StreamMessage::EndOfClip), [0xF021_0000, 0, 0, 0]);
    }

    fn collect(packets: impl Iterator<Item = Packet>) -> (Vec<Part>, Vec<u8>) {
        let mut assembler = Assembler::new(128);
        let mut parts = Vec::new();
        let mut whole = Vec::new();
        for packet in packets {
            let (part, text) = match packet.message() {
                Ok(Message::Stream(StreamMessage::EndpointName(text)))
                | Ok(Message::Stream(StreamMessage::ProductInstanceId(text))) => {
                    (text.part, text.text().to_vec())
                }
                Ok(Message::Stream(StreamMessage::FunctionBlockName { block, name })) => {
                    assert_eq!(block, 7);
                    (name.part, name.text().to_vec())
                }
                other => panic!("unexpected {other:?}"),
            };
            parts.push(part);
            if let Some(message) = assembler.push(part, &text) {
                whole = message.to_vec();
            }
        }
        (parts, whole)
    }

    #[test]
    fn names_are_cut_into_packets_and_put_back_together() {
        // Figure 15: fourteen bytes of name after the status.
        let short: Vec<Packet> = endpoint_name_packets("Mangue").unwrap().collect();
        assert_eq!(short.len(), 1);
        assert_eq!(
            short[0].words(),
            &[0xF003_4D61, 0x6E67_7565, 0, 0],
            "'M' 'a' / 'n' 'g' 'u' 'e'"
        );

        let name = "Manguebyte virtual endpoint número 1";
        let (parts, whole) = collect(endpoint_name_packets(name).unwrap());
        assert_eq!(parts, [Part::Start, Part::Continue, Part::End]);
        assert_eq!(whole, name.as_bytes());
        // Exactly fourteen bytes is one complete packet with no padding.
        let (parts, whole) = collect(endpoint_name_packets("fourteen bytes").unwrap());
        assert_eq!(parts, [Part::Complete]);
        assert_eq!(whole, b"fourteen bytes");
        // 98 bytes is the limit: a start, five continues and an end.
        let longest = "n".repeat(98);
        let (parts, whole) = collect(endpoint_name_packets(&longest).unwrap());
        assert_eq!(parts.len(), 7);
        assert_eq!(whole.len(), 98);
        assert!(endpoint_name_packets(&"n".repeat(99)).is_err());
        assert!(endpoint_name_packets("a\0").is_err());

        // Figure 16: a product instance id is at most 42 printable bytes.
        let (parts, whole) = collect(product_instance_id_packets("SN-0042/2026 rev B").unwrap());
        assert_eq!(parts, [Part::Start, Part::End]);
        assert_eq!(whole, b"SN-0042/2026 rev B");
        assert_eq!(
            collect(product_instance_id_packets(&"9".repeat(42)).unwrap())
                .0
                .len(),
            3
        );
        assert!(product_instance_id_packets(&"9".repeat(43)).is_err());
        assert!(product_instance_id_packets("número").is_err());
        assert!(product_instance_id_packets("tab\there").is_err());

        // Figure 23: the block number takes one byte, leaving thirteen.
        let block: Vec<Packet> = function_block_name_packets(7, "Synth").unwrap().collect();
        assert_eq!(block[0].words(), &[0xF012_0753, 0x796E_7468, 0, 0]);
        let (parts, whole) = collect(function_block_name_packets(7, "thirteen bytes").unwrap());
        assert_eq!(parts, [Part::Start, Part::End]);
        assert_eq!(whole, b"thirteen bytes");
        assert_eq!(
            collect(function_block_name_packets(7, &"b".repeat(91)).unwrap())
                .0
                .len(),
            7
        );
        assert!(function_block_name_packets(7, &"b".repeat(92)).is_err());
    }

    #[test]
    fn text_pieces_keep_their_bytes() {
        for part in [Part::Complete, Part::Start, Part::Continue, Part::End] {
            let name = StreamText::<14>::new(part, b"abc").unwrap();
            assert_eq!(name.text(), b"abc");
            assert_eq!(name.bytes(), b"abc\0\0\0\0\0\0\0\0\0\0\0");
            words(StreamMessage::EndpointName(name));
            words(StreamMessage::ProductInstanceId(name));
            let full = StreamText::<13>::new(part, b"thirteen byte").unwrap();
            assert_eq!(full.text(), b"thirteen byte");
            words(StreamMessage::FunctionBlockName {
                block: 31,
                name: full,
            });
        }
        assert!(StreamText::<14>::new(Part::Complete, &[b'a'; 15]).is_err());
        assert!(StreamText::<13>::new(Part::Complete, &[b'a'; 14]).is_err());
    }

    #[test]
    fn malformed_stream_messages_are_rejected() {
        let read = |w0: u32| Packet::from_words(&[w0, 0, 0, 0]).unwrap().message();
        // Statuses with no definition, across the ten bits.
        for status in [0x007u32, 0x00F, 0x013, 0x01F, 0x022, 0x100, 0x3FF] {
            assert_eq!(
                read(0xF000_0000 | (status << 16)),
                Err(UmpError::UnknownStatus {
                    message_type: 0xF,
                    status: status as u16
                })
            );
        }
        // Messages of fixed size cannot be a start, continue or end.
        for status in [0x00u32, 0x01, 0x02, 0x05, 0x06, 0x10, 0x11, 0x20, 0x21] {
            for form in 1..4u32 {
                assert!(matches!(
                    read(0xF000_0000 | (form << 26) | (status << 16)),
                    Err(UmpError::Invalid(_))
                ));
            }
        }
        // Reserved filter bits are ignored.
        assert_eq!(
            Packet::from_words(&[0xF000_0101, 0xFFFF_FFFF, 0, 0])
                .unwrap()
                .message(),
            Ok(Message::Stream(StreamMessage::EndpointDiscovery {
                version_major: 1,
                version_minor: 1,
                filter: endpoint_filter::ALL
            }))
        );
    }
}
