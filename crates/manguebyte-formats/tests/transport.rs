//! `TransportSource`: a file pulled as a source, with a transport worked
//! from another thread.
//!
//! Most tests step the worker by hand on the test's thread, so what they
//! check is exact: which frames come out, where, and what is counted as
//! late. The last runs the worker on its own thread.

mod common;

use std::io::Cursor;
use std::time::{Duration, Instant};

use common::{Rng, ramp, sample_at};
use manguebyte_core::AudioBuffer;
use manguebyte_core::source::{AudioSource, PositionableSource};
use manguebyte_formats::wav::{self, WavFormat};
use manguebyte_formats::{
    AudioReader, PlayerConfig, PlayerWorker, TransportControl, TransportSource,
};

const SMALL: PlayerConfig = PlayerConfig {
    block_frames: 100,
    blocks: 4,
};

type Worker = PlayerWorker<Cursor<Vec<u8>>>;

fn transport(channels: usize, frames: usize) -> (TransportSource, Worker, TransportControl) {
    let mut bytes = Vec::new();
    wav::write(
        &mut bytes,
        &ramp(channels, frames),
        48_000,
        WavFormat::Float32,
    )
    .unwrap();
    let reader = AudioReader::new(Cursor::new(bytes)).unwrap();
    let (source, worker, control) = TransportSource::new(reader, SMALL);
    control.set_declick(false);
    (source, worker, control)
}

/// Pulls `frames` frames in blocks whose sizes come from `sizes`, stepping
/// the worker until it has nothing to do before each block, and returns
/// channel `channel` of what came out.
fn play(
    source: &mut TransportSource,
    worker: &mut Worker,
    sizes: &mut impl FnMut() -> usize,
    frames: usize,
    channel: usize,
) -> Vec<f32> {
    let mut out = AudioBuffer::new(2, 512);
    let mut heard = Vec::with_capacity(frames);
    while heard.len() < frames {
        worker.fill_ahead().unwrap();
        out.set_num_frames(sizes().min(frames - heard.len()));
        source.pull(&mut out);
        heard.extend_from_slice(out.channel(channel));
    }
    heard
}

fn expected(frames: impl IntoIterator<Item = u64>, channel: usize) -> Vec<f32> {
    frames
        .into_iter()
        .map(|frame| sample_at(channel, frame))
        .collect()
}

#[test]
fn nothing_plays_until_it_is_told_to() {
    let (mut source, mut worker, control) = transport(2, 1_000);
    worker.fill_ahead().unwrap();
    let mut out = AudioBuffer::new(2, 64);
    out.channel_mut(0).fill(9.0);
    source.pull(&mut out);
    assert_eq!(out.peak(), 0.0);
    assert!(!control.is_playing());
    assert_eq!(control.position(), 0);
    control.play();
    source.pull(&mut out);
    assert_eq!(out.channel(1), &expected(0..64, 1)[..]);
    assert!(control.is_playing());
    assert_eq!(
        (control.len_frames(), control.num_channels()),
        (Some(1_000), 2)
    );
    assert_eq!(control.sample_rate(), 48_000);
}

#[test]
fn a_seek_lands_on_the_exact_frame_whatever_the_blocks() {
    let mut rng = Rng::new(7);
    for _ in 0..40 {
        let (mut source, mut worker, control) = transport(2, 5_000);
        control.play();
        let target = rng.below(5_000);
        // Some playing first, then the seek, from the control.
        let before = rng.below(700) as usize;
        let mut sizes = || rng.between(1, 300) as usize;
        let heard = play(&mut source, &mut worker, &mut sizes, before, 0);
        assert_eq!(heard, expected(0..before as u64, 0));
        control.seek(target);
        // The pull that hears of the seek plays silence until the worker
        // has read from there, and that is not an underrun.
        let mut out = AudioBuffer::new(2, 50);
        source.pull(&mut out);
        assert_eq!(out.peak(), 0.0);
        assert!(control.is_seeking());
        let wanted = 300.min(5_000 - target as usize);
        let mut sizes = || rng.between(1, 300) as usize;
        let heard = play(&mut source, &mut worker, &mut sizes, wanted, 1);
        assert_eq!(
            heard,
            expected(target..target + wanted as u64, 1),
            "seek to {target}"
        );
        assert_eq!(control.underrun_frames(), 0);
        assert_eq!(control.position(), target + wanted as u64);
    }
}

#[test]
fn a_loop_goes_round_without_a_gap() {
    let mut rng = Rng::new(11);
    for (start, end) in [
        (0, 1_000),
        (100, 357),
        (999, 1_000),
        (250, 251),
        (0, u64::MAX),
    ] {
        let (mut source, mut worker, control) = transport(2, 1_000);
        control.set_loop(Some(start..end));
        control.seek(start);
        control.play();
        let mut out = AudioBuffer::new(2, 1);
        source.pull(&mut out);
        // Blocks no longer than what three blocks of read-ahead hold, so
        // that the worker, stepped before each, is never late.
        let mut sizes = || rng.between(1, 300) as usize;
        let heard = play(&mut source, &mut worker, &mut sizes, 5_000, 0);
        let end = end.min(1_000);
        let length = end - start;
        let wanted = expected((0..5_000).map(|n| start + n % length), 0);
        assert_eq!(heard, wanted, "loop {start}..{end}");
        assert_eq!(control.underrun_frames(), 0);
        assert!(control.is_playing() && !control.is_finished());
        assert!(source.is_looping());
        // Stopping the loop plays on to the end, then stops by itself.
        control.set_loop(None);
        let mut sizes = || 256;
        let heard = play(&mut source, &mut worker, &mut sizes, 2_000, 0);
        assert_eq!(heard[1_500..], [0.0; 500]);
        assert!(!control.is_playing() && control.is_finished());
    }
}

#[test]
fn pause_keeps_the_place_and_play_goes_on_from_it() {
    let (mut source, mut worker, control) = transport(2, 3_000);
    control.play();
    let mut sizes = || 64;
    let first = play(&mut source, &mut worker, &mut sizes, 640, 0);
    control.pause();
    let mut out = AudioBuffer::new(2, 64);
    for _ in 0..10 {
        worker.fill_ahead().unwrap();
        source.pull(&mut out);
        assert_eq!(out.peak(), 0.0);
    }
    assert_eq!(control.position(), 640);
    assert!(!control.is_playing());
    control.play();
    let second = play(&mut source, &mut worker, &mut sizes, 640, 0);
    assert_eq!([first, second].concat(), expected(0..1_280, 0));

    // Stop goes back to the start.
    control.stop();
    source.pull(&mut out);
    assert_eq!(control.position(), 0);
    control.play();
    let again = play(&mut source, &mut worker, &mut sizes, 64, 0);
    assert_eq!(again, expected(0..64, 0));
}

#[test]
fn declicking_fades_in_and_out_over_a_block_and_glides_the_gain() {
    let (mut source, mut worker, control) = transport(1, 3_000);
    control.set_declick(true);
    control.play();
    worker.fill_ahead().unwrap();
    let mut out = AudioBuffer::new(1, 100);
    source.pull(&mut out);
    // A fade in from silence over the first block.
    for (index, &sample) in out.channel(0).iter().enumerate() {
        let gain = index as f32 / 100.0;
        assert!((sample - sample_at(0, index as u64) * gain).abs() < 1e-6);
    }
    source.pull(&mut out);
    assert_eq!(out.channel(0), &expected(100..200, 0)[..]);
    control.set_gain(0.5);
    worker.fill_ahead().unwrap();
    source.pull(&mut out);
    let last = out.channel(0)[99];
    assert!((last - sample_at(0, 299) * (1.0 - 0.5 * 99.0 / 100.0)).abs() < 1e-6);
    assert_eq!(control.gain(), 0.5);
    // A pause fades out over one more block, which the position counts.
    control.pause();
    worker.fill_ahead().unwrap();
    source.pull(&mut out);
    for (index, &sample) in out.channel(0).iter().enumerate() {
        let gain = 0.5 * (1.0 - index as f32 / 100.0);
        assert!((sample - sample_at(0, 300 + index as u64) * gain).abs() < 1e-6);
    }
    assert_eq!(control.position(), 400);
    assert!(!control.is_playing());
    source.pull(&mut out);
    assert_eq!(out.peak(), 0.0);
}

#[test]
fn a_late_worker_is_an_underrun_and_nothing_is_skipped() {
    let (mut source, mut worker, control) = transport(2, 4_000);
    control.play();
    let mut out = AudioBuffer::new(2, 100);
    // The first block arrives, then the worker stalls for three pulls.
    worker.step().unwrap();
    source.pull(&mut out);
    let mut heard = out.channel(0).to_vec();
    for _ in 0..3 {
        source.pull(&mut out);
        assert_eq!(out.peak(), 0.0);
    }
    assert_eq!(control.underrun_frames(), 300);
    assert_eq!(control.underruns(), 1);
    // The worker catches up and playback carries on from frame 100.
    let mut sizes = || 100;
    heard.extend(play(&mut source, &mut worker, &mut sizes, 500, 0));
    assert_eq!(heard, expected(0..600, 0));
    assert_eq!(control.position(), 600);
    assert_eq!(control.underruns(), 1);
}

#[test]
fn play_after_the_end_starts_again_and_the_source_is_positionable() {
    let (mut source, mut worker, control) = transport(1, 150);
    control.play();
    let mut sizes = || 100;
    let heard = play(&mut source, &mut worker, &mut sizes, 200, 0);
    assert_eq!(heard[..150], expected(0..150, 0)[..]);
    assert!(control.is_finished() && !control.is_playing());
    source.play();
    let again = play(&mut source, &mut worker, &mut sizes, 100, 0);
    assert_eq!(again, expected(0..100, 0));
    // Through the trait, on the audio thread.
    PositionableSource::seek(&mut source, 20);
    assert_eq!(source.position(), 20);
    assert_eq!(source.len_frames(), Some(150));
    let mut out = AudioBuffer::new(1, 10);
    worker.fill_ahead().unwrap();
    source.pull(&mut out);
    assert_eq!(out.channel(0), &expected(20..30, 0)[..]);
}

#[test]
fn a_control_on_another_thread_and_a_worker_on_its_own() {
    let (mut source, worker, control) = transport(2, 48_000);
    let thread = worker.spawn().unwrap();
    let remote = control.clone();
    let ui = std::thread::spawn(move || {
        remote.seek(10_000);
        remote.play();
    });
    ui.join().unwrap();
    let mut out = AudioBuffer::new(2, 256);
    let mut heard = Vec::new();
    let started = Instant::now();
    while heard.len() < 4_096 {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "nothing arrived"
        );
        source.pull(&mut out);
        if !control.is_seeking() {
            heard.extend_from_slice(out.channel(0));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // Whatever the timing, what came out after the seek is the file from
    // frame 10 000, with silence only where the worker was late.
    let late = control.underrun_frames() as usize;
    let played: Vec<f32> = heard
        .iter()
        .copied()
        .filter(|sample| *sample != 0.0)
        .collect();
    assert!(heard.len() - played.len() <= late + 256);
    let first = expected(10_000..10_000 + played.len() as u64, 0);
    assert_eq!(
        played[..played.len().min(1_000)],
        first[..played.len().min(1_000)]
    );
    thread.stop().unwrap();
}
