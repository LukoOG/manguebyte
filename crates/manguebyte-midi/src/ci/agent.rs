//! A MIDI-CI device that an application drives: it answers what it is
//! asked and asks on the application's behalf, and does no I/O.
//!
//! An [`Agent`] is both sides of MIDI-CI at once, as most devices are: a
//! responder, which answers discovery, profile inquiries and requests to
//! turn its profiles on and off by itself, and an initiator, which the
//! application tells to discover, to inquire and to get and set
//! properties. The application:
//!
//! 1. hands every system exclusive message that arrives to
//!    [`receive`](Agent::receive), and acts on the [`AgentEvent`]s it
//!    returns;
//! 2. sends whatever [`outgoing`](Agent::outgoing) gives, on the port the
//!    messages came from;
//! 3. calls [`tick`](Agent::tick) now and then, so that property requests
//!    nobody answers time out.
//!
//! Property requests from others are not answered by the agent: they are
//! reported as [`AgentEvent::PropertyRequest`], and the application
//! replies with [`reply_property`](Agent::reply_property). The agent
//! cuts replies and requests into chunks that fit the receiver, and puts
//! chunked messages back together.
//!
//! Everything is bounded: at most [`MAX_PEERS`] peers are remembered, the
//! outgoing queue holds at most [`MAX_OUTGOING`] messages (the oldest is
//! dropped past that), and the property data being received is held by a
//! bounded [`Assembler`].
//!
//! ```
//! use manguebyte_midi::ci::agent::{Agent, AgentConfig, AgentEvent};
//! use manguebyte_midi::ci::{Address, Muid, ProfileId};
//!
//! let mut synth = Agent::new(Muid::from_seed(1), AgentConfig::default());
//! let organ = ProfileId::standard(0x21, 0x01, 0x01, 0x01);
//! synth.add_profile(Address::Channel(0), organ, false);
//! let mut controller = Agent::new(Muid::from_seed(2), AgentConfig::default());
//!
//! // What one says, the other hears, until both are quiet.
//! fn exchange(a: &mut Agent, b: &mut Agent, events: &mut Vec<AgentEvent>) {
//!     loop {
//!         let mut quiet = true;
//!         while let Some(bytes) = a.outgoing() {
//!             events.extend(b.receive(&bytes).unwrap_or_default());
//!             quiet = false;
//!         }
//!         while let Some(bytes) = b.outgoing() {
//!             events.extend(a.receive(&bytes).unwrap_or_default());
//!             quiet = false;
//!         }
//!         if quiet {
//!             return;
//!         }
//!     }
//! }
//!
//! let mut events = Vec::new();
//! controller.discover();
//! exchange(&mut controller, &mut synth, &mut events);
//! let peer = controller.peers()[0].muid;
//! assert_eq!(peer, synth.muid());
//!
//! controller.set_profile(peer, Address::Channel(0), organ, true, 0).unwrap();
//! exchange(&mut controller, &mut synth, &mut events);
//! assert!(synth.profile_enabled(Address::Channel(0), organ));
//! assert!(events.iter().any(|event| matches!(
//!     event,
//!     AgentEvent::ProfileChanged { enabled: true, .. }
//! )));
//! ```

use std::collections::VecDeque;
use std::time::Duration;

use super::property::{self, Assembled, Assembler, Progress};
use super::{
    Acknowledgement, Address, Categories, CiError, Header, Message, MessageReport, Muid, ProfileId,
    ProfileList, PropertyChunk, PropertyKind, nak, sub_id,
};
use crate::ump::DeviceIdentity;

/// The most peers an agent remembers. A discovery reply from one more
/// replaces the one heard from longest ago.
pub const MAX_PEERS: usize = 64;

/// The most messages waiting in the outgoing queue.
pub const MAX_OUTGOING: usize = 1_024;

/// How an agent presents itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    /// Who made the device.
    pub identity: DeviceIdentity,
    /// The categories it answers. Requests in others are answered with a
    /// NAK.
    pub categories: Categories,
    /// The largest system exclusive message it takes, framed, in bytes.
    pub max_sysex: u32,
    /// The function block it is, or `0x7F` for none.
    pub function_block: u8,
    /// The product instance id it gives when asked: up to 16 bytes of
    /// printable ASCII, or empty for none.
    pub product_instance_id: Vec<u8>,
    /// How many property requests it serves at once.
    pub simultaneous_properties: u8,
    /// The largest property message it puts back together, header and
    /// data, in bytes.
    pub max_property_bytes: usize,
    /// How long a property request it sent waits for its reply.
    pub property_timeout: Duration,
}

impl Default for AgentConfig {
    /// A device of the manufacturer id reserved for development and
    /// education (`0x7D`), with profiles and property exchange, messages
    /// of up to 4 KiB, and property requests that time out after three
    /// seconds.
    fn default() -> Self {
        Self {
            identity: DeviceIdentity {
                manufacturer: [0x7D, 0, 0],
                family: [0, 0],
                model: [0, 0],
                revision: [0, 0, 0, 0],
            },
            categories: Categories::PROFILES.with(Categories::PROPERTIES),
            max_sysex: 4_096,
            function_block: 0x7F,
            product_instance_id: Vec::new(),
            simultaneous_properties: 4,
            max_property_bytes: 1 << 20,
            property_timeout: Duration::from_secs(3),
        }
    }
}

/// A device the agent has heard from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    /// Its MUID.
    pub muid: Muid,
    /// Who made it.
    pub identity: DeviceIdentity,
    /// What it supports.
    pub categories: Categories,
    /// The largest system exclusive message it takes.
    pub max_sysex: u32,
    /// Its function block, or `0x7F`.
    pub function_block: u8,
    /// How many property requests it serves at once, once asked.
    pub simultaneous_properties: Option<u8>,
}

/// What [`Agent::receive`] and [`Agent::tick`] report.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A device answered a discovery, or announced itself with one.
    PeerDiscovered(Peer),
    /// A device's MUID is no longer in use, and it was forgotten.
    PeerInvalidated(Muid),
    /// Another device used this agent's MUID, so the agent took a new one
    /// and told everyone the old one is invalid.
    MuidChanged {
        /// The MUID given up.
        old: Muid,
        /// The new one.
        new: Muid,
    },
    /// A device answered a product instance id inquiry.
    ProductInstanceId {
        /// Who.
        peer: Muid,
        /// The id.
        id: Vec<u8>,
    },
    /// A device said which profiles it has on an address.
    ProfilesReported {
        /// Who.
        peer: Muid,
        /// Where.
        address: Address,
        /// The profiles that are on.
        enabled: Vec<ProfileId>,
        /// The profiles that are off.
        disabled: Vec<ProfileId>,
    },
    /// A device turned a profile on or off, perhaps at this agent's
    /// request.
    ProfileChanged {
        /// Who.
        peer: Muid,
        /// Where.
        address: Address,
        /// The profile.
        profile: ProfileId,
        /// Whether it is now on.
        enabled: bool,
        /// On how many channels.
        channels: u16,
    },
    /// A device gained or lost a profile.
    ProfileAvailability {
        /// Who.
        peer: Muid,
        /// Where.
        address: Address,
        /// The profile.
        profile: ProfileId,
        /// Whether it was added; otherwise removed.
        added: bool,
    },
    /// A device asked this agent to turn one of its profiles on or off,
    /// and the agent did. The application makes the profile's behaviour
    /// follow.
    ProfileSet {
        /// Who asked.
        from: Muid,
        /// Where.
        address: Address,
        /// The profile.
        profile: ProfileId,
        /// Whether it is now on.
        enabled: bool,
        /// On how many channels it was asked for.
        channels: u16,
    },
    /// Data for a profile arrived.
    ProfileData {
        /// Who sent it.
        peer: Muid,
        /// Where.
        address: Address,
        /// The profile.
        profile: ProfileId,
        /// The data.
        data: Vec<u8>,
    },
    /// A device said how many property requests it serves at once.
    PropertyCapabilities {
        /// Who.
        peer: Muid,
        /// How many.
        simultaneous: u8,
    },
    /// A device asks for a property, or sets one, or subscribes to one.
    /// Answer with [`Agent::reply_property`].
    PropertyRequest {
        /// Who asks.
        from: Muid,
        /// [`Get`](PropertyKind::Get), [`Set`](PropertyKind::Set) or
        /// [`Subscribe`](PropertyKind::Subscribe).
        kind: PropertyKind,
        /// To pass back with the reply.
        request_id: u8,
        /// The JSON header.
        header: Vec<u8>,
        /// The data, for a set.
        data: Vec<u8>,
    },
    /// A reply to a property request of this agent's arrived, whole.
    PropertyReply {
        /// Who answers.
        peer: Muid,
        /// [`GetReply`](PropertyKind::GetReply),
        /// [`SetReply`](PropertyKind::SetReply) or
        /// [`SubscribeReply`](PropertyKind::SubscribeReply).
        kind: PropertyKind,
        /// The request it answers.
        request_id: u8,
        /// The JSON header, which holds its status.
        header: Vec<u8>,
        /// The data, for a get.
        data: Vec<u8>,
    },
    /// A device notified a subscriber of a change.
    PropertyNotify {
        /// Who.
        peer: Muid,
        /// The subscription's request id.
        request_id: u8,
        /// The JSON header.
        header: Vec<u8>,
        /// The data.
        data: Vec<u8>,
    },
    /// A property request of this agent's got no reply in time, or was
    /// refused with a NAK.
    PropertyFailed {
        /// Who was asked.
        peer: Muid,
        /// The request.
        request_id: u8,
        /// The NAK status, or [`nak::TIMEOUT`] if nothing came.
        status: u8,
    },
    /// A device asked for a MIDI message report. The application sends
    /// the messages that describe its state, then calls
    /// [`Agent::end_message_report`].
    MessageReportRequested {
        /// Who asks.
        from: Muid,
        /// Where.
        address: Address,
        /// What it asks for.
        report: MessageReport,
    },
    /// A device acknowledged a message.
    Ack {
        /// Who.
        peer: Muid,
        /// The message type acknowledged.
        original: u8,
        /// Its status code.
        status: u8,
    },
    /// A device refused a message.
    Nak {
        /// Who.
        peer: Muid,
        /// The message type refused.
        original: u8,
        /// One of [`nak`].
        status: u8,
        /// What it said, if anything.
        text: Vec<u8>,
    },
}

/// A profile of this agent's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalProfile {
    address: Address,
    profile: ProfileId,
    enabled: bool,
    channels: u16,
}

/// A property request this agent sent and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Waiting {
    peer: Muid,
    kind: PropertyKind,
    request_id: u8,
    age: Duration,
}

/// A MIDI-CI device. See the [module documentation](self).
#[derive(Debug, Clone)]
pub struct Agent {
    muid: Muid,
    config: AgentConfig,
    peers: Vec<Peer>,
    profiles: Vec<LocalProfile>,
    outgoing: VecDeque<Vec<u8>>,
    waiting: Vec<Waiting>,
    assembler: Assembler,
    next_request: u8,
    /// Mixed into the next MUID taken after a collision.
    reseed: u64,
}

impl Agent {
    /// An agent that calls itself `muid`, which should be random: see
    /// [`Muid::from_seed`]. A MUID that is broadcast or reserved is
    /// replaced by one made from it.
    pub fn new(muid: Muid, config: AgentConfig) -> Self {
        let muid = if muid.is_assignable() {
            muid
        } else {
            Muid::from_seed(u64::from(muid.value()))
        };
        let assembler = Assembler::new(
            usize::from(config.simultaneous_properties.max(1)) * 2,
            config.max_property_bytes,
        );
        Self {
            muid,
            config,
            peers: Vec::new(),
            profiles: Vec::new(),
            outgoing: VecDeque::new(),
            waiting: Vec::new(),
            assembler,
            next_request: 0,
            reseed: u64::from(muid.value()),
        }
    }

    /// This agent's MUID.
    pub fn muid(&self) -> Muid {
        self.muid
    }

    /// How the agent presents itself.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// The devices heard from, oldest first.
    pub fn peers(&self) -> &[Peer] {
        &self.peers
    }

    /// The device with this MUID, if it has been heard from.
    pub fn peer(&self, muid: Muid) -> Option<&Peer> {
        self.peers.iter().find(|peer| peer.muid == muid)
    }

    /// The next message to send, as a whole system exclusive message with
    /// its `0xF0` and `0xF7`.
    pub fn outgoing(&mut self) -> Option<Vec<u8>> {
        self.outgoing.pop_front()
    }

    /// How many messages wait to be sent.
    pub fn outgoing_len(&self) -> usize {
        self.outgoing.len()
    }

    /// Offers a profile on `address`, on or off. Another device may turn
    /// it on and off. An agent with profiles should have
    /// [`Categories::PROFILES`] in its configuration.
    pub fn add_profile(&mut self, address: Address, profile: ProfileId, enabled: bool) {
        if let Some(existing) = self.local(address, profile) {
            existing.enabled = enabled;
            return;
        }
        self.profiles.push(LocalProfile {
            address,
            profile,
            enabled,
            channels: 0,
        });
        self.send(address, Muid::BROADCAST, &Message::ProfileAdded { profile });
    }

    /// Withdraws a profile, telling everyone.
    pub fn remove_profile(&mut self, address: Address, profile: ProfileId) {
        let before = self.profiles.len();
        self.profiles
            .retain(|local| !(local.address == address && local.profile == profile));
        if self.profiles.len() != before {
            self.send(
                address,
                Muid::BROADCAST,
                &Message::ProfileRemoved { profile },
            );
        }
    }

    /// Whether this agent's profile on `address` is on.
    pub fn profile_enabled(&self, address: Address, profile: ProfileId) -> bool {
        self.profiles
            .iter()
            .any(|local| local.address == address && local.profile == profile && local.enabled)
    }

    /// Turns this agent's own profile on or off, as the application
    /// decides, and tells everyone. Returns whether there is such a
    /// profile.
    pub fn enable_profile(&mut self, address: Address, profile: ProfileId, enabled: bool) -> bool {
        let Some(local) = self.local(address, profile) else {
            return false;
        };
        local.enabled = enabled;
        let channels = local.channels;
        self.report_profile(address, profile, enabled, channels);
        true
    }

    /// Asks every device who is there.
    pub fn discover(&mut self) {
        let message = Message::Discovery {
            identity: self.config.identity,
            categories: self.config.categories,
            max_sysex: self.config.max_sysex,
            output_path: 0,
        };
        self.send(Address::FunctionBlock, Muid::BROADCAST, &message);
    }

    /// Tells every device that this agent's MUID is no longer in use, and
    /// takes a new one, as a device does when it goes away or resets.
    /// Returns the new MUID.
    pub fn invalidate(&mut self) -> Muid {
        let old = self.muid;
        self.send(
            Address::FunctionBlock,
            Muid::BROADCAST,
            &Message::InvalidateMuid { target: old },
        );
        self.take_new_muid();
        self.muid
    }

    /// Asks a device for its product instance id.
    pub fn inquire_product_instance_id(&mut self, peer: Muid) {
        self.send(
            Address::FunctionBlock,
            peer,
            &Message::EndpointInquiry { status: 0 },
        );
    }

    /// Asks a device which profiles it has on `address`.
    pub fn inquire_profiles(&mut self, peer: Muid, address: Address) {
        self.send(address, peer, &Message::ProfileInquiry);
    }

    /// Asks a device to turn a profile on or off, on `channels` channels
    /// (zero for the profile's default). Its report arrives as
    /// [`AgentEvent::ProfileChanged`], or a refusal as
    /// [`AgentEvent::Nak`].
    pub fn set_profile(
        &mut self,
        peer: Muid,
        address: Address,
        profile: ProfileId,
        on: bool,
        channels: u16,
    ) -> Result<(), CiError> {
        let message = if on {
            Message::SetProfileOn { profile, channels }
        } else {
            Message::SetProfileOff { profile }
        };
        self.try_send(address, peer, &message)
    }

    /// Sends data for a profile to a device.
    pub fn send_profile_data(
        &mut self,
        peer: Muid,
        address: Address,
        profile: ProfileId,
        data: &[u8],
    ) -> Result<(), CiError> {
        self.try_send(
            address,
            peer,
            &Message::ProfileSpecificData { profile, data },
        )
    }

    /// Asks a device how many property requests it serves at once.
    pub fn inquire_property_capabilities(&mut self, peer: Muid) {
        let message = Message::PropertyCapabilities {
            simultaneous: self.config.simultaneous_properties.max(1),
            major: 0,
            minor: 0,
        };
        self.send(Address::FunctionBlock, peer, &message);
    }

    /// Asks a device for a property, described by a JSON `header` such as
    /// `{"resource":"DeviceInfo"}`. Returns the request id the reply will
    /// carry.
    pub fn get_property(&mut self, peer: Muid, header: &[u8]) -> Result<u8, CiError> {
        self.request_property(peer, PropertyKind::Get, header, &[])
    }

    /// Sets a property of a device. The data is cut into chunks that fit
    /// the device. Returns the request id the reply will carry.
    pub fn set_property(&mut self, peer: Muid, header: &[u8], data: &[u8]) -> Result<u8, CiError> {
        self.request_property(peer, PropertyKind::Set, header, data)
    }

    /// Subscribes to, or ends a subscription to, a property of a device,
    /// as the JSON `header` says. Returns the request id the reply will
    /// carry.
    pub fn subscribe_property(&mut self, peer: Muid, header: &[u8]) -> Result<u8, CiError> {
        self.request_property(peer, PropertyKind::Subscribe, header, &[])
    }

    /// Answers a [`AgentEvent::PropertyRequest`]: `kind` is the request's
    /// kind, and the reply's header and data follow it, cut into chunks
    /// that fit the device.
    pub fn reply_property(
        &mut self,
        to: Muid,
        kind: PropertyKind,
        request_id: u8,
        header: &[u8],
        data: &[u8],
    ) -> Result<(), CiError> {
        let reply = kind
            .reply()
            .ok_or(CiError::Invalid("only an inquiry has a reply"))?;
        self.send_property(to, reply, request_id, header, data)
    }

    /// Tells a subscriber that a property changed.
    pub fn notify_property(
        &mut self,
        to: Muid,
        request_id: u8,
        header: &[u8],
        data: &[u8],
    ) -> Result<(), CiError> {
        self.send_property(to, PropertyKind::Notify, request_id, header, data)
    }

    /// Ends a MIDI message report that [`AgentEvent::MessageReportRequested`]
    /// started.
    pub fn end_message_report(&mut self, to: Muid, address: Address) {
        self.send(address, to, &Message::MessageReportEnd);
    }

    /// Lets `elapsed` pass, and reports the property requests of this
    /// agent's that have waited longer than the configured timeout.
    pub fn tick(&mut self, elapsed: Duration) -> Vec<AgentEvent> {
        let timeout = self.config.property_timeout;
        let mut events = Vec::new();
        self.waiting.retain_mut(|waiting| {
            waiting.age = waiting.age.saturating_add(elapsed);
            if waiting.age > timeout {
                events.push(AgentEvent::PropertyFailed {
                    peer: waiting.peer,
                    request_id: waiting.request_id,
                    status: nak::TIMEOUT,
                });
                false
            } else {
                true
            }
        });
        events
    }

    /// Takes one system exclusive message, with or without its `0xF0` and
    /// `0xF7`, answers it if it asks something of this agent, and reports
    /// what it says.
    ///
    /// A message that is not MIDI-CI is an error; so is one that is
    /// malformed, which the agent answers with a NAK when it can tell who
    /// sent it. A message addressed to another device, or sent by this
    /// agent itself, is ignored.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<Vec<AgentEvent>, CiError> {
        let mut events = Vec::new();
        let (header, message) = match super::parse(bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.nak_malformed(bytes, error);
                return Err(error);
            }
        };
        if header.destination != self.muid && !header.destination.is_broadcast() {
            return Ok(events);
        }
        if header.source == self.muid {
            // Our own MUID from someone else: one of us has to move,
            // and this agent does.
            events.push(self.move_muid());
            return Ok(events);
        }
        self.handle(header, message, &mut events);
        Ok(events)
    }

    fn handle(&mut self, header: Header, message: Message<'_>, events: &mut Vec<AgentEvent>) {
        let from = header.source;
        let address = header.address;
        let answers =
            |agent: &Self, category: Categories| agent.config.categories.contains(category);
        match message {
            Message::Discovery {
                identity,
                categories,
                max_sysex,
                output_path,
            } => {
                let peer = Peer {
                    muid: from,
                    identity,
                    categories,
                    max_sysex,
                    function_block: 0x7F,
                    simultaneous_properties: None,
                };
                self.remember(peer, events);
                let reply = Message::DiscoveryReply {
                    identity: self.config.identity,
                    categories: self.config.categories,
                    max_sysex: self.config.max_sysex,
                    output_path,
                    function_block: self.config.function_block,
                };
                self.send(Address::FunctionBlock, from, &reply);
            }
            Message::DiscoveryReply {
                identity,
                categories,
                max_sysex,
                function_block,
                ..
            } => {
                let peer = Peer {
                    muid: from,
                    identity,
                    categories,
                    max_sysex,
                    function_block,
                    simultaneous_properties: None,
                };
                self.remember(peer, events);
            }
            Message::EndpointInquiry { status: 0 }
                if !self.config.product_instance_id.is_empty() =>
            {
                let id = self.config.product_instance_id.clone();
                let reply = Message::EndpointReply {
                    status: 0,
                    data: &id,
                };
                self.send(address, from, &reply);
            }
            Message::EndpointInquiry { .. } => {
                self.nak(address, from, sub_id::ENDPOINT_INQUIRY, nak::NOT_SUPPORTED);
            }
            Message::EndpointReply { status: 0, data } => {
                events.push(AgentEvent::ProductInstanceId {
                    peer: from,
                    id: data.to_vec(),
                });
            }
            Message::InvalidateMuid { target } => {
                if target == self.muid {
                    // Another device says this agent's MUID is taken.
                    events.push(self.move_muid());
                } else if self.forget(target) {
                    events.push(AgentEvent::PeerInvalidated(target));
                }
            }
            Message::Ack(ack) => events.push(AgentEvent::Ack {
                peer: from,
                original: ack.original,
                status: ack.status,
            }),
            Message::Nak(refusal) => {
                // A NAK of a property request ends the oldest one of that
                // kind to that device: a NAK does not say which request.
                let refused = self.waiting.iter().position(|waiting| {
                    waiting.peer == from && waiting.kind.sub_id() == refusal.original
                });
                if let Some(index) = refused {
                    let waiting = self.waiting.remove(index);
                    events.push(AgentEvent::PropertyFailed {
                        peer: from,
                        request_id: waiting.request_id,
                        status: refusal.status,
                    });
                }
                events.push(AgentEvent::Nak {
                    peer: from,
                    original: refusal.original,
                    status: refusal.status,
                    text: refusal.text.to_vec(),
                });
            }
            Message::InitiateProtocolNegotiation { .. }
            | Message::SetNewProtocol { .. }
            | Message::TestNewProtocol { .. }
            | Message::ConfirmNewProtocol { .. } => {
                // Deprecated by MIDI-CI 1.2: protocols are chosen with UMP
                // stream messages instead.
                self.nak(
                    address,
                    from,
                    super::sub_id_of(&message),
                    nak::NOT_SUPPORTED,
                );
            }
            Message::ProfileInquiry if answers(self, Categories::PROFILES) => {
                let mut enabled = Vec::new();
                let mut disabled = Vec::new();
                for local in self
                    .profiles
                    .iter()
                    .filter(|local| local.address == address)
                {
                    let list = if local.enabled {
                        &mut enabled
                    } else {
                        &mut disabled
                    };
                    list.extend_from_slice(&local.profile.0);
                }
                let reply = Message::ProfileInquiryReply {
                    enabled: ProfileList::new(&enabled).unwrap_or(ProfileList::EMPTY),
                    disabled: ProfileList::new(&disabled).unwrap_or(ProfileList::EMPTY),
                };
                self.send(address, from, &reply);
            }
            Message::ProfileInquiryReply { enabled, disabled } => {
                events.push(AgentEvent::ProfilesReported {
                    peer: from,
                    address,
                    enabled: enabled.iter().collect(),
                    disabled: disabled.iter().collect(),
                });
            }
            Message::SetProfileOn { profile, channels } => {
                self.set_local(from, address, profile, true, channels, events);
            }
            Message::SetProfileOff { profile } => {
                self.set_local(from, address, profile, false, 0, events);
            }
            Message::ProfileEnabled { profile, channels } => {
                events.push(AgentEvent::ProfileChanged {
                    peer: from,
                    address,
                    profile,
                    enabled: true,
                    channels,
                })
            }
            Message::ProfileDisabled { profile, channels } => {
                events.push(AgentEvent::ProfileChanged {
                    peer: from,
                    address,
                    profile,
                    enabled: false,
                    channels,
                })
            }
            Message::ProfileAdded { profile } | Message::ProfileRemoved { profile } => {
                events.push(AgentEvent::ProfileAvailability {
                    peer: from,
                    address,
                    profile,
                    added: matches!(message, Message::ProfileAdded { .. }),
                });
            }
            Message::ProfileSpecificData { profile, data } => {
                events.push(AgentEvent::ProfileData {
                    peer: from,
                    address,
                    profile,
                    data: data.to_vec(),
                })
            }
            Message::PropertyCapabilities { .. } if answers(self, Categories::PROPERTIES) => {
                let reply = Message::PropertyCapabilitiesReply {
                    simultaneous: self.config.simultaneous_properties.max(1),
                    major: 0,
                    minor: 0,
                };
                self.send(address, from, &reply);
            }
            Message::PropertyCapabilitiesReply { simultaneous, .. } => {
                if let Some(peer) = self.peers.iter_mut().find(|peer| peer.muid == from) {
                    peer.simultaneous_properties = Some(simultaneous);
                }
                events.push(AgentEvent::PropertyCapabilities {
                    peer: from,
                    simultaneous,
                });
            }
            Message::Property(kind, chunk) => self.property(from, address, kind, &chunk, events),
            Message::ProcessCapabilities if answers(self, Categories::PROCESS_INQUIRY) => {
                self.send(
                    address,
                    from,
                    &Message::ProcessCapabilitiesReply { features: 0x01 },
                );
            }
            Message::MessageReport(report) if answers(self, Categories::PROCESS_INQUIRY) => {
                // Everything asked for is offered; the application sends
                // what it has.
                self.send(address, from, &Message::MessageReportReply(report));
                events.push(AgentEvent::MessageReportRequested {
                    from,
                    address,
                    report,
                });
            }
            Message::ProfileInquiry
            | Message::PropertyCapabilities { .. }
            | Message::ProcessCapabilities
            | Message::MessageReport(_)
            | Message::ProfileDetailsInquiry { .. } => {
                self.nak(
                    address,
                    from,
                    super::sub_id_of(&message),
                    nak::NOT_SUPPORTED,
                );
            }
            // Replies this agent has no use for.
            _ => {}
        }
    }

    fn property(
        &mut self,
        from: Muid,
        address: Address,
        kind: PropertyKind,
        chunk: &PropertyChunk<'_>,
        events: &mut Vec<AgentEvent>,
    ) {
        let inquiry = kind.reply().is_some();
        if inquiry && !self.config.categories.contains(Categories::PROPERTIES) {
            self.nak(address, from, kind.sub_id(), nak::NOT_SUPPORTED);
            return;
        }
        let assembled = match self.assembler.push(from, kind, chunk) {
            Progress::Pending => return,
            Progress::Complete(assembled) => assembled,
            Progress::OutOfSequence => {
                self.nak(address, from, kind.sub_id(), nak::CHUNK_OUT_OF_SEQUENCE);
                return;
            }
            Progress::TooLarge => {
                self.nak(address, from, kind.sub_id(), nak::TERMINATE);
                return;
            }
        };
        let Assembled {
            request_id,
            header,
            data,
            ..
        } = assembled;
        if inquiry {
            events.push(AgentEvent::PropertyRequest {
                from,
                kind,
                request_id,
                header,
                data,
            });
        } else if kind == PropertyKind::Notify {
            events.push(AgentEvent::PropertyNotify {
                peer: from,
                request_id,
                header,
                data,
            });
        } else {
            let asked = self.waiting.iter().position(|waiting| {
                waiting.peer == from
                    && waiting.request_id == request_id
                    && waiting.kind.reply() == Some(kind)
            });
            // A reply nobody asked for is dropped.
            if let Some(index) = asked {
                self.waiting.swap_remove(index);
                events.push(AgentEvent::PropertyReply {
                    peer: from,
                    kind,
                    request_id,
                    header,
                    data,
                });
            }
        }
    }

    fn request_property(
        &mut self,
        peer: Muid,
        kind: PropertyKind,
        header: &[u8],
        data: &[u8],
    ) -> Result<u8, CiError> {
        // A request id is in use until its reply arrives or times out.
        let free = (0..128u16)
            .map(|step| ((u16::from(self.next_request) + step) % 128) as u8)
            .find(|id| self.waiting.iter().all(|waiting| waiting.request_id != *id))
            .ok_or(CiError::Invalid("every request id is waiting for a reply"))?;
        self.send_property(peer, kind, free, header, data)?;
        self.next_request = (free + 1) % 128;
        self.waiting.push(Waiting {
            peer,
            kind,
            request_id: free,
            age: Duration::ZERO,
        });
        Ok(free)
    }

    fn send_property(
        &mut self,
        to: Muid,
        kind: PropertyKind,
        request_id: u8,
        header: &[u8],
        data: &[u8],
    ) -> Result<(), CiError> {
        if request_id > 0x7F {
            return Err(CiError::NotSevenBit);
        }
        // What the receiver takes, and never more than this agent's own
        // limit for a message it builds.
        let limit = self
            .peer(to)
            .map_or(self.config.max_sysex, |peer| peer.max_sysex)
            .clamp(128, 1 << 20) as usize;
        let mut messages = Vec::new();
        for chunk in property::chunks(request_id, header, data, limit)? {
            let mut bytes = Vec::new();
            super::encode_sysex(
                &Header::new(Address::FunctionBlock, self.muid, to),
                &Message::Property(kind, chunk),
                &mut bytes,
            )?;
            messages.push(bytes);
        }
        for bytes in messages {
            self.push_outgoing(bytes);
        }
        Ok(())
    }

    fn set_local(
        &mut self,
        from: Muid,
        address: Address,
        profile: ProfileId,
        enabled: bool,
        channels: u16,
        events: &mut Vec<AgentEvent>,
    ) {
        let original = if enabled {
            sub_id::SET_PROFILE_ON
        } else {
            sub_id::SET_PROFILE_OFF
        };
        if !self.config.categories.contains(Categories::PROFILES) {
            self.nak(address, from, original, nak::NOT_SUPPORTED);
            return;
        }
        let Some(local) = self.local(address, profile) else {
            self.nak(address, from, original, nak::PROFILE_NOT_SUPPORTED);
            return;
        };
        local.enabled = enabled;
        local.channels = if enabled { channels } else { 0 };
        self.report_profile(address, profile, enabled, channels);
        events.push(AgentEvent::ProfileSet {
            from,
            address,
            profile,
            enabled,
            channels,
        });
    }

    /// Tells everyone a profile is now on or off (M2-102-U: reports go to
    /// the broadcast MUID).
    fn report_profile(
        &mut self,
        address: Address,
        profile: ProfileId,
        enabled: bool,
        channels: u16,
    ) {
        let report = if enabled {
            Message::ProfileEnabled { profile, channels }
        } else {
            Message::ProfileDisabled { profile, channels }
        };
        self.send(address, Muid::BROADCAST, &report);
    }

    fn local(&mut self, address: Address, profile: ProfileId) -> Option<&mut LocalProfile> {
        self.profiles
            .iter_mut()
            .find(|local| local.address == address && local.profile == profile)
    }

    fn remember(&mut self, peer: Peer, events: &mut Vec<AgentEvent>) {
        if let Some(known) = self.peers.iter_mut().find(|known| known.muid == peer.muid) {
            let simultaneous = known.simultaneous_properties;
            *known = Peer {
                simultaneous_properties: simultaneous,
                ..peer
            };
        } else {
            if self.peers.len() >= MAX_PEERS {
                let gone = self.peers.remove(0);
                self.assembler.forget(gone.muid);
            }
            self.peers.push(peer);
        }
        events.push(AgentEvent::PeerDiscovered(peer));
    }

    fn forget(&mut self, muid: Muid) -> bool {
        let before = self.peers.len();
        self.peers.retain(|peer| peer.muid != muid);
        self.waiting.retain(|waiting| waiting.peer != muid);
        self.assembler.forget(muid);
        self.peers.len() != before
    }

    /// Gives up this agent's MUID, telling everyone, and reports it.
    fn move_muid(&mut self) -> AgentEvent {
        let old = self.muid;
        let new = self.invalidate();
        AgentEvent::MuidChanged { old, new }
    }

    fn take_new_muid(&mut self) {
        // A MUID nobody known has. Twenty-eight random bits make a second
        // try all but certain to do; the count only keeps this bounded.
        for _ in 0..64 {
            self.reseed = self.reseed.wrapping_add(0x9E37_79B9);
            let candidate = Muid::from_seed(self.reseed);
            if candidate != self.muid && self.peers.iter().all(|peer| peer.muid != candidate) {
                self.muid = candidate;
                return;
            }
        }
    }

    fn nak(&mut self, address: Address, to: Muid, original: u8, status: u8) {
        self.send(
            address,
            to,
            &Message::Nak(Acknowledgement::nak(original, status)),
        );
    }

    /// Answers a message that could not be read with a NAK, if its header
    /// says who sent it and that it was meant for this agent.
    fn nak_malformed(&mut self, bytes: &[u8], error: CiError) {
        if matches!(error, CiError::NotMidiCi | CiError::NotSevenBit) {
            return;
        }
        let body = bytes.strip_prefix(&[0xF0]).unwrap_or(bytes);
        let Some(head) = body.get(..13) else {
            return;
        };
        let muid = |bytes: &[u8]| {
            bytes
                .iter()
                .rev()
                .fold(0u32, |value, byte| (value << 7) | u32::from(*byte & 0x7F))
        };
        let (Some(source), Some(destination)) =
            (Muid::new(muid(&head[5..9])), Muid::new(muid(&head[9..13])))
        else {
            return;
        };
        if destination != self.muid || !source.is_assignable() {
            return;
        }
        let address = match head[1] {
            channel @ 0x00..=0x0F => Address::Channel(channel),
            0x7E => Address::Group,
            _ => Address::FunctionBlock,
        };
        self.nak(address, source, head[3] & 0x7F, nak::MALFORMED);
    }

    fn try_send(
        &mut self,
        address: Address,
        to: Muid,
        message: &Message<'_>,
    ) -> Result<(), CiError> {
        let mut bytes = Vec::new();
        super::encode_sysex(&Header::new(address, self.muid, to), message, &mut bytes)?;
        self.push_outgoing(bytes);
        Ok(())
    }

    /// Queues a message the agent builds itself, which always encodes.
    fn send(&mut self, address: Address, to: Muid, message: &Message<'_>) {
        let _ = self.try_send(address, to, message);
    }

    fn push_outgoing(&mut self, bytes: Vec<u8>) {
        if self.outgoing.len() >= MAX_OUTGOING {
            self.outgoing.pop_front();
        }
        self.outgoing.push_back(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci;

    const ORGAN: ProfileId = ProfileId::standard(0x21, 0x01, 0x01, 0x01);
    const MPE: ProfileId = ProfileId::standard(0x31, 0x01, 0x01, 0x01);

    /// Carries messages between two agents until both are quiet, and
    /// returns what each reported.
    fn exchange(a: &mut Agent, b: &mut Agent) -> (Vec<AgentEvent>, Vec<AgentEvent>) {
        let (mut heard_by_a, mut heard_by_b) = (Vec::new(), Vec::new());
        for _ in 0..10_000 {
            let mut quiet = true;
            while let Some(bytes) = a.outgoing() {
                heard_by_b.extend(b.receive(&bytes).unwrap());
                quiet = false;
            }
            while let Some(bytes) = b.outgoing() {
                heard_by_a.extend(a.receive(&bytes).unwrap());
                quiet = false;
            }
            if quiet {
                return (heard_by_a, heard_by_b);
            }
        }
        panic!("the two agents never stopped talking");
    }

    fn pair() -> (Agent, Agent) {
        let controller = Agent::new(Muid::from_seed(10), AgentConfig::default());
        let synth = Agent::new(
            Muid::from_seed(20),
            AgentConfig {
                product_instance_id: b"SYNTH-0042".to_vec(),
                max_sysex: 256,
                categories: Categories::PROFILES
                    .with(Categories::PROPERTIES)
                    .with(Categories::PROCESS_INQUIRY),
                ..AgentConfig::default()
            },
        );
        (controller, synth)
    }

    #[test]
    fn discovery_introduces_both_sides() {
        let (mut controller, mut synth) = pair();
        controller.discover();
        let (to_controller, to_synth) = exchange(&mut controller, &mut synth);
        assert!(
            matches!(to_controller[..], [AgentEvent::PeerDiscovered(peer)] if peer.muid == synth.muid())
        );
        assert!(
            matches!(to_synth[..], [AgentEvent::PeerDiscovered(peer)] if peer.muid == controller.muid())
        );
        assert_eq!(controller.peer(synth.muid()).unwrap().max_sysex, 256);

        controller.inquire_product_instance_id(synth.muid());
        let (events, _) = exchange(&mut controller, &mut synth);
        assert_eq!(
            events,
            [AgentEvent::ProductInstanceId {
                peer: synth.muid(),
                id: b"SYNTH-0042".to_vec()
            }]
        );
        // The controller has no id to give.
        synth.inquire_product_instance_id(controller.muid());
        let (_, events) = exchange(&mut controller, &mut synth);
        assert!(matches!(
            events[..],
            [AgentEvent::Nak {
                status: nak::NOT_SUPPORTED,
                ..
            }]
        ));
    }

    #[test]
    fn profiles_are_listed_turned_on_and_off_and_refused() {
        let (mut controller, mut synth) = pair();
        let channel = Address::Channel(0);
        synth.add_profile(channel, ORGAN, false);
        synth.add_profile(channel, MPE, true);
        synth.add_profile(Address::Group, ORGAN, true);
        controller.discover();
        let (heard, _) = exchange(&mut controller, &mut synth);
        // The additions were announced to everyone.
        assert_eq!(
            heard
                .iter()
                .filter(|event| matches!(
                    event,
                    AgentEvent::ProfileAvailability { added: true, .. }
                ))
                .count(),
            3
        );
        let peer = synth.muid();

        controller.inquire_profiles(peer, channel);
        let (events, _) = exchange(&mut controller, &mut synth);
        assert_eq!(
            events,
            [AgentEvent::ProfilesReported {
                peer,
                address: channel,
                enabled: vec![MPE],
                disabled: vec![ORGAN],
            }]
        );

        controller
            .set_profile(peer, channel, ORGAN, true, 0)
            .unwrap();
        let (to_controller, to_synth) = exchange(&mut controller, &mut synth);
        assert!(synth.profile_enabled(channel, ORGAN));
        assert!(matches!(
            to_synth[..],
            [AgentEvent::ProfileSet {
                enabled: true,
                profile: ORGAN,
                ..
            }]
        ));
        assert!(matches!(
            to_controller[..],
            [AgentEvent::ProfileChanged {
                enabled: true,
                profile: ORGAN,
                ..
            }]
        ));
        controller
            .set_profile(peer, channel, ORGAN, false, 0)
            .unwrap();
        exchange(&mut controller, &mut synth);
        assert!(!synth.profile_enabled(channel, ORGAN));

        // A profile the synth does not have, or not on that address.
        controller
            .set_profile(peer, Address::Channel(4), ORGAN, true, 0)
            .unwrap();
        let (events, _) = exchange(&mut controller, &mut synth);
        assert!(matches!(
            events[..],
            [AgentEvent::Nak {
                status: nak::PROFILE_NOT_SUPPORTED,
                original: sub_id::SET_PROFILE_ON,
                ..
            }]
        ));

        // The application turns its own profile off, and withdraws one.
        assert!(synth.enable_profile(channel, MPE, false));
        synth.remove_profile(Address::Group, ORGAN);
        let (events, _) = exchange(&mut controller, &mut synth);
        assert!(matches!(
            events[..],
            [
                AgentEvent::ProfileChanged {
                    enabled: false,
                    profile: MPE,
                    ..
                },
                AgentEvent::ProfileAvailability {
                    added: false,
                    profile: ORGAN,
                    ..
                },
            ]
        ));
    }

    #[test]
    fn properties_travel_in_chunks_both_ways() {
        let (mut controller, mut synth) = pair();
        controller.discover();
        exchange(&mut controller, &mut synth);
        let peer = synth.muid();

        controller.inquire_property_capabilities(peer);
        let (events, _) = exchange(&mut controller, &mut synth);
        assert_eq!(
            events,
            [AgentEvent::PropertyCapabilities {
                peer,
                simultaneous: 4
            }]
        );

        // A get, answered with more than fits in one of the controller's
        // messages... the controller takes 4 KiB, so make it longer.
        let programs: Vec<u8> = (0..10_000u32)
            .map(|index| b'a' + (index % 26) as u8)
            .collect();
        let id = controller
            .get_property(peer, br#"{"resource":"ProgramList"}"#)
            .unwrap();
        let (_, requests) = exchange(&mut controller, &mut synth);
        let [
            AgentEvent::PropertyRequest {
                from,
                kind,
                request_id,
                ref header,
                ..
            },
        ] = requests[..]
        else {
            panic!("{requests:?}")
        };
        assert_eq!(
            (from, kind, request_id),
            (controller.muid(), PropertyKind::Get, id)
        );
        assert_eq!(header, br#"{"resource":"ProgramList"}"#);
        synth
            .reply_property(from, kind, request_id, br#"{"status":200}"#, &programs)
            .unwrap();
        assert!(
            synth.outgoing_len() > 2,
            "the reply was not cut into chunks"
        );
        let (replies, _) = exchange(&mut controller, &mut synth);
        assert_eq!(
            replies,
            [AgentEvent::PropertyReply {
                peer,
                kind: PropertyKind::GetReply,
                request_id: id,
                header: br#"{"status":200}"#.to_vec(),
                data: programs.clone(),
            }]
        );

        // A set, cut to the synth's 256 bytes and put back together.
        let header = br#"{"resource":"X"}"#;
        let id = controller.set_property(peer, header, &programs).unwrap();
        let mut messages = 0;
        let mut requests = Vec::new();
        while let Some(bytes) = controller.outgoing() {
            assert!(bytes.len() <= 256, "{} bytes", bytes.len());
            messages += 1;
            requests.extend(synth.receive(&bytes).unwrap());
        }
        assert!(messages > 40, "{messages} messages");
        let [
            AgentEvent::PropertyRequest {
                kind: PropertyKind::Set,
                request_id,
                ref data,
                ..
            },
        ] = requests[..]
        else {
            panic!("{requests:?}")
        };
        assert_eq!((request_id, data), (id, &programs));
        synth
            .reply_property(
                controller.muid(),
                PropertyKind::Set,
                id,
                br#"{"status":200}"#,
                &[],
            )
            .unwrap();
        let (replies, _) = exchange(&mut controller, &mut synth);
        assert!(matches!(
            replies[..],
            [AgentEvent::PropertyReply { kind: PropertyKind::SetReply, request_id, .. }] if request_id == id
        ));

        // A chunk lost on the way: the rest are out of sequence, the synth
        // refuses them, and the request fails without a timeout.
        let id = controller.set_property(peer, header, &programs).unwrap();
        let _lost = controller.outgoing();
        let (heard, to_synth) = exchange(&mut controller, &mut synth);
        assert!(to_synth.is_empty());
        assert!(heard.contains(&AgentEvent::PropertyFailed {
            peer,
            request_id: id,
            status: nak::CHUNK_OUT_OF_SEQUENCE
        }));
        assert!(controller.tick(Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn unanswered_requests_time_out_and_refused_ones_fail() {
        let (mut controller, mut synth) = pair();
        controller.discover();
        exchange(&mut controller, &mut synth);
        let peer = synth.muid();
        let id = controller.get_property(peer, b"{}").unwrap();
        // Nobody carries the message.
        while controller.outgoing().is_some() {}
        assert!(controller.tick(Duration::from_secs(2)).is_empty());
        assert_eq!(
            controller.tick(Duration::from_secs(2)),
            [AgentEvent::PropertyFailed {
                peer,
                request_id: id,
                status: nak::TIMEOUT
            }]
        );

        // A synth without property exchange refuses.
        let mut plain = Agent::new(
            Muid::from_seed(30),
            AgentConfig {
                categories: Categories::PROFILES,
                ..AgentConfig::default()
            },
        );
        controller.discover();
        exchange(&mut controller, &mut plain);
        let id = controller.get_property(plain.muid(), b"{}").unwrap();
        let (events, _) = exchange(&mut controller, &mut plain);
        assert!(events.contains(&AgentEvent::PropertyFailed {
            peer: plain.muid(),
            request_id: id,
            status: nak::NOT_SUPPORTED
        }));
        assert!(controller.tick(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn a_muid_collision_and_an_invalidation() {
        let (mut controller, mut synth) = pair();
        controller.discover();
        exchange(&mut controller, &mut synth);
        // A third device with the synth's MUID speaks: the synth hears its
        // own MUID from elsewhere and moves.
        let mut twin = Agent::new(synth.muid(), AgentConfig::default());
        twin.discover();
        let old = synth.muid();
        let mut events = Vec::new();
        while let Some(bytes) = twin.outgoing() {
            events.extend(synth.receive(&bytes).unwrap());
        }
        assert!(
            matches!(events[..], [AgentEvent::MuidChanged { old: was, new }] if was == old && new != old)
        );
        // Its invalidation reaches the controller, which forgets the old
        // MUID.
        let (heard, _) = exchange(&mut controller, &mut synth);
        assert!(heard.contains(&AgentEvent::PeerInvalidated(old)));
        assert!(controller.peer(old).is_none());
    }

    #[test]
    fn deprecated_and_unsupported_inquiries_get_a_nak() {
        let (mut controller, mut synth) = pair();
        let header = Header::new(Address::FunctionBlock, controller.muid(), synth.muid());
        let mut bytes = Vec::new();
        ci::encode_sysex(
            &header,
            &Message::InitiateProtocolNegotiation {
                authority: 0x10,
                protocols: &[2, 0, 0, 0, 0],
            },
            &mut bytes,
        )
        .unwrap();
        synth.receive(&bytes).unwrap();
        let reply = synth.outgoing().unwrap();
        let (_, Message::Nak(refusal)) = ci::parse(&reply).unwrap() else {
            panic!()
        };
        assert_eq!(refusal.original, sub_id::INITIATE_PROTOCOL_NEGOTIATION);

        // A malformed message meant for the synth: a NAK saying so.
        let mut cut = Vec::new();
        ci::encode(
            &header,
            &Message::SetProfileOn {
                profile: ORGAN,
                channels: 0,
            },
            &mut cut,
        )
        .unwrap();
        cut.truncate(15);
        assert_eq!(synth.receive(&cut), Err(CiError::Truncated));
        let reply = synth.outgoing().unwrap();
        let (_, Message::Nak(refusal)) = ci::parse(&reply).unwrap() else {
            panic!()
        };
        assert_eq!(refusal.status, nak::MALFORMED);

        // A message for someone else is none of its business.
        let elsewhere = Header::new(
            Address::FunctionBlock,
            controller.muid(),
            Muid::new(5).unwrap(),
        );
        let mut bytes = Vec::new();
        ci::encode(&elsewhere, &Message::ProfileInquiry, &mut bytes).unwrap();
        assert_eq!(synth.receive(&bytes), Ok(Vec::new()));
        assert!(synth.outgoing().is_none());
        let _ = &mut controller;
    }

    #[test]
    fn a_message_report_is_offered_and_ended() {
        let (mut controller, mut synth) = pair();
        let header = Header::new(Address::Channel(2), controller.muid(), synth.muid());
        let mut bytes = Vec::new();
        let report = MessageReport {
            data_control: 0x7F,
            system: 1,
            channel_controllers: 2,
            note_data: 3,
        };
        ci::encode(&header, &Message::MessageReport(report), &mut bytes).unwrap();
        let events = synth.receive(&bytes).unwrap();
        assert_eq!(
            events,
            [AgentEvent::MessageReportRequested {
                from: controller.muid(),
                address: Address::Channel(2),
                report
            }]
        );
        synth.end_message_report(controller.muid(), Address::Channel(2));
        let replies: Vec<_> = core::iter::from_fn(|| synth.outgoing()).collect();
        assert!(
            matches!(ci::parse(&replies[0]).unwrap().1, Message::MessageReportReply(r) if r == report)
        );
        assert_eq!(ci::parse(&replies[1]).unwrap().1, Message::MessageReportEnd);
        let _ = controller.outgoing();
    }

    #[test]
    fn hostile_traffic_is_bounded() {
        let (_, mut synth) = pair();
        // Discoveries from more devices than it remembers, and chunks that
        // never end, from as many.
        for seed in 0..(MAX_PEERS as u64 * 3) {
            let muid = Muid::from_seed(1_000 + seed);
            let mut bytes = Vec::new();
            ci::encode(
                &Header::new(Address::FunctionBlock, muid, Muid::BROADCAST),
                &Message::Discovery {
                    identity: AgentConfig::default().identity,
                    categories: Categories::NONE,
                    max_sysex: 128,
                    output_path: 0,
                },
                &mut bytes,
            )
            .unwrap();
            synth.receive(&bytes).unwrap();
            let chunk = PropertyChunk {
                request_id: (seed % 128) as u8,
                header: b"{}",
                chunks: 0,
                chunk: 1,
                data: &[1; 100],
            };
            let mut bytes = Vec::new();
            ci::encode(
                &Header::new(Address::FunctionBlock, muid, synth.muid()),
                &Message::Property(PropertyKind::Set, chunk),
                &mut bytes,
            )
            .unwrap();
            synth.receive(&bytes).unwrap();
        }
        assert_eq!(synth.peers().len(), MAX_PEERS);
        assert!(synth.assembler.pending() <= 8);
        assert!(synth.outgoing_len() <= MAX_OUTGOING);
        // Random bytes after a valid header.
        let mut state = 0x9E37_79B9u32;
        for _ in 0..20_000 {
            let mut bytes = vec![0x7E, 0x7F, 0x0D];
            for _ in 0..(state % 40) {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                bytes.push((state & 0x7F) as u8);
            }
            let _ = synth.receive(&bytes);
        }
        assert!(synth.outgoing_len() <= MAX_OUTGOING);
    }
}
