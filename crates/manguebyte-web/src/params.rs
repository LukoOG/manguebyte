//! The parameters of a part and of the master bus, by stable text
//! identifier.
//!
//! An event names a parameter by its [`ParamId`], the 32-bit FNV-1a hash
//! of the identifier ([`ParamId::from_name`]); the JSON from
//! [`Engine::params_json`](crate::Engine::params_json) and
//! [`Engine::master_params_json`](crate::Engine::master_params_json) gives
//! both.

use manguebyte_params::{
    BoolParam, ChoiceParam, FloatParam, FloatRange, IntParam, Param, ParamId, ParamSet, Smoothing,
};

use crate::effect::Effect;
use crate::engine::{Chain, InstrumentKind};

/// The text identifiers of every parameter a part or the master bus can
/// have.
///
/// An effect's parameters are named after it: `"delay.time"`,
/// `"eq.mid_gain"`. Each effect has an `on` switch (`"delay.on"`), and
/// its other parameters are listed in the order a method chain's
/// positional arguments are meant to fill them: `delay(0.25, 0.5)` is a
/// time of 0.25 s and a feedback of 0.5. The master bus's parameters start
/// with `"master."`.
pub mod ids {
    /// Chain low-pass switch: 1 on (the default), 0 off.
    pub const LOWPASS_ON: &str = "lowpass.on";
    /// Chain low-pass cutoff, hertz.
    pub const LOWPASS_CUTOFF: &str = "lowpass.cutoff";
    /// Chain low-pass resonance, 0 to 1.
    pub const LOWPASS_RESONANCE: &str = "lowpass.resonance";
    /// Chain reverb switch: 1 on (the default), 0 off.
    pub const REVERB_ON: &str = "reverb.on";
    /// Chain reverb balance, 0 (dry) to 1 (wet).
    pub const REVERB_MIX: &str = "reverb.mix";
    /// Chain reverb decay time, seconds.
    pub const REVERB_DECAY: &str = "reverb.decay";
    /// Part level, decibels; the lowest value is silence.
    pub const GAIN: &str = "gain";
    /// Part balance, -1 (left) to 1 (right).
    pub const PAN: &str = "pan";
    /// How much of the part, after gain and pan, goes to the master bus's
    /// shared reverb, 0 to 1.
    pub const SEND: &str = "send";

    /// High-pass switch, 0 (the default) or 1.
    pub const HIGHPASS_ON: &str = "highpass.on";
    /// High-pass cutoff, hertz.
    pub const HIGHPASS_CUTOFF: &str = "highpass.cutoff";
    /// High-pass resonance, 0 to 1.
    pub const HIGHPASS_RESONANCE: &str = "highpass.resonance";

    /// Equaliser switch.
    pub const EQ_ON: &str = "eq.on";
    /// Low shelf gain, decibels.
    pub const EQ_LOW_GAIN: &str = "eq.low_gain";
    /// Middle band (peak) gain, decibels.
    pub const EQ_MID_GAIN: &str = "eq.mid_gain";
    /// High shelf gain, decibels.
    pub const EQ_HIGH_GAIN: &str = "eq.high_gain";
    /// Low shelf corner, hertz.
    pub const EQ_LOW_FREQ: &str = "eq.low_freq";
    /// Middle band centre, hertz.
    pub const EQ_MID_FREQ: &str = "eq.mid_freq";
    /// Middle band width as Q.
    pub const EQ_MID_Q: &str = "eq.mid_q";
    /// High shelf corner, hertz.
    pub const EQ_HIGH_FREQ: &str = "eq.high_freq";

    /// Drive switch.
    pub const DRIVE_ON: &str = "drive.on";
    /// Gain into the curve, decibels.
    pub const DRIVE_AMOUNT: &str = "drive.amount";
    /// Gain after the curve, decibels.
    pub const DRIVE_OUTPUT: &str = "drive.output";
    /// Balance of the clean (0) and the driven (1) signal.
    pub const DRIVE_MIX: &str = "drive.mix";
    /// The curve: 0 tanh, 1 soft clip, 2 hard clip, 3 fold, 4 tube
    /// ([`tube_curve`](crate::tube_curve)).
    pub const DRIVE_SHAPE: &str = "drive.shape";

    /// Bitcrusher switch.
    pub const BITCRUSH_ON: &str = "bitcrush.on";
    /// Bit depth, 1 to 16; fractions give the step sizes between.
    pub const BITCRUSH_BITS: &str = "bitcrush.bits";
    /// The rate the signal is held at, hertz.
    pub const BITCRUSH_RATE: &str = "bitcrush.rate";
    /// Balance of the clean (0) and the crushed (1) signal.
    pub const BITCRUSH_MIX: &str = "bitcrush.mix";

    /// Compressor switch.
    pub const COMPRESSOR_ON: &str = "compressor.on";
    /// Threshold, dBFS.
    pub const COMPRESSOR_THRESHOLD: &str = "compressor.threshold";
    /// Ratio, 1 to 20.
    pub const COMPRESSOR_RATIO: &str = "compressor.ratio";
    /// Attack, milliseconds.
    pub const COMPRESSOR_ATTACK: &str = "compressor.attack";
    /// Release, milliseconds.
    pub const COMPRESSOR_RELEASE: &str = "compressor.release";
    /// Knee width, decibels.
    pub const COMPRESSOR_KNEE: &str = "compressor.knee";
    /// Make-up gain, decibels.
    pub const COMPRESSOR_MAKEUP: &str = "compressor.makeup";
    /// Balance of the dry (0) and the compressed (1) signal: parallel
    /// compression.
    pub const COMPRESSOR_MIX: &str = "compressor.mix";

    /// Chorus switch.
    pub const CHORUS_ON: &str = "chorus.on";
    /// Balance of the dry (0) and the chorused (1) signal.
    pub const CHORUS_MIX: &str = "chorus.mix";
    /// Sweep rate, hertz.
    pub const CHORUS_RATE: &str = "chorus.rate";
    /// Sweep either side of the centre delay, milliseconds.
    pub const CHORUS_DEPTH: &str = "chorus.depth";
    /// Centre delay, milliseconds.
    pub const CHORUS_DELAY: &str = "chorus.delay";
    /// Voices, 1 to 8.
    pub const CHORUS_VOICES: &str = "chorus.voices";
    /// How far apart the channels sweep, 0 to 1.
    pub const CHORUS_SPREAD: &str = "chorus.spread";

    /// Flanger switch.
    pub const FLANGER_ON: &str = "flanger.on";
    /// Balance of the dry (0) and the flanged (1) signal.
    pub const FLANGER_MIX: &str = "flanger.mix";
    /// Sweep rate, hertz.
    pub const FLANGER_RATE: &str = "flanger.rate";
    /// Sweep either side of the centre delay, milliseconds.
    pub const FLANGER_DEPTH: &str = "flanger.depth";
    /// Feedback, -0.95 to 0.95.
    pub const FLANGER_FEEDBACK: &str = "flanger.feedback";
    /// Centre delay, milliseconds.
    pub const FLANGER_DELAY: &str = "flanger.delay";

    /// Phaser switch.
    pub const PHASER_ON: &str = "phaser.on";
    /// Balance of the dry (0) and the phased (1) signal; 0.5 gives the
    /// deepest notches.
    pub const PHASER_MIX: &str = "phaser.mix";
    /// Sweep rate, hertz.
    pub const PHASER_RATE: &str = "phaser.rate";
    /// Sweep either side of the centre, octaves.
    pub const PHASER_DEPTH: &str = "phaser.depth";
    /// Centre of the sweep, hertz.
    pub const PHASER_CENTER: &str = "phaser.center";
    /// Feedback, -0.95 to 0.95.
    pub const PHASER_FEEDBACK: &str = "phaser.feedback";
    /// All-pass stages, 2 to 12; every two make a notch.
    pub const PHASER_STAGES: &str = "phaser.stages";
    /// How far apart the channels sweep, 0 to 1.
    pub const PHASER_SPREAD: &str = "phaser.spread";

    /// Tremolo switch.
    pub const TREMOLO_ON: &str = "tremolo.on";
    /// Rate, hertz, when `tremolo.sync` is 0.
    pub const TREMOLO_RATE: &str = "tremolo.rate";
    /// Depth, 0 to 1: how far the gain dips.
    pub const TREMOLO_DEPTH: &str = "tremolo.depth";
    /// The wave: 0 sine, 1 triangle, 2 square, 3 saw up, 4 saw down.
    pub const TREMOLO_SHAPE: &str = "tremolo.shape";
    /// Beats per cycle at `master.tempo`, locked to `master.beat`; 0 uses
    /// `tremolo.rate`.
    pub const TREMOLO_SYNC: &str = "tremolo.sync";

    /// Auto-pan switch.
    pub const AUTOPAN_ON: &str = "autopan.on";
    /// Rate, hertz, when `autopan.sync` is 0.
    pub const AUTOPAN_RATE: &str = "autopan.rate";
    /// Depth, 0 to 1: how far from the centre it swings.
    pub const AUTOPAN_DEPTH: &str = "autopan.depth";
    /// The wave, as for the tremolo.
    pub const AUTOPAN_SHAPE: &str = "autopan.shape";
    /// Beats per cycle, as for the tremolo.
    pub const AUTOPAN_SYNC: &str = "autopan.sync";

    /// Delay switch.
    pub const DELAY_ON: &str = "delay.on";
    /// Delay time, seconds, when `delay.sync` is 0.
    pub const DELAY_TIME: &str = "delay.time";
    /// How much of each echo comes back as the next, 0 to 0.95.
    pub const DELAY_FEEDBACK: &str = "delay.feedback";
    /// Balance of the dry (0) and the echoes (1).
    pub const DELAY_MIX: &str = "delay.mix";
    /// How much darker each echo is than the one before, 0 to 1.
    pub const DELAY_DAMPING: &str = "delay.damping";
    /// 0 echoes each side on itself; 1 bounces them from side to side.
    pub const DELAY_PINGPONG: &str = "delay.pingpong";
    /// Delay time in beats at `master.tempo`; 0 uses `delay.time`.
    pub const DELAY_SYNC: &str = "delay.sync";

    /// The tempo synced effects follow, beats per minute.
    pub const MASTER_TEMPO: &str = "master.tempo";
    /// The beat the engine is at on the frame this is set: synced
    /// modulation starts a cycle on beat 0 and every multiple of its
    /// length. The engine counts on from it at `master.tempo`.
    pub const MASTER_BEAT: &str = "master.beat";
    /// Master level into the limiter, decibels.
    pub const MASTER_GAIN: &str = "master.gain";
    /// Glue compressor switch, 0 (the default) or 1.
    pub const GLUE_ON: &str = "master.glue.on";
    /// Glue threshold, dBFS.
    pub const GLUE_THRESHOLD: &str = "master.glue.threshold";
    /// Glue ratio.
    pub const GLUE_RATIO: &str = "master.glue.ratio";
    /// Glue attack, milliseconds.
    pub const GLUE_ATTACK: &str = "master.glue.attack";
    /// Glue release, milliseconds.
    pub const GLUE_RELEASE: &str = "master.glue.release";
    /// Glue knee, decibels.
    pub const GLUE_KNEE: &str = "master.glue.knee";
    /// Glue make-up gain, decibels.
    pub const GLUE_MAKEUP: &str = "master.glue.makeup";
    /// Level of the shared reverb's return, decibels.
    pub const MASTER_REVERB_LEVEL: &str = "master.reverb.level";
    /// Decay of the shared reverb, seconds.
    pub const MASTER_REVERB_DECAY: &str = "master.reverb.decay";
    /// Size of the shared reverb's space, 0 to 1.
    pub const MASTER_REVERB_SIZE: &str = "master.reverb.size";
    /// Damping of the shared reverb, 0 (bright) to 1 (dark).
    pub const MASTER_REVERB_DAMPING: &str = "master.reverb.damping";
    /// The level no output sample, nor the waveform between samples, goes
    /// beyond, dBTP.
    pub const LIMITER_CEILING: &str = "master.limiter.ceiling";
    /// How fast the limiter lets the level back up, milliseconds.
    pub const LIMITER_RELEASE: &str = "master.limiter.release";

    /// Synth waveform: 0 saw, 1 square, 2 triangle, 3 sine.
    pub const SYNTH_WAVE: &str = "synth.wave";
    /// Synth detune between its two oscillators, cents.
    pub const SYNTH_DETUNE: &str = "synth.detune";
    /// Synth sub-oscillator level, 0 to 1.
    pub const SYNTH_SUB: &str = "synth.sub";
    /// Synth filter cutoff, hertz.
    pub const SYNTH_CUTOFF: &str = "synth.cutoff";
    /// Synth filter resonance, 0 to 1.
    pub const SYNTH_RESONANCE: &str = "synth.resonance";
    /// How far the filter envelope opens the synth filter, octaves.
    pub const SYNTH_ENV_AMOUNT: &str = "synth.env_amount";
    /// Synth filter envelope decay, seconds.
    pub const SYNTH_FILTER_DECAY: &str = "synth.filter_decay";
    /// Synth amplitude attack, seconds.
    pub const SYNTH_ATTACK: &str = "synth.attack";
    /// Synth amplitude decay, seconds.
    pub const SYNTH_DECAY: &str = "synth.decay";
    /// Synth sustain level, 0 to 1.
    pub const SYNTH_SUSTAIN: &str = "synth.sustain";
    /// Synth release, seconds.
    pub const SYNTH_RELEASE: &str = "synth.release";

    /// Drum kit tuning, semitones.
    pub const KIT_TUNE: &str = "kit.tune";
    /// Drum kit decay, as a factor on every drum's length.
    pub const KIT_DECAY: &str = "kit.decay";
    /// Kick level, 0 to 1.
    pub const KICK_LEVEL: &str = "kick.level";
    /// Snare level, 0 to 1.
    pub const SNARE_LEVEL: &str = "snare.level";
    /// Closed hi-hat level, 0 to 1.
    pub const CLOSED_HAT_LEVEL: &str = "closed_hat.level";
    /// Open hi-hat level, 0 to 1.
    pub const OPEN_HAT_LEVEL: &str = "open_hat.level";
    /// Ride level, 0 to 1.
    pub const RIDE_LEVEL: &str = "ride.level";
    /// Crash level, 0 to 1.
    pub const CRASH_LEVEL: &str = "crash.level";
    /// High tom level, 0 to 1.
    pub const HIGH_TOM_LEVEL: &str = "high_tom.level";
    /// Mid tom level, 0 to 1.
    pub const MID_TOM_LEVEL: &str = "mid_tom.level";
    /// Floor tom level, 0 to 1.
    pub const FLOOR_TOM_LEVEL: &str = "floor_tom.level";
    /// Side stick level, 0 to 1.
    pub const SIDE_STICK_LEVEL: &str = "side_stick.level";
    /// Guitar body tap level, 0 to 1.
    pub const GUITAR_TAP_LEVEL: &str = "guitar_tap.level";

    /// How a sampler part reads its samples between their frames: 0 cubic,
    /// 1 sinc.
    pub const SAMPLER_INTERPOLATION: &str = "sampler.interpolation";
    /// Whether a sampler part plays its instrument at the loudness target:
    /// 1 (the default) at the make-up gain measured when the instrument
    /// loaded, 0 at the level of its samples.
    pub const SAMPLER_NORMALIZE: &str = "sampler.normalize";

    /// Plucked string preset: 0 nylon guitar, 1 steel guitar, 2 finger
    /// bass, 3 slap bass. Setting it moves the other `plucked.*` knobs to
    /// the preset's positions.
    pub const PLUCKED_PRESET: &str = "plucked.preset";
    /// How long the fundamental of middle C rings while held, to -60 dB,
    /// seconds; lower notes ring longer, higher ones shorter.
    pub const PLUCKED_DECAY: &str = "plucked.decay";
    /// The brightness of a pluck at full velocity: the cut-off of the
    /// low-pass that shapes it, hertz.
    pub const PLUCKED_BRIGHTNESS: &str = "plucked.brightness";
    /// Where the string is plucked, as a fraction of its length from the
    /// bridge, 0.02 to 0.5.
    pub const PLUCKED_POSITION: &str = "plucked.position";
    /// The cut-off of the final low-pass (the pickup or the body), hertz.
    pub const PLUCKED_TONE: &str = "plucked.tone";
    /// How long a released string takes to fall 60 dB, seconds.
    pub const PLUCKED_RELEASE: &str = "plucked.release";

    /// FM preset: 0 electric piano, 1 tubular bell, 2 FM bass. Setting it
    /// puts the other `fm.*` knobs back to 1.
    pub const FM_PRESET: &str = "fm.preset";
    /// A factor on every modulator's index: above 1 brighter, below
    /// darker, 0 the carriers alone.
    pub const FM_BRIGHTNESS: &str = "fm.brightness";
    /// A factor on every operator's decay time.
    pub const FM_DECAY: &str = "fm.decay";
    /// A factor on every operator's release time.
    pub const FM_RELEASE: &str = "fm.release";

    /// Organ preset: 0 jazz, 1 gospel, 2 full, 3 ballad. Setting it moves
    /// the drawbars, the percussion, the click and the leakage to the
    /// preset's positions.
    pub const ORGAN_PRESET: &str = "organ.preset";
    /// The 16' drawbar, 0 (in) to 8 (all the way out), 3 dB a step.
    pub const ORGAN_DRAWBAR_16: &str = "organ.drawbar_16";
    /// The 5 1/3' drawbar.
    pub const ORGAN_DRAWBAR_5_1_3: &str = "organ.drawbar_5_1_3";
    /// The 8' drawbar, the pitch of the key.
    pub const ORGAN_DRAWBAR_8: &str = "organ.drawbar_8";
    /// The 4' drawbar.
    pub const ORGAN_DRAWBAR_4: &str = "organ.drawbar_4";
    /// The 2 2/3' drawbar.
    pub const ORGAN_DRAWBAR_2_2_3: &str = "organ.drawbar_2_2_3";
    /// The 2' drawbar.
    pub const ORGAN_DRAWBAR_2: &str = "organ.drawbar_2";
    /// The 1 3/5' drawbar.
    pub const ORGAN_DRAWBAR_1_3_5: &str = "organ.drawbar_1_3_5";
    /// The 1 1/3' drawbar.
    pub const ORGAN_DRAWBAR_1_1_3: &str = "organ.drawbar_1_1_3";
    /// The 1' drawbar, silent while the percussion is on.
    pub const ORGAN_DRAWBAR_1: &str = "organ.drawbar_1";
    /// The percussion: 0 off, 1 second harmonic fast, 2 second slow, 3
    /// third fast, 4 third slow.
    pub const ORGAN_PERCUSSION: &str = "organ.percussion";
    /// How much the keys click, 0 to 1.
    pub const ORGAN_CLICK: &str = "organ.click";
    /// How much of the neighbouring tonewheels leaks in, 0 to 1.
    pub const ORGAN_LEAKAGE: &str = "organ.leakage";
    /// How much the level follows the velocity, 0 (none, as on the
    /// instrument) to 1 (the lowest velocity 24 dB down).
    pub const ORGAN_VELOCITY: &str = "organ.velocity";
    /// The rotating speaker's speed: 0 stop, 1 slow (chorale), 2 fast
    /// (tremolo). The rotors speed up and slow down at their own pace.
    pub const ORGAN_ROTARY: &str = "organ.rotary";
    /// How much of the organ is heard through the rotating speaker, 0 to 1.
    pub const ORGAN_SPEAKER: &str = "organ.speaker";

    /// The analogue kit's tone, 0 to 1, whose meaning each drum gives:
    /// the click of the kick, the snare's upper head, the hats' band...
    pub const ANALOG_TONE: &str = "analog.tone";
    /// The analogue kit's snap, 0 to 1: the kick's pitch drop and click,
    /// the snare's wires, the toms' noise...
    pub const ANALOG_SNAP: &str = "analog.snap";
    /// A factor on the analogue kick's decay, on top of `kit.decay`: the
    /// long kick of the machine.
    pub const ANALOG_KICK_DECAY: &str = "analog.kick_decay";

    /// Kick bass tuning, semitones.
    pub const KICK_BASS_TUNE: &str = "kick_bass.tune";
    /// How long a held note takes to fall 60 dB, seconds.
    pub const KICK_BASS_DECAY: &str = "kick_bass.decay";
    /// How far above the note the pitch starts, semitones.
    pub const KICK_BASS_SWEEP: &str = "kick_bass.sweep";
    /// How hard the saturator is driven, 0 to 1.
    pub const KICK_BASS_DRIVE: &str = "kick_bass.drive";
    /// How long a slide takes, seconds (95% of the way).
    pub const KICK_BASS_GLIDE: &str = "kick_bass.glide";
    /// The click at the start of a note, 0 to 1.
    pub const KICK_BASS_CLICK: &str = "kick_bass.click";

    /// Acid preset: 0 classic, 1 squelch, 2 square. Setting it moves the
    /// other `acid.*` knobs, but `acid.velocity`, to the preset's
    /// positions.
    pub const ACID_PRESET: &str = "acid.preset";
    /// The oscillator: 0 saw, 1 square.
    pub const ACID_WAVE: &str = "acid.wave";
    /// Where the resonance sits with the envelope closed, hertz.
    pub const ACID_CUTOFF: &str = "acid.cutoff";
    /// The filter's resonance, 0 to 1.
    pub const ACID_RESONANCE: &str = "acid.resonance";
    /// How far the envelope opens the filter, 0 to 1 (4.5 octaves).
    pub const ACID_ENV_MOD: &str = "acid.env_mod";
    /// How long the filter envelope takes to fall 60 dB, seconds.
    pub const ACID_DECAY: &str = "acid.decay";
    /// How strong an accent is, 0 to 1.
    pub const ACID_ACCENT: &str = "acid.accent";
    /// How long a slide takes, seconds (95% of the way).
    pub const ACID_SLIDE: &str = "acid.slide";
    /// The gain into the filter's saturator, decibels.
    pub const ACID_DRIVE: &str = "acid.drive";
    /// How much the level and the filter follow the velocity, 0 (none, as
    /// on the instrument) to 1.
    pub const ACID_VELOCITY: &str = "acid.velocity";

    /// Drum kit preset: 0 acoustic, 1 808, 2 909, 3 606, 4 CR-78, 5 trap,
    /// 6 techno, 7 house, 8 drum and bass, 9 lo-fi. A kit's pieces are its
    /// preset's; the kit's knobs (`kit.tune`, `kit.decay`, the drums'
    /// levels) and `drum_kit.tone` and `drum_kit.snap` act on them.
    pub const DRUM_KIT_PRESET: &str = "drum_kit.preset";
    /// Added to every piece's tone, -1 to 1: the stick or beater, where a
    /// drum is struck, a machine drum's colour.
    pub const DRUM_KIT_TONE: &str = "drum_kit.tone";
    /// Added to every piece's snap, -1 to 1: a snare's wires, a cymbal's
    /// shimmer, a machine's snappy or sweep.
    pub const DRUM_KIT_SNAP: &str = "drum_kit.snap";
    /// How much quieter the softest strike is than the hardest, decibels,
    /// 0 to 60. Setting the preset moves it to the preset's.
    pub const DRUM_KIT_VELOCITY: &str = "drum_kit.velocity";
    /// How much louder an accented strike is, decibels, 0 to 12; it is
    /// also struck harder. Setting the preset moves it to the preset's.
    pub const DRUM_KIT_ACCENT: &str = "drum_kit.accent";

    /// Polyphonic synthesiser preset: an index into
    /// `manguebyte_dsp::instruments::poly::PRESETS`, 0 to 67. Setting it
    /// puts the other `poly.*` knobs but `poly.mod_wheel` back to neutral.
    pub const POLY_PRESET: &str = "poly.preset";
    /// Moves both filters' cutoffs, octaves, -4 to 4.
    pub const POLY_CUTOFF: &str = "poly.cutoff";
    /// Added to both filters' resonance, -1 to 1.
    pub const POLY_RESONANCE: &str = "poly.resonance";
    /// A factor on the amplifier envelope's attack, 0.1 to 10.
    pub const POLY_ATTACK: &str = "poly.attack";
    /// A factor on the amplifier envelope's release, 0.1 to 10.
    pub const POLY_RELEASE: &str = "poly.release";
    /// A factor on every oscillator's unison detune, 0 to 2.
    pub const POLY_DETUNE: &str = "poly.detune";
    /// A factor on every oscillator's unison spread across the stereo
    /// field, 0 to 2.
    pub const POLY_SPREAD: &str = "poly.spread";
    /// The modulation wheel the preset's matrix reads, 0 to 1.
    pub const POLY_MOD_WHEEL: &str = "poly.mod_wheel";
    /// How many notes a polyphonic preset plays at once, 1 to
    /// [`POLY_VOICES`](crate::POLY_VOICES); setting the preset moves it to
    /// as many as the preset can play while a part of it stays within its
    /// share of real time (see the crate's README). A monophonic or legato
    /// preset plays one.
    pub const POLY_POLYPHONY: &str = "poly.voices";

    /// Mallet preset: 0 marimba, 1 xylophone, 2 vibraphone, 3
    /// glockenspiel, 4 celesta, 5 kalimba, 6 steel pan, 7 tubular bells.
    /// Setting it puts the factors back to 1 and the position and the
    /// motor to the preset's.
    pub const MALLET_PRESET: &str = "mallet.preset";
    /// How hard the mallet is, a factor on how short its contact is, 0.25
    /// to 4: harder is brighter.
    pub const MALLET_HARDNESS: &str = "mallet.hardness";
    /// A factor on how long the bars ring, 0.25 to 4.
    pub const MALLET_DECAY: &str = "mallet.decay";
    /// How long a released note takes to fall 60 dB, the damper, seconds,
    /// 0.01 to 60: 60 is no damper, as on a marimba.
    pub const MALLET_RELEASE: &str = "mallet.release";
    /// Where the mallet lands, 0 to 1 of the bar from its end (0.5 the
    /// centre).
    pub const MALLET_POSITION: &str = "mallet.position";
    /// The vibraphone motor's speed, turns a second, 0 (stopped) to 16.
    pub const MALLET_MOTOR: &str = "mallet.motor";

    /// Bowed string preset: 0 violin, 1 viola, 2 cello, 3 double bass, 4
    /// string section, 5 violin pizzicato, 6 cello pizzicato, 7 double
    /// bass pizzicato. Setting it moves every other `bowed.*` knob but
    /// `bowed.legato` to the preset's positions.
    pub const BOWED_PRESET: &str = "bowed.preset";
    /// How hard the bow presses, 0 (airy) to 1 (dense, grainy).
    pub const BOWED_PRESSURE: &str = "bowed.pressure";
    /// Where the bow touches the string, 0.02 (at the bridge) to 0.4 of
    /// its length (over the fingerboard).
    pub const BOWED_POSITION: &str = "bowed.position";
    /// The vibrato's depth, the swing either way, cents, 0 to 100.
    pub const BOWED_VIBRATO: &str = "bowed.vibrato";
    /// How long the bow takes to reach its speed, seconds.
    pub const BOWED_ATTACK: &str = "bowed.attack";
    /// How long a released note takes to fall 60 dB, seconds.
    pub const BOWED_RELEASE: &str = "bowed.release";
    /// The cut-off of the final low-pass, hertz.
    pub const BOWED_TONE: &str = "bowed.tone";
    /// Whether slurred notes slide into each other: 1 portamento, 0 a
    /// change of finger.
    pub const BOWED_PORTAMENTO: &str = "bowed.portamento";
    /// Whether a note struck while another is held slurs to it (1) or
    /// starts a note of its own (0, the default).
    pub const BOWED_LEGATO: &str = "bowed.legato";

    /// Wind preset: 0 flute, 1 clarinet, 2 saxophone, 3 trumpet, 4
    /// trombone. Setting it moves every other `wind.*` knob but
    /// `wind.legato` to the preset's positions.
    pub const WIND_PRESET: &str = "wind.preset";
    /// How much noise the breath carries, 0 to 1.
    pub const WIND_NOISE: &str = "wind.noise";
    /// The vibrato's depth, the swing either way, cents, 0 to 100.
    pub const WIND_VIBRATO: &str = "wind.vibrato";
    /// How long the breath takes to build, seconds.
    pub const WIND_ATTACK: &str = "wind.attack";
    /// How long it takes to stop once the note is released, seconds.
    pub const WIND_RELEASE: &str = "wind.release";
    /// The cut-off of the final low-pass, hertz.
    pub const WIND_TONE: &str = "wind.tone";
    /// Whether slurred notes glide into each other (1) or change at once,
    /// as keys and valves do (0).
    pub const WIND_PORTAMENTO: &str = "wind.portamento";
    /// Whether a note struck while another is held slurs to it (1, the
    /// default: one player, one note at a time) or starts a note of its own
    /// (0: a section, chords).
    pub const WIND_LEGATO: &str = "wind.legato";

    /// Choir preset: 0 "aah" choir, 1 "ooh" choir, 2 solo voice. Setting it
    /// moves every other `choir.*` knob but `choir.legato` to the preset's
    /// positions.
    pub const CHOIR_PRESET: &str = "choir.preset";
    /// The vowel, 0 to 4: a, e, i, o, u, and blends between.
    pub const CHOIR_VOWEL: &str = "choir.vowel";
    /// The vibrato's depth, the swing either way, cents, 0 to 200.
    pub const CHOIR_VIBRATO: &str = "choir.vibrato";
    /// How long the voices take to sound, seconds.
    pub const CHOIR_ATTACK: &str = "choir.attack";
    /// How long they take to fade once released, seconds.
    pub const CHOIR_RELEASE: &str = "choir.release";
    /// How much breath the voices carry, 0 to 1.
    pub const CHOIR_BREATHINESS: &str = "choir.breathiness";
    /// The voices' brightness at full velocity, hertz.
    pub const CHOIR_BRIGHTNESS: &str = "choir.brightness";
    /// Whether a note struck while another is held slurs to it (1) or
    /// starts a note of its own (0, the default).
    pub const CHOIR_LEGATO: &str = "choir.legato";

    /// Every identifier a part can have: the chain's, the synth's, the
    /// kit's, the sampler's, then the switches of the low-pass and the
    /// reverb, the send, the other effects' in the order of their codes
    /// ([`Effect`](crate::Effect)), then the instruments' added after them:
    /// the plucked string's, the FM's, the organ's, the analogue kit's, the
    /// kick bass's and the acid bass's, then the sampler's normalisation,
    /// then the drum kit's, the polyphonic synthesiser's, the mallets', the
    /// bowed strings', the winds' and the choir's.
    pub const ALL: [&str; 191] = [
        LOWPASS_CUTOFF,
        LOWPASS_RESONANCE,
        REVERB_MIX,
        REVERB_DECAY,
        GAIN,
        PAN,
        SYNTH_WAVE,
        SYNTH_DETUNE,
        SYNTH_SUB,
        SYNTH_CUTOFF,
        SYNTH_RESONANCE,
        SYNTH_ENV_AMOUNT,
        SYNTH_FILTER_DECAY,
        SYNTH_ATTACK,
        SYNTH_DECAY,
        SYNTH_SUSTAIN,
        SYNTH_RELEASE,
        KIT_TUNE,
        KIT_DECAY,
        KICK_LEVEL,
        SNARE_LEVEL,
        CLOSED_HAT_LEVEL,
        OPEN_HAT_LEVEL,
        RIDE_LEVEL,
        CRASH_LEVEL,
        HIGH_TOM_LEVEL,
        MID_TOM_LEVEL,
        FLOOR_TOM_LEVEL,
        SIDE_STICK_LEVEL,
        GUITAR_TAP_LEVEL,
        SAMPLER_INTERPOLATION,
        LOWPASS_ON,
        REVERB_ON,
        SEND,
        HIGHPASS_ON,
        HIGHPASS_CUTOFF,
        HIGHPASS_RESONANCE,
        EQ_ON,
        EQ_LOW_GAIN,
        EQ_MID_GAIN,
        EQ_HIGH_GAIN,
        EQ_LOW_FREQ,
        EQ_MID_FREQ,
        EQ_MID_Q,
        EQ_HIGH_FREQ,
        DRIVE_ON,
        DRIVE_AMOUNT,
        DRIVE_OUTPUT,
        DRIVE_MIX,
        DRIVE_SHAPE,
        BITCRUSH_ON,
        BITCRUSH_BITS,
        BITCRUSH_RATE,
        BITCRUSH_MIX,
        COMPRESSOR_ON,
        COMPRESSOR_THRESHOLD,
        COMPRESSOR_RATIO,
        COMPRESSOR_ATTACK,
        COMPRESSOR_RELEASE,
        COMPRESSOR_KNEE,
        COMPRESSOR_MAKEUP,
        COMPRESSOR_MIX,
        CHORUS_ON,
        CHORUS_MIX,
        CHORUS_RATE,
        CHORUS_DEPTH,
        CHORUS_DELAY,
        CHORUS_VOICES,
        CHORUS_SPREAD,
        FLANGER_ON,
        FLANGER_MIX,
        FLANGER_RATE,
        FLANGER_DEPTH,
        FLANGER_FEEDBACK,
        FLANGER_DELAY,
        PHASER_ON,
        PHASER_MIX,
        PHASER_RATE,
        PHASER_DEPTH,
        PHASER_CENTER,
        PHASER_FEEDBACK,
        PHASER_STAGES,
        PHASER_SPREAD,
        TREMOLO_ON,
        TREMOLO_RATE,
        TREMOLO_DEPTH,
        TREMOLO_SHAPE,
        TREMOLO_SYNC,
        AUTOPAN_ON,
        AUTOPAN_RATE,
        AUTOPAN_DEPTH,
        AUTOPAN_SHAPE,
        AUTOPAN_SYNC,
        DELAY_ON,
        DELAY_TIME,
        DELAY_FEEDBACK,
        DELAY_MIX,
        DELAY_DAMPING,
        DELAY_PINGPONG,
        DELAY_SYNC,
        PLUCKED_PRESET,
        PLUCKED_DECAY,
        PLUCKED_BRIGHTNESS,
        PLUCKED_POSITION,
        PLUCKED_TONE,
        PLUCKED_RELEASE,
        FM_PRESET,
        FM_BRIGHTNESS,
        FM_DECAY,
        FM_RELEASE,
        ORGAN_PRESET,
        ORGAN_DRAWBAR_16,
        ORGAN_DRAWBAR_5_1_3,
        ORGAN_DRAWBAR_8,
        ORGAN_DRAWBAR_4,
        ORGAN_DRAWBAR_2_2_3,
        ORGAN_DRAWBAR_2,
        ORGAN_DRAWBAR_1_3_5,
        ORGAN_DRAWBAR_1_1_3,
        ORGAN_DRAWBAR_1,
        ORGAN_PERCUSSION,
        ORGAN_CLICK,
        ORGAN_LEAKAGE,
        ORGAN_VELOCITY,
        ORGAN_ROTARY,
        ORGAN_SPEAKER,
        ANALOG_TONE,
        ANALOG_SNAP,
        ANALOG_KICK_DECAY,
        KICK_BASS_TUNE,
        KICK_BASS_DECAY,
        KICK_BASS_SWEEP,
        KICK_BASS_DRIVE,
        KICK_BASS_GLIDE,
        KICK_BASS_CLICK,
        ACID_PRESET,
        ACID_WAVE,
        ACID_CUTOFF,
        ACID_RESONANCE,
        ACID_ENV_MOD,
        ACID_DECAY,
        ACID_ACCENT,
        ACID_SLIDE,
        ACID_DRIVE,
        ACID_VELOCITY,
        SAMPLER_NORMALIZE,
        DRUM_KIT_PRESET,
        DRUM_KIT_TONE,
        DRUM_KIT_SNAP,
        DRUM_KIT_VELOCITY,
        DRUM_KIT_ACCENT,
        POLY_PRESET,
        POLY_CUTOFF,
        POLY_RESONANCE,
        POLY_ATTACK,
        POLY_RELEASE,
        POLY_DETUNE,
        POLY_SPREAD,
        POLY_MOD_WHEEL,
        POLY_POLYPHONY,
        MALLET_PRESET,
        MALLET_HARDNESS,
        MALLET_DECAY,
        MALLET_RELEASE,
        MALLET_POSITION,
        MALLET_MOTOR,
        BOWED_PRESET,
        BOWED_PRESSURE,
        BOWED_POSITION,
        BOWED_VIBRATO,
        BOWED_ATTACK,
        BOWED_RELEASE,
        BOWED_TONE,
        BOWED_PORTAMENTO,
        BOWED_LEGATO,
        WIND_PRESET,
        WIND_NOISE,
        WIND_VIBRATO,
        WIND_ATTACK,
        WIND_RELEASE,
        WIND_TONE,
        WIND_PORTAMENTO,
        WIND_LEGATO,
        CHOIR_PRESET,
        CHOIR_VOWEL,
        CHOIR_VIBRATO,
        CHOIR_ATTACK,
        CHOIR_RELEASE,
        CHOIR_BREATHINESS,
        CHOIR_BRIGHTNESS,
        CHOIR_LEGATO,
    ];

    /// The drawbars' identifiers, from the lowest footage to the highest
    /// (`manguebyte_dsp::instruments::DRAWBAR_FOOTAGES`).
    pub const ORGAN_DRAWBARS: [&str; 9] = [
        ORGAN_DRAWBAR_16,
        ORGAN_DRAWBAR_5_1_3,
        ORGAN_DRAWBAR_8,
        ORGAN_DRAWBAR_4,
        ORGAN_DRAWBAR_2_2_3,
        ORGAN_DRAWBAR_2,
        ORGAN_DRAWBAR_1_3_5,
        ORGAN_DRAWBAR_1_1_3,
        ORGAN_DRAWBAR_1,
    ];

    /// Every identifier of the master bus, in the order
    /// [`Engine::master_params_json`](crate::Engine::master_params_json)
    /// lists them.
    pub const MASTER: [&str; 16] = [
        MASTER_TEMPO,
        MASTER_BEAT,
        MASTER_GAIN,
        MASTER_REVERB_LEVEL,
        MASTER_REVERB_DECAY,
        MASTER_REVERB_SIZE,
        MASTER_REVERB_DAMPING,
        GLUE_ON,
        GLUE_THRESHOLD,
        GLUE_RATIO,
        GLUE_ATTACK,
        GLUE_RELEASE,
        GLUE_KNEE,
        GLUE_MAKEUP,
        LIMITER_CEILING,
        LIMITER_RELEASE,
    ];
}

/// The compact id of every identifier in [`ids`], under the same name.
macro_rules! param_ids {
    ($($name:ident),* $(,)?) => {
        $(pub(crate) const $name: ParamId = ParamId::from_name(ids::$name);)*
    };
}

param_ids!(
    LOWPASS_ON,
    LOWPASS_CUTOFF,
    LOWPASS_RESONANCE,
    REVERB_ON,
    REVERB_MIX,
    REVERB_DECAY,
    GAIN,
    PAN,
    SEND,
    HIGHPASS_ON,
    HIGHPASS_CUTOFF,
    HIGHPASS_RESONANCE,
    EQ_ON,
    EQ_LOW_GAIN,
    EQ_MID_GAIN,
    EQ_HIGH_GAIN,
    EQ_LOW_FREQ,
    EQ_MID_FREQ,
    EQ_MID_Q,
    EQ_HIGH_FREQ,
    DRIVE_ON,
    DRIVE_AMOUNT,
    DRIVE_OUTPUT,
    DRIVE_MIX,
    DRIVE_SHAPE,
    BITCRUSH_ON,
    BITCRUSH_BITS,
    BITCRUSH_RATE,
    BITCRUSH_MIX,
    COMPRESSOR_ON,
    COMPRESSOR_THRESHOLD,
    COMPRESSOR_RATIO,
    COMPRESSOR_ATTACK,
    COMPRESSOR_RELEASE,
    COMPRESSOR_KNEE,
    COMPRESSOR_MAKEUP,
    COMPRESSOR_MIX,
    CHORUS_ON,
    CHORUS_MIX,
    CHORUS_RATE,
    CHORUS_DEPTH,
    CHORUS_DELAY,
    CHORUS_VOICES,
    CHORUS_SPREAD,
    FLANGER_ON,
    FLANGER_MIX,
    FLANGER_RATE,
    FLANGER_DEPTH,
    FLANGER_FEEDBACK,
    FLANGER_DELAY,
    PHASER_ON,
    PHASER_MIX,
    PHASER_RATE,
    PHASER_DEPTH,
    PHASER_CENTER,
    PHASER_FEEDBACK,
    PHASER_STAGES,
    PHASER_SPREAD,
    TREMOLO_ON,
    TREMOLO_RATE,
    TREMOLO_DEPTH,
    TREMOLO_SHAPE,
    TREMOLO_SYNC,
    AUTOPAN_ON,
    AUTOPAN_RATE,
    AUTOPAN_DEPTH,
    AUTOPAN_SHAPE,
    AUTOPAN_SYNC,
    DELAY_ON,
    DELAY_TIME,
    DELAY_FEEDBACK,
    DELAY_MIX,
    DELAY_DAMPING,
    DELAY_PINGPONG,
    DELAY_SYNC,
    MASTER_TEMPO,
    MASTER_BEAT,
    MASTER_GAIN,
    GLUE_ON,
    GLUE_THRESHOLD,
    GLUE_RATIO,
    GLUE_ATTACK,
    GLUE_RELEASE,
    GLUE_KNEE,
    GLUE_MAKEUP,
    MASTER_REVERB_LEVEL,
    MASTER_REVERB_DECAY,
    MASTER_REVERB_SIZE,
    MASTER_REVERB_DAMPING,
    LIMITER_CEILING,
    LIMITER_RELEASE,
    SYNTH_WAVE,
    SYNTH_DETUNE,
    SYNTH_SUB,
    SYNTH_CUTOFF,
    SYNTH_RESONANCE,
    SYNTH_ENV_AMOUNT,
    SYNTH_FILTER_DECAY,
    SYNTH_ATTACK,
    SYNTH_DECAY,
    SYNTH_SUSTAIN,
    SYNTH_RELEASE,
    KIT_TUNE,
    KIT_DECAY,
    SAMPLER_INTERPOLATION,
    SAMPLER_NORMALIZE,
    PLUCKED_PRESET,
    FM_PRESET,
    ORGAN_PRESET,
    ORGAN_ROTARY,
    ORGAN_SPEAKER,
    ACID_PRESET,
    DRUM_KIT_PRESET,
    POLY_PRESET,
    POLY_MOD_WHEEL,
    POLY_POLYPHONY,
    MALLET_PRESET,
    BOWED_PRESET,
    BOWED_LEGATO,
    WIND_PRESET,
    WIND_LEGATO,
    CHOIR_PRESET,
    CHOIR_LEGATO,
);

/// The level parameters of the drums, in [`Drum`](crate::Drum) order.
pub(crate) const DRUM_LEVELS: [ParamId; 11] = [
    ParamId::from_name(ids::KICK_LEVEL),
    ParamId::from_name(ids::SNARE_LEVEL),
    ParamId::from_name(ids::CLOSED_HAT_LEVEL),
    ParamId::from_name(ids::OPEN_HAT_LEVEL),
    ParamId::from_name(ids::RIDE_LEVEL),
    ParamId::from_name(ids::CRASH_LEVEL),
    ParamId::from_name(ids::HIGH_TOM_LEVEL),
    ParamId::from_name(ids::MID_TOM_LEVEL),
    ParamId::from_name(ids::FLOOR_TOM_LEVEL),
    ParamId::from_name(ids::SIDE_STICK_LEVEL),
    ParamId::from_name(ids::GUITAR_TAP_LEVEL),
];

/// Names of the sampler's interpolations, in the order of their values.
pub(crate) const INTERPOLATION_NAMES: [&str; 2] = ["Cubic", "Sinc"];

/// Names of the synth waveforms, in the order of their values.
pub(crate) const WAVE_NAMES: [&str; 4] = ["Saw", "Square", "Triangle", "Sine"];

/// Names of the drive's curves, in the order of their values.
pub(crate) const DRIVE_SHAPE_NAMES: [&str; 5] = ["Tanh", "Soft clip", "Hard clip", "Fold", "Tube"];

/// Names of the waves of the tremolo and the auto-pan, in the order of
/// their values.
pub(crate) const LFO_SHAPE_NAMES: [&str; 5] = ["Sine", "Triangle", "Square", "Saw up", "Saw down"];

/// The smoothing every chain parameter that moves audio uses: the hint in
/// its description and what the chain does.
pub(crate) const FILTER_SMOOTHING: Smoothing = Smoothing::Exponential(20.0);
pub(crate) const LEVEL_SMOOTHING: Smoothing = Smoothing::Linear(20.0);
/// What the chorus, the flanger and the phaser do with their delays,
/// depths and centres, and with a change of their voice or stage count.
const SWEEP_SMOOTHING: Smoothing = Smoothing::Linear(50.0);
/// How an effect fades in and out as it is switched:
/// [`EFFECT_FADE_SECONDS`](crate::EFFECT_FADE_SECONDS).
const SWITCH_SMOOTHING: Smoothing = Smoothing::Linear(10.0);

/// The lowest gain, which the chain treats as silence.
pub(crate) const GAIN_FLOOR_DB: f32 = -60.0;

fn seconds(id: &str, name: &str, min: f32, max: f32, default: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::skewed(min, max, 0.35), default)
        .with_unit("s")
        .with_precision(3)
}

fn unit(id: &str, name: &str, default: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::linear(0.0, 1.0), default)
}

/// A balance or an amount from 0 to 1 that moves audio.
fn amount(id: &str, name: &str, default: f32) -> FloatParam {
    unit(id, name, default)
        .with_precision(2)
        .with_smoothing(LEVEL_SMOOTHING)
}

fn hertz(id: &str, name: &str, min: f32, max: f32, default: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::logarithmic(min, max), default)
        .with_unit("Hz")
        .with_precision(0)
        .with_smoothing(FILTER_SMOOTHING)
}

/// A modulation rate: it changes at once, keeping the place in the cycle.
fn rate(id: &str, name: &str, min: f32, max: f32, default: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::logarithmic(min, max), default)
        .with_unit("Hz")
        .with_precision(2)
}

fn decibels(id: &str, name: &str, min: f32, max: f32, default: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::linear(min, max), default)
        .with_unit("dB")
        .with_precision(1)
        .with_smoothing(LEVEL_SMOOTHING)
}

fn milliseconds(id: &str, name: &str, min: f32, max: f32, default: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::logarithmic(min, max), default)
        .with_unit("ms")
        .with_precision(1)
}

/// A length in beats at `master.tempo`, 0 to 16; 0 is no sync.
fn beats(id: &str, name: &str) -> FloatParam {
    FloatParam::new(id, name, FloatRange::linear(0.0, 16.0), 0.0)
        .with_unit("beats")
        .with_precision(3)
}

/// An effect's switch.
fn switch(id: &str, name: &str, default: bool) -> BoolParam {
    BoolParam::new(id, name, default).with_smoothing(SWITCH_SMOOTHING)
}

pub(crate) struct SynthParams {
    pub(crate) wave: ChoiceParam,
    pub(crate) detune: FloatParam,
    pub(crate) sub: FloatParam,
    pub(crate) cutoff: FloatParam,
    pub(crate) resonance: FloatParam,
    pub(crate) env_amount: FloatParam,
    pub(crate) filter_decay: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) sustain: FloatParam,
    pub(crate) release: FloatParam,
}

impl SynthParams {
    pub(crate) fn new() -> Self {
        Self {
            wave: ChoiceParam::new(ids::SYNTH_WAVE, "Wave", &WAVE_NAMES, 0),
            detune: FloatParam::new(
                ids::SYNTH_DETUNE,
                "Detune",
                FloatRange::linear(0.0, 50.0),
                9.0,
            )
            .with_unit("ct")
            .with_precision(1),
            sub: unit(ids::SYNTH_SUB, "Sub", 0.35),
            cutoff: FloatParam::new(
                ids::SYNTH_CUTOFF,
                "Cutoff",
                FloatRange::logarithmic(40.0, 16_000.0),
                900.0,
            )
            .with_unit("Hz")
            .with_precision(0)
            .with_smoothing(Smoothing::Exponential(10.0)),
            resonance: unit(ids::SYNTH_RESONANCE, "Resonance", 0.25),
            env_amount: FloatParam::new(
                ids::SYNTH_ENV_AMOUNT,
                "Env amount",
                FloatRange::linear(0.0, 6.0),
                2.5,
            )
            .with_unit("oct")
            .with_precision(2),
            filter_decay: seconds(ids::SYNTH_FILTER_DECAY, "Filter decay", 0.01, 4.0, 0.35),
            attack: seconds(ids::SYNTH_ATTACK, "Attack", 0.001, 4.0, 0.003),
            decay: seconds(ids::SYNTH_DECAY, "Decay", 0.01, 8.0, 0.6),
            sustain: unit(ids::SYNTH_SUSTAIN, "Sustain", 0.5),
            release: seconds(ids::SYNTH_RELEASE, "Release", 0.005, 8.0, 0.25),
        }
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.wave);
        visitor(&self.detune);
        visitor(&self.sub);
        visitor(&self.cutoff);
        visitor(&self.resonance);
        visitor(&self.env_amount);
        visitor(&self.filter_decay);
        visitor(&self.attack);
        visitor(&self.decay);
        visitor(&self.sustain);
        visitor(&self.release);
    }
}

pub(crate) struct KitParams {
    pub(crate) tune: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) levels: [FloatParam; 11],
}

impl KitParams {
    pub(crate) fn new() -> Self {
        Self {
            tune: FloatParam::new(ids::KIT_TUNE, "Tune", FloatRange::linear(-12.0, 12.0), 0.0)
                .with_unit("st")
                .with_precision(1),
            decay: FloatParam::new(
                ids::KIT_DECAY,
                "Decay",
                FloatRange::logarithmic(0.25, 4.0),
                1.0,
            )
            .with_unit("x")
            .with_precision(2),
            levels: [
                unit(ids::KICK_LEVEL, "Kick", 0.8),
                unit(ids::SNARE_LEVEL, "Snare", 0.8),
                unit(ids::CLOSED_HAT_LEVEL, "Closed hat", 0.8),
                unit(ids::OPEN_HAT_LEVEL, "Open hat", 0.8),
                unit(ids::RIDE_LEVEL, "Ride", 0.8),
                unit(ids::CRASH_LEVEL, "Crash", 0.8),
                unit(ids::HIGH_TOM_LEVEL, "High tom", 0.8),
                unit(ids::MID_TOM_LEVEL, "Mid tom", 0.8),
                unit(ids::FLOOR_TOM_LEVEL, "Floor tom", 0.8),
                unit(ids::SIDE_STICK_LEVEL, "Side stick", 0.8),
                unit(ids::GUITAR_TAP_LEVEL, "Guitar tap", 0.8),
            ],
        }
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.tune);
        visitor(&self.decay);
        for level in &self.levels {
            visitor(level);
        }
    }
}

/// Names of the plucked string's presets, in the order of their values.
pub(crate) const PLUCKED_PRESET_NAMES: [&str; 4] =
    ["Nylon guitar", "Steel guitar", "Finger bass", "Slap bass"];
/// Names of the FM presets, in the order of their values.
pub(crate) const FM_PRESET_NAMES: [&str; 3] = ["Electric piano", "Tubular bell", "FM bass"];
/// Names of the organ presets, in the order of their values.
pub(crate) const ORGAN_PRESET_NAMES: [&str; 4] = ["Jazz", "Gospel", "Full", "Ballad"];
/// Names of the organ's percussion settings, in the order of their values.
pub(crate) const PERCUSSION_NAMES: [&str; 5] = [
    "Off",
    "Second, fast",
    "Second, slow",
    "Third, fast",
    "Third, slow",
];
/// Names of the rotating speaker's speeds, in the order of their values.
pub(crate) const ROTARY_NAMES: [&str; 3] = ["Stop", "Slow", "Fast"];
/// Names of the acid presets, in the order of their values.
pub(crate) const ACID_PRESET_NAMES: [&str; 3] = ["Classic", "Squelch", "Square"];
/// Names of the acid oscillator's waves, in the order of their values.
pub(crate) const ACID_WAVE_NAMES: [&str; 2] = ["Saw", "Square"];
/// Names of the drum kit presets, in the order of their values.
pub(crate) const DRUM_KIT_PRESET_NAMES: [&str; 10] = [
    "Acoustic",
    "808",
    "909",
    "606",
    "CR-78",
    "Trap",
    "Techno",
    "House",
    "Drum and bass",
    "Lo-fi",
];
/// Names of the polyphonic synthesiser's presets, in the order of their
/// values: those of `manguebyte_dsp::instruments::poly::PRESETS`, which a
/// unit test holds them to.
pub(crate) const POLY_PRESET_NAMES: [&str; 68] = [
    "Moog Bass",
    "Reese",
    "Wobble Bass",
    "Deep Sub",
    "FM Growl",
    "House Organ Bass",
    "Acid Line",
    "Garage Square",
    "Wavetable Growl",
    "Synthpop Bass",
    "Funk Bass",
    "808 Glide",
    "Rolling Bass",
    "Rubber Bass",
    "Talking Bass",
    "Trance Supersaw",
    "Square Lead",
    "Sync Lead",
    "Chiptune",
    "Hoover",
    "Mono Saw Lead",
    "Whistle Lead",
    "Digital Lead",
    "Talking Lead",
    "Screamer",
    "Future Lead",
    "Warm Analog Pad",
    "String Machine",
    "Glass Pad",
    "Dark Drone",
    "Choir Pad",
    "Formant Pad",
    "PWM Pad",
    "Shimmer Pad",
    "Evolving Spectra",
    "Soft Sine Pad",
    "Comb Wash",
    "Sweep Pad",
    "House Pluck",
    "Bell Pluck",
    "Marimba",
    "Plucked String",
    "Future Pluck",
    "Glass Pluck",
    "Trance Pluck",
    "Pizzicato",
    "Kalimba",
    "Polysynth Piano",
    "Clavinet",
    "FM Electric Piano",
    "Drawbar Organ",
    "Juno Keys",
    "Wurly",
    "Vibraphone",
    "Synth Brass",
    "Brass Stab",
    "Jump Brass",
    "Rave Chord Stab",
    "Soft Horns",
    "Fanfare",
    "Riser",
    "Downlifter",
    "Noise Sweep",
    "Laser Zap",
    "Random Bleeps",
    "Wind",
    "Impact Boom",
    "Siren",
];
/// Names of the mallet presets, in the order of their values.
pub(crate) const MALLET_PRESET_NAMES: [&str; 8] = [
    "Marimba",
    "Xylophone",
    "Vibraphone",
    "Glockenspiel",
    "Celesta",
    "Kalimba",
    "Steel pan",
    "Tubular bells",
];
/// Names of the bowed string presets, in the order of their values.
pub(crate) const BOWED_PRESET_NAMES: [&str; 8] = [
    "Violin",
    "Viola",
    "Cello",
    "Double bass",
    "String section",
    "Violin pizzicato",
    "Cello pizzicato",
    "Double bass pizzicato",
];
/// Names of the wind presets, in the order of their values.
pub(crate) const WIND_PRESET_NAMES: [&str; 5] =
    ["Flute", "Clarinet", "Saxophone", "Trumpet", "Trombone"];
/// Names of the choir presets, in the order of their values.
pub(crate) const CHOIR_PRESET_NAMES: [&str; 3] = ["Aah choir", "Ooh choir", "Solo voice"];

/// A factor from a quarter to four times, 1 in the middle of the knob.
fn factor(id: &str, name: &str) -> FloatParam {
    FloatParam::new(id, name, FloatRange::logarithmic(0.25, 4.0), 1.0)
        .with_unit("x")
        .with_precision(2)
}

/// The plucked string's knobs: the preset, and what it sets.
pub(crate) struct PluckedKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) decay: FloatParam,
    pub(crate) brightness: FloatParam,
    pub(crate) position: FloatParam,
    pub(crate) tone: FloatParam,
    pub(crate) release: FloatParam,
}

impl PluckedKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::voiced::string_preset(0);
        Self {
            preset: ChoiceParam::new(ids::PLUCKED_PRESET, "Preset", &PLUCKED_PRESET_NAMES, 0),
            decay: seconds(ids::PLUCKED_DECAY, "Decay", 0.1, 20.0, preset.decay as f32),
            brightness: FloatParam::new(
                ids::PLUCKED_BRIGHTNESS,
                "Brightness",
                FloatRange::logarithmic(100.0, 20_000.0),
                preset.pluck_brightness as f32,
            )
            .with_unit("Hz")
            .with_precision(0),
            position: FloatParam::new(
                ids::PLUCKED_POSITION,
                "Pluck position",
                FloatRange::linear(0.02, 0.5),
                preset.pluck_position as f32,
            )
            .with_precision(3),
            tone: FloatParam::new(
                ids::PLUCKED_TONE,
                "Tone",
                FloatRange::logarithmic(200.0, 20_000.0),
                preset.tone as f32,
            )
            .with_unit("Hz")
            .with_precision(0),
            release: seconds(
                ids::PLUCKED_RELEASE,
                "Release",
                0.01,
                2.0,
                preset.release as f32,
            ),
        }
    }

    /// Moves every knob to the position of the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::voiced::string_preset(self.preset.index());
        self.decay.set_plain(preset.decay);
        self.brightness.set_plain(preset.pluck_brightness);
        self.position.set_plain(preset.pluck_position);
        self.tone.set_plain(preset.tone);
        self.release.set_plain(preset.release);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.decay);
        visitor(&self.brightness);
        visitor(&self.position);
        visitor(&self.tone);
        visitor(&self.release);
    }
}

/// The FM voice's knobs: the preset, and factors on what it sets.
pub(crate) struct FmKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) brightness: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) release: FloatParam,
}

impl FmKnobs {
    pub(crate) fn new() -> Self {
        Self {
            preset: ChoiceParam::new(ids::FM_PRESET, "Preset", &FM_PRESET_NAMES, 0),
            brightness: FloatParam::new(
                ids::FM_BRIGHTNESS,
                "Brightness",
                FloatRange::linear(0.0, 2.0),
                1.0,
            )
            .with_unit("x")
            .with_precision(2),
            decay: factor(ids::FM_DECAY, "Decay"),
            release: factor(ids::FM_RELEASE, "Release"),
        }
    }

    /// Puts the factors back to 1, the preset as it is.
    pub(crate) fn load_preset(&self) {
        self.brightness.set_plain(1.0);
        self.decay.set_plain(1.0);
        self.release.set_plain(1.0);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.brightness);
        visitor(&self.decay);
        visitor(&self.release);
    }
}

/// The organ's knobs: the preset and the registration it sets, then the
/// velocity and the rotating speaker, which presets leave alone.
pub(crate) struct OrganKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) drawbars: [FloatParam; 9],
    pub(crate) percussion: ChoiceParam,
    pub(crate) click: FloatParam,
    pub(crate) leakage: FloatParam,
    pub(crate) velocity: FloatParam,
    pub(crate) rotary: ChoiceParam,
    pub(crate) speaker: FloatParam,
}

/// How much an organ or an acid part follows the velocity unless told
/// otherwise: halfway, so that Aratu's dynamics are heard on instruments
/// that have none of their own.
pub(crate) const DEFAULT_VELOCITY_SENSITIVITY: f32 = 0.5;

impl OrganKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::voiced::organ_preset(0);
        let names = [
            "16'", "5 1/3'", "8'", "4'", "2 2/3'", "2'", "1 3/5'", "1 1/3'", "1'",
        ];
        let mut index = 0;
        let drawbars = ids::ORGAN_DRAWBARS.map(|id| {
            let drawbar = FloatParam::new(
                id,
                names[index],
                FloatRange::linear(0.0, 8.0),
                preset.drawbars[index] as f32,
            )
            .with_precision(1);
            index += 1;
            drawbar
        });
        Self {
            preset: ChoiceParam::new(ids::ORGAN_PRESET, "Preset", &ORGAN_PRESET_NAMES, 0),
            drawbars,
            percussion: ChoiceParam::new(
                ids::ORGAN_PERCUSSION,
                "Percussion",
                &PERCUSSION_NAMES,
                crate::voiced::percussion_index(preset.percussion),
            ),
            click: unit(ids::ORGAN_CLICK, "Click", preset.click as f32).with_precision(2),
            leakage: unit(ids::ORGAN_LEAKAGE, "Leakage", preset.leakage as f32).with_precision(2),
            velocity: unit(
                ids::ORGAN_VELOCITY,
                "Velocity",
                DEFAULT_VELOCITY_SENSITIVITY,
            )
            .with_precision(2),
            rotary: ChoiceParam::new(ids::ORGAN_ROTARY, "Rotary speed", &ROTARY_NAMES, 1),
            speaker: amount(ids::ORGAN_SPEAKER, "Rotary speaker", 1.0),
        }
    }

    /// Moves the registration to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::voiced::organ_preset(self.preset.index());
        for (drawbar, &position) in self.drawbars.iter().zip(&preset.drawbars) {
            drawbar.set_plain(position);
        }
        self.percussion
            .set_plain(crate::voiced::percussion_index(preset.percussion) as f64);
        self.click.set_plain(preset.click);
        self.leakage.set_plain(preset.leakage);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        for drawbar in &self.drawbars {
            visitor(drawbar);
        }
        visitor(&self.percussion);
        visitor(&self.click);
        visitor(&self.leakage);
        visitor(&self.velocity);
        visitor(&self.rotary);
        visitor(&self.speaker);
    }
}

/// The analogue kit's knobs: the kit's own (tune, decay, a level per drum
/// of the drum map), then those of the machine.
pub(crate) struct AnalogKitKnobs {
    pub(crate) kit: KitParams,
    pub(crate) tone: FloatParam,
    pub(crate) snap: FloatParam,
    pub(crate) kick_decay: FloatParam,
}

impl AnalogKitKnobs {
    pub(crate) fn new() -> Self {
        Self {
            kit: KitParams::new(),
            tone: unit(ids::ANALOG_TONE, "Tone", 0.5).with_precision(2),
            snap: unit(ids::ANALOG_SNAP, "Snap", 0.5).with_precision(2),
            kick_decay: FloatParam::new(
                ids::ANALOG_KICK_DECAY,
                "Kick decay",
                FloatRange::logarithmic(0.25, 8.0),
                1.0,
            )
            .with_unit("x")
            .with_precision(2),
        }
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        self.kit.visit(visitor);
        visitor(&self.tone);
        visitor(&self.snap);
        visitor(&self.kick_decay);
    }
}

/// The kick bass's knobs, at the machine's classic setting by default.
pub(crate) struct KickBassKnobs {
    pub(crate) tune: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) sweep: FloatParam,
    pub(crate) drive: FloatParam,
    pub(crate) glide: FloatParam,
    pub(crate) click: FloatParam,
}

impl KickBassKnobs {
    pub(crate) fn new() -> Self {
        let classic = crate::voiced::kick_bass_classic();
        Self {
            tune: FloatParam::new(
                ids::KICK_BASS_TUNE,
                "Tune",
                FloatRange::linear(-12.0, 12.0),
                classic.tune as f32,
            )
            .with_unit("st")
            .with_precision(1),
            decay: seconds(
                ids::KICK_BASS_DECAY,
                "Decay",
                0.05,
                20.0,
                classic.decay as f32,
            ),
            sweep: FloatParam::new(
                ids::KICK_BASS_SWEEP,
                "Sweep",
                FloatRange::linear(0.0, 48.0),
                classic.sweep as f32,
            )
            .with_unit("st")
            .with_precision(1),
            drive: unit(ids::KICK_BASS_DRIVE, "Drive", classic.drive as f32).with_precision(2),
            glide: seconds(
                ids::KICK_BASS_GLIDE,
                "Glide",
                0.0,
                2.0,
                classic.glide as f32,
            ),
            click: unit(ids::KICK_BASS_CLICK, "Click", classic.click as f32).with_precision(2),
        }
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.tune);
        visitor(&self.decay);
        visitor(&self.sweep);
        visitor(&self.drive);
        visitor(&self.glide);
        visitor(&self.click);
    }
}

/// The acid bass's knobs: the preset and the knobs of the machine it sets,
/// then the velocity, which presets leave alone.
pub(crate) struct AcidKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) wave: ChoiceParam,
    pub(crate) cutoff: FloatParam,
    pub(crate) resonance: FloatParam,
    pub(crate) env_mod: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) accent: FloatParam,
    pub(crate) slide: FloatParam,
    pub(crate) drive: FloatParam,
    pub(crate) velocity: FloatParam,
}

impl AcidKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::voiced::acid_preset(0);
        Self {
            preset: ChoiceParam::new(ids::ACID_PRESET, "Preset", &ACID_PRESET_NAMES, 0),
            wave: ChoiceParam::new(
                ids::ACID_WAVE,
                "Wave",
                &ACID_WAVE_NAMES,
                crate::voiced::acid_wave_index(preset.wave),
            ),
            cutoff: FloatParam::new(
                ids::ACID_CUTOFF,
                "Cutoff",
                FloatRange::logarithmic(30.0, 8_000.0),
                preset.cutoff as f32,
            )
            .with_unit("Hz")
            .with_precision(0),
            resonance: unit(ids::ACID_RESONANCE, "Resonance", preset.resonance as f32)
                .with_precision(2),
            env_mod: unit(ids::ACID_ENV_MOD, "Env mod", preset.env_mod as f32).with_precision(2),
            decay: seconds(ids::ACID_DECAY, "Decay", 0.05, 4.0, preset.decay as f32),
            accent: unit(ids::ACID_ACCENT, "Accent", preset.accent as f32).with_precision(2),
            slide: seconds(ids::ACID_SLIDE, "Slide", 0.005, 1.0, preset.slide as f32),
            drive: FloatParam::new(
                ids::ACID_DRIVE,
                "Drive",
                FloatRange::linear(-12.0, 24.0),
                preset.drive_db as f32,
            )
            .with_unit("dB")
            .with_precision(1),
            velocity: unit(ids::ACID_VELOCITY, "Velocity", DEFAULT_VELOCITY_SENSITIVITY)
                .with_precision(2),
        }
    }

    /// Moves every knob but the velocity to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::voiced::acid_preset(self.preset.index());
        self.wave
            .set_plain(crate::voiced::acid_wave_index(preset.wave) as f64);
        self.cutoff.set_plain(preset.cutoff);
        self.resonance.set_plain(preset.resonance);
        self.env_mod.set_plain(preset.env_mod);
        self.decay.set_plain(preset.decay);
        self.accent.set_plain(preset.accent);
        self.slide.set_plain(preset.slide);
        self.drive.set_plain(preset.drive_db);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.wave);
        visitor(&self.cutoff);
        visitor(&self.resonance);
        visitor(&self.env_mod);
        visitor(&self.decay);
        visitor(&self.accent);
        visitor(&self.slide);
        visitor(&self.drive);
        visitor(&self.velocity);
    }
}

/// An offset from -1 to 1, 0 in the middle of the knob.
fn offset(id: &str, name: &str) -> FloatParam {
    FloatParam::new(id, name, FloatRange::linear(-1.0, 1.0), 0.0).with_precision(2)
}

/// A factor from `min` to `max`, 1 by default, that moves evenly in ratios.
fn ratio(id: &str, name: &str, min: f32, max: f32) -> FloatParam {
    FloatParam::new(id, name, FloatRange::logarithmic(min, max), 1.0)
        .with_unit("x")
        .with_precision(2)
}

/// A factor from 0 to 2, 1 by default.
fn scale(id: &str, name: &str) -> FloatParam {
    FloatParam::new(id, name, FloatRange::linear(0.0, 2.0), 1.0)
        .with_unit("x")
        .with_precision(2)
}

fn cents(id: &str, name: &str, max: f32, default: f64) -> FloatParam {
    FloatParam::new(id, name, FloatRange::linear(0.0, max), default as f32)
        .with_unit("ct")
        .with_precision(1)
}

fn tone_hertz(id: &str, name: &str, min: f32, max: f32, default: f64) -> FloatParam {
    FloatParam::new(id, name, FloatRange::logarithmic(min, max), default as f32)
        .with_unit("Hz")
        .with_precision(0)
}

/// The drum kit's knobs: the preset, the kit's own (tune, decay, a level
/// per drum of the drum map), offsets on every piece's tone and snap, and
/// the kit's dynamics, which the preset sets.
pub(crate) struct DrumKitKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) kit: KitParams,
    pub(crate) tone: FloatParam,
    pub(crate) snap: FloatParam,
    pub(crate) velocity: FloatParam,
    pub(crate) accent: FloatParam,
}

impl DrumKitKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::modelled::drum_kit_preset(0);
        Self {
            preset: ChoiceParam::new(ids::DRUM_KIT_PRESET, "Preset", &DRUM_KIT_PRESET_NAMES, 0),
            kit: KitParams::new(),
            tone: offset(ids::DRUM_KIT_TONE, "Tone"),
            snap: offset(ids::DRUM_KIT_SNAP, "Snap"),
            velocity: FloatParam::new(
                ids::DRUM_KIT_VELOCITY,
                "Velocity",
                FloatRange::linear(0.0, 60.0),
                preset.velocity_range_db as f32,
            )
            .with_unit("dB")
            .with_precision(1),
            accent: FloatParam::new(
                ids::DRUM_KIT_ACCENT,
                "Accent",
                FloatRange::linear(0.0, 12.0),
                preset.accent_db as f32,
            )
            .with_unit("dB")
            .with_precision(1),
        }
    }

    /// Moves the dynamics to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::modelled::drum_kit_preset(self.preset.index());
        self.velocity.set_plain(preset.velocity_range_db);
        self.accent.set_plain(preset.accent_db);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        self.kit.visit(visitor);
        visitor(&self.tone);
        visitor(&self.snap);
        visitor(&self.velocity);
        visitor(&self.accent);
    }
}

/// The polyphonic synthesiser's knobs: the preset, macros on what it
/// sets, and the modulation wheel, which presets leave alone.
pub(crate) struct PolyKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) cutoff: FloatParam,
    pub(crate) resonance: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) detune: FloatParam,
    pub(crate) spread: FloatParam,
    pub(crate) mod_wheel: FloatParam,
    pub(crate) voices: IntParam,
}

impl PolyKnobs {
    pub(crate) fn new() -> Self {
        Self {
            preset: ChoiceParam::new(ids::POLY_PRESET, "Preset", &POLY_PRESET_NAMES, 0),
            cutoff: FloatParam::new(
                ids::POLY_CUTOFF,
                "Cutoff",
                FloatRange::linear(-4.0, 4.0),
                0.0,
            )
            .with_unit("oct")
            .with_precision(2),
            resonance: offset(ids::POLY_RESONANCE, "Resonance"),
            attack: ratio(ids::POLY_ATTACK, "Attack", 0.1, 10.0),
            release: ratio(ids::POLY_RELEASE, "Release", 0.1, 10.0),
            detune: scale(ids::POLY_DETUNE, "Detune"),
            spread: scale(ids::POLY_SPREAD, "Spread"),
            mod_wheel: unit(ids::POLY_MOD_WHEEL, "Mod wheel", 0.0).with_precision(2),
            voices: IntParam::new(
                ids::POLY_POLYPHONY,
                "Voices",
                1,
                crate::POLY_VOICES as i32,
                crate::modelled::poly_voices(0) as i32,
            ),
        }
    }

    /// Puts the macros back to neutral, the preset as it is, and the
    /// voices to as many as the preset plays.
    pub(crate) fn load_preset(&self) {
        self.voices
            .set_plain(crate::modelled::poly_voices(self.preset.index()) as f64);
        self.cutoff.set_plain(0.0);
        self.resonance.set_plain(0.0);
        self.attack.set_plain(1.0);
        self.release.set_plain(1.0);
        self.detune.set_plain(1.0);
        self.spread.set_plain(1.0);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.cutoff);
        visitor(&self.resonance);
        visitor(&self.attack);
        visitor(&self.release);
        visitor(&self.detune);
        visitor(&self.spread);
        visitor(&self.mod_wheel);
        visitor(&self.voices);
    }
}

/// The mallets' knobs: the preset, factors on what it sets, and the
/// release, the strike position and the motor, which it sets.
pub(crate) struct MalletKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) hardness: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) position: FloatParam,
    pub(crate) motor: FloatParam,
}

impl MalletKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::modelled::mallet_preset(0);
        Self {
            preset: ChoiceParam::new(ids::MALLET_PRESET, "Preset", &MALLET_PRESET_NAMES, 0),
            hardness: ratio(ids::MALLET_HARDNESS, "Hardness", 0.25, 4.0),
            decay: ratio(ids::MALLET_DECAY, "Decay", 0.25, 4.0),
            release: seconds(
                ids::MALLET_RELEASE,
                "Release",
                0.01,
                60.0,
                preset.release as f32,
            ),
            position: unit(
                ids::MALLET_POSITION,
                "Strike position",
                preset.strike_position as f32,
            )
            .with_precision(3),
            motor: FloatParam::new(
                ids::MALLET_MOTOR,
                "Motor",
                FloatRange::linear(0.0, 16.0),
                preset.motor_rate as f32,
            )
            .with_unit("Hz")
            .with_precision(2),
        }
    }

    /// Puts the factors back to 1 and the release, the position and the
    /// motor to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::modelled::mallet_preset(self.preset.index());
        self.hardness.set_plain(1.0);
        self.decay.set_plain(1.0);
        self.release.set_plain(preset.release);
        self.position.set_plain(preset.strike_position);
        self.motor.set_plain(preset.motor_rate);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.hardness);
        visitor(&self.decay);
        visitor(&self.release);
        visitor(&self.position);
        visitor(&self.motor);
    }
}

/// The bowed strings' knobs: the preset and what it sets, then legato,
/// which presets leave alone.
pub(crate) struct BowedKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) pressure: FloatParam,
    pub(crate) position: FloatParam,
    pub(crate) vibrato: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) tone: FloatParam,
    pub(crate) portamento: BoolParam,
    pub(crate) legato: BoolParam,
}

impl BowedKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::modelled::bowed_preset(0);
        Self {
            preset: ChoiceParam::new(ids::BOWED_PRESET, "Preset", &BOWED_PRESET_NAMES, 0),
            pressure: unit(ids::BOWED_PRESSURE, "Bow pressure", preset.pressure as f32)
                .with_precision(2),
            position: FloatParam::new(
                ids::BOWED_POSITION,
                "Bow position",
                FloatRange::linear(0.02, 0.4),
                preset.bow_position as f32,
            )
            .with_precision(3),
            vibrato: cents(ids::BOWED_VIBRATO, "Vibrato", 100.0, preset.vibrato_depth),
            attack: seconds(
                ids::BOWED_ATTACK,
                "Attack",
                0.005,
                2.0,
                preset.attack as f32,
            ),
            release: seconds(
                ids::BOWED_RELEASE,
                "Release",
                0.02,
                10.0,
                preset.release as f32,
            ),
            tone: tone_hertz(ids::BOWED_TONE, "Tone", 500.0, 20_000.0, preset.tone),
            portamento: BoolParam::new(ids::BOWED_PORTAMENTO, "Portamento", preset.portamento),
            legato: BoolParam::new(ids::BOWED_LEGATO, "Legato", false),
        }
    }

    /// Moves every knob but legato to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::modelled::bowed_preset(self.preset.index());
        self.pressure.set_plain(preset.pressure);
        self.position.set_plain(preset.bow_position);
        self.vibrato.set_plain(preset.vibrato_depth);
        self.attack.set_plain(preset.attack);
        self.release.set_plain(preset.release);
        self.tone.set_plain(preset.tone);
        self.portamento
            .set_plain(f64::from(u8::from(preset.portamento)));
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.pressure);
        visitor(&self.position);
        visitor(&self.vibrato);
        visitor(&self.attack);
        visitor(&self.release);
        visitor(&self.tone);
        visitor(&self.portamento);
        visitor(&self.legato);
    }
}

/// The winds' knobs: the preset and what it sets, then legato, which
/// presets leave alone.
pub(crate) struct WindKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) noise: FloatParam,
    pub(crate) vibrato: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) tone: FloatParam,
    pub(crate) portamento: BoolParam,
    pub(crate) legato: BoolParam,
}

impl WindKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::modelled::wind_preset(0);
        Self {
            preset: ChoiceParam::new(ids::WIND_PRESET, "Preset", &WIND_PRESET_NAMES, 0),
            noise: unit(ids::WIND_NOISE, "Breath noise", preset.noise as f32).with_precision(2),
            vibrato: cents(ids::WIND_VIBRATO, "Vibrato", 100.0, preset.vibrato_depth),
            attack: seconds(ids::WIND_ATTACK, "Attack", 0.002, 1.0, preset.attack as f32),
            release: seconds(
                ids::WIND_RELEASE,
                "Release",
                0.005,
                2.0,
                preset.release as f32,
            ),
            tone: tone_hertz(ids::WIND_TONE, "Tone", 500.0, 20_000.0, preset.tone),
            portamento: BoolParam::new(ids::WIND_PORTAMENTO, "Portamento", preset.portamento),
            legato: BoolParam::new(ids::WIND_LEGATO, "Legato", true),
        }
    }

    /// Moves every knob but legato to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::modelled::wind_preset(self.preset.index());
        self.noise.set_plain(preset.noise);
        self.vibrato.set_plain(preset.vibrato_depth);
        self.attack.set_plain(preset.attack);
        self.release.set_plain(preset.release);
        self.tone.set_plain(preset.tone);
        self.portamento
            .set_plain(f64::from(u8::from(preset.portamento)));
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.noise);
        visitor(&self.vibrato);
        visitor(&self.attack);
        visitor(&self.release);
        visitor(&self.tone);
        visitor(&self.portamento);
        visitor(&self.legato);
    }
}

/// The choir's knobs: the preset and what it sets, then legato, which
/// presets leave alone.
pub(crate) struct ChoirKnobs {
    pub(crate) preset: ChoiceParam,
    pub(crate) vowel: FloatParam,
    pub(crate) vibrato: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) breathiness: FloatParam,
    pub(crate) brightness: FloatParam,
    pub(crate) legato: BoolParam,
}

impl ChoirKnobs {
    pub(crate) fn new() -> Self {
        let preset = crate::modelled::choir_preset(0);
        Self {
            preset: ChoiceParam::new(ids::CHOIR_PRESET, "Preset", &CHOIR_PRESET_NAMES, 0),
            vowel: FloatParam::new(
                ids::CHOIR_VOWEL,
                "Vowel",
                FloatRange::linear(0.0, 4.0),
                preset.vowel as f32,
            )
            .with_precision(2),
            vibrato: cents(ids::CHOIR_VIBRATO, "Vibrato", 200.0, preset.vibrato_depth),
            attack: seconds(
                ids::CHOIR_ATTACK,
                "Attack",
                0.005,
                2.0,
                preset.attack as f32,
            ),
            release: seconds(
                ids::CHOIR_RELEASE,
                "Release",
                0.01,
                4.0,
                preset.release as f32,
            ),
            breathiness: unit(
                ids::CHOIR_BREATHINESS,
                "Breathiness",
                preset.breathiness as f32,
            )
            .with_precision(2),
            brightness: tone_hertz(
                ids::CHOIR_BRIGHTNESS,
                "Brightness",
                500.0,
                12_000.0,
                preset.brightness,
            ),
            legato: BoolParam::new(ids::CHOIR_LEGATO, "Legato", false),
        }
    }

    /// Moves every knob but legato to the preset now chosen.
    pub(crate) fn load_preset(&self) {
        let preset = crate::modelled::choir_preset(self.preset.index());
        self.vowel.set_plain(preset.vowel);
        self.vibrato.set_plain(preset.vibrato_depth);
        self.attack.set_plain(preset.attack);
        self.release.set_plain(preset.release);
        self.breathiness.set_plain(preset.breathiness);
        self.brightness.set_plain(preset.brightness);
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.preset);
        visitor(&self.vowel);
        visitor(&self.vibrato);
        visitor(&self.attack);
        visitor(&self.release);
        visitor(&self.breathiness);
        visitor(&self.brightness);
        visitor(&self.legato);
    }
}

/// A sampler part's own parameters: how it reads its samples, and whether
/// it plays them at the loudness target. A sampled kit has the kit's too,
/// for the drums its SFZ text leaves out.
pub(crate) struct SamplerParams {
    pub(crate) interpolation: ChoiceParam,
    pub(crate) normalize: BoolParam,
    pub(crate) kit: Option<KitParams>,
}

impl SamplerParams {
    pub(crate) fn new(kit: bool) -> Self {
        Self {
            interpolation: ChoiceParam::new(
                ids::SAMPLER_INTERPOLATION,
                "Interpolation",
                &INTERPOLATION_NAMES,
                0,
            ),
            // Switched while notes sound, the level glides.
            normalize: BoolParam::new(ids::SAMPLER_NORMALIZE, "Normalize loudness", true)
                .with_smoothing(LEVEL_SMOOTHING),
            kit: kit.then(KitParams::new),
        }
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        visitor(&self.interpolation);
        visitor(&self.normalize);
        if let Some(kit) = &self.kit {
            kit.visit(visitor);
        }
    }
}

pub(crate) struct LowpassParams {
    pub(crate) on: BoolParam,
    pub(crate) cutoff: FloatParam,
    pub(crate) resonance: FloatParam,
}

pub(crate) struct ReverbParams {
    pub(crate) on: BoolParam,
    pub(crate) mix: FloatParam,
    pub(crate) decay: FloatParam,
}

pub(crate) struct HighpassParams {
    pub(crate) on: BoolParam,
    pub(crate) cutoff: FloatParam,
    pub(crate) resonance: FloatParam,
}

pub(crate) struct EqParams {
    pub(crate) on: BoolParam,
    pub(crate) low_gain: FloatParam,
    pub(crate) mid_gain: FloatParam,
    pub(crate) high_gain: FloatParam,
    pub(crate) low_freq: FloatParam,
    pub(crate) mid_freq: FloatParam,
    pub(crate) mid_q: FloatParam,
    pub(crate) high_freq: FloatParam,
}

pub(crate) struct DriveParams {
    pub(crate) on: BoolParam,
    pub(crate) amount: FloatParam,
    pub(crate) output: FloatParam,
    pub(crate) mix: FloatParam,
    pub(crate) shape: ChoiceParam,
}

pub(crate) struct BitcrushParams {
    pub(crate) on: BoolParam,
    pub(crate) bits: FloatParam,
    pub(crate) rate: FloatParam,
    pub(crate) mix: FloatParam,
}

pub(crate) struct CompressorParams {
    pub(crate) on: BoolParam,
    pub(crate) threshold: FloatParam,
    pub(crate) ratio: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) knee: FloatParam,
    pub(crate) makeup: FloatParam,
    pub(crate) mix: FloatParam,
}

pub(crate) struct ChorusParams {
    pub(crate) on: BoolParam,
    pub(crate) mix: FloatParam,
    pub(crate) rate: FloatParam,
    pub(crate) depth: FloatParam,
    pub(crate) delay: FloatParam,
    pub(crate) voices: IntParam,
    pub(crate) spread: FloatParam,
}

pub(crate) struct FlangerParams {
    pub(crate) on: BoolParam,
    pub(crate) mix: FloatParam,
    pub(crate) rate: FloatParam,
    pub(crate) depth: FloatParam,
    pub(crate) feedback: FloatParam,
    pub(crate) delay: FloatParam,
}

pub(crate) struct PhaserParams {
    pub(crate) on: BoolParam,
    pub(crate) mix: FloatParam,
    pub(crate) rate: FloatParam,
    pub(crate) depth: FloatParam,
    pub(crate) center: FloatParam,
    pub(crate) feedback: FloatParam,
    pub(crate) stages: IntParam,
    pub(crate) spread: FloatParam,
}

/// The tremolo's and the auto-pan's: a wave that moves a gain or a place.
pub(crate) struct ModulatorParams {
    pub(crate) on: BoolParam,
    pub(crate) rate: FloatParam,
    pub(crate) depth: FloatParam,
    pub(crate) shape: ChoiceParam,
    pub(crate) sync: FloatParam,
}

pub(crate) struct DelayParams {
    pub(crate) on: BoolParam,
    pub(crate) time: FloatParam,
    pub(crate) feedback: FloatParam,
    pub(crate) mix: FloatParam,
    pub(crate) damping: FloatParam,
    pub(crate) pingpong: FloatParam,
    pub(crate) sync: FloatParam,
}

/// The parameters of every stage a part's chain can have, kept whether
/// the stage is in the chain or not, then gain, pan and the send.
pub(crate) struct ChainParams {
    pub(crate) lowpass: LowpassParams,
    pub(crate) reverb: ReverbParams,
    pub(crate) highpass: HighpassParams,
    pub(crate) eq: EqParams,
    pub(crate) drive: DriveParams,
    pub(crate) bitcrush: BitcrushParams,
    pub(crate) compressor: CompressorParams,
    pub(crate) chorus: ChorusParams,
    pub(crate) flanger: FlangerParams,
    pub(crate) phaser: PhaserParams,
    pub(crate) tremolo: ModulatorParams,
    pub(crate) autopan: ModulatorParams,
    pub(crate) delay: DelayParams,
    pub(crate) gain: FloatParam,
    pub(crate) pan: FloatParam,
    pub(crate) send: FloatParam,
}

/// The parameters of the tremolo or the auto-pan, under the prefix given.
fn modulator(
    ids: [&str; 5],
    names: [&str; 5],
    rate_range: (f32, f32, f32),
    depth: f32,
) -> ModulatorParams {
    let (min, max, default) = rate_range;
    ModulatorParams {
        on: switch(ids[0], names[0], false),
        rate: rate(ids[1], names[1], min, max, default),
        depth: amount(ids[2], names[2], depth),
        shape: ChoiceParam::new(ids[3], names[3], &LFO_SHAPE_NAMES, 0),
        sync: beats(ids[4], names[4]),
    }
}

impl ChainParams {
    pub(crate) fn new() -> Self {
        Self {
            lowpass: LowpassParams {
                on: switch(ids::LOWPASS_ON, "Low-pass", true),
                cutoff: FloatParam::new(
                    ids::LOWPASS_CUTOFF,
                    "Low-pass cutoff",
                    FloatRange::logarithmic(20.0, 20_000.0),
                    20_000.0,
                )
                .with_unit("Hz")
                .with_precision(0)
                .with_smoothing(FILTER_SMOOTHING),
                resonance: unit(ids::LOWPASS_RESONANCE, "Low-pass resonance", 0.0)
                    .with_smoothing(FILTER_SMOOTHING),
            },
            reverb: ReverbParams {
                on: switch(ids::REVERB_ON, "Reverb", true),
                mix: unit(ids::REVERB_MIX, "Reverb mix", 0.15).with_smoothing(LEVEL_SMOOTHING),
                decay: seconds(ids::REVERB_DECAY, "Reverb decay", 0.2, 10.0, 1.6),
            },
            highpass: HighpassParams {
                on: switch(ids::HIGHPASS_ON, "High-pass", false),
                cutoff: hertz(
                    ids::HIGHPASS_CUTOFF,
                    "High-pass cutoff",
                    20.0,
                    20_000.0,
                    200.0,
                ),
                resonance: unit(ids::HIGHPASS_RESONANCE, "High-pass resonance", 0.0)
                    .with_smoothing(FILTER_SMOOTHING),
            },
            eq: EqParams {
                on: switch(ids::EQ_ON, "EQ", false),
                low_gain: decibels(ids::EQ_LOW_GAIN, "Low gain", -24.0, 24.0, 0.0),
                mid_gain: decibels(ids::EQ_MID_GAIN, "Mid gain", -24.0, 24.0, 0.0),
                high_gain: decibels(ids::EQ_HIGH_GAIN, "High gain", -24.0, 24.0, 0.0),
                low_freq: hertz(ids::EQ_LOW_FREQ, "Low frequency", 20.0, 1_000.0, 120.0),
                mid_freq: hertz(ids::EQ_MID_FREQ, "Mid frequency", 100.0, 10_000.0, 1_000.0),
                mid_q: FloatParam::new(
                    ids::EQ_MID_Q,
                    "Mid Q",
                    FloatRange::logarithmic(0.1, 10.0),
                    0.7,
                )
                .with_precision(2)
                .with_smoothing(FILTER_SMOOTHING),
                high_freq: hertz(
                    ids::EQ_HIGH_FREQ,
                    "High frequency",
                    1_000.0,
                    20_000.0,
                    8_000.0,
                ),
            },
            drive: DriveParams {
                on: switch(ids::DRIVE_ON, "Drive", false),
                amount: decibels(ids::DRIVE_AMOUNT, "Drive", 0.0, 36.0, 12.0),
                output: decibels(ids::DRIVE_OUTPUT, "Drive output", -24.0, 12.0, -6.0),
                mix: amount(ids::DRIVE_MIX, "Drive mix", 1.0),
                shape: ChoiceParam::new(ids::DRIVE_SHAPE, "Drive shape", &DRIVE_SHAPE_NAMES, 0)
                    .with_smoothing(SWITCH_SMOOTHING),
            },
            bitcrush: BitcrushParams {
                on: switch(ids::BITCRUSH_ON, "Bitcrush", false),
                bits: FloatParam::new(
                    ids::BITCRUSH_BITS,
                    "Bits",
                    FloatRange::linear(1.0, 16.0),
                    8.0,
                )
                .with_unit("bits")
                .with_precision(1)
                .with_smoothing(LEVEL_SMOOTHING),
                rate: rate(ids::BITCRUSH_RATE, "Crush rate", 100.0, 48_000.0, 48_000.0)
                    .with_precision(0),
                mix: amount(ids::BITCRUSH_MIX, "Bitcrush mix", 1.0),
            },
            compressor: CompressorParams {
                on: switch(ids::COMPRESSOR_ON, "Compressor", false),
                threshold: FloatParam::new(
                    ids::COMPRESSOR_THRESHOLD,
                    "Threshold",
                    FloatRange::linear(-60.0, 0.0),
                    -18.0,
                )
                .with_unit("dB")
                .with_precision(1),
                ratio: FloatParam::new(
                    ids::COMPRESSOR_RATIO,
                    "Ratio",
                    FloatRange::logarithmic(1.0, 20.0),
                    4.0,
                )
                .with_precision(1),
                attack: milliseconds(ids::COMPRESSOR_ATTACK, "Attack", 0.1, 200.0, 10.0),
                release: milliseconds(ids::COMPRESSOR_RELEASE, "Release", 5.0, 2_000.0, 120.0),
                knee: FloatParam::new(
                    ids::COMPRESSOR_KNEE,
                    "Knee",
                    FloatRange::linear(0.0, 24.0),
                    6.0,
                )
                .with_unit("dB")
                .with_precision(1),
                makeup: decibels(ids::COMPRESSOR_MAKEUP, "Make-up", 0.0, 24.0, 0.0),
                mix: amount(ids::COMPRESSOR_MIX, "Compressor mix", 1.0),
            },
            chorus: ChorusParams {
                on: switch(ids::CHORUS_ON, "Chorus", false),
                mix: amount(ids::CHORUS_MIX, "Chorus mix", 0.5),
                rate: rate(ids::CHORUS_RATE, "Chorus rate", 0.05, 10.0, 0.8),
                depth: FloatParam::new(
                    ids::CHORUS_DEPTH,
                    "Chorus depth",
                    FloatRange::linear(0.0, 10.0),
                    2.5,
                )
                .with_unit("ms")
                .with_precision(2)
                .with_smoothing(SWEEP_SMOOTHING),
                delay: FloatParam::new(
                    ids::CHORUS_DELAY,
                    "Chorus delay",
                    FloatRange::linear(1.0, 40.0),
                    15.0,
                )
                .with_unit("ms")
                .with_precision(1)
                .with_smoothing(SWEEP_SMOOTHING),
                voices: IntParam::new(ids::CHORUS_VOICES, "Voices", 1, 8, 3)
                    .with_smoothing(SWEEP_SMOOTHING),
                spread: amount(ids::CHORUS_SPREAD, "Chorus spread", 1.0)
                    .with_smoothing(SWEEP_SMOOTHING),
            },
            flanger: FlangerParams {
                on: switch(ids::FLANGER_ON, "Flanger", false),
                mix: amount(ids::FLANGER_MIX, "Flanger mix", 0.5),
                rate: rate(ids::FLANGER_RATE, "Flanger rate", 0.02, 10.0, 0.25),
                depth: FloatParam::new(
                    ids::FLANGER_DEPTH,
                    "Flanger depth",
                    FloatRange::linear(0.0, 5.0),
                    1.5,
                )
                .with_unit("ms")
                .with_precision(2)
                .with_smoothing(SWEEP_SMOOTHING),
                feedback: FloatParam::new(
                    ids::FLANGER_FEEDBACK,
                    "Flanger feedback",
                    FloatRange::linear(-0.95, 0.95),
                    0.7,
                )
                .with_precision(2)
                .with_smoothing(LEVEL_SMOOTHING),
                delay: FloatParam::new(
                    ids::FLANGER_DELAY,
                    "Flanger delay",
                    FloatRange::linear(0.5, 10.0),
                    2.0,
                )
                .with_unit("ms")
                .with_precision(2)
                .with_smoothing(SWEEP_SMOOTHING),
            },
            phaser: PhaserParams {
                on: switch(ids::PHASER_ON, "Phaser", false),
                mix: amount(ids::PHASER_MIX, "Phaser mix", 0.5),
                rate: rate(ids::PHASER_RATE, "Phaser rate", 0.02, 10.0, 0.5),
                depth: FloatParam::new(
                    ids::PHASER_DEPTH,
                    "Phaser depth",
                    FloatRange::linear(0.0, 6.0),
                    2.0,
                )
                .with_unit("oct")
                .with_precision(2)
                .with_smoothing(SWEEP_SMOOTHING),
                center: hertz(
                    ids::PHASER_CENTER,
                    "Phaser centre",
                    100.0,
                    10_000.0,
                    1_000.0,
                )
                .with_smoothing(SWEEP_SMOOTHING),
                feedback: FloatParam::new(
                    ids::PHASER_FEEDBACK,
                    "Phaser feedback",
                    FloatRange::linear(-0.95, 0.95),
                    0.3,
                )
                .with_precision(2)
                .with_smoothing(LEVEL_SMOOTHING),
                stages: IntParam::new(ids::PHASER_STAGES, "Stages", 2, 12, 4)
                    .with_smoothing(SWEEP_SMOOTHING),
                spread: amount(ids::PHASER_SPREAD, "Phaser spread", 0.0)
                    .with_smoothing(SWEEP_SMOOTHING),
            },
            tremolo: modulator(
                [
                    ids::TREMOLO_ON,
                    ids::TREMOLO_RATE,
                    ids::TREMOLO_DEPTH,
                    ids::TREMOLO_SHAPE,
                    ids::TREMOLO_SYNC,
                ],
                [
                    "Tremolo",
                    "Tremolo rate",
                    "Tremolo depth",
                    "Tremolo shape",
                    "Tremolo sync",
                ],
                (0.1, 30.0, 5.0),
                0.5,
            ),
            autopan: modulator(
                [
                    ids::AUTOPAN_ON,
                    ids::AUTOPAN_RATE,
                    ids::AUTOPAN_DEPTH,
                    ids::AUTOPAN_SHAPE,
                    ids::AUTOPAN_SYNC,
                ],
                [
                    "Auto-pan",
                    "Auto-pan rate",
                    "Auto-pan depth",
                    "Auto-pan shape",
                    "Auto-pan sync",
                ],
                (0.05, 20.0, 0.5),
                1.0,
            ),
            delay: DelayParams {
                on: switch(ids::DELAY_ON, "Delay", false),
                time: seconds(ids::DELAY_TIME, "Delay time", 0.001, 4.0, 0.375)
                    .with_smoothing(LEVEL_SMOOTHING),
                feedback: FloatParam::new(
                    ids::DELAY_FEEDBACK,
                    "Feedback",
                    FloatRange::linear(0.0, 0.95),
                    0.35,
                )
                .with_precision(2)
                .with_smoothing(LEVEL_SMOOTHING),
                mix: amount(ids::DELAY_MIX, "Delay mix", 0.3),
                damping: amount(ids::DELAY_DAMPING, "Damping", 0.3),
                pingpong: amount(ids::DELAY_PINGPONG, "Ping-pong", 0.0),
                sync: beats(ids::DELAY_SYNC, "Delay sync").with_smoothing(LEVEL_SMOOTHING),
            },
            gain: FloatParam::new(
                ids::GAIN,
                "Gain",
                FloatRange::linear(GAIN_FLOOR_DB, 12.0),
                0.0,
            )
            .with_unit("dB")
            .with_precision(1)
            .with_smoothing(LEVEL_SMOOTHING),
            pan: FloatParam::new(ids::PAN, "Pan", FloatRange::linear(-1.0, 1.0), 0.0)
                .with_precision(2)
                .with_smoothing(LEVEL_SMOOTHING),
            send: amount(ids::SEND, "Reverb send", 0.0),
        }
    }

    /// The switch of `effect`.
    pub(crate) fn switch(&self, effect: Effect) -> Option<&BoolParam> {
        Some(match effect {
            Effect::Lowpass => &self.lowpass.on,
            Effect::Reverb => &self.reverb.on,
            Effect::Highpass => &self.highpass.on,
            Effect::Eq => &self.eq.on,
            Effect::Drive => &self.drive.on,
            Effect::Bitcrush => &self.bitcrush.on,
            Effect::Compressor => &self.compressor.on,
            Effect::Chorus => &self.chorus.on,
            Effect::Flanger => &self.flanger.on,
            Effect::Phaser => &self.phaser.on,
            Effect::Tremolo => &self.tremolo.on,
            Effect::Autopan => &self.autopan.on,
            Effect::Delay => &self.delay.on,
            Effect::Glue | Effect::Limiter => return None,
        })
    }

    /// Visits the parameters of `effect`, its switch first.
    pub(crate) fn visit_effect<'a>(
        &'a self,
        effect: Effect,
        visitor: &mut dyn FnMut(&'a dyn Param),
    ) {
        match effect {
            Effect::Lowpass => {
                let p = &self.lowpass;
                visitor(&p.on);
                visitor(&p.cutoff);
                visitor(&p.resonance);
            }
            Effect::Reverb => {
                let p = &self.reverb;
                visitor(&p.on);
                visitor(&p.mix);
                visitor(&p.decay);
            }
            Effect::Highpass => {
                let p = &self.highpass;
                visitor(&p.on);
                visitor(&p.cutoff);
                visitor(&p.resonance);
            }
            Effect::Eq => {
                let p = &self.eq;
                visitor(&p.on);
                visitor(&p.low_gain);
                visitor(&p.mid_gain);
                visitor(&p.high_gain);
                visitor(&p.low_freq);
                visitor(&p.mid_freq);
                visitor(&p.mid_q);
                visitor(&p.high_freq);
            }
            Effect::Drive => {
                let p = &self.drive;
                visitor(&p.on);
                visitor(&p.amount);
                visitor(&p.output);
                visitor(&p.mix);
                visitor(&p.shape);
            }
            Effect::Bitcrush => {
                let p = &self.bitcrush;
                visitor(&p.on);
                visitor(&p.bits);
                visitor(&p.rate);
                visitor(&p.mix);
            }
            Effect::Compressor => {
                let p = &self.compressor;
                visitor(&p.on);
                visitor(&p.threshold);
                visitor(&p.ratio);
                visitor(&p.attack);
                visitor(&p.release);
                visitor(&p.knee);
                visitor(&p.makeup);
                visitor(&p.mix);
            }
            Effect::Chorus => {
                let p = &self.chorus;
                visitor(&p.on);
                visitor(&p.mix);
                visitor(&p.rate);
                visitor(&p.depth);
                visitor(&p.delay);
                visitor(&p.voices);
                visitor(&p.spread);
            }
            Effect::Flanger => {
                let p = &self.flanger;
                visitor(&p.on);
                visitor(&p.mix);
                visitor(&p.rate);
                visitor(&p.depth);
                visitor(&p.feedback);
                visitor(&p.delay);
            }
            Effect::Phaser => {
                let p = &self.phaser;
                visitor(&p.on);
                visitor(&p.mix);
                visitor(&p.rate);
                visitor(&p.depth);
                visitor(&p.center);
                visitor(&p.feedback);
                visitor(&p.stages);
                visitor(&p.spread);
            }
            Effect::Tremolo | Effect::Autopan => {
                let p = if effect == Effect::Tremolo {
                    &self.tremolo
                } else {
                    &self.autopan
                };
                visitor(&p.on);
                visitor(&p.rate);
                visitor(&p.depth);
                visitor(&p.shape);
                visitor(&p.sync);
            }
            Effect::Delay => {
                let p = &self.delay;
                visitor(&p.on);
                visitor(&p.time);
                visitor(&p.feedback);
                visitor(&p.mix);
                visitor(&p.damping);
                visitor(&p.pingpong);
                visitor(&p.sync);
            }
            Effect::Glue | Effect::Limiter => {}
        }
    }
}

/// Boxed, because the synth's parameters are far larger than the kit's.
pub(crate) enum InstrumentParams {
    Synth(Box<SynthParams>),
    Drums(Box<KitParams>),
    Sampler(Box<SamplerParams>),
    Plucked(Box<PluckedKnobs>),
    Fm(Box<FmKnobs>),
    Organ(Box<OrganKnobs>),
    AnalogKit(Box<AnalogKitKnobs>),
    KickBass(Box<KickBassKnobs>),
    Acid(Box<AcidKnobs>),
    DrumKit(Box<DrumKitKnobs>),
    Poly(Box<PolyKnobs>),
    Mallet(Box<MalletKnobs>),
    Bowed(Box<BowedKnobs>),
    Wind(Box<WindKnobs>),
    Choir(Box<ChoirKnobs>),
}

impl InstrumentParams {
    /// The parameters of an instrument of `kind`, at their defaults.
    pub(crate) fn new(kind: InstrumentKind) -> Self {
        match kind {
            InstrumentKind::Synth => Self::Synth(Box::new(SynthParams::new())),
            InstrumentKind::Drums => Self::Drums(Box::new(KitParams::new())),
            InstrumentKind::Sampler => Self::Sampler(Box::new(SamplerParams::new(false))),
            InstrumentKind::SamplerKit => Self::Sampler(Box::new(SamplerParams::new(true))),
            InstrumentKind::Plucked => Self::Plucked(Box::new(PluckedKnobs::new())),
            InstrumentKind::Fm => Self::Fm(Box::new(FmKnobs::new())),
            InstrumentKind::Organ => Self::Organ(Box::new(OrganKnobs::new())),
            InstrumentKind::AnalogKit => Self::AnalogKit(Box::new(AnalogKitKnobs::new())),
            InstrumentKind::KickBass => Self::KickBass(Box::new(KickBassKnobs::new())),
            InstrumentKind::Acid => Self::Acid(Box::new(AcidKnobs::new())),
            InstrumentKind::DrumKit => Self::DrumKit(Box::new(DrumKitKnobs::new())),
            InstrumentKind::Poly => Self::Poly(Box::new(PolyKnobs::new())),
            InstrumentKind::Mallet => Self::Mallet(Box::new(MalletKnobs::new())),
            InstrumentKind::Bowed => Self::Bowed(Box::new(BowedKnobs::new())),
            InstrumentKind::Wind => Self::Wind(Box::new(WindKnobs::new())),
            InstrumentKind::Choir => Self::Choir(Box::new(ChoirKnobs::new())),
        }
    }

    pub(crate) fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        match self {
            Self::Synth(synth) => synth.visit(visitor),
            Self::Drums(kit) => kit.visit(visitor),
            Self::Sampler(sampler) => sampler.visit(visitor),
            Self::Plucked(knobs) => knobs.visit(visitor),
            Self::Fm(knobs) => knobs.visit(visitor),
            Self::Organ(knobs) => knobs.visit(visitor),
            Self::AnalogKit(knobs) => knobs.visit(visitor),
            Self::KickBass(knobs) => knobs.visit(visitor),
            Self::Acid(knobs) => knobs.visit(visitor),
            Self::DrumKit(knobs) => knobs.visit(visitor),
            Self::Poly(knobs) => knobs.visit(visitor),
            Self::Mallet(knobs) => knobs.visit(visitor),
            Self::Bowed(knobs) => knobs.visit(visitor),
            Self::Wind(knobs) => knobs.visit(visitor),
            Self::Choir(knobs) => knobs.visit(visitor),
        }
    }

    /// Moves the knobs a preset covers to its positions, if `id` is the
    /// instrument's preset. Real-time safe: the parameters are atomics.
    pub(crate) fn load_preset(&self, id: ParamId) {
        match self {
            Self::Plucked(knobs) if id == PLUCKED_PRESET => knobs.load_preset(),
            Self::Fm(knobs) if id == FM_PRESET => knobs.load_preset(),
            Self::Organ(knobs) if id == ORGAN_PRESET => knobs.load_preset(),
            Self::Acid(knobs) if id == ACID_PRESET => knobs.load_preset(),
            Self::DrumKit(knobs) if id == DRUM_KIT_PRESET => knobs.load_preset(),
            Self::Poly(knobs) if id == POLY_PRESET => knobs.load_preset(),
            Self::Mallet(knobs) if id == MALLET_PRESET => knobs.load_preset(),
            Self::Bowed(knobs) if id == BOWED_PRESET => knobs.load_preset(),
            Self::Wind(knobs) if id == WIND_PRESET => knobs.load_preset(),
            Self::Choir(knobs) if id == CHOIR_PRESET => knobs.load_preset(),
            _ => {}
        }
    }
}

/// Every parameter of one part: its instrument's, then those of the chain
/// stages it has, then gain, pan and the send.
pub(crate) struct PartParams {
    pub(crate) instrument: InstrumentParams,
    pub(crate) chain: Box<ChainParams>,
    pub(crate) stages: Chain,
}

impl PartParams {
    pub(crate) fn new(kind: InstrumentKind, stages: Chain) -> Self {
        Self {
            instrument: InstrumentParams::new(kind),
            chain: Box::new(ChainParams::new()),
            stages,
        }
    }

    /// The parameters as [`ParamSet::descriptions_json`] gives them, with
    /// `"effect"` and `"visual"` added to each object of an effect.
    pub(crate) fn tagged_json(&self) -> String {
        let mut json = TaggedJson::new();
        let mut plain = |param: &dyn Param| json.push(param, None);
        self.instrument.visit(&mut plain);
        for effect in Effect::PART_ORDER {
            if self.stages.has(effect) {
                self.chain
                    .visit_effect(effect, &mut |param| json.push(param, Some(effect)));
            }
        }
        json.push(&self.chain.gain, None);
        json.push(&self.chain.pan, None);
        json.push(&self.chain.send, None);
        json.finish()
    }
}

impl ParamSet for PartParams {
    fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        self.instrument.visit(visitor);
        for effect in Effect::PART_ORDER {
            if self.stages.has(effect) {
                self.chain.visit_effect(effect, visitor);
            }
        }
        visitor(&self.chain.gain);
        visitor(&self.chain.pan);
        visitor(&self.chain.send);
    }
}

/// A JSON array of parameter descriptions, written one at a time, with an
/// effect's name and its [`Visual`](crate::Visual) after the fields of
/// [`ParamDescription::to_json`](manguebyte_params::ParamDescription::to_json).
struct TaggedJson {
    out: String,
}

impl TaggedJson {
    fn new() -> Self {
        Self {
            out: String::from("["),
        }
    }

    fn push(&mut self, param: &dyn Param, effect: Option<Effect>) {
        if self.out.len() > 1 {
            self.out.push(',');
        }
        param.describe().write_json(&mut self.out);
        if let Some(effect) = effect {
            // The object ends in `}`: reopen it.
            self.out.pop();
            self.out.push_str(",\"effect\":\"");
            self.out.push_str(effect.name());
            self.out.push_str("\",\"visual\":\"");
            self.out.push_str(effect.visual().name());
            self.out.push_str("\"}");
        }
    }

    fn finish(mut self) -> String {
        self.out.push(']');
        self.out
    }
}

pub(crate) struct GlueParams {
    pub(crate) on: BoolParam,
    pub(crate) threshold: FloatParam,
    pub(crate) ratio: FloatParam,
    pub(crate) attack: FloatParam,
    pub(crate) release: FloatParam,
    pub(crate) knee: FloatParam,
    pub(crate) makeup: FloatParam,
}

pub(crate) struct MasterReverbParams {
    pub(crate) level: FloatParam,
    pub(crate) decay: FloatParam,
    pub(crate) size: FloatParam,
    pub(crate) damping: FloatParam,
}

pub(crate) struct LimiterParams {
    pub(crate) ceiling: FloatParam,
    pub(crate) release: FloatParam,
}

/// The parameters of the master bus.
pub(crate) struct MasterParams {
    pub(crate) tempo: FloatParam,
    pub(crate) beat: FloatParam,
    pub(crate) gain: FloatParam,
    pub(crate) reverb: MasterReverbParams,
    pub(crate) glue: GlueParams,
    pub(crate) limiter: LimiterParams,
}

/// The default of `master.limiter.ceiling`, dBTP: the level the engine's
/// samples never went beyond before it had a master bus
/// ([`OUTPUT_CEILING`](crate::OUTPUT_CEILING)), now held for the waveform
/// between them too. One note or one drum at full velocity, accented,
/// stays under it untouched. A set streamed through a lossy codec wants
/// -1 dBTP, as EBU R 128 asks.
pub(crate) const DEFAULT_CEILING_DB: f32 = -0.3;

impl MasterParams {
    pub(crate) fn new() -> Self {
        Self {
            tempo: FloatParam::new(
                ids::MASTER_TEMPO,
                "Tempo",
                FloatRange::linear(20.0, 400.0),
                120.0,
            )
            .with_unit("bpm")
            .with_precision(2),
            beat: FloatParam::new(
                ids::MASTER_BEAT,
                "Beat",
                FloatRange::linear(0.0, 1_000_000.0),
                0.0,
            )
            .with_unit("beats")
            .with_precision(3),
            gain: decibels(ids::MASTER_GAIN, "Master gain", GAIN_FLOOR_DB, 12.0, 0.0),
            reverb: MasterReverbParams {
                level: decibels(
                    ids::MASTER_REVERB_LEVEL,
                    "Reverb return",
                    GAIN_FLOOR_DB,
                    6.0,
                    0.0,
                ),
                decay: seconds(ids::MASTER_REVERB_DECAY, "Reverb decay", 0.2, 10.0, 2.2),
                size: unit(ids::MASTER_REVERB_SIZE, "Reverb size", 0.7).with_precision(2),
                damping: unit(ids::MASTER_REVERB_DAMPING, "Reverb damping", 0.5).with_precision(2),
            },
            glue: GlueParams {
                on: switch(ids::GLUE_ON, "Glue", false),
                threshold: FloatParam::new(
                    ids::GLUE_THRESHOLD,
                    "Glue threshold",
                    FloatRange::linear(-40.0, 0.0),
                    -12.0,
                )
                .with_unit("dB")
                .with_precision(1),
                ratio: FloatParam::new(
                    ids::GLUE_RATIO,
                    "Glue ratio",
                    FloatRange::logarithmic(1.0, 10.0),
                    2.0,
                )
                .with_precision(1),
                attack: milliseconds(ids::GLUE_ATTACK, "Glue attack", 0.1, 100.0, 20.0),
                release: milliseconds(ids::GLUE_RELEASE, "Glue release", 10.0, 2_000.0, 250.0),
                knee: FloatParam::new(
                    ids::GLUE_KNEE,
                    "Glue knee",
                    FloatRange::linear(0.0, 24.0),
                    6.0,
                )
                .with_unit("dB")
                .with_precision(1),
                makeup: decibels(ids::GLUE_MAKEUP, "Glue make-up", 0.0, 18.0, 0.0),
            },
            limiter: LimiterParams {
                ceiling: FloatParam::new(
                    ids::LIMITER_CEILING,
                    "Ceiling",
                    FloatRange::linear(-12.0, -0.3),
                    DEFAULT_CEILING_DB,
                )
                .with_unit("dBTP")
                .with_precision(1),
                release: milliseconds(
                    ids::LIMITER_RELEASE,
                    "Limiter release",
                    10.0,
                    1_000.0,
                    150.0,
                ),
            },
        }
    }

    /// Visits the parameters, tagging those of an effect.
    fn visit_tagged<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param, Option<Effect>)) {
        visitor(&self.tempo, None);
        visitor(&self.beat, None);
        visitor(&self.gain, None);
        let reverb = Some(Effect::Reverb);
        visitor(&self.reverb.level, reverb);
        visitor(&self.reverb.decay, reverb);
        visitor(&self.reverb.size, reverb);
        visitor(&self.reverb.damping, reverb);
        let glue = Some(Effect::Glue);
        visitor(&self.glue.on, glue);
        visitor(&self.glue.threshold, glue);
        visitor(&self.glue.ratio, glue);
        visitor(&self.glue.attack, glue);
        visitor(&self.glue.release, glue);
        visitor(&self.glue.knee, glue);
        visitor(&self.glue.makeup, glue);
        let limiter = Some(Effect::Limiter);
        visitor(&self.limiter.ceiling, limiter);
        visitor(&self.limiter.release, limiter);
    }

    /// The parameters as JSON, tagged as a part's are.
    pub(crate) fn tagged_json(&self) -> String {
        let mut json = TaggedJson::new();
        self.visit_tagged(&mut |param, effect| json.push(param, effect));
        json.finish()
    }
}

impl ParamSet for MasterParams {
    fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        self.visit_tagged(&mut |param: &'a dyn Param, _effect: Option<Effect>| visitor(param));
    }
}

/// The effect of a part whose parameter `id` is, if it is one.
pub(crate) fn part_effect_of(id: ParamId) -> Option<Effect> {
    Some(match id {
        LOWPASS_ON | LOWPASS_CUTOFF | LOWPASS_RESONANCE => Effect::Lowpass,
        REVERB_ON | REVERB_MIX | REVERB_DECAY => Effect::Reverb,
        HIGHPASS_ON | HIGHPASS_CUTOFF | HIGHPASS_RESONANCE => Effect::Highpass,
        EQ_ON | EQ_LOW_GAIN | EQ_MID_GAIN | EQ_HIGH_GAIN | EQ_LOW_FREQ | EQ_MID_FREQ | EQ_MID_Q
        | EQ_HIGH_FREQ => Effect::Eq,
        DRIVE_ON | DRIVE_AMOUNT | DRIVE_OUTPUT | DRIVE_MIX | DRIVE_SHAPE => Effect::Drive,
        BITCRUSH_ON | BITCRUSH_BITS | BITCRUSH_RATE | BITCRUSH_MIX => Effect::Bitcrush,
        COMPRESSOR_ON | COMPRESSOR_THRESHOLD | COMPRESSOR_RATIO | COMPRESSOR_ATTACK
        | COMPRESSOR_RELEASE | COMPRESSOR_KNEE | COMPRESSOR_MAKEUP | COMPRESSOR_MIX => {
            Effect::Compressor
        }
        CHORUS_ON | CHORUS_MIX | CHORUS_RATE | CHORUS_DEPTH | CHORUS_DELAY | CHORUS_VOICES
        | CHORUS_SPREAD => Effect::Chorus,
        FLANGER_ON | FLANGER_MIX | FLANGER_RATE | FLANGER_DEPTH | FLANGER_FEEDBACK
        | FLANGER_DELAY => Effect::Flanger,
        PHASER_ON | PHASER_MIX | PHASER_RATE | PHASER_DEPTH | PHASER_CENTER | PHASER_FEEDBACK
        | PHASER_STAGES | PHASER_SPREAD => Effect::Phaser,
        TREMOLO_ON | TREMOLO_RATE | TREMOLO_DEPTH | TREMOLO_SHAPE | TREMOLO_SYNC => Effect::Tremolo,
        AUTOPAN_ON | AUTOPAN_RATE | AUTOPAN_DEPTH | AUTOPAN_SHAPE | AUTOPAN_SYNC => Effect::Autopan,
        DELAY_ON | DELAY_TIME | DELAY_FEEDBACK | DELAY_MIX | DELAY_DAMPING | DELAY_PINGPONG
        | DELAY_SYNC => Effect::Delay,
        _ => return None,
    })
}

/// Whether `id` is a parameter of the master bus.
pub(crate) fn is_master(id: ParamId) -> bool {
    matches!(
        id,
        MASTER_TEMPO
            | MASTER_BEAT
            | MASTER_GAIN
            | GLUE_ON
            | GLUE_THRESHOLD
            | GLUE_RATIO
            | GLUE_ATTACK
            | GLUE_RELEASE
            | GLUE_KNEE
            | GLUE_MAKEUP
            | MASTER_REVERB_LEVEL
            | MASTER_REVERB_DECAY
            | MASTER_REVERB_SIZE
            | MASTER_REVERB_DAMPING
            | LIMITER_CEILING
            | LIMITER_RELEASE
    )
}
