//! Instruments: voices for [`Synth`](crate::Synth) that sound like the
//! instruments they are named after.
//!
//! | Voice | What it is | Presets |
//! |---|---|---|
//! | [`PluckedString`] | a plucked string by physical modelling | [nylon](StringParams::nylon_guitar) and [steel](StringParams::steel_guitar) guitar, [finger](StringParams::finger_bass) and [slap](StringParams::slap_bass) bass |
//! | [`FmVoice`] | four-operator frequency modulation | [electric piano](FmParams::electric_piano), [tubular bell](FmParams::tubular_bell), [FM bass](FmParams::fm_bass) |
//! | [`OrganVoice`] | a tonewheel organ: drawbars, percussion, key click | [jazz](OrganParams::jazz), [gospel](OrganParams::gospel), [full](OrganParams::full), [ballad](OrganParams::ballad), any [registration](OrganParams::registration) |
//!
//! [`RotarySpeaker`] is not a voice but a [`Processor`](crate::Processor)
//! to put after the organ: the rotating horn and drum it is usually heard
//! through.
//!
//! Each voice is built from the crate's own parts (delay lines, filters,
//! oscillators) and has a settings struct with presets. Give a
//! [`Synth`](crate::Synth) as many voices as notes should sound at once:
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::event::NoteRef;
//! use manguebyte_dsp::instruments::{PluckedString, StringParams};
//! use manguebyte_dsp::{ProcessSpec, Processor, Synth};
//!
//! let mut bass = Synth::with_voices(4, || PluckedString::new(StringParams::finger_bass()));
//! bass.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 128, num_channels: 1 });
//! bass.note_on_accented(NoteRef::new(0, 28), 0.9, true);
//!
//! let mut block = AudioBuffer::<f32>::new(1, 128);
//! bass.process(&mut block);
//! assert!(block.peak() > 0.0);
//! ```
//!
//! # Velocity and accent
//!
//! Every voice answers velocity with level and timbre: a note played
//! harder is louder and brighter, as on the instrument it models, by
//! amounts its settings say.
//!
//! An accent reaches a voice as an argument of the note that carries it,
//! through [`Synth::note_on_accented`](crate::Synth::note_on_accented) and
//! [`Voice::note_on_accented`](crate::Voice::note_on_accented). The other
//! way to do it, a flag the caller sets on every voice before each note
//! because only the synth knows which voice will take the note, makes the
//! accent a property of the voice instead of the note: it has to be cleared
//! again, and a note started any other way (from the event stream, or by a
//! voice taken over later) inherits whatever was set last. As an argument
//! it cannot outlive its note. The methods have defaults, so a voice that
//! knows nothing of accents is unaffected.
//!
//! What an accent does is the instrument's business, and each settings
//! struct says: usually louder and brighter, and for some more than that
//! (a pop on the slapped bass).
//!
//! # Taking a voice over
//!
//! When every voice is busy, the synth gives a new note the voice that
//! will be missed least and chokes it first. A polyphonic voice here keeps
//! two copies of its sound: the note it was playing moves aside and fades
//! out over [`HANDOVER_SECONDS`], alongside the new note, so the old note
//! does not stop with a click. A voice that is silent when it is choked
//! returns to the state it had when new, as the
//! [`Voice::choke`](crate::Voice::choke) contract asks, so the same notes
//! give the same output however the timeline is cut into blocks.
//!
//! # Settings
//!
//! Every voice takes its settings whole, through a `set_params` method,
//! which is real-time safe: call it on every voice through
//! [`Synth::voices_mut`](crate::Synth::voices_mut). Values out of range are
//! limited, and a value that is not a number takes the preset's default,
//! so no setting can make a voice produce anything but finite numbers.
//!
//! # Determinism
//!
//! Every source of noise is seeded by the note it belongs to, so a note
//! sounds the same every time it is played, whatever was played before it.

mod common;
mod fm;
#[cfg(test)]
pub(crate) mod measure;
mod organ;
mod rotary;
mod string;

pub use common::HANDOVER_SECONDS;
pub use fm::{Algorithm, FM_OPERATORS, FM_VELOCITY_RANGE_DB, FmOperator, FmParams, FmVoice};
pub use organ::{
    DRAWBAR_FOOTAGES, DRAWBARS, OrganManual, OrganParams, OrganVoice, Percussion, PercussionDecay,
    PercussionHarmonic,
};
pub use rotary::{RotaryParams, RotarySpeaker, RotarySpeed};
pub use string::{BodyMode, PluckedString, StringParams};
