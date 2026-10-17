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
//! assert!(!basses.is_empty());
//!
//! let preset = poly::preset(basses[0]).unwrap();
//! let mut bass = PolySynthVoice::synth::<f32>(&preset.params, 4);
//! bass.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 256, num_channels: 2 });
//! bass.note_on(NoteRef::new(0, 36), 0.9);
//! let mut block = AudioBuffer::<f32>::new(2, 256);
//! bass.process(&mut block);
//! assert!(block.peak() > 0.0);
//! ```

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
