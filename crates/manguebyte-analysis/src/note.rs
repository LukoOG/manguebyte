//! The nearest note of the equal-tempered scale, and how far off a
//! frequency is from it.

use core::fmt;

/// The names of the twelve pitch classes, from C, with sharps.
const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The MIDI note number of A4.
const A4_MIDI: i32 = 69;

/// A note of the twelve-tone equal-tempered scale, with the offset of a
/// measured frequency from it in cents.
///
/// Notes are numbered as in MIDI: 69 is A4, 60 is middle C (C4), and each
/// step is a semitone. Octaves follow scientific pitch notation, starting
/// at C, so B3 is followed by C4. Names use sharps.
///
/// ```
/// use manguebyte_analysis::Note;
///
/// let note = Note::nearest(445.0).unwrap();
/// assert_eq!(note.midi_number(), 69);
/// assert_eq!(note.to_string(), "A4");
/// assert!((note.cents() - 19.56).abs() < 0.01);
///
/// let low = Note::nearest(55.0).unwrap();
/// assert_eq!((low.name(), low.octave()), ("A", 1));
/// assert_eq!(Note::nearest(0.0), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    midi: i32,
    cents: f64,
}

impl Note {
    /// The usual tuning reference: A4 at 440 Hz.
    pub const A4_HZ: f64 = 440.0;

    /// The note nearest to `frequency` hertz with A4 at 440 Hz, or `None`
    /// for a frequency that is not a positive number.
    pub fn nearest(frequency: f64) -> Option<Self> {
        Self::nearest_with_reference(frequency, Self::A4_HZ)
    }

    /// The note nearest to `frequency` hertz with A4 at `a4` hertz, or
    /// `None` if either is not a positive number.
    ///
    /// The offset is in `-50.0..=50.0` cents; a frequency exactly half way
    /// between two notes goes to the upper one. Every pair of positive
    /// numbers has a note, however far apart they are.
    pub fn nearest_with_reference(frequency: f64, a4: f64) -> Option<Self> {
        let usable = |value: f64| value.is_finite() && value > 0.0;
        if !(usable(frequency) && usable(a4)) {
            return None;
        }
        // The difference of the logarithms rather than the logarithm of the
        // ratio: the ratio of two finite numbers can overflow to infinity or
        // underflow to zero, while their base-2 logarithms lie between -1074
        // and 1024. So the count is within 12 * 2098 semitones either way,
        // and the note number fits an i32 with room to spare.
        let semitones = 12.0 * (frequency.log2() - a4.log2());
        let steps = (semitones + 0.5).floor();
        Some(Self {
            midi: A4_MIDI + steps as i32,
            cents: 100.0 * (semitones - steps),
        })
    }

    /// The note with MIDI number `midi`, exactly in tune.
    pub fn from_midi(midi: i32) -> Self {
        Self { midi, cents: 0.0 }
    }

    /// The MIDI note number: 69 for A4. Notes below C-1 have negative
    /// numbers.
    pub fn midi_number(&self) -> i32 {
        self.midi
    }

    /// How far the measured frequency lies from the note, in cents
    /// (hundredths of a semitone): positive when it is sharp.
    pub fn cents(&self) -> f64 {
        self.cents
    }

    /// The name of the pitch class: "C", "C#", "D" and so on to "B".
    pub fn name(&self) -> &'static str {
        NAMES[self.midi.rem_euclid(12) as usize]
    }

    /// The octave in scientific pitch notation: 4 for middle C and for A4.
    pub fn octave(&self) -> i32 {
        self.midi.div_euclid(12) - 1
    }

    /// The frequency of the note itself, in tune, with A4 at `a4` hertz.
    ///
    /// Any note number is accepted. One so far from A4 that its frequency
    /// is beyond the range of `f64` gives zero or infinity.
    pub fn frequency(&self, a4: f64) -> f64 {
        // In f64, where the difference cannot overflow as it can in i32.
        let semitones = f64::from(self.midi) - f64::from(A4_MIDI);
        a4 * 2f64.powf(semitones / 12.0)
    }
}

/// The name and octave, such as `C#4`.
impl fmt::Display for Note {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", self.name(), self.octave())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_named_and_numbered_as_in_midi() {
        for (frequency, midi, text) in [
            (440.0, 69, "A4"),
            (261.625_565_300_598_6, 60, "C4"),
            (27.5, 21, "A0"),
            (4_186.009_044_809_578, 108, "C8"),
            (8.175_798_915_643_707, 0, "C-1"),
            (277.182_630_976_872_1, 61, "C#4"),
            (246.941_650_628_062_2, 59, "B3"),
        ] {
            let note = Note::nearest(frequency).unwrap();
            assert_eq!(note.midi_number(), midi, "{frequency}");
            assert_eq!(note.to_string(), text);
            assert!(note.cents().abs() < 1e-9, "{frequency}: {}", note.cents());
            assert!((note.frequency(440.0) / frequency - 1.0).abs() < 1e-14);
        }
        assert_eq!(Note::from_midi(-1).to_string(), "B-2");
        assert_eq!(Note::from_midi(127).to_string(), "G9");
    }

    #[test]
    fn the_offset_is_in_cents_and_never_beyond_half_a_semitone() {
        let a4 = 440.0;
        for cents in [-49.9, -25.0, -1.0, 0.0, 0.5, 13.7, 49.9] {
            let frequency = a4 * 2f64.powf(cents / 1_200.0);
            let note = Note::nearest(frequency).unwrap();
            assert_eq!(note.midi_number(), 69);
            assert!(
                (note.cents() - cents).abs() < 1e-9,
                "{cents}: {}",
                note.cents()
            );
        }
        // Half way between two notes the offset is half a semitone either
        // way; which note wins there is up to the last bit of the input.
        let half = Note::nearest(a4 * 2f64.powf(50.0 / 1_200.0)).unwrap();
        assert!(
            (half.midi_number() == 70 && (half.cents() + 50.0).abs() < 1e-9)
                || (half.midi_number() == 69 && (half.cents() - 50.0).abs() < 1e-9),
            "{half:?}"
        );
        // Across the whole range of a piano and beyond.
        for midi in -20..140 {
            for cents in [-49.0, 0.0, 49.0] {
                let frequency = Note::from_midi(midi).frequency(a4) * 2f64.powf(cents / 1_200.0);
                let note = Note::nearest(frequency).unwrap();
                assert_eq!(note.midi_number(), midi);
                assert!((note.cents() - cents).abs() < 1e-7);
            }
        }
    }

    #[test]
    fn another_reference_moves_every_note() {
        let note = Note::nearest_with_reference(432.0, 432.0).unwrap();
        assert_eq!((note.midi_number(), note.cents()), (69, 0.0));
        let note = Note::nearest_with_reference(440.0, 432.0).unwrap();
        assert!((note.cents() - 31.77).abs() < 0.01, "{}", note.cents());
    }

    #[test]
    fn frequencies_that_are_not_positive_numbers_have_no_note() {
        for frequency in [0.0, -440.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(Note::nearest(frequency), None);
        }
        assert_eq!(Note::nearest_with_reference(440.0, 0.0), None);
        assert_eq!(Note::nearest_with_reference(440.0, f64::NAN), None);
        // Tiny and huge positive frequencies still have one.
        assert!(Note::nearest(f64::MIN_POSITIVE).is_some());
        assert!(Note::nearest(f64::MAX).is_some());
    }

    #[test]
    fn the_most_extreme_arguments_have_a_note_and_never_overflow() {
        // The ratio of these overflows to infinity or underflows to zero;
        // the note is still computed, from the logarithms.
        let smallest = f64::from_bits(1);
        for (frequency, a4) in [
            (1e300, 1e-300),
            (1e-300, 1e300),
            (f64::MAX, smallest),
            (smallest, f64::MAX),
        ] {
            let note = Note::nearest_with_reference(frequency, a4).unwrap();
            let semitones = 12.0 * (frequency.log2() - a4.log2());
            let expected = A4_MIDI + (semitones + 0.5).floor() as i32;
            assert_eq!(note.midi_number(), expected, "{frequency} {a4}");
            assert!(note.cents().abs() <= 50.0, "{note:?}");
        }
        // The widest span there is: 2098 octaves.
        let widest = Note::nearest_with_reference(f64::MAX, smallest).unwrap();
        assert_eq!(widest.midi_number(), A4_MIDI + 12 * 2098);
        // Any note number has a frequency, if not always a finite one.
        assert_eq!(Note::from_midi(i32::MIN).frequency(440.0), 0.0);
        assert_eq!(Note::from_midi(i32::MAX).frequency(440.0), f64::INFINITY);
        let high = Note::from_midi(69 + 12 * 1_000).frequency(1.0);
        assert!((high / 2f64.powi(1_000) - 1.0).abs() < 1e-12, "{high}");
        // And a name and an octave: -2147483648 is 4 above a multiple of 12,
        // 2147483647 is 7 above one.
        assert_eq!(Note::from_midi(i32::MIN).to_string(), "E-178956972");
        assert_eq!(Note::from_midi(i32::MAX).to_string(), "G178956969");
    }
}
