//! Renders every drum of the kit, plain and accented, and a short groove
//! with the toms and the side stick, to 16-bit 48 kHz mono WAV files, to
//! listen to them.
//!
//! ```text
//! cargo run -p manguebyte-web --example drum_wavs -- <directory>
//! ```
//!
//! Each drum is struck once at full velocity through no effect chain, so
//! that what is heard is the drum itself: `<name>.wav`, then
//! `<name>_accent.wav`. `groove.wav` is two bars at 100 beats a minute.

use std::fs;
use std::io;
use std::path::Path;

use manguebyte_web::{Chain, DRUM_MAP, Drum, Engine, EngineConfig, EventRecord, InstrumentKind};

const RATE: u32 = 48_000;

fn engine() -> Engine {
    let mut engine = Engine::new(EngineConfig::new(f64::from(RATE))).expect("engine");
    engine
        .create_part_with(0, InstrumentKind::Drums, Chain::NONE)
        .expect("part");
    engine
}

/// Renders `frames` frames and returns the left channel; the part is in
/// the middle, so the right one is the same.
fn render(engine: &mut Engine, frames: usize) -> Vec<f32> {
    let mut left = Vec::with_capacity(frames);
    while left.len() < frames {
        let count = engine.render((frames - left.len()).min(128));
        left.extend_from_slice(&engine.output().0[..count]);
    }
    left
}

/// Writes `samples` as a 16-bit mono WAV file.
fn write_wav(path: &Path, samples: &[f32]) -> io::Result<()> {
    let data_bytes = u32::try_from(samples.len() * 2).expect("a short file");
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // format chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // one channel
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes a second
    bytes.extend_from_slice(&2u16.to_le_bytes()); // bytes a frame
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    fs::write(path, bytes)
}

/// Two bars of a tom groove, as Aratu would write it, capitals accented:
/// kick and hats under a side-stick backbeat, then a fill down the toms.
fn groove() -> Vec<f32> {
    let lines = [
        "B.......b.b.....B.......b.......",
        "c.c.c.c.c.c.c.c.c.c.c.c.........",
        "....k.......k.......k...........",
        "........................TttMmmFf",
        "..............................X.",
    ];
    let sixteenth = u64::from(RATE) * 60 / 100 / 4;
    let mut engine = engine();
    for line in lines {
        for (step, symbol) in line.chars().enumerate() {
            let Some(drum) = Drum::from_symbol(symbol.to_ascii_lowercase()) else {
                continue;
            };
            let record = EventRecord::note_on(step as u64 * sixteenth, 0, drum.note(), 0.7)
                .with_accent(symbol.is_ascii_uppercase());
            engine.push(record);
        }
    }
    render(&mut engine, (34 * sixteenth) as usize + RATE as usize)
}

fn main() -> io::Result<()> {
    let directory = std::env::args()
        .nth(1)
        .expect("usage: drum_wavs <directory>");
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;
    for entry in DRUM_MAP {
        // Long enough for the drum to die away, at most three seconds.
        let seconds = match entry.drum {
            Drum::Ride | Drum::Crash => 3.0,
            Drum::OpenHat | Drum::FloorTom => 1.2,
            _ => 1.0,
        };
        for accent in [false, true] {
            let mut engine = engine();
            engine.push(EventRecord::note_on(0, 0, entry.note, 1.0).with_accent(accent));
            let samples = render(&mut engine, (seconds * f64::from(RATE)) as usize);
            let name = if accent {
                format!("{}_accent.wav", entry.name)
            } else {
                format!("{}.wav", entry.name)
            };
            write_wav(&directory.join(&name), &samples)?;
            println!("{name}");
        }
    }
    write_wav(&directory.join("groove.wav"), &groove())?;
    println!("groove.wav");
    Ok(())
}
