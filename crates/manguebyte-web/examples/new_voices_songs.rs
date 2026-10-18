//! Renders a short song for each family of the newer voices, the way Aratu
//! would play them, through the whole engine to 16-bit 48 kHz stereo WAV
//! files, to judge them by ear.
//!
//! ```text
//! cargo run --release -p manguebyte-web --example new_voices_songs -- <directory>
//! ```
//!
//! Five songs of eight bars, each scaled so that it peaks at -3 dBFS:
//!
//! - a techno groove on the 909 kit, a rolling bass and rave chord stabs of
//!   the polyphonic synthesiser, the stabs' filter opening with the wheel;
//! - a trap beat: the trap kit's hi-hat rolls and claps over the 808 glide
//!   bass, sliding where its notes overlap;
//! - a house groove: the house kit with its shaker (Aratu's key 82), a
//!   garage bass and a warm pad;
//! - a lo-fi beat: the lo-fi kit, a vibraphone and the synthesiser's
//!   electric piano;
//! - an orchestral bit: a string section and cellos, a clarinet slurring
//!   its melody, a flute, a marimba, an "aah" choir and the acoustic kit's
//!   timpani-like floor tom and crash.
//!
//! Drum lines are written as Aratu writes them, a sixteenth a letter,
//! capitals accented. The names of the files are Portuguese, for the
//! person who listens to them.

use std::fs;
use std::io;
use std::path::Path;

use manguebyte_web::{
    Chain, Drum, Effect, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, ids,
};

const RATE: u32 = 48_000;
const BARS: usize = 8;

fn id(name: &str) -> ParamId {
    ParamId::from_name(name)
}

/// The index of the polyphonic synthesiser's preset `name`.
fn poly(name: &str) -> f64 {
    manguebyte_dsp::instruments::poly::PRESETS
        .iter()
        .position(|preset| preset.name == name)
        .unwrap_or_else(|| panic!("no preset {name}")) as f64
}

/// A song being written: its tempo and its records.
struct Song {
    tempo: f64,
    records: Vec<EventRecord>,
}

impl Song {
    fn new(tempo: f64) -> Self {
        Self {
            tempo,
            records: vec![EventRecord::param(0, 0, id(ids::MASTER_TEMPO), tempo)],
        }
    }

    /// The frame of `bar`, then `step` sixteenths into it.
    fn at(&self, bar: usize, step: f64) -> u64 {
        let sixteenth = 60.0 / self.tempo / 4.0 * f64::from(RATE);
        ((bar as f64 * 16.0 + step) * sixteenth).round() as u64
    }

    fn set(&mut self, part: u16, name: &str, value: f64) {
        self.records
            .push(EventRecord::param(0, part, id(name), value));
    }

    fn set_at(&mut self, bar: usize, part: u16, name: &str, value: f64) {
        let frame = self.at(bar, 0.0);
        self.records
            .push(EventRecord::param(frame, part, id(name), value));
    }

    /// A drum line in Aratu's letters for the bars `bars`: `b` kick, `s`
    /// snare, `c` closed hat, `o` open hat, `x` crash, `r` ride, `t`, `m`,
    /// `f` toms, `k` side stick; and, off the drum map, `p` the clap (39)
    /// and `h` the shaker (82). A `3` plays a roll of three in the
    /// sixteenth before it, on the hat.
    fn drums(&mut self, part: u16, bars: core::ops::Range<usize>, line: &str) {
        for bar in bars {
            for (step, symbol) in line.chars().enumerate() {
                let key = match symbol.to_ascii_lowercase() {
                    'p' => Some(39),
                    'h' => Some(82),
                    '3' => {
                        for third in 0..3 {
                            let at = self.at(bar, step as f64 + f64::from(third) / 3.0);
                            self.records.push(EventRecord::note_on(
                                at,
                                part,
                                42,
                                0.55 + 0.1 * third as f32,
                            ));
                        }
                        None
                    }
                    letter => Drum::from_symbol(letter).map(|drum| drum.note()),
                };
                let Some(key) = key else {
                    continue;
                };
                let velocity = if step % 4 == 0 { 0.9 } else { 0.72 };
                let at = self.at(bar, step as f64);
                self.records.push(
                    EventRecord::note_on(at, part, key, velocity)
                        .with_accent(symbol.is_ascii_uppercase()),
                );
            }
        }
    }

    /// A note from `start` sixteenths into `bar`, `length` sixteenths long.
    fn note(&mut self, part: u16, bar: usize, start: f64, length: f64, key: u8, velocity: f32) {
        let on = self.at(bar, start);
        let off = self.at(bar, start + length);
        self.records
            .push(EventRecord::note_on(on, part, key, velocity));
        self.records.push(EventRecord::note_off(off, part, key));
    }

    fn accented(&mut self, part: u16, bar: usize, start: f64, length: f64, key: u8) {
        let on = self.at(bar, start);
        let off = self.at(bar, start + length);
        self.records
            .push(EventRecord::note_on(on, part, key, 0.9).with_accent(true));
        self.records.push(EventRecord::note_off(off, part, key));
    }

    fn chord(
        &mut self,
        part: u16,
        bar: usize,
        start: f64,
        length: f64,
        keys: &[u8],
        velocity: f32,
    ) {
        for &key in keys {
            self.note(part, bar, start, length, key, velocity);
        }
    }
}

/// A techno groove on the 909 kit, 130 beats a minute in F minor.
fn techno() -> (Vec<(u16, InstrumentKind, Chain)>, Song) {
    let (drums, bass, stabs) = (0, 1, 2);
    let mut song = Song::new(130.0);
    song.set(drums, ids::DRUM_KIT_PRESET, 2.0);
    song.set(drums, ids::SEND, 0.06);
    song.set(bass, ids::POLY_PRESET, poly("Rolling Bass"));
    song.set(bass, ids::GAIN, -3.0);
    song.set(stabs, ids::POLY_PRESET, poly("Rave Chord Stab"));
    song.set(stabs, ids::GAIN, -6.0);
    song.set(stabs, ids::PAN, 0.2);
    song.set(stabs, ids::SEND, 0.25);
    song.set(stabs, ids::DELAY_SYNC, 0.75);
    song.set(stabs, ids::DELAY_MIX, 0.25);
    song.set(stabs, ids::DELAY_ON, 1.0);
    song.drums(drums, 0..BARS, "B...B...B...B...");
    song.drums(drums, 2..BARS, "..c...c...c...cO");
    song.drums(drums, 4..BARS, "....p.......P...");
    song.drums(drums, 4..BARS, "r.r.r.r.r.r.r.r.");
    song.drums(drums, 0..1, "X...............");
    song.drums(drums, 4..5, "X...............");
    for bar in 0..BARS {
        // The off-beat bass, an octave jump on the last.
        for step in [2.0, 6.0, 10.0, 14.0] {
            let key = if step == 14.0 && bar % 2 == 1 { 41 } else { 29 };
            song.note(bass, bar, step, 1.5, key, 0.85);
        }
    }
    for bar in 2..BARS {
        for (step, accent) in [(0.0, true), (3.0, false), (6.0, false), (10.0, true)] {
            for key in [65u8, 68, 72] {
                if accent {
                    song.accented(stabs, bar, step, 1.0, key);
                } else {
                    song.note(stabs, bar, step, 1.0, key, 0.7);
                }
            }
        }
    }
    // The wheel opens the stabs over the second half.
    for bar in 4..BARS {
        song.set_at(bar, stabs, ids::POLY_MOD_WHEEL, (bar - 3) as f64 / 4.0);
        song.set_at(bar, stabs, ids::POLY_CUTOFF, (bar - 4) as f64 * 0.4);
    }
    (
        vec![
            (drums, InstrumentKind::DrumKit, Chain::NONE),
            (bass, InstrumentKind::Poly, Chain::NONE),
            (stabs, InstrumentKind::Poly, Chain::NONE.with(Effect::Delay)),
        ],
        song,
    )
}

/// A trap beat at 140 beats a minute, half time, in C minor.
fn trap() -> (Vec<(u16, InstrumentKind, Chain)>, Song) {
    let (drums, bass) = (0, 1);
    let mut song = Song::new(140.0);
    song.set(drums, ids::DRUM_KIT_PRESET, 5.0);
    song.set(drums, ids::SEND, 0.05);
    song.set(bass, ids::POLY_PRESET, poly("808 Glide"));
    song.set(bass, ids::GAIN, -1.0);
    song.drums(drums, 0..BARS, "B......b..B.....");
    song.drums(drums, 0..BARS, "........P.......");
    for bar in 0..BARS {
        let hats = if bar % 2 == 1 {
            "c.c.c.c.c3c.c.3c"
        } else {
            "c.c.c.c.c.c.cocc"
        };
        song.drums(drums, bar..bar + 1, hats);
    }
    song.drums(drums, 0..1, "X...............");
    // The 808: long notes, the second of each pair sliding from the first.
    let line: [(f64, f64, u8); 4] = [
        (0.0, 7.5, 36),
        (7.0, 3.5, 39),
        (10.0, 4.0, 34),
        (13.5, 2.5, 31),
    ];
    for bar in 0..BARS {
        for (index, &(start, length, key)) in line.iter().enumerate() {
            // Held past the next note's start: a slide.
            let held = if index % 2 == 0 { length + 0.2 } else { length };
            song.note(
                bass,
                bar,
                start,
                held,
                key,
                if index == 0 { 0.95 } else { 0.8 },
            );
        }
    }
    (
        vec![
            (drums, InstrumentKind::DrumKit, Chain::NONE),
            (bass, InstrumentKind::Poly, Chain::NONE),
        ],
        song,
    )
}

/// A house groove at 124 beats a minute in A minor, with a bass and a pad.
fn house() -> (Vec<(u16, InstrumentKind, Chain)>, Song) {
    let (drums, bass, pad) = (0, 1, 2);
    let mut song = Song::new(124.0);
    song.set(drums, ids::DRUM_KIT_PRESET, 7.0);
    song.set(drums, ids::SEND, 0.08);
    song.set(bass, ids::POLY_PRESET, poly("Garage Square"));
    song.set(bass, ids::GAIN, -2.0);
    song.set(pad, ids::POLY_PRESET, poly("Warm Analog Pad"));
    song.set(pad, ids::GAIN, -6.0);
    song.set(pad, ids::SEND, 0.3);
    song.set(pad, ids::CHORUS_MIX, 0.3);
    song.set(pad, ids::CHORUS_ON, 1.0);
    song.drums(drums, 0..BARS, "B...B...B...B...");
    song.drums(drums, 0..BARS, "..o...o...o...o.");
    song.drums(drums, 2..BARS, "....P.......P...");
    song.drums(drums, 2..BARS, "hhHhhhHhhhHhhhHh");
    song.drums(drums, 4..BARS, "c.c.c.c.c.c.c.c.");
    song.drums(drums, 0..1, "X...............");
    song.drums(drums, 4..5, "X...............");
    let chords: [(u8, [u8; 4]); 4] = [
        (33, [57, 60, 64, 67]),
        (29, [57, 60, 65, 69]),
        (31, [55, 59, 62, 67]),
        (28, [56, 59, 64, 68]),
    ];
    for bar in 0..BARS {
        let (root, keys) = chords[bar % 4];
        song.chord(pad, bar, 0.0, 15.5, &keys, 0.7);
        for (step, octave) in [
            (0.0, 0u8),
            (3.0, 12),
            (6.0, 0),
            (8.0, 0),
            (11.0, 12),
            (14.0, 0),
        ] {
            song.note(bass, bar, step, 1.5, root + octave, 0.85);
        }
    }
    (
        vec![
            (drums, InstrumentKind::DrumKit, Chain::NONE),
            (bass, InstrumentKind::Poly, Chain::NONE),
            (pad, InstrumentKind::Poly, Chain::NONE.with(Effect::Chorus)),
        ],
        song,
    )
}

/// A lo-fi beat at 84 beats a minute: the lo-fi kit, a vibraphone and the
/// synthesiser's electric piano, in D minor.
fn lo_fi() -> (Vec<(u16, InstrumentKind, Chain)>, Song) {
    let (drums, vibes, keys) = (0, 1, 2);
    let mut song = Song::new(84.0);
    song.set(drums, ids::DRUM_KIT_PRESET, 9.0);
    song.set(drums, ids::SEND, 0.1);
    song.set(vibes, ids::MALLET_PRESET, 2.0);
    song.set(vibes, ids::GAIN, -3.0);
    song.set(vibes, ids::PAN, 0.3);
    song.set(vibes, ids::SEND, 0.3);
    song.set(keys, ids::POLY_PRESET, poly("Wurly"));
    song.set(keys, ids::GAIN, -4.0);
    song.set(keys, ids::PAN, -0.3);
    song.set(keys, ids::SEND, 0.2);
    song.drums(drums, 0..BARS, "B.....b...B..b..");
    song.drums(drums, 0..BARS, "....S.......S...");
    song.drums(drums, 0..BARS, "c.c.c.cCc.c.c.co");
    song.drums(drums, 0..BARS, "...............k");
    let chords: [[u8; 4]; 4] = [
        [50, 53, 57, 60],
        [55, 58, 62, 65],
        [52, 55, 58, 62],
        [57, 61, 64, 67],
    ];
    let melody: [(f64, u8); 6] = [
        (0.0, 74),
        (3.0, 77),
        (6.0, 76),
        (8.0, 72),
        (11.0, 74),
        (14.0, 69),
    ];
    for bar in 0..BARS {
        song.chord(keys, bar, 0.0, 7.0, &chords[bar % 4], 0.6);
        song.chord(keys, bar, 8.0, 6.0, &chords[bar % 4], 0.5);
        if bar >= 2 {
            for &(step, key) in &melody {
                let key = if bar % 2 == 1 { key - 2 } else { key };
                song.note(vibes, bar, step, 2.0, key, 0.65);
            }
        }
    }
    (
        vec![
            (drums, InstrumentKind::DrumKit, Chain::NONE),
            (vibes, InstrumentKind::Mallet, Chain::NONE),
            (keys, InstrumentKind::Poly, Chain::NONE),
        ],
        song,
    )
}

/// An orchestral bit at 88 beats a minute in D major: strings, cellos, a
/// clarinet and a flute, a marimba, a choir and the acoustic kit's toms
/// and crash.
fn orchestra() -> (Vec<(u16, InstrumentKind, Chain)>, Song) {
    let (strings, cellos, clarinet, flute, marimba, choir, drums) = (0, 1, 2, 3, 4, 5, 6);
    let mut song = Song::new(88.0);
    song.set(strings, ids::BOWED_PRESET, 4.0);
    song.set(strings, ids::GAIN, -3.0);
    song.set(strings, ids::SEND, 0.35);
    song.set(cellos, ids::BOWED_PRESET, 2.0);
    song.set(cellos, ids::GAIN, -2.0);
    song.set(cellos, ids::PAN, 0.35);
    song.set(cellos, ids::SEND, 0.3);
    song.set(clarinet, ids::WIND_PRESET, 1.0);
    song.set(clarinet, ids::PAN, -0.25);
    song.set(clarinet, ids::SEND, 0.3);
    song.set(flute, ids::WIND_PRESET, 0.0);
    song.set(flute, ids::PAN, 0.25);
    song.set(flute, ids::SEND, 0.35);
    song.set(marimba, ids::MALLET_PRESET, 0.0);
    song.set(marimba, ids::GAIN, -5.0);
    song.set(marimba, ids::PAN, -0.4);
    song.set(marimba, ids::SEND, 0.25);
    song.set(choir, ids::CHOIR_PRESET, 0.0);
    song.set(choir, ids::GAIN, -5.0);
    song.set(choir, ids::SEND, 0.45);
    song.set(drums, ids::DRUM_KIT_PRESET, 0.0);
    song.set(drums, ids::GAIN, -4.0);
    song.set(drums, ids::SEND, 0.35);
    song.set(0, ids::MASTER_REVERB_DECAY, 2.8);
    let chords: [(u8, [u8; 3]); 8] = [
        (38, [62, 66, 69]),
        (35, [62, 66, 71]),
        (43, [62, 67, 71]),
        (45, [61, 64, 69]),
        (38, [62, 66, 69]),
        (42, [61, 66, 69]),
        (43, [59, 62, 67]),
        (45, [61, 64, 67]),
    ];
    for (bar, &(root, keys)) in chords.iter().enumerate() {
        song.chord(strings, bar, 0.0, 16.2, &keys, 0.7);
        song.note(cellos, bar, 0.0, 8.0, root, 0.8);
        song.note(cellos, bar, 8.0, 8.0, root + 7, 0.7);
        if bar >= 4 {
            song.chord(choir, bar, 0.0, 15.5, &keys, 0.6);
        }
        // The marimba's arpeggio, an eighth at a time.
        for (eighth, &key) in keys
            .iter()
            .chain(keys.iter().rev())
            .cycle()
            .take(8)
            .enumerate()
        {
            song.note(marimba, bar, eighth as f64 * 2.0, 2.0, key + 12, 0.6);
        }
    }
    // The clarinet sings the tune, its notes held into the next: slurred.
    let tune: [(usize, f64, f64, u8); 10] = [
        (0, 0.0, 6.0, 74),
        (0, 6.0, 2.2, 76),
        (0, 8.0, 8.0, 78),
        (1, 0.0, 4.2, 79),
        (1, 4.0, 4.2, 78),
        (1, 8.0, 8.0, 74),
        (2, 0.0, 8.2, 71),
        (2, 8.0, 8.0, 74),
        (3, 0.0, 12.0, 73),
        (3, 12.0, 4.0, 69),
    ];
    for &(bar, start, length, key) in &tune {
        song.note(clarinet, bar, start, length, key - 12, 0.8);
        // The flute answers it an octave up in the second half.
        song.note(flute, bar + 4, start, length, key, 0.75);
    }
    song.drums(drums, 0..1, "X...............");
    song.drums(drums, 3..4, "........f.f.f.FF");
    song.drums(drums, 4..5, "X...............");
    song.drums(drums, 7..8, "........t.m.f.FX");
    (
        vec![
            (strings, InstrumentKind::Bowed, Chain::NONE),
            (cellos, InstrumentKind::Bowed, Chain::NONE),
            (clarinet, InstrumentKind::Wind, Chain::NONE),
            (flute, InstrumentKind::Wind, Chain::NONE),
            (marimba, InstrumentKind::Mallet, Chain::NONE),
            (choir, InstrumentKind::Choir, Chain::NONE),
            (drums, InstrumentKind::DrumKit, Chain::NONE),
        ],
        song,
    )
}

/// The song in an engine, rendered in quanta of 128 frames as a browser
/// does, with three seconds of tail; also the most the master limiter
/// turned the level down.
fn render(parts: &[(u16, InstrumentKind, Chain)], song: &Song) -> (Vec<f32>, Vec<f32>, f64) {
    let mut engine = Engine::new(EngineConfig {
        pending_capacity: 65_536,
        ..EngineConfig::new(f64::from(RATE))
    })
    .expect("engine");
    for &(part, kind, chain) in parts {
        engine.create_part_with(part, kind, chain).expect("part");
    }
    engine.push(EventRecord::param(0, 0, id(ids::GLUE_ON), 1.0));
    engine.push(EventRecord::param(0, 0, id(ids::GLUE_THRESHOLD), -18.0));
    for &record in &song.records {
        engine.push(record);
    }
    let frames = song.at(BARS, 0.0) as usize + 3 * RATE as usize;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut reduction = 0.0f64;
    while left.len() < frames {
        let count = engine.render((frames - left.len()).min(128));
        let (l, r) = engine.output();
        left.extend_from_slice(&l[..count]);
        right.extend_from_slice(&r[..count]);
        reduction = reduction.max(engine.limiter_reduction_db());
    }
    let stats = engine.stats();
    assert_eq!(stats.dropped, 0, "{stats:?}");
    assert_eq!(stats.recovered, 0, "{stats:?}");
    (left, right, reduction.max(0.0))
}

/// Writes `left` and `right` as a 16-bit stereo WAV file, scaled by `gain`.
fn write_wav(path: &Path, left: &[f32], right: &[f32], gain: f32) -> io::Result<()> {
    let data_bytes = u32::try_from(left.len() * 4).expect("a short file");
    let mut bytes = Vec::with_capacity(44 + left.len() * 4);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for (&l, &r) in left.iter().zip(right) {
        for sample in [l, r] {
            let value = ((sample * gain).clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    fs::write(path, bytes)
}

fn main() -> io::Result<()> {
    let directory = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;
    type Make = fn() -> (Vec<(u16, InstrumentKind, Chain)>, Song);
    let songs: [(&str, Make); 5] = [
        ("1 - techno na bateria 909", techno),
        ("2 - batida de trap com 808 deslizando", trap),
        ("3 - house com baixo e pad", house),
        ("4 - batida lo-fi com vibrafone", lo_fi),
        (
            "5 - trecho orquestral (cordas, sopros, marimba e coro)",
            orchestra,
        ),
    ];
    for (name, make) in songs {
        let (parts, song) = make();
        let (left, right, reduction) = render(&parts, &song);
        let peak = left
            .iter()
            .chain(&right)
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        // Scaled to peak at -3 dBFS.
        let gain = 10f32.powf(-3.0 / 20.0) / peak.max(1e-9);
        let path = directory.join(format!("{name}.wav"));
        write_wav(&path, &left, &right, gain)?;
        println!(
            "{}: {:.1} s, engine peak {:.1} dBFS, scaled by {:+.1} dB to -3 dBFS, limiter up to {reduction:.1} dB",
            path.display(),
            left.len() as f64 / f64::from(RATE),
            20.0 * f64::from(peak).log10(),
            20.0 * f64::from(gain).log10(),
        );
    }
    Ok(())
}
