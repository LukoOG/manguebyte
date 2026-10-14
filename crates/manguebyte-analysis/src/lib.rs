//! Metering and analysis for the Manguebyte audio framework.
//!
//! A meter listens to audio and changes nothing in it. Everything here
//! follows the same three rules:
//!
//! - **It runs on the audio thread.** Memory is allocated by the
//!   constructor, and by `prepare` where sizes depend on the sample rate
//!   (the spectrum analyser's band table, the pitch detector's frame);
//!   `measure` and `reset` never allocate, lock or wait. The tests of this
//!   crate replace the allocator to prove it.
//! - **Any thread can read it.** Each meter has a readings handle (for
//!   example [`LoudnessReadings`]) that can be cloned and given to a user
//!   interface. Reading is a handful of atomic loads; the audio thread
//!   never waits for a reader, nor a reader for it. Requests in the other
//!   direction, such as a reset, are flags that the audio thread picks up
//!   at its next block. A spectrum is too large for atomics: the
//!   [`SpectrumAnalyzer`] hands whole frames to one [`SpectrumReader`]
//!   through a wait-free [triple buffer](triple_buffer).
//! - **The block size does not matter.** A meter fed one sample at a time
//!   and one fed in blocks of thousands end with identical readings, bit
//!   for bit.
//!
//! Every meter is also a [`Processor`](manguebyte_dsp::Processor) that
//! passes audio through untouched, so it can be dropped into a chain.
//!
//! | Meter | What it reads | Standard |
//! |---|---|---|
//! | [`LoudnessMeter`] | Momentary, short-term and integrated loudness, loudness range | ITU-R BS.1770-4, EBU R 128, EBU Tech 3341 and 3342 |
//! | [`TruePeakMeter`] | The peak of the waveform between the samples | ITU-R BS.1770-4 Annex 2 |
//! | [`PeakMeter`] | The largest sample, with fall-back and hold | fall-back of IEC 60268-18 |
//! | [`RmsMeter`] | Root-mean-square level | |
//! | [`VuMeter`] | Volume units, with the needle's ballistics | ANSI C16.5, IEC 60268-17 |
//! | [`PpmMeter`] | Quasi-peak programme level | IEC 60268-10 Type I |
//! | [`StereoMeter`] | Correlation from -1 to +1, and balance | |
//! | [`SpectrumAnalyzer`] | Short-time spectrum in dBFS, fractional-octave bands, A and C weighting | bands of IEC 61260-1, weightings of IEC 61672-1 |
//! | [`PitchDetector`] | Fundamental frequency, confidence, nearest [`Note`] | YIN (2002), McLeod pitch method (2005) |
//!
//! ```
//! use manguebyte_analysis::{LoudnessMeter, TruePeakMeter};
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::{Gain, ProcessSpec, Processor};
//!
//! // A gain stage followed by the two meters EBU R 128 is built on.
//! let mut chain = (Gain::<f32>::from_db(-6.0), LoudnessMeter::stereo(), TruePeakMeter::new(2));
//! chain.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 480, num_channels: 2 });
//! let loudness = chain.1.readings().clone();
//! let true_peak = chain.2.readings().clone();
//!
//! let mut block = AudioBuffer::<f32>::new(2, 480);
//! for _ in 0..100 {
//!     for channel in block.iter_channels_mut() {
//!         for (n, sample) in channel.iter_mut().enumerate() {
//!             *sample = (core::f32::consts::TAU * n as f32 / 48.0).sin();
//!         }
//!     }
//!     chain.process(&mut block);
//! }
//!
//! // A full-scale 1 kHz sine in both channels, 6 dB down.
//! assert!((loudness.integrated() + 6.0).abs() < 0.1);
//! assert!((true_peak.max_overall_db() + 6.0).abs() < 0.1);
//! ```
//!
//! # Bad samples
//!
//! A filter that blows up, a buffer nobody filled or a plugin gone wrong
//! can hand a meter samples that are not audio. Every meter screens each
//! sample before it does anything else with it:
//!
//! - a sample that is not finite, a NaN or an infinity, is measured as
//!   silence;
//! - a finite sample with a magnitude above 1e9, which is 180 dB over full
//!   scale, is measured as 1e9.
//!
//! The second rule is needed as much as the first: the square of a large
//! enough number is an infinity, and the next step makes a NaN of it. With
//! both, nothing a meter is given can leave an infinity or a NaN in its
//! memory, and no meter has to be reset after a glitch upstream.
//!
//! A sample at the limit is still measured, as the enormous peak it is, and
//! each reading comes back from it at its own pace. At 48 kHz and with the
//! default settings:
//!
//! | Meter | One sample at the limit reads | Time until the reading is back |
//! |---|---|---|
//! | [`PeakMeter`], [`TruePeakMeter`] | +180 dB | 15.3 s to fall under full scale |
//! | [`PpmMeter`] | +144 dB | 12.2 s to fall under full scale |
//! | [`RmsMeter`] | +138 dB | 9.6 s to fall under full scale |
//! | [`VuMeter`] | +123 VU | 1.2 s to fall under 0 VU |
//! | [`LoudnessMeter`] | | 0.5 s for momentary loudness, 3.1 s for short-term |
//! | [`StereoMeter`] | all to one side | about 12 s, with a programme at -20 dBFS |
//! | [`SpectrumAnalyzer`] | a loud click across every bin | the transforms that hold it; with a release time `r`, it then falls at 4.34 dB/s divided by `r` |
//! | [`PitchDetector`] | a click, not a pitch | the analyses whose frame holds it |
//!
//! Readings taken over everything since the last reset keep such a sample
//! until the next one: the maximum of the two peak meters, and the
//! integrated loudness, the range and the maxima of the loudness meter.
//!
//! # Sample rates
//!
//! A meter takes its sample rate from the specification it is prepared
//! with. Rates from 1 Hz to 10 MHz are used as they are. A rate outside
//! that range is taken as the nearer end of it, and one that is not a
//! positive number as 44.1 kHz.

mod bands;
mod gating;
mod input;
mod kweighting;
mod level;
mod loudness;
mod note;
mod pitch;
mod ppm;
mod shared;
mod spectrum;
mod stereo;
pub mod triple_buffer;
mod true_peak;
mod vu;
mod weighting;

pub use bands::{Band, OctaveFraction};
pub use input::Source;
pub use kweighting::KWeighting;
pub use level::{PeakMeter, PeakReadings, RmsMeter, RmsReadings};
pub use loudness::{
    ChannelWeight, EBU_R128_TARGET_LUFS, Loudness, LoudnessMeter, LoudnessReadings,
};
pub use note::Note;
pub use pitch::{
    LagInterpolation, Pitch, PitchDetector, PitchMethod, PitchReadings, PitchSettings,
};
pub use ppm::{PpmMeter, PpmReadings};
pub use spectrum::{
    SPECTRUM_FLOOR_DB, SpectrumAnalyzer, SpectrumFrame, SpectrumReader, SpectrumSettings,
};
pub use stereo::{StereoMeter, StereoReadings};
pub use true_peak::{TruePeakMeter, TruePeakReadings};
pub use vu::{VuMeter, VuReadings};
pub use weighting::Weighting;
