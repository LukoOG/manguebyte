//! The preset library: plain data, listed by category.
//!
//! Every preset is a [`PolySynthParams`] built from [`PolySynthParams::INIT`]
//! with what makes it itself changed, so that each reads as a recipe.

use super::params::{
    EnvParams, FilterModel as Model, FilterParams, FilterRouting, LfoParams, ModDestination as Dst,
    ModSlot, ModSource as Src, NoiseParams, OscParams, OscSource, PolySynthParams, SubParams,
    SubWave, TableShape as Table, VoiceMode, Wave, matrix,
};
use super::{Category, Preset};
use crate::{LfoShape as Lfo, NoiseColor};

const BASE: PolySynthParams = PolySynthParams::INIT;
const OFF: OscParams = OscParams::OFF;
const NO_FILTER: FilterParams = FilterParams::OFF;

const fn env(attack: f64, decay: f64, sustain: f64, release: f64) -> EnvParams {
    EnvParams::adsr(attack, decay, sustain, release)
}

/// An envelope of straight lines.
const fn linear(envelope: EnvParams) -> EnvParams {
    EnvParams {
        attack_curve: 0.0,
        decay_curve: 0.0,
        release_curve: 0.0,
        ..envelope
    }
}

/// The amplifier's and the filters' envelopes, the other two as they are.
const fn envs(amp: EnvParams, filter: EnvParams) -> [EnvParams; 4] {
    [amp, filter, BASE.envelopes[2], BASE.envelopes[3]]
}

/// Three envelopes, the fourth as it is.
const fn envs3(amp: EnvParams, filter: EnvParams, third: EnvParams) -> [EnvParams; 4] {
    [amp, filter, third, BASE.envelopes[3]]
}

const fn lfos(first: LfoParams, second: LfoParams, third: LfoParams) -> [LfoParams; 3] {
    [first, second, third]
}

/// LFO 1, the other two as they are.
const fn lfo1(first: LfoParams) -> [LfoParams; 3] {
    [first, BASE.lfos[1], BASE.lfos[2]]
}

const fn lfo(shape: Lfo, rate: f64) -> LfoParams {
    LfoParams::new(shape, rate)
}

const fn beats(shape: Lfo, beats: f64) -> LfoParams {
    LfoParams::synced(shape, beats)
}

const fn osc(wave: Wave, level: f64) -> OscParams {
    OscParams::new(wave, level)
}

const fn table(shape: Table, position: f64, level: f64) -> OscParams {
    OscParams::table(shape, position, level)
}

const fn filter(model: Model, cutoff: f64, resonance: f64) -> FilterParams {
    FilterParams::new(model, cutoff, resonance)
}

const fn slot(source: Src, destination: Dst, amount: f64) -> ModSlot {
    ModSlot::new(source, destination, amount)
}

const fn sub(wave: SubWave, octaves: u8, level: f64) -> SubParams {
    SubParams {
        wave,
        octaves,
        level,
    }
}

const fn noise(color: NoiseColor, level: f64) -> NoiseParams {
    NoiseParams { color, level }
}

const fn preset(
    name: &'static str,
    category: Category,
    description: &'static str,
    params: PolySynthParams,
) -> Preset {
    Preset {
        name,
        category,
        description,
        params,
    }
}

/// The ratio of an FM modulator, as the semitones that give it.
const RATIO_3_5: f64 = 21.688_259_064_691_25;
const RATIO_4: f64 = 24.0;
const RATIO_6: f64 = 31.019_550_008_653_873;
const RATIO_14: f64 = 45.688_259_064_691_25;

/// Every preset, by category.
pub static PRESETS: [Preset; 68] = [
    // Basses.
    preset(
        "Moog Bass",
        Category::Bass,
        "Two saws and a square an octave down through a ladder that snaps shut: round and punchy.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0),
                osc(Wave::Saw, 0.8).tuned(0.0, 7.0),
                osc(Wave::Pulse, 0.6).tuned(-12.0, 0.0),
            ],
            filters: [
                filter(Model::LadderLp24, 180.0, 0.35).env(4.0).track(0.4),
                NO_FILTER,
            ],
            envelopes: envs(env(0.002, 0.4, 0.8, 0.08), env(0.001, 0.25, 0.1, 0.1)),
            matrix: matrix(&[slot(Src::Velocity, Dst::Cutoff, 1.0)]),
            mode: VoiceMode::Mono,
            level: 0.20,
            ..BASE
        },
    ),
    preset(
        "Reese",
        Category::Bass,
        "Two pairs of saws detuned against each other with a sine sub: the slow, phasing drum-and-bass growl.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(2, 0.3, 0.3),
                osc(Wave::Saw, 1.0).tuned(0.0, -14.0).unison(2, 0.3, 0.3),
                OFF,
            ],
            sub: sub(SubWave::Sine, 1, 0.5),
            filters: [
                filter(Model::LadderLp24, 1_100.0, 0.2).track(0.3),
                NO_FILTER,
            ],
            envelopes: envs(env(0.005, 0.3, 1.0, 0.15), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 0.25)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Cutoff, 0.6)]),
            mode: VoiceMode::Mono,
            glide: 0.05,
            saturation: 0.2,
            level: 0.33,
            ..BASE
        },
    ),
    preset(
        "Wobble Bass",
        Category::Bass,
        "Saw and square through a driven MS-20 filter that an LFO opens in eighths of the tempo: dubstep, ready to wobble.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0),
                osc(Wave::Pulse, 0.8).tuned(-12.0, 0.0).shaped(0.2),
                OFF,
            ],
            sub: sub(SubWave::Sine, 1, 0.6),
            filters: [
                filter(Model::Ms20Lp, 250.0, 0.6).driven(9.0).track(0.3),
                NO_FILTER,
            ],
            envelopes: envs(env(0.003, 0.2, 1.0, 0.1), BASE.envelopes[1]),
            lfos: lfo1(beats(Lfo::Sine, 0.5)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Cutoff, 3.5)]),
            mode: VoiceMode::Mono,
            glide: 0.04,
            saturation: 0.2,
            level: 0.30,
            ..BASE
        },
    ),
    preset(
        "Deep Sub",
        Category::Bass,
        "A sine with a touch of triangle an octave up and a quick pitch punch: weight you feel more than hear.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 1.0),
                osc(Wave::Triangle, 0.25).tuned(12.0, 0.0),
                OFF,
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.004, 0.3, 1.0, 0.12),
                BASE.envelopes[1],
                env(0.0, 0.03, 0.0, 0.05),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Pitch, 2.0)]),
            mode: VoiceMode::Mono,
            glide: 0.02,
            saturation: 0.25,
            level: 0.38,
            ..BASE
        },
    ),
    preset(
        "FM Growl",
        Category::Bass,
        "A sine frequency-modulated by another at its own pitch, the index falling after the attack and wobbling in sixteenths.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 0.0),
                osc(Wave::Sine, 1.0).fm_by(OscSource::Osc1, 2.5),
                OFF,
            ],
            sub: sub(SubWave::Sine, 1, 0.5),
            filters: [filter(Model::SvfLp, 3_000.0, 0.2).track(0.5), NO_FILTER],
            envelopes: envs3(
                env(0.002, 0.3, 1.0, 0.1),
                BASE.envelopes[1],
                env(0.0, 0.35, 0.3, 0.2),
            ),
            lfos: lfo1(beats(Lfo::Triangle, 0.25)),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Osc2Fm, 3.0),
                slot(Src::Lfo1, Dst::Osc2Fm, 1.5),
            ]),
            mode: VoiceMode::Mono,
            level: 0.30,
            ..BASE
        },
    ),
    preset(
        "House Organ Bass",
        Category::Bass,
        "A drawbar organ with its fundamental reinforced, a percussive decay and a key click: the bass of 90s house.",
        PolySynthParams {
            oscillators: [table(Table::Organ, 0.4, 1.0), osc(Wave::Sine, 0.4), OFF],
            noise: noise(NoiseColor::White, 0.0),
            filters: [filter(Model::SvfLp, 2_500.0, 0.1).track(0.5), NO_FILTER],
            envelopes: envs3(
                env(0.002, 0.25, 0.55, 0.08),
                BASE.envelopes[1],
                env(0.0, 0.01, 0.0, 0.01),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::NoiseLevel, 0.15)]),
            level: 0.39,
            ..BASE
        },
    ),
    preset(
        "Acid Line",
        Category::Bass,
        "A saw through a resonant ladder its envelope opens and closes on every note; accents open it further, overlapping notes slide.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 1.0), OFF, OFF],
            filters: [
                filter(Model::LadderLp24, 350.0, 0.8).env(3.5).driven(6.0),
                NO_FILTER,
            ],
            envelopes: envs(env(0.002, 0.2, 0.8, 0.05), env(0.001, 0.25, 0.0, 0.1)),
            matrix: matrix(&[
                slot(Src::Accent, Dst::Cutoff, 1.2),
                slot(Src::Accent, Dst::Resonance, 0.1),
            ]),
            mode: VoiceMode::Legato,
            glide: 0.06,
            saturation: 0.2,
            accent_db: 4.0,
            level: 0.35,
            ..BASE
        },
    ),
    preset(
        "Garage Square",
        Category::Bass,
        "A square and a square sub through a gently enveloped ladder: the hollow bass of UK garage.",
        PolySynthParams {
            oscillators: [osc(Wave::Pulse, 1.0), OFF, OFF],
            sub: sub(SubWave::Square, 1, 0.6),
            filters: [filter(Model::LadderLp24, 900.0, 0.2).env(1.5), NO_FILTER],
            envelopes: envs(env(0.002, 0.35, 0.6, 0.1), env(0.002, 0.3, 0.2, 0.1)),
            matrix: matrix(&[slot(Src::Velocity, Dst::Cutoff, 1.0)]),
            mode: VoiceMode::Mono,
            glide: 0.03,
            level: 0.31,
            ..BASE
        },
    ),
    preset(
        "Wavetable Growl",
        Category::Bass,
        "Folded and digital wavetables swept in eighths through a driven MS-20 filter: a modern, metallic growl.",
        PolySynthParams {
            oscillators: [
                table(Table::Fold, 0.3, 1.0).unison(3, 0.15, 0.3),
                table(Table::Digital, 0.4, 0.5).tuned(-12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::Ms20Lp, 1_200.0, 0.4).driven(12.0), NO_FILTER],
            envelopes: envs(env(0.003, 0.3, 1.0, 0.1), BASE.envelopes[1]),
            lfos: lfo1(beats(Lfo::SawDown, 0.5)),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Osc1Shape, 0.5),
                slot(Src::Lfo1, Dst::Cutoff, 1.5),
            ]),
            mode: VoiceMode::Mono,
            saturation: 0.35,
            level: 0.35,
            ..BASE
        },
    ),
    preset(
        "Synthpop Bass",
        Category::Bass,
        "Two slowly pulse-width-modulated squares through a ladder with a short envelope: the 80s sequencer bass.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Pulse, 1.0).shaped(0.3),
                osc(Wave::Pulse, 0.8).tuned(0.0, 6.0).shaped(0.3),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 500.0, 0.25).env(3.0), NO_FILTER],
            envelopes: envs(env(0.002, 0.3, 0.7, 0.12), env(0.001, 0.3, 0.1, 0.1)),
            lfos: lfos(BASE.lfos[0], lfo(Lfo::Triangle, 0.6), BASE.lfos[2]),
            matrix: matrix(&[
                slot(Src::Lfo2, Dst::Osc1Shape, 0.2),
                slot(Src::Lfo2, Dst::Osc2Shape, -0.25),
            ]),
            level: 0.24,
            ..BASE
        },
    ),
    preset(
        "Funk Bass",
        Category::Bass,
        "Saw and pulse through an SEM-like filter set between low-pass and band-pass, with a quick, velocity-hungry envelope.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 1.0), osc(Wave::Pulse, 0.6), OFF],
            filters: [
                filter(Model::SemBand, 600.0, 0.5).env(3.0).morphed(0.3),
                NO_FILTER,
            ],
            envelopes: envs(env(0.002, 0.25, 0.3, 0.08), env(0.001, 0.15, 0.0, 0.1)),
            matrix: matrix(&[slot(Src::Velocity, Dst::Cutoff, 1.5)]),
            mode: VoiceMode::Mono,
            velocity: 0.7,
            level: 1.07,
            ..BASE
        },
    ),
    preset(
        "808 Glide",
        Category::Bass,
        "A saturated sine with a falling pitch at the start and a long decay that slides between overlapping notes: the trap 808.",
        PolySynthParams {
            oscillators: [osc(Wave::Sine, 1.0), OFF, OFF],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.002, 1.5, 0.4, 0.3),
                BASE.envelopes[1],
                env(0.0, 0.06, 0.0, 0.1),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Pitch, 7.0)]),
            mode: VoiceMode::Legato,
            glide: 0.12,
            saturation: 0.45,
            level: 0.44,
            ..BASE
        },
    ),
    preset(
        "Rolling Bass",
        Category::Bass,
        "Three detuned saws and one an octave down, a short filter blip on every note: the off-beat trance bass.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(3, 0.15, 0.2),
                osc(Wave::Saw, 0.5).tuned(-12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 400.0, 0.25).env(3.5), NO_FILTER],
            envelopes: envs(env(0.001, 0.15, 0.4, 0.05), env(0.001, 0.12, 0.05, 0.08)),
            level: 0.38,
            ..BASE
        },
    ),
    preset(
        "Rubber Bass",
        Category::Bass,
        "A triangle and a sine sub through a resonant 12 dB ladder, gliding: round and bouncy.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Triangle, 1.0),
                osc(Wave::Sine, 0.7).tuned(-12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::LadderLp12, 300.0, 0.5).env(2.5), NO_FILTER],
            envelopes: envs(env(0.003, 0.4, 0.7, 0.1), env(0.001, 0.2, 0.0, 0.1)),
            mode: VoiceMode::Mono,
            glide: 0.08,
            level: 0.46,
            ..BASE
        },
    ),
    preset(
        "Talking Bass",
        Category::Bass,
        "Detuned saws and a square through a driven vowel filter that an LFO moves from vowel to vowel every beat: a bass that says \"yoi\".",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(4, 0.2, 0.3),
                osc(Wave::Pulse, 0.6).tuned(-12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::Formant, 900.0, 0.6).driven(9.0), NO_FILTER],
            envelopes: envs(env(0.003, 0.3, 1.0, 0.1), BASE.envelopes[1]),
            lfos: lfo1(beats(Lfo::Triangle, 1.0)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Morph, 0.5)]),
            mode: VoiceMode::Mono,
            saturation: 0.4,
            level: 0.51,
            ..BASE
        },
    ),
    // Leads.
    preset(
        "Trance Supersaw",
        Category::Lead,
        "Seven detuned saws spread wide and seven more an octave up: the classic trance lead; the wheel adds vibrato.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(7, 0.45, 1.0),
                osc(Wave::Saw, 0.4).tuned(12.0, 0.0).unison(7, 0.3, 1.0),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 9_000.0, 0.1).env(1.0), NO_FILTER],
            envelopes: envs(env(0.005, 0.3, 0.9, 0.35), env(0.002, 0.5, 0.4, 0.4)),
            lfos: lfo1(lfo(Lfo::Sine, 5.5)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Pitch, 0.12).via(Src::ModWheel)]),
            level: 0.33,
            ..BASE
        },
    ),
    preset(
        "Square Lead",
        Category::Lead,
        "A square with a softer one an octave up, slowly pulse-width-modulated, with a vibrato that comes in late; it slurs.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Pulse, 1.0),
                osc(Wave::Pulse, 0.5).tuned(12.0, 0.0).shaped(0.3),
                OFF,
            ],
            filters: [
                filter(Model::LadderLp24, 3_500.0, 0.2).env(1.0).track(0.5),
                NO_FILTER,
            ],
            envelopes: envs(env(0.004, 0.2, 0.9, 0.12), env(0.002, 0.4, 0.4, 0.2)),
            lfos: lfos(
                lfo(Lfo::Sine, 5.5).fading_in(0.4),
                lfo(Lfo::Triangle, 0.4),
                BASE.lfos[2],
            ),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Pitch, 0.2),
                slot(Src::Lfo2, Dst::Osc1Shape, 0.25),
            ]),
            mode: VoiceMode::Legato,
            glide: 0.06,
            level: 0.29,
            ..BASE
        },
    ),
    preset(
        "Sync Lead",
        Category::Lead,
        "A saw hard-synced to another, its pitch swept down by an envelope on every note: the tearing sync lead.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 0.35),
                osc(Wave::Saw, 1.0).tuned(12.0, 0.0).synced(),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 7_000.0, 0.15), NO_FILTER],
            envelopes: envs3(
                env(0.003, 0.3, 0.9, 0.15),
                BASE.envelopes[1],
                env(0.0, 0.5, 0.25, 0.3),
            ),
            lfos: lfo1(lfo(Lfo::Sine, 5.0).fading_in(0.3)),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Osc2Pitch, 19.0),
                slot(Src::Lfo1, Dst::Pitch, 0.1),
            ]),
            mode: VoiceMode::Legato,
            glide: 0.04,
            level: 0.48,
            ..BASE
        },
    ),
    preset(
        "Chiptune",
        Category::Lead,
        "A bare 25% pulse with a blip of pitch at the start and a delayed vibrato: the sound of the game consoles.",
        PolySynthParams {
            oscillators: [osc(Wave::Pulse, 1.0).shaped(0.5), OFF, OFF],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                linear(env(0.002, 0.1, 0.85, 0.05)),
                BASE.envelopes[1],
                linear(env(0.0, 0.04, 0.0, 0.0)),
            ),
            lfos: lfo1(lfo(Lfo::Triangle, 7.0).fading_in(0.25)),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Pitch, 12.0),
                slot(Src::Lfo1, Dst::Pitch, 0.3),
            ]),
            mode: VoiceMode::Mono,
            velocity: 0.0,
            level: 0.22,
            ..BASE
        },
    ),
    preset(
        "Hoover",
        Category::Lead,
        "Wide detuned saws and pulses an octave down that bend up into every note and slur: the rave hoover.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(7, 0.7, 0.8),
                osc(Wave::Pulse, 0.7)
                    .tuned(-12.0, 0.0)
                    .shaped(0.35)
                    .unison(4, 0.5, 0.8),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 5_000.0, 0.2), NO_FILTER],
            envelopes: envs3(
                env(0.01, 0.3, 0.9, 0.3),
                BASE.envelopes[1],
                env(0.0, 0.12, 0.0, 0.2),
            ),
            lfos: lfos(BASE.lfos[0], lfo(Lfo::Sine, 0.3), BASE.lfos[2]),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Pitch, -3.0),
                slot(Src::Lfo2, Dst::Osc2Shape, 0.3),
            ]),
            mode: VoiceMode::Legato,
            glide: 0.15,
            level: 0.24,
            ..BASE
        },
    ),
    preset(
        "Mono Saw Lead",
        Category::Lead,
        "Three saws, one an octave down, through a ladder that pressure opens: the fat monophonic lead that glides.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0),
                osc(Wave::Saw, 1.0).tuned(0.0, 9.0),
                osc(Wave::Saw, 0.6).tuned(-12.0, 0.0),
            ],
            filters: [
                filter(Model::LadderLp24, 2_500.0, 0.3).env(2.0).track(0.5),
                NO_FILTER,
            ],
            envelopes: envs(env(0.004, 0.3, 0.95, 0.2), env(0.005, 0.4, 0.5, 0.3)),
            lfos: lfo1(lfo(Lfo::Sine, 5.2)),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Pitch, 0.15).via(Src::ModWheel),
                slot(Src::Aftertouch, Dst::Cutoff, 1.0),
            ]),
            mode: VoiceMode::Legato,
            glide: 0.07,
            level: 0.23,
            ..BASE
        },
    ),
    preset(
        "Whistle Lead",
        Category::Lead,
        "A sine with a faint triangle an octave up, a slow attack and a late vibrato: soft, flute-like, gliding.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 1.0),
                osc(Wave::Triangle, 0.3).tuned(12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::SvfLp, 3_000.0, 0.1), NO_FILTER],
            envelopes: envs(env(0.03, 0.2, 0.9, 0.25), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 5.0).fading_in(0.5)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Pitch, 0.18)]),
            mode: VoiceMode::Legato,
            glide: 0.09,
            level: 0.37,
            ..BASE
        },
    ),
    preset(
        "Digital Lead",
        Category::Lead,
        "A stepped digital wavetable slowly scanned, with a spectral one a fifth up: glassy and modern.",
        PolySynthParams {
            oscillators: [
                table(Table::Digital, 0.6, 1.0).unison(3, 0.2, 0.5),
                table(Table::Spectral, 0.3, 0.5).tuned(7.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::SvfLp, 6_000.0, 0.2), NO_FILTER],
            envelopes: envs(env(0.003, 0.3, 0.85, 0.2), BASE.envelopes[1]),
            lfos: lfos(BASE.lfos[0], lfo(Lfo::Triangle, 0.2), BASE.lfos[2]),
            matrix: matrix(&[slot(Src::Lfo2, Dst::Osc1Shape, 0.3)]),
            level: 0.28,
            ..BASE
        },
    ),
    preset(
        "Talking Lead",
        Category::Lead,
        "Two saws through the vowel filter, which an LFO moves in eighths: a lead that talks.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 1.0).unison(2, 0.2, 0.3), OFF, OFF],
            filters: [
                filter(Model::Formant, 1_000.0, 0.6).morphed(0.25),
                NO_FILTER,
            ],
            envelopes: envs(env(0.01, 0.2, 0.9, 0.2), BASE.envelopes[1]),
            lfos: lfo1(beats(Lfo::Triangle, 0.5)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Morph, 0.5)]),
            mode: VoiceMode::Legato,
            glide: 0.05,
            level: 0.31,
            ..BASE
        },
    ),
    preset(
        "Screamer",
        Category::Lead,
        "Saw and pulse through an MS-20 high-pass and a driven, resonant MS-20 low-pass in a row: aggressive and loud.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0),
                osc(Wave::Pulse, 0.6).tuned(12.0, 0.0),
                OFF,
            ],
            filters: [
                filter(Model::Ms20Hp, 300.0, 0.5),
                filter(Model::Ms20Lp, 2_500.0, 0.75).driven(15.0).env(2.0),
            ],
            envelopes: envs(env(0.003, 0.3, 0.9, 0.2), env(0.003, 0.4, 0.3, 0.3)),
            mode: VoiceMode::Legato,
            glide: 0.05,
            saturation: 0.3,
            level: 0.42,
            ..BASE
        },
    ),
    preset(
        "Future Lead",
        Category::Lead,
        "Detuned saws and squares spread wide with a hint of pitch at the attack: the bright future-bass lead.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(5, 0.35, 0.9),
                osc(Wave::Pulse, 0.6).unison(3, 0.25, 0.6),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 4_000.0, 0.15).env(1.5), NO_FILTER],
            envelopes: envs3(
                env(0.002, 0.4, 0.7, 0.25),
                env(0.002, 0.3, 0.3, 0.3),
                env(0.0, 0.08, 0.0, 0.1),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Pitch, 0.5)]),
            level: 0.35,
            ..BASE
        },
    ),
    // Pads.
    preset(
        "Warm Analog Pad",
        Category::Pad,
        "Two sets of detuned saws through a slowly opening ladder that breathes: the warm polysynth pad.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(4, 0.3, 0.8),
                osc(Wave::Saw, 1.0).tuned(0.0, 5.0).unison(4, 0.35, 0.8),
                OFF,
            ],
            filters: [
                filter(Model::LadderLp24, 1_200.0, 0.15).env(1.2).track(0.3),
                NO_FILTER,
            ],
            envelopes: envs(env(0.6, 1.0, 0.9, 1.6), env(0.8, 1.5, 0.6, 1.5)),
            lfos: lfo1(lfo(Lfo::Sine, 0.2)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Cutoff, 0.4)]),
            level: 0.31,
            ..BASE
        },
    ),
    preset(
        "String Machine",
        Category::Pad,
        "Detuned saws and an octave above, their detune shimmering slowly, through a gentle 12 dB filter: the string ensemble.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(6, 0.35, 0.9),
                osc(Wave::Saw, 0.45).tuned(12.0, 0.0).unison(4, 0.3, 0.9),
                OFF,
            ],
            filters: [
                filter(Model::LadderLp12, 3_200.0, 0.05),
                filter(Model::SvfHp, 150.0, 0.0),
            ],
            envelopes: envs(env(0.25, 0.5, 0.95, 0.9), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Triangle, 0.7)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Detune, 0.15)]),
            level: 0.29,
            ..BASE
        },
    ),
    preset(
        "Glass Pad",
        Category::Pad,
        "The glass wavetable, slowly scanned and spread wide, with a sine an octave up: crystalline and airy.",
        PolySynthParams {
            oscillators: [
                table(Table::Glass, 0.55, 1.0).unison(5, 0.2, 1.0),
                osc(Wave::Sine, 0.3).tuned(12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::SvfHp, 200.0, 0.0), NO_FILTER],
            envelopes: envs(env(0.9, 1.0, 0.85, 2.0), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 0.15)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Osc1Shape, 0.35)]),
            level: 0.32,
            ..BASE
        },
    ),
    preset(
        "Dark Drone",
        Category::Pad,
        "Low detuned saws and a fifth, a sub and brown noise through a dark, driven ladder that moves very slowly.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).tuned(-12.0, 0.0).unison(3, 0.4, 0.7),
                osc(Wave::Saw, 0.8).tuned(7.0, 0.0).unison(3, 0.4, 0.7),
                OFF,
            ],
            sub: sub(SubWave::Sine, 1, 0.5),
            noise: noise(NoiseColor::Brown, 0.15),
            filters: [
                filter(Model::LadderLp24, 350.0, 0.45).driven(6.0),
                NO_FILTER,
            ],
            envelopes: envs(env(2.0, 1.0, 1.0, 3.0), BASE.envelopes[1]),
            lfos: lfos(lfo(Lfo::Sine, 0.07), lfo(Lfo::Triangle, 0.11), BASE.lfos[2]),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Cutoff, 1.2),
                slot(Src::Lfo2, Dst::Detune, 0.2),
            ]),
            level: 0.35,
            ..BASE
        },
    ),
    preset(
        "Choir Pad",
        Category::Pad,
        "The choir wavetable drifting between oo and ah, spread like an ensemble, with a faint vibrato: voices.",
        PolySynthParams {
            oscillators: [
                table(Table::Choir, 0.3, 1.0).unison(5, 0.25, 1.0),
                table(Table::Vowel, 0.6, 0.3).tuned(12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::SvfLp, 4_000.0, 0.1), NO_FILTER],
            envelopes: envs(env(0.7, 1.0, 0.9, 1.8), BASE.envelopes[1]),
            lfos: lfos(lfo(Lfo::Sine, 0.1), lfo(Lfo::Sine, 5.0), BASE.lfos[2]),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Osc1Shape, 0.4),
                slot(Src::Lfo2, Dst::Pitch, 0.05),
            ]),
            level: 0.39,
            ..BASE
        },
    ),
    preset(
        "Formant Pad",
        Category::Pad,
        "Saws through the vowel filter, which an envelope and a slow LFO carry from vowel to vowel: a choir-like formant pad.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 1.0).unison(6, 0.3, 1.0), OFF, OFF],
            filters: [filter(Model::Formant, 1_000.0, 0.5).morphed(0.1), NO_FILTER],
            envelopes: envs3(
                env(0.5, 1.0, 0.9, 1.5),
                BASE.envelopes[1],
                env(1.5, 2.0, 0.5, 2.0),
            ),
            lfos: lfo1(lfo(Lfo::Triangle, 0.12)),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Morph, 0.5),
                slot(Src::Lfo1, Dst::Morph, 0.2),
            ]),
            level: 0.59,
            ..BASE
        },
    ),
    preset(
        "PWM Pad",
        Category::Pad,
        "Two sets of pulses whose widths two LFOs move against each other: the lush, chorused pad of the 80s.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Pulse, 1.0).shaped(0.3).unison(3, 0.25, 0.8),
                osc(Wave::Pulse, 1.0)
                    .shaped(0.3)
                    .tuned(0.0, -6.0)
                    .unison(3, 0.3, 0.8),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 2_200.0, 0.1), NO_FILTER],
            envelopes: envs(env(0.4, 0.8, 0.9, 1.2), BASE.envelopes[1]),
            lfos: lfos(
                lfo(Lfo::Triangle, 0.5),
                lfo(Lfo::Triangle, 0.37),
                BASE.lfos[2],
            ),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Osc1Shape, 0.3),
                slot(Src::Lfo2, Dst::Osc2Shape, 0.3),
            ]),
            level: 0.27,
            ..BASE
        },
    ),
    preset(
        "Shimmer Pad",
        Category::Pad,
        "Saws, a harmonic wavetable an octave up and a sine a twelfth up, high-passed: bright and floating.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 0.8).unison(4, 0.3, 1.0),
                table(Table::Harmonics, 0.7, 0.5)
                    .tuned(12.0, 0.0)
                    .unison(3, 0.2, 1.0),
                osc(Wave::Sine, 0.25).tuned(19.0, 0.0),
            ],
            filters: [
                filter(Model::SvfHp, 400.0, 0.0),
                filter(Model::LadderLp12, 6_000.0, 0.0),
            ],
            envelopes: envs(env(0.5, 1.0, 0.9, 2.2), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 0.3)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Osc2Shape, 0.25)]),
            level: 0.38,
            ..BASE
        },
    ),
    preset(
        "Evolving Spectra",
        Category::Pad,
        "Spectral and resonant wavetables scanned by two slow LFOs through an SEM-like filter that opens over seconds.",
        PolySynthParams {
            oscillators: [
                table(Table::Spectral, 0.2, 1.0).unison(4, 0.25, 1.0),
                table(Table::Resonant, 0.3, 0.5).unison(2, 0.2, 0.6),
                OFF,
            ],
            filters: [
                filter(Model::SemNotch, 1_500.0, 0.3).morphed(0.2),
                NO_FILTER,
            ],
            envelopes: envs3(
                env(1.5, 1.0, 1.0, 2.5),
                BASE.envelopes[1],
                env(3.0, 2.0, 0.5, 2.0),
            ),
            lfos: lfos(lfo(Lfo::Triangle, 0.05), lfo(Lfo::Sine, 0.08), BASE.lfos[2]),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Osc1Shape, 0.6),
                slot(Src::Lfo2, Dst::Osc2Shape, 0.5),
                slot(Src::Env3, Dst::Cutoff, 1.5),
            ]),
            level: 0.58,
            ..BASE
        },
    ),
    preset(
        "Soft Sine Pad",
        Category::Pad,
        "Detuned sines and triangles an octave up, with a slow vibrato: pure and gentle.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 1.0).unison(3, 0.2, 0.8),
                osc(Wave::Triangle, 0.35)
                    .tuned(12.0, 0.0)
                    .unison(2, 0.2, 0.8),
                OFF,
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs(env(0.6, 1.0, 0.9, 1.5), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 4.5)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Pitch, 0.04)]),
            level: 0.31,
            ..BASE
        },
    ),
    preset(
        "Comb Wash",
        Category::Pad,
        "Saws and pink noise through a comb tuned to the note, its damping drifting: a resonant, ambient wash.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 0.6).unison(3, 0.2, 1.0), OFF, OFF],
            noise: noise(NoiseColor::Pink, 0.35),
            filters: [
                filter(Model::CombPlus, 261.63, 0.8).track(1.0).morphed(0.5),
                filter(Model::SvfLp, 3_000.0, 0.0),
            ],
            envelopes: envs(env(1.2, 1.0, 0.9, 2.5), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 0.1)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Morph, 0.3)]),
            level: 0.19,
            ..BASE
        },
    ),
    preset(
        "Sweep Pad",
        Category::Pad,
        "Detuned saws and an octave down through a resonant ladder that a very slow LFO sweeps open and shut.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(5, 0.35, 1.0),
                osc(Wave::Saw, 0.5).tuned(-12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 500.0, 0.55).track(0.2), NO_FILTER],
            envelopes: envs(env(0.5, 1.0, 1.0, 1.5), BASE.envelopes[1]),
            lfos: lfo1(LfoParams {
                phase: 0.75,
                ..lfo(Lfo::Sine, 0.12)
            }),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Cutoff, 2.5)]),
            level: 0.77,
            ..BASE
        },
    ),
    // Plucks.
    preset(
        "House Pluck",
        Category::Pluck,
        "Detuned saws and a pulse with a filter that snaps shut: the short, bright pluck of house music.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(3, 0.2, 0.6),
                osc(Wave::Pulse, 0.5).tuned(0.0, 7.0),
                OFF,
            ],
            filters: [
                filter(Model::LadderLp24, 300.0, 0.25).env(5.0).track(0.5),
                NO_FILTER,
            ],
            envelopes: envs(env(0.001, 0.3, 0.0, 0.2), env(0.001, 0.18, 0.0, 0.15)),
            matrix: matrix(&[slot(Src::Velocity, Dst::Cutoff, 1.0)]),
            level: 0.36,
            ..BASE
        },
    ),
    preset(
        "Bell Pluck",
        Category::Pluck,
        "A sine frequency-modulated at three and a half times its pitch, the index dying with the note: a bright bell.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 0.0).tuned(RATIO_3_5, 0.0),
                osc(Wave::Sine, 1.0).fm_by(OscSource::Osc1, 1.0),
                osc(Wave::Sine, 0.3).tuned(12.0, 0.0),
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.001, 1.5, 0.0, 0.8),
                BASE.envelopes[1],
                env(0.0, 1.2, 0.0, 0.8),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Osc2Fm, 2.5)]),
            level: 0.36,
            ..BASE
        },
    ),
    preset(
        "Marimba",
        Category::Pluck,
        "A sine struck by a quick burst of FM at four times its pitch, dying fast: a wooden mallet.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 0.0).tuned(RATIO_4, 0.0),
                osc(Wave::Sine, 1.0).fm_by(OscSource::Osc1, 0.0),
                OFF,
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.001, 0.6, 0.0, 0.3),
                BASE.envelopes[1],
                env(0.0, 0.05, 0.0, 0.05),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Osc2Fm, 3.0)]),
            velocity: 0.7,
            level: 0.52,
            ..BASE
        },
    ),
    preset(
        "Plucked String",
        Category::Pluck,
        "A burst of noise into a comb tuned to the note, damped: a plucked string, by Karplus and Strong's idea.",
        PolySynthParams {
            oscillators: [OFF, OFF, OFF],
            noise: noise(NoiseColor::Pink, 0.0),
            filters: [
                filter(Model::CombPlus, 261.63, 0.97)
                    .track(1.0)
                    .morphed(0.7),
                filter(Model::SvfLp, 5_000.0, 0.0).track(0.3),
            ],
            envelopes: envs3(
                env(0.001, 2.0, 0.0, 0.5),
                BASE.envelopes[1],
                env(0.0, 0.02, 0.0, 0.01),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::NoiseLevel, 1.0)]),
            level: 4.0,
            ..BASE
        },
    ),
    preset(
        "Future Pluck",
        Category::Pluck,
        "Seven wide saws and an octave above with a closing filter, a pitch flick and some saturation: the future-bass pluck.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(7, 0.4, 1.0),
                osc(Wave::Saw, 0.4).tuned(12.0, 0.0).unison(3, 0.3, 1.0),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 600.0, 0.2).env(4.5), NO_FILTER],
            envelopes: envs3(
                env(0.001, 0.45, 0.0, 0.3),
                env(0.001, 0.25, 0.0, 0.2),
                env(0.0, 0.05, 0.0, 0.0),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Pitch, 0.4)]),
            saturation: 0.25,
            level: 0.48,
            ..BASE
        },
    ),
    preset(
        "Glass Pluck",
        Category::Pluck,
        "The glass wavetable with a sine two octaves up, decaying: a delicate, chiming pluck.",
        PolySynthParams {
            oscillators: [
                table(Table::Glass, 0.8, 1.0),
                osc(Wave::Sine, 0.25).tuned(24.0, 0.0),
                OFF,
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.001, 0.9, 0.0, 0.6),
                BASE.envelopes[1],
                env(0.0, 0.6, 0.0, 0.3),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Osc1Shape, -0.3)]),
            level: 0.41,
            ..BASE
        },
    ),
    preset(
        "Trance Pluck",
        Category::Pluck,
        "Five detuned saws through a resonant ladder with a short envelope: the arpeggiated trance pluck.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 1.0).unison(5, 0.3, 0.8), OFF, OFF],
            filters: [filter(Model::LadderLp24, 400.0, 0.35).env(4.0), NO_FILTER],
            envelopes: envs(env(0.001, 0.25, 0.0, 0.15), env(0.001, 0.15, 0.0, 0.1)),
            level: 0.59,
            ..BASE
        },
    ),
    preset(
        "Pizzicato",
        Category::Pluck,
        "A narrow pulse and a saw an octave down through an SEM-like band-pass that clicks open: plucked strings, synthetic.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Pulse, 1.0).shaped(0.6),
                osc(Wave::Saw, 0.5).tuned(-12.0, 0.0),
                OFF,
            ],
            filters: [
                filter(Model::SemBand, 900.0, 0.4)
                    .morphed(0.5)
                    .env(2.0)
                    .track(0.6),
                NO_FILTER,
            ],
            envelopes: envs(env(0.001, 0.2, 0.0, 0.12), env(0.001, 0.08, 0.0, 0.08)),
            level: 0.61,
            ..BASE
        },
    ),
    preset(
        "Kalimba",
        Category::Pluck,
        "A sine with a quick FM ping at six times its pitch and a quiet octave: a thumb piano.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 0.0).tuned(RATIO_6, 0.0),
                osc(Wave::Sine, 1.0).fm_by(OscSource::Osc1, 0.2),
                osc(Wave::Sine, 0.1).tuned(12.0, 0.0),
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.001, 1.0, 0.0, 0.5),
                BASE.envelopes[1],
                env(0.0, 0.08, 0.0, 0.05),
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Osc2Fm, 1.5)]),
            level: 0.44,
            ..BASE
        },
    ),
    // Keys.
    preset(
        "Polysynth Piano",
        Category::Keys,
        "Saw and triangle through a ladder that velocity opens, dying away like a struck string: the synth piano.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 0.8),
                osc(Wave::Triangle, 1.0).tuned(0.0, 4.0),
                OFF,
            ],
            filters: [
                filter(Model::LadderLp24, 900.0, 0.1).env(3.0).track(0.6),
                NO_FILTER,
            ],
            envelopes: envs(env(0.002, 1.8, 0.0, 0.35), env(0.002, 0.6, 0.15, 0.4)),
            matrix: matrix(&[slot(Src::Velocity, Dst::Cutoff, 2.0)]),
            velocity: 0.8,
            level: 0.39,
            ..BASE
        },
    ),
    preset(
        "Clavinet",
        Category::Keys,
        "A narrow pulse and a saw through a key-tracked ladder band-pass with a snappy envelope: funky and percussive.",
        PolySynthParams {
            oscillators: [osc(Wave::Pulse, 1.0).shaped(0.75), osc(Wave::Saw, 0.4), OFF],
            filters: [
                filter(Model::LadderBp12, 1_400.0, 0.4).env(1.5).track(0.7),
                NO_FILTER,
            ],
            envelopes: envs(env(0.001, 0.4, 0.25, 0.06), env(0.001, 0.12, 0.0, 0.05)),
            matrix: matrix(&[slot(Src::Velocity, Dst::Cutoff, 1.0)]),
            velocity: 0.7,
            level: 0.43,
            ..BASE
        },
    ),
    preset(
        "FM Electric Piano",
        Category::Keys,
        "A sine modulated at its own pitch that barks harder the harder it is struck, a faint tine and a tremolo: the FM electric piano.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 0.0),
                osc(Wave::Sine, 1.0).fm_by(OscSource::Osc1, 0.3),
                osc(Wave::Sine, 0.12).tuned(RATIO_14, 0.0),
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                env(0.001, 2.5, 0.0, 0.5),
                BASE.envelopes[1],
                env(0.0, 1.2, 0.2, 0.5),
            ),
            lfos: lfo1(lfo(Lfo::Sine, 4.5)),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Osc2Fm, 1.2),
                slot(Src::Velocity, Dst::Osc2Fm, 1.0),
                slot(Src::Lfo1, Dst::Volume, -1.5),
            ]),
            velocity: 0.7,
            level: 0.48,
            ..BASE
        },
    ),
    preset(
        "Drawbar Organ",
        Category::Keys,
        "A gospel registration of the organ wavetable with a key click and a light vibrato.",
        PolySynthParams {
            oscillators: [table(Table::Organ, 0.75, 1.0), OFF, OFF],
            noise: noise(NoiseColor::White, 0.0),
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs3(
                linear(env(0.002, 0.0, 1.0, 0.03)),
                BASE.envelopes[1],
                env(0.0, 0.008, 0.0, 0.0),
            ),
            lfos: lfo1(lfo(Lfo::Sine, 6.5)),
            matrix: matrix(&[
                slot(Src::Env3, Dst::NoiseLevel, 0.06),
                slot(Src::Lfo1, Dst::Pitch, 0.06),
            ]),
            velocity: 0.0,
            level: 0.35,
            ..BASE
        },
    ),
    preset(
        "Juno Keys",
        Category::Keys,
        "A saw and a pulse an octave down, slightly spread, through a ladder that settles after the attack: bright chord keys.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 0.8).unison(2, 0.15, 0.5),
                osc(Wave::Pulse, 0.7).tuned(-12.0, 0.0).shaped(0.25),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 2_000.0, 0.15).env(1.5), NO_FILTER],
            envelopes: envs(env(0.002, 1.0, 0.5, 0.4), env(0.002, 0.5, 0.4, 0.4)),
            level: 0.35,
            ..BASE
        },
    ),
    preset(
        "Wurly",
        Category::Keys,
        "A saturated triangle with an octave that velocity brings in, and a tremolo: a reedy electric piano.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Triangle, 1.0),
                osc(Wave::Sine, 0.2).tuned(12.0, 0.0),
                OFF,
            ],
            filters: [filter(Model::SvfLp, 2_500.0, 0.1), NO_FILTER],
            envelopes: envs(env(0.002, 1.5, 0.2, 0.3), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 5.5)),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Volume, -3.0),
                slot(Src::Velocity, Dst::Osc2Level, 0.4),
            ]),
            velocity: 0.8,
            saturation: 0.45,
            level: 0.48,
            ..BASE
        },
    ),
    preset(
        "Vibraphone",
        Category::Keys,
        "A sine and its fourth partial ringing a long time, with the motor's tremolo: metal bars.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Sine, 1.0),
                osc(Wave::Sine, 0.25).tuned(RATIO_4, 0.0),
                OFF,
            ],
            filters: [NO_FILTER, NO_FILTER],
            envelopes: envs(env(0.001, 2.5, 0.0, 1.0), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Sine, 5.0)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Volume, -4.0)]),
            level: 0.30,
            ..BASE
        },
    ),
    // Brass.
    preset(
        "Synth Brass",
        Category::Brass,
        "Two pairs of saws through a ladder whose envelope swells after the attack, with a little scoop of pitch: polysynth brass.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(2, 0.15, 0.3),
                osc(Wave::Saw, 0.9).tuned(0.0, -8.0).unison(2, 0.15, 0.3),
                OFF,
            ],
            filters: [
                filter(Model::LadderLp24, 700.0, 0.15).env(2.8).track(0.5),
                NO_FILTER,
            ],
            envelopes: envs3(
                env(0.03, 0.3, 0.9, 0.25),
                env(0.06, 0.6, 0.5, 0.3),
                env(0.0, 0.08, 0.0, 0.0),
            ),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Pitch, -0.5),
                slot(Src::Velocity, Dst::Cutoff, 1.0),
            ]),
            level: 0.26,
            ..BASE
        },
    ),
    preset(
        "Brass Stab",
        Category::Brass,
        "Wide saws and a square with a filter that bites and closes: the short brass stab of 90s house.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(5, 0.3, 0.8),
                osc(Wave::Pulse, 0.6),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 500.0, 0.2).env(4.0), NO_FILTER],
            envelopes: envs(env(0.002, 0.35, 0.15, 0.15), env(0.002, 0.2, 0.1, 0.15)),
            level: 0.32,
            ..BASE
        },
    ),
    preset(
        "Jump Brass",
        Category::Brass,
        "A saw and a narrow pulse through an SEM-like low-pass with a swelling envelope: the bright, brassy stab of the 80s.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(2, 0.15, 0.5),
                osc(Wave::Pulse, 0.8).shaped(0.15).tuned(0.0, 6.0),
                OFF,
            ],
            filters: [
                filter(Model::SemNotch, 900.0, 0.35).env(2.2).track(0.4),
                NO_FILTER,
            ],
            envelopes: envs(env(0.01, 0.3, 0.85, 0.3), env(0.02, 0.5, 0.45, 0.4)),
            level: 0.18,
            ..BASE
        },
    ),
    preset(
        "Rave Chord Stab",
        Category::Brass,
        "A minor chord of saws on every key with a snapping filter: the chord stab of old-school rave.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(2, 0.2, 0.4),
                osc(Wave::Saw, 0.9).tuned(3.0, 0.0).unison(2, 0.2, 0.4),
                osc(Wave::Saw, 0.9).tuned(7.0, 0.0).unison(2, 0.2, 0.4),
            ],
            filters: [filter(Model::LadderLp24, 800.0, 0.3).env(3.5), NO_FILTER],
            envelopes: envs(env(0.002, 0.4, 0.2, 0.2), env(0.002, 0.25, 0.2, 0.2)),
            level: 0.22,
            ..BASE
        },
    ),
    preset(
        "Soft Horns",
        Category::Brass,
        "A saw and a pulse through a 12 dB ladder that opens slowly, with a late vibrato: mellow horns.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 0.8), osc(Wave::Pulse, 0.6).shaped(0.4), OFF],
            filters: [filter(Model::LadderLp12, 900.0, 0.1).env(1.6), NO_FILTER],
            envelopes: envs(env(0.08, 0.3, 0.85, 0.35), env(0.12, 0.6, 0.6, 0.4)),
            lfos: lfo1(lfo(Lfo::Sine, 5.0).fading_in(0.6)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Pitch, 0.1)]),
            level: 0.72,
            ..BASE
        },
    ),
    preset(
        "Fanfare",
        Category::Brass,
        "Bright saws and an octave above through an opening ladder: a full, triumphant brass section.",
        PolySynthParams {
            oscillators: [
                osc(Wave::Saw, 1.0).unison(3, 0.2, 0.6),
                osc(Wave::Saw, 0.5).tuned(12.0, 0.0).unison(3, 0.2, 0.6),
                OFF,
            ],
            filters: [filter(Model::LadderLp24, 1_500.0, 0.15).env(2.5), NO_FILTER],
            envelopes: envs(env(0.02, 0.3, 0.9, 0.4), env(0.04, 0.4, 0.6, 0.4)),
            level: 0.31,
            ..BASE
        },
    ),
    // Effects.
    preset(
        "Riser",
        Category::Fx,
        "Wide saws and noise whose pitch climbs an octave and filter opens over five seconds: the build-up before the drop.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 0.6).unison(7, 0.6, 1.0), OFF, OFF],
            noise: noise(NoiseColor::White, 0.5),
            filters: [
                filter(Model::LadderLp24, 300.0, 0.5),
                filter(Model::SvfHp, 150.0, 0.0),
            ],
            envelopes: envs3(
                env(0.5, 0.0, 1.0, 1.5),
                BASE.envelopes[1],
                EnvParams {
                    attack_curve: -0.5,
                    ..env(5.0, 0.0, 1.0, 1.0)
                },
            ),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Pitch, 12.0),
                slot(Src::Env3, Dst::Cutoff, 6.0),
            ]),
            level: 0.40,
            ..BASE
        },
    ),
    preset(
        "Downlifter",
        Category::Fx,
        "Wide saws and pink noise falling an octave and closing over four seconds: the release after the peak.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 0.8).unison(5, 0.5, 1.0), OFF, OFF],
            noise: noise(NoiseColor::Pink, 0.4),
            filters: [filter(Model::LadderLp24, 400.0, 0.3), NO_FILTER],
            envelopes: envs3(
                env(0.01, 4.0, 0.0, 1.0),
                BASE.envelopes[1],
                env(0.0, 4.0, 0.0, 1.0),
            ),
            matrix: matrix(&[
                slot(Src::Env3, Dst::Pitch, 12.0),
                slot(Src::Env3, Dst::Cutoff, 5.0),
            ]),
            level: 0.44,
            ..BASE
        },
    ),
    preset(
        "Noise Sweep",
        Category::Fx,
        "White noise through a resonant band-pass that an LFO sweeps up and down: a whoosh.",
        PolySynthParams {
            oscillators: [OFF, OFF, OFF],
            noise: noise(NoiseColor::White, 1.0),
            filters: [filter(Model::SvfBp, 1_000.0, 0.6), NO_FILTER],
            envelopes: envs(env(0.3, 0.0, 1.0, 1.0), BASE.envelopes[1]),
            lfos: lfo1(LfoParams {
                phase: 0.75,
                ..lfo(Lfo::Sine, 0.2)
            }),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Cutoff, 2.5)]),
            level: 3.01,
            ..BASE
        },
    ),
    preset(
        "Laser Zap",
        Category::Fx,
        "A saw and a sine whose pitch falls three octaves in a flash: a laser.",
        PolySynthParams {
            oscillators: [osc(Wave::Saw, 0.7), osc(Wave::Sine, 1.0), OFF],
            filters: [filter(Model::SvfLp, 8_000.0, 0.5), NO_FILTER],
            envelopes: envs3(
                env(0.001, 0.3, 0.0, 0.1),
                BASE.envelopes[1],
                EnvParams {
                    decay_curve: 0.9,
                    ..env(0.0, 0.15, 0.0, 0.0)
                },
            ),
            matrix: matrix(&[slot(Src::Env3, Dst::Pitch, 36.0)]),
            level: 0.42,
            ..BASE
        },
    ),
    preset(
        "Random Bleeps",
        Category::Fx,
        "A pulse whose pitch and filter a sample-and-hold sets at random eight times a second: a computer thinking.",
        PolySynthParams {
            oscillators: [osc(Wave::Pulse, 1.0).shaped(0.3), OFF, OFF],
            filters: [filter(Model::LadderLp24, 2_000.0, 0.6), NO_FILTER],
            envelopes: envs(env(0.002, 0.1, 0.8, 0.2), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::SampleAndHold, 8.0)),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Pitch, 12.0),
                slot(Src::Lfo1, Dst::Cutoff, 1.5),
            ]),
            level: 0.33,
            ..BASE
        },
    ),
    preset(
        "Wind",
        Category::Fx,
        "Pink noise through two band-passes side by side, each drifting on its own slow LFO: wind.",
        PolySynthParams {
            oscillators: [OFF, OFF, OFF],
            noise: noise(NoiseColor::Pink, 1.0),
            filters: [
                filter(Model::SvfBp, 600.0, 0.7),
                filter(Model::SvfBp, 1_800.0, 0.7),
            ],
            routing: FilterRouting::Parallel,
            envelopes: envs(env(1.0, 0.0, 1.0, 2.0), BASE.envelopes[1]),
            lfos: lfos(lfo(Lfo::Sine, 0.13), lfo(Lfo::Sine, 0.21), BASE.lfos[2]),
            matrix: matrix(&[
                slot(Src::Lfo1, Dst::Cutoff1, 1.5),
                slot(Src::Lfo2, Dst::Cutoff2, 1.5),
            ]),
            level: 1.29,
            ..BASE
        },
    ),
    preset(
        "Impact Boom",
        Category::Fx,
        "A sine an octave down falling two octaves, with a burst of noise and saturation: a cinematic hit.",
        PolySynthParams {
            oscillators: [osc(Wave::Sine, 1.0).tuned(-12.0, 0.0), OFF, OFF],
            noise: noise(NoiseColor::White, 0.0),
            filters: [filter(Model::LadderLp24, 1_500.0, 0.1), NO_FILTER],
            envelopes: [
                env(0.001, 2.5, 0.0, 1.0),
                BASE.envelopes[1],
                env(0.0, 0.6, 0.0, 0.0),
                env(0.0, 0.15, 0.0, 0.0),
            ],
            matrix: matrix(&[
                slot(Src::Env3, Dst::Pitch, 24.0),
                slot(Src::Env4, Dst::NoiseLevel, 0.8),
            ]),
            saturation: 0.4,
            level: 0.47,
            ..BASE
        },
    ),
    preset(
        "Siren",
        Category::Fx,
        "A square whose pitch an LFO swings a fifth up and down: an alarm.",
        PolySynthParams {
            oscillators: [osc(Wave::Pulse, 1.0), OFF, OFF],
            filters: [filter(Model::SvfLp, 4_000.0, 0.1), NO_FILTER],
            envelopes: envs(env(0.05, 0.0, 1.0, 0.3), BASE.envelopes[1]),
            lfos: lfo1(lfo(Lfo::Triangle, 0.5)),
            matrix: matrix(&[slot(Src::Lfo1, Dst::Pitch, 7.0)]),
            level: 0.40,
            ..BASE
        },
    ),
];
