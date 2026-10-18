//! What a plugin sends out, as CLAP events on its output note port.
//!
//! The host says which note dialects it takes (`clap_host_note_ports`);
//! a host without that extension is taken to take CLAP's own notes and
//! MIDI 1.0, which every host with note ports does.
//!
//! - **CLAP dialect.** The framework's notes, chokes and expressions
//!   become CLAP's own note events and note expressions, keeping their
//!   identifiers. MIDI 2.0 from the plugin goes out as
//!   `CLAP_EVENT_MIDI2` if the host takes MIDI 2.0. If not, its notes and
//!   per-note messages are read into notes and expressions, as the
//!   framework's input reads MIDI 2.0, and what applies to a channel goes
//!   out as `CLAP_EVENT_MIDI` by the default translation. MIDI 1.0 from the
//!   plugin goes out as `CLAP_EVENT_MIDI`, or as MIDI 2.0 if that is all
//!   the host takes besides CLAP.
//! - **No CLAP dialect.** Everything goes through an [`OutputTranslator`]:
//!   as MIDI 2.0 packets if the host takes them, else as MPE, else as MIDI
//!   1.0.
//!
//! An event later than the block is sent on its last frame. An event the
//! host's list refuses is dropped: notes cannot wait for the next block
//! the way parameter values do.

use clap_sys::events::*;
use clap_sys::ext::note_ports::*;
use manguebyte_dsp::event::{Event, EventKind, Expression, NoteRef};
use manguebyte_dsp::note_input::{Mpe, NoteInput};
use manguebyte_midi::MidiMessage;
use manguebyte_midi::mpe::Zone;
use manguebyte_midi::translate::UmpTranslator;
use manguebyte_midi::ump::{Message, Packet};
use manguebyte_plugin::output::{
    NoteIds, OutputBuffer, OutputKind, OutputProtocol, OutputTranslator,
};

use crate::wrapper::push_event;

/// How many of the plugin's identified notes are followed at once.
const IDENTIFIED_NOTES: usize = 256;

/// What a host is taken to take when it does not say.
pub(crate) const ASSUMED_DIALECTS: u32 = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;

/// The audio thread's side of the output note port.
pub(crate) struct NoteSender {
    /// What the plugin writes during a block.
    pub(crate) output: OutputBuffer,
    /// The dialects the host takes.
    dialects: u32,
    /// The channel and key of the plugin's identified notes, for its
    /// events that name a note by identifier alone.
    ids: NoteIds,
    /// Reads MIDI from the plugin into notes and expressions, for a host
    /// that takes CLAP's notes but not that MIDI.
    raise: NoteInput,
    /// For a host that takes MIDI 2.0 but not MIDI 1.0.
    up: UmpTranslator,
    /// For a host that does not take CLAP's notes.
    translator: Option<OutputTranslator>,
}

impl NoteSender {
    /// A sender for a plugin that sends with room for `capacity` events a
    /// block, or none. Allocates.
    pub(crate) fn new(capacity: usize) -> Self {
        let mut sender = Self {
            output: if capacity > 0 {
                OutputBuffer::with_capacity(capacity)
            } else {
                OutputBuffer::new()
            },
            dialects: 0,
            ids: NoteIds::with_capacity(if capacity > 0 { IDENTIFIED_NOTES } else { 0 }),
            raise: NoteInput::new(Mpe::Off),
            up: UmpTranslator::new(),
            translator: None,
        };
        sender.set_dialects(ASSUMED_DIALECTS);
        sender
    }

    /// Takes the dialects the host says it takes. Allocates.
    pub(crate) fn set_dialects(&mut self, dialects: u32) {
        self.dialects = dialects;
        self.translator = if self.takes(CLAP_NOTE_DIALECT_CLAP) {
            None
        } else if self.takes(CLAP_NOTE_DIALECT_MIDI2) {
            Some(OutputTranslator::new(OutputProtocol::Midi2))
        } else if self.takes(CLAP_NOTE_DIALECT_MIDI_MPE) {
            Zone::lower(15).map(|zone| OutputTranslator::new(OutputProtocol::Mpe(zone)))
        } else if self.takes(CLAP_NOTE_DIALECT_MIDI) {
            Some(OutputTranslator::new(OutputProtocol::Midi1))
        } else {
            None
        };
    }

    fn takes(&self, dialect: u32) -> bool {
        self.dialects & dialect != 0
    }

    /// Forgets the notes being followed, after a reset.
    pub(crate) fn reset(&mut self) {
        self.output.clear();
        self.ids.clear();
        self.raise.reset();
        self.up.reset();
        if let Some(translator) = &mut self.translator {
            translator.reset();
        }
    }

    /// Sends what the plugin wrote during a block of `frames` frames to
    /// the host's `list`. Real-time safe.
    ///
    /// # Safety
    ///
    /// `list` must be a valid output event list for the duration of the
    /// call, or null.
    pub(crate) unsafe fn send(&mut self, frames: u32, list: *const clap_output_events) {
        let last = frames.saturating_sub(1);
        let Self {
            output,
            dialects,
            ids,
            raise,
            up,
            translator,
        } = self;
        let takes = |dialect: u32| *dialects & dialect != 0;
        for event in output.iter() {
            let time = event.offset.min(last);
            // SAFETY: `list` is valid for this call, per our contract.
            let send_packet = |packet: Packet| unsafe { push_packet(list, time, packet) };
            if let Some(translator) = translator.as_mut() {
                translator.translate(&event.kind, send_packet);
                continue;
            }
            // SAFETY: as above.
            let send_raised =
                |raised: Event| unsafe { push_raised(list, time, raised.kind, takes) };
            match event.kind {
                OutputKind::NoteOn { note, velocity } => {
                    ids.start(note);
                    // SAFETY: as above.
                    unsafe { push_note(list, time, CLAP_EVENT_NOTE_ON, note, f64::from(velocity)) };
                }
                OutputKind::NoteOff { note, velocity } => {
                    let note = ids.end(note);
                    // SAFETY: as above.
                    unsafe {
                        push_note(list, time, CLAP_EVENT_NOTE_OFF, note, f64::from(velocity))
                    };
                }
                OutputKind::NoteChoke { note } => {
                    let note = ids.end(note);
                    // SAFETY: as above.
                    unsafe { push_note(list, time, CLAP_EVENT_NOTE_CHOKE, note, 0.0) };
                }
                OutputKind::NoteExpression {
                    note,
                    expression,
                    value,
                } => {
                    let note = ids.resolve(note);
                    // SAFETY: as above.
                    unsafe { push_expression(list, time, note, expression, value) };
                }
                OutputKind::Midi2 { group, message } if takes(CLAP_NOTE_DIALECT_MIDI2) => {
                    send_packet(Message::Midi2 { group, message }.to_packet());
                }
                OutputKind::Midi2 { message, .. } => raise.midi2(0, message, send_raised),
                OutputKind::Midi1 { message, .. }
                    if takes(CLAP_NOTE_DIALECT_MIDI) || takes(CLAP_NOTE_DIALECT_MIDI_MPE) =>
                {
                    // SAFETY: as above.
                    unsafe { push_midi1(list, time, message) };
                }
                OutputKind::Midi1 { group, message } if takes(CLAP_NOTE_DIALECT_MIDI2) => {
                    for &packet in up.to_midi2(Packet::from_midi1(group, message)).as_slice() {
                        send_packet(packet);
                    }
                }
                OutputKind::Midi1 { message, .. } => raise.midi1(0, message, send_raised),
            }
        }
        output.clear();
    }
}

/// The CLAP identifier of a note: the plugin's own, or -1 for none and for
/// the identifiers the framework's input makes up, which have their top
/// bit set and so cannot be CLAP's.
fn clap_note_id(note: NoteRef) -> i32 {
    note.id.and_then(|id| i32::try_from(id).ok()).unwrap_or(-1)
}

fn header<T>(time: u32, type_: clap_event_type) -> clap_event_header {
    clap_event_header {
        size: size_of::<T>() as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

/// # Safety
///
/// `list` must be a valid output event list for the duration of the call,
/// or null.
unsafe fn push_note(
    list: *const clap_output_events,
    time: u32,
    type_: clap_event_type,
    note: NoteRef,
    velocity: f64,
) {
    let event = clap_event_note {
        header: header::<clap_event_note>(time, type_),
        note_id: clap_note_id(note),
        port_index: 0,
        channel: i16::from(note.channel & 0x0F),
        key: i16::from(note.key & 0x7F),
        velocity: velocity.clamp(0.0, 1.0),
    };
    // SAFETY: per this function's contract; the event is a whole CLAP note
    // event whose header gives its size.
    unsafe { push_event(list, &event) };
}

/// # Safety
///
/// `list` must be a valid output event list for the duration of the call,
/// or null.
unsafe fn push_expression(
    list: *const clap_output_events,
    time: u32,
    note: NoteRef,
    expression: Expression,
    value: f64,
) {
    // CLAP's pan runs from 0 (left) to 1 (right), the framework's from -1;
    // the other expressions share their units.
    let (expression_id, value) = match expression {
        Expression::Volume => (CLAP_NOTE_EXPRESSION_VOLUME, value),
        Expression::Pan => (CLAP_NOTE_EXPRESSION_PAN, (value + 1.0) / 2.0),
        Expression::Tuning => (CLAP_NOTE_EXPRESSION_TUNING, value),
        Expression::Vibrato => (CLAP_NOTE_EXPRESSION_VIBRATO, value),
        Expression::Brightness => (CLAP_NOTE_EXPRESSION_BRIGHTNESS, value),
        Expression::Pressure => (CLAP_NOTE_EXPRESSION_PRESSURE, value),
    };
    let event = clap_event_note_expression {
        header: header::<clap_event_note_expression>(time, CLAP_EVENT_NOTE_EXPRESSION),
        expression_id,
        note_id: clap_note_id(note),
        port_index: 0,
        channel: i16::from(note.channel & 0x0F),
        key: i16::from(note.key & 0x7F),
        value,
    };
    // SAFETY: per this function's contract.
    unsafe { push_event(list, &event) };
}

/// # Safety
///
/// `list` must be a valid output event list for the duration of the call,
/// or null.
unsafe fn push_midi1(list: *const clap_output_events, time: u32, message: MidiMessage) {
    let raw = message.to_bytes();
    let mut data = [0u8; 3];
    data[..raw.as_slice().len()].copy_from_slice(raw.as_slice());
    let event = clap_event_midi {
        header: header::<clap_event_midi>(time, CLAP_EVENT_MIDI),
        port_index: 0,
        data,
    };
    // SAFETY: per this function's contract.
    unsafe { push_event(list, &event) };
}

/// Sends a packet as the CLAP event of its kind: a MIDI 1.0 packet as
/// `CLAP_EVENT_MIDI`, anything else as `CLAP_EVENT_MIDI2`.
///
/// # Safety
///
/// `list` must be a valid output event list for the duration of the call,
/// or null.
unsafe fn push_packet(list: *const clap_output_events, time: u32, packet: Packet) {
    if let Some(message) = packet.to_midi1() {
        // SAFETY: per this function's contract.
        unsafe { push_midi1(list, time, message) };
        return;
    }
    let mut data = [0u32; 4];
    let _ = packet.write(&mut data);
    let event = clap_event_midi2 {
        header: header::<clap_event_midi2>(time, CLAP_EVENT_MIDI2),
        port_index: 0,
        data,
    };
    // SAFETY: per this function's contract.
    unsafe { push_event(list, &event) };
}

/// Sends what the framework's input made of the plugin's MIDI: notes and
/// expressions as CLAP's own, and channel messages as MIDI 1.0 if the host
/// takes it.
///
/// # Safety
///
/// `list` must be a valid output event list for the duration of the call,
/// or null.
unsafe fn push_raised(
    list: *const clap_output_events,
    time: u32,
    kind: EventKind,
    takes: impl Fn(u32) -> bool,
) {
    // SAFETY: per this function's contract, for each call below.
    unsafe {
        match kind {
            EventKind::NoteOn { note, velocity } => {
                push_note(list, time, CLAP_EVENT_NOTE_ON, note, f64::from(velocity));
            }
            EventKind::NoteOff { note, velocity } => {
                push_note(list, time, CLAP_EVENT_NOTE_OFF, note, f64::from(velocity));
            }
            EventKind::NoteChoke { note } => {
                push_note(list, time, CLAP_EVENT_NOTE_CHOKE, note, 0.0)
            }
            EventKind::NoteExpression {
                note,
                expression,
                value,
            } => push_expression(list, time, note, expression, value),
            EventKind::Midi(message)
                if takes(CLAP_NOTE_DIALECT_MIDI) || takes(CLAP_NOTE_DIALECT_MIDI_MPE) =>
            {
                push_midi1(list, time, message);
            }
            _ => {}
        }
    }
}
