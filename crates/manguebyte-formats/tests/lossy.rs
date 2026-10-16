//! The sources of the lossy test files, and how fast the lossy decoders
//! are.

mod common;

use std::time::Instant;

use common::lossy::{SOURCES, source};
use manguebyte_formats::decode;
use manguebyte_formats::wav::{self, WavFormat};

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!("data/", $name)).as_slice()
    };
}

/// Writes the audio the lossy files in `data/` were encoded from, as 32-bit
/// float WAV, into the directory `MANGUEBYTE_DUMP_DIR` names:
///
/// ```text
/// MANGUEBYTE_DUMP_DIR=/some/dir cargo test -p manguebyte-formats --test lossy -- --ignored
/// ```
///
/// `data/README.md` says how each file was encoded from them.
#[test]
#[ignore = "writes the sources of the lossy test files"]
fn write_the_sources_of_the_lossy_files() {
    let directory = std::env::var("MANGUEBYTE_DUMP_DIR").expect("set MANGUEBYTE_DUMP_DIR");
    let directory = std::path::Path::new(&directory);
    for name in SOURCES {
        let (buffer, rate) = source(name);
        let path = directory.join(format!("{name}.wav"));
        wav::write_file(path, &buffer, rate, WavFormat::Float32).unwrap();
    }
}

/// How fast the decoders are, as a multiple of real time. Printed, and held
/// only to real time itself: a debug build on a busy machine is many times
/// slower than a release build, and the figures in the documentation come
/// from `cargo run --release -p manguebyte-formats --example decode_speed`.
#[test]
fn decoding_is_faster_than_real_time() {
    for (name, bytes) in [
        ("Vorbis, 44.1 kHz stereo", data!("vorbis-2ch-44100-q3.ogg")),
        ("Vorbis, 48 kHz 5.1", data!("vorbis-6ch-48000-q2.ogg")),
    ] {
        let started = Instant::now();
        let file = decode(bytes).unwrap();
        let seconds = started.elapsed().as_secs_f64();
        let audio = file.buffer.num_frames() as f64 / f64::from(file.sample_rate);
        println!("{name}: {:.1} times real time", audio / seconds);
        assert!(audio / seconds > 1.0, "{name}");
    }
}
