//! A drum kit voice whose pieces are chosen and tuned per key: acoustic
//! drums by modal synthesis, the drum machines, and kits for genres that
//! mix them.

use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use manguebyte_core::{AudioBuffer, Sample};

use super::acoustic::{Cymbal, CymbalKind, HeadKind, Heads, Hit};
use super::analog_drums::{AnalogDrum, AnalogKitParams, DrumSettings, DrumTone};
use super::common::{HANDOVER_SECONDS, SoftClip, Tone, db, setting, velocity_gain};
use super::machines::{Machine, MachineHit, MachineKind};
use crate::event::NoteRef;
use crate::{ProcessSpec, Processor, StateVariableFilter, SvfMode, Voice};

/// The General MIDI percussion keys (General MIDI 1, the percussion key
/// map of channel 10).
pub mod gm {
    /// Acoustic bass drum.
    pub const ACOUSTIC_BASS_DRUM: u8 = 35;
    /// Bass drum 1: the kick.
    pub const BASS_DRUM: u8 = 36;
    /// Side stick.
    pub const SIDE_STICK: u8 = 37;
    /// Acoustic snare.
    pub const SNARE: u8 = 38;
    /// Hand clap.
    pub const HAND_CLAP: u8 = 39;
    /// Electric snare; electronic kits and drum modules put a snare's
    /// rimshot here.
    pub const ELECTRIC_SNARE: u8 = 40;
    /// Low floor tom.
    pub const LOW_FLOOR_TOM: u8 = 41;
    /// Closed hi-hat.
    pub const CLOSED_HI_HAT: u8 = 42;
    /// High floor tom.
    pub const HIGH_FLOOR_TOM: u8 = 43;
    /// Pedal hi-hat: the hi-hat closed by the foot.
    pub const PEDAL_HI_HAT: u8 = 44;
    /// Low tom.
    pub const LOW_TOM: u8 = 45;
    /// Open hi-hat.
    pub const OPEN_HI_HAT: u8 = 46;
    /// Low-mid tom.
    pub const LOW_MID_TOM: u8 = 47;
    /// High-mid tom.
    pub const HIGH_MID_TOM: u8 = 48;
    /// Crash cymbal 1.
    pub const CRASH: u8 = 49;
    /// High tom.
    pub const HIGH_TOM: u8 = 50;
    /// Ride cymbal 1.
    pub const RIDE: u8 = 51;
    /// Chinese cymbal.
    pub const CHINA: u8 = 52;
    /// Ride bell.
    pub const RIDE_BELL: u8 = 53;
    /// Tambourine.
    pub const TAMBOURINE: u8 = 54;
    /// Splash cymbal.
    pub const SPLASH: u8 = 55;
    /// Cowbell.
    pub const COWBELL: u8 = 56;
    /// Crash cymbal 2.
    pub const CRASH_2: u8 = 57;
    /// Ride cymbal 2.
    pub const RIDE_2: u8 = 59;
    /// High bongo.
    pub const HIGH_BONGO: u8 = 60;
    /// Low bongo.
    pub const LOW_BONGO: u8 = 61;
    /// Muted high conga.
    pub const MUTE_HIGH_CONGA: u8 = 62;
    /// Open high conga.
    pub const OPEN_HIGH_CONGA: u8 = 63;
    /// Low conga.
    pub const LOW_CONGA: u8 = 64;
    /// Cabasa.
    pub const CABASA: u8 = 69;
    /// Maracas.
    pub const MARACAS: u8 = 70;
    /// Short guiro.
    pub const SHORT_GUIRO: u8 = 73;
    /// Long guiro.
    pub const LONG_GUIRO: u8 = 74;
    /// Claves.
    pub const CLAVES: u8 = 75;
}

/// What makes the sound of a piece of a [`DrumKit`].
///
/// The models without a machine in their name are acoustic drums by modal
/// synthesis; the others are drum machines, modelled on the published
/// descriptions of their circuits (see [`DrumKitVoice`]).
///
/// What a piece's `tone`, `snap` and `texture` do depends on its model:
///
/// | Model | `tone` | `snap` | `texture` |
/// |---|---|---|---|
/// | `Kick` | the beater, felt to plastic | its click | how open the front head is |
/// | `Snare`, `SnareRimshot` | where it is struck, centre to edge | the wires | how freely it rings; for the rimshot, the rim's ring |
/// | `SideStick` | the stick's brightness | the wires | the shell's knock |
/// | `Tom`, `Conga`, `Bongo` | where it is struck | the stick's or hand's attack | how freely it rings |
/// | cymbals and hi-hats | the stick, shoulder to tip | the shimmer: how far a strike's energy spreads upwards | the ride's bell under the bow; how open the open hi-hat is |
/// | `Kick909` | the attack (click) | the depth of the pitch sweep | the rumble |
/// | other machine drums | the colour, as their knob of that name | the snappy, accent or sweep | the guiro's length |
/// | `Analog` | as [`DrumSettings::tone`] | as [`DrumSettings::snap`] | - |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrumModel {
    /// An acoustic bass drum: two heads coupled by the air in the shell,
    /// struck with a beater.
    Kick,
    /// An acoustic snare drum, with its wires.
    Snare,
    /// The snare struck on the head and the rim at once.
    SnareRimshot,
    /// The stick laid on the snare head and its butt struck on the rim.
    SideStick,
    /// An acoustic tom; tune it per key.
    Tom,
    /// A conga, struck with the hand.
    Conga,
    /// A bongo.
    Bongo,
    /// The hi-hat struck closed.
    ClosedHat,
    /// The hi-hat closed with the foot.
    PedalHat,
    /// The hi-hat struck open.
    OpenHat,
    /// A crash cymbal.
    Crash,
    /// A ride cymbal, struck on the bow.
    Ride,
    /// The ride struck on its bell.
    RideBell,
    /// A splash cymbal.
    Splash,
    /// A china cymbal.
    China,
    /// The 909's bass drum: an oscillator swept down in pitch, a click and
    /// a breath of noise, and a rumble if asked.
    Kick909,
    /// The 909's snare: two oscillators and filtered noise.
    Snare909,
    /// The 909's hand clap: noise bursts and a tail.
    Clap909,
    /// The 909's tom; tune it per key.
    Tom909,
    /// The 909's rimshot.
    Rim909,
    /// The 909's closed hi-hat, as six-bit metal.
    ClosedHat909,
    /// The 909's open hi-hat.
    OpenHat909,
    /// The 909's crash.
    Crash909,
    /// The 909's ride.
    Ride909,
    /// The 606's bass drum: short and tight.
    Kick606,
    /// The 606's snare: thin and snappy.
    Snare606,
    /// The 606's tom.
    Tom606,
    /// The 606's closed hi-hat.
    ClosedHat606,
    /// The 606's open hi-hat.
    OpenHat606,
    /// The 606's cymbal.
    Cymbal606,
    /// The CR-78's bass drum: soft and round.
    KickCr78,
    /// The CR-78's snare.
    SnareCr78,
    /// The CR-78's rimshot.
    RimCr78,
    /// The CR-78's hi-hat.
    HatCr78,
    /// The CR-78's hi-hat, left open.
    OpenHatCr78,
    /// The CR-78's cymbal.
    CymbalCr78,
    /// The CR-78's cowbell.
    CowbellCr78,
    /// The CR-78's guiro.
    GuiroCr78,
    /// The CR-78's maracas.
    MaracasCr78,
    /// The CR-78's tambourine.
    TambourineCr78,
    /// The CR-78's bongo; tune it per key.
    BongoCr78,
    /// The CR-78's conga.
    CongaCr78,
    /// The CR-78's claves.
    ClavesCr78,
    /// A drum of the 808, as [`AnalogKitVoice`](super::AnalogKitVoice)
    /// plays it.
    Analog(AnalogDrum),
}

/// Which engine plays a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Heads(HeadKind),
    Cymbal(CymbalKind),
    Machine(MachineKind),
    Analog(AnalogDrum),
}

impl DrumModel {
    fn engine(self) -> Engine {
        use DrumModel as M;
        match self {
            M::Kick => Engine::Heads(HeadKind::Kick),
            M::Snare => Engine::Heads(HeadKind::Snare),
            M::SnareRimshot => Engine::Heads(HeadKind::Rimshot),
            M::SideStick => Engine::Heads(HeadKind::SideStick),
            M::Tom => Engine::Heads(HeadKind::Tom),
            M::Conga => Engine::Heads(HeadKind::Conga),
            M::Bongo => Engine::Heads(HeadKind::Bongo),
            M::ClosedHat => Engine::Cymbal(CymbalKind::ClosedHat),
            M::PedalHat => Engine::Cymbal(CymbalKind::PedalHat),
            M::OpenHat => Engine::Cymbal(CymbalKind::OpenHat),
            M::Crash => Engine::Cymbal(CymbalKind::Crash),
            M::Ride => Engine::Cymbal(CymbalKind::Ride),
            M::RideBell => Engine::Cymbal(CymbalKind::RideBell),
            M::Splash => Engine::Cymbal(CymbalKind::Splash),
            M::China => Engine::Cymbal(CymbalKind::China),
            M::Kick909 => Engine::Machine(MachineKind::Kick909),
            M::Snare909 => Engine::Machine(MachineKind::Snare909),
            M::Clap909 => Engine::Machine(MachineKind::Clap909),
            M::Tom909 => Engine::Machine(MachineKind::Tom909),
            M::Rim909 => Engine::Machine(MachineKind::Rim909),
            M::ClosedHat909 => Engine::Machine(MachineKind::ClosedHat909),
            M::OpenHat909 => Engine::Machine(MachineKind::OpenHat909),
            M::Crash909 => Engine::Machine(MachineKind::Crash909),
            M::Ride909 => Engine::Machine(MachineKind::Ride909),
            M::Kick606 => Engine::Machine(MachineKind::Kick606),
            M::Snare606 => Engine::Machine(MachineKind::Snare606),
            M::Tom606 => Engine::Machine(MachineKind::Tom606),
            M::ClosedHat606 => Engine::Machine(MachineKind::ClosedHat606),
            M::OpenHat606 => Engine::Machine(MachineKind::OpenHat606),
            M::Cymbal606 => Engine::Machine(MachineKind::Cymbal606),
            M::KickCr78 => Engine::Machine(MachineKind::KickCr78),
            M::SnareCr78 => Engine::Machine(MachineKind::SnareCr78),
            M::RimCr78 => Engine::Machine(MachineKind::RimCr78),
            M::HatCr78 => Engine::Machine(MachineKind::HatCr78),
            M::OpenHatCr78 => Engine::Machine(MachineKind::OpenHatCr78),
            M::CymbalCr78 => Engine::Machine(MachineKind::CymbalCr78),
            M::CowbellCr78 => Engine::Machine(MachineKind::CowbellCr78),
            M::GuiroCr78 => Engine::Machine(MachineKind::GuiroCr78),
            M::MaracasCr78 => Engine::Machine(MachineKind::MaracasCr78),
            M::TambourineCr78 => Engine::Machine(MachineKind::TambourineCr78),
            M::BongoCr78 => Engine::Machine(MachineKind::BongoCr78),
            M::CongaCr78 => Engine::Machine(MachineKind::CongaCr78),
            M::ClavesCr78 => Engine::Machine(MachineKind::ClavesCr78),
            M::Analog(drum) => Engine::Analog(drum),
        }
    }

    /// Whether the model is an acoustic instrument, which a strike adds to
    /// while it rings instead of starting again.
    pub fn is_acoustic(self) -> bool {
        matches!(self.engine(), Engine::Heads(_) | Engine::Cymbal(_))
    }
}

/// One piece of a [`DrumKit`]: a model and how it is set. Values out of
/// range are limited when the kit is taken; a value that is not a number
/// takes the default of [`Piece::new`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    /// What makes the sound.
    pub model: DrumModel,
    /// Level, as a gain, 0 to 4.
    pub level: f64,
    /// Tuning, in semitones, -24 to 24.
    pub tune: f64,
    /// How long it rings, as a multiple of the model's own decay times,
    /// 0.1 to 10.
    pub decay: f64,
    /// See [`DrumModel`], 0 to 1.
    pub tone: f64,
    /// See [`DrumModel`], 0 to 1.
    pub snap: f64,
    /// See [`DrumModel`], 0 to 1.
    pub texture: f64,
    /// Where it sits from left (-1) to right (1); in a stereo buffer the
    /// left and right channels are the first two.
    pub pan: f64,
    /// Its choke group, 1 to [`CHOKE_GROUPS`], or 0 for none: striking a
    /// piece of a group silences, within [`CHOKE_SECONDS`], every other
    /// piece of the group that is still sounding, as closing the hi-hat
    /// stops it ringing open.
    pub choke: u8,
}

impl Piece {
    /// A piece of `model` at its defaults: full level, in tune, its own
    /// decay, every timbre setting at the middle, in the centre, in no
    /// choke group.
    pub const fn new(model: DrumModel) -> Self {
        Self {
            model,
            level: 1.0,
            tune: 0.0,
            decay: 1.0,
            tone: 0.5,
            snap: 0.5,
            texture: 0.5,
            pan: 0.0,
            choke: 0,
        }
    }

    fn limited(self) -> Self {
        let default = Self::new(self.model);
        Self {
            model: self.model,
            level: setting(self.level, 0.0, 4.0, default.level),
            tune: setting(self.tune, -24.0, 24.0, default.tune),
            decay: setting(self.decay, 0.1, 10.0, default.decay),
            tone: setting(self.tone, 0.0, 1.0, default.tone),
            snap: setting(self.snap, 0.0, 1.0, default.snap),
            texture: setting(self.texture, 0.0, 1.0, default.texture),
            pan: setting(self.pan, -1.0, 1.0, default.pan),
            choke: self.choke.min(CHOKE_GROUPS as u8),
        }
    }
}

/// How many choke groups a kit has.
pub const CHOKE_GROUPS: usize = 8;

/// How long a choked piece takes to fall silent, in seconds: about as long
/// as a hi-hat takes to close.
pub const CHOKE_SECONDS: f64 = 0.012;

/// How long a voice's sampler rings on after its strikes end: its
/// low-pass, at 500 Hz or above, has fallen far below hearing by then.
const CRUSHER_TAIL_SECONDS: f64 = 0.01;

/// The lowest key a [`DrumKit`] maps: the acoustic bass drum of General
/// MIDI.
pub const KIT_FIRST_KEY: u8 = 35;

/// How many keys a [`DrumKit`] maps, from [`KIT_FIRST_KEY`]: up to 81, the
/// open triangle, the last of General MIDI's percussion.
pub const KIT_KEYS: usize = 47;

/// The character of an old sampler or a worn tape, put on every piece of
/// a kit: a saturator, the sample rate and word length of the sampler,
/// and its output filter. Each piece goes through it on its own, as
/// drums sampled one by one did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoFi {
    /// How hard the saturator is driven, 0 (clean) to 1.
    pub drive: f64,
    /// The sample rate it is held at, in hertz, 1 000 to 192 000: at or
    /// above the sample rate it changes nothing. Below, the sound is held
    /// from one of its samples to the next with no filter before, so it
    /// aliases, as those samplers did.
    pub rate: f64,
    /// The word length, in bits, 2 to 24: at 24 it changes nothing.
    pub bits: f64,
    /// The low-pass after it, in hertz, 500 to 24 000; at or above 0.45 of
    /// the sample rate it is left out.
    pub cutoff: f64,
}

impl LoFi {
    /// Nothing at all.
    pub const OFF: Self = Self {
        drive: 0.0,
        rate: 192_000.0,
        bits: 24.0,
        cutoff: 24_000.0,
    };

    fn limited(self) -> Self {
        Self {
            drive: setting(self.drive, 0.0, 1.0, 0.0),
            rate: setting(self.rate, 1_000.0, 192_000.0, 192_000.0),
            bits: setting(self.bits, 2.0, 24.0, 24.0),
            cutoff: setting(self.cutoff, 500.0, 24_000.0, 24_000.0),
        }
    }
}

impl Default for LoFi {
    fn default() -> Self {
        Self::OFF
    }
}

/// The settings of a [`DrumKitVoice`]: a piece for each key, and what is
/// common to them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrumKit {
    /// The piece each key plays, from [`KIT_FIRST_KEY`]; `None` is silent.
    pub pieces: [Option<Piece>; KIT_KEYS],
    /// How much quieter the lowest velocity is than the highest, in
    /// decibels, 0 to 60. Velocity also strikes harder: an acoustic piece
    /// is brighter, a machine's snap stronger.
    pub velocity_range_db: f64,
    /// How much louder an accented strike is, in decibels, 0 to 24. An
    /// accent also strikes harder.
    pub accent_db: f64,
    /// The output level, as a gain, 0 to 4.
    pub level: f64,
    /// The sampler's character on every piece.
    pub lo_fi: LoFi,
}

impl Default for DrumKit {
    /// The acoustic kit.
    fn default() -> Self {
        Self::acoustic()
    }
}

impl DrumKit {
    /// A kit with no pieces.
    pub const fn empty() -> Self {
        Self {
            pieces: [None; KIT_KEYS],
            velocity_range_db: 30.0,
            accent_db: 4.0,
            level: 1.0,
            lo_fi: LoFi::OFF,
        }
    }

    /// The piece `key` plays, if any.
    pub fn piece(&self, key: u8) -> Option<&Piece> {
        let index = usize::from(key.checked_sub(KIT_FIRST_KEY)?);
        self.pieces.get(index)?.as_ref()
    }

    /// Puts `piece` on `key`, or takes the key's piece away. A key outside
    /// the kit's range is ignored.
    pub fn set(&mut self, key: u8, piece: Option<Piece>) {
        if let Some(slot) = key
            .checked_sub(KIT_FIRST_KEY)
            .and_then(|index| self.pieces.get_mut(usize::from(index)))
        {
            *slot = piece;
        }
    }

    /// Puts `piece` on every key of `keys`.
    fn with(mut self, keys: &[u8], piece: Piece) -> Self {
        for &key in keys {
            self.set(key, Some(piece));
        }
        self
    }

    /// The keys that play a piece, in increasing order.
    pub fn keys(&self) -> impl Iterator<Item = u8> + '_ {
        (0..KIT_KEYS as u8)
            .filter(|&index| self.pieces[usize::from(index)].is_some())
            .map(|index| index + KIT_FIRST_KEY)
    }

    fn limited(self) -> Self {
        Self {
            pieces: self.pieces.map(|piece| piece.map(Piece::limited)),
            velocity_range_db: setting(self.velocity_range_db, 0.0, 60.0, 30.0),
            accent_db: setting(self.accent_db, 0.0, 24.0, 4.0),
            level: setting(self.level, 0.0, 4.0, 1.0),
            lo_fi: self.lo_fi.limited(),
        }
    }
}

/// The choke groups of one kit: striking a piece of a group chokes the
/// others of its group, on any voice that shares these groups.
///
/// Give every voice of a kit a clone of the same groups, or build them with
/// [`DrumKitVoice::voices`]. Each group is one atomic counter of strikes,
/// so the voices stay `Send` and nothing locks.
#[derive(Debug, Clone)]
pub struct ChokeGroups(Arc<[AtomicU32; CHOKE_GROUPS]>);

impl ChokeGroups {
    /// Groups nothing has been struck in. Allocates.
    pub fn new() -> Self {
        Self(Arc::new(core::array::from_fn(|_| AtomicU32::new(0))))
    }

    fn strikes(&self, group: u8) -> u32 {
        match group {
            0 => 0,
            group => self.0[usize::from(group - 1).min(CHOKE_GROUPS - 1)].load(Ordering::Relaxed),
        }
    }

    /// Counts a strike in `group` and returns the new count.
    fn strike(&self, group: u8) -> u32 {
        match group {
            0 => 0,
            group => self.0[usize::from(group - 1).min(CHOKE_GROUPS - 1)]
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1),
        }
    }
}

impl Default for ChokeGroups {
    fn default() -> Self {
        Self::new()
    }
}

/// One strike of a piece, by whichever engine plays it.
#[derive(Debug, Clone)]
struct KitTone {
    engine: Option<Engine>,
    key: u8,
    heads: Heads,
    cymbal: Cymbal,
    machine: Machine,
    analog: DrumTone,
    /// The level and the pan of the piece.
    gain: f64,
    left: f64,
    right: f64,
    /// The choke group, the count it was struck at, and the fade once
    /// choked.
    group: u8,
    strikes: u32,
    choke: f64,
    choke_step: f64,
}

impl KitTone {
    fn new(rate: f64) -> Self {
        let mut analog = DrumTone::new();
        analog.prepare(rate);
        Self {
            engine: None,
            key: 0,
            heads: Heads::new(rate),
            cymbal: Cymbal::new(rate),
            machine: Machine::new(rate),
            analog,
            gain: 0.0,
            left: 1.0,
            right: 1.0,
            group: 0,
            strikes: 0,
            choke: 1.0,
            choke_step: 1.0 / (CHOKE_SECONDS * rate).max(1.0),
        }
    }

    #[inline]
    fn next(&mut self) -> f64 {
        let value = match self.engine {
            Some(Engine::Heads(_)) => self.heads.next(),
            Some(Engine::Cymbal(_)) => self.cymbal.next(),
            Some(Engine::Machine(_)) => self.machine.next(),
            Some(Engine::Analog(_)) => self.analog.next(),
            None => 0.0,
        };
        let mut value = value * self.gain;
        if self.choke < 1.0 {
            value *= self.choke;
            self.choke -= self.choke_step;
            if self.choke <= 0.0 {
                self.silence();
            }
        }
        value
    }

    /// Starts the choke's fade if the group was struck since this tone.
    fn check_choke(&mut self, groups: &ChokeGroups) {
        if self.group != 0
            && self.choke >= 1.0
            && self.is_sounding()
            && groups.strikes(self.group) != self.strikes
        {
            self.choke = 1.0 - self.choke_step;
        }
    }
}

impl Tone for KitTone {
    fn is_sounding(&self) -> bool {
        match self.engine {
            Some(Engine::Heads(_)) => self.heads.is_sounding(),
            Some(Engine::Cymbal(_)) => self.cymbal.is_sounding(),
            Some(Engine::Machine(_)) => self.machine.is_sounding(),
            Some(Engine::Analog(_)) => self.analog.is_sounding(),
            None => false,
        }
    }

    fn silence(&mut self) {
        match self.engine {
            Some(Engine::Heads(_)) => self.heads.silence(),
            Some(Engine::Cymbal(_)) => self.cymbal.silence(),
            Some(Engine::Machine(_)) => self.machine.silence(),
            Some(Engine::Analog(_)) => self.analog.silence(),
            None => {}
        }
        self.engine = None;
        self.choke = 1.0;
    }
}

/// The sampler of a [`LoFi`] on one channel.
#[derive(Debug, Clone)]
struct Crusher {
    held: f64,
    phase: f64,
    clip: SoftClip,
    low: StateVariableFilter<f64>,
}

impl Crusher {
    fn new(rate: f64) -> Self {
        let mut low = StateVariableFilter::new(SvfMode::Lowpass, 20_000.0, 0.707);
        Processor::<f64>::prepare(
            &mut low,
            &ProcessSpec {
                sample_rate: rate,
                max_block_size: 1,
                num_channels: 1,
            },
        );
        Self {
            held: 0.0,
            phase: 1.0,
            clip: SoftClip::default(),
            low,
        }
    }

    #[inline]
    fn next(&mut self, x: f64, lo_fi: &LoFi, hold: f64, step: f64, filtered: bool) -> f64 {
        let mut x = x;
        if lo_fi.drive > 0.0 {
            // Small signals pass at their level; loud ones flatten towards
            // `1 / drive`.
            let drive = 1.0 + 5.0 * lo_fi.drive;
            x = self.clip.next(drive * x) / drive;
        }
        if hold < 1.0 {
            self.phase += hold;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.held = x;
            }
            x = self.held;
        }
        if step > 0.0 {
            x = (x / step).round() * step;
        }
        if filtered {
            x = self.low.process_sample(0, x);
        }
        x
    }

    fn reset(&mut self) {
        self.held = 0.0;
        self.phase = 1.0;
        self.clip.reset();
        Processor::<f64>::reset(&mut self.low);
    }
}

/// A drum kit: a voice for [`Synth`](crate::Synth) that plays the piece
/// its [`DrumKit`] puts on the key struck, from acoustic drums by modal
/// synthesis to the drum machines.
///
/// **Acoustic pieces.** The drums with heads ring in the modes of a
/// circular membrane (the zeros of the Bessel functions), each taking the
/// strike by its shape where the stick lands; the two heads of a drum are
/// coupled by the air between them, so every mode becomes a close pair; a
/// head struck hard starts sharp and settles; the snare's wires buzz with
/// the snare head's motion and ring on after the heads. The cymbals ring in
/// hundreds of modes as dense as a plate's, each decaying at the rate a
/// real crash's do at its frequency, and a cascade carries a strike's
/// energy from their low modes to their high ones over a few tenths of a
/// second: the shimmer of a cymbal, stronger the harder it is struck. The
/// snare and the crash are calibrated against recordings of real ones. An
/// acoustic piece struck again while it rings adds the new strike to its
/// ringing, as the instrument does; the stick lands a little differently
/// each time.
///
/// **Drum machines.** The 909's, 606's and CR-78's drums are recipes of
/// the parts their circuits are made of: swept oscillators and struck
/// resonators, the trigger's click, filtered noise in envelopes and bursts,
/// and six square waves of metal. The 909's hi-hats and cymbals were
/// six-bit samples; here they are metal quantised to six bits. The 808's
/// drums are [`AnalogKitVoice`](super::AnalogKitVoice)'s. A machine's drum
/// struck again starts again, as its envelope is retriggered.
///
/// Velocity sets the level (over [`DrumKit::velocity_range_db`]) and how
/// hard a piece is struck; an accent is louder by
/// [`DrumKit::accent_db`] and harder. Pieces are one-shots: a note off
/// changes nothing, and a key with no piece is silent. Striking a piece of
/// a [choke group](Piece::choke) silences the others of its group on every
/// voice sharing the [`ChokeGroups`]. A voice taken over for another key
/// fades its last strike out over
/// [`HANDOVER_SECONDS`](super::HANDOVER_SECONDS) while the new one starts.
///
/// **Cost.** A cymbal is some 450 resonators, a drum with heads about 80;
/// each part that has fallen silent is skipped. Measured on one core of a
/// desktop processor at 48 kHz, in a release build, a voice ringing the
/// whole time takes about 3.5 % for a crash, ride or china, 2.3 % for an
/// open hi-hat, 1.3 % for a snare or a tom, 0.5 % for a kick and under 1 %
/// for any drum of the machines. The `drum_kit_wavs` example measures them
/// again.
///
/// Every random choice is seeded by the note and by how many times it has
/// been struck again while it rang, so the same notes sound the same every
/// time, and a roll on one drum does not repeat itself stroke for stroke.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::event::NoteRef;
/// use manguebyte_dsp::instruments::{DrumKit, DrumKitVoice, gm};
/// use manguebyte_dsp::{ProcessSpec, Processor, Synth};
///
/// let mut kit = Synth::new(DrumKitVoice::voices(16, DrumKit::style_909()));
/// kit.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 });
/// kit.note_on_accented(NoteRef::new(9, gm::BASS_DRUM), 1.0, true);
/// kit.note_on(NoteRef::new(9, gm::CLOSED_HI_HAT), 0.6);
/// let mut block = AudioBuffer::<f32>::new(2, 256);
/// kit.process(&mut block);
/// assert!(block.peak() > 0.0);
/// ```
#[derive(Debug, Clone)]
pub struct DrumKitVoice {
    kit: DrumKit,
    groups: ChokeGroups,
    rate: f64,
    now: KitTone,
    old: KitTone,
    fade: f64,
    fade_step: f64,
    /// Strikes of the piece on this voice since its fresh note: they seed
    /// the next.
    strikes: u32,
    crushers: [Crusher; 2],
    /// Samples the sampler still rings for after the strikes have ended
    /// (its held sample and its low-pass), and how many it rings for.
    tail: u32,
    tail_len: u32,
}

impl DrumKitVoice {
    /// A voice with choke groups of its own. Use
    /// [`with_groups`](Self::with_groups) or [`voices`](Self::voices) for a
    /// whole kit. Allocates.
    pub fn new(kit: DrumKit) -> Self {
        Self::with_groups(kit, &ChokeGroups::new())
    }

    /// A voice in the given choke groups. Every voice of one kit should
    /// share them. Allocates.
    pub fn with_groups(kit: DrumKit, groups: &ChokeGroups) -> Self {
        let rate = 48_000.0;
        Self {
            kit: kit.limited(),
            groups: groups.clone(),
            rate,
            now: KitTone::new(rate),
            old: KitTone::new(rate),
            fade: 0.0,
            fade_step: 1.0,
            strikes: 0,
            crushers: [Crusher::new(rate), Crusher::new(rate)],
            tail: 0,
            tail_len: (CRUSHER_TAIL_SECONDS * rate) as u32,
        }
    }

    /// `count` voices sharing new choke groups, for a
    /// [`Synth`](crate::Synth). Allocates.
    pub fn voices(count: usize, kit: DrumKit) -> Vec<Self> {
        let groups = ChokeGroups::new();
        (0..count)
            .map(|_| Self::with_groups(kit, &groups))
            .collect()
    }

    /// Takes new settings, limited to their ranges. Real-time safe. They
    /// apply from the next strike; the sampler's character at once.
    pub fn set_params(&mut self, kit: DrumKit) {
        self.kit = kit.limited();
    }

    /// The settings, as limited.
    pub fn params(&self) -> &DrumKit {
        &self.kit
    }

    fn is_sounding(&self) -> bool {
        self.now.is_sounding() || self.fade > 0.0 || self.tail > 0
    }

    /// Strikes `piece` on the current tone.
    fn strike(&mut self, key: u8, piece: &Piece, velocity: f64, accent: bool) {
        let kit = &self.kit;
        let mut amplitude =
            piece.level * kit.level * velocity_gain(velocity, kit.velocity_range_db);
        let mut hardness = velocity;
        if accent {
            amplitude *= db(kit.accent_db);
            hardness += 0.3;
        }
        self.strikes = self.strikes.wrapping_add(1);
        let seed = u64::from(key)
            ^ velocity.to_bits().rotate_left(13)
            ^ u64::from(self.strikes).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ u64::from(accent) << 40;
        let tune = (piece.tune / 12.0).exp2();
        let engine = piece.model.engine();
        let tone = &mut self.now;
        tone.key = key;
        tone.gain = 1.0;
        let angle = (piece.pan + 1.0) * core::f64::consts::FRAC_PI_4;
        tone.left = core::f64::consts::SQRT_2 * angle.cos();
        tone.right = core::f64::consts::SQRT_2 * angle.sin();
        tone.group = piece.choke;
        tone.strikes = self.groups.strike(piece.choke);
        tone.choke = 1.0;
        match engine {
            Engine::Heads(kind) => {
                tone.heads.strike(
                    kind,
                    &Hit {
                        amplitude,
                        hardness,
                        tune,
                        decay: piece.decay,
                        tone: piece.tone,
                        snap: piece.snap,
                        texture: piece.texture,
                        seed,
                    },
                );
            }
            Engine::Cymbal(kind) => {
                tone.cymbal.strike(
                    kind,
                    &Hit {
                        amplitude,
                        hardness,
                        tune,
                        decay: piece.decay,
                        tone: piece.tone,
                        snap: piece.snap,
                        texture: piece.texture,
                        seed,
                    },
                );
            }
            Engine::Machine(kind) => {
                let mut snap = piece.snap * (0.5 + 0.5 * velocity);
                if accent {
                    snap = (snap + 0.25).min(1.0);
                }
                // A machine's noise is the same at every strike.
                let seed = u64::from(key) ^ velocity.to_bits().rotate_left(13);
                tone.machine.strike(
                    kind,
                    &MachineHit {
                        amplitude,
                        tune,
                        decay: piece.decay,
                        tone: piece.tone,
                        snap,
                        texture: piece.texture,
                        accent,
                    },
                    seed,
                );
            }
            Engine::Analog(drum) => {
                // The 808's drums at full level: the kit sets the level.
                let mut params = AnalogKitParams {
                    velocity_range_db: kit.velocity_range_db,
                    accent_db: kit.accent_db,
                    level: 1.0,
                    ..AnalogKitParams::default()
                };
                *params.drum_mut(drum) = DrumSettings {
                    level: 1.0,
                    tune: piece.tune,
                    decay: piece.decay,
                    tone: piece.tone,
                    snap: piece.snap,
                };
                tone.analog.strike(drum, &params, velocity, accent, 0);
                tone.gain = piece.level * kit.level;
            }
        }
        tone.engine = Some(engine);
    }

    /// The left and right samples of the two tones, through the sampler.
    #[inline]
    fn frame(&mut self, lo_fi: &LoFi, hold: f64, step: f64, filtered: bool) -> (f64, f64) {
        let (mut left, mut right) = (0.0, 0.0);
        if self.now.is_sounding() {
            let value = self.now.next();
            left += value * self.now.left;
            right += value * self.now.right;
        }
        if self.fade > 0.0 {
            let value = self.old.next() * self.fade;
            left += value * self.old.left;
            right += value * self.old.right;
            self.fade -= self.fade_step;
            if self.fade <= 0.0 || !self.old.is_sounding() {
                self.fade = 0.0;
                self.old.silence();
            }
        }
        if hold < 1.0 || step > 0.0 || filtered || lo_fi.drive > 0.0 {
            // The sampler rings on a little after the strikes: it is
            // rendered, sample by sample, until its tail is over, whatever
            // the blocks.
            if self.now.is_sounding() || self.fade > 0.0 {
                self.tail = self.tail_len;
            } else {
                self.tail = self.tail.saturating_sub(1);
            }
            left = self.crushers[0].next(left, lo_fi, hold, step, filtered);
            right = self.crushers[1].next(right, lo_fi, hold, step, filtered);
        } else {
            self.tail = 0;
        }
        (left, right)
    }
}

impl<S: Sample> Voice<S> for DrumKitVoice {
    fn prepare(&mut self, spec: &ProcessSpec) {
        let rate = spec.sample_rate;
        self.rate = rate;
        self.now = KitTone::new(rate);
        self.old = KitTone::new(rate);
        self.fade = 0.0;
        self.fade_step = 1.0 / (HANDOVER_SECONDS * rate).max(1.0);
        self.strikes = 0;
        self.crushers = [Crusher::new(rate), Crusher::new(rate)];
        self.tail = 0;
        self.tail_len = (CRUSHER_TAIL_SECONDS * rate) as u32;
    }

    fn note_on(&mut self, note: NoteRef, velocity: f32) {
        Voice::<S>::note_on_accented(self, note, velocity, false);
    }

    fn note_on_accented(&mut self, note: NoteRef, velocity: f32, accent: bool) {
        let Some(&piece) = self.kit.piece(note.key) else {
            return;
        };
        let velocity = f64::from(velocity).clamp(0.0, 1.0);
        let same = self.now.is_sounding()
            && self.now.key == note.key
            && self.now.engine == Some(piece.model.engine())
            && self.now.choke >= 1.0;
        if self.now.is_sounding() && !(same && piece.model.is_acoustic()) {
            // Struck again, or another piece: the last strike fades out.
            self.choke_now();
        }
        self.strike(note.key, &piece, velocity, accent);
    }

    fn note_off(&mut self, _velocity: f32) {}

    fn choke(&mut self) {
        // A fresh note: its strikes are counted from one again.
        self.strikes = 0;
        if self.now.is_sounding() {
            self.choke_now();
        } else {
            self.old.silence();
            self.now.silence();
            self.fade = 0.0;
            self.strikes = 0;
            self.tail = 0;
            for crusher in &mut self.crushers {
                crusher.reset();
            }
        }
    }

    fn is_active(&self) -> bool {
        self.is_sounding()
    }

    fn render(&mut self, buffer: &mut AudioBuffer<S>) {
        self.now.check_choke(&self.groups);
        self.old.check_choke(&self.groups);
        if !self.is_sounding() {
            return;
        }
        let lo_fi = self.kit.lo_fi;
        let hold = (lo_fi.rate / self.rate).min(1.0);
        let step = if lo_fi.bits < 24.0 {
            1.0 / (lo_fi.bits - 1.0).exp2()
        } else {
            0.0
        };
        let filtered = lo_fi.cutoff < 0.45 * self.rate;
        if filtered {
            for crusher in &mut self.crushers {
                if (crusher.low.frequency() - lo_fi.cutoff).abs() > 1e-9 {
                    crusher.low.set_frequency(lo_fi.cutoff);
                }
            }
        }
        let channels = buffer.num_channels();
        for frame in 0..buffer.num_frames() {
            let (left, right) = self.frame(&lo_fi, hold, step, filtered);
            match channels {
                0 => {}
                1 => buffer.channel_mut(0)[frame] += S::from_f64(0.5 * (left + right)),
                _ => {
                    buffer.channel_mut(0)[frame] += S::from_f64(left);
                    buffer.channel_mut(1)[frame] += S::from_f64(right);
                    for channel in 2..channels {
                        buffer.channel_mut(channel)[frame] += S::from_f64(0.5 * (left + right));
                    }
                }
            }
        }
    }
}

impl DrumKitVoice {
    /// Moves the sounding strike aside to fade out, and clears the tone
    /// for the next.
    fn choke_now(&mut self) {
        core::mem::swap(&mut self.now, &mut self.old);
        self.fade = 1.0;
        self.now.silence();
    }
}

/// A piece of `model` set by the fields given, the rest at their defaults.
macro_rules! piece {
    ($model:expr $(, $field:ident: $value:expr)* $(,)?) => {
        Piece {
            $($field: $value,)*
            ..Piece::new($model)
        }
    };
}

impl DrumKit {
    /// An acoustic kit by modal synthesis, as seen from the drummer's
    /// seat: kick, snare with its rimshot (on 40) and side stick, six toms
    /// from the low floor tom to the high tom, the hi-hat closed, pedalled
    /// and open (one choke group), two crashes, two rides and the bell,
    /// china and splash; and congas and bongos. The clap, tambourine,
    /// cowbell, maracas, guiro and claves are the CR-78's and the 909's.
    pub fn acoustic() -> Self {
        use DrumModel as M;
        use gm::*;
        let hat = |model| piece!(model, pan: -0.4, choke: 1);
        let tom = |tune, pan| piece!(M::Tom, tune: tune, pan: pan);
        Self {
            level: 0.9,
            ..Self::empty()
        }
        .with(
            &[ACOUSTIC_BASS_DRUM],
            piece!(M::Kick, tune: -2.0, texture: 0.6),
        )
        .with(&[BASS_DRUM], piece!(M::Kick))
        .with(&[SIDE_STICK], piece!(M::SideStick, pan: -0.1))
        .with(&[SNARE], piece!(M::Snare, pan: -0.1))
        .with(&[ELECTRIC_SNARE], piece!(M::SnareRimshot, pan: -0.1))
        .with(
            &[HAND_CLAP],
            piece!(M::Clap909, level: 1.6, tone: 0.3, decay: 0.8),
        )
        .with(&[LOW_FLOOR_TOM], tom(-5.0, 0.45))
        .with(&[HIGH_FLOOR_TOM], tom(-2.0, 0.35))
        .with(&[LOW_TOM], tom(2.0, 0.1))
        .with(&[LOW_MID_TOM], tom(4.0, -0.05))
        .with(&[HIGH_MID_TOM], tom(6.0, -0.2))
        .with(&[HIGH_TOM], tom(9.0, -0.3))
        .with(&[CLOSED_HI_HAT], hat(M::ClosedHat))
        .with(&[PEDAL_HI_HAT], hat(M::PedalHat))
        .with(&[OPEN_HI_HAT], hat(M::OpenHat))
        .with(&[CRASH], piece!(M::Crash, pan: -0.3))
        .with(&[CRASH_2], piece!(M::Crash, tune: 1.5, pan: 0.5))
        .with(&[RIDE], piece!(M::Ride, pan: 0.45))
        .with(&[RIDE_2], piece!(M::Ride, tune: -1.0, pan: 0.5))
        .with(&[RIDE_BELL], piece!(M::RideBell, pan: 0.45))
        .with(&[CHINA], piece!(M::China, pan: 0.6))
        .with(&[SPLASH], piece!(M::Splash, pan: -0.5))
        .with(&[TAMBOURINE], piece!(M::TambourineCr78, pan: 0.3))
        .with(&[COWBELL], piece!(M::CowbellCr78, pan: 0.2))
        .with(&[HIGH_BONGO], piece!(M::Bongo, pan: 0.2))
        .with(&[LOW_BONGO], piece!(M::Bongo, tune: -5.0, pan: 0.25))
        .with(
            &[MUTE_HIGH_CONGA],
            piece!(M::Conga, tune: 4.0, decay: 0.3, pan: -0.2),
        )
        .with(&[OPEN_HIGH_CONGA], piece!(M::Conga, tune: 4.0, pan: -0.2))
        .with(&[LOW_CONGA], piece!(M::Conga, pan: -0.25))
        .with(&[CABASA, MARACAS], piece!(M::MaracasCr78, pan: 0.3))
        .with(&[SHORT_GUIRO], piece!(M::GuiroCr78, texture: 0.0, pan: 0.3))
        .with(&[LONG_GUIRO], piece!(M::GuiroCr78, texture: 1.0, pan: 0.3))
        .with(&[CLAVES], piece!(M::ClavesCr78, pan: 0.2))
    }

    /// The 808's sixteen drums on the keys
    /// [`AnalogDrum::from_key`] gives them, the hi-hats in one choke group.
    pub fn style_808() -> Self {
        let mut kit = Self {
            level: 0.5,
            ..Self::empty()
        };
        for key in KIT_FIRST_KEY..KIT_FIRST_KEY + KIT_KEYS as u8 {
            if let Some(drum) = AnalogDrum::from_key(key) {
                let choke = if matches!(drum, AnalogDrum::ClosedHat | AnalogDrum::OpenHat) {
                    1
                } else {
                    0
                };
                kit.set(key, Some(piece!(DrumModel::Analog(drum), choke: choke)));
            }
        }
        kit
    }

    /// The 909's eleven drums: kick, snare, clap, rimshot, three toms, the
    /// hi-hat closed and open (one choke group), crash and ride.
    pub fn style_909() -> Self {
        use DrumModel as M;
        use gm::*;
        Self::empty()
            .with(&[ACOUSTIC_BASS_DRUM, BASS_DRUM], piece!(M::Kick909))
            .with(&[SNARE, ELECTRIC_SNARE], piece!(M::Snare909))
            .with(&[HAND_CLAP], piece!(M::Clap909))
            .with(&[SIDE_STICK], piece!(M::Rim909))
            .with(
                &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM],
                piece!(M::Tom909, tune: -5.0),
            )
            .with(&[LOW_TOM, LOW_MID_TOM], piece!(M::Tom909))
            .with(&[HIGH_MID_TOM, HIGH_TOM], piece!(M::Tom909, tune: 5.0))
            .with(
                &[CLOSED_HI_HAT, PEDAL_HI_HAT],
                piece!(M::ClosedHat909, choke: 1),
            )
            .with(&[OPEN_HI_HAT], piece!(M::OpenHat909, choke: 1))
            .with(&[CRASH, CRASH_2, SPLASH, CHINA], piece!(M::Crash909))
            .with(&[RIDE, RIDE_2, RIDE_BELL], piece!(M::Ride909))
    }

    /// The 606's seven drums: kick, snare, two toms, the hi-hat closed and
    /// open (one choke group), and cymbal.
    pub fn style_606() -> Self {
        use DrumModel as M;
        use gm::*;
        Self::empty()
            .with(&[ACOUSTIC_BASS_DRUM, BASS_DRUM], piece!(M::Kick606))
            .with(&[SNARE, ELECTRIC_SNARE], piece!(M::Snare606))
            .with(
                &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM, LOW_TOM],
                piece!(M::Tom606, tune: -4.0),
            )
            .with(
                &[LOW_MID_TOM, HIGH_MID_TOM, HIGH_TOM],
                piece!(M::Tom606, tune: 3.0),
            )
            .with(
                &[CLOSED_HI_HAT, PEDAL_HI_HAT],
                piece!(M::ClosedHat606, choke: 1),
            )
            .with(&[OPEN_HI_HAT], piece!(M::OpenHat606, choke: 1))
            .with(
                &[CRASH, CRASH_2, RIDE, RIDE_2, SPLASH, CHINA],
                piece!(M::Cymbal606),
            )
    }

    /// The CR-78's voices: kick, snare, rimshot, hi-hat (closed and left
    /// open, one choke group), cymbal, cowbell, short and long guiro,
    /// maracas, tambourine, high and low bongo, conga and claves.
    pub fn style_cr78() -> Self {
        use DrumModel as M;
        use gm::*;
        Self::empty()
            .with(&[ACOUSTIC_BASS_DRUM, BASS_DRUM], piece!(M::KickCr78))
            .with(&[SNARE, ELECTRIC_SNARE], piece!(M::SnareCr78))
            .with(&[SIDE_STICK], piece!(M::RimCr78))
            .with(&[CLOSED_HI_HAT, PEDAL_HI_HAT], piece!(M::HatCr78, choke: 1))
            .with(&[OPEN_HI_HAT], piece!(M::OpenHatCr78, choke: 1))
            .with(&[CRASH, CRASH_2, RIDE, RIDE_2], piece!(M::CymbalCr78))
            .with(&[COWBELL], piece!(M::CowbellCr78))
            .with(&[SHORT_GUIRO], piece!(M::GuiroCr78, texture: 0.0))
            .with(&[LONG_GUIRO], piece!(M::GuiroCr78, texture: 1.0))
            .with(&[CABASA, MARACAS], piece!(M::MaracasCr78))
            .with(&[TAMBOURINE], piece!(M::TambourineCr78))
            .with(&[HIGH_BONGO], piece!(M::BongoCr78))
            .with(&[LOW_BONGO], piece!(M::BongoCr78, tune: -6.0))
            .with(
                &[MUTE_HIGH_CONGA, OPEN_HIGH_CONGA],
                piece!(M::CongaCr78, tune: 4.0),
            )
            .with(&[LOW_CONGA], piece!(M::CongaCr78))
            .with(&[CLAVES], piece!(M::ClavesCr78))
    }

    /// Trap: the 808's long kick tuned down, a crisp 909 clap and snare,
    /// the 808's tight hi-hats for rolls and its open hat, rimshot and
    /// cowbell, and the 909's cymbals. Play the 808's slides beside it on
    /// a [`KickBassVoice`](super::KickBassVoice).
    pub fn trap() -> Self {
        use AnalogDrum as A;
        use DrumModel as M;
        use gm::*;
        let analog = |drum| piece!(M::Analog(drum), level: 0.5);
        Self::empty()
            .with(
                &[BASS_DRUM],
                piece!(M::Analog(A::Kick), level: 0.55, tune: -2.0, decay: 3.5, snap: 0.3),
            )
            .with(
                &[ACOUSTIC_BASS_DRUM],
                piece!(M::Analog(A::Kick), level: 0.5, decay: 1.2),
            )
            .with(
                &[SNARE],
                piece!(M::Snare909, tune: 2.0, decay: 0.8, tone: 0.7, snap: 0.8),
            )
            .with(
                &[HAND_CLAP, ELECTRIC_SNARE],
                piece!(M::Clap909, tone: 0.75, snap: 0.8, decay: 0.7),
            )
            .with(&[SIDE_STICK], analog(A::Rimshot))
            .with(
                &[CLOSED_HI_HAT, PEDAL_HI_HAT],
                piece!(M::Analog(A::ClosedHat), level: 0.45, decay: 0.6, tone: 0.8, choke: 1),
            )
            .with(
                &[OPEN_HI_HAT],
                piece!(M::Analog(A::OpenHat), level: 0.45, choke: 1),
            )
            .with(
                &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM],
                piece!(M::Analog(A::LowTom), level: 0.5, decay: 1.5),
            )
            .with(
                &[LOW_TOM, LOW_MID_TOM],
                piece!(M::Analog(A::MidTom), level: 0.5, decay: 1.5),
            )
            .with(
                &[HIGH_MID_TOM, HIGH_TOM],
                piece!(M::Analog(A::HighTom), level: 0.5, decay: 1.5),
            )
            .with(&[COWBELL], analog(A::Cowbell))
            .with(&[CRASH, CRASH_2], piece!(M::Crash909))
            .with(&[RIDE, RIDE_2], piece!(M::Ride909))
    }

    /// Techno: the 909's kick with a rumble under it (35 without), a
    /// punchy clap, the 909's hi-hats with a longer open one, its ride,
    /// crash, snare, rimshot and toms.
    pub fn techno() -> Self {
        use DrumModel as M;
        use gm::*;
        Self::empty()
            .with(
                &[BASS_DRUM],
                piece!(M::Kick909, decay: 1.2, snap: 0.6, texture: 0.6),
            )
            .with(
                &[ACOUSTIC_BASS_DRUM],
                piece!(M::Kick909, decay: 1.2, snap: 0.6, texture: 0.0),
            )
            .with(&[SNARE, ELECTRIC_SNARE], piece!(M::Snare909, tone: 0.4))
            .with(&[HAND_CLAP], piece!(M::Clap909, snap: 0.85, decay: 0.6))
            .with(&[SIDE_STICK], piece!(M::Rim909))
            .with(&[CLOSED_HI_HAT], piece!(M::ClosedHat909, choke: 1))
            .with(
                &[PEDAL_HI_HAT],
                piece!(M::ClosedHat909, decay: 0.6, choke: 1),
            )
            .with(&[OPEN_HI_HAT], piece!(M::OpenHat909, decay: 1.1, choke: 1))
            .with(
                &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM],
                piece!(M::Tom909, tune: -5.0),
            )
            .with(&[LOW_TOM, LOW_MID_TOM], piece!(M::Tom909))
            .with(&[HIGH_MID_TOM, HIGH_TOM], piece!(M::Tom909, tune: 5.0))
            .with(&[RIDE, RIDE_2, RIDE_BELL], piece!(M::Ride909))
            .with(&[CRASH, CRASH_2], piece!(M::Crash909))
            .with(
                &[COWBELL],
                piece!(M::Analog(AnalogDrum::Cowbell), level: 0.5),
            )
    }

    /// House: the 909's kick, a clap with a longer tail, short crisp
    /// hi-hats that swing well and a medium open one, the 909's ride, a
    /// shaker and tambourine, the 808's congas and cowbell.
    pub fn house() -> Self {
        use AnalogDrum as A;
        use DrumModel as M;
        use gm::*;
        Self::empty()
            .with(
                &[ACOUSTIC_BASS_DRUM, BASS_DRUM],
                piece!(M::Kick909, decay: 0.9, snap: 0.55, texture: 0.0),
            )
            .with(&[SNARE, ELECTRIC_SNARE], piece!(M::Snare909, tone: 0.6))
            .with(&[HAND_CLAP], piece!(M::Clap909, decay: 1.2, snap: 0.6))
            .with(&[SIDE_STICK], piece!(M::Rim909))
            .with(
                &[CLOSED_HI_HAT],
                piece!(M::ClosedHat909, decay: 0.8, tone: 0.6, choke: 1),
            )
            .with(
                &[PEDAL_HI_HAT],
                piece!(M::ClosedHat909, decay: 0.6, tone: 0.4, choke: 1),
            )
            .with(&[OPEN_HI_HAT], piece!(M::OpenHat909, decay: 0.75, choke: 1))
            .with(&[RIDE, RIDE_2, RIDE_BELL], piece!(M::Ride909))
            .with(&[CRASH, CRASH_2], piece!(M::Crash909))
            .with(
                &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM, LOW_TOM],
                piece!(M::Tom909, tune: -3.0),
            )
            .with(
                &[LOW_MID_TOM, HIGH_MID_TOM, HIGH_TOM],
                piece!(M::Tom909, tune: 4.0),
            )
            .with(
                &[CABASA, MARACAS],
                piece!(M::MaracasCr78, level: 0.8, pan: 0.3),
            )
            .with(
                &[TAMBOURINE],
                piece!(M::TambourineCr78, level: 0.7, pan: -0.3),
            )
            .with(&[COWBELL], piece!(M::Analog(A::Cowbell), level: 0.45))
            .with(
                &[MUTE_HIGH_CONGA],
                piece!(M::Analog(A::HighConga), level: 0.5, pan: 0.2),
            )
            .with(
                &[OPEN_HIGH_CONGA],
                piece!(M::Analog(A::MidConga), level: 0.5, pan: -0.2),
            )
            .with(
                &[LOW_CONGA],
                piece!(M::Analog(A::LowConga), level: 0.5, pan: -0.3),
            )
    }

    /// Drum and bass: the 808's deep kick, cut short, and the acoustic kit
    /// tight and bright on top of it, as a breakbeat would be: a snare
    /// tuned up with short, hard wires, its rimshot, tight hi-hats, ride
    /// and crash; an acoustic kick on 35 to layer.
    pub fn drum_and_bass() -> Self {
        use DrumModel as M;
        use gm::*;
        let hat = |model, decay| piece!(model, decay: decay, tone: 0.7, pan: -0.35, choke: 1);
        Self {
            level: 0.9,
            ..Self::empty()
        }
        .with(
            &[BASS_DRUM],
            piece!(M::Analog(AnalogDrum::Kick), level: 0.6, tune: -1.0, decay: 0.9, snap: 0.7),
        )
        .with(
            &[ACOUSTIC_BASS_DRUM],
            piece!(M::Kick, tune: 2.0, decay: 0.6, tone: 0.75, snap: 0.7),
        )
        .with(
            &[SNARE],
            piece!(M::Snare, tune: 5.0, decay: 0.6, tone: 0.55, snap: 0.75, texture: 0.3),
        )
        .with(
            &[ELECTRIC_SNARE],
            piece!(M::SnareRimshot, tune: 5.0, decay: 0.6, snap: 0.75),
        )
        .with(&[SIDE_STICK], piece!(M::SideStick, tune: 3.0))
        .with(
            &[HAND_CLAP],
            piece!(M::Clap909, level: 1.5, snap: 0.8, decay: 0.6),
        )
        .with(&[CLOSED_HI_HAT], hat(M::ClosedHat, 0.7))
        .with(&[PEDAL_HI_HAT], hat(M::PedalHat, 0.8))
        .with(
            &[OPEN_HI_HAT],
            piece!(M::OpenHat, texture: 0.35, pan: -0.35, choke: 1),
        )
        .with(&[RIDE, RIDE_2], piece!(M::Ride, tone: 0.7, pan: 0.4))
        .with(&[RIDE_BELL], piece!(M::RideBell, pan: 0.4))
        .with(&[CRASH, CRASH_2, SPLASH], piece!(M::Crash, pan: -0.3))
        .with(&[CHINA], piece!(M::China, pan: 0.5))
        .with(
            &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM],
            piece!(M::Tom, tune: -2.0, decay: 0.6, pan: 0.35),
        )
        .with(
            &[LOW_TOM, LOW_MID_TOM],
            piece!(M::Tom, tune: 4.0, decay: 0.6),
        )
        .with(
            &[HIGH_MID_TOM, HIGH_TOM],
            piece!(M::Tom, tune: 9.0, decay: 0.6, pan: -0.3),
        )
    }

    /// Lo-fi: a soft acoustic kit, struck gently and dark, through a
    /// twelve-bit sampler at about 26 kHz with a little saturation and a
    /// low-pass, as drums sampled into an old sampler sound: the kick
    /// round, the snare with loose wires, the side stick, dark hi-hats and
    /// ride, and the CR-78's shaker and tambourine.
    pub fn lo_fi() -> Self {
        use DrumModel as M;
        use gm::*;
        let hat = |model| piece!(model, tone: 0.2, snap: 0.3, pan: -0.3, choke: 1);
        Self {
            level: 1.4,
            velocity_range_db: 24.0,
            lo_fi: LoFi {
                drive: 0.35,
                rate: 26_040.0,
                bits: 12.0,
                cutoff: 7_000.0,
            },
            ..Self::empty()
        }
        .with(
            &[ACOUSTIC_BASS_DRUM, BASS_DRUM],
            piece!(M::Kick, tune: -1.0, tone: 0.2, snap: 0.3),
        )
        .with(
            &[SNARE, ELECTRIC_SNARE],
            piece!(M::Snare, tune: -1.0, tone: 0.35, snap: 0.6, decay: 0.9),
        )
        .with(&[SIDE_STICK], piece!(M::SideStick, tone: 0.3))
        .with(
            &[HAND_CLAP],
            piece!(M::Clap909, level: 1.5, tone: 0.2, decay: 0.8),
        )
        .with(&[CLOSED_HI_HAT], hat(M::ClosedHat))
        .with(&[PEDAL_HI_HAT], hat(M::PedalHat))
        .with(
            &[OPEN_HI_HAT],
            piece!(M::OpenHat, tone: 0.2, snap: 0.3, texture: 0.3, pan: -0.3, choke: 1),
        )
        .with(
            &[RIDE, RIDE_2, RIDE_BELL],
            piece!(M::Ride, tone: 0.2, snap: 0.3, pan: 0.4),
        )
        .with(
            &[CRASH, CRASH_2],
            piece!(M::Crash, tone: 0.2, snap: 0.35, pan: -0.3),
        )
        .with(
            &[LOW_FLOOR_TOM, HIGH_FLOOR_TOM, LOW_TOM],
            piece!(M::Tom, tune: -3.0, tone: 0.3),
        )
        .with(
            &[LOW_MID_TOM, HIGH_MID_TOM, HIGH_TOM],
            piece!(M::Tom, tune: 3.0, tone: 0.3),
        )
        .with(
            &[CABASA, MARACAS],
            piece!(M::MaracasCr78, level: 1.5, pan: 0.3),
        )
        .with(
            &[TAMBOURINE],
            piece!(M::TambourineCr78, level: 1.5, pan: 0.3),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Synth;
    use crate::fft::{Complex, RealFft};
    use crate::instruments::measure::{
        Act, RATE, accented, band_energy, band_passed, between, centroid, centroid_track,
        decay_from_peak, decibels, finite_within, frame_levels, largest_jump, loudest_at, note, on,
        peak, play, synth_of,
    };

    /// Every kit, by name.
    fn kits() -> [(&'static str, DrumKit); 10] {
        [
            ("acoustic", DrumKit::acoustic()),
            ("808", DrumKit::style_808()),
            ("909", DrumKit::style_909()),
            ("606", DrumKit::style_606()),
            ("cr78", DrumKit::style_cr78()),
            ("trap", DrumKit::trap()),
            ("techno", DrumKit::techno()),
            ("house", DrumKit::house()),
            ("drum and bass", DrumKit::drum_and_bass()),
            ("lo-fi", DrumKit::lo_fi()),
        ]
    }

    fn strike(kit: DrumKit, key: u8, velocity: f32, accent: bool, seconds: f64) -> Vec<f64> {
        let act = Act::On {
            key,
            velocity,
            accent,
        };
        note(DrumKitVoice::voices(2, kit), act, 0.05, seconds)
    }

    /// A kit of one piece, on the key of the acoustic snare.
    fn only(piece: Piece) -> DrumKit {
        let mut kit = DrumKit::empty();
        kit.set(gm::SNARE, Some(piece));
        kit
    }

    /// The magnitude spectrum of `signal` through a Hann window, zero
    /// padded to `len`, and the frequency of each bin.
    fn spectrum(signal: &[f64], len: usize) -> (Vec<f64>, Vec<f64>) {
        let mut input = vec![0.0; len];
        let count = signal.len().min(len);
        for (n, (slot, &x)) in input.iter_mut().zip(signal).enumerate() {
            let w = 0.5 - 0.5 * (core::f64::consts::TAU * n as f64 / (count - 1) as f64).cos();
            *slot = x * w;
        }
        let mut fft = RealFft::<f64>::new(len);
        let mut out = vec![Complex::<f64>::ZERO; fft.spectrum_len()];
        fft.forward(&input, &mut out);
        let magnitudes = out.iter().map(|value| value.norm()).collect();
        let hertz = (0..out.len())
            .map(|bin| bin as f64 * RATE / len as f64)
            .collect();
        (magnitudes, hertz)
    }

    /// The peaks of a spectrum between `low` and `high` hertz: bins louder
    /// than every other within `reach` bins, as `(hertz, decibels)`.
    fn peaks(signal: &[f64], low: f64, high: f64, reach: usize) -> Vec<(f64, f64)> {
        let (magnitudes, hertz) = spectrum(signal, 1 << 15);
        let mut found = Vec::new();
        for bin in reach..magnitudes.len() - reach {
            if hertz[bin] < low || hertz[bin] > high {
                continue;
            }
            let around = &magnitudes[bin - reach..=bin + reach];
            if around.iter().all(|&m| m <= magnitudes[bin]) {
                found.push((hertz[bin], decibels(magnitudes[bin])));
            }
        }
        found
    }

    #[test]
    fn every_kit_plays_every_key_it_maps_at_a_sensible_level() {
        let mut wrong = Vec::new();
        for (name, kit) in kits() {
            let mut keys = 0;
            for key in kit.keys() {
                let signal = strike(kit, key, 1.0, false, 0.4);
                let loudest = peak(&signal);
                println!(
                    "{name:14} {key:3} {:?}: peak {:6.1} dB, centroid {:6.0} Hz",
                    kit.piece(key).unwrap().model,
                    decibels(loudest),
                    centroid(&signal, 8_192),
                );
                // Between -24 and -5 dB at full velocity, so that a kit
                // leaves room for accents and for drums struck together.
                if !(0.063..0.56).contains(&loudest) {
                    wrong.push(format!("{name} {key}: {loudest}"));
                }
                keys += 1;
            }
            assert!(keys >= 7, "{name}");
            // A key with no piece is silent.
            let empty = (KIT_FIRST_KEY..KIT_FIRST_KEY + KIT_KEYS as u8)
                .find(|&key| kit.piece(key).is_none())
                .unwrap_or(100);
            assert_eq!(peak(&strike(kit, empty, 1.0, false, 0.1)), 0.0, "{name}");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert_eq!(peak(&strike(DrumKit::acoustic(), 20, 1.0, true, 0.1)), 0.0);
        assert_eq!(peak(&strike(DrumKit::acoustic(), 127, 1.0, true, 0.1)), 0.0);
    }

    /// The acoustic snare against a real one recorded for the project, two
    /// takes of four and five hits, measured as below (the files were
    /// lossy, one band-limited at 11.8 kHz, so the shapes and times are
    /// trusted more than the levels above 10 kHz):
    ///
    /// - the level, in frames of 1 ms, peaks 3 ms after the attack;
    /// - its strong partials crowd 160 to 360 Hz (162.6, 181.6, 221.2,
    ///   246.1, 279.8, 303.2, 338.4 and 353.0 Hz in one take), denser than
    ///   one membrane's: pairs a few per cent apart;
    /// - its body, 120 to 400 Hz, falls 60 dB in 0.15 to 0.33 s, its wires,
    ///   2 to 10 kHz, in 0.40 to 0.70 s (both read from their first 20 dB
    ///   below their loudest 5 ms), and the wires are loudest 30 to 50 ms
    ///   after the body;
    /// - its spectral centroid, in frames of 20 ms, is 1.0 to 1.8 kHz in the
    ///   first, 3.4 to 4.3 kHz from 40 to 100 ms, and about 3.2 to 3.3 kHz
    ///   by 260 ms;
    /// - its wires carry 10.6 to 12 dB less energy than its body (2 to
    ///   10 kHz against 60 to 1 000 Hz, over the first 300 ms).
    ///
    /// The tolerances below are what this model reaches.
    #[test]
    fn the_snare_measures_as_the_one_recorded() {
        let signal = strike(DrumKit::acoustic(), gm::SNARE, 0.9, false, 0.9);
        let attack = loudest_at(&frame_levels(&signal, 0.001), 0.001);
        let body = frame_levels(&band_passed(&signal, 120.0, 400.0), 0.005);
        let wires = frame_levels(&band_passed(&signal, 2_000.0, 10_000.0), 0.005);
        let body_t60 = decay_from_peak(&body, 0.005, 20.0).unwrap();
        let wires_t60 = decay_from_peak(&wires, 0.005, 20.0).unwrap();
        let wires_late = loudest_at(&wires, 0.005) - loudest_at(&body, 0.005);
        let track = centroid_track(&signal, 0.02, 16);
        let middle = track[2..5].iter().sum::<f64>() / 3.0;
        let balance = decibels(
            (band_energy(&signal, 2_000.0, 10_000.0, 0.3)
                / band_energy(&signal, 60.0, 1_000.0, 0.3))
            .sqrt(),
        );
        println!(
            "peak at {:.1} ms, body {body_t60:.2} s, wires {wires_t60:.2} s, {:.0} ms later, \
             balance {balance:.1} dB, centroid {:.0?}",
            attack * 1e3,
            wires_late * 1e3,
            track
        );
        assert!((0.0015..=0.0045).contains(&attack), "{attack}");
        assert!((0.13..=0.36).contains(&body_t60), "{body_t60}");
        assert!((0.38..=0.75).contains(&wires_t60), "{wires_t60}");
        assert!(wires_t60 > body_t60);
        assert!((0.02..=0.06).contains(&wires_late), "{wires_late}");
        assert!((-13.5..=-9.5).contains(&balance), "{balance}");
        assert!((800.0..=1_900.0).contains(&track[0]), "{track:?}");
        assert!((3_200.0..=4_700.0).contains(&middle), "{middle}");
        assert!((2_400.0..=4_400.0).contains(&track[13]), "{track:?}");
        // The partials: the strongest between 150 and 360 Hz, with at
        // least four there within 12 dB of it, among them pairs 3 to 15 %
        // apart.
        let found = peaks(between(&signal, 0.004, 0.7), 100.0, 1_500.0, 6);
        let strongest = found.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let strong: Vec<f64> = found
            .iter()
            .filter(|p| p.1 > strongest - 12.0)
            .map(|p| p.0)
            .collect();
        println!("strong partials {strong:.1?}");
        let loudest = found.iter().find(|p| p.1 == strongest).unwrap().0;
        assert!((150.0..=360.0).contains(&loudest), "{loudest}");
        let crowd: Vec<f64> = strong
            .iter()
            .copied()
            .filter(|f| (150.0..=360.0).contains(f))
            .collect();
        assert!(crowd.len() >= 4, "{crowd:?}");
        let pairs = crowd
            .windows(2)
            .filter(|pair| (1.03..=1.15).contains(&(pair[1] / pair[0])))
            .count();
        assert!(pairs >= 2, "{crowd:?}");
    }

    /// The acoustic crash against a real one recorded for the project, four
    /// hits measured as below:
    ///
    /// - by octave band from 63 Hz, it falls 60 dB in 0.15, 0.24, 0.51,
    ///   2.8, 3.0, 3.6, 3.3 and 2.9 s (from the first 20 dB below each
    ///   band's loudest 10 ms): the low end dies fast, the middle rings
    ///   longest;
    /// - the bands up to 2 kHz are loudest at the strike, but 2 to 4 kHz
    ///   0.17 s after it, 4 to 8 kHz 0.23 s and 8 to 16 kHz 0.22 s: the
    ///   shimmer, energy moving up after the strike;
    /// - its spectral centroid every 100 ms (frames of 20 ms) goes 1.5 kHz
    ///   at the strike, 3.9, 5.3, 5.1, 5.5 (the highest, at 0.4 s), then
    ///   slowly down: 5.2, 5.1, 5.1, 5.1, 4.9, 4.7, 4.7, 4.4, 4.4 kHz at
    ///   1.3 s;
    /// - partials stand out of its spectrum in clusters near 530 to 545 Hz,
    ///   670 to 690 Hz and 1 820 to 1 925 Hz.
    ///
    /// The tolerances below are what this model reaches.
    #[test]
    fn the_crash_measures_as_the_one_recorded() {
        let signal = strike(DrumKit::acoustic(), gm::CRASH, 0.9, false, 3.8);
        let octaves = [
            (63.0, 0.15),
            (125.0, 0.24),
            (250.0, 0.51),
            (500.0, 2.76),
            (1_000.0, 3.0),
            (2_000.0, 3.57),
            (4_000.0, 3.3),
            (8_000.0, 2.9),
        ];
        let mut loudest = Vec::new();
        for (low, expected) in octaves {
            let levels = frame_levels(&band_passed(&signal, low, 2.0 * low), 0.01);
            let t60 = decay_from_peak(&levels, 0.01, 20.0).unwrap();
            let at = loudest_at(&levels, 0.01);
            println!("{low:5.0} Hz: T60 {t60:.2} s (recorded {expected}), loudest at {at:.2} s");
            assert!(
                (0.6 * expected..=1.5 * expected).contains(&t60),
                "{low} Hz: {t60}"
            );
            loudest.push(at);
        }
        // Up to 500 Hz, loudest at the strike; from 2 kHz up, later.
        for (index, &at) in loudest.iter().enumerate() {
            if index < 3 {
                assert!(at <= 0.05, "{index}: {at}");
            }
            if index >= 5 {
                assert!((0.1..=0.4).contains(&at), "{index}: {at}");
            }
        }
        let recorded = [
            1_531.0, 3_948.0, 5_274.0, 5_082.0, 5_526.0, 5_243.0, 5_081.0, 5_142.0, 5_056.0,
            4_859.0, 4_713.0, 4_686.0, 4_415.0, 4_399.0,
        ];
        let track = centroid_track(&signal, 0.02, 70);
        for (step, &expected) in recorded.iter().enumerate() {
            let measured = track[5 * step];
            println!(
                "{:.1} s: centroid {measured:.0} Hz, recorded {expected}",
                step as f64 * 0.1
            );
            let tolerance = if step == 0 { 0.4 } else { 0.25 };
            assert!(
                (measured / expected - 1.0).abs() <= tolerance,
                "{step}: {measured}"
            );
        }
        // The clusters: a peak in each that stands 6 dB or more above the
        // median of the spectrum from 400 Hz to 2 kHz.
        let tail = between(&signal, 0.05, 0.73);
        let (magnitudes, hertz) = spectrum(tail, 1 << 15);
        let mut middle: Vec<f64> = hertz
            .iter()
            .zip(&magnitudes)
            .filter(|(f, _)| (400.0..2_000.0).contains(*f))
            .map(|(_, &m)| decibels(m))
            .collect();
        middle.sort_by(f64::total_cmp);
        let median = middle[middle.len() / 2];
        for (low, high) in [(525.0, 550.0), (665.0, 695.0), (1_815.0, 1_930.0)] {
            let best = hertz
                .iter()
                .zip(&magnitudes)
                .filter(|(f, _)| (low..high).contains(*f))
                .map(|(_, &m)| decibels(m))
                .fold(f64::NEG_INFINITY, f64::max);
            println!("{low}-{high} Hz: {:.1} dB over the median", best - median);
            assert!(best - median >= 6.0, "{low}: {}", best - median);
        }
    }

    #[test]
    fn a_cymbal_struck_harder_shimmers_more() {
        // How far 4 to 16 kHz blooms, 0.15 to 0.3 s after the strike,
        // over the strike's own 125 to 500 Hz, struck hard and softly: the
        // coupling that carries energy up the bands grows with the swing.
        let bloom = |velocity| {
            let signal = strike(DrumKit::acoustic(), gm::CRASH, velocity, false, 0.4);
            let late = band_energy(between(&signal, 0.15, 0.3), 4_000.0, 16_000.0, 1.0);
            let strike = band_energy(&signal, 125.0, 500.0, 0.02);
            10.0 * (late / strike).log10()
        };
        let (hard, soft) = (bloom(1.0), bloom(0.4));
        println!("the bloom is {hard:.1} dB struck hard, {soft:.1} dB softly");
        assert!(hard > soft + 3.0, "{hard} against {soft}");
    }

    #[test]
    fn the_ride_bell_rings_its_partials_over_the_wash() {
        // Struck on the bell, its lowest partial stands far out of the
        // spectrum around it; struck on the bow, much less.
        let stand_out = |key| {
            let signal = strike(DrumKit::acoustic(), key, 0.9, false, 1.0);
            let (magnitudes, hertz) = spectrum(between(&signal, 0.1, 0.8), 1 << 15);
            let level = |low: f64, high: f64| {
                let values: Vec<f64> = hertz
                    .iter()
                    .zip(&magnitudes)
                    .filter(|(f, _)| (low..high).contains(*f))
                    .map(|(_, &m)| decibels(m))
                    .collect();
                values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            };
            level(735.0, 745.0) - level(600.0, 700.0)
        };
        let (bell, bow) = (stand_out(gm::RIDE_BELL), stand_out(gm::RIDE));
        println!("the bell's partial stands {bell:.1} dB out on the bell, {bow:.1} dB on the bow");
        assert!(bell > 15.0, "{bell}");
        assert!(bell > bow + 6.0, "{bell} against {bow}");
    }

    #[test]
    fn velocity_and_accent_change_level_and_brightness() {
        let pieces = [
            ("acoustic", gm::BASS_DRUM, DrumKit::acoustic(), true),
            ("acoustic", gm::SNARE, DrumKit::acoustic(), true),
            ("acoustic", gm::LOW_TOM, DrumKit::acoustic(), true),
            ("acoustic", gm::CLOSED_HI_HAT, DrumKit::acoustic(), true),
            ("acoustic", gm::CRASH, DrumKit::acoustic(), true),
            ("909", gm::BASS_DRUM, DrumKit::style_909(), false),
            ("909", gm::SNARE, DrumKit::style_909(), true),
            ("909", gm::HAND_CLAP, DrumKit::style_909(), false),
            ("909", gm::CLOSED_HI_HAT, DrumKit::style_909(), false),
            ("606", gm::SNARE, DrumKit::style_606(), true),
            ("cr78", gm::SNARE, DrumKit::style_cr78(), true),
            ("808", gm::SNARE, DrumKit::style_808(), true),
        ];
        for (name, key, kit, brighter) in pieces {
            let loud = strike(kit, key, 1.0, false, 0.3);
            let soft = strike(kit, key, 0.5, false, 0.3);
            let strong = strike(kit, key, 0.8, true, 0.3);
            let plain = strike(kit, key, 0.8, false, 0.3);
            let velocity_db = decibels(peak(&loud) / peak(&soft));
            let accent_db = decibels(peak(&strong) / peak(&plain));
            // Over the strike, where how hard it is struck shows.
            let brightness = centroid(&loud, 256) / centroid(&soft, 256);
            println!(
                "{name} {key}: velocity {velocity_db:.1} dB ×{brightness:.2}, accent {accent_db:.1} dB"
            );
            // Half the velocity is 15 dB down on the level the kit asks
            // for; struck harder, a wire or a cymbal's cascade answers more
            // than in proportion, a saturator less.
            assert!(
                (8.0..=22.0).contains(&velocity_db),
                "{name} {key}: {velocity_db}"
            );
            assert!(
                (2.0..=9.0).contains(&accent_db),
                "{name} {key}: {accent_db}"
            );
            if brighter {
                assert!(brightness > 1.02, "{name} {key}: {brightness}");
            }
        }
    }

    /// The residual of the open hi-hat after a closing strike: everything
    /// but the closing strike itself.
    fn residual_after(kit: DrumKit, open: u8, closing: u8, shared: bool) -> (Vec<f64>, usize) {
        let at = (0.15 * RATE) as usize;
        let voices = |kit| {
            if shared {
                DrumKitVoice::voices(4, kit)
            } else {
                (0..4).map(|_| DrumKitVoice::new(kit)).collect()
            }
        };
        let both = play(
            &mut synth_of(voices(kit)),
            &[(0, on(open, 1.0)), (at, on(closing, 1.0))],
            2 * at,
            128,
        );
        let alone = play(
            &mut synth_of(voices(kit)),
            &[(at, on(closing, 1.0))],
            2 * at,
            128,
        );
        (both.iter().zip(&alone).map(|(a, b)| a - b).collect(), at)
    }

    #[test]
    fn closing_the_hi_hat_chokes_it_open() {
        for (kit, open, closing) in [
            (DrumKit::acoustic(), gm::OPEN_HI_HAT, gm::CLOSED_HI_HAT),
            (DrumKit::acoustic(), gm::OPEN_HI_HAT, gm::PEDAL_HI_HAT),
            (DrumKit::style_909(), gm::OPEN_HI_HAT, gm::CLOSED_HI_HAT),
            (DrumKit::style_808(), gm::OPEN_HI_HAT, gm::CLOSED_HI_HAT),
            (DrumKit::lo_fi(), gm::OPEN_HI_HAT, gm::PEDAL_HI_HAT),
        ] {
            let (left, at) = residual_after(kit, open, closing, true);
            let fade = (CHOKE_SECONDS * RATE) as usize + 2;
            let before = largest_jump(&left[at - 500..at]);
            // Silent within the choke time, and no click on the way.
            assert!(peak(&left[at + fade + 600..]) < 1e-9, "{open}/{closing}");
            assert!(largest_jump(&left[at - 10..at + fade]) <= before * 1.05 + 1e-12);
            // Voices that do not share their groups ring on.
            let (free, at) = residual_after(kit, open, closing, false);
            assert!(peak(&free[at + 2_000..]) > 1e-3, "{open}/{closing}");
        }
    }

    #[test]
    fn strikes_start_and_are_taken_over_without_a_click() {
        for (name, kit) in kits() {
            for key in kit.keys() {
                let signal = strike(kit, key, 1.0, false, 0.15);
                assert!(
                    signal[0].abs() < 0.02 * peak(&signal),
                    "{name} {key}: {}",
                    signal[0]
                );
                // No step at the start. A step is heard below 1 kHz as
                // well as above, where the noise of a hi-hat or the
                // metal of a cymbal steps at every sample: below it, over
                // the first half millisecond, the samples step no more
                // than in the next five, or than a tenth of the peak: the
                // click of a trigger is a pulse that rises over ten
                // samples or more, not a step.
                let pole = (-core::f64::consts::TAU * 1_000.0 / RATE).exp();
                let (mut first, mut second) = (0.0, 0.0);
                let lowered: Vec<f64> = signal
                    .iter()
                    .map(|&x| {
                        first = x + pole * (first - x);
                        second = first + pole * (second - first);
                        second
                    })
                    .collect();
                let onset = largest_jump(&lowered[..24]);
                let body = largest_jump(&lowered[24..264]);
                assert!(
                    onset <= 2.0 * body || onset <= 0.1 * peak(&signal),
                    "{name} {key}: {onset} against {body}"
                );
            }
            // A voice taken over by another key fades the first strike out
            // under the second.
            let keys: Vec<u8> = kit.keys().collect();
            let again = (0.05 * RATE) as usize;
            for pair in keys.windows(2).step_by(3) {
                let mut synth = synth_of(DrumKitVoice::voices(1, kit));
                let both = play(
                    &mut synth,
                    &[(0, on(pair[0], 1.0)), (again, on(pair[1], 1.0))],
                    3 * again,
                    128,
                );
                let mut synth = synth_of(DrumKitVoice::voices(1, kit));
                let second = play(&mut synth, &[(again, on(pair[1], 1.0))], 3 * again, 128);
                let residual: Vec<f64> = both.iter().zip(&second).map(|(a, b)| a - b).collect();
                let at_strike = largest_jump(&residual[again - 10..again + 400]);
                let before = largest_jump(&residual[again - 1_000..again]);
                assert!(
                    at_strike <= 1.05 * before + 1e-9,
                    "{name} {pair:?}: {at_strike} against {before}"
                );
                assert!(peak(&residual[again + 400..]) < 1e-9, "{name} {pair:?}");
            }
        }
    }

    #[test]
    fn an_acoustic_piece_struck_again_adds_to_its_ringing_and_a_machine_starts_again() {
        let again = (0.1 * RATE) as usize;
        for (kit, key, adds) in [
            (DrumKit::acoustic(), gm::CRASH, true),
            (DrumKit::acoustic(), gm::SNARE, true),
            (DrumKit::style_909(), gm::OPEN_HI_HAT, false),
        ] {
            let twice = play(
                &mut synth_of(DrumKitVoice::voices(2, kit)),
                &[(0, on(key, 1.0)), (again, on(key, 1.0))],
                4 * again,
                128,
            );
            let mut second = play(
                &mut synth_of(DrumKitVoice::voices(2, kit)),
                &[(0, on(key, 1.0)), (again, on(key, 1.0))],
                4 * again,
                128,
            );
            // The same notes give the same sound.
            assert!(twice == second);
            // The first strike's share of what follows the second.
            let alone = play(
                &mut synth_of(DrumKitVoice::voices(2, kit)),
                &[(again, on(key, 1.0))],
                4 * again,
                128,
            );
            for (value, single) in second.iter_mut().zip(&alone) {
                *value -= single;
            }
            let carried = peak(&second[3 * again..]);
            if adds {
                assert!(carried > 1e-3, "{key}: {carried}");
            } else {
                assert!(carried < 1e-9, "{key}: {carried}");
            }
        }
    }

    #[test]
    fn the_output_does_not_depend_on_the_block_size() {
        for kit in [DrumKit::acoustic(), DrumKit::techno(), DrumKit::lo_fi()] {
            let keys: Vec<u8> = kit.keys().collect();
            let mut script = Vec::new();
            for (step, &key) in keys.iter().enumerate() {
                script.push((step * 700, accented(key, 0.8)));
                script.push((step * 700 + 350, on(gm::CLOSED_HI_HAT, 0.5)));
                script.push((step * 700 + 500, on(key, 0.3)));
            }
            let frames = keys.len() * 700 + 4_000;
            let run = |block| {
                play(
                    &mut synth_of(DrumKitVoice::voices(6, kit)),
                    &script,
                    frames,
                    block,
                )
            };
            let reference = run(256);
            for block in [1, 7, 64, 1_000] {
                let other = run(block);
                let first = other.iter().zip(&reference).position(|(a, b)| a != b);
                assert!(
                    first.is_none(),
                    "{:?} blocks of {block}: first differs at {first:?}",
                    kit.piece(36).map(|p| p.model)
                );
            }
        }
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let models = [
            DrumModel::Kick,
            DrumModel::Snare,
            DrumModel::SnareRimshot,
            DrumModel::SideStick,
            DrumModel::Tom,
            DrumModel::Bongo,
            DrumModel::OpenHat,
            DrumModel::Crash,
            DrumModel::RideBell,
            DrumModel::China,
            DrumModel::Kick909,
            DrumModel::Clap909,
            DrumModel::OpenHat909,
            DrumModel::Ride909,
            DrumModel::Snare606,
            DrumModel::GuiroCr78,
            DrumModel::TambourineCr78,
            DrumModel::Analog(AnalogDrum::Cymbal),
        ];
        for (values, lo_fi) in [
            (
                [
                    f64::INFINITY,
                    1e9,
                    f64::NAN,
                    -5.0,
                    1e9,
                    f64::NEG_INFINITY,
                    f64::NAN,
                ],
                LoFi {
                    drive: f64::INFINITY,
                    rate: 0.0,
                    bits: -3.0,
                    cutoff: f64::NAN,
                },
            ),
            (
                [4.0, 24.0, 10.0, 1.0, 1.0, 1.0, -1.0],
                LoFi {
                    drive: 1.0,
                    rate: 1_000.0,
                    bits: 2.0,
                    cutoff: 500.0,
                },
            ),
            ([4.0, -24.0, 0.1, 0.0, 0.0, 0.0, 1.0], LoFi::OFF),
        ] {
            let mut kit = DrumKit {
                velocity_range_db: f64::NAN,
                accent_db: 1e9,
                level: 1e9,
                lo_fi,
                ..DrumKit::empty()
            };
            for (index, &model) in models.iter().enumerate() {
                let [level, tune, decay, tone, snap, texture, pan] = values;
                kit.set(
                    KIT_FIRST_KEY + index as u8,
                    Some(Piece {
                        model,
                        level,
                        tune,
                        decay,
                        tone,
                        snap,
                        texture,
                        pan,
                        choke: 200,
                    }),
                );
            }
            let mut script = Vec::new();
            for index in 0..models.len() {
                script.push((index * 150, accented(KIT_FIRST_KEY + index as u8, 1.0)));
                script.push((index * 150 + 50, accented(KIT_FIRST_KEY + index as u8, 1.0)));
            }
            let signal = play(
                &mut synth_of(DrumKitVoice::voices(8, kit)),
                &script,
                12_000,
                64,
            );
            assert!(finite_within(&signal, 1e4), "{}", peak(&signal));
        }
    }

    #[test]
    fn the_sampler_holds_and_quantises() {
        let mut kit = only(Piece::new(DrumModel::Snare909));
        kit.lo_fi = LoFi {
            drive: 0.0,
            rate: 12_000.0,
            bits: 8.0,
            cutoff: 24_000.0,
        };
        let signal = strike(kit, gm::SNARE, 1.0, false, 0.1);
        let sounding = &signal[10..2_000];
        // Held for four samples at a time, at multiples of 2^-7.
        let held = sounding
            .windows(2)
            .filter(|pair| pair[0] == pair[1])
            .count();
        let share = held as f64 / (sounding.len() - 1) as f64;
        assert!((0.74..=0.78).contains(&share), "{share}");
        for &x in sounding {
            let steps = x * 128.0;
            assert!((steps - steps.round()).abs() < 1e-9, "{x}");
        }
    }

    #[test]
    fn pieces_are_panned_with_constant_power() {
        let mut kit = only(Piece {
            pan: -1.0,
            ..Piece::new(DrumModel::Snare909)
        });
        let mut synth = Synth::new(DrumKitVoice::voices(1, kit));
        synth.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: 2,
        });
        synth.note_on(NoteRef::new(9, gm::SNARE), 1.0);
        let mut buffer = AudioBuffer::<f64>::new(2, 512);
        synth.process(&mut buffer);
        assert!(peak(buffer.channel(0)) > 0.1 && peak(buffer.channel(1)) < 1e-12);
        // In the middle, both sides as loud as the mono voice.
        kit.set(gm::SNARE, Some(Piece::new(DrumModel::Snare909)));
        let mut synth = Synth::new(DrumKitVoice::voices(1, kit));
        synth.prepare(&ProcessSpec {
            sample_rate: RATE,
            max_block_size: 512,
            num_channels: 2,
        });
        synth.note_on(NoteRef::new(9, gm::SNARE), 1.0);
        let mut buffer = AudioBuffer::<f64>::new(2, 512);
        synth.process(&mut buffer);
        let mono = strike(kit, gm::SNARE, 1.0, false, 512.0 / RATE);
        for (frame, &value) in mono.iter().enumerate() {
            assert!((buffer.channel(0)[frame] - value).abs() < 1e-12);
            assert!((buffer.channel(1)[frame] - value).abs() < 1e-12);
        }
    }
}
