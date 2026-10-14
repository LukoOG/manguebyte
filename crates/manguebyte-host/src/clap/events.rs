//! Turning the framework's events into CLAP's and back.
//!
//! Both models say the same things: a note with an identity, an expression
//! that shapes one note, a parameter value, a modulation offset, a MIDI
//! message, each on a frame. The conversion is therefore a relabelling,
//! and the few places where the two disagree on a scale are listed with
//! the function that handles them.
//!
//! Everything here is free of allocation and is used on the audio thread.

use std::ffi::c_void;

use clap_sys::events::*;
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use manguebyte_core::ParamId;
use manguebyte_dsp::Transport;
use manguebyte_dsp::event::{Event, EventKind, Expression, NoteRef};
use manguebyte_midi::MidiMessage;

/// Something a plugin reported while processing, on the frame it happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputEvent {
    /// Frame within the block that was processed, counted from its start.
    pub offset: u32,
    /// What happened.
    pub kind: OutputKind,
}

/// What a plugin can report while processing.
///
/// More kinds may be added as the host learns to pass on more of what CLAP
/// defines, so a `match` on this needs a catch-all arm.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum OutputKind {
    /// Something the framework's own event type can say: a note starting or
    /// stopping, a note expression, a parameter value the plugin changed by
    /// itself, a MIDI message. It can be pushed into an
    /// [`EventBuffer`](manguebyte_dsp::event::EventBuffer) as it is, for
    /// example to feed a note effect's output to an instrument.
    ///
    /// A parameter value is in the plugin's own scale, and is for a
    /// parameter that was in
    /// [`ClapPlugin::params`](super::ClapPlugin::params) when the plugin
    /// was activated. The host holds it to the range between the
    /// [`min`](super::ParamInfo::min) and [`max`](super::ParamInfo::max)
    /// declared there, whatever the plugin sent. A note expression is a
    /// finite number within the range CLAP gives that expression.
    Event(EventKind),
    /// A note has finished sounding, tail included. A host that gave the
    /// note an id may reuse the id from here on.
    NoteEnd {
        /// The note.
        note: NoteRef,
    },
    /// Someone took hold of a control in the plugin's own interface. Until
    /// the matching [`GestureEnd`](Self::GestureEnd), values the plugin
    /// reports for this parameter are one continuous edit. The parameter
    /// is one the plugin listed, as for a value.
    GestureBegin {
        /// The parameter.
        param: ParamId,
    },
    /// The control was let go.
    GestureEnd {
        /// The parameter.
        param: ParamId,
    },
}

/// One event on its way to the plugin, stored as its concrete CLAP struct.
#[derive(Clone, Copy)]
pub(crate) enum InEvent {
    Note(clap_event_note),
    Expression(clap_event_note_expression),
    Value(clap_event_param_value),
    Modulation(clap_event_param_mod),
    Midi(clap_event_midi),
}

impl InEvent {
    /// A pointer to the whole event, viewed as its header, which is how
    /// CLAP hands events over. It is derived from the whole struct because
    /// the plugin reads past the header through it.
    pub(crate) fn header(&self) -> *const clap_event_header {
        match self {
            Self::Note(event) => std::ptr::from_ref(event).cast(),
            Self::Expression(event) => std::ptr::from_ref(event).cast(),
            Self::Value(event) => std::ptr::from_ref(event).cast(),
            Self::Modulation(event) => std::ptr::from_ref(event).cast(),
            Self::Midi(event) => std::ptr::from_ref(event).cast(),
        }
    }
}

/// What the audio thread knows about one parameter.
#[derive(Clone, Copy)]
pub(crate) struct ParamSlot {
    pub(crate) id: u32,
    /// The pointer the plugin asked to be given back with every event for
    /// this parameter, so that it need not look the parameter up.
    pub(crate) cookie: *mut c_void,
    pub(crate) min: f64,
    pub(crate) max: f64,
    /// Where the parameter is in the list the plugin gave.
    pub(crate) index: u32,
}

impl ParamSlot {
    /// `value` held to the range the plugin declared for this parameter.
    /// Something that is not a number is returned as it is; callers turn
    /// those away themselves.
    ///
    /// Written out and not `f64::clamp`, which panics when its bounds are
    /// out of order or not numbers. A slot is only ever made with a sound
    /// range, but this runs on the audio thread, where nothing may panic
    /// whatever a table holds.
    pub(crate) fn hold(&self, value: f64) -> f64 {
        if value < self.min {
            self.min
        } else if value > self.max {
            self.max
        } else {
            value
        }
    }
}

/// Finds a parameter in a table sorted by id. Does not allocate.
pub(crate) fn find_param(params: &[ParamSlot], id: u32) -> Option<&ParamSlot> {
    params
        .binary_search_by_key(&id, |slot| slot.id)
        .ok()
        .map(|index| &params[index])
}

/// Which kinds of note event the plugin's first note input accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct NoteRoute {
    /// CLAP's own note and note expression events.
    pub(crate) clap: bool,
    /// MIDI 1.0 messages, whether the port reads them as plain MIDI or as
    /// MPE: on the wire the two are the same events.
    pub(crate) midi: bool,
}

/// The range CLAP gives a note expression, as `(lowest, highest)`, or
/// `None` for an expression this host does not convert.
///
/// From the CLAP specification (`events.h`, "note expression"): volume is a
/// gain factor of up to 4, which is +12 dB; tuning is in semitones, 120
/// either way; pan, vibrato, brightness and pressure run from 0 to 1.
fn expression_range(id: clap_note_expression) -> Option<(f64, f64)> {
    match id {
        CLAP_NOTE_EXPRESSION_VOLUME => Some((0.0, 4.0)),
        CLAP_NOTE_EXPRESSION_TUNING => Some((-120.0, 120.0)),
        CLAP_NOTE_EXPRESSION_PAN
        | CLAP_NOTE_EXPRESSION_VIBRATO
        | CLAP_NOTE_EXPRESSION_BRIGHTNESS
        | CLAP_NOTE_EXPRESSION_PRESSURE => Some((0.0, 1.0)),
        _ => None,
    }
}

/// A note expression value, in CLAP's scale, held to the range CLAP gives
/// that expression. `None` if the value is not a finite number or the
/// expression is not one this host converts.
fn expression_value(id: clap_note_expression, value: f64) -> Option<f64> {
    let (lowest, highest) = expression_range(id)?;
    value.is_finite().then(|| value.clamp(lowest, highest))
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

/// A note as CLAP addresses it: `(note_id, channel, key)`, where -1 would
/// be a wildcard. An id too large for CLAP's signed field is left out, and
/// the note is then addressed by channel and key.
fn address(note: NoteRef) -> (i32, i16, i16) {
    let id = note.id.and_then(|id| i32::try_from(id).ok()).unwrap_or(-1);
    (
        id,
        i16::from(note.channel & 0x0F),
        i16::from(note.key & 0x7F),
    )
}

/// A level in `0..=1` as a 7-bit MIDI value.
fn seven_bits(level: f64) -> u8 {
    (unit(level) * 127.0).round() as u8
}

/// `value` held to `0..=1`, with anything that is not a number as zero.
fn unit(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn midi(time: u32, data: [u8; 3]) -> InEvent {
    InEvent::Midi(clap_event_midi {
        header: header::<clap_event_midi>(time, CLAP_EVENT_MIDI),
        port_index: 0,
        data,
    })
}

fn note(type_: clap_event_type, note: NoteRef, velocity: f32, time: u32) -> InEvent {
    let (note_id, channel, key) = address(note);
    InEvent::Note(clap_event_note {
        header: header::<clap_event_note>(time, type_),
        note_id,
        port_index: 0,
        channel,
        key,
        velocity: unit(f64::from(velocity)),
    })
}

/// Converts one event for the plugin, or returns `None` if the plugin has
/// no way to receive it: a note for a plugin without a note input, a value
/// for a parameter it does not have, a number that is not one.
///
/// Scales:
///
/// - A parameter value or a modulation amount is in the plugin's own scale
///   and is passed through; a value is held to the range the plugin
///   declared.
/// - Pan is `-1..=1` in the framework and `0..=1` in CLAP.
/// - A note expression is held to the range CLAP gives it: a volume of 0 to
///   4, a tuning of 120 semitones either way, 0 to 1 for the rest. One that
///   is not a finite number is not sent.
/// - A plugin that takes only MIDI gets notes as note-on and note-off
///   messages, a choke as a note-off, and pressure as polyphonic
///   aftertouch. Other expressions have no MIDI form and are not sent.
pub(crate) fn to_clap(
    kind: &EventKind,
    time: u32,
    params: &[ParamSlot],
    route: NoteRoute,
) -> Option<InEvent> {
    match *kind {
        EventKind::NoteOn {
            note: which,
            velocity,
        } => {
            if route.clap {
                Some(note(CLAP_EVENT_NOTE_ON, which, velocity, time))
            } else if route.midi {
                // Velocity zero would mean note-off, so the quietest
                // note-on is one.
                let velocity = seven_bits(f64::from(velocity)).max(1);
                let status = 0x90 | (which.channel & 0x0F);
                Some(midi(time, [status, which.key & 0x7F, velocity]))
            } else {
                None
            }
        }
        EventKind::NoteOff {
            note: which,
            velocity,
        } => {
            if route.clap {
                Some(note(CLAP_EVENT_NOTE_OFF, which, velocity, time))
            } else if route.midi {
                let status = 0x80 | (which.channel & 0x0F);
                let velocity = seven_bits(f64::from(velocity));
                Some(midi(time, [status, which.key & 0x7F, velocity]))
            } else {
                None
            }
        }
        EventKind::NoteChoke { note: which } => {
            if route.clap {
                Some(note(CLAP_EVENT_NOTE_CHOKE, which, 0.0, time))
            } else if route.midi {
                let status = 0x80 | (which.channel & 0x0F);
                Some(midi(time, [status, which.key & 0x7F, 0]))
            } else {
                None
            }
        }
        EventKind::NoteExpression {
            note: which,
            expression,
            value,
        } => {
            if !value.is_finite() {
                return None;
            }
            if route.clap {
                let (expression_id, value) = match expression {
                    Expression::Volume => (CLAP_NOTE_EXPRESSION_VOLUME, value),
                    Expression::Pan => (CLAP_NOTE_EXPRESSION_PAN, 0.5 + 0.5 * value),
                    Expression::Tuning => (CLAP_NOTE_EXPRESSION_TUNING, value),
                    Expression::Vibrato => (CLAP_NOTE_EXPRESSION_VIBRATO, value),
                    Expression::Brightness => (CLAP_NOTE_EXPRESSION_BRIGHTNESS, value),
                    Expression::Pressure => (CLAP_NOTE_EXPRESSION_PRESSURE, value),
                };
                let value = expression_value(expression_id, value)?;
                let (note_id, channel, key) = address(which);
                Some(InEvent::Expression(clap_event_note_expression {
                    header: header::<clap_event_note_expression>(time, CLAP_EVENT_NOTE_EXPRESSION),
                    expression_id,
                    note_id,
                    port_index: 0,
                    channel,
                    key,
                    value,
                }))
            } else if route.midi && expression == Expression::Pressure {
                let status = 0xA0 | (which.channel & 0x0F);
                Some(midi(time, [status, which.key & 0x7F, seven_bits(value)]))
            } else {
                None
            }
        }
        EventKind::ParamValue { param, value } => {
            let slot = find_param(params, param.0)?;
            if value.is_nan() {
                return None;
            }
            Some(InEvent::Value(clap_event_param_value {
                header: header::<clap_event_param_value>(time, CLAP_EVENT_PARAM_VALUE),
                param_id: slot.id,
                cookie: slot.cookie,
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: slot.hold(value),
            }))
        }
        EventKind::ParamModulation {
            param,
            amount,
            note: which,
        } => {
            let slot = find_param(params, param.0)?;
            if !amount.is_finite() {
                return None;
            }
            let (note_id, port_index, channel, key) = match which {
                Some(which) => {
                    let (note_id, channel, key) = address(which);
                    (note_id, 0, channel, key)
                }
                None => (-1, -1, -1, -1),
            };
            Some(InEvent::Modulation(clap_event_param_mod {
                header: header::<clap_event_param_mod>(time, CLAP_EVENT_PARAM_MOD),
                param_id: slot.id,
                cookie: slot.cookie,
                note_id,
                port_index,
                channel,
                key,
                amount,
            }))
        }
        EventKind::Midi(message) => {
            if !route.midi {
                return None;
            }
            let raw = message.to_bytes();
            let mut data = [0u8; 3];
            for (slot, &byte) in data.iter_mut().zip(raw.as_slice()) {
                *slot = byte;
            }
            Some(midi(time, data))
        }
    }
}

/// The note a plugin's event is about, or `None` if it addresses nothing in
/// particular. In CLAP -1 in a field means "any".
fn note_ref(note_id: i32, channel: i16, key: i16) -> Option<NoteRef> {
    let id = u32::try_from(note_id).ok();
    let channel = u8::try_from(channel).unwrap_or(0) & 0x0F;
    match (u8::try_from(key), id) {
        (Ok(key), Some(id)) => Some(NoteRef::with_id(channel, key & 0x7F, id)),
        (Ok(key), None) => Some(NoteRef::new(channel, key & 0x7F)),
        // Any key, but one particular id: the id alone decides.
        (Err(_), Some(id)) => Some(NoteRef::with_id(channel, 0, id)),
        (Err(_), None) => None,
    }
}

/// Reads an event as its concrete type, if the event says it is large
/// enough to be one and sits where such a type can sit.
///
/// # Safety
///
/// `header` must point to an aligned event header, readable for the number
/// of bytes its own `size` field gives.
unsafe fn read<'a, T>(header: *const clap_event_header) -> Option<&'a T> {
    let event = header.cast::<T>();
    // SAFETY: the header itself is readable per the contract.
    let size = unsafe { (*header).size } as usize;
    if size < size_of::<T>() || !event.is_aligned() {
        return None;
    }
    // SAFETY: `size` says the event is at least as large as `T`, the
    // contract makes that many bytes readable, and the alignment was just
    // checked.
    Some(unsafe { &*event })
}

/// Converts one event a plugin pushed, as `(frame, what)`.
///
/// Returns `None` for an event this host has no use for: another event
/// space, a kind without an equivalent (system exclusive, MIDI 2.0, the
/// `expression` note expression), a wildcard where a note was needed, or a
/// value that is not a finite number.
///
/// A note expression is held to the range CLAP gives it. A parameter value
/// is passed on as the plugin sent it, for [`held_to_declared`] to check
/// against the plugin's own list, which this function does not have.
///
/// # Safety
///
/// `header` must be null, or point to an event that is readable for the
/// number of bytes its own `size` field gives.
pub(crate) unsafe fn from_clap(header: *const clap_event_header) -> Option<(u32, OutputKind)> {
    if header.is_null() || !header.is_aligned() {
        return None;
    }
    // SAFETY: not null, aligned, and readable per the contract.
    let head = unsafe { &*header };
    if head.space_id != CLAP_CORE_EVENT_SPACE_ID
        || (head.size as usize) < size_of::<clap_event_header>()
    {
        return None;
    }
    let kind = match head.type_ {
        CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE | CLAP_EVENT_NOTE_END => {
            // SAFETY: these four types all carry a `clap_event_note`.
            let event = unsafe { read::<clap_event_note>(header) }?;
            let note = note_ref(event.note_id, event.channel, event.key)?;
            let velocity = unit(event.velocity) as f32;
            match head.type_ {
                CLAP_EVENT_NOTE_ON => OutputKind::Event(EventKind::NoteOn { note, velocity }),
                CLAP_EVENT_NOTE_OFF => OutputKind::Event(EventKind::NoteOff { note, velocity }),
                CLAP_EVENT_NOTE_CHOKE => OutputKind::Event(EventKind::NoteChoke { note }),
                _ => OutputKind::NoteEnd { note },
            }
        }
        CLAP_EVENT_NOTE_EXPRESSION => {
            // SAFETY: this type carries a `clap_event_note_expression`.
            let event = unsafe { read::<clap_event_note_expression>(header) }?;
            let note = note_ref(event.note_id, event.channel, event.key)?;
            // Not a number, infinite, or an expression with no equivalent:
            // not passed on. Anything else is held to CLAP's range for it.
            let value = expression_value(event.expression_id, event.value)?;
            let (expression, value) = match event.expression_id {
                CLAP_NOTE_EXPRESSION_VOLUME => (Expression::Volume, value),
                CLAP_NOTE_EXPRESSION_PAN => (Expression::Pan, 2.0 * value - 1.0),
                CLAP_NOTE_EXPRESSION_TUNING => (Expression::Tuning, value),
                CLAP_NOTE_EXPRESSION_VIBRATO => (Expression::Vibrato, value),
                CLAP_NOTE_EXPRESSION_BRIGHTNESS => (Expression::Brightness, value),
                CLAP_NOTE_EXPRESSION_PRESSURE => (Expression::Pressure, value),
                _ => return None,
            };
            OutputKind::Event(EventKind::NoteExpression {
                note,
                expression,
                value,
            })
        }
        CLAP_EVENT_PARAM_VALUE => {
            // SAFETY: this type carries a `clap_event_param_value`.
            let event = unsafe { read::<clap_event_param_value>(header) }?;
            if !event.value.is_finite() {
                return None;
            }
            OutputKind::Event(EventKind::ParamValue {
                param: ParamId(event.param_id),
                value: event.value,
            })
        }
        CLAP_EVENT_PARAM_GESTURE_BEGIN | CLAP_EVENT_PARAM_GESTURE_END => {
            // SAFETY: these two types carry a `clap_event_param_gesture`.
            let event = unsafe { read::<clap_event_param_gesture>(header) }?;
            let param = ParamId(event.param_id);
            if head.type_ == CLAP_EVENT_PARAM_GESTURE_BEGIN {
                OutputKind::GestureBegin { param }
            } else {
                OutputKind::GestureEnd { param }
            }
        }
        CLAP_EVENT_MIDI => {
            // SAFETY: this type carries a `clap_event_midi`.
            let event = unsafe { read::<clap_event_midi>(header) }?;
            let message = MidiMessage::from_bytes(&event.data)?;
            OutputKind::Event(Event::from_midi(head.time, message).kind)
        }
        _ => return None,
    };
    Some((head.time, kind))
}

/// Holds what a plugin reported about one of its parameters against the
/// list the plugin itself gave.
///
/// A value is held to the range declared for that parameter. A value or a
/// gesture for a parameter that is not in the list comes back as `None`:
/// the application could not look such a parameter up, so there is nothing
/// it could do with the report. Every other kind of event is returned as it
/// is. Does not allocate.
pub(crate) fn held_to_declared(kind: OutputKind, params: &[ParamSlot]) -> Option<OutputKind> {
    match kind {
        OutputKind::Event(EventKind::ParamValue { param, value }) => {
            let slot = find_param(params, param.0)?;
            Some(OutputKind::Event(EventKind::ParamValue {
                param,
                value: slot.hold(value),
            }))
        }
        OutputKind::GestureBegin { param } | OutputKind::GestureEnd { param } => {
            find_param(params, param.0)?;
            Some(kind)
        }
        other => Some(other),
    }
}

/// A position in beats or seconds as CLAP's fixed-point number.
fn fixed(value: f64, factor: i64) -> i64 {
    // A float-to-integer cast saturates, and turns "not a number" into 0.
    (value * factor as f64).round() as i64
}

/// The transport as CLAP describes it.
///
/// A field the framework's transport does not know is left at zero with
/// its flag clear, so the plugin can tell "not known" from "zero". The
/// position in seconds is worked out from the position in frames. The
/// loop's position in seconds is worked out from the tempo of this block,
/// which is exact only if the tempo does not change inside the loop.
pub(crate) fn transport_event(transport: &Transport, sample_rate: f64) -> clap_event_transport {
    let mut flags = 0;
    if transport.playing {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }
    if transport.recording {
        flags |= CLAP_TRANSPORT_IS_RECORDING;
    }

    let tempo = transport.tempo_bpm.filter(|tempo| tempo.is_finite());
    if tempo.is_some() {
        flags |= CLAP_TRANSPORT_HAS_TEMPO;
    }
    let (tsig_num, tsig_denom) = match transport.time_signature {
        Some(signature) => {
            flags |= CLAP_TRANSPORT_HAS_TIME_SIGNATURE;
            signature
        }
        None => (0, 0),
    };

    let (mut song_pos_beats, mut bar_start, mut bar_number) = (0, 0, 0);
    let (mut loop_start_beats, mut loop_end_beats) = (0, 0);
    let (mut loop_start_seconds, mut loop_end_seconds) = (0, 0);
    if let Some(beats) = transport.position_beats {
        flags |= CLAP_TRANSPORT_HAS_BEATS_TIMELINE;
        song_pos_beats = fixed(beats, CLAP_BEATTIME_FACTOR);
        if let Some(start) = transport.bar_start_beats {
            bar_start = fixed(start, CLAP_BEATTIME_FACTOR);
            // Quarter notes in a bar: beats per bar times the length of a
            // beat, which is 4 over the lower figure.
            let bar = f64::from(tsig_num) * 4.0 / f64::from(tsig_denom.max(1));
            if bar > 0.0 {
                bar_number = (start / bar).round() as i32;
            }
        }
        if let Some((start, end)) = transport.loop_beats {
            flags |= CLAP_TRANSPORT_IS_LOOP_ACTIVE;
            loop_start_beats = fixed(start, CLAP_BEATTIME_FACTOR);
            loop_end_beats = fixed(end, CLAP_BEATTIME_FACTOR);
            if let Some(tempo) = tempo.filter(|&tempo| tempo > 0.0) {
                loop_start_seconds = fixed(start * 60.0 / tempo, CLAP_SECTIME_FACTOR);
                loop_end_seconds = fixed(end * 60.0 / tempo, CLAP_SECTIME_FACTOR);
            }
        }
    }

    let mut song_pos_seconds = 0;
    if let Some(frames) = transport.position_samples {
        if sample_rate > 0.0 {
            flags |= CLAP_TRANSPORT_HAS_SECONDS_TIMELINE;
            song_pos_seconds = fixed(frames as f64 / sample_rate, CLAP_SECTIME_FACTOR);
        }
    }

    clap_event_transport {
        header: header::<clap_event_transport>(0, CLAP_EVENT_TRANSPORT),
        flags,
        song_pos_beats,
        song_pos_seconds,
        tempo: tempo.unwrap_or(0.0),
        tempo_inc: 0.0,
        loop_start_beats,
        loop_end_beats,
        loop_start_seconds,
        loop_end_seconds,
        bar_start,
        bar_number,
        tsig_num,
        tsig_denom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUTOFF: ParamId = ParamId::from_name("cutoff");
    const BOTH: NoteRoute = NoteRoute {
        clap: true,
        midi: true,
    };
    const MIDI_ONLY: NoteRoute = NoteRoute {
        clap: false,
        midi: true,
    };
    const NEITHER: NoteRoute = NoteRoute {
        clap: false,
        midi: false,
    };

    fn slots() -> Vec<ParamSlot> {
        let mut slots = vec![
            ParamSlot {
                id: CUTOFF.0,
                cookie: std::ptr::null_mut(),
                min: 0.0,
                max: 1.0,
                index: 0,
            },
            ParamSlot {
                id: 7,
                cookie: std::ptr::null_mut(),
                min: -10.0,
                max: 10.0,
                index: 1,
            },
        ];
        slots.sort_by_key(|slot| slot.id);
        slots
    }

    /// Sends an event to the plugin and reads it back as if the plugin had
    /// echoed it, which exercises both directions at once.
    fn echo(kind: EventKind, route: NoteRoute) -> Option<(u32, OutputKind)> {
        let event = to_clap(&kind, 33, &slots(), route)?;
        // SAFETY: the header points into `event`, which is a whole event.
        unsafe { from_clap(event.header()) }
    }

    #[test]
    fn every_kind_survives_the_trip_there_and_back() {
        let note = NoteRef::with_id(3, 61, 900);
        let plain = NoteRef::new(15, 127);
        let bend = MidiMessage::PitchBend {
            channel: 2,
            value: -1234,
        };
        for kind in [
            EventKind::NoteOn {
                note,
                velocity: 0.625,
            },
            EventKind::NoteOn {
                note: plain,
                velocity: 1.0,
            },
            EventKind::NoteOff {
                note,
                velocity: 0.25,
            },
            EventKind::NoteChoke { note: plain },
            EventKind::NoteExpression {
                note,
                expression: Expression::Tuning,
                value: -0.37,
            },
            EventKind::NoteExpression {
                note,
                expression: Expression::Pan,
                value: -1.0,
            },
            EventKind::NoteExpression {
                note: plain,
                expression: Expression::Pan,
                value: 0.5,
            },
            EventKind::ParamValue {
                param: CUTOFF,
                value: 0.123_456_789,
            },
            EventKind::Midi(bend),
            EventKind::Midi(MidiMessage::ControlChange {
                channel: 9,
                controller: 74,
                value: 100,
            }),
            EventKind::Midi(MidiMessage::ProgramChange {
                channel: 1,
                program: 5,
            }),
        ] {
            assert_eq!(
                echo(kind, BOTH),
                Some((33, OutputKind::Event(kind))),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn events_are_laid_out_as_clap_says() {
        let note = NoteRef::with_id(3, 61, 900);
        let Some(InEvent::Note(event)) = to_clap(
            &EventKind::NoteOn {
                note,
                velocity: 0.5,
            },
            12,
            &[],
            BOTH,
        ) else {
            panic!("a note-on did not become a CLAP note");
        };
        assert_eq!(event.header.size as usize, size_of::<clap_event_note>());
        assert_eq!(event.header.time, 12);
        assert_eq!(event.header.space_id, CLAP_CORE_EVENT_SPACE_ID);
        assert_eq!(event.header.type_, CLAP_EVENT_NOTE_ON);
        assert_eq!(
            (event.note_id, event.port_index, event.channel, event.key),
            (900, 0, 3, 61)
        );
        assert_eq!(event.velocity, 0.5);

        // No id: CLAP's wildcard, so that channel and key decide.
        let Some(InEvent::Note(event)) = to_clap(
            &EventKind::NoteChoke {
                note: NoteRef::new(0, 60),
            },
            0,
            &[],
            BOTH,
        ) else {
            panic!("a choke did not become a CLAP note");
        };
        assert_eq!(
            (event.header.type_, event.note_id),
            (CLAP_EVENT_NOTE_CHOKE, -1)
        );

        // An id CLAP cannot hold is dropped; the note is still addressed.
        let huge = NoteRef::with_id(0, 60, u32::MAX);
        let Some(InEvent::Note(event)) = to_clap(
            &EventKind::NoteOff {
                note: huge,
                velocity: 0.0,
            },
            0,
            &[],
            BOTH,
        ) else {
            panic!("a note-off did not become a CLAP note");
        };
        assert_eq!((event.note_id, event.key), (-1, 60));
    }

    #[test]
    fn parameters_are_held_to_their_range_and_unknown_ones_are_not_sent() {
        let slots = slots();
        let value = |param, value| match to_clap(
            &EventKind::ParamValue { param, value },
            0,
            &slots,
            BOTH,
        ) {
            Some(InEvent::Value(event)) => Some((event.param_id, event.value, event.note_id)),
            Some(_) => panic!("a value became another kind of event"),
            None => None,
        };
        assert_eq!(value(CUTOFF, 0.5), Some((CUTOFF.0, 0.5, -1)));
        assert_eq!(value(CUTOFF, 7.0), Some((CUTOFF.0, 1.0, -1)));
        assert_eq!(value(ParamId(7), -99.0), Some((7, -10.0, -1)));
        assert_eq!(value(ParamId(7), f64::NEG_INFINITY), Some((7, -10.0, -1)));
        assert_eq!(value(ParamId(8), 0.5), None);
        assert_eq!(value(CUTOFF, f64::NAN), None);

        let modulation = |amount, note| match to_clap(
            &EventKind::ParamModulation {
                param: CUTOFF,
                amount,
                note,
            },
            5,
            &slots,
            BOTH,
        ) {
            Some(InEvent::Modulation(event)) => Some((
                event.amount,
                event.note_id,
                event.port_index,
                event.channel,
                event.key,
            )),
            Some(_) => panic!("a modulation became another kind of event"),
            None => None,
        };
        // For every note: all four address fields are wildcards.
        assert_eq!(modulation(0.25, None), Some((0.25, -1, -1, -1, -1)));
        assert_eq!(
            modulation(-0.5, Some(NoteRef::with_id(1, 72, 4))),
            Some((-0.5, 4, 0, 1, 72))
        );
        assert_eq!(modulation(f64::INFINITY, None), None);
        assert_eq!(modulation(f64::NAN, None), None);
    }

    #[test]
    fn a_plugin_that_only_speaks_midi_gets_midi() {
        let note = NoteRef::with_id(4, 60, 1);
        let bytes = |kind| match to_clap(&kind, 9, &[], MIDI_ONLY) {
            Some(InEvent::Midi(event)) => Some(event.data),
            Some(_) => panic!("a MIDI-only plugin was sent a CLAP note"),
            None => None,
        };
        assert_eq!(
            bytes(EventKind::NoteOn {
                note,
                velocity: 1.0
            }),
            Some([0x94, 60, 127])
        );
        // A velocity that rounds to zero must not turn into a note-off.
        assert_eq!(
            bytes(EventKind::NoteOn {
                note,
                velocity: 0.001
            }),
            Some([0x94, 60, 1])
        );
        assert_eq!(
            bytes(EventKind::NoteOff {
                note,
                velocity: 0.5
            }),
            Some([0x84, 60, 64])
        );
        assert_eq!(bytes(EventKind::NoteChoke { note }), Some([0x84, 60, 0]));
        assert_eq!(
            bytes(EventKind::NoteExpression {
                note,
                expression: Expression::Pressure,
                value: 1.0
            }),
            Some([0xA4, 60, 127])
        );
        assert_eq!(
            bytes(EventKind::NoteExpression {
                note,
                expression: Expression::Tuning,
                value: 1.0
            }),
            None
        );
    }

    #[test]
    fn a_plugin_without_a_note_input_gets_no_notes() {
        let note = NoteRef::new(0, 60);
        for kind in [
            EventKind::NoteOn {
                note,
                velocity: 1.0,
            },
            EventKind::NoteOff {
                note,
                velocity: 0.0,
            },
            EventKind::NoteChoke { note },
            EventKind::NoteExpression {
                note,
                expression: Expression::Pressure,
                value: 0.5,
            },
            EventKind::Midi(MidiMessage::ChannelPressure {
                channel: 0,
                pressure: 1,
            }),
        ] {
            assert!(to_clap(&kind, 0, &slots(), NEITHER).is_none(), "{kind:?}");
        }
        // Parameters do not depend on note ports.
        let value = EventKind::ParamValue {
            param: CUTOFF,
            value: 0.5,
        };
        assert!(to_clap(&value, 0, &slots(), NEITHER).is_some());
        // CLAP notes but no MIDI: raw MIDI has nowhere to go.
        let clap_only = NoteRoute {
            clap: true,
            midi: false,
        };
        let controller = EventKind::Midi(MidiMessage::ControlChange {
            channel: 0,
            controller: 1,
            value: 2,
        });
        assert!(to_clap(&controller, 0, &[], clap_only).is_none());
    }

    #[test]
    fn what_a_plugin_reports_is_read_with_care() {
        let gesture = |type_| clap_event_param_gesture {
            header: header::<clap_event_param_gesture>(4, type_),
            param_id: CUTOFF.0,
        };
        let begin = gesture(CLAP_EVENT_PARAM_GESTURE_BEGIN);
        let end = gesture(CLAP_EVENT_PARAM_GESTURE_END);
        let mut finished = clap_event_note {
            header: header::<clap_event_note>(20, CLAP_EVENT_NOTE_END),
            note_id: 5,
            port_index: 0,
            channel: 2,
            key: -1,
            velocity: 0.0,
        };
        // SAFETY: every pointer below is to a whole, live event.
        unsafe {
            assert_eq!(
                from_clap((&raw const begin).cast()),
                Some((4, OutputKind::GestureBegin { param: CUTOFF }))
            );
            assert_eq!(
                from_clap((&raw const end).cast()),
                Some((4, OutputKind::GestureEnd { param: CUTOFF }))
            );
            // A wildcard key with an id: the id identifies the note.
            assert_eq!(
                from_clap((&raw const finished).cast()),
                Some((
                    20,
                    OutputKind::NoteEnd {
                        note: NoteRef::with_id(2, 0, 5)
                    }
                ))
            );
            // Wildcards all round address nothing.
            finished.note_id = -1;
            assert_eq!(from_clap((&raw const finished).cast()), None);

            assert_eq!(from_clap(std::ptr::null()), None);

            // An event that claims to be smaller than its type is not read
            // as that type.
            let mut short = finished;
            short.note_id = 5;
            short.header.size = size_of::<clap_event_header>() as u32;
            assert_eq!(from_clap((&raw const short).cast()), None);
            short.header.size = 3;
            assert_eq!(from_clap((&raw const short).cast()), None);

            // Another vendor's event space, and kinds with no equivalent.
            let mut foreign = begin;
            foreign.header.space_id = 77;
            assert_eq!(from_clap((&raw const foreign).cast()), None);
            let mut unknown = begin;
            unknown.header.type_ = 9_999;
            assert_eq!(from_clap((&raw const unknown).cast()), None);

            // Values that are not numbers are not passed on.
            let mut value = clap_event_param_value {
                header: header::<clap_event_param_value>(0, CLAP_EVENT_PARAM_VALUE),
                param_id: 1,
                cookie: std::ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: f64::NAN,
            };
            assert_eq!(from_clap((&raw const value).cast()), None);
            value.value = f64::INFINITY;
            assert_eq!(from_clap((&raw const value).cast()), None);

            // MIDI that is not a channel message is not passed on either.
            let system = clap_event_midi {
                header: header::<clap_event_midi>(0, CLAP_EVENT_MIDI),
                port_index: 0,
                data: [0xF8, 0, 0],
            };
            assert_eq!(from_clap((&raw const system).cast()), None);
            // A MIDI note-on comes back as a note, like everywhere else in
            // the framework.
            let struck = clap_event_midi {
                header: header::<clap_event_midi>(6, CLAP_EVENT_MIDI),
                port_index: 0,
                data: [0x91, 64, 127],
            };
            assert_eq!(
                from_clap((&raw const struck).cast()),
                Some((
                    6,
                    OutputKind::Event(EventKind::NoteOn {
                        note: NoteRef::new(1, 64),
                        velocity: 1.0
                    })
                ))
            );
        }
    }

    #[test]
    fn the_transport_says_what_is_known_and_flags_what_is_not() {
        let stopped = transport_event(&Transport::STOPPED, 48_000.0);
        assert_eq!(stopped.flags, 0);
        assert_eq!(stopped.header.type_, CLAP_EVENT_TRANSPORT);
        assert_eq!(
            stopped.header.size as usize,
            size_of::<clap_event_transport>()
        );
        assert_eq!((stopped.tempo, stopped.song_pos_beats), (0.0, 0));

        let mut transport = Transport::playing_at(97.0);
        transport.recording = true;
        transport.time_signature = Some((6, 8));
        transport.position_beats = Some(7.5);
        // 6/8 is three quarter notes to the bar, so 7.5 is in the third.
        transport.bar_start_beats = Some(6.0);
        transport.position_samples = Some(72_000);
        transport.loop_beats = Some((4.0, 8.0));
        let event = transport_event(&transport, 48_000.0);
        assert_eq!(
            event.flags,
            CLAP_TRANSPORT_IS_PLAYING
                | CLAP_TRANSPORT_IS_RECORDING
                | CLAP_TRANSPORT_HAS_TEMPO
                | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                | CLAP_TRANSPORT_IS_LOOP_ACTIVE
        );
        assert_eq!(event.tempo, 97.0);
        assert_eq!((event.tsig_num, event.tsig_denom), (6, 8));
        // Fixed point with 31 fractional bits.
        assert_eq!(event.song_pos_beats, 15 << 30);
        assert_eq!(event.bar_start, 6 << 31);
        assert_eq!(event.bar_number, 2);
        assert_eq!(event.song_pos_seconds, 3 << 30);
        assert_eq!(
            (event.loop_start_beats, event.loop_end_beats),
            (4 << 31, 8 << 31)
        );
        let seconds = |fixed: i64| fixed as f64 / CLAP_SECTIME_FACTOR as f64;
        assert!((seconds(event.loop_start_seconds) - 4.0 * 60.0 / 97.0).abs() < 1e-9);
        assert!((seconds(event.loop_end_seconds) - 8.0 * 60.0 / 97.0).abs() < 1e-9);

        // Without a position in beats there is no loop to describe, and
        // values that are not numbers do not become positions.
        let mut vague = Transport::STOPPED;
        vague.loop_beats = Some((1.0, 2.0));
        vague.tempo_bpm = Some(f64::NAN);
        vague.position_samples = Some(i64::MAX);
        let event = transport_event(&vague, 0.0);
        assert_eq!(event.flags, 0);
        assert_eq!(event.tempo, 0.0);
    }

    #[test]
    fn a_parameter_is_found_by_id() {
        let slots = slots();
        assert_eq!(find_param(&slots, 7).map(|slot| slot.max), Some(10.0));
        assert_eq!(find_param(&slots, CUTOFF.0).map(|slot| slot.max), Some(1.0));
        assert!(find_param(&slots, 6).is_none());
        assert!(find_param(&[], 7).is_none());
    }

    #[test]
    fn what_a_plugin_reports_about_a_parameter_is_held_to_what_it_declared() {
        let slots = slots();
        let value = |param, value| OutputKind::Event(EventKind::ParamValue { param, value });
        let wide = ParamId(7);
        let unlisted = ParamId(8);

        // Inside the range: untouched. Outside: brought to its nearer end.
        for (param, reported, kept) in [
            (CUTOFF, 0.25, 0.25),
            (CUTOFF, 0.0, 0.0),
            (CUTOFF, 1.0, 1.0),
            (CUTOFF, 99.0, 1.0),
            (CUTOFF, -0.001, 0.0),
            (wide, -1e300, -10.0),
            (wide, f64::MAX, 10.0),
            (wide, 9.5, 9.5),
        ] {
            assert_eq!(
                held_to_declared(value(param, reported), &slots),
                Some(value(param, kept)),
                "{reported} for parameter {}",
                param.0
            );
        }

        // A parameter the plugin never listed: the report is refused,
        // whether it is a value or a gesture.
        assert_eq!(held_to_declared(value(unlisted, 0.5), &slots), None);
        assert_eq!(held_to_declared(value(CUTOFF, 0.5), &[]), None);
        for param in [CUTOFF, wide, unlisted] {
            let listed = param != unlisted;
            let begin = OutputKind::GestureBegin { param };
            let end = OutputKind::GestureEnd { param };
            assert_eq!(held_to_declared(begin, &slots), listed.then_some(begin));
            assert_eq!(held_to_declared(end, &slots), listed.then_some(end));
        }

        // What is not about a parameter passes as it is, list or no list.
        let note = NoteRef::with_id(1, 60, 4);
        for kind in [
            OutputKind::NoteEnd { note },
            OutputKind::Event(EventKind::NoteOn {
                note,
                velocity: 0.5,
            }),
            OutputKind::Event(EventKind::Midi(MidiMessage::ProgramChange {
                channel: 1,
                program: 5,
            })),
        ] {
            assert_eq!(held_to_declared(kind, &[]), Some(kind));
        }
    }

    #[test]
    fn holding_a_value_agrees_with_clamp_and_cannot_panic() {
        let slot = |min, max| ParamSlot {
            id: 0,
            cookie: std::ptr::null_mut(),
            min,
            max,
            index: 0,
        };
        // Bit for bit what `f64::clamp` gives, negative zero included, for
        // every range a slot is made with.
        for (min, max) in [(0.0, 1.0), (-10.0, 10.0), (3.0, 3.0), (-0.0, 0.0)] {
            for value in [
                -1e9,
                -10.0,
                -1.0,
                -0.0,
                0.0,
                1e-320,
                0.5,
                1.0,
                3.0,
                10.0,
                1e9,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ] {
                assert_eq!(
                    slot(min, max).hold(value).to_bits(),
                    value.clamp(min, max).to_bits(),
                    "{value} in {min}..={max}"
                );
            }
        }
        assert!(slot(0.0, 1.0).hold(f64::NAN).is_nan());
        // Ranges no slot is made with, where `f64::clamp` would panic: an
        // answer comes back all the same.
        let _ = slot(2.0, 0.0).hold(1.0);
        let _ = slot(f64::NAN, 1.0).hold(1.0);
        let _ = slot(0.0, f64::NAN).hold(1.0);
    }

    #[test]
    fn note_expressions_stay_within_the_ranges_clap_gives_them() {
        let note = NoteRef::with_id(0, 60, 1);
        let every = [
            Expression::Volume,
            Expression::Pan,
            Expression::Tuning,
            Expression::Vibrato,
            Expression::Brightness,
            Expression::Pressure,
        ];
        let not_finite = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];

        // On the way to the plugin.
        let sent = |expression, value, route| {
            let kind = EventKind::NoteExpression {
                note,
                expression,
                value,
            };
            match to_clap(&kind, 0, &[], route) {
                Some(InEvent::Expression(event)) => Some(event.value),
                Some(InEvent::Midi(event)) => Some(f64::from(event.data[2])),
                Some(_) => panic!("an expression became another kind of event"),
                None => None,
            }
        };
        for (expression, value, expected) in [
            (Expression::Volume, 9.0, 4.0),
            (Expression::Volume, -1.0, 0.0),
            (Expression::Volume, 2.5, 2.5),
            (Expression::Tuning, 500.0, 120.0),
            (Expression::Tuning, -500.0, -120.0),
            (Expression::Tuning, -119.5, -119.5),
            // The framework's pan of -1..=1 is CLAP's 0..=1.
            (Expression::Pan, 3.0, 1.0),
            (Expression::Pan, -3.0, 0.0),
            (Expression::Pan, 0.0, 0.5),
            (Expression::Vibrato, 1.5, 1.0),
            (Expression::Brightness, -0.5, 0.0),
            (Expression::Pressure, 7.0, 1.0),
            (Expression::Pressure, 0.25, 0.25),
        ] {
            assert_eq!(
                sent(expression, value, BOTH),
                Some(expected),
                "{expression:?} {value}"
            );
        }
        for expression in every {
            for value in not_finite {
                assert_eq!(
                    sent(expression, value, BOTH),
                    None,
                    "{expression:?} {value}"
                );
                assert_eq!(
                    sent(expression, value, MIDI_ONLY),
                    None,
                    "{expression:?} {value} as MIDI"
                );
            }
        }

        // On the way back.
        let reported = |expression_id, value| {
            let event = clap_event_note_expression {
                header: header::<clap_event_note_expression>(0, CLAP_EVENT_NOTE_EXPRESSION),
                expression_id,
                note_id: 1,
                port_index: 0,
                channel: 0,
                key: 60,
                value,
            };
            // SAFETY: the pointer is to a whole, live event.
            match unsafe { from_clap((&raw const event).cast()) } {
                Some((_, OutputKind::Event(EventKind::NoteExpression { value, .. }))) => {
                    Some(value)
                }
                Some(other) => panic!("an expression came back as {other:?}"),
                None => None,
            }
        };
        for (id, value, expected) in [
            (CLAP_NOTE_EXPRESSION_VOLUME, 100.0, 4.0),
            (CLAP_NOTE_EXPRESSION_VOLUME, -0.5, 0.0),
            (CLAP_NOTE_EXPRESSION_VOLUME, 1.25, 1.25),
            (CLAP_NOTE_EXPRESSION_TUNING, -1e9, -120.0),
            (CLAP_NOTE_EXPRESSION_TUNING, 1e300, 120.0),
            (CLAP_NOTE_EXPRESSION_TUNING, 0.5, 0.5),
            // CLAP's pan of 0..=1 is the framework's -1..=1.
            (CLAP_NOTE_EXPRESSION_PAN, 7.0, 1.0),
            (CLAP_NOTE_EXPRESSION_PAN, -7.0, -1.0),
            (CLAP_NOTE_EXPRESSION_PAN, 0.5, 0.0),
            (CLAP_NOTE_EXPRESSION_VIBRATO, 2.0, 1.0),
            (CLAP_NOTE_EXPRESSION_BRIGHTNESS, -2.0, 0.0),
            (CLAP_NOTE_EXPRESSION_PRESSURE, -3.0, 0.0),
            (CLAP_NOTE_EXPRESSION_PRESSURE, 0.75, 0.75),
        ] {
            assert_eq!(reported(id, value), Some(expected), "{id} {value}");
        }
        for id in [
            CLAP_NOTE_EXPRESSION_VOLUME,
            CLAP_NOTE_EXPRESSION_PAN,
            CLAP_NOTE_EXPRESSION_TUNING,
            CLAP_NOTE_EXPRESSION_VIBRATO,
            CLAP_NOTE_EXPRESSION_BRIGHTNESS,
            CLAP_NOTE_EXPRESSION_PRESSURE,
        ] {
            for value in not_finite {
                assert_eq!(reported(id, value), None, "{id} {value}");
            }
        }
        // An expression with no equivalent in the framework, and one CLAP
        // does not define.
        assert_eq!(reported(CLAP_NOTE_EXPRESSION_EXPRESSION, 0.5), None);
        assert_eq!(reported(99, 0.5), None);
    }
}
