//! The platform-independent engine.

use core::fmt;

use manguebyte_core::{AudioBuffer, NoDenormals};
use manguebyte_dsp::{ProcessSpec, Processor, Voice};
use manguebyte_params::{Param, ParamId, ParamSet};

use crate::chain::ChainDsp;
use crate::curve::{self, Curve};
use crate::drums::DrumKit;
use crate::effect::Effect;
use crate::fx::Clock;
use crate::load::SfzInstrument;
use crate::master::Master;
use crate::params::{self, InstrumentParams, PartParams};
use crate::queue::{Admitted, Pending};
use crate::record::{Event, EventRecord, RECORD_SIZE};
use crate::ring::RingConsumer;
use crate::sampled::SampledInstrument;
use crate::synth::{SynthInstrument, choke_synth, fade_synth, new_synth, synth_note_on};

/// The most the output ever reaches: about -0.3 dB below full scale. The
/// master bus's limiter keeps every sample within
/// `-OUTPUT_CEILING..=OUTPUT_CEILING`, and the waveform between the
/// samples under its ceiling, `master.limiter.ceiling`, -0.3 dBTP by
/// default.
pub const OUTPUT_CEILING: f32 = 0.966;

/// How fast the master limiter lets the level back up once the peaks that
/// made it turn the level down have passed, by default
/// (`master.limiter.release`): the time constant, in seconds, of the
/// exponential return to full level.
pub const LIMITER_RELEASE_SECONDS: f64 = 0.15;

/// How long a cancel record ([`RecordKind::Cancel`](crate::RecordKind::Cancel))
/// takes to silence the notes of its part that are sounding on its frame: a
/// straight fade over `round(CANCEL_FADE_SECONDS × sample rate)` frames, the
/// first of them the cancel's own, short enough to clear the way for the
/// notes that replace them and long enough not to click. A note already
/// fading faster keeps its own fade.
pub const CANCEL_FADE_SECONDS: f64 = 0.008;

use manguebyte_dsp::event::NoteRef;

/// Whether `event` sets a parameter of the master bus, which belongs to no
/// part whatever part its record names.
fn is_master(event: Event) -> bool {
    matches!(event, Event::Param { param, .. } if params::is_master(param))
}

/// What a part plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstrumentKind {
    /// Code 0: the polyphonic synth, eight voices.
    Synth,
    /// Code 1: the drum kit of [`DRUM_MAP`](crate::DRUM_MAP).
    Drums,
    /// Code 2: an SFZ instrument, loaded with
    /// [`Engine::load_instrument`]; silent until one is.
    Sampler,
    /// Code 3: an SFZ drum kit, loaded the same way, that plays the
    /// synthesized drum of [`Drums`](Self::Drums) for a note of the drum
    /// map its SFZ text has no region for, and every drum until it has
    /// one. It has the kit's parameters as well as the sampler's.
    SamplerKit,
}

impl InstrumentKind {
    /// Every kind, in the order of their codes.
    pub const ALL: [Self; 4] = [Self::Synth, Self::Drums, Self::Sampler, Self::SamplerKit];

    /// The code the WebAssembly exports use.
    pub const fn code(self) -> u32 {
        match self {
            Self::Synth => 0,
            Self::Drums => 1,
            Self::Sampler => 2,
            Self::SamplerKit => 3,
        }
    }

    /// Whether parts of this kind play SFZ instruments.
    pub const fn is_sampler(self) -> bool {
        matches!(self, Self::Sampler | Self::SamplerKit)
    }

    /// The kind with this code, if there is one.
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Synth),
            1 => Some(Self::Drums),
            2 => Some(Self::Sampler),
            3 => Some(Self::SamplerKit),
            _ => None,
        }
    }
}

/// Which optional stages a part's chain has: its [`Effect`]s. The order
/// they run in is fixed ([`Effect::PART_ORDER`]), and gain, pan and the
/// send come after them on every part.
///
/// A stage in the chain has its memory and its parameters, which the
/// part's description lists and events reach; it sounds while its switch
/// (`"lowpass.on"`, `"delay.on"`...) is on. The low-pass and the reverb
/// start switched on, as they always did; every other stage starts off,
/// so that putting it in the chain changes nothing until an event
/// switches it on, on its exact frame. A stage not in the chain costs
/// nothing, neither memory nor time, until it first joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chain {
    /// The resonant low-pass filter (`lowpass.*`).
    pub lowpass: bool,
    /// The reverb (`reverb.*`).
    pub reverb: bool,
    /// The resonant high-pass filter (`highpass.*`).
    pub highpass: bool,
    /// The three-band equaliser (`eq.*`).
    pub eq: bool,
    /// The drive (`drive.*`).
    pub drive: bool,
    /// The bitcrusher (`bitcrush.*`).
    pub bitcrush: bool,
    /// The compressor (`compressor.*`).
    pub compressor: bool,
    /// The chorus (`chorus.*`).
    pub chorus: bool,
    /// The flanger (`flanger.*`).
    pub flanger: bool,
    /// The phaser (`phaser.*`).
    pub phaser: bool,
    /// The tremolo (`tremolo.*`).
    pub tremolo: bool,
    /// The auto-pan (`autopan.*`).
    pub autopan: bool,
    /// The delay (`delay.*`).
    pub delay: bool,
}

impl Chain {
    /// The chain [`Engine::create_part`] gives a part: the low-pass and the
    /// reverb, the full chain of the engine before it had other effects.
    pub const FULL: Self = Self {
        lowpass: true,
        reverb: true,
        ..Self::NONE
    };
    /// Only gain and pan.
    pub const NONE: Self = Self {
        lowpass: false,
        reverb: false,
        highpass: false,
        eq: false,
        drive: false,
        bitcrush: false,
        compressor: false,
        chorus: false,
        flanger: false,
        phaser: false,
        tremolo: false,
        autopan: false,
        delay: false,
    };
    /// Every stage.
    pub const ALL: Self = Self {
        lowpass: true,
        reverb: true,
        highpass: true,
        eq: true,
        drive: true,
        bitcrush: true,
        compressor: true,
        chorus: true,
        flanger: true,
        phaser: true,
        tremolo: true,
        autopan: true,
        delay: true,
    };
    /// The bit of [`bits`](Self::bits) for the low-pass.
    pub const LOWPASS_BIT: u32 = 1;
    /// The bit of [`bits`](Self::bits) for the reverb.
    pub const REVERB_BIT: u32 = 2;
    /// Every bit [`from_bits`](Self::from_bits) knows: one per effect a
    /// part can have, `1 << `[`Effect::code`].
    pub const ALL_BITS: u32 = (1 << 13) - 1;

    /// The chain as bits, as the WebAssembly exports take it: bit
    /// [`Effect::code`] for each stage.
    pub const fn bits(self) -> u32 {
        let flags = [
            self.lowpass,
            self.reverb,
            self.highpass,
            self.eq,
            self.drive,
            self.bitcrush,
            self.compressor,
            self.chorus,
            self.flanger,
            self.phaser,
            self.tremolo,
            self.autopan,
            self.delay,
        ];
        let mut bits = 0;
        let mut code = 0;
        while code < flags.len() {
            if flags[code] {
                bits |= 1 << code;
            }
            code += 1;
        }
        bits
    }

    /// The chain these bits stand for, or `None` if an unknown bit is set.
    ///
    /// ```
    /// use manguebyte_web::{Chain, Effect};
    ///
    /// assert_eq!(Chain::from_bits(3), Some(Chain::FULL));
    /// assert_eq!(Chain::from_bits(0), Some(Chain::NONE));
    /// assert_eq!(Chain::from_bits(1 << 12), Some(Chain::NONE.with(Effect::Delay)));
    /// assert_eq!(Chain::from_bits(1 << 13), None);
    /// ```
    pub const fn from_bits(bits: u32) -> Option<Self> {
        const fn has(bits: u32, code: u32) -> bool {
            bits & (1 << code) != 0
        }
        if bits & !Self::ALL_BITS != 0 {
            return None;
        }
        Some(Self {
            lowpass: has(bits, 0),
            reverb: has(bits, 1),
            highpass: has(bits, 2),
            eq: has(bits, 3),
            drive: has(bits, 4),
            bitcrush: has(bits, 5),
            compressor: has(bits, 6),
            chorus: has(bits, 7),
            flanger: has(bits, 8),
            phaser: has(bits, 9),
            tremolo: has(bits, 10),
            autopan: has(bits, 11),
            delay: has(bits, 12),
        })
    }

    /// Whether the chain has `effect`. Never for an effect of the master
    /// bus.
    pub const fn has(self, effect: Effect) -> bool {
        self.bits() & effect.bit() != 0
    }

    /// The chain with `effect` added.
    #[must_use]
    pub const fn with(self, effect: Effect) -> Self {
        match Self::from_bits(self.bits() | effect.bit()) {
            Some(chain) => chain,
            None => self,
        }
    }

    /// The chain with `effect` taken out.
    #[must_use]
    pub const fn without(self, effect: Effect) -> Self {
        match Self::from_bits(self.bits() & !effect.bit()) {
            Some(chain) => chain,
            None => self,
        }
    }
}

impl Default for Chain {
    /// [`Chain::FULL`].
    fn default() -> Self {
        Self::FULL
    }
}

/// How an engine is built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    /// Sample rate in hertz, [`MIN_SAMPLE_RATE`](Self::MIN_SAMPLE_RATE) to
    /// [`MAX_SAMPLE_RATE`](Self::MAX_SAMPLE_RATE): 3 000 to 768 000, the
    /// range browsers allow for an `AudioContext`.
    pub sample_rate: f64,
    /// The most frames one [`Engine::render`] call produces, 1 to 8 192.
    pub max_quantum: usize,
    /// How many parts can exist at once, 1 to 1 024.
    pub max_parts: usize,
    /// How many events can wait for their frame, 1 to 1 048 576.
    pub pending_capacity: usize,
}

impl EngineConfig {
    /// The lowest sample rate an engine runs at, in hertz.
    pub const MIN_SAMPLE_RATE: f64 = 3_000.0;
    /// The highest sample rate an engine runs at, in hertz.
    pub const MAX_SAMPLE_RATE: f64 = 768_000.0;

    /// A configuration for `sample_rate` with room for quanta of 512
    /// frames, 32 parts and 4 096 waiting events.
    pub const fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            max_quantum: 512,
            max_parts: 32,
            pending_capacity: 4_096,
        }
    }

    fn check(&self) -> Result<(), EngineError> {
        let rate = self.sample_rate;
        if !(Self::MIN_SAMPLE_RATE..=Self::MAX_SAMPLE_RATE).contains(&rate) {
            return Err(EngineError::InvalidConfig("sample rate"));
        }
        if !(1..=8_192).contains(&self.max_quantum) {
            return Err(EngineError::InvalidConfig("max quantum"));
        }
        if !(1..=1_024).contains(&self.max_parts) {
            return Err(EngineError::InvalidConfig("max parts"));
        }
        if !(1..=1 << 20).contains(&self.pending_capacity) {
            return Err(EngineError::InvalidConfig("pending capacity"));
        }
        Ok(())
    }
}

/// Why a control call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineError {
    /// A field of the [`EngineConfig`] is out of range; the text names it.
    InvalidConfig(&'static str),
    /// Every part slot is taken.
    TooManyParts {
        /// The configured maximum.
        max: usize,
    },
    /// There is no part with this number.
    NoSuchPart(u16),
    /// The part does not play SFZ instruments.
    NotASampler(u16),
    /// The part has been given more instruments than it has taken yet;
    /// render, and try again.
    LoaderFull(u16),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(field) => write!(f, "the {field} is out of range"),
            Self::TooManyParts { max } => write!(f, "the engine already has {max} parts"),
            Self::NoSuchPart(part) => write!(f, "there is no part {part}"),
            Self::NotASampler(part) => write!(f, "part {part} is not a sampler"),
            Self::LoaderFull(part) => {
                write!(
                    f,
                    "part {part} has not taken the instruments it was given yet"
                )
            }
        }
    }
}

impl std::error::Error for EngineError {}

/// What became of a pushed record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// It waits for its frame. If the queue was full, the event due last
    /// was dropped to make room, and counted in [`EngineStats::dropped`].
    Queued,
    /// [`EventRecord::decode`] refused it.
    Ignored,
    /// The queue was full of events due no later than this one; it was
    /// dropped.
    Full,
}

/// Counts of what has happened to events since the engine was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineStats {
    /// Records accepted and queued.
    pub queued: u64,
    /// Records refused by [`EventRecord::decode`].
    pub ignored: u64,
    /// Valid events dropped because the queue of waiting events was full:
    /// records refused because every waiting event was due no later, and
    /// waiting events pushed out by an earlier one.
    pub dropped: u64,
    /// Events applied at the start of a quantum because their frame had
    /// already passed.
    pub late: u64,
    /// Events applied to the engine.
    pub applied: u64,
    /// Applied events that reached nothing: no such part, or no such
    /// parameter in it.
    pub unrouted: u64,
    /// Of the [`dropped`](Self::dropped) events, those that were waiting
    /// and were pushed out by an earlier one. The others were refused when
    /// they arrived, so `queued + ignored + dropped - displaced` is the
    /// number of records pushed.
    pub displaced: u64,
    /// Times [`Engine::drain`] or [`Engine::admit_bytes`] stopped with
    /// records left over, because the queue was full of events due before
    /// the next record. The records stay with the caller.
    pub stalled: u64,
    /// Times a part produced a sample that was not a finite number. The
    /// part was then silenced and its memory cleared, and that stretch of
    /// its sound was left out of the output.
    pub recovered: u64,
    /// Frames on which the output limiter held the level below full.
    pub limited: u64,
    /// Waiting events dropped by a cancel record
    /// ([`RecordKind::Cancel`](crate::RecordKind::Cancel)).
    pub cancelled: u64,
}

enum Instrument {
    Synth(Box<SynthInstrument>),
    Drums(Box<DrumKit>),
    Sampled(Box<SampledInstrument>),
}

struct Part {
    id: u16,
    params: PartParams,
    instrument: Instrument,
    chain: ChainDsp,
}

impl Part {
    fn new(id: u16, kind: InstrumentKind, stages: Chain, spec: &ProcessSpec) -> Self {
        let params = PartParams::new(kind, stages);
        let instrument = match &params.instrument {
            InstrumentParams::Synth(synth) => Instrument::Synth(Box::new(new_synth(
                synth,
                &ProcessSpec {
                    num_channels: 1,
                    ..*spec
                },
            ))),
            InstrumentParams::Drums(kit) => {
                Instrument::Drums(Box::new(DrumKit::new(kit, spec.sample_rate)))
            }
            InstrumentParams::Sampler(sampler) => {
                Instrument::Sampled(Box::new(SampledInstrument::new(sampler, spec)))
            }
        };
        let chain = ChainDsp::new(&params.chain, stages, spec);
        Self {
            id,
            params,
            instrument,
            chain,
        }
    }

    fn kind(&self) -> InstrumentKind {
        match self.instrument {
            Instrument::Synth(_) => InstrumentKind::Synth,
            Instrument::Drums(_) => InstrumentKind::Drums,
            Instrument::Sampled(ref sampled) if sampled.is_kit() => InstrumentKind::SamplerKit,
            Instrument::Sampled(_) => InstrumentKind::Sampler,
        }
    }

    /// Sets a parameter and passes the value it ends up with to whatever
    /// uses it. `false` if the part has no such parameter.
    fn set_param(&mut self, id: ParamId, value: f64) -> bool {
        let Some(param) = self.params.find_param(id) else {
            return false;
        };
        param.set_plain(value);
        let plain = param.plain();
        if self.chain.set_param(id, plain, &self.params.chain) {
            return true;
        }
        match &mut self.instrument {
            Instrument::Synth(synth) => {
                for voice in synth.voices_mut() {
                    voice.set_param(id, plain);
                }
                true
            }
            Instrument::Drums(kit) => kit.set_param(id, plain),
            Instrument::Sampled(sampled) => sampled.set_param(id, plain),
        }
    }

    fn apply(&mut self, event: Event) -> bool {
        if let Event::Param { param, value, .. } = event {
            return self.set_param(param, value);
        }
        match (&mut self.instrument, event) {
            (
                Instrument::Synth(synth),
                Event::NoteOn {
                    channel,
                    note,
                    velocity,
                    accent,
                    ..
                },
            ) => {
                synth_note_on(synth, NoteRef::new(channel, note), velocity, accent);
            }
            (Instrument::Synth(synth), Event::NoteOff { channel, note, .. }) => {
                synth.note_off(NoteRef::new(channel, note), 0.0);
            }
            (Instrument::Synth(synth), Event::Choke { .. }) => choke_synth(synth),
            (Instrument::Synth(synth), Event::AllNotesOff { .. }) => synth.all_notes_off(),
            (
                Instrument::Drums(kit),
                Event::NoteOn {
                    note,
                    velocity,
                    accent,
                    ..
                },
            ) => {
                kit.note_on(note, velocity, accent);
            }
            // Drums are one-shots: letting go of a key changes nothing.
            (Instrument::Drums(_), Event::NoteOff { .. }) => {}
            (Instrument::Drums(kit), Event::Choke { .. }) => kit.choke(),
            (Instrument::Drums(kit), Event::AllNotesOff { .. }) => kit.damp(),
            (
                Instrument::Sampled(sampled),
                Event::NoteOn {
                    channel,
                    note,
                    velocity,
                    accent,
                    ..
                },
            ) => sampled.note_on(channel, note, velocity, accent),
            (Instrument::Sampled(sampled), Event::NoteOff { channel, note, .. }) => {
                sampled.note_off(channel, note);
            }
            (Instrument::Sampled(sampled), Event::Choke { .. }) => sampled.choke(),
            (Instrument::Sampled(sampled), Event::AllNotesOff { .. }) => sampled.all_notes_off(),
            (Instrument::Synth(synth), Event::Cancel { .. }) => {
                fade_synth(synth, CANCEL_FADE_SECONDS);
            }
            (Instrument::Drums(kit), Event::Cancel { .. }) => kit.fade_out_all(CANCEL_FADE_SECONDS),
            (Instrument::Sampled(sampled), Event::Cancel { .. }) => {
                sampled.fade_out(CANCEL_FADE_SECONDS);
            }
            (_, Event::Param { .. }) => {}
        }
        true
    }

    fn active_voices(&self) -> usize {
        match &self.instrument {
            Instrument::Synth(synth) => synth.active_voices(),
            Instrument::Drums(kit) => kit.active_voices(),
            Instrument::Sampled(sampled) => sampled.active_voices(),
        }
    }

    /// Renders the part's sound into `stereo`, sized to the frames wanted.
    /// Returns `false`, with `stereo` silent, if a sample was not a finite
    /// number: the part has then been silenced and cleared.
    fn render(
        &mut self,
        mono: &mut AudioBuffer<f32>,
        stereo: &mut AudioBuffer<f32>,
        clock: Clock,
    ) -> bool {
        let mono_sound = match &mut self.instrument {
            Instrument::Synth(synth) => {
                synth.process(mono);
                true
            }
            Instrument::Drums(kit) => {
                kit.render(mono.channel_mut(0));
                true
            }
            Instrument::Sampled(sampled) => {
                sampled.render(mono, stereo);
                false
            }
        };
        if mono_sound {
            for channel in stereo.iter_channels_mut() {
                channel.copy_from_slice(mono.channel(0));
            }
        }
        self.chain.process(stereo, clock);
        let finite = stereo
            .iter_channels()
            .all(|channel| channel.iter().all(|sample| sample.is_finite()));
        if !finite {
            self.recover();
            stereo.clear();
        }
        finite
    }

    /// Silences the part and clears every memory in it, keeping its
    /// parameters. Real-time safe.
    fn recover(&mut self) {
        match &mut self.instrument {
            Instrument::Synth(synth) => choke_synth(synth),
            Instrument::Drums(kit) => kit.choke(),
            Instrument::Sampled(sampled) => sampled.choke(),
        }
        self.chain.recover(&self.params.chain);
    }
}

/// The engine: parts that play sample-exact events into a stereo output.
///
/// Time is a frame counter that starts at 0 and moves on by the number of
/// frames each [`render`](Self::render) produces. An event is heard on the
/// frame it names; the engine renders its parts
/// [`master_latency_frames`](Self::master_latency_frames) ahead of the
/// output, for the master bus's limiter to look ahead, so an event lands
/// exactly as long as it arrives that far ahead of the frame being
/// rendered ([`latency_frames`](Self::latency_frames) says how far, with
/// the quantum). Before the first render nothing has been rendered ahead,
/// so every event pushed by then lands exactly. An event further ahead
/// waits, in order, with others on the same frame applied in the order
/// they were pushed; one that arrives too late lands on the first frame
/// the engine renders next, and is heard that many frames into the
/// quantum.
///
/// Control calls (making parts, setting their chains, describing their
/// parameters) allocate and belong outside the audio callback. Pushing
/// events, draining a ring and rendering never allocate, lock or panic.
///
/// ```
/// use manguebyte_web::{Engine, EngineConfig, EventRecord, InstrumentKind};
///
/// let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
/// engine.create_part(0, InstrumentKind::Synth).unwrap();
/// engine.push(EventRecord::note_on(100, 0, 57, 0.9));
///
/// let rendered = engine.render(128);
/// let (left, _right) = engine.output();
/// assert_eq!(rendered, 128);
/// assert!(left[..100].iter().all(|&sample| sample == 0.0));
/// assert!(left[100..].iter().any(|&sample| sample != 0.0));
/// assert_eq!(engine.frame(), 128);
/// ```
pub struct Engine {
    config: EngineConfig,
    spec: ProcessSpec,
    frame: u64,
    parts: Vec<Part>,
    pending: Pending,
    stats: EngineStats,
    mono: AudioBuffer<f32>,
    stereo: AudioBuffer<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
    rendered: usize,
    /// The frames of the last render that produced any, for the latency.
    last_quantum: usize,
    master: Master,
    /// How far ahead of the output the parts are rendered: the master
    /// bus's latency.
    lookahead: usize,
    /// Whether the first `lookahead` frames have been rendered.
    primed: bool,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("config", &self.config)
            .field("frame", &self.frame)
            .field("parts", &self.part_ids().collect::<Vec<_>>())
            .field("pending", &self.pending.len())
            .field("stats", &self.stats)
            .finish()
    }
}

impl Engine {
    /// Makes an engine with no parts. Allocates, and the first time in a
    /// program also makes the tables that sampler parts share
    /// ([`manguebyte_sampler::prepare_tables`]), a few milliseconds of work
    /// that loading an instrument then does not have to do.
    ///
    /// # Errors
    ///
    /// [`EngineError::InvalidConfig`] if a field of `config` is out of range.
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        config.check()?;
        manguebyte_sampler::prepare_tables();
        let frames = config.max_quantum;
        let spec = ProcessSpec {
            sample_rate: config.sample_rate,
            max_block_size: frames,
            num_channels: 2,
        };
        let master = Master::new(&spec);
        Ok(Self {
            config,
            spec,
            lookahead: master.latency(),
            master,
            primed: false,
            frame: 0,
            parts: Vec::with_capacity(config.max_parts),
            pending: Pending::with_capacity(config.pending_capacity),
            stats: EngineStats::default(),
            mono: AudioBuffer::new(1, frames),
            stereo: AudioBuffer::new(2, frames),
            left: vec![0.0; frames],
            right: vec![0.0; frames],
            rendered: 0,
            last_quantum: frames,
        })
    }

    /// The configuration the engine was made with.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// The engine's latency in frames: how far ahead of
    /// [`frame`](Self::frame) an event pushed from another thread must be
    /// due to be sure of landing on its frame, while the engine is rendered
    /// in quanta like the last one. That is one quantum, the size of the
    /// last [`render`](Self::render) (or
    /// [`max_quantum`](EngineConfig::max_quantum) before the first), since
    /// the quantum being rendered may have begun when the event arrives,
    /// and the master bus's look-ahead,
    /// [`master_latency_frames`](Self::master_latency_frames), which the
    /// parts are rendered ahead by. The device's output latency comes
    /// after it.
    ///
    /// ```
    /// use manguebyte_web::{Engine, EngineConfig};
    ///
    /// let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
    /// assert_eq!(engine.master_latency_frames(), 111);
    /// assert_eq!(engine.latency_frames(), 512 + 111);
    /// engine.render(128);
    /// assert_eq!(engine.latency_frames(), 128 + 111);
    /// ```
    pub fn latency_frames(&self) -> usize {
        self.last_quantum + self.lookahead
    }

    /// The frames the master bus's limiter looks ahead by, which the
    /// engine renders its parts ahead of the output by:
    /// [`master_latency_frames`](crate::master_latency_frames) at the
    /// engine's rate, 111 at 48 kHz (2.3 ms). An event is heard on its own
    /// frame all the same, as long as it arrives this far, and a quantum,
    /// ahead ([`latency_frames`](Self::latency_frames)).
    pub fn master_latency_frames(&self) -> usize {
        self.lookahead
    }

    /// The frame the next [`render`](Self::render) starts on.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Counts of what has happened to events.
    pub fn stats(&self) -> EngineStats {
        self.stats
    }

    /// Makes part `part` play `kind` through the full chain, replacing any
    /// part already there. Allocates.
    ///
    /// # Errors
    ///
    /// [`EngineError::TooManyParts`] if `part` is new and every slot is
    /// taken.
    pub fn create_part(&mut self, part: u16, kind: InstrumentKind) -> Result<(), EngineError> {
        self.create_part_with(part, kind, Chain::FULL)
    }

    /// Makes part `part` play `kind` through `chain`, replacing any part
    /// already there. Its parameters start at their defaults. Allocates.
    ///
    /// # Errors
    ///
    /// [`EngineError::TooManyParts`] if `part` is new and every slot is
    /// taken.
    pub fn create_part_with(
        &mut self,
        part: u16,
        kind: InstrumentKind,
        chain: Chain,
    ) -> Result<(), EngineError> {
        let slot = self
            .parts
            .binary_search_by_key(&part, |existing| existing.id);
        if slot.is_err() && self.parts.len() >= self.config.max_parts {
            return Err(EngineError::TooManyParts {
                max: self.config.max_parts,
            });
        }
        let new = Part::new(part, kind, chain, &self.spec);
        match slot {
            Ok(index) => self.parts[index] = new,
            Err(index) => self.parts.insert(index, new),
        }
        Ok(())
    }

    /// Removes a part. Its sound stops at once.
    ///
    /// # Errors
    ///
    /// [`EngineError::NoSuchPart`] if there is none.
    pub fn remove_part(&mut self, part: u16) -> Result<(), EngineError> {
        let index = self.index_of(part)?;
        self.parts.remove(index);
        Ok(())
    }

    /// Changes which optional stages a part's chain has. A stage that is
    /// added for the first time is made now (this is what allocates its
    /// memory, a delay's four seconds of it most of all); one that is added
    /// silent starts with an empty memory; its parameters keep their
    /// values. A stage added with its switch on fades in over
    /// [`EFFECT_FADE_SECONDS`](crate::EFFECT_FADE_SECONDS) from the next
    /// frame rendered, and one taken out fades out, the delay and the
    /// reverb letting their echoes and tails ring on. The parameters of a
    /// stage that is not in the chain are left out of the part's
    /// description, and events that set them are not routed.
    ///
    /// To switch an effect in or out on an exact frame, put it in the chain
    /// beforehand and send its switch (`"delay.on"`) as an event.
    ///
    /// # Errors
    ///
    /// [`EngineError::NoSuchPart`] if there is no such part.
    pub fn set_chain(&mut self, part: u16, chain: Chain) -> Result<(), EngineError> {
        let index = self.index_of(part)?;
        let part = &mut self.parts[index];
        part.chain.set_stages(chain, &part.params.chain);
        part.params.stages = chain;
        Ok(())
    }

    /// Gives a sampler part an SFZ instrument, made with
    /// [`SfzLoad`](crate::SfzLoad). New notes play it from the next frame
    /// rendered; the notes the part's previous instrument had started ring
    /// on with it to their end, and it is freed by a later control call
    /// (this one, or [`collect_instruments`](Self::collect_instruments))
    /// once they have. Allocates, and frees instruments the part is done
    /// with.
    ///
    /// # Errors
    ///
    /// [`EngineError::NoSuchPart`] if there is no such part,
    /// [`EngineError::NotASampler`] if it plays something else, and
    /// [`EngineError::LoaderFull`] if it has been given several instruments
    /// since it last rendered and has taken none of them.
    pub fn load_instrument(
        &mut self,
        part: u16,
        instrument: SfzInstrument,
    ) -> Result<(), EngineError> {
        let index = self.index_of(part)?;
        let Instrument::Sampled(sampled) = &mut self.parts[index].instrument else {
            return Err(EngineError::NotASampler(part));
        };
        sampled
            .load(instrument)
            .map_err(|_| EngineError::LoaderFull(part))
    }

    /// Frees the SFZ instruments that sampler parts have finished with:
    /// those replaced by [`load_instrument`](Self::load_instrument) whose
    /// last note has ended. Returns how many. Not real-time safe: it frees.
    pub fn collect_instruments(&mut self) -> usize {
        self.parts
            .iter_mut()
            .map(|part| match &mut part.instrument {
                Instrument::Sampled(sampled) => sampled.collect(),
                _ => 0,
            })
            .sum()
    }

    fn index_of(&self, part: u16) -> Result<usize, EngineError> {
        self.parts
            .binary_search_by_key(&part, |existing| existing.id)
            .map_err(|_| EngineError::NoSuchPart(part))
    }

    fn part(&self, part: u16) -> Option<&Part> {
        self.index_of(part).ok().map(|index| &self.parts[index])
    }

    /// The parts, in increasing order.
    pub fn part_ids(&self) -> impl Iterator<Item = u16> + '_ {
        self.parts.iter().map(|part| part.id)
    }

    /// What a part plays.
    pub fn part_kind(&self, part: u16) -> Option<InstrumentKind> {
        self.part(part).map(Part::kind)
    }

    /// Which stages a part's chain has.
    pub fn part_chain(&self, part: u16) -> Option<Chain> {
        self.part(part).map(|part| part.params.stages)
    }

    /// How many voices of a part are sounding.
    pub fn active_voices(&self, part: u16) -> Option<usize> {
        self.part(part).map(Part::active_voices)
    }

    /// Whether a part's reverb is asleep: cleared and skipped because
    /// nothing has reached it for a while. A reverb asleep and one that is
    /// running on silence sound the same; this is for tests and meters.
    pub fn reverb_asleep(&self, part: u16) -> Option<bool> {
        self.part(part).map(|part| part.chain.reverb_asleep())
    }

    /// The value a parameter of a part holds, in its own units.
    pub fn param_value(&self, part: u16, id: &str) -> Option<f64> {
        self.part(part)?.params.find(id).map(|param| param.plain())
    }

    /// The value the parameter with this [`ParamId`] holds.
    pub fn param_value_by_id(&self, part: u16, id: ParamId) -> Option<f64> {
        self.part(part)?
            .params
            .find_param(id)
            .map(|param| param.plain())
    }

    /// The parameters of a part as a JSON array, in the format of
    /// [`ParamSet::descriptions_json`]: the instrument's, then those of the
    /// chain stages it has in the order they run, then `gain`, `pan` and
    /// `send`. The object of an effect's parameter has two more fields,
    /// `"effect"`, the effect's [name](Effect::name), and `"visual"`, what
    /// an editor draws for it ([`Visual`](crate::Visual)), as in
    /// `{"id":"delay.time",...,"flags":{...},"effect":"delay","visual":"delay_taps"}`.
    /// Allocates.
    pub fn params_json(&self, part: u16) -> Option<String> {
        self.part(part).map(|part| part.params.tagged_json())
    }

    /// The master bus's parameters as JSON, in the format of
    /// [`params_json`](Self::params_json): `master.tempo`, `master.beat`,
    /// `master.gain`, then those of the shared reverb, the glue compressor
    /// and the limiter. Allocates.
    pub fn master_params_json(&self) -> String {
        self.master.params.tagged_json()
    }

    /// The value a parameter of the master bus holds, in its own units.
    pub fn master_param_value(&self, id: &str) -> Option<f64> {
        self.master.params.find(id).map(|param| param.plain())
    }

    /// The value the master parameter with this [`ParamId`] holds.
    pub fn master_param_value_by_id(&self, id: ParamId) -> Option<f64> {
        self.master.params.find_param(id).map(|param| param.plain())
    }

    /// The curve an editor draws for `effect` of a part, worked out from
    /// the values its parameters hold, with `points` points (2 to
    /// [`MAX_CURVE_POINTS`](crate::MAX_CURVE_POINTS); a delay gives one per
    /// echo, until they fall below -60 dB). The effect does not have to be
    /// in the part's chain. `None` if there is no such part or a part
    /// cannot have the effect. Allocates; it reads the parameters, never
    /// the state the render call works on.
    pub fn effect_curve(&self, part: u16, effect: Effect, points: usize) -> Option<Curve> {
        let tempo = self.master.params.tempo.plain();
        let part = self.part(part)?;
        curve::part_curve(
            &part.params.chain,
            effect,
            self.config.sample_rate,
            tempo,
            points,
        )
    }

    /// The curve of `effect` of the master bus: [`Effect::Glue`],
    /// [`Effect::Limiter`] or [`Effect::Reverb`], as
    /// [`effect_curve`](Self::effect_curve) gives a part's. `None` for any
    /// other effect.
    pub fn master_curve(&self, effect: Effect, points: usize) -> Option<Curve> {
        curve::master_curve(&self.master.params, effect, points)
    }

    /// Whether `effect` of a part has work to do: switched on, fading, or
    /// with echoes or a tail still ringing. An effect that is not is
    /// skipped. `None` if there is no such part.
    pub fn effect_running(&self, part: u16, effect: Effect) -> Option<bool> {
        self.part(part)
            .map(|part| part.chain.effect_running(effect))
    }

    /// Whether the master bus's shared reverb is asleep: nothing has been
    /// sent to it for a while, it has been cleared and is skipped.
    pub fn master_reverb_asleep(&self) -> bool {
        self.master.reverb_asleep()
    }

    /// Whether the master bus's glue compressor is in or fading.
    pub fn glue_running(&self) -> bool {
        self.master.glue_running()
    }

    /// How far the master limiter turned the level down on the last frame
    /// rendered, in decibels, as a positive number. For a meter.
    pub fn limiter_reduction_db(&self) -> f64 {
        self.master.reduction_db()
    }

    /// The text of the value `plain` for a parameter of a part, with its
    /// unit, as [`ParamSet::value_to_text`] gives it. Allocates.
    pub fn value_to_text(&self, part: u16, id: &str, plain: f64) -> Option<String> {
        self.part(part)?.params.value_to_text(id, plain)
    }

    /// Accepts a record. Real-time safe.
    ///
    /// When the queue of waiting events is full, the engine keeps the
    /// events that are due first: a record due before the latest waiting
    /// event takes its place, and that event is dropped; any other record
    /// is dropped. Either way [`EngineStats::dropped`] counts it.
    ///
    /// ```
    /// use manguebyte_web::{Engine, EngineConfig, EventRecord, Pushed};
    ///
    /// let config = EngineConfig { pending_capacity: 1, ..EngineConfig::new(48_000.0) };
    /// let mut engine = Engine::new(config).unwrap();
    /// assert_eq!(engine.push(EventRecord::choke(900, 0)), Pushed::Queued);
    /// assert_eq!(engine.push(EventRecord::choke(900, 0)), Pushed::Full);
    /// // Earlier than everything waiting: it takes the place of frame 900.
    /// assert_eq!(engine.push(EventRecord::choke(5, 0)), Pushed::Queued);
    /// assert_eq!(engine.stats().dropped, 2);
    /// ```
    pub fn push(&mut self, record: EventRecord) -> Pushed {
        let Some(event) = record.decode() else {
            self.stats.ignored += 1;
            return Pushed::Ignored;
        };
        if let Event::Cancel { part } = event {
            let from = record.frame;
            // The master bus's parameters belong to no part, whatever part
            // their record names: a part's cancel leaves them.
            self.stats.cancelled += self.pending.retain(|waiting| {
                waiting.event.part() != part || waiting.frame < from || is_master(waiting.event)
            }) as u64;
        }
        match self.pending.push(record.frame, event) {
            Admitted::Queued => {
                self.stats.queued += 1;
                Pushed::Queued
            }
            Admitted::Displaced => {
                self.stats.queued += 1;
                self.stats.dropped += 1;
                self.stats.displaced += 1;
                Pushed::Queued
            }
            Admitted::Refused => {
                self.stats.dropped += 1;
                Pushed::Full
            }
        }
    }

    /// Whether [`push`](Self::push) would keep this record: it is to be
    /// ignored, or there is room, or it is due before the latest waiting
    /// event. A cancel is always taken, since what it drops matters more
    /// than its own fade, and holding it back would let through the events
    /// it is meant to drop.
    fn admits(&self, record: &EventRecord) -> bool {
        match record.decode() {
            None | Some(Event::Cancel { .. }) => true,
            Some(_) => self.pending.admits(record.frame),
        }
    }

    /// Accepts records laid end to end, 32 bytes each, as a message from
    /// another thread carries them. Bytes after the last whole record are
    /// ignored. Returns how many were queued. Real-time safe.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> usize {
        let mut queued = 0;
        for chunk in bytes.chunks_exact(RECORD_SIZE) {
            let mut record = [0u8; RECORD_SIZE];
            record.copy_from_slice(chunk);
            if self.push(EventRecord::from_bytes(&record)) == Pushed::Queued {
                queued += 1;
            }
        }
        queued
    }

    /// Like [`push_bytes`](Self::push_bytes), but stops before the first
    /// record that a full queue would drop (one due no earlier than every
    /// waiting event), so that the caller can keep it and the records after
    /// it for later. Returns how many records were taken, queued or
    /// ignored, from the start of `bytes`; a stop with whole records left
    /// counts in [`EngineStats::stalled`]. Real-time safe.
    ///
    /// ```
    /// use manguebyte_web::{Engine, EngineConfig, EventRecord};
    ///
    /// let config = EngineConfig { pending_capacity: 1, ..EngineConfig::new(48_000.0) };
    /// let mut engine = Engine::new(config).unwrap();
    /// let mut bytes = Vec::new();
    /// for frame in [1_000, 2_000, 10] {
    ///     bytes.extend_from_slice(&EventRecord::choke(frame, 0).to_bytes());
    /// }
    /// // Frame 1 000 fills the queue; frame 2 000 would be dropped: stop there.
    /// assert_eq!(engine.admit_bytes(&bytes), 1);
    /// assert_eq!(engine.stats().stalled, 1);
    /// ```
    pub fn admit_bytes(&mut self, bytes: &[u8]) -> usize {
        let mut taken = 0;
        for chunk in bytes.chunks_exact(RECORD_SIZE) {
            let mut raw = [0u8; RECORD_SIZE];
            raw.copy_from_slice(chunk);
            let record = EventRecord::from_bytes(&raw);
            if !self.admits(&record) {
                self.stats.stalled += 1;
                break;
            }
            self.push(record);
            taken += 1;
        }
        taken
    }

    /// Moves records from a ring into the engine. Call it before each
    /// [`render`](Self::render). Returns how many records were taken.
    /// Real-time safe.
    ///
    /// While there is room every record is taken. Once the queue of waiting
    /// events is full, a record due before the latest waiting event is
    /// still taken, and that event is dropped to make room
    /// ([`EngineStats::dropped`]), so that events far in the future never
    /// keep an imminent one out. The first record due no earlier than every
    /// waiting event stops the drain, and it and the records behind it stay
    /// in the ring for a later quantum ([`EngineStats::stalled`]).
    pub fn drain(&mut self, ring: &mut RingConsumer) -> usize {
        let mut taken = 0;
        while let Some(record) = ring.peek() {
            if !self.admits(&record) {
                self.stats.stalled += 1;
                break;
            }
            ring.pop();
            self.push(record);
            taken += 1;
        }
        taken
    }

    /// How many more events can wait.
    pub fn pending_room(&self) -> usize {
        self.pending.room()
    }

    /// How many events are waiting.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Forgets every waiting event. Real-time safe.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }

    /// Renders the next `frames` frames, at most
    /// [`max_quantum`](EngineConfig::max_quantum), applying every event on
    /// its frame. Returns how many were rendered; [`output`](Self::output)
    /// holds them. Real-time safe.
    ///
    /// The parts are added up, in increasing part order, with the return
    /// of the master bus's shared reverb, and go through the master bus:
    /// the glue compressor when it is on, `master.gain`, and a look-ahead
    /// limiter, `manguebyte-dsp`'s [`Limiter`](manguebyte_dsp::Limiter),
    /// that keeps the waveform under `master.limiter.ceiling` (-0.3 dBTP
    /// by default), reading it between the samples as well as on them, and
    /// every sample within [`OUTPUT_CEILING`] whatever the ceiling. The
    /// limiter brings the level down in a ramp over the
    /// [`MASTER_LOOKAHEAD_SECONDS`](crate::MASTER_LOOKAHEAD_SECONDS) before
    /// a peak and lets it back up with the release set
    /// ([`LIMITER_RELEASE_SECONDS`] by default); below the ceiling it leaves
    /// the sound exactly as it is. To look ahead, the engine renders its
    /// parts [`master_latency_frames`](Self::master_latency_frames) ahead
    /// of the output; the first render also renders those frames first, so
    /// that every event pushed before it is heard on its frame. A part that
    /// produces something that is not a finite number is silenced and
    /// cleared ([`EngineStats::recovered`]).
    ///
    /// The result does not depend on how the frames are cut into quanta:
    /// rendering 512 frames at once or one at a time gives the same
    /// samples, bit for bit, as long as every event is pushed before its
    /// frame is rendered.
    pub fn render(&mut self, frames: usize) -> usize {
        let frames = frames.min(self.config.max_quantum);
        let _denormals = NoDenormals::new();
        if !self.primed && frames > 0 {
            // The frames the parts run ahead by: what they put out comes
            // out of the master bus after this many frames, so nothing of
            // it is heard yet.
            self.primed = true;
            let mut done = 0;
            while done < self.lookahead {
                let count = (self.lookahead - done).min(self.config.max_quantum);
                self.run(done as u64, count);
                done += count;
            }
        }
        // The parts are `lookahead` frames ahead of the output.
        let start = self.frame.saturating_add(self.lookahead as u64);
        self.run(start, frames);
        self.frame = self.frame.saturating_add(frames as u64);
        self.rendered = frames;
        if frames > 0 {
            self.last_quantum = frames;
        }
        frames
    }

    /// Renders the parts over `frames` frames from `start`, applying the
    /// events due by then, and puts them through the master bus into the
    /// first `frames` frames of the output.
    fn run(&mut self, start: u64, frames: usize) {
        let end = start.saturating_add(frames as u64);
        self.left[..frames].fill(0.0);
        self.right[..frames].fill(0.0);
        self.master.begin(frames);
        let limited = self.master.limited();

        let mut position = 0;
        while let Some(waiting) = self.pending.pop_before(end) {
            let at = waiting.frame.saturating_sub(start) as usize;
            if waiting.frame < start {
                self.stats.late += 1;
            }
            if at > position {
                self.render_span(position, at);
                position = at;
            }
            self.stats.applied += 1;
            let routed = match waiting.event {
                Event::Param { param, value, .. } if params::is_master(param) => {
                    self.master.set_param(param, value)
                }
                event => self
                    .parts
                    .binary_search_by_key(&event.part(), |part| part.id)
                    .is_ok_and(|index| self.parts[index].apply(event)),
            };
            if !routed {
                self.stats.unrouted += 1;
            }
        }
        self.render_span(position, frames);
        self.stats.limited += self.master.limited() - limited;
    }

    /// Renders every part over frames `from..to` of the quantum, adds them
    /// and their sends up, and runs the master bus over them.
    fn render_span(&mut self, from: usize, to: usize) {
        if to <= from {
            return;
        }
        let frames = to - from;
        self.mono.set_num_frames(frames);
        self.stereo.set_num_frames(frames);
        let clock = self.master.clock();
        for part in &mut self.parts {
            if !part.render(&mut self.mono, &mut self.stereo, clock) {
                self.stats.recovered += 1;
                continue;
            }
            for (output, part) in [&mut self.left, &mut self.right]
                .into_iter()
                .zip(self.stereo.iter_channels())
            {
                for (output, &sample) in output[from..to].iter_mut().zip(part) {
                    *output += sample;
                }
            }
            let (send_left, send_right) = self.master.sends(from, to);
            part.chain.send_into(&self.stereo, send_left, send_right);
        }
        self.master
            .process(&mut self.left, &mut self.right, from, to);
    }

    /// The left and right channels of what the last
    /// [`render`](Self::render) produced.
    pub fn output(&self) -> (&[f32], &[f32]) {
        (&self.left[..self.rendered], &self.right[..self.rendered])
    }

    /// Where channel `channel` (0 left, 1 right) of the output starts in
    /// memory. The buffers never move: they hold
    /// [`max_quantum`](EngineConfig::max_quantum) frames from the start.
    pub(crate) fn output_ptr(&self, channel: usize) -> *const f32 {
        match channel {
            0 => self.left.as_ptr(),
            _ => self.right.as_ptr(),
        }
    }

    /// Renders as many frames as `left` and `right` both hold, quantum by
    /// quantum, into them. Real-time safe.
    pub fn render_into(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut done = 0;
        while done < frames {
            let count = self.render(frames - done);
            left[done..done + count].copy_from_slice(&self.left[..count]);
            right[done..done + count].copy_from_slice(&self.right[..count]);
            done += count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_codes_round_trip() {
        for (code, kind) in InstrumentKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.code(), code as u32);
            assert_eq!(InstrumentKind::from_code(kind.code()), Some(kind));
        }
        assert_eq!(
            InstrumentKind::from_code(InstrumentKind::ALL.len() as u32),
            None
        );
        for bits in 0..4 {
            assert_eq!(Chain::from_bits(bits).map(Chain::bits), Some(bits));
        }
        assert_eq!(Chain::from_bits(u32::MAX), None);
    }

    #[test]
    fn bad_configurations_are_refused() {
        let good = EngineConfig::new(48_000.0);
        for (config, field) in [
            (
                EngineConfig {
                    sample_rate: f64::NAN,
                    ..good
                },
                "sample rate",
            ),
            (
                EngineConfig {
                    sample_rate: 1_000.0,
                    ..good
                },
                "sample rate",
            ),
            (
                EngineConfig {
                    sample_rate: 2_999.0,
                    ..good
                },
                "sample rate",
            ),
            (
                EngineConfig {
                    sample_rate: 768_001.0,
                    ..good
                },
                "sample rate",
            ),
            (
                EngineConfig {
                    sample_rate: f64::INFINITY,
                    ..good
                },
                "sample rate",
            ),
            (
                EngineConfig {
                    max_quantum: 0,
                    ..good
                },
                "max quantum",
            ),
            (
                EngineConfig {
                    max_parts: 0,
                    ..good
                },
                "max parts",
            ),
            (
                EngineConfig {
                    pending_capacity: 0,
                    ..good
                },
                "pending capacity",
            ),
        ] {
            assert_eq!(
                Engine::new(config).err(),
                Some(EngineError::InvalidConfig(field))
            );
        }
        assert!(Engine::new(good).is_ok());
        for rate in [EngineConfig::MIN_SAMPLE_RATE, EngineConfig::MAX_SAMPLE_RATE] {
            assert!(Engine::new(EngineConfig::new(rate)).is_ok(), "{rate}");
        }
    }

    fn left_bits(engine: &Engine) -> Vec<u32> {
        engine
            .output()
            .0
            .iter()
            .map(|sample| sample.to_bits())
            .collect()
    }

    /// Nothing outside the engine can put a number that is not finite into
    /// a part (tests/instruments.rs tries), so this test does it from the
    /// inside, to show what the engine does if one ever gets there.
    #[test]
    fn a_part_that_goes_wrong_is_silenced_cleared_and_left_out() {
        let records = [
            // Soft enough that the limiter stays out of the comparison.
            EventRecord::note_on(0, 0, 45, 0.7),
            EventRecord::note_on(0, 1, 38, 0.5),
        ];
        let mut engine = Engine::new(EngineConfig::new(48_000.0)).unwrap();
        engine.create_part(0, InstrumentKind::Synth).unwrap();
        engine.create_part(1, InstrumentKind::Drums).unwrap();
        // The drums alone, for comparison.
        let mut drums = Engine::new(EngineConfig::new(48_000.0)).unwrap();
        drums.create_part(1, InstrumentKind::Drums).unwrap();
        for record in records {
            engine.push(record);
            drums.push(record);
        }
        engine.render(256);
        drums.render(256);
        assert_eq!(engine.active_voices(0), Some(1));

        engine.parts[0].chain.poison();
        engine.render(128);
        drums.render(128);
        assert_eq!(engine.stats().recovered, 1);
        assert_eq!(engine.active_voices(0), Some(0));
        // The parts run ahead of the output by the master's look-ahead:
        // what the synth played before it went wrong is still coming out.
        let ahead = engine.master_latency_frames();
        assert_ne!(left_bits(&engine)[..ahead], left_bits(&drums)[..ahead]);
        assert_eq!(
            left_bits(&engine)[ahead..],
            left_bits(&drums)[ahead..],
            "only the drums"
        );
        assert_eq!(engine.stats().limited, 0);
        assert!(engine.output().1.iter().all(|sample| sample.is_finite()));

        // Cleared, the part plays again, and nothing else goes wrong.
        engine.push(EventRecord::note_on(engine.frame(), 0, 45, 1.0));
        for _ in 0..40 {
            engine.render(128);
            assert!(engine.output().0.iter().all(|sample| sample.is_finite()));
        }
        assert_eq!(engine.active_voices(0), Some(1));
        assert!(engine.output().0.iter().any(|&sample| sample != 0.0));
        assert_eq!(engine.stats().recovered, 1);
        assert_eq!(
            engine.param_value(0, crate::ids::GAIN),
            Some(0.0),
            "the parameters are kept"
        );
    }
}
