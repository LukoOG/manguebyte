//! The preset library.

use super::params::{
    EnvParams, FilterModel, FilterParams, ModDestination as To, ModSlot, ModSource as From,
    OscParams, PolySynthParams, Wave, matrix,
};
use super::{Category, Preset};

const BASE: PolySynthParams = PolySynthParams::INIT;

/// Every preset, by category.
pub static PRESETS: [Preset; 2] = [
    Preset {
        name: "Trance Supersaw",
        category: Category::Lead,
        description: "Seven detuned saws spread wide: the classic trance lead.",
        params: PolySynthParams {
            oscillators: [
                OscParams::new(Wave::Saw, 1.0).unison(7, 0.45, 1.0),
                OscParams::OFF,
                OscParams::OFF,
            ],
            filters: [
                FilterParams::new(FilterModel::LadderLp24, 9_000.0, 0.1).env(1.0),
                FilterParams::OFF,
            ],
            envelopes: [
                EnvParams::adsr(0.005, 0.3, 0.9, 0.35),
                EnvParams::adsr(0.002, 0.5, 0.4, 0.4),
                BASE.envelopes[2],
                BASE.envelopes[3],
            ],
            matrix: matrix(&[ModSlot::new(From::Lfo1, To::Pitch, 0.1).via(From::ModWheel)]),
            ..BASE
        },
    },
    Preset {
        name: "Moog Bass",
        category: Category::Bass,
        description: "Two saws and a square through a closing ladder: round and punchy.",
        params: PolySynthParams {
            oscillators: [
                OscParams::new(Wave::Saw, 1.0),
                OscParams::new(Wave::Saw, 0.8).tuned(0.0, 7.0),
                OscParams::new(Wave::Pulse, 0.6).tuned(-12.0, 0.0),
            ],
            filters: [
                FilterParams::new(FilterModel::LadderLp24, 180.0, 0.35)
                    .env(4.0)
                    .track(0.4),
                FilterParams::OFF,
            ],
            envelopes: [
                EnvParams::adsr(0.002, 0.4, 0.8, 0.08),
                EnvParams::adsr(0.001, 0.25, 0.1, 0.1),
                BASE.envelopes[2],
                BASE.envelopes[3],
            ],
            ..BASE
        },
    },
];
