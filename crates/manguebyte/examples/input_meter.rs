//! Shows the level of the default audio input as a text meter.
//!
//! ```text
//! cargo run --example input_meter           # five seconds
//! cargo run --example input_meter -- 30     # thirty seconds
//! ```
//!
//! The audio callback only measures the peak of each block and publishes it
//! through an atomic, which is how values should leave the audio thread.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;

use manguebyte::devices::{DeviceError, InputDevice};
use manguebyte::{AtomicF32, decibels};

fn main() -> Result<(), DeviceError> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5);

    let device = InputDevice::open_default()?;
    println!(
        "Listening to {} ({} Hz, {} channels)",
        device.name(),
        device.sample_rate(),
        device.num_channels()
    );

    let peak = Arc::new(AtomicF32::new(0.0));
    let shared = Arc::clone(&peak);
    let _stream = device.start(512, move |buffer| {
        // Keep the loudest block seen since the meter last looked.
        let held = shared.load(Ordering::Relaxed);
        shared.store(held.max(buffer.peak()), Ordering::Relaxed);
    })?;

    for _ in 0..seconds * 10 {
        sleep(Duration::from_millis(100));
        let level = decibels::gain_to_db(f64::from(peak.swap(0.0, Ordering::Relaxed)));
        let bars = ((level + 60.0).clamp(0.0, 60.0) / 2.0) as usize;
        println!("{level:7.1} dB |{:<30}|", "#".repeat(bars));
    }
    Ok(())
}
