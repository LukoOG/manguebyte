//! Renders a short synth riff to a WAV file, without touching any audio device.
//!
//! Run with: `cargo run --example render_wav -- riff.wav`

mod common;

use common::RiffPlayer;
use manguebyte::prelude::*;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_SIZE: usize = 256;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("manguebyte-riff.wav"));

    let mut player = RiffPlayer::new(f64::from(SAMPLE_RATE), 2, BLOCK_SIZE);
    let mut song = AudioBuffer::<f32>::new(2, player.total_frames());
    player.render(&mut song);

    wav::write_file(&path, &song, SAMPLE_RATE, WavFormat::Int16)?;
    println!(
        "wrote {path}: {:.2} s, peak {:.1} dBFS",
        song.num_frames() as f64 / f64::from(SAMPLE_RATE),
        decibels::gain_to_db(f64::from(song.peak())),
    );
    Ok(())
}
