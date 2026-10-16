//! Measures how fast the lossy decoders are, as a multiple of real time.
//!
//! ```text
//! cargo run --release -p manguebyte-formats --example decode_speed
//! ```
//!
//! Each file of the tests is decoded whole, through
//! [`AudioReader`](manguebyte_formats::AudioReader) in blocks of 1024
//! frames, enough times to take a second or so, and the best time counts.

use std::io::Cursor;
use std::time::Instant;

use manguebyte_core::AudioBuffer;
use manguebyte_formats::{AudioReader, FormatError};

macro_rules! data {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!("../tests/data/", $name)).as_slice(),
        )
    };
}

/// Decodes `bytes` to the end and returns how many seconds of audio that
/// was.
fn decode(bytes: &[u8]) -> Result<f64, FormatError> {
    let mut reader = AudioReader::new(Cursor::new(bytes))?;
    let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 1_024);
    let mut frames = 0;
    loop {
        let got = reader.read(&mut block)?;
        if got == 0 {
            break;
        }
        frames += got;
    }
    Ok(frames as f64 / f64::from(reader.sample_rate()))
}

fn main() -> Result<(), FormatError> {
    let files = [
        data!("vorbis-2ch-44100-q3.ogg"),
        data!("vorbis-6ch-48000-q2.ogg"),
        data!("vorbis-tones-2ch-48000-q10.ogg"),
        data!("vorbis-1ch-8000-bursts.ogg"),
    ];
    println!("{:<34} {:>10} {:>12}", "file", "audio (s)", "x real time");
    for (name, bytes) in files {
        let mut best = f64::MAX;
        let mut audio = 0.0;
        let started = Instant::now();
        while started.elapsed().as_secs_f64() < 1.0 || best == f64::MAX {
            let run = Instant::now();
            audio = decode(bytes)?;
            best = best.min(run.elapsed().as_secs_f64());
        }
        println!("{name:<34} {audio:>10.2} {:>12.0}", audio / best);
    }
    Ok(())
}
