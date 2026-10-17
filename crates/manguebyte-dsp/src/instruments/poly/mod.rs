//! A deep polyphonic synthesiser, [`PolySynthVoice`], and a library of
//! [`PRESETS`] for electronic and pop music.
//!
//! The voice is subtractive at heart, with the means of the hybrid
//! software synthesisers: three oscillators of classic shapes and
//! wavetables with sixteen-voice unison, sync, FM and ring modulation, a
//! sub oscillator and noise, two filters of fifteen models, four
//! envelopes, three LFOs and a sixteen-slot modulation matrix. Its
//! settings, [`PolySynthParams`], are plain data, and so is every preset:
//! a constant an interface can list by index or find by name.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::event::NoteRef;
//! use manguebyte_dsp::instruments::poly::{self, Category, PolySynthVoice};
//! use manguebyte_dsp::{ProcessSpec, Processor};
//!
//! // Every bass in the library.
//! let basses: Vec<&str> = poly::PRESETS
//!     .iter()
//!     .filter(|preset| preset.category == Category::Bass)
//!     .map(|preset| preset.name)
//!     .collect();
//! assert!(basses.len() >= 8);
//!
//! let preset = poly::preset(basses[0]).unwrap();
//! let mut bass = PolySynthVoice::synth::<f32>(&preset.params, 4);
//! bass.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 });
//! bass.note_on(NoteRef::new(0, 36), 0.9);
//! let mut block = AudioBuffer::<f32>::new(2, 256);
//! bass.process(&mut block);
//! assert!(block.peak() > 0.0);
//! ```
//!
//! # Presets
//!
//! [`PRESETS`] holds them in this order, so an interface can offer them by
//! index, and [`preset`] finds one by name. Each one's level is set so that
//! a single note peaks near -9 dBFS, which leaves room for chords. The
//! example `poly_synth_presets` renders every one as a phrase suited to its
//! category: a bass line, a lead melody, pad chords, a pluck arpeggio,
//! keyboard comping, brass stabs, effects.
//!
//! | # | Name | Category | What it is |
//! |---|---|---|---|
//! | 0 | Moog Bass | bass | Two saws and a square an octave down through a ladder that snaps shut: round and punchy. |
//! | 1 | Reese | bass | Two pairs of saws detuned against each other with a sine sub: the slow, phasing drum-and-bass growl. |
//! | 2 | Wobble Bass | bass | Saw and square through a driven MS-20 filter that an LFO opens in eighths of the tempo: dubstep, ready to wobble. |
//! | 3 | Deep Sub | bass | A sine with a touch of triangle an octave up and a quick pitch punch: weight you feel more than hear. |
//! | 4 | FM Growl | bass | A sine frequency-modulated by another at its own pitch, the index falling after the attack and wobbling in sixteenths. |
//! | 5 | House Organ Bass | bass | A drawbar organ with its fundamental reinforced, a percussive decay and a key click: the bass of 90s house. |
//! | 6 | Acid Line | bass | A saw through a resonant ladder its envelope opens and closes on every note; accents open it further, overlapping notes slide. |
//! | 7 | Garage Square | bass | A square and a square sub through a gently enveloped ladder: the hollow bass of UK garage. |
//! | 8 | Wavetable Growl | bass | Folded and digital wavetables swept in eighths through a driven MS-20 filter: a modern, metallic growl. |
//! | 9 | Synthpop Bass | bass | Two slowly pulse-width-modulated squares through a ladder with a short envelope: the 80s sequencer bass. |
//! | 10 | Funk Bass | bass | Saw and pulse through an SEM-like filter set between low-pass and band-pass, with a quick, velocity-hungry envelope. |
//! | 11 | 808 Glide | bass | A saturated sine with a falling pitch at the start and a long decay that slides between overlapping notes: the trap 808. |
//! | 12 | Rolling Bass | bass | Three detuned saws and one an octave down, a short filter blip on every note: the off-beat trance bass. |
//! | 13 | Rubber Bass | bass | A triangle and a sine sub through a resonant 12 dB ladder, gliding: round and bouncy. |
//! | 14 | Talking Bass | bass | Detuned saws and a square through a driven vowel filter that an LFO moves from vowel to vowel every beat: a bass that says "yoi". |
//! | 15 | Trance Supersaw | lead | Seven detuned saws spread wide and seven more an octave up: the classic trance lead; the wheel adds vibrato. |
//! | 16 | Square Lead | lead | A square with a softer one an octave up, slowly pulse-width-modulated, with a vibrato that comes in late; it slurs. |
//! | 17 | Sync Lead | lead | A saw hard-synced to another, its pitch swept down by an envelope on every note: the tearing sync lead. |
//! | 18 | Chiptune | lead | A bare 25% pulse with a blip of pitch at the start and a delayed vibrato: the sound of the game consoles. |
//! | 19 | Hoover | lead | Wide detuned saws and pulses an octave down that bend up into every note and slur: the rave hoover. |
//! | 20 | Mono Saw Lead | lead | Three saws, one an octave down, through a ladder that pressure opens: the fat monophonic lead that glides. |
//! | 21 | Whistle Lead | lead | A sine with a faint triangle an octave up, a slow attack and a late vibrato: soft, flute-like, gliding. |
//! | 22 | Digital Lead | lead | A stepped digital wavetable slowly scanned, with a spectral one a fifth up: glassy and modern. |
//! | 23 | Talking Lead | lead | Two saws through the vowel filter, which an LFO moves in eighths: a lead that talks. |
//! | 24 | Screamer | lead | Saw and pulse through an MS-20 high-pass and a driven, resonant MS-20 low-pass in a row: aggressive and loud. |
//! | 25 | Future Lead | lead | Detuned saws and squares spread wide with a hint of pitch at the attack: the bright future-bass lead. |
//! | 26 | Warm Analog Pad | pad | Two sets of detuned saws through a slowly opening ladder that breathes: the warm polysynth pad. |
//! | 27 | String Machine | pad | Detuned saws and an octave above, their detune shimmering slowly, through a gentle 12 dB filter: the string ensemble. |
//! | 28 | Glass Pad | pad | The glass wavetable, slowly scanned and spread wide, with a sine an octave up: crystalline and airy. |
//! | 29 | Dark Drone | pad | Low detuned saws and a fifth, a sub and brown noise through a dark, driven ladder that moves very slowly. |
//! | 30 | Choir Pad | pad | The choir wavetable drifting between oo and ah, spread like an ensemble, with a faint vibrato: voices. |
//! | 31 | Formant Pad | pad | Saws through the vowel filter, which an envelope and a slow LFO carry from vowel to vowel: a choir-like formant pad. |
//! | 32 | PWM Pad | pad | Two sets of pulses whose widths two LFOs move against each other: the lush, chorused pad of the 80s. |
//! | 33 | Shimmer Pad | pad | Saws, a harmonic wavetable an octave up and a sine a twelfth up, high-passed: bright and floating. |
//! | 34 | Evolving Spectra | pad | Spectral and resonant wavetables scanned by two slow LFOs through an SEM-like filter that opens over seconds. |
//! | 35 | Soft Sine Pad | pad | Detuned sines and triangles an octave up, with a slow vibrato: pure and gentle. |
//! | 36 | Comb Wash | pad | Saws and pink noise through a comb tuned to the note, its damping drifting: a resonant, ambient wash. |
//! | 37 | Sweep Pad | pad | Detuned saws and an octave down through a resonant ladder that a very slow LFO sweeps open and shut. |
//! | 38 | House Pluck | pluck | Detuned saws and a pulse with a filter that snaps shut: the short, bright pluck of house music. |
//! | 39 | Bell Pluck | pluck | A sine frequency-modulated at three and a half times its pitch, the index dying with the note: a bright bell. |
//! | 40 | Marimba | pluck | A sine struck by a quick burst of FM at four times its pitch, dying fast: a wooden mallet. |
//! | 41 | Plucked String | pluck | A burst of noise into a comb tuned to the note, damped: a plucked string, by Karplus and Strong's idea. |
//! | 42 | Future Pluck | pluck | Seven wide saws and an octave above with a closing filter, a pitch flick and some saturation: the future-bass pluck. |
//! | 43 | Glass Pluck | pluck | The glass wavetable with a sine two octaves up, decaying: a delicate, chiming pluck. |
//! | 44 | Trance Pluck | pluck | Five detuned saws through a resonant ladder with a short envelope: the arpeggiated trance pluck. |
//! | 45 | Pizzicato | pluck | A narrow pulse and a saw an octave down through an SEM-like band-pass that clicks open: plucked strings, synthetic. |
//! | 46 | Kalimba | pluck | A sine with a quick FM ping at six times its pitch and a quiet octave: a thumb piano. |
//! | 47 | Polysynth Piano | keys | Saw and triangle through a ladder that velocity opens, dying away like a struck string: the synth piano. |
//! | 48 | Clavinet | keys | A narrow pulse and a saw through a key-tracked ladder band-pass with a snappy envelope: funky and percussive. |
//! | 49 | FM Electric Piano | keys | A sine modulated at its own pitch that barks harder the harder it is struck, a faint tine and a tremolo: the FM electric piano. |
//! | 50 | Drawbar Organ | keys | A gospel registration of the organ wavetable with a key click and a light vibrato. |
//! | 51 | Juno Keys | keys | A saw and a pulse an octave down, slightly spread, through a ladder that settles after the attack: bright chord keys. |
//! | 52 | Wurly | keys | A saturated triangle with an octave that velocity brings in, and a tremolo: a reedy electric piano. |
//! | 53 | Vibraphone | keys | A sine and its fourth partial ringing a long time, with the motor's tremolo: metal bars. |
//! | 54 | Synth Brass | brass | Two pairs of saws through a ladder whose envelope swells after the attack, with a little scoop of pitch: polysynth brass. |
//! | 55 | Brass Stab | brass | Wide saws and a square with a filter that bites and closes: the short brass stab of 90s house. |
//! | 56 | Jump Brass | brass | A saw and a narrow pulse through an SEM-like low-pass with a swelling envelope: the bright, brassy stab of the 80s. |
//! | 57 | Rave Chord Stab | brass | A minor chord of saws on every key with a snapping filter: the chord stab of old-school rave. |
//! | 58 | Soft Horns | brass | A saw and a pulse through a 12 dB ladder that opens slowly, with a late vibrato: mellow horns. |
//! | 59 | Fanfare | brass | Bright saws and an octave above through an opening ladder: a full, triumphant brass section. |
//! | 60 | Riser | fx | Wide saws and noise whose pitch climbs an octave and filter opens over five seconds: the build-up before the drop. |
//! | 61 | Downlifter | fx | Wide saws and pink noise falling an octave and closing over four seconds: the release after the peak. |
//! | 62 | Noise Sweep | fx | White noise through a resonant band-pass that an LFO sweeps up and down: a whoosh. |
//! | 63 | Laser Zap | fx | A saw and a sine whose pitch falls three octaves in a flash: a laser. |
//! | 64 | Random Bleeps | fx | A pulse whose pitch and filter a sample-and-hold sets at random eight times a second: a computer thinking. |
//! | 65 | Wind | fx | Pink noise through two band-passes side by side, each drifting on its own slow LFO: wind. |
//! | 66 | Impact Boom | fx | A sine an octave down falling two octaves, with a burst of noise and saturation: a cinematic hit. |
//! | 67 | Siren | fx | A square whose pitch an LFO swings a fifth up and down: an alarm. |

mod filter;
mod modulation;
mod osc;
mod params;
mod presets;
mod tables;
#[cfg(test)]
mod tests;
mod voice;

pub use params::{
    ENVELOPES, EnvParams, FILTERS, FilterModel, FilterParams, FilterRouting, LFOS, LfoParams,
    MAX_DETUNE_CENTS, MAX_UNISON, MOD_SLOTS, ModDestination, ModSlot, ModSource, NoiseParams,
    OSCILLATORS, OscParams, OscSource, PolySynthParams, SubParams, SubWave, TableShape, VoiceMode,
    Wave, matrix,
};
pub use presets::PRESETS;
pub use voice::{LATENCY_SAMPLES, PolySynthVoice};

/// What a preset is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Basses.
    Bass,
    /// Leads.
    Lead,
    /// Pads.
    Pad,
    /// Plucks and mallets.
    Pluck,
    /// Keyboards.
    Keys,
    /// Brass and stabs.
    Brass,
    /// Effects: risers, sweeps, noise.
    Fx,
}

impl Category {
    /// Every category, in the order of their indices.
    pub const ALL: [Self; 7] = [
        Self::Bass,
        Self::Lead,
        Self::Pad,
        Self::Pluck,
        Self::Keys,
        Self::Brass,
        Self::Fx,
    ];

    /// The category's name, in lower case, for an interface or a file.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bass => "bass",
            Self::Lead => "lead",
            Self::Pad => "pad",
            Self::Pluck => "pluck",
            Self::Keys => "keys",
            Self::Brass => "brass",
            Self::Fx => "fx",
        }
    }
}

/// A named setting of the voice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// Its name, unique in the library.
    pub name: &'static str,
    /// What it is for.
    pub category: Category,
    /// What it sounds like, in a line.
    pub description: &'static str,
    /// The settings.
    pub params: PolySynthParams,
}

/// The preset of this name, ignoring case.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS
        .iter()
        .find(|preset| preset.name.eq_ignore_ascii_case(name))
}
