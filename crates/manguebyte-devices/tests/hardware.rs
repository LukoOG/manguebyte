//! Tests that need real audio hardware.
//!
//! They are ignored by default because CI machines have no sound card. Run
//! them on a machine that does with:
//!
//! ```text
//! cargo test -p manguebyte-devices --test hardware -- --ignored --test-threads 1
//! ```
//!
//! Nothing audible is played: every output callback leaves its buffer silent.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

use manguebyte_devices::{
    DuplexStream, InputDevice, OutputDevice, StreamSettings, input_devices, output_devices,
};

#[test]
#[ignore = "needs an audio output device"]
fn lists_output_devices_and_marks_one_as_default() {
    let devices = output_devices().unwrap();
    for device in &devices {
        println!("{device:?}");
    }
    assert_eq!(devices.iter().filter(|d| d.is_default).count(), 1);
    assert!(devices.iter().all(|d| !d.id.is_empty()));
}

#[test]
#[ignore = "needs an audio output device"]
fn output_runs_and_can_be_reopened_by_id() {
    let default = OutputDevice::open_default().unwrap();
    let device = OutputDevice::open_by_id(default.id()).unwrap();
    assert_eq!(device.name(), default.name());

    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let stream = device
        .start(256, move |buffer| {
            assert!(buffer.num_frames() <= 256);
            counter.fetch_add(buffer.num_frames(), Ordering::Relaxed);
        })
        .unwrap();
    sleep(Duration::from_millis(400));
    drop(stream);

    let rendered = frames.load(Ordering::Relaxed) as f64;
    assert!(
        rendered > device.sample_rate() * 0.2,
        "only {rendered} frames in 400 ms"
    );
}

#[test]
#[ignore = "needs an audio output device"]
fn impossible_settings_are_refused() {
    let device = OutputDevice::open_default().unwrap();
    let result = device.with_settings(StreamSettings::default().with_sample_rate(1));
    assert!(result.is_err());
}

#[test]
#[ignore = "needs an audio input device"]
fn input_delivers_blocks() {
    println!("{:?}", input_devices().unwrap());
    let device = InputDevice::open_default().unwrap();
    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let stream = device
        .start(256, move |buffer| {
            counter.fetch_add(buffer.num_frames(), Ordering::Relaxed);
        })
        .unwrap();
    sleep(Duration::from_millis(400));
    drop(stream);
    assert!(frames.load(Ordering::Relaxed) as f64 > device.sample_rate() * 0.2);
}

#[test]
#[ignore = "needs audio input and output devices"]
fn duplex_carries_input_to_the_output_callback() {
    let output = OutputDevice::open_default().unwrap();
    let input = InputDevice::open_default()
        .unwrap()
        .with_settings(StreamSettings::default().with_sample_rate(output.sample_rate() as u32))
        .unwrap();

    let frames = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&frames);
    let stream = DuplexStream::start(&input, &output, 256, move |input, output| {
        assert_eq!(input.num_frames(), output.num_frames());
        counter.fetch_add(input.num_frames(), Ordering::Relaxed);
    })
    .unwrap();
    sleep(Duration::from_millis(1_000));
    let (underruns, overruns) = (stream.underruns(), stream.overruns());
    drop(stream);

    println!("underruns: {underruns}, overruns: {overruns}");
    assert!(frames.load(Ordering::Relaxed) as f64 > output.sample_rate() * 0.5);
    assert!(
        underruns < 20,
        "the reserve never settled: {underruns} underruns"
    );
}
