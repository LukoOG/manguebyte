//! Helpers shared by the bridge's tests.

#![allow(dead_code)]

use std::time::Duration;

use aratu_bridge::AratuBridge;
use aratu_bridge::client::Client;
use manguebyte::prelude::*;

pub const RATE: f64 = 48_000.0;

/// An origin the plugin allows by default.
pub const ORIGIN: &str = "http://localhost:5173";

/// Makes the plugin listen on a free port, and returns it.
pub fn listen(harness: &mut Harness<AratuBridge>) -> u16 {
    let plugin = harness.plugin_mut();
    plugin.set_port_override(Some(0));
    plugin
        .wait_listening(Duration::from_secs(10))
        .expect("the plugin listens")
}

/// Connects a client and says hello.
pub fn connect(harness: &mut Harness<AratuBridge>) -> Client {
    let port = listen(harness);
    let mut client = Client::connect(port, ORIGIN).expect("the plugin accepts");
    client.hello().expect("the plugin says hello");
    client
}

/// A 16-bit mono WAV file of `samples` at `rate`.
pub fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let mut buffer = AudioBuffer::<f32>::new(1, samples.len());
    buffer.channel_mut(0).copy_from_slice(samples);
    let mut bytes = Vec::new();
    manguebyte_formats::wav::write(
        &mut bytes,
        &buffer,
        rate,
        manguebyte_formats::wav::WavFormat::Int16,
    )
    .unwrap();
    bytes
}

/// `frames` frames of a sine at `frequency`.
pub fn sine(frequency: f64, rate: f64, frames: usize, amplitude: f64) -> Vec<f32> {
    (0..frames)
        .map(|n| (amplitude * (core::f64::consts::TAU * frequency * n as f64 / rate).sin()) as f32)
        .collect()
}

/// A melodic SFZ instrument: an A4 at 24 kHz, looped, and its files.
pub fn tone_instrument() -> (String, Vec<(String, Vec<u8>)>) {
    let low = wav16(&sine(440.0, 24_000.0, 24_000, 0.8), 24_000);
    let sfz = "<control> default_path=./
        <global> ampeg_release=0.05
        <region> sample=a4.wav pitch_keycenter=69 lokey=0 hikey=127 loop_mode=loop_continuous loop_start=12000 loop_end=17999";
    (sfz.to_owned(), vec![("a4.wav".to_owned(), low)])
}

/// The peak of both channels.
pub fn peak(buffer: &AudioBuffer<f32>) -> f32 {
    buffer
        .iter_channels()
        .flat_map(|channel| channel.iter())
        .fold(0.0, |peak: f32, sample| peak.max(sample.abs()))
}

/// The first frame two outputs differ on, by bits.
pub fn first_difference(a: &[f32], b: &[f32]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    a.iter()
        .zip(b)
        .position(|(x, y)| x.to_bits() != y.to_bits())
}
