//! Plays a short synth riff on the default audio output device.
//!
//! Run with: `cargo run --example play_riff`
//!
//! Pass a level in decibels to turn it down: `cargo run --example play_riff -- -12`

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::RiffPlayer;
use manguebyte::prelude::*;

const BLOCK_SIZE: usize = 512;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let trim_db: f64 = match std::env::args().nth(1) {
        Some(arg) => arg.parse()?,
        None => 0.0,
    };
    let trim = decibels::db_to_gain(trim_db.min(0.0)) as f32;

    let device = OutputDevice::open_default()?;
    println!(
        "playing on {} ({} Hz, {} channels)",
        device.name(),
        device.sample_rate(),
        device.num_channels()
    );

    let mut player = RiffPlayer::new(device.sample_rate(), device.num_channels(), BLOCK_SIZE);
    let finished = Arc::new(AtomicBool::new(false));
    let finished_in_callback = Arc::clone(&finished);

    let _stream = device.start(BLOCK_SIZE, move |buffer| {
        if player.is_finished() {
            finished_in_callback.store(true, Ordering::Relaxed);
        } else {
            player.render(buffer);
            buffer.apply_gain(trim);
        }
    })?;

    while !finished.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
