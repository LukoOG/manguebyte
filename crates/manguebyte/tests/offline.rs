//! Offline rendering is deterministic and does not depend on block size.
//!
//! A host may hand a plugin blocks of any length, and a different length on
//! every call. The sound must not depend on it: notes, automation and effect
//! tails have to land on the same frames however the timeline is cut up.
//! This renders the example song several ways and requires the results to be
//! identical, bit for bit.

#[path = "../examples/common/mod.rs"]
mod common;

use common::RiffPlayer;
use manguebyte::prelude::*;

const SAMPLE_RATE: f64 = 48_000.0;

/// Renders the whole song, asking for blocks of the lengths in `pattern`,
/// repeated for as long as it takes.
fn render(max_block_size: usize, pattern: &[usize]) -> AudioBuffer<f32> {
    let mut player = RiffPlayer::new(SAMPLE_RATE, 2, max_block_size);
    let frames = player.total_frames();
    let mut song = AudioBuffer::<f32>::new(2, frames);
    let mut position = 0;
    for &length in pattern.iter().cycle() {
        if position == frames {
            break;
        }
        let length = length.min(frames - position);
        song.windowed(position..position + length, |block| player.render(block));
        position += length;
    }
    assert!(player.is_finished());
    song
}

#[test]
fn the_song_is_the_same_whatever_the_block_size() {
    let reference = render(256, &[256]);
    assert!(reference.peak() > 0.1, "the reference render is silent");

    for (max_block_size, pattern) in [
        (64, &[64][..]),
        (1024, &[1024]),
        // Odd lengths that never line up with a note boundary.
        (997, &[997]),
        // A host that changes its mind on every call.
        (512, &[1, 511, 7, 300, 64, 2, 129]),
        // Blocks larger than the player's own, which it must split itself.
        (48, &[5_000]),
    ] {
        let other = render(max_block_size, pattern);
        assert!(
            other == reference,
            "blocks of {pattern:?} (max {max_block_size}) changed the output"
        );
    }
}

#[test]
fn two_renders_are_identical() {
    assert!(render(256, &[256]) == render(256, &[256]));
}

#[test]
fn the_song_contains_the_notes_that_were_asked_for() {
    use manguebyte::dsp::fft::{Complex, RealFft};
    use manguebyte::dsp::window::{Symmetry, Window};

    let song = render(256, &[256]);

    // The first half second holds an A minor chord over a low A.
    let len = 16_384;
    let mut slice: Vec<f32> = song.channel(0)[2_000..2_000 + len].to_vec();
    Window::Hann.apply(&mut slice, Symmetry::Periodic);
    let mut fft = RealFft::<f32>::new(len);
    let mut spectrum = vec![Complex::ZERO; fft.spectrum_len()];
    fft.forward(&slice, &mut spectrum);

    let level_at = |hz: f64| {
        let bin = (hz * len as f64 / SAMPLE_RATE).round() as usize;
        (bin - 1..=bin + 1)
            .map(|index| spectrum[index].norm())
            .fold(0.0f32, f32::max)
    };
    // Something no note in the song produces: between A2 and its octave.
    let floor = level_at(160.0);
    for key in [45.0, 57.0, 60.0, 64.0] {
        let level = level_at(note_to_hz(key));
        assert!(
            level > 8.0 * floor,
            "key {key}: level {level} against a floor of {floor}"
        );
    }
}
