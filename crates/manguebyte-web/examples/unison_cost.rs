//! Patches of the polyphonic synthesiser no preset of its library goes as
//! far as, for `voice_cost.mjs` to time in WebAssembly: a voice of sixteen
//! unison saws, of sixteen unison wavetables, and the heaviest patch of
//! `manguebyte-dsp`'s `poly_synth_presets` example (three oscillators of
//! sixteen unison voices through two driven filters, with saturation and
//! modulation). The presets go to seven unison voices an oscillator.
//!
//! Built as a library for `wasm32-unknown-unknown`:
//!
//! ```text
//! cargo build -p manguebyte-web --release --example unison_cost --target wasm32-unknown-unknown
//! ```
//!
//! it exports `unison_cost(patch, blocks)`, which holds one note of patch
//! 0 (the init patch, one saw through the ladder), 1, 2 or 3 (the three
//! above) and renders `blocks` blocks of 128 frames at 48 kHz, returning
//! the peak so that nothing is optimised away. The first call builds the
//! wavetables; time the ones after.

use manguebyte_core::AudioBuffer;
use manguebyte_dsp::event::NoteRef;
use manguebyte_dsp::instruments::PolySynthVoice;
use manguebyte_dsp::instruments::poly::{
    EnvParams, FilterModel, FilterParams, ModDestination, ModSlot, ModSource, OscParams,
    PolySynthParams, TableShape, Wave, matrix,
};
use manguebyte_dsp::{ProcessSpec, Processor};

fn unfiltered(osc: OscParams) -> PolySynthParams {
    PolySynthParams {
        oscillators: [osc, OscParams::OFF, OscParams::OFF],
        filters: [FilterParams::OFF, FilterParams::OFF],
        ..PolySynthParams::INIT
    }
}

fn heaviest() -> PolySynthParams {
    let stack = OscParams::new(Wave::Saw, 0.7).unison(16, 0.5, 1.0);
    let mut params = PolySynthParams {
        oscillators: [
            stack,
            OscParams::table(TableShape::Spectral, 0.4, 0.6).unison(16, 0.4, 1.0),
            stack.tuned(12.0, 0.0),
        ],
        filters: [
            FilterParams::new(FilterModel::LadderLp24, 2_000.0, 0.5).driven(6.0),
            FilterParams::new(FilterModel::Ms20Lp, 5_000.0, 0.3).driven(6.0),
        ],
        ..PolySynthParams::INIT
    };
    params.envelopes[1] = EnvParams::adsr(0.01, 1.0, 0.5, 0.5);
    params.saturation = 0.3;
    params.matrix = matrix(&[
        ModSlot::new(ModSource::Lfo1, ModDestination::Detune, 0.2),
        ModSlot::new(ModSource::Lfo2, ModDestination::Cutoff, 1.0),
        ModSlot::new(ModSource::Env3, ModDestination::Osc2Shape, 0.5),
    ]);
    params
}

/// Holds a note of `patch` and renders `blocks` blocks of 128 frames;
/// returns the peak, or -1 for a patch that does not exist.
// SAFETY: the exported name is unique in this example, which is a small
// library of its own.
#[unsafe(no_mangle)]
pub extern "C" fn unison_cost(patch: u32, blocks: u32) -> f32 {
    let params = match patch {
        0 => PolySynthParams::INIT,
        1 => unfiltered(OscParams::new(Wave::Saw, 1.0).unison(16, 0.5, 1.0)),
        2 => unfiltered(OscParams::table(TableShape::Spectral, 0.4, 1.0).unison(16, 0.5, 1.0)),
        3 => heaviest(),
        _ => return -1.0,
    };
    let mut synth = PolySynthVoice::synth::<f32>(&params, 1);
    synth.prepare(&ProcessSpec {
        sample_rate: 48_000.0,
        max_block_size: 128,
        num_channels: 2,
    });
    synth.note_on(NoteRef::new(0, 48), 0.8);
    let mut block = AudioBuffer::<f32>::new(2, 128);
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        synth.process(&mut block);
        peak = peak.max(block.peak());
    }
    peak
}
