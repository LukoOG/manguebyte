//! The settings of the newer voices of `manguebyte-dsp` that parts play:
//! the drum kits of [`DrumKitVoice`](manguebyte_dsp::instruments::DrumKitVoice),
//! the polyphonic synthesiser, the mallets, the bowed strings, the winds
//! and the choir. Each preset by its index, and the settings a part's knobs
//! make of it.

#[cfg(test)]
use manguebyte_dsp::instruments::poly::Category;
use manguebyte_dsp::instruments::poly::{PRESETS, PolySynthParams};
use manguebyte_dsp::instruments::{
    BowedParams, ChoirParams, DrumKit as Pieces, MalletParams, WindParams, gm,
};

use manguebyte_params::Param;

use crate::drums::Drum;
#[cfg(test)]
use crate::level::Phrase;
use crate::params::{BowedKnobs, ChoirKnobs, DrumKitKnobs, MalletKnobs, PolyKnobs, WindKnobs};

/// The dynamics a drum kit part plays each kit preset with, in the order
/// of `drum_kit.preset`: how much quieter the softest strike is than the
/// hardest, and how much louder an accent is, in decibels.
///
/// The kits of `manguebyte-dsp` span 30 dB of velocity and accent by 4 dB
/// (the lo-fi kit 24 dB). At the loudness target, a kit whose drums are
/// spiky (the acoustic kick's beater, the 606's short kick) would then put
/// its loudest drum, struck accented at full velocity, past the master
/// limiter's ceiling: its groove at velocity 0.8 is that far below it. So a
/// part narrows those kits' dynamics as far as it takes for that drum to
/// peak at -1 dBTP or below, and leaves the others as they are. A strike
/// still grows harder and brighter with velocity and an accent as before;
/// `drum_kit.velocity` and `drum_kit.accent` widen them again.
pub(crate) const KIT_DYNAMICS: [(f64, f64); 10] = [
    (11.0, 2.0), // acoustic
    (24.0, 3.0), // 808
    (30.0, 3.0), // 909
    (10.0, 1.5), // 606
    (16.0, 2.0), // CR-78
    (30.0, 4.0), // trap
    (30.0, 3.0), // techno
    (30.0, 3.0), // house
    (24.0, 3.0), // drum and bass
    (24.0, 4.0), // lo-fi
];

/// The drum kit preset with this index: acoustic, 808, 909, 606, CR-78,
/// trap, techno, house, drum and bass, lo-fi; at the dynamics of
/// [`KIT_DYNAMICS`].
pub(crate) fn drum_kit_preset(index: usize) -> Pieces {
    let kit = match index {
        1 => Pieces::style_808(),
        2 => Pieces::style_909(),
        3 => Pieces::style_606(),
        4 => Pieces::style_cr78(),
        5 => Pieces::trap(),
        6 => Pieces::techno(),
        7 => Pieces::house(),
        8 => Pieces::drum_and_bass(),
        9 => Pieces::lo_fi(),
        _ => Pieces::acoustic(),
    };
    let (velocity_range_db, accent_db) = KIT_DYNAMICS[index.min(KIT_DYNAMICS.len() - 1)];
    Pieces {
        velocity_range_db,
        accent_db,
        ..kit
    }
}

/// Aratu's shaker, which General MIDI 2 puts on 82, past the last key a
/// kit maps.
pub(crate) const ARATU_SHAKER: u8 = 82;

/// The key of `pieces` that plays `key`: the key itself if the kit has a
/// piece there, or for Aratu's shaker (82, see the crate's README) the
/// kit's maracas (70) or cabasa (69), the shakers of General MIDI 1.
/// `None` if the kit has nothing for it.
pub(crate) fn kit_key(pieces: &Pieces, key: u8) -> Option<u8> {
    if pieces.piece(key).is_some() {
        return Some(key);
    }
    if key == ARATU_SHAKER {
        return [gm::MARACAS, gm::CABASA]
            .into_iter()
            .find(|&shaker| pieces.piece(shaker).is_some());
    }
    None
}

/// The level of `kick.level` and the other drum levels, 0.8 by default,
/// as a factor on the kit's own level of the piece, exactly 1 by default.
fn piece_level(level: f64) -> f64 {
    level / f64::from(0.8_f32)
}

/// The drum kit's settings from its knobs: the preset's pieces tuned by
/// `kit.tune`, stretched by `kit.decay` and moved by `drum_kit.tone` and
/// `drum_kit.snap`; each drum of the drum map at its level.
pub(crate) fn drum_kit_settings(knobs: &DrumKitKnobs) -> Pieces {
    let mut pieces = Pieces {
        velocity_range_db: knobs.velocity.plain(),
        accent_db: knobs.accent.plain(),
        ..drum_kit_preset(knobs.preset.index())
    };
    let (tune, decay) = (knobs.kit.tune.plain(), knobs.kit.decay.plain());
    let (tone, snap) = (knobs.tone.plain(), knobs.snap.plain());
    for piece in pieces.pieces.iter_mut().flatten() {
        piece.tune += tune;
        piece.decay *= decay;
        piece.tone = (piece.tone + tone).clamp(0.0, 1.0);
        piece.snap = (piece.snap + snap).clamp(0.0, 1.0);
    }
    for (drum, level) in Drum::ALL.into_iter().zip(&knobs.kit.levels) {
        if let Some(mut piece) = pieces.piece(drum.note()).copied() {
            piece.level *= piece_level(level.plain());
            pieces.set(drum.note(), Some(piece));
        }
    }
    pieces
}

/// The presets of the polyphonic synthesiser whose dynamics a part
/// narrows, as for the kits of [`KIT_DYNAMICS`]: the index, the share of
/// 24 dB the level follows the velocity over, and the accent in decibels.
/// "Plucked String" excites its comb with a burst of noise seeded by the
/// note, which for an accented G4 at full velocity peaks 10 dB above the
/// same note at 0.8; at its own 0.5 and 3 dB that note would reach the
/// master limiter.
pub(crate) const POLY_DYNAMICS: [(usize, f64, f64); 1] = [(41, 0.2, 1.3)];

/// The polyphonic synthesiser's preset with this index, of [`PRESETS`],
/// with the dynamics of [`POLY_DYNAMICS`].
pub(crate) fn poly_preset(index: usize) -> PolySynthParams {
    let mut params = PRESETS.get(index).unwrap_or(&PRESETS[0]).params;
    if let Some(&(_, velocity, accent_db)) = POLY_DYNAMICS.iter().find(|row| row.0 == index) {
        params.velocity = velocity;
        params.accent_db = accent_db;
    }
    params
}

/// How many notes a part plays each preset of the polyphonic synthesiser
/// with at once, in the order of `poly.preset` (`poly.voices` sets it
/// otherwise): as many, up to [`POLY_VOICES`](crate::POLY_VOICES), as keep
/// a part of the preset playing all of them within 40% of one core of a
/// 15 W laptop in WebAssembly, from the cost of one voice of each preset
/// measured there by the example `voice_cost.mjs`; four at the least, a
/// triad and a note ringing on. The
/// cost is the least of three runs, the machine being busy. The monophonic
/// and legato presets play one note whatever this says.
pub(crate) const POLY_PRESET_VOICES: [u8; 68] = [
    4, 4, 4, 4, 4, 5, 4, 5, 4, 5, 8, 8, 4, 5, 4, // basses
    4, 4, 4, 8, 4, 4, 5, 4, 4, 4, 4, // leads
    5, 6, 7, 4, 4, 4, 4, 4, 4, 4, 4, 4, // pads
    4, 4, 8, 8, 4, 6, 5, 6, 4, // plucks
    5, 5, 4, 7, 4, 4, 5, // keys
    4, 4, 5, 4, 5, 4, // brass
    4, 4, 5, 8, 8, 4, 4, 7, // effects
];

/// How many notes the polyphonic synthesiser's preset `index` plays at
/// once by default: [`POLY_PRESET_VOICES`].
pub(crate) fn poly_voices(index: usize) -> usize {
    usize::from(POLY_PRESET_VOICES[index.min(POLY_PRESET_VOICES.len() - 1)])
}

/// The phrase a preset of the polyphonic synthesiser is measured on, by
/// its category: a bass line for the basses, chords for the pads, keys
/// and brass, the melody for the rest.
#[cfg(test)]
pub(crate) const fn poly_phrase(category: Category) -> Phrase {
    match category {
        Category::Bass => Phrase::Bass,
        Category::Pad | Category::Keys | Category::Brass => Phrase::Chord,
        Category::Lead | Category::Pluck | Category::Fx => Phrase::Melody,
    }
}

/// The polyphonic synthesiser's settings: the preset, its filters'
/// cutoffs moved by `poly.cutoff` octaves and their resonance by
/// `poly.resonance`, the amplifier envelope's attack and release times
/// `poly.attack` and `poly.release`, every oscillator's unison detune and
/// spread times `poly.detune` and `poly.spread`.
pub(crate) fn poly_settings(knobs: &PolyKnobs) -> PolySynthParams {
    let mut params = poly_preset(knobs.preset.index());
    let shift = knobs.cutoff.plain().exp2();
    let resonance = knobs.resonance.plain();
    for filter in &mut params.filters {
        filter.cutoff = (filter.cutoff * shift).clamp(20.0, 20_000.0);
        filter.resonance = (filter.resonance + resonance).clamp(0.0, 1.0);
    }
    let amp = &mut params.envelopes[0];
    amp.attack *= knobs.attack.plain();
    amp.release *= knobs.release.plain();
    let (detune, spread) = (knobs.detune.plain(), knobs.spread.plain());
    for osc in &mut params.oscillators {
        osc.detune = (osc.detune * detune).clamp(0.0, 1.0);
        osc.spread = (osc.spread * spread).clamp(0.0, 1.0);
    }
    params
}

/// The mallet preset with this index: marimba, xylophone, vibraphone,
/// glockenspiel, celesta, kalimba, steel pan, tubular bells.
pub(crate) fn mallet_preset(index: usize) -> MalletParams {
    match index {
        1 => MalletParams::xylophone(),
        2 => MalletParams::vibraphone(),
        3 => MalletParams::glockenspiel(),
        4 => MalletParams::celesta(),
        5 => MalletParams::kalimba(),
        6 => MalletParams::steel_pan(),
        7 => MalletParams::tubular_bells(),
        _ => MalletParams::marimba(),
    }
}

/// The mallets' settings: the preset, its contact shorter by
/// `mallet.hardness`, its decay times `mallet.decay`, released over
/// `mallet.release`, struck at `mallet.position`, the motor at
/// `mallet.motor`.
pub(crate) fn mallet_settings(knobs: &MalletKnobs) -> MalletParams {
    let preset = mallet_preset(knobs.preset.index());
    MalletParams {
        contact: preset.contact / knobs.hardness.plain(),
        decay: preset.decay * knobs.decay.plain(),
        release: knobs.release.plain(),
        strike_position: knobs.position.plain(),
        motor_rate: knobs.motor.plain(),
        ..preset
    }
}

/// The bowed string preset with this index: violin, viola, cello, double
/// bass, string section, violin, cello and double bass pizzicato.
///
/// The violin's pizzicato is narrowed as the kits of [`KIT_DYNAMICS`] are,
/// from 24 dB of velocity and a 3 dB accent to 14 dB and 2 dB: a finger
/// plucking harder pulls the string further and lets it go sharper, so its
/// loudest pluck, accented at full velocity, peaks 12 dB above its melody's
/// at 0.8 and would reach the master limiter at the loudness target.
pub(crate) fn bowed_preset(index: usize) -> BowedParams {
    match index {
        1 => BowedParams::viola(),
        2 => BowedParams::cello(),
        3 => BowedParams::contrabass(),
        4 => BowedParams::string_ensemble(),
        5 => BowedParams {
            velocity_range_db: 14.0,
            accent_db: 2.0,
            ..BowedParams::violin_pizzicato()
        },
        6 => BowedParams::cello_pizzicato(),
        7 => BowedParams::contrabass_pizzicato(),
        _ => BowedParams::violin(),
    }
}

/// The bowed strings' settings: the preset, with the knobs.
pub(crate) fn bowed_settings(knobs: &BowedKnobs) -> BowedParams {
    BowedParams {
        pressure: knobs.pressure.plain(),
        bow_position: knobs.position.plain(),
        vibrato_depth: knobs.vibrato.plain(),
        attack: knobs.attack.plain(),
        release: knobs.release.plain(),
        tone: knobs.tone.plain(),
        portamento: knobs.portamento.value(),
        ..bowed_preset(knobs.preset.index())
    }
}

/// The wind preset with this index: flute, clarinet, saxophone, trumpet,
/// trombone.
pub(crate) fn wind_preset(index: usize) -> WindParams {
    match index {
        1 => WindParams::clarinet(),
        2 => WindParams::saxophone(),
        3 => WindParams::trumpet(),
        4 => WindParams::trombone(),
        _ => WindParams::flute(),
    }
}

/// The winds' settings: the preset, with the knobs.
pub(crate) fn wind_settings(knobs: &WindKnobs) -> WindParams {
    WindParams {
        noise: knobs.noise.plain(),
        vibrato_depth: knobs.vibrato.plain(),
        attack: knobs.attack.plain(),
        release: knobs.release.plain(),
        tone: knobs.tone.plain(),
        portamento: knobs.portamento.value(),
        ..wind_preset(knobs.preset.index())
    }
}

/// The choir preset with this index: the "aah" choir, the "ooh" choir,
/// the solo voice.
pub(crate) fn choir_preset(index: usize) -> ChoirParams {
    match index {
        1 => ChoirParams::ooh_choir(),
        2 => ChoirParams::solo_voice(),
        _ => ChoirParams::aah_choir(),
    }
}

/// The choir's settings: the preset, with the knobs.
pub(crate) fn choir_settings(knobs: &ChoirKnobs) -> ChoirParams {
    ChoirParams {
        vowel: knobs.vowel.plain(),
        vibrato_depth: knobs.vibrato.plain(),
        attack: knobs.attack.plain(),
        release: knobs.release.plain(),
        breathiness: knobs.breathiness.plain(),
        brightness: knobs.brightness.plain(),
        ..choir_preset(knobs.preset.index())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{
        BOWED_PRESET_NAMES, CHOIR_PRESET_NAMES, DRUM_KIT_PRESET_NAMES, MALLET_PRESET_NAMES,
        POLY_PRESET_NAMES, WIND_PRESET_NAMES,
    };

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * b.abs().max(1e-3)
    }

    #[test]
    fn the_poly_names_are_the_librarys() {
        assert_eq!(POLY_PRESET_NAMES.len(), PRESETS.len());
        for (name, preset) in POLY_PRESET_NAMES.iter().zip(&PRESETS) {
            assert_eq!(*name, preset.name);
        }
        // The narrowed presets are the ones their notes name.
        assert_eq!(PRESETS[POLY_DYNAMICS[0].0].name, "Plucked String");
        assert_eq!(poly_preset(41).accent_db, 1.3);
        assert_eq!(poly_preset(40), PRESETS[40].params);
    }

    #[test]
    fn every_preset_index_is_its_own_preset() {
        let kits: Vec<Pieces> = (0..DRUM_KIT_PRESET_NAMES.len())
            .map(drum_kit_preset)
            .collect();
        for (index, kit) in kits.iter().enumerate() {
            assert!(
                kits[..index].iter().all(|other| other != kit),
                "drum kit {index} is new"
            );
        }
        for index in 0..MALLET_PRESET_NAMES.len() {
            assert!((0..index).all(|other| mallet_preset(other) != mallet_preset(index)));
        }
        for index in 0..BOWED_PRESET_NAMES.len() {
            assert!((0..index).all(|other| bowed_preset(other) != bowed_preset(index)));
        }
        for index in 0..WIND_PRESET_NAMES.len() {
            assert!((0..index).all(|other| wind_preset(other) != wind_preset(index)));
        }
        for index in 0..CHOIR_PRESET_NAMES.len() {
            assert!((0..index).all(|other| choir_preset(other) != choir_preset(index)));
        }
    }

    /// The knobs at the positions a preset moves them to are the preset as
    /// it is, to the precision of a parameter, for every preset.
    #[test]
    fn the_knobs_at_a_presets_positions_are_the_preset() {
        let poly = PolyKnobs::new();
        for index in 0..POLY_PRESET_NAMES.len() {
            poly.preset.set_plain(index as f64);
            poly.load_preset();
            assert_eq!(poly_settings(&poly), poly_preset(index), "poly {index}");
        }
        let mallet = MalletKnobs::new();
        for index in 0..MALLET_PRESET_NAMES.len() {
            mallet.preset.set_plain(index as f64);
            mallet.load_preset();
            let (made, preset) = (mallet_settings(&mallet), mallet_preset(index));
            assert!(
                close(made.strike_position, preset.strike_position),
                "mallet {index}"
            );
            assert!(close(made.motor_rate, preset.motor_rate), "mallet {index}");
            assert!(close(made.contact, preset.contact) && close(made.decay, preset.decay));
            assert!(close(made.release, preset.release));
        }
        let bowed = BowedKnobs::new();
        for index in 0..BOWED_PRESET_NAMES.len() {
            bowed.preset.set_plain(index as f64);
            bowed.load_preset();
            let (made, preset) = (bowed_settings(&bowed), bowed_preset(index));
            for (knob, value) in [
                (made.pressure, preset.pressure),
                (made.bow_position, preset.bow_position),
                (made.vibrato_depth, preset.vibrato_depth),
                (made.attack, preset.attack),
                (made.release, preset.release),
                (made.tone, preset.tone),
            ] {
                assert!(close(knob, value), "bowed {index}: {knob} {value}");
            }
            assert_eq!(made.portamento, preset.portamento);
            assert_eq!(made.players, preset.players);
        }
        let wind = WindKnobs::new();
        for index in 0..WIND_PRESET_NAMES.len() {
            wind.preset.set_plain(index as f64);
            wind.load_preset();
            let (made, preset) = (wind_settings(&wind), wind_preset(index));
            for (knob, value) in [
                (made.noise, preset.noise),
                (made.vibrato_depth, preset.vibrato_depth),
                (made.attack, preset.attack),
                (made.release, preset.release),
                (made.tone, preset.tone),
            ] {
                assert!(close(knob, value), "wind {index}: {knob} {value}");
            }
            assert_eq!(made.portamento, preset.portamento);
            assert_eq!(made.kind, preset.kind);
        }
        let choir = ChoirKnobs::new();
        for index in 0..CHOIR_PRESET_NAMES.len() {
            choir.preset.set_plain(index as f64);
            choir.load_preset();
            let (made, preset) = (choir_settings(&choir), choir_preset(index));
            for (knob, value) in [
                (made.vowel, preset.vowel),
                (made.vibrato_depth, preset.vibrato_depth),
                (made.attack, preset.attack),
                (made.release, preset.release),
                (made.breathiness, preset.breathiness),
                (made.brightness, preset.brightness),
            ] {
                assert!(close(knob, value), "choir {index}: {knob} {value}");
            }
        }
        // The kit's knobs at their defaults leave every piece as it is.
        let kit = DrumKitKnobs::new();
        for index in 0..DRUM_KIT_PRESET_NAMES.len() {
            kit.preset.set_plain(index as f64);
            kit.load_preset();
            assert_eq!(
                drum_kit_settings(&kit),
                drum_kit_preset(index),
                "kit {index}"
            );
        }
    }

    #[test]
    fn the_kits_knobs_reach_their_pieces() {
        let knobs = DrumKitKnobs::new();
        knobs.preset.set_plain(2.0);
        knobs.kit.tune.set_plain(-3.0);
        knobs.kit.decay.set_plain(2.0);
        knobs.kit.levels[1].set_plain(0.4);
        knobs.tone.set_plain(1.0);
        let made = drum_kit_settings(&knobs);
        let plain = drum_kit_preset(2);
        let snare = made.piece(gm::SNARE).unwrap();
        let before = plain.piece(gm::SNARE).unwrap();
        assert!(close(snare.level, before.level * 0.5));
        assert!(close(snare.tune, before.tune - 3.0));
        assert!(close(snare.decay, before.decay * 2.0));
        assert_eq!(snare.tone, 1.0);
        // Only the drum map's snare key takes the snare's level.
        let rimshot = made.piece(gm::ELECTRIC_SNARE).unwrap();
        assert!(close(rimshot.level, before.level));
    }

    #[test]
    fn aratus_shaker_plays_a_kits_shaker() {
        assert_eq!(kit_key(&Pieces::house(), ARATU_SHAKER), Some(gm::MARACAS));
        assert_eq!(kit_key(&Pieces::style_909(), ARATU_SHAKER), None);
        assert_eq!(kit_key(&Pieces::style_909(), gm::SNARE), Some(gm::SNARE));
        assert_eq!(kit_key(&Pieces::style_606(), gm::SIDE_STICK), None);
    }
}
