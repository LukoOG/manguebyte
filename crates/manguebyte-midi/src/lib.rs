//! MIDI messages and sample-accurate event buffers.
//!
//! - [`MidiMessage`]: a parsed channel voice message.
//! - [`MidiBuffer`]: messages ordered by their position inside an audio block.
//! - [`note_to_hz`] / [`hz_to_note`]: equal-temperament pitch conversions.

mod buffer;
mod message;

pub use buffer::{MidiBuffer, MidiEvent};
pub use message::{MidiMessage, RawMidi};

/// Frequency in hertz of a MIDI note number, in equal temperament with
/// A4 (note 69) at 440 Hz. Fractional notes are allowed.
#[inline]
pub fn note_to_hz(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

/// MIDI note number of a frequency in hertz. The inverse of [`note_to_hz`].
#[inline]
pub fn hz_to_note(hz: f64) -> f64 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_conversions() {
        assert!((note_to_hz(69.0) - 440.0).abs() < 1e-9);
        assert!((note_to_hz(81.0) - 880.0).abs() < 1e-9);
        assert!((note_to_hz(60.0) - 261.625_565).abs() < 1e-5);
        assert!((hz_to_note(note_to_hz(42.5)) - 42.5).abs() < 1e-9);
    }
}
