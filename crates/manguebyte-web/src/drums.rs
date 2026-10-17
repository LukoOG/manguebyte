//! The drum kit: eleven synthesised drums on the General MIDI note numbers.

use core::f64::consts::{LN_2, TAU};

use manguebyte_dsp::{Oscillator, ProcessSpec, Processor, StateVariableFilter, SvfMode, Waveform};
use manguebyte_params::ParamId;

use crate::engine::InstrumentKind;
use crate::level::{gain_of, make_up_db};
use crate::params::{self, KitParams};
use crate::synth::{SILENT, decay_coefficient};

/// One drum of the kit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Drum {
    /// Bass drum: a sine that falls in pitch, with a click.
    Kick,
    /// Snare: two tuned sines and filtered noise.
    Snare,
    /// Closed hi-hat. Striking it chokes the open hi-hat.
    ClosedHat,
    /// Open hi-hat.
    OpenHat,
    /// Ride cymbal.
    Ride,
    /// Crash cymbal.
    Crash,
    /// High tom: a tuned membrane that falls a little in pitch, with the
    /// stick's noise.
    HighTom,
    /// Mid tom, a fourth or so below the high tom.
    MidTom,
    /// Floor tom, the lowest.
    FloorTom,
    /// Side stick (cross-stick): the stick laid on the head, struck on the
    /// rim, a short woody click.
    SideStick,
    /// A tap on the body of an acoustic guitar: the thump of its air and
    /// top resonances, with the click of the finger or nail.
    GuitarTap,
}

/// One row of the published drum map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrumNote {
    /// The drum.
    pub drum: Drum,
    /// Its General MIDI note number.
    pub note: u8,
    /// The symbol Aratu writes for it in a drum line, if it has one.
    pub symbol: Option<char>,
    /// Its name, as in the JSON map.
    pub name: &'static str,
}

/// The drum map: General MIDI note numbers and Aratu's symbols.
///
/// | Drum       | Note | Symbol |
/// |------------|------|--------|
/// | kick       | 36   | `b`    |
/// | snare      | 38   | `s`    |
/// | closed hat | 42   | `c`    |
/// | open hat   | 46   | `o`    |
/// | crash      | 49   | `x`    |
/// | ride       | 51   | `r`    |
/// | high tom   | 50   | `t`    |
/// | mid tom    | 47   | `m`    |
/// | floor tom  | 43   | `f`    |
/// | side stick | 37   | `k`    |
/// | guitar tap | 88   | none   |
///
/// The first six rows are the original kit, in their original order; the
/// others were added after them, so that a row never moves. Note 88 is not
/// a General MIDI drum: it is the guitar body tap, which Aratu plays from
/// its guitar lines rather than a drum line, so it has no symbol. A drum
/// part ignores every other note.
pub const DRUM_MAP: [DrumNote; 11] = [
    DrumNote {
        drum: Drum::Kick,
        note: 36,
        symbol: Some('b'),
        name: "kick",
    },
    DrumNote {
        drum: Drum::Snare,
        note: 38,
        symbol: Some('s'),
        name: "snare",
    },
    DrumNote {
        drum: Drum::ClosedHat,
        note: 42,
        symbol: Some('c'),
        name: "closed_hat",
    },
    DrumNote {
        drum: Drum::OpenHat,
        note: 46,
        symbol: Some('o'),
        name: "open_hat",
    },
    DrumNote {
        drum: Drum::Crash,
        note: 49,
        symbol: Some('x'),
        name: "crash",
    },
    DrumNote {
        drum: Drum::Ride,
        note: 51,
        symbol: Some('r'),
        name: "ride",
    },
    DrumNote {
        drum: Drum::HighTom,
        note: 50,
        symbol: Some('t'),
        name: "high_tom",
    },
    DrumNote {
        drum: Drum::MidTom,
        note: 47,
        symbol: Some('m'),
        name: "mid_tom",
    },
    DrumNote {
        drum: Drum::FloorTom,
        note: 43,
        symbol: Some('f'),
        name: "floor_tom",
    },
    DrumNote {
        drum: Drum::SideStick,
        note: 37,
        symbol: Some('k'),
        name: "side_stick",
    },
    DrumNote {
        drum: Drum::GuitarTap,
        note: 88,
        symbol: None,
        name: "guitar_tap",
    },
];

impl Drum {
    /// Every drum, in the order the kit adds them up.
    pub const ALL: [Drum; 11] = [
        Drum::Kick,
        Drum::Snare,
        Drum::ClosedHat,
        Drum::OpenHat,
        Drum::Ride,
        Drum::Crash,
        Drum::HighTom,
        Drum::MidTom,
        Drum::FloorTom,
        Drum::SideStick,
        Drum::GuitarTap,
    ];

    fn entry(self) -> &'static DrumNote {
        // Every drum has exactly one row.
        let mut index = 0;
        while DRUM_MAP[index].drum as u8 != self as u8 {
            index += 1;
        }
        &DRUM_MAP[index]
    }

    /// The drum on General MIDI note `note`, if the kit has one there.
    ///
    /// ```
    /// use manguebyte_web::Drum;
    ///
    /// assert_eq!(Drum::from_note(42), Some(Drum::ClosedHat));
    /// assert_eq!(Drum::from_note(60), None);
    /// ```
    pub fn from_note(note: u8) -> Option<Self> {
        DRUM_MAP
            .iter()
            .find(|entry| entry.note == note)
            .map(|entry| entry.drum)
    }

    /// The drum Aratu writes as `symbol`.
    ///
    /// ```
    /// use manguebyte_web::Drum;
    ///
    /// assert_eq!(Drum::from_symbol('o').map(Drum::note), Some(46));
    /// assert_eq!(Drum::from_symbol('t'), Some(Drum::HighTom));
    /// ```
    pub fn from_symbol(symbol: char) -> Option<Self> {
        DRUM_MAP
            .iter()
            .find(|entry| entry.symbol == Some(symbol))
            .map(|entry| entry.drum)
    }

    /// The General MIDI note number.
    pub fn note(self) -> u8 {
        self.entry().note
    }

    /// Aratu's symbol, if the drum has one.
    pub fn symbol(self) -> Option<char> {
        self.entry().symbol
    }

    /// The name used in the JSON map and in parameter identifiers.
    pub fn name(self) -> &'static str {
        self.entry().name
    }
}

/// [`DRUM_MAP`] as JSON: an array of `{"name", "note", "symbol"}` objects
/// in the map's order, `symbol` a one-letter string or, for a drum without
/// one, `null`. Allocates.
///
/// ```
/// let json = manguebyte_web::drum_map_json();
/// assert!(json.starts_with(r#"[{"name":"kick","note":36,"symbol":"b"},"#));
/// assert!(json.ends_with(r#"{"name":"guitar_tap","note":88,"symbol":null}]"#));
/// ```
pub fn drum_map_json() -> String {
    let rows: Vec<String> = DRUM_MAP
        .iter()
        .map(|entry| {
            let symbol = match entry.symbol {
                Some(symbol) => format!(r#""{symbol}""#),
                None => "null".to_owned(),
            };
            format!(
                r#"{{"name":"{}","note":{},"symbol":{symbol}}}"#,
                entry.name, entry.note
            )
        })
        .collect();
    format!("[{}]", rows.join(","))
}

/// Settings every drum reads when it is struck.
#[derive(Debug, Clone, Copy, PartialEq)]
struct KitSettings {
    /// Frequency ratio from the tune parameter.
    tune: f64,
    /// Factor on every decay time.
    decay: f64,
    /// Whether this strike is accented. Only a drum whose character an
    /// accent changes beyond what [`Slot`] does reads it.
    accent: bool,
}

/// A level that falls by a fixed factor every sample, from 1.
#[derive(Debug, Clone, Copy)]
struct Shot {
    level: f64,
    coefficient: f64,
}

impl Shot {
    const SILENT: Self = Self {
        level: 0.0,
        coefficient: 0.0,
    };

    fn start(&mut self, seconds: f64, sample_rate: f64) {
        self.level = 1.0;
        self.coefficient = decay_coefficient(seconds, sample_rate);
    }

    fn next(&mut self) -> f64 {
        let level = self.level;
        self.level *= self.coefficient;
        level
    }

    fn sounding(&self) -> bool {
        self.level > SILENT
    }
}

/// White noise from a 64-bit xorshift generator (G. Marsaglia,
/// "Xorshift RNGs", Journal of Statistical Software, 2003).
#[derive(Debug, Clone, Copy)]
struct WhiteNoise(u64);

impl WhiteNoise {
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// A fade to silence over a fixed number of frames, from wherever the
/// level is when it starts.
#[derive(Debug, Clone, Copy)]
struct Fade {
    gain: f64,
    step: f64,
    remaining: u32,
}

impl Fade {
    const OFF: Self = Self {
        gain: 1.0,
        step: 0.0,
        remaining: 0,
    };

    fn is_on(&self) -> bool {
        self.remaining > 0
    }

    /// Fades out over `frames`, unless a shorter fade is already running.
    fn start(&mut self, frames: u32) {
        let frames = frames.max(1);
        if !self.is_on() || frames < self.remaining {
            self.step = self.gain / f64::from(frames);
            self.remaining = frames;
        }
    }

    /// The gain for this frame, and whether the fade has ended after it.
    fn next(&mut self) -> (f64, bool) {
        let gain = self.gain;
        self.gain -= self.step;
        self.remaining -= 1;
        (gain, self.remaining == 0)
    }
}

trait Sound {
    fn prepare(&mut self, sample_rate: f64);
    fn strike(&mut self, kit: &KitSettings);
    fn next(&mut self) -> f64;
    fn sounding(&self) -> bool;
}

fn prepared_filter(
    mode: SvfMode,
    frequency: f64,
    q: f64,
    sample_rate: f64,
) -> StateVariableFilter<f64> {
    let mut filter = StateVariableFilter::new(mode, frequency, q);
    Processor::<f64>::prepare(
        &mut filter,
        &ProcessSpec {
            sample_rate,
            max_block_size: 1,
            num_channels: 1,
        },
    );
    filter
}

/// The kick's level after its saturation.
const KICK_LEVEL: f64 = 0.75;

/// How an accent changes the kick beyond its level and attack: the beater's
/// click is twice as loud and the pitch starts higher, at 225 Hz rather
/// than 180 Hz before the tune, the sound of a harder hit.
const KICK_ACCENT_CLICK: f64 = 2.0;
const KICK_ACCENT_SWEEP: f64 = 180.0;
const KICK_SWEEP: f64 = 135.0;

#[derive(Debug, Clone)]
struct Kick {
    sample_rate: f64,
    tune: f64,
    /// How far above its resting pitch the kick starts, in hertz.
    sweep: f64,
    /// The click's level, relative to the body's.
    click_level: f64,
    phase: f64,
    pitch: Shot,
    body: Shot,
    click: Shot,
    noise: WhiteNoise,
}

impl Sound for Kick {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
    }

    fn strike(&mut self, kit: &KitSettings) {
        self.tune = kit.tune;
        (self.sweep, self.click_level) = if kit.accent {
            (KICK_ACCENT_SWEEP, KICK_ACCENT_CLICK)
        } else {
            (KICK_SWEEP, 1.0)
        };
        self.phase = 0.0;
        self.pitch.start(0.12, self.sample_rate);
        self.body.start(0.6 * kit.decay, self.sample_rate);
        self.click.start(0.006, self.sample_rate);
    }

    fn next(&mut self) -> f64 {
        let frequency = self.tune * (45.0 + self.sweep * self.pitch.next());
        self.phase = (self.phase + frequency / self.sample_rate).fract();
        let body = (TAU * self.phase).sin() * self.body.next();
        let click = self.noise.next() * self.click.next() * self.click_level;
        // A soft saturation thickens the body; at most 1 in, at most 1 out,
        // then 2.5 dB down, to leave room for the other drums and parts.
        let drive = 2.2;
        KICK_LEVEL * (drive * (0.85 * body + 0.15 * click)).tanh() / drive.tanh()
    }

    fn sounding(&self) -> bool {
        self.body.sounding()
    }
}

#[derive(Debug, Clone)]
struct Snare {
    sample_rate: f64,
    tune: f64,
    phases: [f64; 2],
    tone: Shot,
    rattle: Shot,
    noise: WhiteNoise,
    highpass: StateVariableFilter<f64>,
}

impl Sound for Snare {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.highpass = prepared_filter(SvfMode::Highpass, 1_800.0, 0.7, sample_rate);
    }

    fn strike(&mut self, kit: &KitSettings) {
        self.tune = kit.tune;
        self.phases = [0.0, 0.0];
        self.tone.start(0.14 * kit.decay, self.sample_rate);
        self.rattle.start(0.24 * kit.decay, self.sample_rate);
        Processor::<f64>::reset(&mut self.highpass);
    }

    fn next(&mut self) -> f64 {
        for (phase, frequency) in self.phases.iter_mut().zip([185.0, 330.0]) {
            *phase = (*phase + self.tune * frequency / self.sample_rate).fract();
        }
        let body = (0.65 * (TAU * self.phases[0]).sin() + 0.35 * (TAU * self.phases[1]).sin())
            * self.tone.next();
        let hiss = self.highpass.process_sample(0, self.noise.next()) * self.rattle.next();
        // About -6 dB at full velocity, like the kick.
        0.38 * body + 0.3 * hiss
    }

    fn sounding(&self) -> bool {
        self.tone.sounding() || self.rattle.sounding()
    }
}

/// The six square waves of the classic analogue cymbal circuit, in hertz.
const METAL: [f64; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

/// What makes one cymbal different from another.
#[derive(Debug, Clone, Copy)]
struct CymbalShape {
    /// Factor on the metal frequencies.
    spread: f64,
    band: f64,
    band_q: f64,
    high: f64,
    decay: f64,
    noise: f64,
    gain: f64,
}

const CLOSED_HAT: CymbalShape = CymbalShape {
    spread: 1.0,
    band: 10_000.0,
    band_q: 0.9,
    high: 7_000.0,
    decay: 0.07,
    noise: 0.4,
    gain: 1.2,
};

const OPEN_HAT: CymbalShape = CymbalShape {
    decay: 0.6,
    ..CLOSED_HAT
};

const RIDE: CymbalShape = CymbalShape {
    spread: 1.47,
    band: 5_200.0,
    band_q: 2.0,
    high: 2_600.0,
    decay: 1.8,
    noise: 0.1,
    gain: 2.0,
};

const CRASH: CymbalShape = CymbalShape {
    spread: 1.18,
    band: 6_500.0,
    band_q: 0.6,
    high: 3_600.0,
    decay: 2.4,
    noise: 0.8,
    gain: 0.6,
};

#[derive(Debug, Clone)]
struct Cymbal {
    shape: CymbalShape,
    sample_rate: f64,
    metal: [Oscillator; 6],
    band: StateVariableFilter<f64>,
    high: StateVariableFilter<f64>,
    amplitude: Shot,
    noise: WhiteNoise,
}

impl Cymbal {
    fn new(shape: CymbalShape, seed: u64) -> Self {
        Self {
            shape,
            sample_rate: 48_000.0,
            metal: METAL.map(|frequency| Oscillator::new(Waveform::Square, frequency)),
            band: StateVariableFilter::new(SvfMode::Bandpass, shape.band, shape.band_q),
            high: StateVariableFilter::new(SvfMode::Highpass, shape.high, 0.7),
            amplitude: Shot::SILENT,
            noise: WhiteNoise(seed),
        }
    }
}

impl Sound for Cymbal {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        for oscillator in &mut self.metal {
            oscillator.set_sample_rate(sample_rate);
        }
        // At low sample rates the bands move down to stay below the Nyquist
        // frequency, so that a cymbal still sounds; at 44.1 kHz and above
        // they are where the shape puts them.
        self.band = prepared_filter(
            SvfMode::Bandpass,
            self.shape.band.min(0.4 * sample_rate),
            self.shape.band_q,
            sample_rate,
        );
        self.high = prepared_filter(
            SvfMode::Highpass,
            self.shape.high.min(0.3 * sample_rate),
            0.7,
            sample_rate,
        );
    }

    fn strike(&mut self, kit: &KitSettings) {
        for (oscillator, frequency) in self.metal.iter_mut().zip(METAL) {
            oscillator.set_frequency(frequency * self.shape.spread * kit.tune);
            oscillator.reset_phase();
        }
        Processor::<f64>::reset(&mut self.band);
        Processor::<f64>::reset(&mut self.high);
        self.amplitude
            .start(self.shape.decay * kit.decay, self.sample_rate);
    }

    fn next(&mut self) -> f64 {
        let metal: f64 = self
            .metal
            .iter_mut()
            .map(Oscillator::next_sample)
            .sum::<f64>()
            / 6.0;
        let source = metal + self.shape.noise * self.noise.next();
        let banded = self.band.process_sample(0, source);
        let bright = self.high.process_sample(0, banded);
        bright * self.amplitude.next() * self.shape.gain
    }

    fn sounding(&self) -> bool {
        self.amplitude.sounding()
    }
}

/// One resonance of a modal drum: a sine that dies away.
#[derive(Debug, Clone, Copy)]
struct Mode {
    /// Frequency at rest, before the tune, in hertz.
    frequency: f64,
    /// Level at the strike.
    gain: f64,
    /// Seconds to fall by 60 dB, before the kit's decay factor.
    decay: f64,
}

impl Mode {
    const NONE: Self = Self {
        frequency: 0.0,
        gain: 0.0,
        decay: 0.0,
    };

    const fn new(frequency: f64, gain: f64, decay: f64) -> Self {
        Self {
            frequency,
            gain,
            decay,
        }
    }
}

/// What makes one modal drum different from another: its resonances, how
/// far their pitch falls after the strike, and the short burst of filtered
/// noise of what struck it.
#[derive(Debug, Clone, Copy)]
struct ModalShape {
    modes: [Mode; 4],
    /// How far above their resting pitch the modes start, as a fraction of
    /// it: a head struck hard is stretched further and sounds higher, then
    /// settles.
    sweep: f64,
    /// Seconds for that extra pitch to fall by 60 dB.
    sweep_decay: f64,
    noise_mode: SvfMode,
    noise_frequency: f64,
    noise_q: f64,
    /// Seconds for the noise to fall by 60 dB. The kit's decay does not
    /// change it: it is the contact, not the ringing.
    noise_decay: f64,
    noise_level: f64,
    /// Seconds the resonances take to rise to full level: 0 for a stick, a
    /// little more for the soft contact of a finger.
    attack: f64,
    gain: f64,
}

/// A small rack tom: resonances near the ratios of a stretched membrane
/// loaded by the air in its shell (about 1, 1.5 and 2.3), the lowest
/// starting about a third sharp and settling within a quarter of a second,
/// and the stick's noise around 2 to 3 kHz.
const HIGH_TOM: ModalShape = ModalShape {
    modes: [
        Mode::new(165.0, 1.0, 0.5),
        Mode::new(251.0, 0.35, 0.22),
        Mode::new(375.0, 0.12, 0.1),
        Mode::NONE,
    ],
    sweep: 0.35,
    sweep_decay: 0.3,
    noise_mode: SvfMode::Bandpass,
    noise_frequency: 2_600.0,
    noise_q: 0.8,
    noise_decay: 0.04,
    noise_level: 1.5,
    attack: 0.0,
    gain: 0.38,
};

/// A larger rack tom, a fifth or so below the high tom.
const MID_TOM: ModalShape = ModalShape {
    modes: [
        Mode::new(123.0, 1.0, 0.6),
        Mode::new(187.0, 0.35, 0.26),
        Mode::new(280.0, 0.12, 0.12),
        Mode::NONE,
    ],
    sweep: 0.32,
    sweep_decay: 0.32,
    noise_frequency: 2_200.0,
    ..HIGH_TOM
};

/// A floor tom: the lowest and longest.
const FLOOR_TOM: ModalShape = ModalShape {
    modes: [
        Mode::new(84.0, 1.0, 0.8),
        Mode::new(128.0, 0.38, 0.32),
        Mode::new(192.0, 0.14, 0.15),
        Mode::NONE,
    ],
    sweep: 0.3,
    sweep_decay: 0.35,
    noise_frequency: 1_800.0,
    gain: 0.4,
    ..HIGH_TOM
};

/// The stick laid across the head and struck on the rim: a hollow wooden
/// knock from the shell and the stick, a few inharmonic resonances that
/// are gone within 80 ms, on a sharp click.
const SIDE_STICK: ModalShape = ModalShape {
    modes: [
        Mode::new(520.0, 1.0, 0.06),
        Mode::new(1_750.0, 0.7, 0.045),
        Mode::new(2_950.0, 0.4, 0.03),
        Mode::new(330.0, 0.5, 0.08),
    ],
    sweep: 0.0,
    sweep_decay: 0.01,
    noise_mode: SvfMode::Bandpass,
    noise_frequency: 3_200.0,
    noise_q: 1.2,
    noise_decay: 0.012,
    noise_level: 1.5,
    attack: 0.0,
    gain: 0.3,
};

/// A knock on the top of an acoustic guitar, near the bridge: the air in
/// the body at about 100 Hz (the Helmholtz resonance of the sound hole),
/// the top's first modes at about 205 and 240 Hz and a faint one at
/// 410 Hz, under a short bright click of the nail. The finger's contact
/// softens the onset over 1.5 ms.
const GUITAR_TAP: ModalShape = ModalShape {
    modes: [
        Mode::new(100.0, 1.0, 0.3),
        Mode::new(205.0, 0.6, 0.2),
        Mode::new(240.0, 0.4, 0.15),
        Mode::new(410.0, 0.15, 0.07),
    ],
    sweep: 0.0,
    sweep_decay: 0.01,
    noise_mode: SvfMode::Highpass,
    noise_frequency: 4_000.0,
    noise_q: 0.7,
    noise_decay: 0.012,
    noise_level: 0.5,
    attack: 0.0015,
    gain: 0.4,
};

/// A drum made of a few decaying sines and a burst of noise: the toms, the
/// side stick and the guitar tap.
#[derive(Debug, Clone)]
struct Modal {
    shape: ModalShape,
    sample_rate: f64,
    tune: f64,
    /// Modes at or above this frequency are left out, rather than folded
    /// back below the Nyquist frequency at low sample rates.
    highest: f64,
    phases: [f64; 4],
    amplitudes: [Shot; 4],
    sweep: Shot,
    contact: Shot,
    filter: StateVariableFilter<f64>,
    noise: WhiteNoise,
    attack_step: f64,
    attack: f64,
}

impl Modal {
    fn new(shape: ModalShape, seed: u64) -> Self {
        Self {
            shape,
            sample_rate: 48_000.0,
            tune: 1.0,
            highest: 48_000.0 * 0.45,
            phases: [0.0; 4],
            amplitudes: [Shot::SILENT; 4],
            sweep: Shot::SILENT,
            contact: Shot::SILENT,
            filter: StateVariableFilter::new(
                shape.noise_mode,
                shape.noise_frequency,
                shape.noise_q,
            ),
            noise: WhiteNoise(seed),
            attack_step: 1.0,
            attack: 1.0,
        }
    }
}

impl Sound for Modal {
    fn prepare(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.highest = 0.45 * sample_rate;
        self.filter = prepared_filter(
            self.shape.noise_mode,
            self.shape.noise_frequency.min(0.4 * sample_rate),
            self.shape.noise_q,
            sample_rate,
        );
        self.attack_step = 1.0 / (self.shape.attack * sample_rate).max(1.0);
    }

    fn strike(&mut self, kit: &KitSettings) {
        self.tune = kit.tune;
        self.phases = [0.0; 4];
        for (amplitude, mode) in self.amplitudes.iter_mut().zip(self.shape.modes) {
            if mode.gain > 0.0 {
                amplitude.start(mode.decay * kit.decay, self.sample_rate);
            } else {
                *amplitude = Shot::SILENT;
            }
        }
        self.sweep.start(self.shape.sweep_decay, self.sample_rate);
        self.contact.start(self.shape.noise_decay, self.sample_rate);
        Processor::<f64>::reset(&mut self.filter);
        self.attack = 0.0;
    }

    fn next(&mut self) -> f64 {
        // Whatever has fallen below -80 dB is left out from then on, rather
        // than left to fall into subnormal numbers while the longest
        // resonance still rings: slow in WebAssembly, where nothing flushes
        // them to zero.
        let sweep = if self.sweep.sounding() {
            self.shape.sweep * self.sweep.next()
        } else {
            0.0
        };
        let pitch = self.tune * (1.0 + sweep);
        let mut body = 0.0;
        for ((phase, amplitude), mode) in self
            .phases
            .iter_mut()
            .zip(&mut self.amplitudes)
            .zip(self.shape.modes)
        {
            if !amplitude.sounding() {
                continue;
            }
            let frequency = mode.frequency * pitch;
            *phase = (*phase + frequency / self.sample_rate).fract();
            let level = amplitude.next();
            if frequency < self.highest {
                body += mode.gain * level * (TAU * *phase).sin();
            }
        }
        let contact = if self.contact.sounding() {
            self.filter.process_sample(0, self.noise.next()) * self.contact.next()
        } else {
            0.0
        };
        self.attack = (self.attack + self.attack_step).min(1.0);
        // The contact is heard at once; only the resonances take the
        // attack time to build up.
        (body * self.attack + self.shape.noise_level * contact) * self.shape.gain
    }

    fn sounding(&self) -> bool {
        self.amplitudes.iter().any(Shot::sounding) || self.contact.sounding()
    }
}

/// How much louder an accented drum is, in decibels: its level is
/// multiplied by `10^(DRUM_ACCENT_DB / 20)`, about 1.4.
pub const DRUM_ACCENT_DB: f64 = 3.0;

/// How hard an accented drum's attack is sharpened. For the first
/// [`DRUM_ACCENT_SNAP_SECONDS`] the drum's sound `x` becomes
/// `x[n] + k·(x[n] − x[n−1])`, a lift of the high frequencies that is
/// strongest on the transient, as when a drum is hit harder; `k` starts at
/// `DRUM_ACCENT_SNAP` and falls by 60 dB over that time. The hi-hats and
/// cymbals, whose sound is nearly all high frequencies already, take
/// [`CYMBAL_ACCENT_SNAP`] instead, so that the lift does not add more to
/// their peak than the gain does.
pub const DRUM_ACCENT_SNAP: f64 = 0.5;

/// [`DRUM_ACCENT_SNAP`] for the hi-hats and cymbals.
pub const CYMBAL_ACCENT_SNAP: f64 = 0.12;

/// How long the sharpened attack of an accented drum lasts. See
/// [`DRUM_ACCENT_SNAP`].
pub const DRUM_ACCENT_SNAP_SECONDS: f64 = 0.03;

/// A drum and what is common to every drum: velocity, level, accent and
/// fades.
#[derive(Debug, Clone)]
struct Slot<D> {
    sound: D,
    sounding: bool,
    velocity: f64,
    level: f64,
    fade: Fade,
    /// The accent's gain: 1 for a drum struck without one.
    accent_gain: f64,
    /// The accent's high-frequency lift, falling from 1, and how much of
    /// it there is at the start.
    snap: Shot,
    snap_amount: f64,
    /// The sound's previous sample, for the lift.
    previous: f64,
}

impl<D: Sound> Slot<D> {
    fn new(sound: D, snap_amount: f64) -> Self {
        Self {
            sound,
            sounding: false,
            velocity: 0.0,
            level: 1.0,
            fade: Fade::OFF,
            accent_gain: 1.0,
            snap: Shot::SILENT,
            snap_amount,
            previous: 0.0,
        }
    }

    fn strike(&mut self, velocity: f64, accent: bool, kit: &KitSettings, sample_rate: f64) {
        self.sound.strike(kit);
        self.sounding = true;
        self.velocity = velocity;
        self.fade = Fade::OFF;
        if accent {
            self.accent_gain = (DRUM_ACCENT_DB / 20.0 * core::f64::consts::LN_10).exp();
            self.snap.start(DRUM_ACCENT_SNAP_SECONDS, sample_rate);
        } else {
            self.accent_gain = 1.0;
            self.snap = Shot::SILENT;
        }
    }

    fn fade_out(&mut self, frames: u32) {
        if self.sounding {
            self.fade.start(frames);
        }
    }

    fn stop(&mut self) {
        self.sounding = false;
        self.fade = Fade::OFF;
        self.snap = Shot::SILENT;
        self.previous = 0.0;
    }

    fn next(&mut self) -> f64 {
        if !self.sounding {
            return 0.0;
        }
        let raw = self.sound.next();
        // Past -80 dB the lift is left out, rather than left to fall
        // into subnormal numbers.
        let lift = if self.snap.sounding() {
            self.snap_amount * self.snap.next() * (raw - self.previous)
        } else {
            0.0
        };
        self.previous = raw;
        let mut value = (raw + lift) * self.velocity * self.level * self.accent_gain;
        if self.fade.is_on() {
            let (gain, ended) = self.fade.next();
            value *= gain;
            if ended {
                self.stop();
            }
        }
        if !self.sound.sounding() {
            self.stop();
        }
        value
    }
}

/// How long the open hi-hat takes to fall silent when the closed one is
/// struck: a straight fade over `round(HAT_CHOKE_SECONDS × sample rate)`
/// frames, the first of them the frame the closed hat lands on.
pub const HAT_CHOKE_SECONDS: f64 = 0.002;

/// How long every sounding drum takes to fall silent when its part gets
/// all-notes-off: a straight fade over `round(DAMP_SECONDS × sample rate)`
/// frames. Drums are one-shots, so a note off does nothing to them.
pub const DAMP_SECONDS: f64 = 0.05;

/// A drum part's instrument.
#[derive(Debug, Clone)]
pub(crate) struct DrumKit {
    settings: KitSettings,
    sample_rate: f64,
    kick: Slot<Kick>,
    snare: Slot<Snare>,
    hats: [Slot<Cymbal>; 2],
    cymbals: [Slot<Cymbal>; 2],
    /// The toms, the side stick and the guitar tap, in [`Drum::ALL`] order.
    modal: [Slot<Modal>; 5],
    choke_frames: u32,
    damp_frames: u32,
    /// The drums' make-up gain ([`InstrumentKind::calibration`]), which
    /// brings the standard groove to the loudness target wherever the kit
    /// plays: in a drum part, and in a sampler kit or the drum machine
    /// that borrows its drums.
    make_up: f64,
}

/// Where a drum of [`Modal`] sound sits in [`DrumKit::modal`].
fn modal_index(drum: Drum) -> usize {
    drum as usize - Drum::HighTom as usize
}

fn frames_for(seconds: f64, sample_rate: f64) -> u32 {
    (seconds * sample_rate).round().max(1.0) as u32
}

impl DrumKit {
    pub(crate) fn new(params: &KitParams, sample_rate: f64) -> Self {
        let mut kit = Self {
            settings: KitSettings {
                tune: 1.0,
                decay: 1.0,
                accent: false,
            },
            sample_rate,
            kick: Slot::new(
                Kick {
                    sample_rate,
                    tune: 1.0,
                    sweep: KICK_SWEEP,
                    click_level: 1.0,
                    phase: 0.0,
                    pitch: Shot::SILENT,
                    body: Shot::SILENT,
                    click: Shot::SILENT,
                    noise: WhiteNoise(0x9E37_79B9_7F4A_7C15),
                },
                DRUM_ACCENT_SNAP,
            ),
            snare: Slot::new(
                Snare {
                    sample_rate,
                    tune: 1.0,
                    phases: [0.0; 2],
                    tone: Shot::SILENT,
                    rattle: Shot::SILENT,
                    noise: WhiteNoise(0xD1B5_4A32_D192_ED03),
                    highpass: StateVariableFilter::new(SvfMode::Highpass, 1_800.0, 0.7),
                },
                DRUM_ACCENT_SNAP,
            ),
            hats: [
                Slot::new(
                    Cymbal::new(CLOSED_HAT, 0x8CB9_2BA7_2F3D_8DD7),
                    CYMBAL_ACCENT_SNAP,
                ),
                Slot::new(
                    Cymbal::new(OPEN_HAT, 0xABC9_8388_FB8F_AC03),
                    CYMBAL_ACCENT_SNAP,
                ),
            ],
            cymbals: [
                Slot::new(Cymbal::new(RIDE, 0x2545_F491_4F6C_DD1D), CYMBAL_ACCENT_SNAP),
                Slot::new(
                    Cymbal::new(CRASH, 0x6A09_E667_F3BC_C909),
                    CYMBAL_ACCENT_SNAP,
                ),
            ],
            modal: [
                (HIGH_TOM, 0xBB67_AE85_84CA_A73B),
                (MID_TOM, 0x3C6E_F372_FE94_F82B),
                (FLOOR_TOM, 0xA54F_F53A_5F1D_36F1),
                (SIDE_STICK, 0x510E_527F_ADE6_82D1),
                (GUITAR_TAP, 0x9B05_688C_2B3E_6C1F),
            ]
            .map(|(shape, seed)| Slot::new(Modal::new(shape, seed), DRUM_ACCENT_SNAP)),
            choke_frames: frames_for(HAT_CHOKE_SECONDS, sample_rate),
            damp_frames: frames_for(DAMP_SECONDS, sample_rate),
            make_up: gain_of(make_up_db(InstrumentKind::Drums, 0)),
        };
        kit.kick.sound.prepare(sample_rate);
        kit.snare.sound.prepare(sample_rate);
        for slot in kit.hats.iter_mut().chain(&mut kit.cymbals) {
            slot.sound.prepare(sample_rate);
        }
        for slot in &mut kit.modal {
            slot.sound.prepare(sample_rate);
        }
        params.visit(&mut |param| {
            kit.set_param(param.param_id(), param.plain());
        });
        kit
    }

    pub(crate) fn note_on(&mut self, note: u8, velocity: f32, accent: bool) {
        let Some(drum) = Drum::from_note(note) else {
            return;
        };
        let velocity = f64::from(velocity);
        let kit = KitSettings {
            accent,
            ..self.settings
        };
        let rate = self.sample_rate;
        match drum {
            Drum::Kick => self.kick.strike(velocity, accent, &kit, rate),
            Drum::Snare => self.snare.strike(velocity, accent, &kit, rate),
            Drum::ClosedHat => {
                self.hats[1].fade_out(self.choke_frames);
                self.hats[0].strike(velocity, accent, &kit, rate);
            }
            Drum::OpenHat => self.hats[1].strike(velocity, accent, &kit, rate),
            Drum::Ride => self.cymbals[0].strike(velocity, accent, &kit, rate),
            Drum::Crash => self.cymbals[1].strike(velocity, accent, &kit, rate),
            Drum::HighTom | Drum::MidTom | Drum::FloorTom | Drum::SideStick | Drum::GuitarTap => {
                self.modal[modal_index(drum)].strike(velocity, accent, &kit, rate);
            }
        }
    }

    /// Every drum stops at once.
    pub(crate) fn choke(&mut self) {
        self.kick.stop();
        self.snare.stop();
        for slot in self.hats.iter_mut().chain(&mut self.cymbals) {
            slot.stop();
        }
        for slot in &mut self.modal {
            slot.stop();
        }
    }

    /// Every drum fades out over [`DAMP_SECONDS`].
    pub(crate) fn damp(&mut self) {
        self.fade_all(self.damp_frames);
    }

    /// Every drum fades out over `seconds`, or sooner if it is already
    /// fading faster.
    pub(crate) fn fade_out_all(&mut self, seconds: f64) {
        self.fade_all(frames_for(seconds, self.sample_rate));
    }

    fn fade_all(&mut self, frames: u32) {
        self.kick.fade_out(frames);
        self.snare.fade_out(frames);
        for slot in self.hats.iter_mut().chain(&mut self.cymbals) {
            slot.fade_out(frames);
        }
        for slot in &mut self.modal {
            slot.fade_out(frames);
        }
    }

    pub(crate) fn active_voices(&self) -> usize {
        usize::from(self.kick.sounding)
            + usize::from(self.snare.sounding)
            + self
                .hats
                .iter()
                .chain(&self.cymbals)
                .filter(|slot| slot.sounding)
                .count()
            + self.modal.iter().filter(|slot| slot.sounding).count()
    }

    /// Takes a new value; `false` if `id` is not a kit parameter.
    pub(crate) fn set_param(&mut self, id: ParamId, value: f64) -> bool {
        match id {
            params::KIT_TUNE => self.settings.tune = (value / 12.0 * LN_2).exp(),
            params::KIT_DECAY => self.settings.decay = value,
            _ => {
                let Some(index) = params::DRUM_LEVELS.iter().position(|&level| level == id) else {
                    return false;
                };
                let level = value.clamp(0.0, 1.0);
                match Drum::ALL[index] {
                    Drum::Kick => self.kick.level = level,
                    Drum::Snare => self.snare.level = level,
                    Drum::ClosedHat => self.hats[0].level = level,
                    Drum::OpenHat => self.hats[1].level = level,
                    Drum::Ride => self.cymbals[0].level = level,
                    Drum::Crash => self.cymbals[1].level = level,
                    drum @ (Drum::HighTom
                    | Drum::MidTom
                    | Drum::FloorTom
                    | Drum::SideStick
                    | Drum::GuitarTap) => self.modal[modal_index(drum)].level = level,
                }
            }
        }
        true
    }

    /// Writes the kit's sound over `output`, adding the drums up in
    /// [`Drum::ALL`] order.
    pub(crate) fn render(&mut self, output: &mut [f32]) {
        if self.active_voices() == 0 {
            output.fill(0.0);
            return;
        }
        for sample in output {
            let mut sum = self.kick.next();
            sum += self.snare.next();
            sum += self.hats[0].next();
            sum += self.hats[1].next();
            sum += self.cymbals[0].next();
            sum += self.cymbals[1].next();
            for slot in &mut self.modal {
                sum += slot.next();
            }
            *sample = (sum * self.make_up) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_map_is_the_published_one() {
        let rows: Vec<(u8, Option<char>, &str)> = DRUM_MAP
            .iter()
            .map(|entry| (entry.note, entry.symbol, entry.name))
            .collect();
        assert_eq!(
            rows,
            [
                (36, Some('b'), "kick"),
                (38, Some('s'), "snare"),
                (42, Some('c'), "closed_hat"),
                (46, Some('o'), "open_hat"),
                (49, Some('x'), "crash"),
                (51, Some('r'), "ride"),
                (50, Some('t'), "high_tom"),
                (47, Some('m'), "mid_tom"),
                (43, Some('f'), "floor_tom"),
                (37, Some('k'), "side_stick"),
                (88, None, "guitar_tap"),
            ]
        );
        for drum in Drum::ALL {
            assert_eq!(Drum::from_note(drum.note()), Some(drum));
            if let Some(symbol) = drum.symbol() {
                assert_eq!(Drum::from_symbol(symbol), Some(drum));
            }
            assert_eq!(Drum::ALL[drum as usize], drum);
        }
        for (index, drum) in Drum::ALL[Drum::HighTom as usize..].iter().enumerate() {
            assert_eq!(modal_index(*drum), index);
        }
        let notes: Vec<u8> = (0..=127)
            .filter(|&n| Drum::from_note(n).is_some())
            .collect();
        assert_eq!(notes, [36, 37, 38, 42, 43, 46, 47, 49, 50, 51, 88]);
        assert_eq!(Drum::from_symbol('q'), None);
    }

    #[test]
    fn the_map_json_is_exact() {
        assert_eq!(
            drum_map_json(),
            concat!(
                r#"[{"name":"kick","note":36,"symbol":"b"},"#,
                r#"{"name":"snare","note":38,"symbol":"s"},"#,
                r#"{"name":"closed_hat","note":42,"symbol":"c"},"#,
                r#"{"name":"open_hat","note":46,"symbol":"o"},"#,
                r#"{"name":"crash","note":49,"symbol":"x"},"#,
                r#"{"name":"ride","note":51,"symbol":"r"},"#,
                r#"{"name":"high_tom","note":50,"symbol":"t"},"#,
                r#"{"name":"mid_tom","note":47,"symbol":"m"},"#,
                r#"{"name":"floor_tom","note":43,"symbol":"f"},"#,
                r#"{"name":"side_stick","note":37,"symbol":"k"},"#,
                r#"{"name":"guitar_tap","note":88,"symbol":null}]"#,
            )
        );
    }

    #[test]
    fn a_fade_falls_from_where_it_is_and_a_shorter_one_wins() {
        let mut fade = Fade::OFF;
        fade.start(4);
        let gains: Vec<(f64, bool)> = (0..4).map(|_| fade.next()).collect();
        assert_eq!(
            gains,
            [(1.0, false), (0.75, false), (0.5, false), (0.25, true)]
        );

        let mut fade = Fade::OFF;
        fade.start(10);
        fade.next();
        fade.next();
        fade.start(20); // longer than what is left: ignored
        assert_eq!(fade.remaining, 8);
        fade.start(2); // shorter: taken, from the current gain of 0.8
        assert!((fade.next().0 - 0.8).abs() < 1e-12);
        let (gain, ended) = fade.next();
        assert!((gain - 0.4).abs() < 1e-12 && ended);
    }

    /// At the longest kit decay, no level of a modal drum or of its slot's
    /// accent ever becomes a subnormal number while the drum sounds.
    #[test]
    fn the_modal_drums_never_reach_subnormal_numbers() {
        let normal_or_zero = |level: f64| level == 0.0 || level.is_normal();
        let kit = KitSettings {
            tune: 1.0,
            decay: 4.0,
            accent: true,
        };
        for shape in [HIGH_TOM, MID_TOM, FLOOR_TOM, SIDE_STICK, GUITAR_TAP] {
            let mut slot = Slot::new(Modal::new(shape, 1), DRUM_ACCENT_SNAP);
            slot.sound.prepare(48_000.0);
            slot.strike(1.0, true, &kit, 48_000.0);
            let mut frames = 0;
            while slot.sounding {
                assert!(slot.next().is_finite());
                let sound = &slot.sound;
                let levels = sound
                    .amplitudes
                    .iter()
                    .chain([&sound.sweep, &sound.contact, &slot.snap])
                    .map(|shot| shot.level);
                for level in levels {
                    assert!(normal_or_zero(level), "{level:e} after {frames} frames");
                }
                frames += 1;
            }
            // It does end: at most the longest resonance's decay, times 4,
            // to -80 dB.
            assert!(frames > 0 && frames < (4.0 * 0.8 * 48_000.0 * 4.0 / 3.0) as usize + 2);
        }
    }

    #[test]
    fn white_noise_stays_in_range_and_is_centred() {
        let mut noise = WhiteNoise(1);
        let values: Vec<f64> = (0..100_000).map(|_| noise.next()).collect();
        assert!(values.iter().all(|v| (-1.0..1.0).contains(v)));
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        assert!(mean.abs() < 0.01, "mean {mean}");
    }
}
