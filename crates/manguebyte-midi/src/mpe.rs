//! MIDI Polyphonic Expression (MPE).
//!
//! MIDI 1.0 bends, presses and brightens a whole channel at once. MPE gets
//! per-note control out of it by giving each sounding note a channel of
//! its own: a *zone* is one master channel, for what applies to every
//! note, and up to fifteen member channels, for one note each. A note is
//! then shaped by the pitch bend, channel pressure and controller 74 of
//! its member channel, on top of the same three on the master channel.
//!
//! - [`Zone`], [`ZoneLayout`] and [`Configuration`]: which channels form
//!   the lower and upper zones, and the configuration message that says
//!   so.
//! - [`ChannelAllocator`]: for sending. Chooses the member channel for
//!   each new note.
//! - [`NoteTracker`]: for receiving. Follows a stream and keeps every
//!   sounding note as a [`Note`], plain data with its own and its zone's
//!   pitch bend, pressure and timbre.
//!
//! Written from "MIDI Polyphonic Expression", version 1.0 (the MIDI
//! Association, RP-053). Section numbers in comments refer to that
//! document.
//!
//! Nothing in this module allocates.
//!
//! ```
//! use manguebyte_midi::mpe::{Configuration, NoteEvent, NoteTracker, ZoneKind};
//! use manguebyte_midi::MidiMessage;
//!
//! let mut tracker = NoteTracker::<16>::new();
//! let mut started = None;
//! let mut feed = |message| {
//!     tracker.feed(message, |event| {
//!         if let NoteEvent::Started(note) = event {
//!             started = Some(note);
//!         }
//!     })
//! };
//!
//! // A lower zone with fifteen member channels.
//! let lower = Configuration { zone: ZoneKind::Lower, members: 15 };
//! for message in &lower.messages() {
//!     feed(*message);
//! }
//! // Bend member channel 2 up by half its range, then play a note on it.
//! feed(MidiMessage::PitchBend { channel: 1, value: 4096 });
//! feed(MidiMessage::NoteOn { channel: 1, note: 60, velocity: 100 });
//!
//! let note = started.unwrap();
//! assert_eq!(note.zone, Some(ZoneKind::Lower));
//! // Half of the default 48 semitones.
//! assert_eq!(note.pitch(), 84.0);
//! ```
//!
//! # Limits
//!
//! The tracker follows the sustain pedal but not the sostenuto pedal. A
//! note-on written as a note-on with velocity zero has release velocity
//! zero; the specification suggests reading it as 64, which cannot be done
//! here because [`MidiMessage`] does not keep the difference. MIDI mode 4
//! (one voice per channel) is not modelled: every channel may hold several
//! notes. The mode messages (controllers 124 to 127) are followed only for
//! the "all notes off" that MIDI 1.0 makes part of each of them; this was
//! written without the MPE specification's own text on mode messages
//! inside a zone at hand. Data increment and decrement (controllers 96 and
//! 97) do not move the pitch bend range.

use crate::rpn::{Change, Parameter, ParameterEvent, RpnDecoder};
use crate::{MessageList, MidiMessage};

/// Pitch bend range of member channels after a zone is configured, in
/// semitones up and down (section 2.4).
pub const MEMBER_PITCH_BEND_RANGE: u8 = 48;

/// Pitch bend range of a master channel after its zone is configured, and
/// of any channel outside a zone, in semitones up and down (section 2.4).
pub const MASTER_PITCH_BEND_RANGE: u8 = 2;

/// The controller that carries the third dimension of per-note control,
/// usually mapped to timbre (section 2.6).
pub const TIMBRE_CONTROLLER: u8 = 74;

const SUSTAIN_PEDAL: u8 = 64;
const ALL_SOUND_OFF: u8 = 120;
const RESET_ALL_CONTROLLERS: u8 = 121;
const ALL_NOTES_OFF: u8 = 123;
/// The first and the last of the four mode messages: omni off, omni on,
/// mono on and poly on. The MIDI 1.0 Detailed Specification gives each of
/// them the effect of "all notes off" as well.
const OMNI_OFF: u8 = 124;
const POLY_ON: u8 = 127;

/// Which of the two zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoneKind {
    /// The zone whose master is channel 1, with members counted upwards
    /// from channel 2.
    Lower,
    /// The zone whose master is channel 16, with members counted downwards
    /// from channel 15.
    Upper,
}

impl ZoneKind {
    /// The zero-based master channel of this zone: 0 or 15.
    pub const fn master_channel(self) -> u8 {
        match self {
            Self::Lower => 0,
            Self::Upper => 15,
        }
    }
}

/// An active zone: a master channel and one to fifteen member channels.
///
/// Channels are zero-based throughout, so the lower zone's master is
/// channel 0 and its first member channel 1.
///
/// ```
/// use manguebyte_midi::mpe::Zone;
///
/// let upper = Zone::upper(7).unwrap();
/// assert_eq!(upper.master_channel(), 15);
/// // Members are handed out from the master inwards.
/// assert_eq!(upper.member_channels().collect::<Vec<_>>(), [14, 13, 12, 11, 10, 9, 8]);
/// assert!(upper.is_member(8) && !upper.is_member(7) && !upper.is_member(15));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Zone {
    kind: ZoneKind,
    members: u8,
}

impl Zone {
    /// A zone with `members` member channels, or `None` unless that is
    /// between 1 and 15.
    pub const fn new(kind: ZoneKind, members: u8) -> Option<Self> {
        if members == 0 || members > 15 {
            None
        } else {
            Some(Self { kind, members })
        }
    }

    /// A lower zone with `members` member channels.
    pub const fn lower(members: u8) -> Option<Self> {
        Self::new(ZoneKind::Lower, members)
    }

    /// An upper zone with `members` member channels.
    pub const fn upper(members: u8) -> Option<Self> {
        Self::new(ZoneKind::Upper, members)
    }

    /// Which zone this is.
    pub const fn kind(&self) -> ZoneKind {
        self.kind
    }

    /// How many member channels the zone has, `1..=15`.
    pub const fn member_count(&self) -> u8 {
        self.members
    }

    /// The master channel: 0 for the lower zone, 15 for the upper.
    pub const fn master_channel(&self) -> u8 {
        self.kind.master_channel()
    }

    /// The `index`-th member channel, counting from the one next to the
    /// master, or `None` past the last.
    pub const fn member(&self, index: u8) -> Option<u8> {
        if index >= self.members {
            return None;
        }
        Some(match self.kind {
            ZoneKind::Lower => 1 + index,
            ZoneKind::Upper => 14 - index,
        })
    }

    /// The member channels, starting next to the master.
    pub fn member_channels(self) -> impl Iterator<Item = u8> {
        (0..self.members).filter_map(move |index| self.member(index))
    }

    /// Whether `channel` is one of the member channels.
    pub const fn is_member(&self, channel: u8) -> bool {
        self.member_index(channel).is_some()
    }

    /// Whether `channel` is the master or a member of this zone.
    pub const fn contains(&self, channel: u8) -> bool {
        channel == self.master_channel() || self.is_member(channel)
    }

    /// The message that configures this zone.
    pub const fn configuration(&self) -> Configuration {
        Configuration {
            zone: self.kind,
            members: self.members,
        }
    }

    /// The position of `channel` among the members, counting from the one
    /// next to the master.
    const fn member_index(&self, channel: u8) -> Option<u8> {
        let index = match self.kind {
            ZoneKind::Lower => channel.wrapping_sub(1),
            ZoneKind::Upper => 14u8.wrapping_sub(channel),
        };
        if index < self.members {
            Some(index)
        } else {
            None
        }
    }
}

/// The MPE configuration message: registered parameter 6 sent on a master
/// channel, with the number of member channels as its coarse data byte
/// (section 2.1.1). Zero members switches the zone off.
///
/// ```
/// use manguebyte_midi::mpe::{Configuration, ZoneKind};
///
/// // Example one of the specification: a lower zone with seven members.
/// let bytes: Vec<u8> = Configuration { zone: ZoneKind::Lower, members: 7 }
///     .messages()
///     .iter()
///     .flat_map(|message| message.to_bytes().as_slice().to_vec())
///     .collect();
/// assert_eq!(bytes, [0xB0, 0x64, 0x06, 0xB0, 0x65, 0x00, 0xB0, 0x06, 0x07]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Configuration {
    /// The zone being configured.
    pub zone: ZoneKind,
    /// How many member channels it gets, `0..=15`. Zero switches it off.
    pub members: u8,
}

impl Configuration {
    /// The three controller messages that make up the configuration
    /// message, in the order the specification writes them: parameter
    /// number low byte, parameter number high byte, data entry. Counts
    /// above 15 are sent as 15.
    pub fn messages(&self) -> MessageList {
        let channel = self.zone.master_channel();
        let cc = |controller, value| MidiMessage::ControlChange {
            channel,
            controller,
            value,
        };
        let parameter = Parameter::MPE_CONFIGURATION;
        MessageList::from_slice(&[
            cc(100, parameter.lsb),
            cc(101, parameter.msb),
            cc(6, self.members.min(15)),
        ])
    }

    /// Recognises a configuration message in what an
    /// [`RpnDecoder`] reports.
    ///
    /// It is one when registered parameter 6 gets its coarse data byte on
    /// channel 0 or 15 and the byte is at most 15. On any other channel
    /// the message is invalid and is ignored (section 2.1.1); the fine
    /// data byte is not used.
    pub fn from_event(event: &ParameterEvent) -> Option<Self> {
        if event.parameter != Parameter::MPE_CONFIGURATION {
            return None;
        }
        let Change::Value { fine: false, .. } = event.change else {
            return None;
        };
        let members = event.change.coarse()?;
        let zone = match event.channel {
            0 => ZoneKind::Lower,
            15 => ZoneKind::Upper,
            _ => return None,
        };
        (members <= 15).then_some(Self { zone, members })
    }
}

/// What a channel is used for under a [`ZoneLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The master channel of a zone.
    Master(ZoneKind),
    /// A member channel of a zone.
    Member(ZoneKind),
    /// Not part of any zone: an ordinary MIDI channel.
    Outside,
}

/// Which channels belong to the lower and upper zones.
///
/// A channel belongs to at most one zone. When a zone is configured to
/// take channels the other zone holds, the newer configuration wins and
/// the other zone shrinks, down to nothing if need be (section 2.1.1).
///
/// ```
/// use manguebyte_midi::mpe::{Configuration, Role, ZoneKind, ZoneLayout};
///
/// let mut layout = ZoneLayout::new();
/// layout.configure(Configuration { zone: ZoneKind::Lower, members: 15 });
/// assert_eq!(layout.role(15), Role::Member(ZoneKind::Lower));
///
/// // An upper zone of four takes channels 11 to 15 back from the lower one.
/// layout.configure(Configuration { zone: ZoneKind::Upper, members: 4 });
/// assert_eq!(layout.lower().unwrap().member_count(), 10);
/// assert_eq!(layout.role(10), Role::Member(ZoneKind::Lower));
/// assert_eq!(layout.role(11), Role::Member(ZoneKind::Upper));
/// assert_eq!(layout.role(15), Role::Master(ZoneKind::Upper));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ZoneLayout {
    /// Member channels of the lower zone, `0..=15`; zero means off.
    lower: u8,
    /// Member channels of the upper zone, `0..=15`; zero means off.
    upper: u8,
}

impl ZoneLayout {
    /// No zones: MPE is off and all sixteen channels are ordinary ones.
    pub const fn new() -> Self {
        Self { lower: 0, upper: 0 }
    }

    /// The lower zone, if it is active.
    pub const fn lower(&self) -> Option<Zone> {
        Zone::lower(self.lower)
    }

    /// The upper zone, if it is active.
    pub const fn upper(&self) -> Option<Zone> {
        Zone::upper(self.upper)
    }

    /// The zone of the given kind, if it is active.
    pub const fn zone(&self, kind: ZoneKind) -> Option<Zone> {
        match kind {
            ZoneKind::Lower => self.lower(),
            ZoneKind::Upper => self.upper(),
        }
    }

    /// Whether at least one zone is active.
    pub const fn is_active(&self) -> bool {
        self.lower > 0 || self.upper > 0
    }

    /// Applies a configuration message. The other zone gives up any
    /// channel the configured zone now takes.
    pub fn configure(&mut self, configuration: Configuration) {
        let members = configuration.members.min(15);
        let (this, other) = match configuration.zone {
            ZoneKind::Lower => (&mut self.lower, &mut self.upper),
            ZoneKind::Upper => (&mut self.upper, &mut self.lower),
        };
        *this = members;
        if members > 0 {
            // The configured zone holds its master and `members` more
            // channels. Of the sixteen, that leaves `15 - members` for the
            // other zone, one of which has to be its master.
            *other = (*other).min(14u8.saturating_sub(members));
        }
    }

    /// What `channel` is used for.
    pub const fn role(&self, channel: u8) -> Role {
        if let Some(zone) = self.lower() {
            if channel == zone.master_channel() {
                return Role::Master(ZoneKind::Lower);
            }
            if zone.is_member(channel) {
                return Role::Member(ZoneKind::Lower);
            }
        }
        if let Some(zone) = self.upper() {
            if channel == zone.master_channel() {
                return Role::Master(ZoneKind::Upper);
            }
            if zone.is_member(channel) {
                return Role::Member(ZoneKind::Upper);
            }
        }
        Role::Outside
    }
}

/// What the allocator remembers about one member channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemberState {
    /// One bit for each key that is down on this channel.
    keys: u128,
    /// The key most recently started here, or `NO_KEY`.
    last_key: u8,
    /// When a note last started here, on the allocator's own clock.
    last_on: u64,
    /// When a note last ended here. Zero if none has.
    last_off: u64,
}

const NO_KEY: u8 = 0xFF;

impl MemberState {
    const NEW: Self = Self {
        keys: 0,
        last_key: NO_KEY,
        last_on: 0,
        last_off: 0,
    };

    fn count(&self) -> u32 {
        self.keys.count_ones()
    }
}

/// Chooses the member channel for each new note of an MPE sender.
///
/// The rules are the ones section 3.2 recommends, in this order:
///
/// 1. A free channel whose last note was this same key is used again,
///    so that a repeated note does not sound on two channels at once
///    while the first is still ringing out.
/// 2. Otherwise the channel with the fewest sounding notes.
/// 3. Among equals, the one whose last note ended longest ago, which
///    leaves the most recent release tails undisturbed.
/// 4. Among equals again, the one nearest the master channel.
///
/// **When the channels run out**, rule 2 makes the new note share a
/// channel with an older one. Both then follow the same pitch bend,
/// pressure and timbre; nothing is refused and nothing is cut off, which
/// is what the specification prefers to limiting polyphony.
/// [`is_shared`](Self::is_shared) tells a sender when that has happened.
///
/// All methods are real-time safe.
///
/// ```
/// use manguebyte_midi::mpe::{ChannelAllocator, Zone};
///
/// let mut allocator = ChannelAllocator::new(Zone::lower(2).unwrap());
/// assert_eq!(allocator.note_on(60), 1);
/// assert_eq!(allocator.note_on(64), 2);
/// // Both member channels are taken: the third note has to share.
/// let shared = allocator.note_on(67);
/// assert!(allocator.is_shared(shared));
/// // Releasing by key finds the channel the note went to.
/// assert_eq!(allocator.release(64), Some(2));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAllocator {
    zone: Zone,
    members: [MemberState; 15],
    clock: u64,
}

impl ChannelAllocator {
    /// An allocator for the member channels of `zone`, all free.
    pub const fn new(zone: Zone) -> Self {
        Self {
            zone,
            members: [MemberState::NEW; 15],
            clock: 0,
        }
    }

    /// The zone whose channels are being handed out.
    pub const fn zone(&self) -> Zone {
        self.zone
    }

    /// Forgets every note, as after "all notes off".
    pub fn reset(&mut self) {
        *self = Self::new(self.zone);
    }

    /// Chooses the channel for a new note on `key` and records the note
    /// as sounding there. Returns the zero-based channel to send it on.
    pub fn note_on(&mut self, key: u8) -> u8 {
        let key = key & 0x7F;
        self.clock += 1;
        let members = &self.members[..usize::from(self.zone.members)];

        let mut best: Option<usize> = None;
        // Rule 1: a free channel that last played this key; the most
        // recently released if there are several.
        for (index, member) in members.iter().enumerate() {
            if member.keys == 0
                && member.last_key == key
                && best.is_none_or(|chosen| member.last_off > members[chosen].last_off)
            {
                best = Some(index);
            }
        }
        if best.is_none() {
            // Rules 2 to 4. Free channels have a count of zero, so they
            // come before any occupied one.
            for (index, member) in members.iter().enumerate() {
                let rank = (member.count(), member.last_off);
                if best
                    .is_none_or(|chosen| rank < (members[chosen].count(), members[chosen].last_off))
                {
                    best = Some(index);
                }
            }
        }
        // A zone always has at least one member, so `best` is set.
        let index = best.unwrap_or(0);

        let member = &mut self.members[index];
        member.keys |= 1 << key;
        member.last_key = key;
        member.last_on = self.clock;
        self.zone
            .member(index as u8)
            .unwrap_or(self.zone.master_channel())
    }

    /// Records that the note on `key` sent on `channel` has ended. Returns
    /// whether such a note was sounding.
    pub fn note_off(&mut self, channel: u8, key: u8) -> bool {
        let Some(index) = self.zone.member_index(channel) else {
            return false;
        };
        let member = &mut self.members[usize::from(index)];
        let bit = 1u128 << (key & 0x7F);
        if member.keys & bit == 0 {
            return false;
        }
        member.keys &= !bit;
        self.clock += 1;
        member.last_off = self.clock;
        true
    }

    /// The channel `key` is sounding on. If it sounds on several, the one
    /// where it started last.
    pub fn channel_of(&self, key: u8) -> Option<u8> {
        let bit = 1u128 << (key & 0x7F);
        (0..self.zone.members)
            .filter(|index| self.members[usize::from(*index)].keys & bit != 0)
            .max_by_key(|index| self.members[usize::from(*index)].last_on)
            .and_then(|index| self.zone.member(index))
    }

    /// Ends the note on `key` and returns the channel its note-off belongs
    /// on, or `None` if the key is not sounding. If the key sounds on
    /// several channels, the most recently started one is ended.
    pub fn release(&mut self, key: u8) -> Option<u8> {
        let channel = self.channel_of(key)?;
        self.note_off(channel, key);
        Some(channel)
    }

    /// How many notes are sounding on `channel`. The same key started
    /// twice on one channel counts once.
    pub fn note_count(&self, channel: u8) -> usize {
        self.zone
            .member_index(channel)
            .map_or(0, |index| self.members[usize::from(index)].count() as usize)
    }

    /// Whether more than one note is sounding on `channel`, so that they
    /// cannot be shaped separately.
    pub fn is_shared(&self, channel: u8) -> bool {
        self.note_count(channel) > 1
    }
}

/// A sounding note with everything that shapes it, as plain data.
///
/// The three per-note dimensions come in pairs: the note's own value, set
/// on its member channel, and its zone's value, set on the master channel.
/// A receiver has to combine each pair. For pitch the combination is a
/// sum and [`pitch`](Self::pitch) does it. For pressure and timbre the
/// specification leaves the choice to the receiver;
/// [`combined_pressure`](Self::combined_pressure) and
/// [`combined_timbre`](Self::combined_timbre) offer one.
///
/// A note on a master channel, or on a channel outside every zone, has no
/// channel of its own. Its own pitch bend is zero and its own timbre is
/// the neutral 64, its own pressure is polyphonic key pressure, and the
/// channel-wide values are found in the `master_` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Note {
    /// A number that is different for every note the tracker has started,
    /// until it wraps after four thousand million notes. It tells apart
    /// two notes on the same key. The count carries on across
    /// [`NoteTracker::reset`] and changes of zones.
    pub id: u32,
    /// The channel the note arrived on, `0..=15`.
    pub channel: u8,
    /// The note number, `0..=127`.
    pub key: u8,
    /// Attack velocity, `1..=127`.
    pub velocity: u8,
    /// Release velocity. Meaningful once `key_down` is false.
    pub release_velocity: u8,
    /// Whether the key is still held. A note whose key was released stays
    /// in the tracker while the sustain pedal holds it; it then no longer
    /// follows its member channel, only the master channel (section
    /// 3.3.3).
    pub key_down: bool,
    /// The zone the note's channel belongs to, or `None` for a channel
    /// outside every zone.
    pub zone: Option<ZoneKind>,
    /// The note's own pitch bend, `-8192..=8191`.
    pub pitch_bend: i16,
    /// The note's own pressure, `0..=127`.
    pub pressure: u8,
    /// The note's own timbre (controller 74), `0..=127`, 64 at rest.
    pub timbre: u8,
    /// What a full pitch bend of the note's own amounts to, in cents.
    /// 4800 unless the sender changed it. Zero for a note that has no
    /// channel of its own.
    pub bend_range_cents: u16,
    /// The pitch bend of the zone, or of the channel when it is outside
    /// every zone, `-8192..=8191`.
    pub master_pitch_bend: i16,
    /// The pressure of the zone or channel, `0..=127`.
    pub master_pressure: u8,
    /// The timbre (controller 74) of the zone or channel, `0..=127`, 64
    /// at rest.
    pub master_timbre: u8,
    /// What a full pitch bend of the zone or channel amounts to, in
    /// cents. 200 unless the sender changed it.
    pub master_bend_range_cents: u16,
}

impl Note {
    /// How far the note is bent, in semitones: its own bend plus its
    /// zone's, each scaled by its range. A pitch bend of `-8192` is the
    /// full range downwards; `8191` is one step short of the full range
    /// upwards.
    pub fn pitch_bend_semitones(&self) -> f64 {
        semitones(self.pitch_bend, self.bend_range_cents)
            + semitones(self.master_pitch_bend, self.master_bend_range_cents)
    }

    /// The pitch that sounds, as a fractional note number: the key plus
    /// [`pitch_bend_semitones`](Self::pitch_bend_semitones). Pass it to
    /// [`note_to_hz`](crate::note_to_hz) for a frequency.
    pub fn pitch(&self) -> f64 {
        f64::from(self.key) + self.pitch_bend_semitones()
    }

    /// The larger of the note's own pressure and its zone's.
    pub fn combined_pressure(&self) -> u8 {
        self.pressure.max(self.master_pressure)
    }

    /// The note's own timbre moved by as much as the zone's timbre is
    /// away from its rest position of 64, kept within `0..=127`.
    pub fn combined_timbre(&self) -> u8 {
        (i16::from(self.timbre) + i16::from(self.master_timbre) - 64).clamp(0, 127) as u8
    }
}

fn semitones(bend: i16, range_cents: u16) -> f64 {
    f64::from(bend) / 8192.0 * f64::from(range_cents) / 100.0
}

/// What [`NoteTracker::feed`] reports.
///
/// More kinds of event may be added, so a `match` on this type needs a
/// wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NoteEvent {
    /// A note began.
    Started(Note),
    /// Something that shapes a sounding note changed: its own or its
    /// zone's pitch bend, pressure, timbre or bend range.
    Changed(Note),
    /// The key was released, by a note-off or by "all notes off", but the
    /// sustain pedal keeps the note sounding. An [`Ended`](Self::Ended)
    /// follows when the pedal lifts.
    Released(Note),
    /// The note is over: its key was released with the pedal up, the
    /// pedal lifted, it was cut off by "all sound off" or by a change of
    /// zones, the same key was struck again, or the tracker ran out of
    /// room.
    Ended(Note),
    /// A configuration message changed the zones.
    LayoutChanged(ZoneLayout),
}

/// What the tracker remembers about one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelState {
    pitch_bend: i16,
    pressure: u8,
    timbre: u8,
    sustain: bool,
    bend_range_cents: u16,
}

impl ChannelState {
    const fn new(role: Role) -> Self {
        Self {
            pitch_bend: 0,
            pressure: 0,
            timbre: 64,
            sustain: false,
            bend_range_cents: default_bend_range(role),
        }
    }

    /// Does what "reset all controllers" asks of a channel, following
    /// recommended practice RP-015: pitch bend, channel pressure and the
    /// pedal go back to rest. Controller 74 is one of the sound
    /// controllers (70 to 79), which RP-015 says the message leaves
    /// alone, and the bend range is a parameter, not a controller.
    fn reset_controllers(&mut self) {
        self.pitch_bend = 0;
        self.pressure = 0;
        self.sustain = false;
    }
}

const fn default_bend_range(role: Role) -> u16 {
    match role {
        Role::Member(_) => MEMBER_PITCH_BEND_RANGE as u16 * 100,
        Role::Master(_) | Role::Outside => MASTER_PITCH_BEND_RANGE as u16 * 100,
    }
}

/// Turns an incoming MIDI stream into the notes that are sounding, each
/// with its own and its zone's pitch bend, pressure and timbre.
///
/// The tracker holds at most `N` notes in a fixed array and never
/// allocates, so [`feed`](Self::feed) can run on the audio thread. It
/// starts with no zones, where every channel behaves as ordinary MIDI, and
/// follows configuration messages in the stream; use
/// [`with_layout`](Self::with_layout) or [`set_layout`](Self::set_layout)
/// for zones agreed some other way.
///
/// What it does with each message, following sections 2 and 3.3:
///
/// - **Note-on** starts a note. On a member channel it takes the pitch
///   bend, pressure and timbre that channel has at that moment, which is
///   why senders set them just before the note. A note-on for a key that
///   is already sounding on the same channel ends the earlier note first.
///   When all `N` places are taken the oldest note is ended to make room.
/// - **Note-off** ends the note, or with the zone's sustain pedal down
///   marks it released and keeps it until the pedal lifts.
/// - **Pitch bend, channel pressure and controller 74** on a member
///   channel shape the notes held on that channel, and stop shaping a note
///   once its key is released. On a master channel they shape every note
///   of the zone, released ones included.
/// - **Polyphonic key pressure** sets a note's own pressure on a master
///   channel or outside zones, and is ignored on member channels, where
///   the specification forbids it.
/// - **Registered parameter 0** sets the pitch bend range: of the master
///   channel alone when sent there, of every member channel of the zone
///   when sent on any one of them. The coarse data byte gives semitones
///   and sets the cents to zero; the fine data byte gives cents, up to
///   99, and when it is sent alone the semitones stay as they are.
/// - **Registered parameter 6** on channel 0 or 15 reconfigures the
///   zones. Notes on channels that enter or leave a zone, or change role,
///   are ended and those channels' controllers reset; the configured
///   zone's bend ranges return to 48 and 2 semitones.
/// - **The sustain pedal** (controller 64) of a master channel holds the
///   released notes of its whole zone.
/// - **All notes off** (controller 123) is a note-off for every key that
///   is down: with the pedal down the notes are released and kept until
///   it lifts, otherwise they end. Omni off, omni on, mono on and poly on
///   (controllers 124 to 127) include it and are treated the same.
/// - **All sound off** (controller 120) ends the notes at once, pedal or
///   not.
/// - **Reset all controllers** (controller 121) puts pitch bend, channel
///   pressure, polyphonic key pressure and the pedal back to rest, which
///   ends the notes the pedal was holding. Following recommended practice
///   RP-015 it leaves controller 74 and the pitch bend range as they are.
///
/// The last four act on the whole zone when they arrive on its master
/// channel and are ignored on member channels.
///
/// A channel outside every zone is tracked as ordinary MIDI: its notes
/// share the channel's pitch bend, pressure and controller 74, reported in
/// the `master_` fields of [`Note`], with a default range of 2 semitones.
///
/// Two trackers are equal when they hold the same notes and would treat
/// every further message alike. Notes that have ended leave no trace.
#[derive(Clone)]
pub struct NoteTracker<const N: usize> {
    layout: ZoneLayout,
    channels: [ChannelState; 16],
    rpn: RpnDecoder,
    /// The sounding notes in the order they started. Entries past `len`
    /// are stale and must not be looked at: equality and `Debug` go
    /// through [`notes`](Self::notes).
    notes: [Note; N],
    len: usize,
    next_id: u32,
}

impl<const N: usize> PartialEq for NoteTracker<N> {
    fn eq(&self, other: &Self) -> bool {
        self.layout == other.layout
            && self.channels == other.channels
            && self.rpn == other.rpn
            && self.notes() == other.notes()
            && self.next_id == other.next_id
    }
}

impl<const N: usize> Eq for NoteTracker<N> {}

impl<const N: usize> core::fmt::Debug for NoteTracker<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NoteTracker")
            .field("layout", &self.layout)
            .field("channels", &self.channels)
            .field("rpn", &self.rpn)
            .field("notes", &self.notes())
            .field("next_id", &self.next_id)
            .finish()
    }
}

/// What unused entries of the note array hold. Never visible from outside.
const NO_NOTE: Note = Note {
    id: 0,
    channel: 0,
    key: 0,
    velocity: 0,
    release_velocity: 0,
    key_down: false,
    zone: None,
    pitch_bend: 0,
    pressure: 0,
    timbre: 64,
    bend_range_cents: 0,
    master_pitch_bend: 0,
    master_pressure: 0,
    master_timbre: 64,
    master_bend_range_cents: 0,
};

impl<const N: usize> NoteTracker<N> {
    /// A tracker with no zones and no notes.
    pub const fn new() -> Self {
        Self {
            layout: ZoneLayout::new(),
            channels: [ChannelState::new(Role::Outside); 16],
            rpn: RpnDecoder::new(),
            notes: [NO_NOTE; N],
            len: 0,
            next_id: 0,
        }
    }

    /// A tracker that starts with the given zones, as if their
    /// configuration messages had just arrived.
    pub fn with_layout(layout: ZoneLayout) -> Self {
        let mut tracker = Self::new();
        tracker.set_layout(layout, |_| {});
        tracker
    }

    /// The zones as they are now.
    pub fn layout(&self) -> ZoneLayout {
        self.layout
    }

    /// The sounding notes, oldest first.
    pub fn notes(&self) -> &[Note] {
        &self.notes[..self.len]
    }

    /// The note sounding on `key` of `channel`, if there is one.
    pub fn note(&self, channel: u8, key: u8) -> Option<&Note> {
        self.notes()
            .iter()
            .find(|note| note.channel == channel && note.key == key)
    }

    /// Forgets every note without reporting it and puts every channel back
    /// to rest. The zones stay as they are, with their default bend
    /// ranges. Note ids carry on from where they were, so that a note
    /// started after the reset is not taken for one from before it.
    pub fn reset(&mut self) {
        let (layout, next_id) = (self.layout, self.next_id);
        *self = Self::new();
        self.next_id = next_id;
        self.set_layout(layout, |_| {});
    }

    /// Replaces the zones, as two configuration messages would. Notes on
    /// channels whose role changes are ended, and every channel's bend
    /// range returns to its default.
    pub fn set_layout(&mut self, layout: ZoneLayout, mut on_event: impl FnMut(NoteEvent)) {
        self.apply_layout(layout, None, &mut on_event);
    }

    /// Takes the next message of the stream and calls `on_event` for each
    /// thing it does to a note. Real-time safe as long as `on_event` is.
    pub fn feed(&mut self, message: MidiMessage, mut on_event: impl FnMut(NoteEvent)) {
        let on_event = &mut on_event;
        if let Some(event) = self.rpn.feed(message) {
            self.parameter(&event, on_event);
        }
        match message {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                if velocity & 0x7F == 0 {
                    self.note_off(channel & 0x0F, note & 0x7F, 0, on_event);
                } else {
                    self.note_on(channel & 0x0F, note & 0x7F, velocity & 0x7F, on_event);
                }
            }
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => self.note_off(channel & 0x0F, note & 0x7F, velocity & 0x7F, on_event),
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => self.poly_pressure(channel & 0x0F, note & 0x7F, pressure & 0x7F, on_event),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => self.control_change(channel & 0x0F, controller, value & 0x7F, on_event),
            MidiMessage::ChannelPressure { channel, pressure } => {
                self.channels[usize::from(channel & 0x0F)].pressure = pressure & 0x7F;
            }
            MidiMessage::PitchBend { channel, value } => {
                self.channels[usize::from(channel & 0x0F)].pitch_bend = value.clamp(-8192, 8191);
            }
            MidiMessage::ProgramChange { .. } => {}
        }
        // Whatever changed on a channel reaches its notes here.
        self.refresh(on_event);
    }

    fn parameter(&mut self, event: &ParameterEvent, on_event: &mut impl FnMut(NoteEvent)) {
        if let Some(configuration) = Configuration::from_event(event) {
            let mut layout = self.layout;
            layout.configure(configuration);
            self.apply_layout(layout, Some(configuration.zone), on_event);
            return;
        }
        if event.parameter != Parameter::PITCH_BEND_SENSITIVITY {
            return;
        }
        let cents = match event.change {
            // Semitones in the coarse byte, cents in the fine one.
            Change::Value { value, .. } => (value >> 7) * 100 + (value & 0x7F).min(99),
            // The cents alone: the semitones stay as they are. Member
            // channels of a zone all have the same range, so the channel
            // the message came on is the one to ask.
            Change::FineOnly(fine) => {
                let whole = self.channels[usize::from(event.channel)].bend_range_cents / 100;
                whole * 100 + u16::from(fine).min(99)
            }
            Change::Increment(_) | Change::Decrement(_) => return,
        };
        match self.layout.role(event.channel) {
            // Member channels of a zone may not differ: the last range
            // sent to any of them applies to all (section 2.4).
            Role::Member(kind) => {
                for channel in 0..16u8 {
                    if self.layout.role(channel) == Role::Member(kind) {
                        self.channels[usize::from(channel)].bend_range_cents = cents;
                    }
                }
            }
            Role::Master(_) | Role::Outside => {
                self.channels[usize::from(event.channel)].bend_range_cents = cents;
            }
        }
    }

    /// Moves to a new layout. `configured` is the zone a configuration
    /// message named, or `None` to treat both zones as just configured.
    fn apply_layout(
        &mut self,
        layout: ZoneLayout,
        configured: Option<ZoneKind>,
        on_event: &mut impl FnMut(NoteEvent),
    ) {
        let old = self.layout;
        for channel in 0..16u8 {
            let role = layout.role(channel);
            if old.role(channel) != role {
                // Section 2.1.4: stop the notes and reset the controls of
                // every channel that enters or leaves a zone.
                self.end_notes(|note| note.channel == channel, on_event);
                self.channels[usize::from(channel)] = ChannelState::new(role);
            } else {
                let in_configured = match (role, configured) {
                    (_, None) => true,
                    (Role::Master(kind) | Role::Member(kind), Some(zone)) => kind == zone,
                    (Role::Outside, Some(_)) => false,
                };
                if in_configured {
                    // Section 2.4: a configuration message sets the bend
                    // ranges of its zone back to their defaults.
                    self.channels[usize::from(channel)].bend_range_cents = default_bend_range(role);
                }
            }
        }
        self.layout = layout;
        if old != layout {
            on_event(NoteEvent::LayoutChanged(layout));
        }
        self.refresh(on_event);
    }

    fn note_on(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
        on_event: &mut impl FnMut(NoteEvent),
    ) {
        if N == 0 {
            return;
        }
        // The same key again on the same channel replaces the note.
        self.end_notes(|note| note.channel == channel && note.key == key, on_event);
        if self.len == N {
            if let Some(&oldest) = self.notes.first() {
                self.notes.copy_within(1..self.len, 0);
                self.len -= 1;
                on_event(NoteEvent::Ended(Note {
                    key_down: false,
                    ..oldest
                }));
            }
        }
        let note = self.resolve(Note {
            id: self.next_id,
            channel,
            key,
            velocity,
            key_down: true,
            ..NO_NOTE
        });
        self.next_id = self.next_id.wrapping_add(1);
        if let Some(slot) = self.notes.get_mut(self.len) {
            *slot = note;
            self.len += 1;
            on_event(NoteEvent::Started(note));
        }
    }

    fn note_off(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
        on_event: &mut impl FnMut(NoteEvent),
    ) {
        let Some(index) = self
            .notes()
            .iter()
            .position(|note| note.channel == channel && note.key == key && note.key_down)
        else {
            return;
        };
        let note = &mut self.notes[index];
        note.key_down = false;
        note.release_velocity = velocity;
        let note = *note;
        if self.channels[usize::from(self.pedal_channel(channel))].sustain {
            on_event(NoteEvent::Released(note));
        } else {
            self.notes.copy_within(index + 1..self.len, index);
            self.len -= 1;
            on_event(NoteEvent::Ended(note));
        }
    }

    fn poly_pressure(
        &mut self,
        channel: u8,
        key: u8,
        pressure: u8,
        on_event: &mut impl FnMut(NoteEvent),
    ) {
        // Not allowed on member channels (section 2.5).
        if matches!(self.layout.role(channel), Role::Member(_)) {
            return;
        }
        let len = self.len;
        if let Some(note) = self.notes[..len]
            .iter_mut()
            .find(|note| note.channel == channel && note.key == key && note.key_down)
        {
            if note.pressure != pressure {
                note.pressure = pressure;
                on_event(NoteEvent::Changed(*note));
            }
        }
    }

    fn control_change(
        &mut self,
        channel: u8,
        controller: u8,
        value: u8,
        on_event: &mut impl FnMut(NoteEvent),
    ) {
        if controller == TIMBRE_CONTROLLER {
            self.channels[usize::from(channel)].timbre = value;
            return;
        }
        // The rest are zone messages: they act from a master channel, or
        // on an ordinary channel, and are ignored on member channels
        // (section 2.3.1).
        let role = self.layout.role(channel);
        if matches!(role, Role::Member(_)) {
            return;
        }
        let layout = self.layout;
        let in_scope = move |other: u8| match role {
            Role::Master(kind) => layout.zone(kind).is_some_and(|zone| zone.contains(other)),
            _ => other == channel,
        };
        match controller {
            SUSTAIN_PEDAL => {
                let down = value >= 64;
                let state = &mut self.channels[usize::from(channel)];
                let lifted = state.sustain && !down;
                state.sustain = down;
                if lifted {
                    self.end_notes(|note| !note.key_down && in_scope(note.channel), on_event);
                }
            }
            ALL_SOUND_OFF => {
                self.end_notes(|note| in_scope(note.channel), on_event);
            }
            // A note-off for every key that is down. `channel` is a master
            // channel or an ordinary one here, so its pedal is the one
            // that holds the notes in scope.
            ALL_NOTES_OFF | OMNI_OFF..=POLY_ON if self.channels[usize::from(channel)].sustain => {
                for index in 0..self.len {
                    let note = &mut self.notes[index];
                    if note.key_down && in_scope(note.channel) {
                        note.key_down = false;
                        on_event(NoteEvent::Released(*note));
                    }
                }
            }
            ALL_NOTES_OFF | OMNI_OFF..=POLY_ON => {
                self.end_notes(|note| in_scope(note.channel), on_event);
            }
            RESET_ALL_CONTROLLERS => {
                for other in (0..16u8).filter(|other| in_scope(*other)) {
                    self.channels[usize::from(other)].reset_controllers();
                }
                // The pedal is one of the controllers that were reset.
                self.end_notes(|note| !note.key_down && in_scope(note.channel), on_event);
                for index in 0..self.len {
                    let note = self.notes[index];
                    // Polyphonic pressure lives in the note, not in a
                    // channel, so it is put back to rest here. Notes on
                    // member channels have none and follow their channel
                    // in `refresh`.
                    let own_pressure = !matches!(layout.role(note.channel), Role::Member(_));
                    if own_pressure && in_scope(note.channel) && note.key_down && note.pressure != 0
                    {
                        let rested = self.resolve(Note {
                            pressure: 0,
                            ..note
                        });
                        self.notes[index] = rested;
                        on_event(NoteEvent::Changed(rested));
                    }
                }
            }
            _ => {}
        }
    }

    /// The channel whose sustain pedal holds notes of `channel`: the
    /// zone's master, or the channel itself outside zones.
    fn pedal_channel(&self, channel: u8) -> u8 {
        match self.layout.role(channel) {
            Role::Master(kind) | Role::Member(kind) => kind.master_channel(),
            Role::Outside => channel,
        }
    }

    /// Ends every note `ends` picks, keeping the others in order.
    fn end_notes(&mut self, ends: impl Fn(&Note) -> bool, on_event: &mut impl FnMut(NoteEvent)) {
        let mut kept = 0;
        for index in 0..self.len {
            let mut note = self.notes[index];
            if ends(&note) {
                note.key_down = false;
                on_event(NoteEvent::Ended(note));
            } else {
                self.notes[kept] = note;
                kept += 1;
            }
        }
        self.len = kept;
    }

    /// Brings every note up to date with its channels and reports the
    /// ones that changed.
    fn refresh(&mut self, on_event: &mut impl FnMut(NoteEvent)) {
        for index in 0..self.len {
            let resolved = self.resolve(self.notes[index]);
            if resolved != self.notes[index] {
                self.notes[index] = resolved;
                on_event(NoteEvent::Changed(resolved));
            }
        }
    }

    /// The note with the fields that come from channels filled in.
    fn resolve(&self, mut note: Note) -> Note {
        let own = self.channels[usize::from(note.channel)];
        let master = match self.layout.role(note.channel) {
            Role::Member(kind) => {
                note.zone = Some(kind);
                // A released note keeps the values it had (section 3.3.3).
                if note.key_down {
                    note.pitch_bend = own.pitch_bend;
                    note.pressure = own.pressure;
                    note.timbre = own.timbre;
                }
                note.bend_range_cents = own.bend_range_cents;
                self.channels[usize::from(kind.master_channel())]
            }
            role => {
                note.zone = match role {
                    Role::Master(kind) => Some(kind),
                    _ => None,
                };
                // No channel of its own: only polyphonic pressure, which
                // is already in the note, is per note.
                note.pitch_bend = 0;
                note.timbre = 64;
                note.bend_range_cents = 0;
                own
            }
        };
        note.master_pitch_bend = master.pitch_bend;
        note.master_pressure = master.pressure;
        note.master_timbre = master.timbre;
        note.master_bend_range_cents = master.bend_range_cents;
        note
    }
}

impl<const N: usize> Default for NoteTracker<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(channel: u8, controller: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        }
    }

    fn on(channel: u8, note: u8, velocity: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        }
    }

    fn off(channel: u8, note: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            channel,
            note,
            velocity: 40,
        }
    }

    fn bend(channel: u8, value: i16) -> MidiMessage {
        MidiMessage::PitchBend { channel, value }
    }

    fn pressure(channel: u8, pressure: u8) -> MidiMessage {
        MidiMessage::ChannelPressure { channel, pressure }
    }

    fn configure(zone: ZoneKind, members: u8) -> Vec<MidiMessage> {
        Configuration { zone, members }
            .messages()
            .as_slice()
            .to_vec()
    }

    /// Feeds messages and returns the events, written as short strings so
    /// that a scenario reads top to bottom: kind, channel, key.
    fn run<const N: usize>(tracker: &mut NoteTracker<N>, messages: &[MidiMessage]) -> Vec<String> {
        let mut log = Vec::new();
        for message in messages {
            tracker.feed(*message, |event| {
                log.push(match event {
                    NoteEvent::Started(note) => format!("start {}:{}", note.channel, note.key),
                    NoteEvent::Changed(note) => format!("change {}:{}", note.channel, note.key),
                    NoteEvent::Released(note) => format!("release {}:{}", note.channel, note.key),
                    NoteEvent::Ended(note) => format!("end {}:{}", note.channel, note.key),
                    NoteEvent::LayoutChanged(_) => "layout".to_string(),
                });
            });
        }
        log
    }

    fn lower_zone_tracker() -> NoteTracker<16> {
        let mut layout = ZoneLayout::new();
        layout.configure(Configuration {
            zone: ZoneKind::Lower,
            members: 15,
        });
        NoteTracker::with_layout(layout)
    }

    #[test]
    fn zones_follow_the_examples_of_the_specification() {
        // Example one (section 2.1.2.1): lower zone with seven members on
        // channels 2 to 8, upper zone off.
        let mut layout = ZoneLayout::new();
        layout.configure(Configuration {
            zone: ZoneKind::Lower,
            members: 7,
        });
        layout.configure(Configuration {
            zone: ZoneKind::Upper,
            members: 0,
        });
        let lower = layout.lower().unwrap();
        assert_eq!(lower.master_channel(), 0);
        assert_eq!(
            lower.member_channels().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(layout.upper(), None);
        assert!(layout.is_active());
        // Channels 9 to 16 stay available for conventional use.
        for channel in 8..16 {
            assert_eq!(layout.role(channel), Role::Outside);
        }

        // Example two: lower zone on 2 to 8 and upper zone on 9 to 15.
        layout.configure(Configuration {
            zone: ZoneKind::Upper,
            members: 7,
        });
        let roles: Vec<Role> = (0..16).map(|channel| layout.role(channel)).collect();
        let mut expected = vec![Role::Master(ZoneKind::Lower)];
        expected.extend([Role::Member(ZoneKind::Lower); 7]);
        expected.extend([Role::Member(ZoneKind::Upper); 7]);
        expected.push(Role::Master(ZoneKind::Upper));
        assert_eq!(roles, expected);

        // Example three: an upper zone with fifteen members uses channels
        // 1 to 15, taking the lower zone's master as a member.
        let mut layout = ZoneLayout::new();
        layout.configure(Configuration {
            zone: ZoneKind::Upper,
            members: 15,
        });
        assert_eq!(layout.lower(), None);
        let upper = layout.upper().unwrap();
        assert_eq!(upper.member(0), Some(14));
        assert_eq!(upper.member(14), Some(0));
        assert_eq!(upper.member(15), None);
        assert_eq!(layout.role(0), Role::Member(ZoneKind::Upper));
        assert_eq!(layout.role(15), Role::Master(ZoneKind::Upper));

        // Both off: MPE is off.
        layout.configure(Configuration {
            zone: ZoneKind::Upper,
            members: 0,
        });
        assert!(!layout.is_active());
        assert_eq!(layout, ZoneLayout::default());
    }

    #[test]
    fn the_newer_zone_takes_channels_from_the_older_one() {
        // Every pair of sizes, in both orders: no channel is in two zones,
        // the configured zone gets what it asked for and the other keeps
        // what is left.
        for first in 0..=15u8 {
            for second in 0..=15u8 {
                for (older, newer) in [
                    (ZoneKind::Lower, ZoneKind::Upper),
                    (ZoneKind::Upper, ZoneKind::Lower),
                ] {
                    let mut layout = ZoneLayout::new();
                    layout.configure(Configuration {
                        zone: older,
                        members: first,
                    });
                    layout.configure(Configuration {
                        zone: newer,
                        members: second,
                    });
                    let count = |kind| layout.zone(kind).map_or(0, |zone| zone.member_count());
                    assert_eq!(count(newer), second);
                    let left = if second == 0 {
                        first
                    } else {
                        first.min(14u8.saturating_sub(second))
                    };
                    assert_eq!(count(older), left, "{first} then {second}");
                    // Each channel has exactly one role, and the zones
                    // agree with it.
                    for channel in 0..16u8 {
                        let in_lower = layout.lower().is_some_and(|z| z.contains(channel));
                        let in_upper = layout.upper().is_some_and(|z| z.contains(channel));
                        assert!(!(in_lower && in_upper));
                        match layout.role(channel) {
                            Role::Outside => assert!(!in_lower && !in_upper),
                            Role::Master(ZoneKind::Lower) | Role::Member(ZoneKind::Lower) => {
                                assert!(in_lower)
                            }
                            Role::Master(ZoneKind::Upper) | Role::Member(ZoneKind::Upper) => {
                                assert!(in_upper)
                            }
                        }
                    }
                }
            }
        }
        // Counts above fifteen are treated as fifteen.
        let mut layout = ZoneLayout::new();
        layout.configure(Configuration {
            zone: ZoneKind::Lower,
            members: 200,
        });
        assert_eq!(layout.lower().unwrap().member_count(), 15);
        assert_eq!(Zone::lower(0), None);
        assert_eq!(Zone::upper(16), None);
        assert_eq!(
            Zone::lower(3).unwrap().configuration(),
            Configuration {
                zone: ZoneKind::Lower,
                members: 3
            }
        );
        assert_eq!(Zone::upper(3).unwrap().kind(), ZoneKind::Upper);
    }

    #[test]
    fn configuration_messages_match_the_bytes_of_the_specification() {
        let bytes = |configuration: Configuration| -> Vec<u8> {
            configuration
                .messages()
                .iter()
                .flat_map(|message| message.to_bytes().as_slice().to_vec())
                .collect()
        };
        // Section 2.1.2, without the optional "reset all controllers" the
        // examples begin with.
        assert_eq!(
            bytes(Configuration {
                zone: ZoneKind::Lower,
                members: 7
            }),
            [0xB0, 0x64, 0x06, 0xB0, 0x65, 0x00, 0xB0, 0x06, 0x07]
        );
        assert_eq!(
            bytes(Configuration {
                zone: ZoneKind::Upper,
                members: 0
            }),
            [0xBF, 0x64, 0x06, 0xBF, 0x65, 0x00, 0xBF, 0x06, 0x00]
        );
        assert_eq!(
            bytes(Configuration {
                zone: ZoneKind::Upper,
                members: 15
            }),
            [0xBF, 0x64, 0x06, 0xBF, 0x65, 0x00, 0xBF, 0x06, 0x0F]
        );

        // Read back through the parameter decoder, for every size.
        for zone in [ZoneKind::Lower, ZoneKind::Upper] {
            for members in 0..=15 {
                let configuration = Configuration { zone, members };
                let mut decoder = RpnDecoder::new();
                let parsed: Vec<Configuration> = configuration
                    .messages()
                    .iter()
                    .filter_map(|message| decoder.feed(*message))
                    .filter_map(|event| Configuration::from_event(&event))
                    .collect();
                assert_eq!(parsed, [configuration]);
            }
        }
    }

    #[test]
    fn configuration_messages_elsewhere_or_malformed_are_ignored() {
        let parse = |channel: u8, parameter: u8, controller: u8, value: u8| {
            let mut decoder = RpnDecoder::new();
            [
                cc(channel, 101, 0),
                cc(channel, 100, parameter),
                cc(channel, controller, value),
            ]
            .into_iter()
            .filter_map(|message| decoder.feed(message))
            .find_map(|event| Configuration::from_event(&event))
        };
        assert!(parse(0, 6, 6, 4).is_some());
        assert!(parse(15, 6, 6, 4).is_some());
        // Any channel but 1 and 16 is invalid (section 2.1.1).
        for channel in 1..15 {
            assert_eq!(parse(channel, 6, 6, 4), None);
        }
        // Another parameter, the fine data byte, and a count too large.
        assert_eq!(parse(0, 0, 6, 4), None);
        assert_eq!(parse(0, 6, 38, 4), None);
        assert_eq!(parse(0, 6, 6, 16), None);
        assert_eq!(parse(0, 6, 96, 0), None);
    }

    #[test]
    fn the_note_on_setup_example_of_the_specification() {
        // Section 3.3.1, on channel 3: pitch bend E2 2B 41, controller 74
        // at 64, pressure zero, then middle C with velocity 56. The note
        // must start with those values.
        let mut tracker = lower_zone_tracker();
        let mut started = Vec::new();
        let stream: [&[u8]; 4] = [
            &[0xE2, 0x2B, 0x41],
            &[0xB2, 0x4A, 0x40],
            &[0xD2, 0x00],
            &[0x92, 0x3C, 0x38],
        ];
        for bytes in stream {
            let message = MidiMessage::from_bytes(bytes).unwrap();
            tracker.feed(message, |event| started.push(event));
        }
        let [NoteEvent::Started(note)] = started[..] else {
            panic!("expected one note to start, got {started:?}");
        };
        assert_eq!((note.channel, note.key, note.velocity), (2, 60, 56));
        assert_eq!(note.zone, Some(ZoneKind::Lower));
        assert!(note.key_down);
        assert_eq!((note.pitch_bend, note.pressure, note.timbre), (171, 0, 64));
        assert_eq!(note.bend_range_cents, 4800);
        assert_eq!(note.master_bend_range_cents, 200);
        // 171 steps of 8192 over a range of 48 semitones. (The
        // specification calls this bend a quarter tone; the bytes it gives
        // amount to one semitone.)
        let expected = 171.0 / 8192.0 * 48.0;
        assert!((note.pitch_bend_semitones() - expected).abs() < 1e-12);
        assert!((note.pitch() - 61.001_953_125).abs() < 1e-9);
        assert_eq!(tracker.notes(), [note]);
        assert_eq!(tracker.note(2, 60), Some(&note));
        assert_eq!(tracker.note(3, 60), None);
    }

    #[test]
    fn each_note_is_shaped_by_its_own_channel_and_by_the_master() {
        let mut tracker = lower_zone_tracker();
        assert_eq!(
            run(&mut tracker, &[on(1, 60, 100), on(2, 64, 100)]),
            ["start 1:60", "start 2:64"]
        );
        // Per-note messages reach one note each.
        assert_eq!(
            run(
                &mut tracker,
                &[bend(1, 8191), pressure(2, 90), cc(2, 74, 100), cc(1, 7, 3)]
            ),
            ["change 1:60", "change 2:64", "change 2:64"]
        );
        let first = *tracker.note(1, 60).unwrap();
        let second = *tracker.note(2, 64).unwrap();
        assert_eq!(
            (first.pitch_bend, first.pressure, first.timbre),
            (8191, 0, 64)
        );
        assert_eq!(
            (second.pitch_bend, second.pressure, second.timbre),
            (0, 90, 100)
        );
        // Almost the full 48 semitones up.
        assert!((first.pitch() - (60.0 + 48.0 * 8191.0 / 8192.0)).abs() < 1e-12);
        assert_eq!(second.pitch(), 64.0);

        // Master messages reach every note of the zone.
        assert_eq!(
            run(&mut tracker, &[bend(0, -8192)]),
            ["change 1:60", "change 2:64"]
        );
        assert_eq!(
            run(&mut tracker, &[pressure(0, 20), cc(0, 74, 74)]),
            ["change 1:60", "change 2:64", "change 1:60", "change 2:64"]
        );
        let first = *tracker.note(1, 60).unwrap();
        let second = *tracker.note(2, 64).unwrap();
        // Master bend is the full default range of two semitones down.
        assert_eq!(second.pitch(), 62.0);
        assert!((first.pitch() - (58.0 + 48.0 * 8191.0 / 8192.0)).abs() < 1e-12);
        assert_eq!((first.master_pressure, first.master_timbre), (20, 74));
        // The offered combinations: the larger pressure, and timbre moved
        // by the master's distance from 64.
        assert_eq!(first.combined_pressure(), 20);
        assert_eq!(second.combined_pressure(), 90);
        assert_eq!(first.combined_timbre(), 74);
        assert_eq!(second.combined_timbre(), 110);
        assert_eq!(
            Note {
                timbre: 127,
                master_timbre: 127,
                ..first
            }
            .combined_timbre(),
            127
        );
        assert_eq!(
            Note {
                timbre: 0,
                master_timbre: 0,
                ..first
            }
            .combined_timbre(),
            0
        );
        // Repeating a value changes nothing and reports nothing.
        assert_eq!(
            run(&mut tracker, &[bend(0, -8192), pressure(2, 90)]),
            [""; 0]
        );
    }

    #[test]
    fn a_channel_remembers_its_values_for_the_next_note() {
        // Section 3.3: values must be tracked on member channels even
        // while no note is playing, to give a new note its initial state.
        let mut tracker = lower_zone_tracker();
        run(
            &mut tracker,
            &[bend(5, -100), pressure(5, 11), cc(5, 74, 12), bend(0, 50)],
        );
        assert_eq!(run(&mut tracker, &[on(5, 70, 1)]), ["start 5:70"]);
        let note = tracker.notes()[0];
        assert_eq!(
            (note.pitch_bend, note.pressure, note.timbre),
            (-100, 11, 12)
        );
        assert_eq!(note.master_pitch_bend, 50);
    }

    #[test]
    fn a_released_note_stops_following_its_channel_but_not_the_master() {
        let mut tracker = lower_zone_tracker();
        // Sustain pedal down on the master channel, then a note that is
        // bent, released and left ringing.
        assert_eq!(
            run(
                &mut tracker,
                &[cc(0, 64, 127), on(3, 60, 100), bend(3, 1000), off(3, 60)]
            ),
            ["start 3:60", "change 3:60", "release 3:60"]
        );
        let held = tracker.notes()[0];
        assert!(!held.key_down);
        assert_eq!(held.release_velocity, 40);
        // Section 3.3.3: the channel's messages no longer reach it, so the
        // channel can be reused at once...
        assert_eq!(
            run(&mut tracker, &[bend(3, -2000), pressure(3, 99)]),
            [""; 0]
        );
        assert_eq!(tracker.notes()[0].pitch_bend, 1000);
        assert_eq!(run(&mut tracker, &[on(3, 72, 100)]), ["start 3:72"]);
        assert_eq!(tracker.note(3, 72).unwrap().pitch_bend, -2000);
        // ...while master bend still applies to it.
        assert_eq!(
            run(&mut tracker, &[bend(0, 4096)]),
            ["change 3:60", "change 3:72"]
        );
        assert_eq!(tracker.note(3, 60).unwrap().master_pitch_bend, 4096);
        // Lifting the pedal ends what it was holding, and only that.
        assert_eq!(run(&mut tracker, &[cc(0, 64, 0)]), ["end 3:60"]);
        assert_eq!(tracker.notes().len(), 1);
        assert_eq!(run(&mut tracker, &[off(3, 72)]), ["end 3:72"]);
        assert!(tracker.notes().is_empty());
        // A note-off with nothing to end is ignored.
        assert_eq!(run(&mut tracker, &[off(3, 72), off(9, 1)]), [""; 0]);
    }

    #[test]
    fn zone_messages_are_ignored_on_member_channels() {
        let mut tracker = lower_zone_tracker();
        run(
            &mut tracker,
            &[on(1, 60, 100), on(2, 62, 100), on(0, 30, 100)],
        );
        // Section 2.3.1: pedal, all notes off, all sound off and reset all
        // controllers mean nothing on a member channel.
        assert_eq!(
            run(
                &mut tracker,
                &[
                    cc(1, 64, 127),
                    off(1, 60),
                    cc(2, 123, 0),
                    cc(2, 120, 0),
                    cc(2, 121, 0)
                ]
            ),
            ["end 1:60"]
        );
        // Polyphonic key pressure is not allowed there either
        // (section 2.5), but works for a note on the master channel.
        assert_eq!(
            run(
                &mut tracker,
                &[
                    MidiMessage::PolyPressure {
                        channel: 2,
                        note: 62,
                        pressure: 50
                    },
                    MidiMessage::PolyPressure {
                        channel: 0,
                        note: 30,
                        pressure: 51
                    },
                    MidiMessage::PolyPressure {
                        channel: 0,
                        note: 31,
                        pressure: 52
                    },
                ]
            ),
            ["change 0:30"]
        );
        let on_master = *tracker.note(0, 30).unwrap();
        assert_eq!(on_master.zone, Some(ZoneKind::Lower));
        assert_eq!(on_master.pressure, 51);
        // A note on the master channel has no bend of its own.
        assert_eq!((on_master.pitch_bend, on_master.bend_range_cents), (0, 0));
        // From the master channel, all notes off ends the whole zone.
        assert_eq!(
            run(&mut tracker, &[cc(0, 123, 0)]),
            ["end 2:62", "end 0:30"]
        );
    }

    #[test]
    fn reset_all_controllers_puts_a_zone_back_to_rest() {
        let mut tracker = lower_zone_tracker();
        run(
            &mut tracker,
            &[
                cc(0, 64, 127),
                on(1, 60, 100),
                on(2, 62, 100),
                off(2, 62),
                bend(1, 500),
                pressure(1, 60),
                cc(1, 74, 10),
                bend(0, -500),
                // Change the member range to 24 semitones.
                cc(1, 101, 0),
                cc(1, 100, 0),
                cc(1, 6, 24),
            ],
        );
        run(&mut tracker, &[cc(0, 74, 20), pressure(0, 30)]);
        assert_eq!(
            run(&mut tracker, &[cc(0, 121, 0)]),
            // The pedal is reset, so the note it held ends; the other goes
            // back to rest.
            ["end 2:62", "change 1:60"]
        );
        let note = tracker.notes()[0];
        assert_eq!((note.pitch_bend, note.pressure), (0, 0));
        assert_eq!((note.master_pitch_bend, note.master_pressure), (0, 0));
        // RP-015 lists the sound controllers, 70 to 79, among what the
        // message does not reset: controller 74 stays where it was, on
        // the member channel and on the master.
        assert_eq!((note.timbre, note.master_timbre), (10, 20));
        // The bend range is a parameter, not a controller: it stays.
        assert_eq!(note.bend_range_cents, 2400);
        // The same on an ordinary channel, where polyphonic key pressure
        // is the note's own and goes back to rest too.
        let mut tracker = NoteTracker::<4>::new();
        run(
            &mut tracker,
            &[
                on(9, 60, 100),
                cc(9, 74, 33),
                bend(9, 77),
                MidiMessage::PolyPressure {
                    channel: 9,
                    note: 60,
                    pressure: 44,
                },
                cc(9, 121, 0),
            ],
        );
        let note = tracker.notes()[0];
        assert_eq!((note.pressure, note.master_pitch_bend), (0, 0));
        assert_eq!(note.master_timbre, 33);
    }

    #[test]
    fn all_notes_off_releases_and_the_pedal_decides_what_ends() {
        // An ordinary channel. All notes off is a note-off for every key
        // that is down, so with the pedal down the notes ring on.
        for controller in [123u8, 124, 125, 126, 127] {
            let mut tracker = NoteTracker::<8>::new();
            run(
                &mut tracker,
                &[on(3, 60, 100), on(3, 64, 100), on(4, 67, 100)],
            );
            assert_eq!(
                run(&mut tracker, &[cc(3, controller, 0)]),
                ["end 3:60", "end 3:64"],
                "controller {controller}"
            );
            assert_eq!(tracker.notes().len(), 1);

            run(
                &mut tracker,
                &[cc(3, 64, 127), on(3, 60, 100), on(3, 64, 100), off(3, 64)],
            );
            // Key 64 was released already; only key 60 is still down.
            assert_eq!(
                run(&mut tracker, &[cc(3, controller, 0)]),
                ["release 3:60"],
                "controller {controller}"
            );
            assert!(
                tracker
                    .notes()
                    .iter()
                    .all(|note| !note.key_down || note.channel == 4)
            );
            assert_eq!(tracker.notes().len(), 3);
            // A second one finds no key down.
            assert_eq!(run(&mut tracker, &[cc(3, controller, 0)]), [""; 0]);
            assert_eq!(
                run(&mut tracker, &[cc(3, 64, 0)]),
                ["end 3:60", "end 3:64"],
                "controller {controller}"
            );
        }

        // A zone, from its master channel, under the master's pedal. The
        // released notes keep the values they had and still follow the
        // master.
        for controller in [123u8, 124, 125, 126, 127] {
            let mut tracker = lower_zone_tracker();
            run(
                &mut tracker,
                &[
                    cc(0, 64, 127),
                    on(1, 60, 100),
                    on(2, 62, 100),
                    on(0, 30, 100),
                    bend(1, 300),
                ],
            );
            assert_eq!(
                run(&mut tracker, &[cc(0, controller, 0)]),
                ["release 1:60", "release 2:62", "release 0:30"],
                "controller {controller}"
            );
            assert_eq!(run(&mut tracker, &[bend(1, -300)]), [""; 0]);
            assert_eq!(tracker.note(1, 60).unwrap().pitch_bend, 300);
            assert_eq!(run(&mut tracker, &[bend(0, 5)]).len(), 3);
            // On a member channel the message means nothing.
            run(&mut tracker, &[on(5, 70, 100)]);
            assert_eq!(run(&mut tracker, &[cc(5, controller, 0)]), [""; 0]);
            // All sound off does not wait for the pedal.
            assert_eq!(
                run(&mut tracker, &[cc(0, 120, 0)]),
                ["end 1:60", "end 2:62", "end 0:30", "end 5:70"]
            );
        }
    }

    #[test]
    fn the_cents_of_the_bend_range_can_be_set_alone() {
        let mut tracker = lower_zone_tracker();
        run(&mut tracker, &[on(1, 60, 100), on(0, 30, 100)]);
        let ranges = |tracker: &NoteTracker<16>| {
            let note = tracker.notes()[0];
            (note.bend_range_cents, note.master_bend_range_cents)
        };
        // 24 semitones and no cents on the member channels.
        run(
            &mut tracker,
            &[cc(1, 101, 0), cc(1, 100, 0), cc(1, 6, 24), cc(1, 38, 0)],
        );
        assert_eq!(ranges(&tracker), (2400, 200));
        // MIDI 1.0: "a subsequent fine adjustment only requires the
        // sending of the LSB". With the parameter named again first...
        run(&mut tracker, &[cc(1, 101, 0), cc(1, 100, 0), cc(1, 38, 50)]);
        assert_eq!(ranges(&tracker), (2450, 200));
        // ...after the null parameter, which makes the decoder forget the
        // coarse byte: the tracker still has the semitones...
        run(
            &mut tracker,
            &[
                cc(1, 101, 127),
                cc(1, 100, 127),
                cc(1, 101, 0),
                cc(1, 100, 0),
                cc(1, 38, 25),
            ],
        );
        assert_eq!(ranges(&tracker), (2425, 200));
        // ...and on another member channel, which never sent a coarse
        // byte but shares the range of its zone.
        run(
            &mut tracker,
            &[cc(7, 101, 0), cc(7, 100, 0), cc(7, 38, 127)],
        );
        assert_eq!(ranges(&tracker), (2499, 200));
        // On the master channel the cents go under the default of two
        // semitones, which nobody sent.
        run(&mut tracker, &[cc(0, 101, 0), cc(0, 100, 0), cc(0, 38, 10)]);
        assert_eq!(ranges(&tracker), (2499, 210));
        // A coarse byte sets the cents back to zero, as MIDI asks.
        run(&mut tracker, &[cc(7, 6, 12)]);
        assert_eq!(ranges(&tracker), (1200, 210));
        // Increment and decrement are not followed, and what comes after
        // them is merged with the range the tracker holds.
        run(&mut tracker, &[cc(7, 96, 0), cc(7, 38, 1)]);
        assert_eq!(ranges(&tracker), (1201, 210));
    }

    #[test]
    fn ended_notes_leave_no_trace_in_equality_debug_or_ids() {
        let mut first = NoteTracker::<4>::new();
        let mut second = NoteTracker::<4>::new();
        run(&mut first, &[on(0, 60, 100), off(0, 60)]);
        run(&mut second, &[on(0, 61, 90), off(0, 61)]);
        // The same notes (none), zones, channel values and next id.
        assert_eq!(first, second);
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert!(format!("{first:?}").contains("notes: []"));
        // A sounding note, a channel value or the next id tell them apart.
        run(&mut first, &[on(0, 60, 100)]);
        run(&mut second, &[on(0, 61, 100)]);
        assert_ne!(first, second);
        assert!(format!("{first:?}").contains("key: 60"));
        assert!(!format!("{first:?}").contains("key: 61"));
        run(&mut first, &[off(0, 60)]);
        run(&mut second, &[off(0, 61)]);
        assert_eq!(first, second);
        run(&mut first, &[bend(5, 1)]);
        assert_ne!(first, second);
        run(&mut first, &[bend(5, 0), on(0, 60, 100), off(0, 60)]);
        assert_ne!(first, second, "the next id differs");

        // Eviction, replacement and "all sound off" free entries in the
        // middle of the array; what is left compares by content.
        let mut busy = NoteTracker::<3>::new();
        run(
            &mut busy,
            &[
                on(0, 1, 1),
                on(0, 2, 1),
                on(0, 3, 1),
                on(0, 4, 1),
                on(0, 2, 1),
                cc(0, 120, 0),
            ],
        );
        let mut fresh = NoteTracker::<3>::new();
        for _ in 0..5 {
            run(&mut fresh, &[on(9, 9, 9), off(9, 9)]);
        }
        assert_eq!(busy, fresh);

        // Ids carry on across a reset, so a consumer that keys voices by
        // id cannot mistake a new note for one from before.
        let mut tracker = lower_zone_tracker();
        run(&mut tracker, &[on(1, 60, 100), on(2, 61, 100)]);
        let before: Vec<u32> = tracker.notes().iter().map(|note| note.id).collect();
        tracker.reset();
        assert!(tracker.notes().is_empty());
        run(&mut tracker, &[on(1, 60, 100)]);
        assert_eq!(before, [0, 1]);
        assert_eq!(tracker.notes()[0].id, 2);
        // A tracker that was reset equals a new one that has counted as
        // far.
        let mut counted = lower_zone_tracker();
        run(
            &mut counted,
            &[
                on(1, 1, 1),
                off(1, 1),
                on(1, 2, 1),
                off(1, 2),
                on(1, 60, 100),
            ],
        );
        assert_eq!(tracker, counted);
    }

    #[test]
    fn pitch_bend_range_is_set_per_zone_level() {
        let mut layout = ZoneLayout::new();
        layout.configure(Configuration {
            zone: ZoneKind::Lower,
            members: 7,
        });
        layout.configure(Configuration {
            zone: ZoneKind::Upper,
            members: 7,
        });
        let mut tracker = NoteTracker::<8>::with_layout(layout);
        run(
            &mut tracker,
            &[on(1, 60, 100), on(7, 61, 100), on(8, 62, 100)],
        );
        let ranges = |tracker: &NoteTracker<8>| -> Vec<(u16, u16)> {
            tracker
                .notes()
                .iter()
                .map(|note| (note.bend_range_cents, note.master_bend_range_cents))
                .collect()
        };
        // Section 2.4: 48 semitones for members and 2 for masters.
        assert_eq!(ranges(&tracker), [(4800, 200), (4800, 200), (4800, 200)]);

        // Sent on one member channel, the range applies to every member
        // of that zone and to no other channel. 12 semitones and 50 cents.
        assert_eq!(
            run(
                &mut tracker,
                &[cc(3, 101, 0), cc(3, 100, 0), cc(3, 6, 12), cc(3, 38, 50)]
            ),
            ["change 1:60", "change 7:61", "change 1:60", "change 7:61"]
        );
        assert_eq!(ranges(&tracker), [(1250, 200), (1250, 200), (4800, 200)]);
        // Sent on the master channel, it sets the master range alone.
        run(
            &mut tracker,
            &[cc(15, 101, 0), cc(15, 100, 0), cc(15, 6, 7)],
        );
        assert_eq!(ranges(&tracker), [(1250, 200), (1250, 200), (4800, 700)]);
        // A fine byte above 99 cents is taken as 99.
        run(&mut tracker, &[cc(15, 38, 127)]);
        assert_eq!(tracker.notes()[2].master_bend_range_cents, 799);

        // A configuration message puts its zone's ranges back, even when
        // the zone keeps its size, and leaves the other zone alone.
        assert_eq!(
            run(&mut tracker, &configure(ZoneKind::Lower, 7)),
            ["change 1:60", "change 7:61"]
        );
        assert_eq!(ranges(&tracker), [(4800, 200), (4800, 200), (4800, 799)]);
    }

    #[test]
    fn reconfiguring_zones_ends_the_notes_on_channels_that_change_role() {
        let mut tracker = NoteTracker::<8>::new();
        // No zones yet: all channels are ordinary.
        assert_eq!(
            run(
                &mut tracker,
                &[
                    on(0, 40, 100),
                    on(3, 60, 100),
                    on(9, 61, 100),
                    on(15, 62, 100)
                ]
            ),
            ["start 0:40", "start 3:60", "start 9:61", "start 15:62"]
        );
        assert!(tracker.notes().iter().all(|note| note.zone.is_none()));

        // A lower zone of seven: channels 0 to 7 change role, so their
        // notes end. Channels 9 and 15 are untouched.
        assert_eq!(
            run(&mut tracker, &configure(ZoneKind::Lower, 7)),
            ["end 0:40", "end 3:60", "layout"]
        );
        assert_eq!(tracker.layout().lower().unwrap().member_count(), 7);
        run(&mut tracker, &[on(7, 70, 100)]);

        // An upper zone of ten takes channels 5 to 7 from the lower zone,
        // which shrinks to four members.
        assert_eq!(
            run(&mut tracker, &configure(ZoneKind::Upper, 10)),
            ["end 7:70", "end 9:61", "end 15:62", "layout"]
        );
        assert_eq!(tracker.layout().lower().unwrap().member_count(), 4);
        assert_eq!(tracker.layout().role(5), Role::Member(ZoneKind::Upper));
        assert!(tracker.notes().is_empty());

        // The same configuration again changes nothing.
        run(&mut tracker, &[on(5, 71, 100), on(2, 72, 100)]);
        assert_eq!(run(&mut tracker, &configure(ZoneKind::Upper, 10)), [""; 0]);
        // Switching a zone off returns its channels to ordinary use.
        assert_eq!(
            run(&mut tracker, &configure(ZoneKind::Upper, 0)),
            ["end 5:71", "layout"]
        );
        assert_eq!(tracker.layout().upper(), None);
        assert_eq!(tracker.notes().len(), 1);

        // Setting a layout directly behaves the same way.
        let mut events = Vec::new();
        tracker.set_layout(ZoneLayout::new(), |event| events.push(event));
        assert!(matches!(
            events[..],
            [NoteEvent::Ended(_), NoteEvent::LayoutChanged(_)]
        ));
        assert!(tracker.notes().is_empty());
        assert!(!tracker.layout().is_active());
    }

    #[test]
    fn notes_that_share_a_channel_are_shaped_together() {
        // What happens when a sender runs out of member channels.
        let mut layout = ZoneLayout::new();
        layout.configure(Configuration {
            zone: ZoneKind::Lower,
            members: 1,
        });
        let mut tracker = NoteTracker::<4>::with_layout(layout);
        run(&mut tracker, &[on(1, 60, 100), on(1, 67, 100)]);
        assert_eq!(
            run(&mut tracker, &[bend(1, 4096)]),
            ["change 1:60", "change 1:67"]
        );
        assert_eq!(tracker.note(1, 60).unwrap().pitch(), 84.0);
        assert_eq!(tracker.note(1, 67).unwrap().pitch(), 91.0);
        // They are still two notes, ended separately.
        assert_eq!(run(&mut tracker, &[off(1, 60)]), ["end 1:60"]);
        assert_eq!(tracker.notes().len(), 1);
    }

    #[test]
    fn the_same_key_again_replaces_the_note_and_a_full_tracker_drops_the_oldest() {
        let mut tracker = NoteTracker::<3>::with_layout({
            let mut layout = ZoneLayout::new();
            layout.configure(Configuration {
                zone: ZoneKind::Lower,
                members: 15,
            });
            layout
        });
        assert_eq!(
            run(&mut tracker, &[on(1, 60, 100), on(1, 60, 90)]),
            ["start 1:60", "end 1:60", "start 1:60"]
        );
        assert_eq!(tracker.notes().len(), 1);
        assert_eq!(tracker.notes()[0].velocity, 90);
        let ids: Vec<u32> = {
            run(&mut tracker, &[on(2, 61, 100), on(3, 62, 100)]);
            tracker.notes().iter().map(|note| note.id).collect()
        };
        assert_eq!(ids, [1, 2, 3]);
        // A fourth note does not fit: the oldest is ended.
        assert_eq!(
            run(&mut tracker, &[on(4, 63, 100)]),
            ["end 1:60", "start 4:63"]
        );
        let keys: Vec<u8> = tracker.notes().iter().map(|note| note.key).collect();
        assert_eq!(keys, [61, 62, 63]);
        // A note-on with velocity zero is a note-off.
        assert_eq!(run(&mut tracker, &[on(3, 62, 0)]), ["end 3:62"]);

        // A tracker with no room at all ignores notes without panicking.
        let mut none = NoteTracker::<0>::new();
        assert_eq!(
            run(&mut none, &[on(0, 60, 100), off(0, 60), bend(0, 1)]),
            [""; 0]
        );

        tracker.reset();
        assert!(tracker.notes().is_empty());
        assert_eq!(tracker.layout().lower().unwrap().member_count(), 15);
    }

    #[test]
    fn channels_outside_zones_behave_as_ordinary_midi() {
        let mut tracker = NoteTracker::<8>::default();
        run(
            &mut tracker,
            &[on(4, 60, 100), on(4, 64, 100), on(5, 67, 100)],
        );
        // One wheel for the whole channel, with the usual two semitones.
        assert_eq!(
            run(&mut tracker, &[bend(4, -8192)]),
            ["change 4:60", "change 4:64"]
        );
        let note = *tracker.note(4, 60).unwrap();
        assert_eq!(note.zone, None);
        assert_eq!(note.pitch(), 58.0);
        assert_eq!(tracker.note(5, 67).unwrap().pitch(), 67.0);
        // Channel pressure and polyphonic pressure both arrive.
        run(
            &mut tracker,
            &[
                pressure(4, 30),
                MidiMessage::PolyPressure {
                    channel: 4,
                    note: 64,
                    pressure: 80,
                },
            ],
        );
        assert_eq!(tracker.note(4, 60).unwrap().combined_pressure(), 30);
        assert_eq!(tracker.note(4, 64).unwrap().combined_pressure(), 80);
        // Each channel has its own pedal.
        assert_eq!(
            run(&mut tracker, &[cc(4, 64, 127), off(4, 60), off(5, 67)]),
            ["release 4:60", "end 5:67"]
        );
        assert_eq!(
            run(&mut tracker, &[cc(5, 64, 0), cc(4, 64, 63)]),
            ["end 4:60"]
        );
        // And its own "all sound off".
        assert_eq!(
            run(&mut tracker, &[cc(5, 120, 0), cc(4, 120, 0)]),
            ["end 4:64"]
        );
    }

    #[test]
    fn the_allocator_fills_free_channels_before_sharing() {
        let mut allocator = ChannelAllocator::new(Zone::lower(3).unwrap());
        assert_eq!(allocator.zone().member_count(), 3);
        // Three notes, three channels, nearest the master first.
        assert_eq!(
            [
                allocator.note_on(60),
                allocator.note_on(62),
                allocator.note_on(64)
            ],
            [1, 2, 3]
        );
        assert!(!allocator.is_shared(1));
        // Out of channels: the fourth note shares. All hold one note and
        // none has had a note-off, so the first member is chosen.
        assert_eq!(allocator.note_on(65), 1);
        assert!(allocator.is_shared(1));
        assert_eq!(allocator.note_count(1), 2);
        // The fifth goes where there are fewest notes: channel 2 and 3
        // hold one each.
        assert_eq!(allocator.note_on(67), 2);
        assert_eq!(allocator.note_on(69), 3);
        // All hold two; a seventh makes three on the first.
        assert_eq!(allocator.note_on(71), 1);
        assert_eq!(
            [
                allocator.note_count(1),
                allocator.note_count(2),
                allocator.note_count(3)
            ],
            [3, 2, 2]
        );
        // Channels that are not members hold nothing.
        assert_eq!(allocator.note_count(0), 0);
        assert_eq!(allocator.note_count(4), 0);
        assert!(!allocator.note_off(4, 60));
        assert!(!allocator.note_off(0, 60));
    }

    #[test]
    fn the_allocator_prefers_the_channel_released_longest_ago() {
        let mut allocator = ChannelAllocator::new(Zone::lower(3).unwrap());
        for key in [60, 62, 64] {
            allocator.note_on(key);
        }
        // Released in the order 2, 3, 1.
        assert!(allocator.note_off(2, 62));
        assert!(allocator.note_off(3, 64));
        assert!(allocator.note_off(1, 60));
        // Not round robin: new keys go to the channel whose release tail
        // is oldest.
        assert_eq!(allocator.note_on(70), 2);
        assert_eq!(allocator.note_on(71), 3);
        assert_eq!(allocator.note_on(72), 1);
        // With one channel free it is used, whatever the others hold.
        assert!(allocator.note_off(3, 71));
        assert_eq!(allocator.note_on(73), 3);
        // A note-off for a key that is not sounding is reported as such.
        assert!(!allocator.note_off(3, 71));
    }

    #[test]
    fn the_allocator_returns_a_repeated_key_to_its_channel() {
        let mut allocator = ChannelAllocator::new(Zone::lower(4).unwrap());
        for key in [60, 62, 64, 65] {
            allocator.note_on(key);
        }
        // Key 62 is released last, so by age alone its channel would be
        // the last choice.
        for (channel, key) in [(1, 60), (3, 64), (4, 65), (2, 62)] {
            assert!(allocator.note_off(channel, key));
        }
        // Section 3.2: the same key goes back to the channel it last
        // played on, so the note does not stack on its own release tail.
        assert_eq!(allocator.note_on(62), 2);
        // Other keys follow the age rule.
        assert_eq!(allocator.note_on(50), 1);
        // While a key is still sounding, playing it again uses another
        // channel: the same pitch can be active twice.
        assert_eq!(allocator.note_on(62), 3);
        assert_eq!(allocator.channel_of(62), Some(3));
        assert_eq!(allocator.release(62), Some(3));
        assert_eq!(allocator.release(62), Some(2));
        assert_eq!(allocator.release(62), None);
        assert_eq!(allocator.channel_of(62), None);
    }

    #[test]
    fn the_allocator_counts_from_the_master_in_the_upper_zone() {
        let mut allocator = ChannelAllocator::new(Zone::upper(2).unwrap());
        assert_eq!([allocator.note_on(60), allocator.note_on(61)], [14, 13]);
        assert!(allocator.note_off(14, 60));
        assert!(!allocator.note_off(12, 60));
        assert_eq!(allocator.note_on(62), 14);
        allocator.reset();
        assert_eq!(allocator, ChannelAllocator::new(Zone::upper(2).unwrap()));
        // Fifteen members: every channel but the master, and a sixteenth
        // note shares the first.
        let mut full = ChannelAllocator::new(Zone::lower(15).unwrap());
        let channels: Vec<u8> = (0..16).map(|key| full.note_on(key)).collect();
        assert_eq!(channels[..15], (1..=15).collect::<Vec<u8>>()[..]);
        assert_eq!(channels[15], 1);
    }

    #[test]
    fn what_the_allocator_sends_the_tracker_keeps_apart() {
        // A sender with a zone of four plays five notes and bends each
        // one differently; the receiver sees the first four apart and the
        // fifth sharing with the first.
        let zone = Zone::lower(4).unwrap();
        let mut allocator = ChannelAllocator::new(zone);
        let mut layout = ZoneLayout::new();
        layout.configure(zone.configuration());
        let mut tracker = NoteTracker::<8>::with_layout(layout);
        for (index, key) in [60u8, 62, 64, 65, 67].into_iter().enumerate() {
            let channel = allocator.note_on(key);
            let amount = 100 * (index as i16 + 1);
            run(
                &mut tracker,
                &[bend(channel, amount), on(channel, key, 100)],
            );
        }
        let bends: Vec<(u8, i16)> = tracker
            .notes()
            .iter()
            .map(|note| (note.key, note.pitch_bend))
            .collect();
        assert_eq!(
            bends,
            [(60, 500), (62, 200), (64, 300), (65, 400), (67, 500)]
        );
        assert!(allocator.is_shared(1));
    }
}
