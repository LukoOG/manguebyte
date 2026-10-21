//! A part's effect chain: its effects in their fixed order
//! ([`Effect::PART_ORDER`]), then gain and pan, then the send to the
//! master bus's reverb.

use manguebyte_core::{AudioBuffer, vector};
use manguebyte_dsp::{ProcessSpec, Processor, Reverb};
use manguebyte_params::{Param, ParamId, ParamSmoother};

use crate::effect::Effect;
use crate::engine::Chain;
use crate::fx::{
    Bitcrush, Clock, Drive, Echo, Equaliser, Modulated, PhaserFx, ResonantFilter, Squeeze, Stage,
    StageOps, Switch, Wobble, gain_from_db, pan_gains,
};
use crate::params::{self, ChainParams, LEVEL_SMOOTHING};

/// The reverb counts as silent when its output stays below this, -120 dB,
/// while its input is silent.
const REVERB_QUIET: f32 = 1e-6;
/// How many frames the reverb has to stay quiet before it is cleared and
/// put to sleep, at 48 kHz: about 85 ms.
const REVERB_SLEEP_FRAMES_AT_48K: u32 = 4_096;

/// How many frames the reverb has to stay quiet at `sample_rate` before it
/// is cleared and put to sleep.
///
/// The reverb's delay lines are a fixed time long, so they hold more frames
/// at higher rates: at the chain's size the shortest is about 23.5 ms and
/// the longest about 56 ms, and nothing comes out of the reverb until a
/// sound has been round the shortest one. The quiet stretch is therefore
/// the same time at every rate, 4 096 frames at 48 kHz scaled with the
/// rate, longer than every line, so that a reverb is never put to sleep
/// before its first echo.
pub(crate) fn reverb_sleep_frames(sample_rate: f64) -> u32 {
    let scaled = (f64::from(REVERB_SLEEP_FRAMES_AT_48K) * sample_rate / 48_000.0).ceil();
    // Engine configurations stop at 768 kHz: 65 536 frames, far below u32::MAX.
    (scaled as u32).max(1)
}

fn smoother(style: manguebyte_params::Smoothing, sample_rate: f64, value: f32) -> ParamSmoother {
    let mut smoother = ParamSmoother::new(style);
    smoother.prepare(sample_rate, value);
    smoother
}

/// A reverb that is cleared and skipped while nothing reaches it.
pub(crate) struct SleepyReverb {
    pub(crate) reverb: Reverb<f32>,
    asleep: bool,
    quiet_frames: u32,
    sleep_frames: u32,
}

impl SleepyReverb {
    /// `reverb`, prepared for two channels under `spec`.
    pub(crate) fn new(mut reverb: Reverb<f32>, spec: &ProcessSpec) -> Self {
        reverb.prepare(&ProcessSpec {
            num_channels: 2,
            ..*spec
        });
        Self {
            reverb,
            asleep: true,
            quiet_frames: 0,
            sleep_frames: reverb_sleep_frames(spec.sample_rate),
        }
    }

    pub(crate) fn is_asleep(&self) -> bool {
        self.asleep
    }

    /// Clears the tail and puts the reverb to sleep.
    pub(crate) fn clear(&mut self) {
        self.reverb.reset();
        self.asleep = true;
        self.quiet_frames = 0;
    }

    /// Replaces the input in `buffer` with the reverb's output.
    ///
    /// Once its input has been silent and its output below
    /// [`REVERB_QUIET`] for [`reverb_sleep_frames`] frames in a row, longer
    /// than any of its delay lines, it is cleared and skipped until sound
    /// reaches it again. What is cleared away has had time to come out and
    /// was below -120 dB; a cleared reverb fed silence gives exact zeros, so
    /// skipping it changes nothing more. The frame it falls asleep on is
    /// worked out per frame, so the output does not depend on how the
    /// timeline is cut into blocks.
    pub(crate) fn process(&mut self, buffer: &mut AudioBuffer<f32>) {
        let frames = buffer.num_frames();
        let mut start = 0;
        while start < frames {
            if self.asleep {
                // Asleep, silence stays silence: find where sound starts.
                let woken = (start..frames).find(|&frame| {
                    buffer.channel(0)[frame] != 0.0 || buffer.channel(1)[frame] != 0.0
                });
                let Some(woken) = woken else {
                    break;
                };
                self.asleep = false;
                self.quiet_frames = 0;
                start = woken;
            }
            // Run up to the first frame on which it could fall asleep, so
            // that it falls asleep on the same frame however the frames
            // arrive.
            let room = (self.sleep_frames - self.quiet_frames) as usize;
            let end = frames.min(start + room);
            // The reverb overwrites its input, so find the last frame with
            // sound in it first: the quiet run cannot start before it.
            let last_loud = (start..end)
                .rev()
                .find(|&frame| buffer.channel(0)[frame] != 0.0 || buffer.channel(1)[frame] != 0.0);
            buffer.windowed(start..end, |piece| self.reverb.process(piece));
            let quiet_from = last_loud.map_or(start, |frame| frame + 1);
            let run = (quiet_from..end)
                .rev()
                .take_while(|&frame| {
                    buffer.channel(0)[frame].abs() < REVERB_QUIET
                        && buffer.channel(1)[frame].abs() < REVERB_QUIET
                })
                .count();
            self.quiet_frames = if last_loud.is_none() && run == end - start {
                self.quiet_frames + run as u32
            } else {
                run as u32
            };
            if self.quiet_frames >= self.sleep_frames {
                self.clear();
            }
            start = end;
        }
    }
}

/// The part's reverb: wet and dry balanced by `reverb.mix`. Switched off,
/// its input fades out and its tail rings on.
pub(crate) struct PartReverb {
    sleepy: SleepyReverb,
    mix: ParamSmoother,
    switch: Switch,
    /// The signal before the reverb, and the mix and the switch's level of
    /// each frame.
    dry: AudioBuffer<f32>,
    mixes: Vec<f32>,
    levels: Vec<f32>,
    /// Whether nothing came in on the last frame: the switch's fade then
    /// jumps to its end, as the dry level and the reverb's input can change
    /// at once without a step.
    input_quiet: bool,
}

impl PartReverb {
    fn new(params: &ChainParams, spec: &ProcessSpec, on: bool) -> Self {
        let reverb = Reverb::new()
            .with_mix(1.0)
            .with_damping(0.45)
            .with_size(0.6)
            .with_decay_seconds(f64::from(params.reverb.decay.value()));
        Self {
            sleepy: SleepyReverb::new(reverb, spec),
            mix: smoother(LEVEL_SMOOTHING, spec.sample_rate, params.reverb.mix.value()),
            switch: Switch::new(spec.sample_rate, on),
            dry: AudioBuffer::new(2, spec.max_block_size),
            mixes: vec![0.0; spec.max_block_size],
            levels: vec![0.0; spec.max_block_size],
            input_quiet: true,
        }
    }
}

impl StageOps for PartReverb {
    /// The reverb, mixed in by `reverb.mix` and faded in and out by its
    /// switch: `dry × (1 - mix × level) + wet × mix`, its input the dry
    /// signal times the level. While the mix is 0 the reverb hears
    /// nothing.
    fn run(&mut self, buffer: &mut AudioBuffer<f32>, _dry: &mut AudioBuffer<f32>, _clock: Clock) {
        let frames = buffer.num_frames();
        if frames == 0 {
            return;
        }
        if self.switch.is_silent() && self.sleepy.is_asleep() {
            self.input_quiet =
                buffer.channel(0)[frames - 1] == 0.0 && buffer.channel(1)[frames - 1] == 0.0;
            return;
        }
        self.dry.set_num_frames(frames);
        self.dry.copy_from(buffer);
        let (dry_left, dry_right) = (self.dry.channel(0), self.dry.channel(1));
        for (frame, (mix, level)) in self.mixes[..frames]
            .iter_mut()
            .zip(&mut self.levels[..frames])
            .enumerate()
        {
            *mix = self.mix.next_value();
            // A fade only matters while sound comes in.
            if self.input_quiet {
                self.switch.snap();
            }
            *level = self.switch.next();
            self.input_quiet = dry_left[frame] == 0.0 && dry_right[frame] == 0.0;
        }
        // The reverb's input: the dry signal while the mix is above 0,
        // faded by the switch.
        {
            let (left, right) = buffer.channel_pair_mut(0, 1);
            let settings = self.mixes[..frames].iter().zip(&self.levels[..frames]);
            for ((left, right), (&mix, &level)) in left.iter_mut().zip(right).zip(settings) {
                if mix <= 0.0 {
                    *left = 0.0;
                    *right = 0.0;
                } else {
                    *left *= level;
                    *right *= level;
                }
            }
        }
        self.sleepy.process(buffer);
        // `buffer` now holds the wet signal.
        let (left, right) = buffer.channel_pair_mut(0, 1);
        let dry = self.dry.iter_channels().zip([left, right]);
        for (dry, wet) in dry {
            let settings = self.mixes[..frames].iter().zip(&self.levels[..frames]);
            for ((wet, &dry), (&mix, &level)) in wet.iter_mut().zip(dry).zip(settings) {
                *wet = dry * (1.0 - mix * level) + *wet * mix;
            }
        }
    }

    fn set_on(&mut self, on: bool, params: &ChainParams) {
        if on && self.switch.is_silent() && self.sleepy.is_asleep() {
            self.mix.reset(params.reverb.mix.value());
        }
        self.switch.set(on);
    }

    fn set_param(&mut self, id: ParamId, value: f64) {
        match id {
            params::REVERB_MIX => {
                if self.is_idle() {
                    self.mix.reset(value as f32);
                } else {
                    self.mix.set_target(value as f32);
                }
            }
            params::REVERB_DECAY => self.sleepy.reverb.set_decay_seconds(value),
            _ => {}
        }
    }

    fn recover(&mut self, params: &ChainParams) {
        self.sleepy.clear();
        self.mix.reset(params.reverb.mix.value());
        self.sleepy
            .reverb
            .set_decay_seconds(f64::from(params.reverb.decay.value()));
        self.switch.snap();
        self.input_quiet = true;
    }

    fn is_idle(&self) -> bool {
        self.switch.is_silent() && self.sleepy.is_asleep()
    }
}

/// The stages a part can have; those beyond the low-pass and the reverb
/// are made the first time the part's chain has them.
struct Stages {
    /// The stages in the chain: their parameters are the part's, and they
    /// sound while their switch is on.
    present: Chain,
    lowpass: Stage<ResonantFilter>,
    reverb: PartReverb,
    highpass: Option<Box<Stage<ResonantFilter>>>,
    eq: Option<Box<Stage<Equaliser>>>,
    drive: Option<Box<Stage<Drive>>>,
    bitcrush: Option<Box<Stage<Bitcrush>>>,
    compressor: Option<Box<Stage<Squeeze>>>,
    chorus: Option<Box<Stage<Modulated>>>,
    flanger: Option<Box<Stage<Modulated>>>,
    phaser: Option<Box<Stage<PhaserFx>>>,
    tremolo: Option<Box<Stage<Wobble>>>,
    autopan: Option<Box<Stage<Wobble>>>,
    delay: Option<Box<Echo>>,
}

fn boxed<E: StageOps>(stage: &mut Option<Box<E>>) -> Option<&mut dyn StageOps> {
    stage.as_deref_mut().map(|stage| stage as &mut dyn StageOps)
}

fn shared<E: StageOps>(stage: &Option<Box<E>>) -> Option<&dyn StageOps> {
    stage.as_deref().map(|stage| stage as &dyn StageOps)
}

impl Stages {
    fn get_mut(&mut self, effect: Effect) -> Option<&mut dyn StageOps> {
        match effect {
            Effect::Lowpass => Some(&mut self.lowpass),
            Effect::Reverb => Some(&mut self.reverb),
            Effect::Highpass => boxed(&mut self.highpass),
            Effect::Eq => boxed(&mut self.eq),
            Effect::Drive => boxed(&mut self.drive),
            Effect::Bitcrush => boxed(&mut self.bitcrush),
            Effect::Compressor => boxed(&mut self.compressor),
            Effect::Chorus => boxed(&mut self.chorus),
            Effect::Flanger => boxed(&mut self.flanger),
            Effect::Phaser => boxed(&mut self.phaser),
            Effect::Tremolo => boxed(&mut self.tremolo),
            Effect::Autopan => boxed(&mut self.autopan),
            Effect::Delay => boxed(&mut self.delay),
            Effect::Glue | Effect::Limiter => None,
        }
    }

    fn get(&self, effect: Effect) -> Option<&dyn StageOps> {
        match effect {
            Effect::Lowpass => Some(&self.lowpass),
            Effect::Reverb => Some(&self.reverb),
            Effect::Highpass => shared(&self.highpass),
            Effect::Eq => shared(&self.eq),
            Effect::Drive => shared(&self.drive),
            Effect::Bitcrush => shared(&self.bitcrush),
            Effect::Compressor => shared(&self.compressor),
            Effect::Chorus => shared(&self.chorus),
            Effect::Flanger => shared(&self.flanger),
            Effect::Phaser => shared(&self.phaser),
            Effect::Tremolo => shared(&self.tremolo),
            Effect::Autopan => shared(&self.autopan),
            Effect::Delay => shared(&self.delay),
            Effect::Glue | Effect::Limiter => None,
        }
    }

    /// Makes `effect`'s stage if it has none, fully in if `on`, silent
    /// otherwise. Allocates.
    fn make(&mut self, effect: Effect, params: &ChainParams, spec: &ProcessSpec, on: bool) {
        let rate = spec.sample_rate;
        fn stage<E: crate::fx::Insert>(
            slot: &mut Option<Box<Stage<E>>>,
            rate: f64,
            on: bool,
            make: impl FnOnce() -> E,
        ) {
            if slot.is_none() {
                *slot = Some(Box::new(Stage::new(make(), rate, on)));
            }
        }
        match effect {
            Effect::Highpass => stage(&mut self.highpass, rate, on, || {
                ResonantFilter::new(Effect::Highpass, params, spec)
            }),
            Effect::Eq => stage(&mut self.eq, rate, on, || Equaliser::new(params, spec)),
            Effect::Drive => stage(&mut self.drive, rate, on, || Drive::new(params, spec)),
            Effect::Bitcrush => stage(&mut self.bitcrush, rate, on, || Bitcrush::new(params, spec)),
            Effect::Compressor => stage(&mut self.compressor, rate, on, || {
                Squeeze::new(params, spec)
            }),
            Effect::Chorus => stage(&mut self.chorus, rate, on, || {
                Modulated::new(Effect::Chorus, params, spec)
            }),
            Effect::Flanger => stage(&mut self.flanger, rate, on, || {
                Modulated::new(Effect::Flanger, params, spec)
            }),
            Effect::Phaser => stage(&mut self.phaser, rate, on, || PhaserFx::new(params, spec)),
            Effect::Tremolo => stage(&mut self.tremolo, rate, on, || {
                Wobble::new(Effect::Tremolo, params, spec)
            }),
            Effect::Autopan => stage(&mut self.autopan, rate, on, || {
                Wobble::new(Effect::Autopan, params, spec)
            }),
            Effect::Delay => {
                if self.delay.is_none() {
                    self.delay = Some(Box::new(Echo::new(params, spec, on)));
                }
            }
            Effect::Lowpass | Effect::Reverb | Effect::Glue | Effect::Limiter => {}
        }
    }
}

/// Whether `effect` sounds with these stages and parameters: in the chain,
/// and switched on.
fn wanted(effect: Effect, present: Chain, params: &ChainParams) -> bool {
    present.has(effect) && params.switch(effect).is_some_and(|switch| switch.value())
}

/// The chain's signal processing, with its own scratch space.
pub(crate) struct ChainDsp {
    spec: ProcessSpec,
    stages: Stages,
    /// What went into a stage that is fading in or out.
    dry: AudioBuffer<f32>,
    gain: ParamSmoother,
    pan: ParamSmoother,
    pan_setting: (f32, (f32, f32)),
    send: ParamSmoother,
    /// Whether the part was silent on the last frame.
    send_quiet: bool,
}

impl ChainDsp {
    /// The chain of a new part with the stages in `present`. Allocates
    /// every stage it has.
    pub(crate) fn new(params: &ChainParams, present: Chain, spec: &ProcessSpec) -> Self {
        let rate = spec.sample_rate;
        let stereo = ProcessSpec {
            num_channels: 2,
            ..*spec
        };
        let pan = params.pan.value();
        let mut stages = Stages {
            present,
            lowpass: Stage::new(
                ResonantFilter::new(Effect::Lowpass, params, &stereo),
                rate,
                wanted(Effect::Lowpass, present, params),
            ),
            reverb: PartReverb::new(params, &stereo, wanted(Effect::Reverb, present, params)),
            highpass: None,
            eq: None,
            drive: None,
            bitcrush: None,
            compressor: None,
            chorus: None,
            flanger: None,
            phaser: None,
            tremolo: None,
            autopan: None,
            delay: None,
        };
        for effect in Effect::PART_ORDER {
            if present.has(effect) {
                stages.make(effect, params, &stereo, wanted(effect, present, params));
            }
        }
        Self {
            spec: stereo,
            stages,
            dry: AudioBuffer::new(2, spec.max_block_size),
            gain: smoother(
                LEVEL_SMOOTHING,
                rate,
                gain_from_db(f64::from(params.gain.value())),
            ),
            pan: smoother(LEVEL_SMOOTHING, rate, pan),
            pan_setting: (pan, pan_gains(pan)),
            send: smoother(LEVEL_SMOOTHING, rate, params.send.value()),
            send_quiet: true,
        }
    }

    /// Changes which stages are in the chain. A stage that joins it is
    /// made if it has never been, and fades in if its switch is on; one
    /// that leaves fades out, the delay and the reverb ringing on. A stage
    /// that joins silent starts with an empty memory. Allocates.
    pub(crate) fn set_stages(&mut self, present: Chain, params: &ChainParams) {
        for effect in Effect::PART_ORDER {
            if present.has(effect) {
                self.stages.make(effect, params, &self.spec, false);
            }
        }
        self.stages.present = present;
        for effect in Effect::PART_ORDER {
            let on = wanted(effect, present, params);
            if let Some(stage) = self.stages.get_mut(effect) {
                stage.set_on(on, params);
            }
        }
    }

    /// Takes a new value; `false` if `id` is not a chain parameter.
    pub(crate) fn set_param(&mut self, id: ParamId, value: f64, params: &ChainParams) -> bool {
        match id {
            params::GAIN => self.gain.set_target(gain_from_db(value)),
            params::PAN => self.pan.set_target(value as f32),
            params::SEND => self.send.set_target(value as f32),
            _ => {
                let Some(effect) = params::part_effect_of(id) else {
                    return false;
                };
                let present = self.stages.present;
                let is_switch = params
                    .switch(effect)
                    .is_some_and(|switch| switch.param_id() == id);
                if let Some(stage) = self.stages.get_mut(effect) {
                    if is_switch {
                        stage.set_on(value >= 0.5 && present.has(effect), params);
                    } else {
                        stage.set_param(id, value);
                    }
                }
            }
        }
        true
    }

    /// Whether the reverb is cleared and skipped until sound reaches it.
    pub(crate) fn reverb_asleep(&self) -> bool {
        self.stages.reverb.sleepy.is_asleep()
    }

    /// Whether `effect` has work to do: in the chain and on, fading, or
    /// with echoes or a tail still ringing.
    pub(crate) fn effect_running(&self, effect: Effect) -> bool {
        self.stages
            .get(effect)
            .is_some_and(|stage| !stage.is_idle())
    }

    /// Processes `buffer`, two channels, in place: every stage in order,
    /// then gain and pan.
    pub(crate) fn process(&mut self, buffer: &mut AudioBuffer<f32>, clock: Clock) {
        for effect in Effect::PART_ORDER {
            if let Some(stage) = self.stages.get_mut(effect) {
                stage.run(buffer, &mut self.dry, clock);
            }
        }
        self.gain_and_pan(buffer);
    }

    /// Adds `buffer`, the part's output, times the send to `left` and
    /// `right`, the master bus's reverb input. Does nothing while the send
    /// is 0.
    pub(crate) fn send_into(
        &mut self,
        buffer: &AudioBuffer<f32>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let frames = buffer.num_frames();
        let (from_left, from_right) = (buffer.channel(0), buffer.channel(1));
        if frames == 0 {
            return;
        }
        // A change of send only glides while sound goes out: after a
        // silent frame it applies at once, so that a send set with a note
        // sends the note from its first frame.
        if self.send_quiet && self.send.is_smoothing() {
            self.send.reset(self.send.target());
        }
        if !self.send.is_smoothing() && self.send.current() == 0.0 {
            self.send_quiet = from_left[frames - 1] == 0.0 && from_right[frames - 1] == 0.0;
            return;
        }
        if !self.send.is_smoothing() {
            // A steady send: every frame gets the same gain, and resetting
            // to the target would change nothing.
            let send = self.send.next_value();
            vector::add_scaled(&mut left[..frames], from_left, send);
            vector::add_scaled(&mut right[..frames], from_right, send);
            self.send_quiet = from_left[frames - 1] == 0.0 && from_right[frames - 1] == 0.0;
            return;
        }
        for frame in 0..frames {
            if self.send_quiet {
                self.send.reset(self.send.target());
            }
            let send = self.send.next_value();
            left[frame] += from_left[frame] * send;
            right[frame] += from_right[frame] * send;
            self.send_quiet = from_left[frame] == 0.0 && from_right[frame] == 0.0;
        }
    }

    /// A chain that holds only silent stages for `effects`, each made as
    /// [`set_stages`](Self::set_stages) makes a stage that joins a part's
    /// chain for the first time, from the parameters' defaults, which is
    /// what a part's parameters of an effect it has never had hold: the
    /// stages another chain can [`adopt`](Self::adopt_stages). Allocates.
    pub(crate) fn stage_donor(effects: Chain, spec: &ProcessSpec) -> Self {
        let params = ChainParams::new();
        let mut donor = Self::new(&params, Chain::NONE, spec);
        let stereo = donor.spec;
        for effect in Effect::PART_ORDER {
            if effects.has(effect) {
                donor.stages.make(effect, &params, &stereo, false);
            }
        }
        donor
    }

    /// The effects of `effects` whose stage this chain has never made:
    /// those [`set_stages`](Self::set_stages) would allocate for.
    pub(crate) fn unmade(&self, effects: Chain) -> Chain {
        Effect::PART_ORDER
            .into_iter()
            .filter(|&effect| effects.has(effect) && self.stages.get(effect).is_none())
            .fold(Chain::NONE, Chain::with)
    }

    /// Moves in the stages `donor` holds for the effects of `effects` this
    /// chain has never made, leaving the others with `donor`. Never
    /// allocates or frees.
    pub(crate) fn adopt_stages(&mut self, donor: &mut Self, effects: Chain) {
        macro_rules! adopt {
            ($($field:ident: $effect:ident),* $(,)?) => {$(
                if effects.has(Effect::$effect) && self.stages.$field.is_none() {
                    self.stages.$field = donor.stages.$field.take();
                }
            )*};
        }
        adopt!(
            highpass: Highpass,
            eq: Eq,
            drive: Drive,
            bitcrush: Bitcrush,
            compressor: Compressor,
            chorus: Chorus,
            flanger: Flanger,
            phaser: Phaser,
            tremolo: Tremolo,
            autopan: Autopan,
            delay: Delay,
        );
    }

    /// Starts again from `params` after something that is not a number got
    /// in: every memory cleared, every smoother at its parameter's value.
    /// Real-time safe.
    pub(crate) fn recover(&mut self, params: &ChainParams) {
        for effect in Effect::PART_ORDER {
            if let Some(stage) = self.stages.get_mut(effect) {
                stage.recover(params);
            }
        }
        self.gain
            .reset(gain_from_db(f64::from(params.gain.value())));
        let pan = params.pan.value();
        self.pan.reset(pan);
        self.pan_setting = (pan, pan_gains(pan));
        self.send.reset(params.send.value());
        self.send_quiet = true;
    }

    /// Lets a non-finite number into the gain, as nothing outside this
    /// module can, so that the engine's recovery can be tested.
    #[cfg(test)]
    pub(crate) fn poison(&mut self) {
        self.gain.reset(f32::NAN);
    }

    fn gain_and_pan(&mut self, buffer: &mut AudioBuffer<f32>) {
        let (left, right) = buffer.channel_pair_mut(0, 1);
        for (left, right) in left.iter_mut().zip(right) {
            let gain = self.gain.next_value();
            let pan = self.pan.next_value();
            if pan != self.pan_setting.0 {
                self.pan_setting = (pan, pan_gains(pan));
            }
            let (left_gain, right_gain) = self.pan_setting.1;
            *left *= gain * left_gain;
            *right *= gain * right_gain;
        }
    }
}
