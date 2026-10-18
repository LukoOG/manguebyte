//! Control calls in two halves, for an engine that runs on a real-time
//! thread.
//!
//! In a browser the control calls run on the audio thread between render
//! quanta, and allocate there, which an `AudioWorklet` allows. A native
//! program whose engine renders in a real-time callback, such as a plugin,
//! must not allocate or free there. So each control call that allocates has
//! a counterpart here, cut in two:
//!
//! - **made off the audio thread**: a [`DetachedPart`], a
//!   [`DetachedInstrument`], [`DetachedStages`] or a [`DetachedSfz`] holds
//!   what the call would have made, built from the engine's
//!   [`EngineConfig`] alone, with nothing of the engine itself;
//! - **handed over on it**: [`Engine::attach_part`],
//!   [`Engine::detach_part`], [`Engine::swap_instrument`],
//!   [`Engine::set_chain_from`] and [`Engine::give_sfz`] move it in, and
//!   move what the engine lets go of back into the same slot, for the
//!   other thread to drop. They never allocate or free, and
//!   [`Engine::take_finished_instrument`] hands back, unfreed, the SFZ
//!   instruments sampler parts have finished with.
//!
//! What the engine renders is the same, bit for bit, as after the control
//! call: [`Engine::attach_part`] is [`Engine::create_part_with`],
//! [`Engine::swap_instrument`] is [`Engine::set_instrument`],
//! [`Engine::set_chain_from`] is [`Engine::set_chain`] and
//! [`Engine::give_sfz`] is [`Engine::load_instrument`], as
//! `tests/handoff.rs` checks with a counting allocator.

use core::fmt;

use manguebyte_dsp::ProcessSpec;

use super::{Chain, Engine, EngineConfig, EngineError, Instrument, InstrumentKind, Part};
use crate::chain::ChainDsp;
use crate::load::SfzInstrument;
use crate::params::InstrumentParams;
use crate::probe::LoudnessProbe;

/// Why a hand-over was refused. The engine and the slot are as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    /// What the control call itself would have refused: no such part,
    /// every part slot taken, a part that does not play SFZ instruments, or
    /// one that has not taken the instruments it was given yet.
    Engine(EngineError),
    /// The slot holds nothing to hand over.
    Empty,
    /// The slot must be empty to take something out of the engine.
    Occupied,
    /// What the slot holds was made for an engine of another sample rate
    /// or quantum, an SFZ instrument for the other kind of sampler part, or
    /// the stages lack an effect the part has never had.
    Mismatch,
}

impl From<EngineError> for HandoffError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl fmt::Display for HandoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(error) => error.fmt(f),
            Self::Empty => f.write_str("the slot holds nothing to hand over"),
            Self::Occupied => f.write_str("the slot is not empty"),
            Self::Mismatch => f.write_str("the slot was made for something else"),
        }
    }
}

impl std::error::Error for HandoffError {}

/// What a slot was made for: the sample rate and the quantum, which size
/// every buffer in it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Made {
    sample_rate: f64,
    max_quantum: usize,
}

impl Made {
    fn of(config: &EngineConfig) -> Result<(Self, ProcessSpec), EngineError> {
        config.check()?;
        let made = Self {
            sample_rate: config.sample_rate,
            max_quantum: config.max_quantum,
        };
        let spec = ProcessSpec {
            sample_rate: config.sample_rate,
            max_block_size: config.max_quantum,
            num_channels: 2,
        };
        Ok((made, spec))
    }

    fn fits(self, engine: &Engine) -> bool {
        self.sample_rate == engine.config.sample_rate
            && self.max_quantum == engine.config.max_quantum
    }
}

/// A part outside any engine: made off the audio thread for
/// [`Engine::attach_part`], or taken out of an engine by it (the part it
/// replaced) or by [`Engine::detach_part`], to be dropped off the audio
/// thread.
pub struct DetachedPart {
    part: Option<Part>,
    made: Option<Made>,
}

impl fmt::Debug for DetachedPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DetachedPart")
            .field("part", &self.number())
            .finish_non_exhaustive()
    }
}

impl DetachedPart {
    /// Part `part` playing `kind` through `chain`, as
    /// [`Engine::create_part_with`] makes it for an engine made with
    /// `config`. Allocates.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidConfig`] if a field of `config` is out of
    /// range.
    pub fn new(
        config: &EngineConfig,
        part: u16,
        kind: InstrumentKind,
        chain: Chain,
    ) -> Result<Self, EngineError> {
        let (made, spec) = Made::of(config)?;
        manguebyte_sampler::prepare_tables();
        Ok(Self {
            part: Some(Part::new(part, kind, chain, &spec, 0)),
            made: Some(made),
        })
    }

    /// An empty slot, for [`Engine::detach_part`] to fill. Never
    /// allocates.
    pub const fn empty() -> Self {
        Self {
            part: None,
            made: None,
        }
    }

    /// The number of the part it holds, if it holds one.
    pub fn number(&self) -> Option<u16> {
        self.part.as_ref().map(|part| part.id)
    }

    /// Whether it holds no part.
    pub fn is_empty(&self) -> bool {
        self.part.is_none()
    }

    /// The kind of the part it holds, if it holds one.
    pub fn kind(&self) -> Option<InstrumentKind> {
        self.part.as_ref().map(Part::kind)
    }
}

/// An instrument outside any part: made off the audio thread for
/// [`Engine::swap_instrument`], which leaves the one it replaces in it.
pub struct DetachedInstrument {
    held: Option<(InstrumentParams, Instrument)>,
    made: Made,
    kind: InstrumentKind,
}

impl fmt::Debug for DetachedInstrument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DetachedInstrument")
            .field("kind", &self.kind)
            .field("empty", &self.held.is_none())
            .finish_non_exhaustive()
    }
}

impl DetachedInstrument {
    /// `kind` with its parameters at their defaults, as
    /// [`Engine::set_instrument`] makes it for an engine made with
    /// `config`. Allocates.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidConfig`] if a field of `config` is out of
    /// range.
    pub fn new(config: &EngineConfig, kind: InstrumentKind) -> Result<Self, EngineError> {
        let (made, spec) = Made::of(config)?;
        manguebyte_sampler::prepare_tables();
        let params = InstrumentParams::new(kind);
        let instrument = Instrument::new(&params, &spec);
        Ok(Self {
            held: Some((params, instrument)),
            made,
            kind,
        })
    }

    /// The kind it holds: the one it was made with until it is handed
    /// over, then the one it replaced.
    pub fn kind(&self) -> InstrumentKind {
        self.kind
    }
}

/// Silent stages of effects, made off the audio thread, for a part to take
/// those it has never had in [`Engine::set_chain_from`].
pub struct DetachedStages {
    donor: Option<ChainDsp>,
    made: Made,
    effects: Chain,
}

impl fmt::Debug for DetachedStages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DetachedStages")
            .field("effects", &self.effects)
            .finish_non_exhaustive()
    }
}

impl DetachedStages {
    /// A stage for each effect of `effects`, made as [`Engine::set_chain`]
    /// makes one that joins a part's chain for the first time, for an
    /// engine made with `config`. Allocates, unless `effects` is empty.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidConfig`] if a field of `config` is out of
    /// range.
    pub fn new(config: &EngineConfig, effects: Chain) -> Result<Self, EngineError> {
        let (made, spec) = Made::of(config)?;
        let donor = (effects != Chain::NONE).then(|| ChainDsp::stage_donor(effects, &spec));
        Ok(Self {
            donor,
            made,
            effects,
        })
    }

    /// The effects it was made with.
    pub fn effects(&self) -> Chain {
        self.effects
    }
}

/// An SFZ instrument, measured for a sampler part's loudness normalisation
/// off the audio thread, for [`Engine::give_sfz`].
pub struct DetachedSfz {
    instrument: Option<Box<SfzInstrument>>,
    make_up_db: Option<f64>,
    kit: bool,
    made: Made,
    /// A measurement the part had under way, which the hand-over drops
    /// here rather than on the audio thread.
    dropped: Option<Box<LoudnessProbe>>,
}

impl fmt::Debug for DetachedSfz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DetachedSfz")
            .field("kit", &self.kit)
            .field("make_up_db", &self.make_up_db)
            .field("empty", &self.instrument.is_none())
            .finish_non_exhaustive()
    }
}

impl DetachedSfz {
    /// `instrument`, for a sampler part ([`InstrumentKind::Sampler`]) or,
    /// with `kit`, a sampler kit ([`InstrumentKind::SamplerKit`]) of an
    /// engine made with `config`, measured as [`Engine::load_instrument`]
    /// measures it for a part that normalises: its kind's phrase rendered
    /// offline, 4.5 s of it. Not real-time safe; allocates.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidConfig`] if a field of `config` is out of
    /// range.
    pub fn measured(
        config: &EngineConfig,
        instrument: SfzInstrument,
        kit: bool,
    ) -> Result<Self, EngineError> {
        let (made, _) = Made::of(config)?;
        manguebyte_sampler::prepare_tables();
        let make_up_db =
            LoudnessProbe::new(instrument.clone(), kit, config.sample_rate, true).finish();
        Ok(Self {
            instrument: Some(Box::new(instrument)),
            make_up_db,
            kit,
            made,
            dropped: None,
        })
    }

    /// The make-up gain measured, in decibels: what
    /// [`Engine::sampler_make_up_db`] says once the part plays it, if the
    /// part normalises. `None` if the instrument plays no key of the
    /// phrase.
    pub fn make_up_db(&self) -> Option<f64> {
        self.make_up_db
    }

    /// Whether it was made for a sampler kit.
    pub fn is_kit(&self) -> bool {
        self.kit
    }

    /// Whether the instrument has been handed over.
    pub fn is_empty(&self) -> bool {
        self.instrument.is_none()
    }
}

impl Engine {
    /// Puts the part `slot` holds into the engine, as
    /// [`create_part_with`](Self::create_part_with) would have made it
    /// there: a part of the same number is replaced and left in `slot`
    /// (keeping its scope and levels, as `create_part_with` does), and a new
    /// one starts its scope on the frame the parts render next. Real-time
    /// safe: never allocates or frees.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Empty`] if `slot` holds no part,
    /// [`HandoffError::Mismatch`] if it was made for another configuration,
    /// and [`EngineError::TooManyParts`] if the part is new and every slot
    /// is taken.
    pub fn attach_part(&mut self, slot: &mut DetachedPart) -> Result<(), HandoffError> {
        let made = slot.made.ok_or(HandoffError::Empty)?;
        let Some(number) = slot.number() else {
            return Err(HandoffError::Empty);
        };
        if !made.fits(self) {
            return Err(HandoffError::Mismatch);
        }
        match self
            .parts
            .binary_search_by_key(&number, |existing| existing.id)
        {
            Ok(index) => {
                let Some(new) = slot.part.as_mut() else {
                    return Err(HandoffError::Empty);
                };
                core::mem::swap(&mut new.meter, &mut self.parts[index].meter);
                core::mem::swap(new, &mut self.parts[index]);
            }
            Err(index) => {
                // The vector was made with room for every part: inserting
                // never grows it.
                if self.parts.len() >= self.config.max_parts {
                    return Err(EngineError::TooManyParts {
                        max: self.config.max_parts,
                    }
                    .into());
                }
                let frame = self.parts_frame();
                let Some(mut part) = slot.part.take() else {
                    return Err(HandoffError::Empty);
                };
                part.meter.start_at(frame);
                self.parts.insert(index, part);
            }
        }
        Ok(())
    }

    /// Takes part `part` out of the engine into `slot`, as
    /// [`remove_part`](Self::remove_part) does, so that it is dropped off
    /// the audio thread. Real-time safe.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Occupied`] if `slot` holds a part, and
    /// [`EngineError::NoSuchPart`] if there is none.
    pub fn detach_part(&mut self, part: u16, slot: &mut DetachedPart) -> Result<(), HandoffError> {
        if !slot.is_empty() {
            return Err(HandoffError::Occupied);
        }
        let index = self.index_of(part)?;
        slot.part = Some(self.parts.remove(index));
        slot.made = Some(Made {
            sample_rate: self.config.sample_rate,
            max_quantum: self.config.max_quantum,
        });
        Ok(())
    }

    /// Makes part `part` play the instrument `slot` holds, as
    /// [`set_instrument`](Self::set_instrument) would, keeping its chain;
    /// the instrument it played is left in `slot`. Real-time safe.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Empty`] if `slot` holds nothing,
    /// [`HandoffError::Mismatch`] if it was made for another configuration,
    /// and [`EngineError::NoSuchPart`] if there is no such part.
    pub fn swap_instrument(
        &mut self,
        part: u16,
        slot: &mut DetachedInstrument,
    ) -> Result<(), HandoffError> {
        if !slot.made.fits(self) {
            return Err(HandoffError::Mismatch);
        }
        let index = self.index_of(part)?;
        let Some((params, instrument)) = slot.held.as_mut() else {
            return Err(HandoffError::Empty);
        };
        let target = &mut self.parts[index];
        let old_kind = target.kind();
        core::mem::swap(params, &mut target.params.instrument);
        core::mem::swap(instrument, &mut target.instrument);
        slot.kind = old_kind;
        Ok(())
    }

    /// Changes which stages part `part`'s chain has, as
    /// [`set_chain`](Self::set_chain) does, taking the stages of the
    /// effects it has never had from `stages` instead of making them. Real-
    /// time safe.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Mismatch`] if `stages` was made for another
    /// configuration, or lacks an effect of `chain` the part has never
    /// had, and [`EngineError::NoSuchPart`] if there is no such part.
    pub fn set_chain_from(
        &mut self,
        part: u16,
        chain: Chain,
        stages: &mut DetachedStages,
    ) -> Result<(), HandoffError> {
        if !stages.made.fits(self) {
            return Err(HandoffError::Mismatch);
        }
        let index = self.index_of(part)?;
        let target = &mut self.parts[index];
        let missing = target.chain.unmade(chain);
        if missing != Chain::NONE {
            let Some(donor) = stages.donor.as_mut() else {
                return Err(HandoffError::Mismatch);
            };
            if donor.unmade(missing) != Chain::NONE {
                return Err(HandoffError::Mismatch);
            }
            target.chain.adopt_stages(donor, missing);
        }
        // Every stage of the chain is made now: this allocates nothing.
        self.set_chain(part, chain)?;
        Ok(())
    }

    /// Gives sampler part `part` the SFZ instrument `slot` holds, measured
    /// already, as [`load_instrument`](Self::load_instrument) would: new
    /// notes play it from the next frame rendered, at the make-up measured
    /// if the part normalises (`sampler.normalize`) and at the level of its
    /// samples if it does not; the notes of the instrument it replaces ring
    /// on, and [`take_finished_instrument`](Self::take_finished_instrument)
    /// hands that one back once they have ended. Real-time safe.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Empty`] if `slot` holds nothing,
    /// [`HandoffError::Mismatch`] if it was made for another configuration
    /// or for the other kind of sampler part, and the errors of
    /// `load_instrument`.
    pub fn give_sfz(&mut self, part: u16, slot: &mut DetachedSfz) -> Result<(), HandoffError> {
        if !slot.made.fits(self) {
            return Err(HandoffError::Mismatch);
        }
        let index = self.index_of(part)?;
        let Self { parts, stereo, .. } = self;
        let Instrument::Sampled(sampled) = &mut parts[index].instrument else {
            return Err(EngineError::NotASampler(part).into());
        };
        if sampled.is_kit() != slot.kit {
            return Err(HandoffError::Mismatch);
        }
        let instrument = slot.instrument.take().ok_or(HandoffError::Empty)?;
        match sampled.give(instrument, slot.make_up_db, stereo) {
            Ok(dropped) => {
                slot.dropped = dropped;
                Ok(())
            }
            Err(instrument) => {
                slot.instrument = Some(instrument);
                Err(EngineError::LoaderFull(part).into())
            }
        }
    }

    /// One SFZ instrument a sampler part has finished with (it was
    /// replaced, and its last note has ended), not freed, for the caller to
    /// drop off the audio thread. Call it until it returns `None`, as
    /// often as a part can be given instruments, or
    /// [`give_sfz`](Self::give_sfz) refuses for want of room. Real-time
    /// safe.
    pub fn take_finished_instrument(&mut self) -> Option<Box<SfzInstrument>> {
        self.parts
            .iter_mut()
            .find_map(|part| match &mut part.instrument {
                Instrument::Sampled(sampled) => sampled.take_finished(),
                _ => None,
            })
    }
}
