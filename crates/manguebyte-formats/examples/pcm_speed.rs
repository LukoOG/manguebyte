//! Measures how fast WAV files are written and read, as a multiple of real
//! time.
//!
//! ```text
//! cargo run --release -p manguebyte-formats --example pcm_speed
//! ```
//!
//! Ten seconds of noise at 48 kHz, in one, two and six channels, are
//! written to memory as 16-bit, 24-bit and float WAV, read back whole, and
//! read through [`AudioReader`](manguebyte_formats::AudioReader) in blocks
//! of 512 frames. Each figure is the best of several runs. The conversions
//! between samples and bytes are most of the work, so this is what the
//! vector conversions of `manguebyte_core::vector` speed up.

use std::hint::black_box;
use std::io::Cursor;
use std::time::Instant;

use manguebyte_core::AudioBuffer;
use manguebyte_formats::AudioReader;
use manguebyte_formats::wav::{self, WavFormat};

const RATE: u32 = 48_000;
const SECONDS: f64 = 10.0;

/// The best of several runs of `work`, in seconds.
fn best(mut work: impl FnMut()) -> f64 {
    (0..7)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed().as_secs_f64()
        })
        .fold(f64::INFINITY, f64::min)
}

fn noise(channels: usize) -> AudioBuffer<f32> {
    let frames = (f64::from(RATE) * SECONDS) as usize;
    let mut buffer = AudioBuffer::new(channels, frames);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for channel in buffer.iter_channels_mut() {
        for sample in channel {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *sample = ((state >> 40) as f32 / (1u64 << 24) as f32) * 1.8 - 0.9;
        }
    }
    buffer
}

fn main() {
    println!("| channels | format | write | read whole | read in blocks |");
    println!("|---:|---|---:|---:|---:|");
    for channels in [1, 2, 6] {
        let buffer = noise(channels);
        for (format, name) in [
            (WavFormat::Int16, "16-bit"),
            (WavFormat::Int24, "24-bit"),
            (WavFormat::Float32, "float"),
        ] {
            let mut bytes = Vec::new();
            wav::write(&mut bytes, &buffer, RATE, format).unwrap();
            let write = best(|| {
                let mut out = Vec::with_capacity(bytes.len());
                wav::write(&mut out, black_box(&buffer), RATE, format).unwrap();
                black_box(out);
            });
            let read = best(|| {
                black_box(wav::read(Cursor::new(black_box(&bytes))).unwrap());
            });
            let mut block = AudioBuffer::<f32>::new(channels, 512);
            let blocks = best(|| {
                let mut reader = AudioReader::new(Cursor::new(black_box(&bytes))).unwrap();
                while reader.read(&mut block).unwrap() > 0 {}
                black_box(&block);
            });
            println!(
                "| {channels} | {name} | {:.0}x | {:.0}x | {:.0}x |",
                SECONDS / write,
                SECONDS / read,
                SECONDS / blocks
            );
        }
    }
}
