//! Registered and non-registered parameter numbers (RPN and NRPN).
//!
//! MIDI 1.0 has room for 128 controllers, so settings that are changed
//! rarely are reached in two steps. A device first says which parameter it
//! means, with controllers 101 and 100 (registered) or 99 and 98
//! (non-registered), and then sends the value with the data entry
//! controllers 6 and 38, or nudges it with 96 and 97. Sending 127 for both
//! halves of the number selects the "null" parameter, after which data
//! entry does nothing.
//!
//! - [`RpnDecoder`] follows a stream and reports each value as a
//!   [`ParameterEvent`].
//! - [`RpnEncoder`] produces the controller messages for a value and skips
//!   the selection when the parameter is already selected.
//! - [`sequence`], [`select`] and [`deselect`] produce the messages with no
//!   state.
//!
//! Written from the MIDI 1.0 Detailed Specification (the MIDI Association),
//! chapter "Registered and Non-Registered Parameter Numbers", and from
//! recommended practice RP-015 for what "reset all controllers" does to the
//! selection.
//!
//! ```
//! use manguebyte_midi::rpn::{Change, Parameter, RpnDecoder};
//! use manguebyte_midi::MidiMessage;
//!
//! let cc = |controller, value| MidiMessage::ControlChange { channel: 3, controller, value };
//! let mut decoder = RpnDecoder::new();
//! let mut seen = Vec::new();
//! // Select RPN 0 (pitch bend sensitivity) and set it to 12 semitones.
//! for message in [cc(101, 0), cc(100, 0), cc(6, 12), cc(38, 0)] {
//!     seen.extend(decoder.feed(message));
//! }
//! assert_eq!(seen.len(), 2); // once for the coarse byte, once for the fine one
//! assert_eq!(seen[1].channel, 3);
//! assert_eq!(seen[1].parameter, Parameter::PITCH_BEND_SENSITIVITY);
//! assert_eq!(seen[1].change, Change::Value { value: 12 << 7, fine: true });
//! ```

use crate::{MessageList, MidiMessage};

/// Controller numbers that make up parameter sequences.
mod controller {
    pub const DATA_ENTRY_MSB: u8 = 6;
    pub const DATA_ENTRY_LSB: u8 = 38;
    pub const DATA_INCREMENT: u8 = 96;
    pub const DATA_DECREMENT: u8 = 97;
    pub const NRPN_LSB: u8 = 98;
    pub const NRPN_MSB: u8 = 99;
    pub const RPN_LSB: u8 = 100;
    pub const RPN_MSB: u8 = 101;
    pub const RESET_ALL_CONTROLLERS: u8 = 121;
}

/// The two families of parameter numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterKind {
    /// Registered: the meaning is defined by the MIDI Association.
    /// Selected with controllers 101 and 100.
    Registered,
    /// Non-registered: the meaning is up to the device. Selected with
    /// controllers 99 and 98.
    NonRegistered,
}

/// A parameter number: its family and the two 7-bit halves of the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Parameter {
    /// Registered or non-registered.
    pub kind: ParameterKind,
    /// Most significant byte of the number, `0..=127`.
    pub msb: u8,
    /// Least significant byte of the number, `0..=127`.
    pub lsb: u8,
}

impl Parameter {
    /// RPN 0: pitch bend sensitivity. The value's coarse byte is in
    /// semitones and its fine byte in cents.
    pub const PITCH_BEND_SENSITIVITY: Self = Self::registered(0, 0);
    /// RPN 1: channel fine tuning, with `0x2000` meaning no detuning.
    pub const FINE_TUNING: Self = Self::registered(0, 1);
    /// RPN 2: channel coarse tuning in semitones, with a coarse byte of 64
    /// meaning no transposition.
    pub const COARSE_TUNING: Self = Self::registered(0, 2);
    /// RPN 3: tuning program change.
    pub const TUNING_PROGRAM: Self = Self::registered(0, 3);
    /// RPN 4: tuning bank select.
    pub const TUNING_BANK: Self = Self::registered(0, 4);
    /// RPN 5: modulation depth range.
    pub const MODULATION_DEPTH_RANGE: Self = Self::registered(0, 5);
    /// RPN 6: the MPE configuration message. See [`mpe`](crate::mpe).
    pub const MPE_CONFIGURATION: Self = Self::registered(0, 6);
    /// The null parameter, `127, 127`: selecting it makes data entry do
    /// nothing until another parameter is selected.
    pub const NULL: Self = Self::registered(127, 127);

    /// A registered parameter number.
    pub const fn registered(msb: u8, lsb: u8) -> Self {
        Self {
            kind: ParameterKind::Registered,
            msb,
            lsb,
        }
    }

    /// A non-registered parameter number.
    pub const fn non_registered(msb: u8, lsb: u8) -> Self {
        Self {
            kind: ParameterKind::NonRegistered,
            msb,
            lsb,
        }
    }

    /// The number as one 14-bit value, `0..=16383`.
    pub const fn number(&self) -> u16 {
        (((self.msb & 0x7F) as u16) << 7) | (self.lsb & 0x7F) as u16
    }

    /// Whether this is the null parameter of either family.
    pub const fn is_null(&self) -> bool {
        self.msb & 0x7F == 0x7F && self.lsb & 0x7F == 0x7F
    }

    /// The controllers that carry the two halves of the number: most
    /// significant first.
    const fn controllers(&self) -> (u8, u8) {
        match self.kind {
            ParameterKind::Registered => (controller::RPN_MSB, controller::RPN_LSB),
            ParameterKind::NonRegistered => (controller::NRPN_MSB, controller::NRPN_LSB),
        }
    }
}

/// What happened to a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// A data entry controller set the value, and both of its halves are
    /// known.
    Value {
        /// The 14-bit value, `0..=16383`: the coarse byte in the upper
        /// seven bits and the fine byte in the lower seven.
        ///
        /// When the coarse byte arrives (controller 6) the fine byte is
        /// taken as zero, which is what the MIDI specification asks of a
        /// receiver. When the fine byte arrives (controller 38) it is
        /// joined to the coarse byte that was sent for this parameter
        /// since it was selected.
        value: u16,
        /// Whether this came from the fine byte. A device that sends both
        /// bytes produces two events: one with `fine` false, then one with
        /// `fine` true and the complete value.
        fine: bool,
    },
    /// The fine data byte (controller 38) arrived, `0..=127`, and the
    /// decoder cannot say which coarse byte it belongs under: none has
    /// been sent since this parameter was selected, or an increment or
    /// decrement has moved the value since.
    ///
    /// MIDI allows a sender to adjust the fine half alone. The receiver
    /// keeps the coarse half of the value the parameter has, which only
    /// the receiver knows, and replaces the fine half.
    FineOnly(u8),
    /// The data increment controller (96) asked for the value to go up.
    /// The byte it carried is passed on; most devices send zero and mean
    /// one step.
    Increment(u8),
    /// The data decrement controller (97) asked for the value to go down.
    Decrement(u8),
}

impl Change {
    /// The coarse byte of a [`Value`](Self::Value), `0..=127`. `None` for
    /// a [`FineOnly`](Self::FineOnly), which does not know it.
    pub fn coarse(&self) -> Option<u8> {
        match *self {
            Self::Value { value, .. } => Some((value >> 7) as u8 & 0x7F),
            _ => None,
        }
    }

    /// The fine byte of a [`Value`](Self::Value) or a
    /// [`FineOnly`](Self::FineOnly), `0..=127`.
    pub fn fine(&self) -> Option<u8> {
        match *self {
            Self::Value { value, .. } => Some(value as u8 & 0x7F),
            Self::FineOnly(fine) => Some(fine & 0x7F),
            _ => None,
        }
    }
}

/// A change to a parameter on one channel, as [`RpnDecoder`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParameterEvent {
    /// Channel, `0..=15`.
    pub channel: u8,
    /// The parameter that was selected when the change arrived.
    pub parameter: Parameter,
    /// What happened to it.
    pub change: Change,
}

/// The parameter state of one channel: which number is selected and the
/// coarse data byte given for it, if one was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelState {
    /// The family whose number was touched last, if any was.
    kind: Option<ParameterKind>,
    /// Controllers 101 and 100 as last received.
    registered: [u8; 2],
    /// Controllers 99 and 98 as last received.
    non_registered: [u8; 2],
    /// Controller 6 as received since the current parameter was selected,
    /// while it still is the coarse half of the parameter's value.
    data_msb: Option<u8>,
}

impl ChannelState {
    /// The state a receiver starts in: nothing selected.
    const NEW: Self = Self {
        kind: None,
        registered: [0x7F; 2],
        non_registered: [0x7F; 2],
        data_msb: None,
    };

    /// The parameter the number controllers name, null or not.
    fn named(&self) -> Option<Parameter> {
        let (kind, [msb, lsb]) = match self.kind? {
            ParameterKind::Registered => (ParameterKind::Registered, self.registered),
            ParameterKind::NonRegistered => (ParameterKind::NonRegistered, self.non_registered),
        };
        Some(Parameter { kind, msb, lsb })
    }

    fn selected(&self) -> Option<Parameter> {
        self.named().filter(|parameter| !parameter.is_null())
    }

    /// Takes one of the four parameter number controllers.
    fn select(&mut self, kind: ParameterKind, lsb: bool, value: u8) {
        let before = self.named();
        let pair = match kind {
            ParameterKind::Registered => &mut self.registered,
            ParameterKind::NonRegistered => &mut self.non_registered,
        };
        pair[usize::from(lsb)] = value;
        self.kind = Some(kind);
        // A coarse byte belongs to the parameter it was sent for. Naming
        // the same parameter again keeps it, so that a sender can select,
        // send both bytes, and later select again and adjust the fine
        // byte alone.
        if self.named() != before {
            self.data_msb = None;
        }
    }

    fn control_change(&mut self, number: u8, value: u8) -> Option<(Parameter, Change)> {
        let value = value & 0x7F;
        match number {
            controller::RPN_MSB | controller::RPN_LSB => {
                self.select(
                    ParameterKind::Registered,
                    number == controller::RPN_LSB,
                    value,
                );
                None
            }
            controller::NRPN_MSB | controller::NRPN_LSB => {
                self.select(
                    ParameterKind::NonRegistered,
                    number == controller::NRPN_LSB,
                    value,
                );
                None
            }
            controller::DATA_ENTRY_MSB => {
                let parameter = self.selected()?;
                self.data_msb = Some(value);
                Some((
                    parameter,
                    Change::Value {
                        value: u16::from(value) << 7,
                        fine: false,
                    },
                ))
            }
            controller::DATA_ENTRY_LSB => {
                let parameter = self.selected()?;
                let change = match self.data_msb {
                    Some(coarse) => Change::Value {
                        value: (u16::from(coarse) << 7) | u16::from(value),
                        fine: true,
                    },
                    None => Change::FineOnly(value),
                };
                Some((parameter, change))
            }
            controller::DATA_INCREMENT | controller::DATA_DECREMENT => {
                let parameter = self.selected()?;
                // The receiver moves the value by a step of its own
                // choosing, so the coarse byte is no longer known here.
                self.data_msb = None;
                let change = if number == controller::DATA_INCREMENT {
                    Change::Increment(value)
                } else {
                    Change::Decrement(value)
                };
                Some((parameter, change))
            }
            controller::RESET_ALL_CONTROLLERS => {
                // RP-015: reset all controllers sets both parameter
                // numbers to null.
                *self = Self::NEW;
                None
            }
            _ => None,
        }
    }
}

/// Follows the controller messages of all sixteen channels and reports
/// parameter changes.
///
/// [`feed`](Self::feed) is real-time safe: the decoder is a fixed array of
/// sixteen small states.
///
/// Data entry with no parameter selected, or with the null parameter
/// selected, is ignored. A device that sends only one half of a parameter
/// number leaves the other half at whatever was sent before, which at
/// start-up is 127.
///
/// # Limits
///
/// The decoder does not store the values of parameters; there are 32768
/// of them on each channel. It remembers one coarse data byte per
/// channel: the one sent for the parameter that is selected now. It keeps
/// that byte while the same parameter stays selected, also when its
/// number is sent again, and forgets it when another parameter or the
/// null parameter is selected, on an increment or decrement, and on
/// "reset all controllers". A fine data byte that arrives without it is
/// reported as [`Change::FineOnly`], never joined to a coarse byte nobody
/// sent, and the receiver has to merge it with the value it holds.
///
/// ```
/// use manguebyte_midi::rpn::{Change, RpnDecoder};
/// use manguebyte_midi::MidiMessage;
///
/// let cc = |controller, value| MidiMessage::ControlChange { channel: 0, controller, value };
/// let mut decoder = RpnDecoder::new();
/// let mut changes = Vec::new();
/// for message in [
///     // Pitch bend sensitivity: 24 semitones and no cents.
///     cc(101, 0), cc(100, 0), cc(6, 24), cc(38, 0),
///     // The same parameter named again, and the cents alone: complete.
///     cc(101, 0), cc(100, 0), cc(38, 50),
///     // Deselected and selected again: the semitones are not known here.
///     cc(101, 127), cc(100, 127), cc(101, 0), cc(100, 0), cc(38, 25),
/// ] {
///     changes.extend(decoder.feed(message).map(|event| event.change));
/// }
/// assert_eq!(
///     changes[2..],
///     [Change::Value { value: (24 << 7) | 50, fine: true }, Change::FineOnly(25)]
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpnDecoder {
    channels: [ChannelState; 16],
}

impl RpnDecoder {
    /// A decoder with no parameter selected on any channel.
    pub const fn new() -> Self {
        Self {
            channels: [ChannelState::NEW; 16],
        }
    }

    /// Forgets every selection, as after a device reset.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Takes the next message of the stream. Returns an event when it is a
    /// data entry, increment or decrement controller arriving while a
    /// parameter is selected. Messages that are not controllers are
    /// ignored.
    pub fn feed(&mut self, message: MidiMessage) -> Option<ParameterEvent> {
        let MidiMessage::ControlChange {
            channel,
            controller,
            value,
        } = message
        else {
            return None;
        };
        let channel = channel & 0x0F;
        self.channels[usize::from(channel)]
            .control_change(controller, value)
            .map(|(parameter, change)| ParameterEvent {
                channel,
                parameter,
                change,
            })
    }

    /// The parameter selected on `channel`, or `None` if there is none or
    /// it is the null parameter.
    pub fn selected(&self, channel: u8) -> Option<Parameter> {
        self.channels[usize::from(channel & 0x0F)].selected()
    }
}

impl Default for RpnDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn cc(channel: u8, controller: u8, value: u8) -> MidiMessage {
    MidiMessage::ControlChange {
        channel: channel & 0x0F,
        controller,
        value: value & 0x7F,
    }
}

/// The two messages that select `parameter` on `channel`: most significant
/// byte of the number first.
pub fn select(channel: u8, parameter: Parameter) -> MessageList {
    let (msb, lsb) = parameter.controllers();
    MessageList::from_slice(&[
        cc(channel, msb, parameter.msb),
        cc(channel, lsb, parameter.lsb),
    ])
}

/// The two messages that select the null parameter on `channel`, so that
/// stray data entry messages change nothing.
pub fn deselect(channel: u8) -> MessageList {
    select(channel, Parameter::NULL)
}

/// The four messages that set `parameter` to a 14-bit `value` on
/// `channel`: the two halves of the number, then the coarse and fine data
/// bytes.
///
/// ```
/// use manguebyte_midi::rpn::{self, Parameter};
///
/// // NRPN 0x0102 = 0x1234 on channel 5, as bytes on the wire.
/// let bytes: Vec<u8> = rpn::sequence(5, Parameter::non_registered(1, 2), 0x1234)
///     .iter()
///     .flat_map(|message| message.to_bytes().as_slice().to_vec())
///     .collect();
/// assert_eq!(
///     bytes,
///     [0xB5, 99, 1, 0xB5, 98, 2, 0xB5, 6, 0x24, 0xB5, 38, 0x34]
/// );
/// ```
pub fn sequence(channel: u8, parameter: Parameter, value: u16) -> MessageList {
    let mut list = select(channel, parameter);
    list.push(cc(channel, controller::DATA_ENTRY_MSB, (value >> 7) as u8));
    list.push(cc(channel, controller::DATA_ENTRY_LSB, value as u8));
    list
}

/// Produces parameter sequences and remembers what is selected on each
/// channel, so that the two selection messages are sent only when the
/// parameter changes.
///
/// The encoder assumes it is the only source of parameter messages on the
/// connection. If something else may have changed the selection, call
/// [`reset`](Self::reset). All methods are real-time safe.
///
/// ```
/// use manguebyte_midi::rpn::{Parameter, RpnEncoder};
///
/// let mut encoder = RpnEncoder::new();
/// let range = Parameter::PITCH_BEND_SENSITIVITY;
/// assert_eq!(encoder.set(0, range, 2 << 7).len(), 4); // select, then two data bytes
/// assert_eq!(encoder.set(0, range, 12 << 7).len(), 2); // already selected
/// assert_eq!(encoder.set(1, range, 12 << 7).len(), 4); // another channel
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpnEncoder {
    selected: [Option<Parameter>; 16],
}

impl RpnEncoder {
    /// An encoder that assumes nothing is selected on any channel.
    pub const fn new() -> Self {
        Self {
            selected: [None; 16],
        }
    }

    /// Forgets what is selected, so that the next value on each channel is
    /// preceded by a selection again.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The messages that set `parameter` to a 14-bit `value`: the
    /// selection if needed, then the coarse and fine data bytes.
    pub fn set(&mut self, channel: u8, parameter: Parameter, value: u16) -> MessageList {
        let mut list = self.selection(channel, parameter);
        list.push(cc(channel, controller::DATA_ENTRY_MSB, (value >> 7) as u8));
        list.push(cc(channel, controller::DATA_ENTRY_LSB, value as u8));
        list
    }

    /// The messages that set only the coarse byte of `parameter`. A
    /// receiver takes the fine byte as zero.
    pub fn set_coarse(&mut self, channel: u8, parameter: Parameter, coarse: u8) -> MessageList {
        let mut list = self.selection(channel, parameter);
        list.push(cc(channel, controller::DATA_ENTRY_MSB, coarse));
        list
    }

    /// The messages that ask for `parameter` to go up by one step.
    pub fn increment(&mut self, channel: u8, parameter: Parameter) -> MessageList {
        let mut list = self.selection(channel, parameter);
        list.push(cc(channel, controller::DATA_INCREMENT, 0));
        list
    }

    /// The messages that ask for `parameter` to go down by one step.
    pub fn decrement(&mut self, channel: u8, parameter: Parameter) -> MessageList {
        let mut list = self.selection(channel, parameter);
        list.push(cc(channel, controller::DATA_DECREMENT, 0));
        list
    }

    /// The messages that select the null parameter on `channel`, or none
    /// if the encoder has already done so.
    pub fn deselect(&mut self, channel: u8) -> MessageList {
        self.selection(channel, Parameter::NULL)
    }

    fn selection(&mut self, channel: u8, parameter: Parameter) -> MessageList {
        let slot = &mut self.selected[usize::from(channel & 0x0F)];
        // Compare what would be sent, so that out-of-range bytes do not
        // defeat the comparison.
        let parameter = Parameter {
            kind: parameter.kind,
            msb: parameter.msb & 0x7F,
            lsb: parameter.lsb & 0x7F,
        };
        // The null parameter is the same whichever family names it.
        let same = match *slot {
            Some(current) => current == parameter || (current.is_null() && parameter.is_null()),
            None => false,
        };
        if same {
            MessageList::new()
        } else {
            *slot = Some(parameter);
            select(channel, parameter)
        }
    }
}

impl Default for RpnEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(decoder: &mut RpnDecoder, messages: &[MidiMessage]) -> Vec<ParameterEvent> {
        messages
            .iter()
            .filter_map(|message| decoder.feed(*message))
            .collect()
    }

    fn value(channel: u8, parameter: Parameter, value: u16, fine: bool) -> ParameterEvent {
        ParameterEvent {
            channel,
            parameter,
            change: Change::Value { value, fine },
        }
    }

    #[test]
    fn a_full_sequence_reports_the_coarse_and_then_the_complete_value() {
        let mut decoder = RpnDecoder::new();
        let tuning = Parameter::FINE_TUNING;
        let events = feed_all(
            &mut decoder,
            &[
                cc(2, 101, 0),
                cc(2, 100, 1),
                cc(2, 6, 0x41),
                cc(2, 38, 0x05),
            ],
        );
        assert_eq!(
            events,
            [
                value(2, tuning, 0x41 << 7, false),
                value(2, tuning, (0x41 << 7) | 0x05, true),
            ]
        );
        assert_eq!(events[1].change.coarse(), Some(0x41));
        assert_eq!(events[1].change.fine(), Some(0x05));
        assert_eq!(decoder.selected(2), Some(tuning));
        // The selection stays, so further data needs no new selection, and
        // a fine byte alone adjusts under the last coarse byte.
        assert_eq!(
            feed_all(&mut decoder, &[cc(2, 38, 0x7F), cc(2, 6, 0x10)]),
            [
                value(2, tuning, (0x41 << 7) | 0x7F, true),
                // A new coarse byte sets the fine byte back to zero.
                value(2, tuning, 0x10 << 7, false),
            ]
        );
    }

    #[test]
    fn the_null_parameter_switches_data_entry_off() {
        let mut decoder = RpnDecoder::new();
        // Nothing selected at the start.
        assert_eq!(decoder.selected(0), None);
        assert_eq!(feed_all(&mut decoder, &[cc(0, 6, 1), cc(0, 38, 2)]), []);

        let select_and_null = [
            cc(0, 101, 0),
            cc(0, 100, 0),
            cc(0, 6, 2),
            cc(0, 101, 127),
            cc(0, 100, 127),
            cc(0, 6, 99),
            cc(0, 38, 99),
            cc(0, 96, 0),
            cc(0, 97, 0),
        ];
        assert_eq!(
            feed_all(&mut decoder, &select_and_null),
            [value(0, Parameter::PITCH_BEND_SENSITIVITY, 2 << 7, false)]
        );
        assert_eq!(decoder.selected(0), None);

        // Only both halves at 127 are null: 127 in one half is a number
        // like any other.
        let events = feed_all(&mut decoder, &[cc(0, 100, 5), cc(0, 6, 1)]);
        assert_eq!(events[0].parameter, Parameter::registered(127, 5));
        // The non-registered null works the same way.
        assert_eq!(
            feed_all(&mut decoder, &[cc(0, 99, 127), cc(0, 98, 127), cc(0, 6, 1)]),
            []
        );
    }

    #[test]
    fn registered_and_non_registered_numbers_are_kept_apart() {
        let mut decoder = RpnDecoder::new();
        let events = feed_all(
            &mut decoder,
            &[
                cc(9, 99, 3),
                cc(9, 98, 4),
                cc(9, 6, 5),
                // Switching family: the registered pair now applies.
                cc(9, 101, 0),
                cc(9, 100, 2),
                cc(9, 38, 6),
                // Touching one non-registered half switches back, and the
                // other half is still what it was.
                cc(9, 98, 7),
                cc(9, 6, 8),
            ],
        );
        assert_eq!(
            events,
            [
                value(9, Parameter::non_registered(3, 4), 5 << 7, false),
                // No coarse byte was given for this selection. The one
                // sent for the other parameter is not borrowed, and no
                // zero is made up in its place.
                ParameterEvent {
                    channel: 9,
                    parameter: Parameter::COARSE_TUNING,
                    change: Change::FineOnly(6),
                },
                value(9, Parameter::non_registered(3, 7), 8 << 7, false),
            ]
        );
        assert_eq!(events[1].change.coarse(), None);
        assert_eq!(events[1].change.fine(), Some(6));
        assert_eq!(Parameter::non_registered(3, 4).number(), (3 << 7) | 4);
        assert_eq!(Parameter::NULL.number(), 16_383);
    }

    #[test]
    fn increment_and_decrement_are_reported_with_their_byte() {
        let mut decoder = RpnDecoder::new();
        let events = feed_all(
            &mut decoder,
            &[cc(1, 101, 0), cc(1, 100, 0), cc(1, 96, 0), cc(1, 97, 3)],
        );
        let change: Vec<Change> = events.iter().map(|event| event.change).collect();
        assert_eq!(change, [Change::Increment(0), Change::Decrement(3)]);
        assert_eq!(change[0].coarse(), None);
        assert_eq!(change[0].fine(), None);
    }

    #[test]
    fn channels_do_not_share_state_and_other_messages_are_ignored() {
        let mut decoder = RpnDecoder::new();
        let events = feed_all(
            &mut decoder,
            &[
                cc(0, 101, 0),
                cc(0, 100, 0),
                cc(1, 6, 12),
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 6,
                    velocity: 38,
                },
                cc(0, 7, 100),
                cc(0, 6, 12),
            ],
        );
        assert_eq!(
            events,
            [value(0, Parameter::PITCH_BEND_SENSITIVITY, 12 << 7, false)]
        );
        for channel in 1..16 {
            assert_eq!(decoder.selected(channel), None);
        }
        decoder.reset();
        assert_eq!(decoder, RpnDecoder::default());
    }

    #[test]
    fn reset_all_controllers_clears_the_selection() {
        let mut decoder = RpnDecoder::new();
        let events = feed_all(
            &mut decoder,
            &[cc(0, 101, 0), cc(0, 100, 0), cc(0, 121, 0), cc(0, 6, 12)],
        );
        assert_eq!(events, []);
        assert_eq!(decoder.selected(0), None);
    }

    #[test]
    fn a_fine_byte_is_joined_only_to_a_coarse_byte_sent_for_this_selection() {
        // MIDI 1.0 Detailed Specification: "If both the MSB and LSB are
        // sent initially, a subsequent fine adjustment only requires the
        // sending of the LSB."
        let range = Parameter::PITCH_BEND_SENSITIVITY;
        let fine_only = |fine| ParameterEvent {
            channel: 1,
            parameter: range,
            change: Change::FineOnly(fine),
        };
        let mut decoder = RpnDecoder::new();
        let set = [cc(1, 101, 0), cc(1, 100, 0), cc(1, 6, 24), cc(1, 38, 0)];
        assert_eq!(feed_all(&mut decoder, &set).len(), 2);

        // The same parameter named again, in either order of its two
        // halves, and on another channel in between: the coarse byte
        // stands.
        assert_eq!(
            feed_all(&mut decoder, &[cc(1, 101, 0), cc(1, 100, 0), cc(1, 38, 50)]),
            [value(1, range, (24 << 7) | 50, true)]
        );
        assert_eq!(
            feed_all(
                &mut decoder,
                &[
                    cc(1, 100, 0),
                    cc(2, 101, 0),
                    cc(2, 100, 0),
                    cc(2, 6, 99),
                    cc(1, 101, 0),
                    cc(1, 38, 51)
                ]
            ),
            [
                value(2, range, 99 << 7, false),
                value(1, range, (24 << 7) | 51, true)
            ]
        );

        // Through the null parameter, as careful senders do after every
        // change: the decoder no longer knows the coarse byte and says so.
        assert_eq!(
            feed_all(
                &mut decoder,
                &[
                    cc(1, 101, 127),
                    cc(1, 100, 127),
                    cc(1, 101, 0),
                    cc(1, 100, 0),
                    cc(1, 38, 52)
                ]
            ),
            [fine_only(52)]
        );
        // A coarse byte makes the value whole again.
        assert_eq!(
            feed_all(&mut decoder, &[cc(1, 6, 12), cc(1, 38, 53)]),
            [
                value(1, range, 12 << 7, false),
                value(1, range, (12 << 7) | 53, true)
            ]
        );
        // Through another parameter and back, and through the other
        // family with the same number.
        assert_eq!(
            feed_all(&mut decoder, &[cc(1, 100, 1), cc(1, 100, 0), cc(1, 38, 54)]),
            [fine_only(54)]
        );
        assert_eq!(
            feed_all(
                &mut decoder,
                &[
                    cc(1, 6, 12),
                    cc(1, 99, 0),
                    cc(1, 98, 0),
                    cc(1, 101, 0),
                    cc(1, 38, 55)
                ]
            )[1..],
            [fine_only(55)]
        );
        // After an increment or a decrement the receiver has moved the
        // value by a step only it knows.
        for step in [96u8, 97] {
            let events = feed_all(&mut decoder, &[cc(1, 6, 12), cc(1, step, 0), cc(1, 38, 56)]);
            assert_eq!(events.len(), 3);
            assert_eq!(events[2], fine_only(56));
        }
    }

    #[test]
    fn sequences_have_the_documented_order_and_bytes() {
        // The pitch bend sensitivity example of the MIDI 1.0 specification:
        // Bn 65 00, Bn 64 00, Bn 06 mm, Bn 26 ll.
        let bytes: Vec<u8> = sequence(0, Parameter::PITCH_BEND_SENSITIVITY, (2 << 7) | 50)
            .iter()
            .flat_map(|message| message.to_bytes().as_slice().to_vec())
            .collect();
        assert_eq!(
            bytes,
            [
                0xB0, 0x65, 0x00, 0xB0, 0x64, 0x00, 0xB0, 0x06, 0x02, 0xB0, 0x26, 0x32
            ]
        );
        assert_eq!(deselect(4).as_slice(), [cc(4, 101, 127), cc(4, 100, 127)]);
        assert_eq!(
            select(4, Parameter::non_registered(1, 2)).as_slice(),
            [cc(4, 99, 1), cc(4, 98, 2)]
        );
    }

    #[test]
    fn what_the_encoder_sends_the_decoder_reads_back() {
        let mut encoder = RpnEncoder::new();
        let mut decoder = RpnDecoder::new();
        let parameters = [
            Parameter::PITCH_BEND_SENSITIVITY,
            Parameter::MPE_CONFIGURATION,
            Parameter::registered(0x3D, 0x7F),
            Parameter::non_registered(0, 0),
            Parameter::non_registered(127, 0),
            Parameter::non_registered(0x55, 0x2A),
        ];
        let mut sent = 0;
        for round in 0..3u16 {
            for (index, parameter) in parameters.into_iter().enumerate() {
                for channel in [0u8, 7, 15] {
                    let wanted = (round * 5_000 + index as u16 * 911 + u16::from(channel)) & 0x3FFF;
                    let list = encoder.set(channel, parameter, wanted);
                    sent += list.len();
                    let events = feed_all(&mut decoder, list.as_slice());
                    assert_eq!(
                        events.last(),
                        Some(&value(channel, parameter, wanted, true))
                    );
                }
            }
        }
        // Every value needed its selection, because the parameter changed
        // each time on each channel.
        assert_eq!(sent, 3 * 6 * 3 * 4);
        // Exhaustive over all 14-bit values on one parameter: after the
        // first, two messages each.
        let nrpn = Parameter::non_registered(1, 1);
        let first = encoder.set(3, nrpn, 0);
        assert_eq!(first.len(), 4);
        feed_all(&mut decoder, first.as_slice());
        for wanted in 0..=0x3FFF {
            let list = encoder.set(3, nrpn, wanted);
            assert_eq!(list.len(), 2);
            let events = feed_all(&mut decoder, list.as_slice());
            assert_eq!(
                events,
                [
                    value(3, nrpn, wanted & !0x7F, false),
                    value(3, nrpn, wanted, true)
                ]
            );
        }
    }

    #[test]
    fn the_encoder_selects_only_when_the_parameter_changes() {
        let mut encoder = RpnEncoder::new();
        let range = Parameter::PITCH_BEND_SENSITIVITY;
        assert_eq!(
            encoder.set_coarse(0, range, 48).as_slice(),
            [cc(0, 101, 0), cc(0, 100, 0), cc(0, 6, 48)]
        );
        assert_eq!(encoder.set_coarse(0, range, 24).as_slice(), [cc(0, 6, 24)]);
        assert_eq!(encoder.increment(0, range).as_slice(), [cc(0, 96, 0)]);
        assert_eq!(encoder.decrement(0, range).as_slice(), [cc(0, 97, 0)]);
        // The same number in the other family is a different parameter.
        assert_eq!(
            encoder.increment(0, Parameter::non_registered(0, 0)).len(),
            3
        );
        assert_eq!(
            encoder.deselect(0).as_slice(),
            [cc(0, 101, 127), cc(0, 100, 127)]
        );
        assert!(encoder.deselect(0).is_empty());
        assert!(
            encoder
                .selection(0, Parameter::non_registered(127, 127))
                .is_empty()
        );
        encoder.reset();
        assert_eq!(encoder.deselect(0).len(), 2);
        assert_eq!(encoder, {
            let mut expected = RpnEncoder::default();
            expected.deselect(0);
            expected
        });
    }
}
