//! The sampler part: an SFZ instrument played by `manguebyte-sampler`, and
//! the sampler kit, which falls back on the synthesized drums.

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::{Event as NoteEvent, EventKind, Expression, NoteRef};
use manguebyte_dsp::{ProcessSpec, Processor};
use manguebyte_params::{Param, ParamId};
use manguebyte_sampler::{Instrument, Interpolation, Sampler, SamplerLoader};

use crate::drums::{Drum, DrumKit};
use crate::level::{Trim, gain_of};
use crate::params::{self, SamplerParams};

/// How many voices a sampler part plays at once. A voice plays one region,
/// so a note that starts two layered regions takes two. A quarter as many
/// again are kept for voices fading out after being cut short.
pub const SAMPLER_VOICES: usize = 32;

/// The level a sampler part plays its samples at, in decibels: a sample
/// that peaks at full scale, played at full velocity in the centre, peaks
/// at -6 dB, which leaves room for chords and other parts. While the part
/// normalises its loudness (`sampler.normalize`, on by default), the
/// make-up gain measured for the instrument when it loaded comes on top
/// ([`Engine::sampler_make_up_db`](crate::Engine::sampler_make_up_db)).
pub const SAMPLER_LEVEL_DB: f64 = -6.0;

/// How much louder an accented note of a sampler part is, in decibels: its
/// regions play `10^(SAMPLER_ACCENT_DB / 20)`, about 1.41, times as loud.
pub const SAMPLER_ACCENT_DB: f64 = 3.0;

/// The shortest time a sampler part's all-notes-off takes to silence a
/// voice: a voice whose release (`ampeg_release`) is shorter, and a voice
/// that would play through (a one-shot, or a region started by a release),
/// fades out in a straight line over `round(PANIC_FADE_SECONDS × sample
/// rate)` frames instead, so that letting go never clicks.
pub const PANIC_FADE_SECONDS: f64 = 0.008;

/// A sampler part's instrument.
pub(crate) struct SampledInstrument {
    sampler: Sampler<f32>,
    loader: SamplerLoader,
    /// A sampler kit's synthesized drums, for the notes of the drum map its
    /// SFZ text has no region for.
    kit: Option<Box<DrumKit>>,
    accent_gain: f64,
    /// Numbers the notes, so that an accent reaches the voices of its own
    /// note and of no other on the same key.
    next_id: u32,
    /// The make-up gain the loudness normalisation measured for the
    /// instrument the part plays (`crate::probe`), if it did.
    make_up_db: Option<f64>,
    /// Whether the part normalises (`sampler.normalize`).
    normalize: bool,
    /// What the samples are played at: [`SAMPLER_LEVEL_DB`], and the
    /// make-up while the part normalises.
    trim: Trim,
}

impl SampledInstrument {
    pub(crate) fn new(params: &SamplerParams, spec: &ProcessSpec) -> Self {
        let (mut sampler, loader) = Sampler::new(SAMPLER_VOICES);
        sampler.prepare(spec);
        let mut instrument = Self {
            sampler,
            loader,
            kit: params
                .kit
                .as_ref()
                .map(|kit| Box::new(DrumKit::new(kit, spec.sample_rate))),
            accent_gain: gain_of(SAMPLER_ACCENT_DB),
            next_id: 0,
            make_up_db: None,
            normalize: params.normalize.value(),
            trim: Trim::new(gain_of(SAMPLER_LEVEL_DB) as f32, spec.sample_rate),
        };
        instrument.set_param(params::SAMPLER_INTERPOLATION, params.interpolation.plain());
        instrument
    }

    /// Whether this is a sampler kit, with drums of its own to fall back
    /// on.
    pub(crate) fn is_kit(&self) -> bool {
        self.kit.is_some()
    }

    /// Whether the part plays its instruments at the loudness target.
    pub(crate) fn normalizes(&self) -> bool {
        self.normalize
    }

    /// The make-up gain measured for the instrument the part plays, in
    /// decibels, if it was.
    pub(crate) fn make_up_db(&self) -> Option<f64> {
        self.make_up_db
    }

    /// What the samples are to be played at now.
    fn gain(&self) -> f32 {
        let make_up = match (self.normalize, self.make_up_db) {
            (true, Some(make_up)) => make_up,
            _ => 0.0,
        };
        gain_of(SAMPLER_LEVEL_DB + make_up) as f32
    }

    /// Moves to the gain the part should play at: at once while no sample
    /// sounds, gliding in over 20 ms while one does.
    fn retrim(&mut self) {
        let silent = self.sampler.active_voices() == 0;
        self.trim.set(self.gain(), silent);
    }

    /// Takes a new value; `false` if `id` is not a parameter of the part's
    /// instrument.
    pub(crate) fn set_param(&mut self, id: ParamId, value: f64) -> bool {
        match id {
            params::SAMPLER_INTERPOLATION => {
                self.sampler.set_interpolation(if value >= 0.5 {
                    Interpolation::Sinc
                } else {
                    Interpolation::Cubic
                });
                true
            }
            params::SAMPLER_NORMALIZE => {
                self.normalize = value >= 0.5;
                self.retrim();
                true
            }
            _ => self
                .kit
                .as_mut()
                .is_some_and(|kit| kit.set_param(id, value)),
        }
    }

    /// Hands `instrument` to the sampler, which plays new notes with it from
    /// the next frame it renders; the notes of the one it replaces ring on
    /// to their end. `make_up_db` is what the loudness normalisation
    /// measured for it, if it did: the part plays at it from the next frame
    /// if nothing sounds, and glides to it over 20 ms if something does,
    /// the notes of the instrument it replaces too. Frees the instruments
    /// the sampler is done with first. Allocates.
    pub(crate) fn load(
        &mut self,
        instrument: Instrument,
        make_up_db: Option<f64>,
    ) -> Result<(), Box<Instrument>> {
        self.loader.collect();
        self.loader.load(instrument)?;
        // The sampler takes what has arrived when it renders; rendering no
        // frames now lets it take this one at once, so that a note on the
        // very next frame plays it. It waits only if the sampler still
        // holds two instruments with notes sounding: those of the older
        // fade out over 6 ms, and then it comes in.
        self.sampler.process(&mut AudioBuffer::new(2, 0));
        self.make_up_db = make_up_db;
        self.retrim();
        Ok(())
    }

    /// Frees the instruments the sampler is done with; returns how many.
    pub(crate) fn collect(&mut self) -> usize {
        self.loader.collect()
    }

    pub(crate) fn note_on(&mut self, channel: u8, key: u8, velocity: f32, accent: bool) {
        if let Some(kit) = &mut self.kit {
            // The instrument the sampler starts notes with now, which is
            // the one a note on this frame plays.
            let sampled = self
                .sampler
                .instrument()
                .is_some_and(|instrument| instrument.covers_key(key));
            if !sampled && Drum::from_note(key).is_some() {
                kit.note_on(key, velocity, accent);
                return;
            }
        }
        self.next_id = self.next_id.wrapping_add(1);
        let note = NoteRef::with_id(channel, key, self.next_id);
        self.sampler.note_on(note, velocity);
        if accent {
            self.sampler.handle_event(&NoteEvent {
                offset: 0,
                kind: EventKind::NoteExpression {
                    note,
                    expression: Expression::Volume,
                    value: self.accent_gain,
                },
            });
        }
    }

    /// Drums are one-shots: a note off reaches only the sampler, whose
    /// regions say what it does.
    pub(crate) fn note_off(&mut self, channel: u8, key: u8) {
        self.sampler.note_off(NoteRef::new(channel, key), 0.0);
    }

    /// Every note stops: each voice of the sampler sounds one more frame,
    /// and the kit's drums stop at once.
    pub(crate) fn choke(&mut self) {
        self.sampler.fade_out_all(0.0);
        if let Some(kit) = &mut self.kit {
            kit.choke();
        }
    }

    /// Every note is let go of, never in less than [`PANIC_FADE_SECONDS`];
    /// the kit's drums fade out over [`DAMP_SECONDS`](crate::DAMP_SECONDS).
    pub(crate) fn all_notes_off(&mut self) {
        self.sampler.release_all(PANIC_FADE_SECONDS);
        if let Some(kit) = &mut self.kit {
            kit.damp();
        }
    }

    /// Every note fades out in a straight line over `seconds`, from where
    /// it is, or sooner if it is already fading faster: a cancel.
    pub(crate) fn fade_out(&mut self, seconds: f64) {
        self.sampler.fade_out_all(seconds);
        if let Some(kit) = &mut self.kit {
            kit.fade_out_all(seconds);
        }
    }

    pub(crate) fn active_voices(&self) -> usize {
        self.sampler.active_voices() + self.kit.as_ref().map_or(0, |kit| kit.active_voices())
    }

    /// Writes the part's sound over `stereo`, using `mono`, as long, for the
    /// kit's drums.
    pub(crate) fn render(&mut self, mono: &mut AudioBuffer<f32>, stereo: &mut AudioBuffer<f32>) {
        self.sampler.process(stereo);
        let (left, right) = stereo.channel_pair_mut(0, 1);
        self.trim.apply_stereo(left, right);
        if let Some(kit) = &mut self.kit {
            if kit.active_voices() > 0 {
                kit.render(mono.channel_mut(0));
                for channel in stereo.iter_channels_mut() {
                    for (sample, &drum) in channel.iter_mut().zip(mono.channel(0)) {
                        *sample += drum;
                    }
                }
            }
        }
    }
}
