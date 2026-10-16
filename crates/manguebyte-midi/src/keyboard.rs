//! Which keys are down, for showing on screen.
//!
//! [`KeyboardState`] follows note and pedal messages and remembers, for
//! each of the sixteen channels, which keys are held, which are still
//! sounding because the sustain pedal holds them, and how hard each was
//! struck. It exists so that a user interface can draw a keyboard without
//! taking part in the audio thread's work: the thread that handles MIDI
//! writes into it and the interface reads from it, with no lock between
//! them.
//!
//! ```
//! use std::sync::Arc;
//!
//! use manguebyte_midi::keyboard::KeyboardState;
//! use manguebyte_midi::MidiMessage;
//!
//! let keyboard = Arc::new(KeyboardState::new());
//!
//! // On the thread that receives MIDI:
//! let writer = Arc::clone(&keyboard);
//! writer.process(MidiMessage::NoteOn { channel: 0, note: 60, velocity: 100 });
//! writer.process(MidiMessage::NoteOn { channel: 0, note: 64, velocity: 80 });
//!
//! // On the interface's timer:
//! assert!(keyboard.is_down(0, 60));
//! assert_eq!(keyboard.velocity(0, 64), 80);
//! assert_eq!(keyboard.keys_down(0).iter().collect::<Vec<_>>(), [60, 64]);
//! ```

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use crate::MidiMessage;

const SUSTAIN_PEDAL: u8 = 64;
const ALL_SOUND_OFF: u8 = 120;
const RESET_ALL_CONTROLLERS: u8 = 121;
const ALL_NOTES_OFF: u8 = 123;
/// The first and the last of the four mode messages: omni off, omni on,
/// mono on and poly on. The MIDI 1.0 Detailed Specification gives each of
/// them the effect of "all notes off" as well.
const OMNI_OFF: u8 = 124;
const POLY_ON: u8 = 127;

/// A set of keys: one bit for each of the 128 note numbers.
///
/// ```
/// use manguebyte_midi::keyboard::KeySet;
///
/// let mut chord = KeySet::new();
/// chord.insert(60);
/// chord.insert(67);
/// assert!(chord.contains(67) && !chord.contains(64));
/// assert_eq!(chord.len(), 2);
/// assert_eq!((chord.lowest(), chord.highest()), (Some(60), Some(67)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeySet(u128);

impl KeySet {
    /// The empty set.
    pub const fn new() -> Self {
        Self(0)
    }

    /// Adds a key. Note numbers above 127 are taken modulo 128.
    pub fn insert(&mut self, note: u8) {
        self.0 |= 1 << (note & 0x7F);
    }

    /// Removes a key.
    pub fn remove(&mut self, note: u8) {
        self.0 &= !(1 << (note & 0x7F));
    }

    /// Whether the key is in the set.
    pub const fn contains(&self, note: u8) -> bool {
        note < 128 && self.0 & (1 << note) != 0
    }

    /// How many keys are in the set.
    pub const fn len(&self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether the set holds no keys.
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// The lowest key in the set.
    pub const fn lowest(&self) -> Option<u8> {
        if self.0 == 0 {
            None
        } else {
            Some(self.0.trailing_zeros() as u8)
        }
    }

    /// The highest key in the set.
    pub const fn highest(&self) -> Option<u8> {
        if self.0 == 0 {
            None
        } else {
            Some(127 - self.0.leading_zeros() as u8)
        }
    }

    /// The keys that are in this set or in `other`.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The keys, lowest first.
    pub const fn iter(&self) -> Keys {
        Keys(self.0)
    }
}

impl IntoIterator for KeySet {
    type Item = u8;
    type IntoIter = Keys;

    fn into_iter(self) -> Keys {
        self.iter()
    }
}

/// The iterator over a [`KeySet`]: note numbers in rising order.
#[derive(Debug, Clone)]
pub struct Keys(u128);

impl Iterator for Keys {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.0 == 0 {
            return None;
        }
        let note = self.0.trailing_zeros() as u8;
        // Clears the lowest set bit.
        self.0 &= self.0 - 1;
        Some(note)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.0.count_ones() as usize;
        (len, Some(len))
    }
}

impl ExactSizeIterator for Keys {}

/// The "held" bit of each of the sixteen keys in a word.
const HELD: u32 = 0x5555_5555;
/// The "kept by the pedal" bit of each of the sixteen keys in a word.
const KEPT: u32 = 0xAAAA_AAAA;

/// The keys of one channel.
///
/// Each key has two bits side by side in one word: the lower says the key
/// is physically held, the upper that it was released while the pedal was
/// down and is still sounding. Both can be set, for a key the pedal was
/// keeping and that was then struck again. Sixteen keys fill a word and
/// eight words a channel; 32-bit atomics are used because they exist on
/// every target that has atomics at all.
///
/// The two bits share a word so that a reader gets both with one load, and
/// the writer changes a key only through states in which the key reads as
/// it did before the change or as it will after it. A key that goes from
/// held to kept by the pedal is marked kept first and stops being held
/// second, so there is no moment at which it is neither.
struct ChannelKeys {
    keys: [AtomicU32; 8],
    /// Velocity of the most recent note-on for each key.
    velocity: [AtomicU8; 128],
    pedal: AtomicBool,
}

impl ChannelKeys {
    const fn new() -> Self {
        Self {
            keys: [const { AtomicU32::new(0) }; 8],
            velocity: [const { AtomicU8::new(0) }; 128],
            pedal: AtomicBool::new(false),
        }
    }

    /// Reads the eight words one after the other and collects one bit for
    /// each key: the one `pick` leaves in the position of the "held" bit.
    fn collect(&self, pick: impl Fn(u32) -> u32) -> KeySet {
        let mut bits = 0u128;
        for (index, word) in self.keys.iter().enumerate() {
            let picked = pick(word.load(Ordering::Relaxed));
            bits |= u128::from(even_bits(picked)) << (16 * index);
        }
        KeySet(bits)
    }
}

/// Packs the sixteen even-numbered bits of a word into its lower half:
/// bit 0 stays, bit 2 becomes bit 1, bit 4 becomes bit 2 and so on. Each
/// step moves the kept bits together in groups twice as large.
const fn even_bits(word: u32) -> u16 {
    let mut bits = word & 0x5555_5555;
    bits = (bits | (bits >> 1)) & 0x3333_3333;
    bits = (bits | (bits >> 2)) & 0x0F0F_0F0F;
    bits = (bits | (bits >> 4)) & 0x00FF_00FF;
    bits = (bits | (bits >> 8)) & 0x0000_FFFF;
    bits as u16
}

/// Where a note's two bits live: which word, and the "held" bit in it.
/// The "kept by the pedal" bit is the next one up.
fn position(note: u8) -> (usize, u32) {
    let note = note & 0x7F;
    (usize::from(note >> 4), 1 << (2 * u32::from(note & 15)))
}

/// Which keys are down on each channel, with the sustain pedal, readable
/// from any thread while another thread updates it.
///
/// Every method takes `&self` and is wait-free: each is a fixed number of
/// atomic operations, with no lock, no retry loop and no allocation, so
/// the writing side can be the audio thread. Share it in an
/// [`Arc`](std::sync::Arc).
///
/// A key is **down** from its note-on to its note-off. It is **sounding**
/// while it is down and, if the sustain pedal (controller 64) was down
/// when the key was released, until the pedal lifts.
///
/// # What a reader sees
///
/// Whether a key is down and whether the pedal keeps it are stored
/// together and read with one load. While one thread writes, a reader on
/// another thread therefore gets, for every key, either what was true
/// before the message being applied or what is true after it: a key that
/// goes on sounding as it passes from the hand to the pedal, or back, is
/// never reported silent in between, and a key that was not struck is
/// never reported.
///
/// # Limits
///
/// Updates are expected from one thread at a time. Two threads writing at
/// once cannot corrupt anything, but a key may be left sounding after the
/// message that should have ended it.
///
/// A reader sees each group of 16 keys at one instant, not the whole
/// keyboard, so a chord struck during a read may appear a frame late in
/// part. The velocity of a key is stored apart from its state: a reader
/// may get the velocity of a new strike an instant before the strike
/// itself.
///
/// Each channel has its own sustain pedal and its own keys, as in
/// ordinary MIDI. MPE zones are not known here: in an MPE stream the
/// pedal on a master channel does not keep the keys of the member
/// channels sounding, so [`sounding`](Self::sounding) can differ from
/// what [`NoteTracker`](crate::mpe::NoteTracker) reports for the same
/// stream. [`keys_down`](Self::keys_down) is not affected.
///
/// The sostenuto pedal is not followed, and two note-ons for one key
/// count as one key down.
pub struct KeyboardState {
    channels: [ChannelKeys; 16],
}

impl core::fmt::Debug for KeyboardState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut map = f.debug_map();
        for channel in 0..16u8 {
            let sounding = self.sounding(channel);
            if !sounding.is_empty() {
                map.entry(&channel, &sounding.iter().collect::<SmallList>());
            }
        }
        map.finish()
    }
}

/// Up to 128 note numbers, for printing without allocating.
struct SmallList {
    notes: [u8; 128],
    len: usize,
}

impl FromIterator<u8> for SmallList {
    fn from_iter<I: IntoIterator<Item = u8>>(iter: I) -> Self {
        let mut list = Self {
            notes: [0; 128],
            len: 0,
        };
        for note in iter.into_iter().take(128) {
            list.notes[list.len] = note;
            list.len += 1;
        }
        list
    }
}

impl core::fmt::Debug for SmallList {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(&self.notes[..self.len]).finish()
    }
}

impl KeyboardState {
    /// A keyboard with no key down and every pedal up.
    pub const fn new() -> Self {
        Self {
            channels: [const { ChannelKeys::new() }; 16],
        }
    }

    fn channel(&self, channel: u8) -> &ChannelKeys {
        &self.channels[usize::from(channel & 0x0F)]
    }

    /// Updates the state from a message. Note-on and note-off move keys,
    /// controller 64 moves the sustain pedal, "all notes off" releases
    /// every key of the channel as note-offs would, "all sound off" clears
    /// the channel at once, and "reset all controllers" lifts the pedal.
    /// The mode messages (controllers 124 to 127: omni off, omni on, mono
    /// and poly) are followed for the "all notes off" that MIDI makes part
    /// of each of them; the modes themselves change nothing here.
    /// Everything else is ignored. Real-time safe.
    pub fn process(&self, message: MidiMessage) {
        match message {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                if velocity & 0x7F == 0 {
                    self.note_off(channel, note);
                } else {
                    self.note_on(channel, note, velocity);
                }
            }
            MidiMessage::NoteOff { channel, note, .. } => self.note_off(channel, note),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => match controller {
                SUSTAIN_PEDAL => self.set_sustain(channel, value >= 64),
                ALL_SOUND_OFF => self.all_sound_off(channel),
                RESET_ALL_CONTROLLERS => self.set_sustain(channel, false),
                ALL_NOTES_OFF | OMNI_OFF..=POLY_ON => self.all_notes_off(channel),
                _ => {}
            },
            _ => {}
        }
    }

    /// Marks a key as down, struck with `velocity`.
    pub fn note_on(&self, channel: u8, note: u8, velocity: u8) {
        let keys = self.channel(channel);
        let (word, held) = position(note);
        keys.velocity[usize::from(note & 0x7F)].store(velocity & 0x7F, Ordering::Relaxed);
        // If the pedal was keeping the key its "kept" bit stays set. That
        // changes nothing a reader sees, and the bit is in place if the
        // key is released again under the pedal.
        keys.keys[word].fetch_or(held, Ordering::Relaxed);
    }

    /// Marks a key as released. With the sustain pedal down it keeps
    /// sounding until the pedal lifts.
    pub fn note_off(&self, channel: u8, note: u8) {
        let keys = self.channel(channel);
        let (word, held) = position(note);
        let kept = held << 1;
        let cell = &keys.keys[word];
        if !keys.pedal.load(Ordering::Relaxed) {
            cell.fetch_and(!(held | kept), Ordering::Relaxed);
        } else if cell.load(Ordering::Relaxed) & held != 0 {
            // Kept first, released second: at every step the key sounds.
            cell.fetch_or(kept, Ordering::Relaxed);
            cell.fetch_and(!held, Ordering::Relaxed);
        }
    }

    /// Moves the sustain pedal of a channel. Lifting it ends the notes it
    /// was holding.
    pub fn set_sustain(&self, channel: u8, down: bool) {
        let keys = self.channel(channel);
        keys.pedal.store(down, Ordering::Relaxed);
        if !down {
            for word in &keys.keys {
                word.fetch_and(HELD, Ordering::Relaxed);
            }
        }
    }

    /// Releases every key of a channel, as a note-off for each would: with
    /// the pedal down they keep sounding until it lifts.
    pub fn all_notes_off(&self, channel: u8) {
        let keys = self.channel(channel);
        let pedal = keys.pedal.load(Ordering::Relaxed);
        for word in &keys.keys {
            if pedal {
                // As in `note_off`, for every held key of the word at
                // once: kept first, released second.
                let held = word.load(Ordering::Relaxed) & HELD;
                word.fetch_or(held << 1, Ordering::Relaxed);
                word.fetch_and(KEPT, Ordering::Relaxed);
            } else {
                word.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Silences a channel at once: no key down, none sustained. The pedal
    /// stays where it is.
    pub fn all_sound_off(&self, channel: u8) {
        for word in &self.channel(channel).keys {
            word.store(0, Ordering::Relaxed);
        }
    }

    /// Clears every channel and lifts every pedal.
    pub fn reset(&self) {
        for channel in 0..16 {
            self.all_sound_off(channel);
            self.channel(channel).pedal.store(false, Ordering::Relaxed);
        }
    }

    /// Whether the key is held on `channel`.
    pub fn is_down(&self, channel: u8, note: u8) -> bool {
        let (word, held) = position(note);
        note < 128 && self.channel(channel).keys[word].load(Ordering::Relaxed) & held != 0
    }

    /// Whether the key is held on `channel` or kept sounding by its
    /// sustain pedal.
    pub fn is_sounding(&self, channel: u8, note: u8) -> bool {
        let (word, held) = position(note);
        let either = held | (held << 1);
        note < 128 && self.channel(channel).keys[word].load(Ordering::Relaxed) & either != 0
    }

    /// The velocity the key was last struck with on `channel`, `1..=127`,
    /// or 0 if it is not sounding.
    pub fn velocity(&self, channel: u8, note: u8) -> u8 {
        if self.is_sounding(channel, note) {
            self.channel(channel).velocity[usize::from(note & 0x7F)].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// Whether the sustain pedal of `channel` is down.
    pub fn is_sustain_down(&self, channel: u8) -> bool {
        self.channel(channel).pedal.load(Ordering::Relaxed)
    }

    /// The keys held on `channel`.
    pub fn keys_down(&self, channel: u8) -> KeySet {
        self.channel(channel).collect(|word| word)
    }

    /// The keys sounding on `channel`: held, or kept by the pedal.
    pub fn sounding(&self, channel: u8) -> KeySet {
        // Either of a key's two bits, brought down to the lower one.
        self.channel(channel).collect(|word| word | (word >> 1))
    }

    /// The keys held on any channel, for a keyboard that shows all of
    /// them together.
    pub fn keys_down_on_any_channel(&self) -> KeySet {
        (0..16).fold(KeySet::new(), |all, channel| {
            all.union(self.keys_down(channel))
        })
    }

    /// The keys sounding on any channel.
    pub fn sounding_on_any_channel(&self) -> KeySet {
        (0..16).fold(KeySet::new(), |all, channel| {
            all.union(self.sounding(channel))
        })
    }
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

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
            velocity: 0,
        }
    }

    fn cc(channel: u8, controller: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        }
    }

    fn keys(set: KeySet) -> Vec<u8> {
        set.iter().collect()
    }

    #[test]
    fn keys_go_down_and_up_on_their_own_channel() {
        let keyboard = KeyboardState::new();
        for message in [on(0, 60, 100), on(0, 0, 1), on(0, 127, 127), on(3, 60, 50)] {
            keyboard.process(message);
        }
        assert_eq!(keys(keyboard.keys_down(0)), [0, 60, 127]);
        assert_eq!(keys(keyboard.keys_down(3)), [60]);
        assert!(keyboard.keys_down(1).is_empty());
        assert!(keyboard.is_down(0, 60) && !keyboard.is_down(1, 60));
        assert_eq!(keyboard.velocity(0, 60), 100);
        assert_eq!(keyboard.velocity(3, 60), 50);
        assert_eq!(keyboard.velocity(0, 61), 0);
        assert_eq!(keys(keyboard.keys_down_on_any_channel()), [0, 60, 127]);

        keyboard.process(off(0, 60));
        assert!(!keyboard.is_down(0, 60) && !keyboard.is_sounding(0, 60));
        assert_eq!(keyboard.velocity(0, 60), 0);
        // The same key on another channel is not touched.
        assert!(keyboard.is_down(3, 60));
        // A note-on with velocity zero is a note-off.
        keyboard.process(on(3, 60, 0));
        assert!(!keyboard.is_down(3, 60));
        // Releasing a key that is not down does nothing.
        keyboard.process(off(5, 60));
        assert!(keyboard.sounding(5).is_empty());
        // Note numbers that are not MIDI notes are never down.
        assert!(!keyboard.is_down(0, 128) && !keyboard.is_sounding(0, 255));
    }

    #[test]
    fn every_key_of_every_channel_is_tracked_separately() {
        let keyboard = KeyboardState::new();
        for channel in 0..16u8 {
            for note in 0..128u8 {
                keyboard.note_on(channel, note, 1 + (note + channel) % 127);
                assert_eq!(keyboard.keys_down(channel).len(), usize::from(note) + 1);
                assert_eq!(keyboard.keys_down(channel).highest(), Some(note));
            }
        }
        for channel in 0..16u8 {
            for note in 0..128u8 {
                assert!(keyboard.is_down(channel, note));
                assert_eq!(keyboard.velocity(channel, note), 1 + (note + channel) % 127);
            }
        }
        for channel in 0..16u8 {
            for note in 0..128u8 {
                keyboard.note_off(channel, note);
                assert_eq!(keyboard.keys_down(channel).len(), 127 - usize::from(note));
                assert_eq!(
                    keyboard.keys_down(channel).lowest(),
                    note.checked_add(1).filter(|next| *next < 128)
                );
            }
            // Other channels are still full until their turn.
            if channel < 15 {
                assert_eq!(keyboard.keys_down(channel + 1).len(), 128);
            }
        }
        assert!(keyboard.sounding_on_any_channel().is_empty());
    }

    #[test]
    fn the_sustain_pedal_keeps_released_keys_sounding() {
        let keyboard = KeyboardState::new();
        keyboard.process(on(0, 60, 100));
        keyboard.process(cc(0, 64, 127));
        assert!(keyboard.is_sustain_down(0) && !keyboard.is_sustain_down(1));
        keyboard.process(on(0, 64, 90));
        keyboard.process(off(0, 60));
        keyboard.process(off(0, 64));
        // Released, but the pedal holds them.
        assert!(keyboard.keys_down(0).is_empty());
        assert_eq!(keys(keyboard.sounding(0)), [60, 64]);
        assert!(keyboard.is_sounding(0, 60) && !keyboard.is_down(0, 60));
        assert_eq!(keyboard.velocity(0, 64), 90);
        assert_eq!(keys(keyboard.sounding_on_any_channel()), [60, 64]);

        // Struck again under the pedal, then held while the pedal lifts:
        // the held key stays, the other ends.
        keyboard.process(on(0, 64, 70));
        assert_eq!(keys(keyboard.keys_down(0)), [64]);
        keyboard.process(cc(0, 64, 63));
        assert!(!keyboard.is_sustain_down(0));
        assert_eq!(keys(keyboard.sounding(0)), [64]);
        assert_eq!(keyboard.velocity(0, 64), 70);
        // With the pedal up a release is final.
        keyboard.process(off(0, 64));
        assert!(keyboard.sounding(0).is_empty());

        // The pedal of one channel does not hold another channel's keys.
        keyboard.process(cc(1, 64, 64));
        keyboard.process(on(2, 50, 100));
        keyboard.process(off(2, 50));
        assert!(keyboard.sounding(2).is_empty());
    }

    #[test]
    fn channel_mode_messages_clear_keys_the_way_midi_says() {
        let keyboard = KeyboardState::new();
        let press = || {
            keyboard.reset();
            keyboard.process(on(0, 60, 100));
            keyboard.process(on(0, 100, 100));
            keyboard.process(on(1, 61, 100));
        };

        // All notes off releases the keys of its channel.
        press();
        keyboard.process(cc(0, 123, 0));
        assert!(keyboard.sounding(0).is_empty());
        assert_eq!(keys(keyboard.sounding(1)), [61]);

        // Under the pedal the notes ring on until it lifts.
        press();
        keyboard.process(cc(0, 64, 127));
        keyboard.process(cc(0, 123, 0));
        assert!(keyboard.keys_down(0).is_empty());
        assert_eq!(keys(keyboard.sounding(0)), [60, 100]);
        // Reset all controllers lifts the pedal.
        keyboard.process(cc(0, 121, 0));
        assert!(keyboard.sounding(0).is_empty() && !keyboard.is_sustain_down(0));

        // All sound off does not wait for the pedal.
        press();
        keyboard.process(cc(0, 64, 127));
        keyboard.process(off(0, 60));
        keyboard.process(cc(0, 120, 0));
        assert!(keyboard.sounding(0).is_empty());
        assert!(keyboard.is_sustain_down(0));
        assert_eq!(keys(keyboard.sounding(1)), [61]);

        // Omni off, omni on, mono on and poly on each include "all notes
        // off" (MIDI 1.0 Detailed Specification, channel mode messages),
        // with the same regard for the pedal.
        for mode in [124u8, 125, 126, 127] {
            press();
            keyboard.process(cc(0, mode, 0));
            assert!(keyboard.sounding(0).is_empty(), "controller {mode}");
            assert_eq!(keys(keyboard.sounding(1)), [61], "controller {mode}");

            press();
            keyboard.process(cc(0, 64, 127));
            keyboard.process(cc(0, mode, 0));
            assert!(keyboard.keys_down(0).is_empty(), "controller {mode}");
            assert_eq!(keys(keyboard.sounding(0)), [60, 100], "controller {mode}");
            keyboard.process(cc(0, 64, 0));
            assert!(keyboard.sounding(0).is_empty(), "controller {mode}");
        }

        // Other messages change nothing.
        press();
        keyboard.process(cc(0, 120, 0));
        for message in [
            cc(1, 7, 0),
            cc(1, 122, 0),
            MidiMessage::PitchBend {
                channel: 1,
                value: 100,
            },
            MidiMessage::ProgramChange {
                channel: 1,
                program: 2,
            },
        ] {
            keyboard.process(message);
        }
        assert_eq!(keys(keyboard.sounding(1)), [61]);
        keyboard.reset();
        assert!(keyboard.sounding_on_any_channel().is_empty());
        assert!(!keyboard.is_sustain_down(0));
    }

    #[test]
    fn key_sets_behave_like_sets_of_note_numbers() {
        let mut set = KeySet::new();
        assert!(set.is_empty());
        assert_eq!((set.lowest(), set.highest()), (None, None));
        for note in [127, 0, 64, 31, 32, 96, 64] {
            set.insert(note);
        }
        assert_eq!(set.len(), 6);
        assert_eq!(keys(set), [0, 31, 32, 64, 96, 127]);
        assert_eq!(set.iter().len(), 6);
        assert_eq!(set.into_iter().last(), Some(127));
        assert_eq!((set.lowest(), set.highest()), (Some(0), Some(127)));
        set.remove(0);
        set.remove(127);
        set.remove(5);
        assert_eq!((set.lowest(), set.highest()), (Some(31), Some(96)));
        assert!(!set.contains(200));
        let mut other = KeySet::default();
        other.insert(1);
        assert_eq!(keys(set.union(other)), [1, 31, 32, 64, 96]);
        // Every single key round-trips through the eight words, held and
        // kept by the pedal.
        for note in 0..128u8 {
            let keyboard = KeyboardState::new();
            keyboard.note_on(9, note, 1);
            assert_eq!(keys(keyboard.keys_down(9)), [note]);
            assert_eq!(keys(keyboard.sounding(9)), [note]);
            keyboard.set_sustain(9, true);
            keyboard.note_off(9, note);
            assert!(keyboard.keys_down(9).is_empty());
            assert_eq!(keys(keyboard.sounding(9)), [note]);
            assert!(keyboard.is_sounding(9, note) && !keyboard.is_down(9, note));
        }
    }

    #[test]
    fn the_bits_of_a_word_are_gathered_key_by_key() {
        // Every pattern of sixteen keys, placed in the "held" bits, in the
        // "kept" bits and in both: the plain loop and the packing agree.
        let spread = |keys: u16| -> u32 {
            (0..16).fold(0u32, |word, key| {
                word | (u32::from((keys >> key) & 1) << (2 * key))
            })
        };
        for pattern in 0..=u16::MAX {
            let held = spread(pattern);
            assert_eq!(even_bits(held), pattern);
            assert_eq!(even_bits(held << 1), 0);
            assert_eq!(even_bits(held | (held << 1)), pattern);
            let other = pattern.rotate_left(5) ^ 0xA5A5;
            let word = held | (spread(other) << 1);
            assert_eq!(even_bits(word), pattern);
            assert_eq!(even_bits(word | (word >> 1)), pattern | other);
        }
        assert_eq!(HELD, spread(0xFFFF));
        assert_eq!(KEPT, spread(0xFFFF) << 1);
    }

    #[test]
    fn debug_output_lists_the_sounding_keys() {
        let keyboard = KeyboardState::default();
        assert_eq!(format!("{keyboard:?}"), "{}");
        keyboard.note_on(2, 60, 1);
        keyboard.note_on(2, 64, 1);
        assert_eq!(format!("{keyboard:?}"), "{2: [60, 64]}");
    }

    #[test]
    fn an_interface_can_read_while_another_thread_writes() {
        fn assert_shareable<T: Send + Sync>() {}
        assert_shareable::<KeyboardState>();

        let keyboard = Arc::new(KeyboardState::new());
        let done = Arc::new(AtomicBool::new(false));
        // Key 126 is down before the reader starts and stays down.
        keyboard.note_on(0, 126, 99);

        let writer = {
            let (keyboard, done) = (Arc::clone(&keyboard), Arc::clone(&done));
            std::thread::spawn(move || {
                // Only even keys are ever pressed, and never key 126.
                for round in 0..2_000u32 {
                    let note = ((round * 2) % 126) as u8;
                    keyboard.note_on(0, note, 64);
                    keyboard.set_sustain(0, round % 3 == 0);
                    keyboard.note_off(0, note);
                }
                keyboard.set_sustain(0, false);
                done.store(true, Ordering::Release);
            })
        };

        // The reader must never see a key that was not pressed, whatever
        // moment it looks at. And since each group of 16 keys is read at
        // one instant, it never sees two of the passing keys held within a
        // group, because the writer never holds two. Key 126, held
        // throughout, is there in every read.
        // The loop checks `done` after reading, so it reads at least once
        // even when the writer finishes before this thread gets going.
        let mut reads = 0u32;
        loop {
            let finished = done.load(Ordering::Acquire);
            let sounding = keyboard.sounding(0);
            assert!(sounding.iter().all(|note| note % 2 == 0), "{sounding:?}");
            assert!(sounding.contains(126), "{sounding:?}");
            let down = keyboard.keys_down(0);
            assert!(down.contains(126), "{down:?}");
            for group in 0..8u8 {
                let passing = down
                    .iter()
                    .filter(|note| note / 16 == group && *note != 126)
                    .count();
                assert!(passing <= 1, "{down:?}");
            }
            // A key that is down is sounding, in one and the same read.
            assert!(keyboard.is_down(0, 126) && keyboard.is_sounding(0, 126));
            assert_eq!(keyboard.velocity(0, 126), 99);
            reads += 1;
            if finished {
                break;
            }
        }
        writer.join().unwrap();
        assert!(reads > 0);
        // And once the writer is done, the state is exactly what it left.
        assert_eq!(keys(keyboard.sounding(0)), [126]);
        assert_eq!(keyboard.velocity(0, 126), 99);
    }

    /// Runs `writer` on another thread, over and over, on a keyboard whose
    /// channel 0 has the pedal down and keys 60 and 64 sounding, and
    /// counts the reads in which either key was missing. Every step of
    /// `writer` must leave both keys sounding, held or kept by the pedal.
    fn silent_reads(rounds: u32, writer: fn(&KeyboardState)) -> (u64, u64) {
        let keyboard = Arc::new(KeyboardState::new());
        let done = Arc::new(AtomicBool::new(false));
        let reading = Arc::new(AtomicBool::new(false));
        keyboard.set_sustain(0, true);
        keyboard.note_on(0, 60, 100);
        keyboard.note_on(0, 64, 90);
        let thread = {
            let (keyboard, done) = (Arc::clone(&keyboard), Arc::clone(&done));
            let reading = Arc::clone(&reading);
            std::thread::spawn(move || {
                // Not before the reader is looking.
                while !reading.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                for _ in 0..rounds {
                    writer(&keyboard);
                }
                done.store(true, Ordering::Release);
            })
        };
        let (mut reads, mut silent) = (0u64, 0u64);
        reading.store(true, Ordering::Release);
        while !done.load(Ordering::Acquire) {
            for note in [60u8, 64] {
                reads += 4;
                silent += u64::from(!keyboard.is_sounding(0, note));
                silent += u64::from(!keyboard.sounding(0).contains(note));
                silent += u64::from(!keyboard.sounding_on_any_channel().contains(note));
                silent += u64::from(keyboard.velocity(0, note) == 0);
            }
        }
        thread.join().unwrap();
        assert!(reads > 0);
        assert_eq!(keys(keyboard.sounding(0)), [60, 64]);
        (reads, silent)
    }

    #[test]
    fn a_key_that_goes_on_sounding_is_never_read_as_silent() {
        // Each key passes from the hand to the pedal and back to the
        // hand, and then the pedal lifts under the held keys and comes
        // down again, so that every round starts from keys that are held
        // and nothing more. Kept in two separate words, as this type once
        // kept them, a key was in neither for a moment and a reader saw
        // it silent in a sixth to a third of its reads.
        let (reads, silent) = silent_reads(200_000, |keyboard| {
            keyboard.note_off(0, 60);
            keyboard.note_on(0, 60, 100);
            keyboard.note_off(0, 64);
            keyboard.note_on(0, 64, 90);
            keyboard.set_sustain(0, false);
            keyboard.set_sustain(0, true);
        });
        assert_eq!(silent, 0, "silent in {silent} of {reads} reads");

        // The same through "all notes off", which moves a whole word of
        // keys to the pedal at once.
        let (reads, silent) = silent_reads(200_000, |keyboard| {
            keyboard.all_notes_off(0);
            keyboard.note_on(0, 64, 90);
            keyboard.note_on(0, 60, 100);
            keyboard.set_sustain(0, false);
            keyboard.set_sustain(0, true);
        });
        assert_eq!(silent, 0, "silent in {silent} of {reads} reads");
    }
}
