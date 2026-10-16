//! Renders a short song, the way Aratu would play it, through the whole
//! engine to 16-bit 48 kHz stereo WAV files, to judge it by ear.
//!
//! ```text
//! cargo run -p manguebyte-web --example aratu_song -- <directory>
//! ```
//!
//! Eight bars at 120 beats a minute in A minor: the analogue kit (the
//! open hat choked by the next closed one, a crash, a fill down the toms),
//! an acid bass whose capital notes are accented and whose slides are notes
//! struck before the last one is let go, organ chords whose rotating
//! speaker speeds up in bar 5 and slows down in bar 7, and a steel guitar
//! strummed down and up. Through the parts' effects (a synced delay on the
//! bass, chorus on the guitar), the shared reverb and the glue compressor.
//! `aratu_song.wav` is the whole song; `drums.wav`, `acid.wav`,
//! `organ.wav` and `guitar.wav` are each part alone through the same bus.

use std::fs;
use std::io;
use std::path::Path;

use manguebyte_web::{
    Chain, Drum, Effect, Engine, EngineConfig, EventRecord, InstrumentKind, ParamId, ids,
};

const RATE: u32 = 48_000;
const BEATS_PER_MINUTE: f64 = 120.0;
const BARS: usize = 8;

const DRUMS: u16 = 0;
const ACID: u16 = 1;
const ORGAN: u16 = 2;
const GUITAR: u16 = 3;

/// The frame of a sixteenth note: `bar`, then `step` sixteenths into it.
fn at(bar: usize, step: f64) -> u64 {
    let sixteenth = 60.0 / BEATS_PER_MINUTE / 4.0 * f64::from(RATE);
    ((bar as f64 * 16.0 + step) * sixteenth).round() as u64
}

fn id(name: &str) -> ParamId {
    ParamId::from_name(name)
}

/// The chords of each bar: the bass's root, the organ's voicing and the
/// guitar's six strings (or five).
const CHORDS: [(u8, &[u8], &[u8]); 4] = [
    // A minor, F, C, G.
    (33, &[57, 60, 64, 69], &[45, 52, 57, 60, 64]),
    (29, &[57, 60, 65, 69], &[41, 48, 53, 57, 60, 65]),
    (36, &[55, 60, 64, 67], &[48, 52, 55, 60, 64]),
    (31, &[55, 59, 62, 67], &[43, 47, 50, 55, 59, 67]),
];

/// Drum lines as Aratu writes them, a sixteenth a letter, capitals accented.
const GROOVE: [&str; 4] = [
    "B.....b.B..b..b.",
    "....S.......S...",
    "c.cCc.cCc.cCc.co",
    "x...............",
];

/// The fill of the last bar.
const FILL: [&str; 4] = [
    "B.....b.B.......",
    "....S...........",
    "c.cCc.cC........",
    "........TtMmFfX.",
];

fn drums(records: &mut Vec<EventRecord>) {
    for bar in 0..BARS {
        let lines = if bar == BARS - 1 { FILL } else { GROOVE };
        for (line, pattern) in lines.iter().enumerate() {
            // The crash only where a phrase starts.
            if line == 3 && bar != 0 && bar != 4 && bar != BARS - 1 {
                continue;
            }
            for (step, symbol) in pattern.chars().enumerate() {
                let Some(drum) = Drum::from_symbol(symbol.to_ascii_lowercase()) else {
                    continue;
                };
                let velocity = if step % 4 == 0 { 0.95 } else { 0.75 };
                records.push(
                    EventRecord::note_on(at(bar, step as f64), DRUMS, drum.note(), velocity)
                        .with_accent(symbol.is_ascii_uppercase()),
                );
            }
        }
    }
}

/// The acid line over a root: `(semitones above it, accent, slide)` a
/// sixteenth each, `None` a rest. A slide is a note held into the next one:
/// the next note on comes before this one's note off.
const LINE: [Option<(i8, bool, bool)>; 16] = [
    Some((0, true, false)),
    Some((0, false, false)),
    Some((12, false, true)),
    Some((10, false, false)),
    Some((0, false, false)),
    None,
    Some((3, true, false)),
    Some((0, false, false)),
    Some((12, false, false)),
    Some((0, false, true)),
    Some((7, false, false)),
    Some((0, false, false)),
    Some((0, true, false)),
    None,
    Some((10, false, true)),
    Some((12, true, false)),
];

fn acid(records: &mut Vec<EventRecord>) {
    for bar in 0..BARS {
        let root = CHORDS[bar % 4].0 + 12;
        for (step, note) in LINE.iter().enumerate() {
            let Some((interval, accent, slide)) = *note else {
                continue;
            };
            let key = (i16::from(root) + i16::from(interval)) as u8;
            let start = at(bar, step as f64);
            // A plain note sounds for half a sixteenth; a sliding one until
            // just after the next one has started.
            let end = if slide {
                at(bar, step as f64 + 1.0) + 10
            } else {
                at(bar, step as f64 + 0.5)
            };
            records.push(EventRecord::note_on(start, ACID, key, 0.85).with_accent(accent));
            records.push(EventRecord::note_off(end, ACID, key));
        }
    }
    // The second half squelches.
    records.push(EventRecord::param(
        at(4, 0.0),
        ACID,
        id(ids::ACID_PRESET),
        1.0,
    ));
    records.push(EventRecord::param(
        at(4, 0.0),
        ACID,
        id(ids::ACID_CUTOFF),
        300.0,
    ));
}

fn organ(records: &mut Vec<EventRecord>) {
    for bar in 0..BARS {
        for &key in CHORDS[bar % 4].1 {
            records.push(EventRecord::note_on(at(bar, 0.0), ORGAN, key, 0.8));
            records.push(EventRecord::note_off(at(bar, 15.5), ORGAN, key));
        }
    }
    // The rotating speaker: fast in bar 5, slow again in bar 7.
    records.push(EventRecord::param(
        at(4, 0.0),
        ORGAN,
        id(ids::ORGAN_ROTARY),
        2.0,
    ));
    records.push(EventRecord::param(
        at(6, 0.0),
        ORGAN,
        id(ids::ORGAN_ROTARY),
        1.0,
    ));
}

fn guitar(records: &mut Vec<EventRecord>) {
    // Down, down-up, up-down-up, in beats; the strings 12 ms apart.
    let strokes = [
        (0.0, true),
        (1.0, true),
        (1.5, false),
        (2.5, false),
        (3.0, true),
        (3.5, false),
    ];
    let string_gap = (0.012 * f64::from(RATE)) as u64;
    for bar in 0..BARS {
        let chord = CHORDS[bar % 4].2;
        for (index, &(beat, down)) in strokes.iter().enumerate() {
            let start = at(bar, beat * 4.0);
            let end = strokes
                .get(index + 1)
                .map_or(at(bar + 1, 0.0), |&(next, _)| at(bar, next * 4.0));
            let strings: Vec<u8> = if down {
                chord.to_vec()
            } else {
                chord.iter().rev().take(4).copied().collect()
            };
            for (order, &key) in strings.iter().enumerate() {
                let velocity = if down { 0.8 } else { 0.55 };
                let onset = start + order as u64 * string_gap;
                records.push(
                    EventRecord::note_on(onset, GUITAR, key, velocity).with_accent(beat == 0.0),
                );
                records.push(EventRecord::note_off(end, GUITAR, key));
            }
        }
    }
}

/// An engine with the four parts, their settings and the master bus's,
/// and the notes of the parts in `playing`.
fn song(playing: &[u16]) -> Engine {
    let mut engine = Engine::new(EngineConfig::new(f64::from(RATE))).expect("engine");
    for (part, kind, chain) in [
        (DRUMS, InstrumentKind::AnalogKit, Chain::NONE),
        (ACID, InstrumentKind::Acid, Chain::NONE.with(Effect::Delay)),
        (ORGAN, InstrumentKind::Organ, Chain::NONE),
        (
            GUITAR,
            InstrumentKind::Plucked,
            Chain::NONE.with(Effect::Chorus),
        ),
    ] {
        engine.create_part_with(part, kind, chain).expect("part");
    }
    let mut records = vec![
        EventRecord::param(0, 0, id(ids::MASTER_TEMPO), BEATS_PER_MINUTE),
        EventRecord::param(0, 0, id(ids::GLUE_THRESHOLD), -16.0),
        EventRecord::param(0, 0, id(ids::GLUE_ON), 1.0),
        EventRecord::param(0, 0, id(ids::MASTER_REVERB_DECAY), 1.8),
        // The drums.
        EventRecord::param(0, DRUMS, id(ids::GAIN), -2.0),
        EventRecord::param(0, DRUMS, id(ids::SEND), 0.08),
        EventRecord::param(0, DRUMS, id(ids::ANALOG_KICK_DECAY), 1.5),
        // The acid bass: a dotted-eighth echo.
        EventRecord::param(0, ACID, id(ids::GAIN), -2.0),
        EventRecord::param(0, ACID, id(ids::DELAY_SYNC), 0.75),
        EventRecord::param(0, ACID, id(ids::DELAY_FEEDBACK), 0.3),
        EventRecord::param(0, ACID, id(ids::DELAY_MIX), 0.18),
        EventRecord::param(0, ACID, id(ids::DELAY_ON), 1.0),
        EventRecord::param(0, ACID, id(ids::SEND), 0.05),
        // The organ, gospel, to the left.
        EventRecord::param(0, ORGAN, id(ids::ORGAN_PRESET), 1.0),
        EventRecord::param(0, ORGAN, id(ids::GAIN), -5.0),
        EventRecord::param(0, ORGAN, id(ids::PAN), -0.35),
        EventRecord::param(0, ORGAN, id(ids::SEND), 0.2),
        // The steel guitar, to the right, through a chorus.
        EventRecord::param(0, GUITAR, id(ids::PLUCKED_PRESET), 1.0),
        EventRecord::param(0, GUITAR, id(ids::GAIN), 1.0),
        EventRecord::param(0, GUITAR, id(ids::PAN), 0.4),
        EventRecord::param(0, GUITAR, id(ids::CHORUS_MIX), 0.3),
        EventRecord::param(0, GUITAR, id(ids::CHORUS_ON), 1.0),
        EventRecord::param(0, GUITAR, id(ids::SEND), 0.25),
    ];
    let mut notes = Vec::new();
    drums(&mut notes);
    acid(&mut notes);
    organ(&mut notes);
    guitar(&mut notes);
    records.extend(
        notes
            .into_iter()
            .filter(|record| playing.contains(&record.part)),
    );
    for record in records {
        engine.push(record);
    }
    engine
}

/// Renders the song's eight bars and two seconds of tail, in quanta of
/// 128 frames as a browser does; also gives the most the master limiter
/// turned the level down, in decibels.
fn render(engine: &mut Engine) -> (Vec<f32>, Vec<f32>, f64) {
    let frames = at(BARS, 0.0) as usize + 2 * RATE as usize;
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
    (left, right, if reduction > 0.0 { reduction } else { 0.0 })
}

/// Writes `left` and `right` as a 16-bit stereo WAV file.
fn write_wav(path: &Path, left: &[f32], right: &[f32]) -> io::Result<()> {
    let data_bytes = u32::try_from(left.len() * 4).expect("a short file");
    let mut bytes = Vec::with_capacity(44 + left.len() * 4);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // format chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes()); // two channels
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes()); // bytes a second
    bytes.extend_from_slice(&4u16.to_le_bytes()); // bytes a frame
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for (&l, &r) in left.iter().zip(right) {
        for sample in [l, r] {
            let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    fs::write(path, bytes)
}

fn peak_db(samples: &[f32]) -> f64 {
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    20.0 * f64::from(peak).log10()
}

fn main() -> io::Result<()> {
    let directory = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;
    for (name, parts) in [
        ("aratu_song", &[DRUMS, ACID, ORGAN, GUITAR][..]),
        ("drums", &[DRUMS][..]),
        ("acid", &[ACID][..]),
        ("organ", &[ORGAN][..]),
        ("guitar", &[GUITAR][..]),
    ] {
        let mut engine = song(parts);
        let (left, right, reduction) = render(&mut engine);
        let path = directory.join(format!("{name}.wav"));
        write_wav(&path, &left, &right)?;
        let stats = engine.stats();
        println!(
            "{}: peak {:.1} dBFS, limited by up to {reduction:.1} dB, {} events, {} late",
            path.display(),
            peak_db(&left).max(peak_db(&right)),
            stats.applied,
            stats.late,
        );
    }
    Ok(())
}
