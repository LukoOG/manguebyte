//! The bridges between the audio thread and the disk.
//!
//! Most of these tests run both halves of a bridge on one thread, stepping
//! the worker by hand. Nothing in them depends on timing, so what they
//! assert is exact: which frames are played, how many are counted as late
//! or lost, where playback is.
//!
//! The tests at the end run the worker on a real thread. They make no
//! assumption about how fast either thread runs: they wait for progress and
//! check what arrives, so a slow machine makes them slower, not wrong.

mod common;

use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{
    Failing, FlacSpec, FullDisk, Rng, flac_sample, flac_scale, ramp, sample_at, slice, write_flac,
};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::aiff::AiffFormat;
use manguebyte_formats::wav::{self, WavFormat};
use manguebyte_formats::{
    AudioReader, AudioWriter, FormatError, PlayerConfig, RecorderConfig, RecordingWriter,
    StreamingPlayer, decode,
};

/// A value the tests put in an output buffer before it is filled, to see
/// that every frame of it was written.
const STALE: f32 = 99.0;

/// How long a threaded test waits for progress before it gives up. This is
/// a guard against hanging for ever, not a deadline anything is expected to
/// come near.
const PATIENCE: Duration = Duration::from_secs(120);

/// Small blocks, so that a short file crosses many of them.
const SMALL: PlayerConfig = PlayerConfig {
    block_frames: 100,
    blocks: 4,
};

/// A float WAV file in memory whose frame `n` holds `sample_at(channel, n)`.
fn wav_bytes(channels: usize, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    wav::write(
        &mut bytes,
        &ramp(channels, frames),
        48_000,
        WavFormat::Float32,
    )
    .unwrap();
    bytes
}

fn wav_reader(channels: usize, frames: usize) -> AudioReader<Cursor<Vec<u8>>> {
    AudioReader::new(Cursor::new(wav_bytes(channels, frames))).unwrap()
}

/// Asks the player for `frames` frames and checks that whatever it did not
/// fill with audio is silence. Returns how many frames of audio came.
fn play(player: &mut StreamingPlayer, out: &mut AudioBuffer<f32>, frames: usize) -> usize {
    out.set_num_frames(frames);
    for channel in out.iter_channels_mut() {
        channel.fill(STALE);
    }
    let played = player.process(out);
    assert!(played <= frames);
    for channel in out.iter_channels() {
        assert!(
            channel[played..].iter().all(|&sample| sample == 0.0),
            "what follows the audio is not silence"
        );
    }
    played
}

/// Checks that the first `count` frames of `out` are the file's frames from
/// `position` on.
#[track_caller]
fn assert_plays(out: &AudioBuffer<f32>, count: usize, position: u64) {
    for (channel, samples) in out.iter_channels().enumerate() {
        for (offset, &sample) in samples[..count].iter().enumerate() {
            let frame = position + offset as u64;
            assert!(
                sample == sample_at(channel, frame),
                "channel {channel}: expected frame {frame} at offset {offset}"
            );
        }
    }
}

#[test]
fn nothing_plays_before_the_worker_reads_and_that_is_not_an_underrun() {
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, 1_000), SMALL);
    assert_eq!(player.num_channels(), 2);
    assert_eq!(player.sample_rate(), 48_000);
    assert_eq!(player.len_frames(), Some(1_000));
    let mut out = AudioBuffer::<f32>::new(2, 256);

    for _ in 0..3 {
        assert_eq!(play(&mut player, &mut out, 256), 0);
    }
    assert!(player.is_seeking());
    assert!(!player.is_finished());
    assert_eq!(player.position(), 0);
    assert_eq!(player.buffered_frames(), 0);
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));

    // One step reads one block.
    assert!(worker.step().unwrap());
    assert_eq!(player.buffered_frames(), 100);
    assert_eq!(play(&mut player, &mut out, 60), 60);
    assert_plays(&out, 60, 0);
    assert!(!player.is_seeking());
    assert_eq!(player.position(), 60);
    assert_eq!(player.buffered_frames(), 40);

    // The read-ahead fills up and then there is nothing to do.
    assert_eq!(worker.fill_ahead().unwrap(), 3);
    assert!(!worker.step().unwrap());
    assert_eq!(player.buffered_frames(), 340);
}

#[test]
fn the_file_is_played_exactly_whatever_the_block_sizes() {
    for (channels, frames) in [(1usize, 0usize), (1, 1), (2, 100), (2, 9_999), (3, 10_000)] {
        let (mut player, mut worker) = StreamingPlayer::new(wav_reader(channels, frames), SMALL);
        let mut out = AudioBuffer::<f32>::new(channels, 300);
        let mut rng = Rng::new(frames as u64 + 1);
        let mut position = 0u64;
        while !player.is_finished() {
            worker.fill_ahead().unwrap();
            // Never more than the three blocks that are always ahead.
            let size = rng.between(0, 300) as usize;
            let played = play(&mut player, &mut out, size);
            assert_plays(&out, played, position);
            position += played as u64;
            assert_eq!(player.position(), position);
            // Short only at the end of the file.
            assert!(played == size || position == frames as u64);
        }
        assert_eq!(position, frames as u64);

        // After the end: silence, and no complaint about the worker.
        for _ in 0..3 {
            assert_eq!(play(&mut player, &mut out, 300), 0);
            worker.fill_ahead().unwrap();
        }
        assert!(player.is_finished());
        assert_eq!(player.position(), frames as u64);
        assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));
    }
}

#[test]
fn a_late_worker_is_counted_and_nothing_is_skipped() {
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, 1_000), SMALL);
    let mut out = AudioBuffer::<f32>::new(2, 64);

    // One block of 100 frames, then the worker stalls.
    assert!(worker.step().unwrap());
    assert_eq!(play(&mut player, &mut out, 64), 64);
    assert_eq!(play(&mut player, &mut out, 64), 36);
    assert_plays(&out, 36, 64);
    assert_eq!((player.underruns(), player.underrun_frames()), (1, 28));
    assert_eq!(player.position(), 100);

    // Still stalled: more silence, but it is the same underrun.
    assert_eq!(play(&mut player, &mut out, 64), 0);
    assert_eq!(play(&mut player, &mut out, 0), 0);
    assert_eq!((player.underruns(), player.underrun_frames()), (1, 92));

    // The worker catches up. Playback carries on from frame 100: the audio
    // is late, not lost.
    assert_eq!(worker.fill_ahead().unwrap(), 4);
    assert_eq!(play(&mut player, &mut out, 64), 64);
    assert_plays(&out, 64, 100);
    assert_eq!((player.underruns(), player.underrun_frames()), (1, 92));

    // Running dry a second time is a second underrun.
    for position in [164u64, 228, 292, 356, 420] {
        assert_eq!(play(&mut player, &mut out, 64), 64);
        assert_plays(&out, 64, position);
    }
    assert_eq!(play(&mut player, &mut out, 64), 16);
    assert_plays(&out, 16, 484);
    assert_eq!((player.underruns(), player.underrun_frames()), (2, 140));

    // To the end, where silence is no longer anybody's fault.
    let mut position = 500;
    while !player.is_finished() {
        worker.fill_ahead().unwrap();
        let played = play(&mut player, &mut out, 64);
        assert_plays(&out, played, position);
        position += played as u64;
    }
    assert_eq!(position, 1_000);
    assert_eq!(play(&mut player, &mut out, 64), 0);
    assert_eq!((player.underruns(), player.underrun_frames()), (2, 140));
}

#[test]
fn a_seek_throws_away_the_read_ahead_and_plays_from_the_new_place() {
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, 10_000), SMALL);
    let mut out = AudioBuffer::<f32>::new(2, 250);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 150), 150);

    // The position moves at once; the audio follows when the worker has
    // been there. The wait is not an underrun.
    player.seek(5_000);
    assert_eq!(player.position(), 5_000);
    assert!(player.is_seeking());
    assert_eq!(player.buffered_frames(), 0);
    assert_eq!(play(&mut player, &mut out, 250), 0);
    assert_eq!(player.underrun_frames(), 0);

    assert_eq!(worker.fill_ahead().unwrap(), 4);
    assert_eq!(play(&mut player, &mut out, 250), 250);
    assert_plays(&out, 250, 5_000);
    assert_eq!(player.position(), 5_250);
    assert!(!player.is_seeking());

    // What was read ahead for the old place is handed back to the worker,
    // which fills all four blocks anew.
    worker.fill_ahead().unwrap();
    assert!(player.buffered_frames() >= 300);
    player.seek(20);
    assert_eq!(player.buffered_frames(), 0);
    assert_eq!(worker.fill_ahead().unwrap(), 4);
    assert_eq!(play(&mut player, &mut out, 250), 250);
    assert_plays(&out, 250, 20);

    // Seeks in quick succession: only the last one counts.
    player.seek(100);
    player.seek(7_000);
    player.seek(3_333);
    assert_eq!(player.position(), 3_333);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 250), 250);
    assert_plays(&out, 250, 3_333);

    // Past the end is the end.
    player.seek(u64::MAX);
    assert_eq!(player.position(), 10_000);
    assert!(!player.is_finished());
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 250), 0);
    assert!(player.is_finished());

    // And from the end back into the file.
    player.seek(9_900);
    assert!(!player.is_finished());
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 250), 100);
    assert_plays(&out, 100, 9_900);
    assert!(player.is_finished());
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));
}

#[test]
fn a_seek_the_worker_has_not_heard_of_still_wins() {
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, 10_000), SMALL);
    let mut out = AudioBuffer::<f32>::new(2, 250);

    // More commands than the queue to the worker holds, with the worker
    // stalled: the seek at the end of them cannot be sent yet.
    for _ in 0..100 {
        player.set_loop(None);
    }
    player.seek(7_000);
    assert_eq!(player.position(), 7_000);

    // The worker runs, knows nothing of the seek, and reads from the start.
    assert_eq!(worker.fill_ahead().unwrap(), 4);

    // Those blocks are for a place playback has left. They are dropped,
    // not played, and the seek goes out with this call.
    assert_eq!(play(&mut player, &mut out, 250), 0);
    assert!(player.is_seeking());
    assert_eq!(player.position(), 7_000);
    assert_eq!(player.underrun_frames(), 0);

    assert_eq!(worker.fill_ahead().unwrap(), 4);
    assert_eq!(play(&mut player, &mut out, 250), 250);
    assert_plays(&out, 250, 7_000);
    assert_eq!(player.position(), 7_250);
}

/// Where playback goes after the frame at `position`, by the rules the
/// player documents. `None` is the end.
fn after(position: u64, region: Option<(u64, u64)>, len: u64) -> Option<u64> {
    let next = position + 1;
    match region {
        Some((start, end)) if start < end => {
            if (position < end && next == end) || (next == len && start < next) {
                Some(start)
            } else if next == len {
                None
            } else {
                Some(next)
            }
        }
        _ => (next < len).then_some(next),
    }
}

/// Plays `total` frames in blocks of random size and checks every frame,
/// and the reported position after every block, against `after`.
fn check_loop(
    player: &mut StreamingPlayer,
    step: &mut dyn FnMut(),
    region: Option<(u64, u64)>,
    len: u64,
    mut position: Option<u64>,
    total: usize,
    seed: u64,
) -> Option<u64> {
    let channels = player.num_channels();
    let mut out = AudioBuffer::<f32>::new(channels, 300);
    let mut rng = Rng::new(seed);
    let mut done = 0;
    while done < total {
        step();
        let size = (rng.between(1, 300) as usize).min(total - done);
        let played = play(player, &mut out, size);
        for offset in 0..played {
            let at = position.expect("audio was played after the end");
            for channel in 0..channels {
                assert!(
                    out.channel(channel)[offset] == sample_at(channel, at),
                    "after {} frames: expected frame {at}",
                    done + offset
                );
            }
            position = after(at, region, len);
        }
        done += played;
        match position {
            Some(position) => {
                assert_eq!(played, size);
                assert_eq!(player.position(), position);
            }
            None => {
                assert!(player.is_finished());
                assert_eq!(player.position(), len);
                break;
            }
        }
    }
    position
}

#[test]
fn a_loop_repeats_without_a_gap_and_the_position_follows_it() {
    let len = 2_000u64;
    for (region, start) in [
        // Longer than a block, shorter than a block, a single frame.
        ((1_000u64, 1_250u64), 0u64),
        ((500, 507), 0),
        ((77, 78), 0),
        // The whole file, by its length and by "to the end".
        ((0, 2_000), 0),
        ((0, u64::MAX), 1_900),
        // An end past the end of the file is the end of the file.
        ((1_500, 5_000), 1_400),
        // Starting inside the loop, and on its last frame.
        ((300, 900), 600),
        ((300, 900), 899),
        // Starting past the loop: on to the end of the file, then round.
        ((300, 900), 900),
        ((300, 900), 1_700),
    ] {
        let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, len as usize), SMALL);
        player.set_loop(Some(region.0..region.1));
        if start != 0 {
            player.seek(start);
        }
        let position = check_loop(
            &mut player,
            &mut || {
                worker.fill_ahead().unwrap();
            },
            Some(region),
            len,
            Some(start),
            6_000,
            region.0 + start,
        );
        assert!(position.is_some(), "a loop does not end");
        assert!(!player.is_finished());
        assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));
    }
}

#[test]
fn a_range_that_holds_nothing_does_not_loop() {
    for region in [(5u64, 5u64), (900, 100), (2_000, 3_000), (2_500, u64::MAX)] {
        let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 2_000), SMALL);
        player.set_loop(Some(region.0..region.1));
        let position = check_loop(
            &mut player,
            &mut || {
                worker.fill_ahead().unwrap();
            },
            None,
            2_000,
            Some(0),
            6_000,
            3,
        );
        assert_eq!(position, None, "{region:?}");
        assert!(player.is_finished());
    }
}

#[test]
fn a_loop_can_be_started_and_stopped_while_playing() {
    let len = 3_000u64;
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, len as usize), SMALL);
    let mut out = AudioBuffer::<f32>::new(2, 300);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 50), 50);

    // A loop whose end lies beyond what has been read ahead, here the
    // first 400 frames, takes hold the first time round.
    player.set_loop(Some(200..450));
    let position = check_loop(
        &mut player,
        &mut || {
            worker.fill_ahead().unwrap();
        },
        Some((200, 450)),
        len,
        Some(50),
        1_500,
        5,
    )
    .unwrap();

    // Stopping it does not reach the audio already read ahead, which
    // still goes round. Frame by frame: each one follows the one before,
    // or is the loop's start after its end, and that once or twice more,
    // the read-ahead being 300 to 400 frames and the loop 250.
    worker.fill_ahead().unwrap();
    player.set_loop(None);
    let mut position = position;
    let mut rounds = 0;
    for _ in 0..1_000 {
        worker.fill_ahead().unwrap();
        assert_eq!(player.position(), position);
        assert_eq!(play(&mut player, &mut out, 1), 1);
        assert_plays(&out, 1, position);
        let next = player.position();
        if next != position + 1 {
            assert_eq!((position, next), (449, 200));
            rounds += 1;
        }
        position = next;
    }
    assert!((1..=2).contains(&rounds), "{rounds} more rounds");
    // By now playback has left the loop behind.
    assert!((800..1_500).contains(&position), "at {position}");

    // To change at once, seek to where playback is: the read-ahead is
    // thrown away and read again under the new rule.
    player.set_loop(Some(1_500..1_800));
    player.seek(position);
    let position = check_loop(
        &mut player,
        &mut || {
            worker.fill_ahead().unwrap();
        },
        Some((1_500, 1_800)),
        len,
        Some(position),
        2_000,
        8,
    )
    .unwrap();
    player.set_loop(None);
    player.seek(position);
    let position = check_loop(
        &mut player,
        &mut || {
            worker.fill_ahead().unwrap();
        },
        None,
        len,
        Some(position),
        10_000,
        6,
    );
    assert_eq!(position, None);
    assert!(player.is_finished());

    // A file that has ended starts again when it is given a loop, and says
    // so at once: a caller that stops when `is_finished` is true must not
    // stop here. Until the worker has read, there is silence, which is not
    // an underrun.
    player.set_loop(Some(100..160));
    assert!(!player.is_finished());
    assert!(player.is_seeking());
    assert_eq!(player.position(), 100);
    assert_eq!(play(&mut player, &mut out, 10), 0);
    let position = check_loop(
        &mut player,
        &mut || {
            worker.fill_ahead().unwrap();
        },
        Some((100, 160)),
        len,
        Some(100),
        500,
        7,
    );
    assert!(position.is_some());
    assert!(!player.is_finished());
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));
}

#[test]
fn starting_a_loop_after_the_end_makes_the_player_unfinished() {
    // A caller that polls `is_finished`, as the example in the
    // documentation does, must not be told the file has ended just after
    // asking for it to go round.
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 250), SMALL);
    let mut out = AudioBuffer::<f32>::new(1, 300);
    worker.fill_ahead().unwrap();
    assert_eq!(player.process(&mut out), 250);
    assert!(player.is_finished());
    assert_eq!(player.position(), 250);

    player.set_loop(Some(0..100));
    assert!(
        !player.is_finished(),
        "a loop was started and the player still says it has finished"
    );
    // The next frame played is the loop's first, and the position says so
    // before the worker has done anything.
    assert_eq!(player.position(), 0);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 10), 10);
    assert_plays(&out, 10, 0);
    assert_eq!(player.position(), 10);
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));

    // A loop that leads nowhere from the end leaves the file finished: one
    // that holds nothing, and one that starts where the file ends.
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 250), SMALL);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 300), 250);
    assert!(player.is_finished());
    for (start, end) in [(40u64, 40u64), (90, 20), (250, 900), (300, u64::MAX)] {
        player.set_loop(Some(start..end));
        assert!(player.is_finished());
        assert_eq!(player.position(), 250);
        worker.fill_ahead().unwrap();
        assert_eq!(play(&mut player, &mut out, 10), 0);
        assert!(player.is_finished());
    }
    // And the same file comes back to life with one that does lead back,
    // here from its last frame.
    player.set_loop(Some(249..u64::MAX));
    assert!(!player.is_finished());
    assert_eq!(player.position(), 249);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 3), 3);
    for &sample in &out.channel(0)[..3] {
        assert_eq!(sample, sample_at(0, 249));
    }
}

#[test]
fn a_loop_set_when_the_end_was_already_read_ahead_still_goes_round() {
    // The whole file fits in the read-ahead, so the worker has read its
    // end, and stopped, before the loop is asked for.
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 250), SMALL);
    let mut out = AudioBuffer::<f32>::new(1, 300);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 100), 100);

    // With the worker stalled: the end is played, and the player neither
    // says it has finished nor counts the wait against the worker.
    player.set_loop(Some(50..120));
    assert_eq!(play(&mut player, &mut out, 300), 150);
    assert_plays(&out, 150, 100);
    assert!(!player.is_finished());
    assert!(player.is_seeking());
    assert_eq!(player.position(), 50);
    assert_eq!(play(&mut player, &mut out, 300), 0);
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));

    // The worker reads again, and playback goes round from then on.
    let position = check_loop(
        &mut player,
        &mut || {
            worker.fill_ahead().unwrap();
        },
        Some((50, 120)),
        250,
        Some(50),
        1_000,
        9,
    );
    assert!(position.is_some());
    assert!(!player.is_finished());

    // With a worker that has already gone round by the time the end is
    // played, there is no gap at all.
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 250), SMALL);
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 100), 100);
    player.set_loop(Some(50..120));
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 300), 300);
    assert_plays(&out, 150, 100);
    for (offset, &sample) in out.channel(0)[150..].iter().enumerate() {
        assert_eq!(sample, sample_at(0, 50 + offset as u64 % 70));
    }
    assert!(!player.is_finished());
}

#[test]
fn a_seek_to_the_end_with_a_loop_set_is_a_seek_to_the_start_of_the_loop() {
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 1_000), SMALL);
    let mut out = AudioBuffer::<f32>::new(1, 300);
    player.set_loop(Some(200..300));
    for target in [1_000u64, 5_000, u64::MAX] {
        player.seek(target);
        // The end is where the loop sends playback back, so that is where
        // it is, and the position says so before the worker has answered.
        assert_eq!(player.position(), 200);
        assert!(!player.is_finished());
        worker.fill_ahead().unwrap();
        assert_eq!(play(&mut player, &mut out, 150), 150);
        assert_plays(&out, 100, 200);
        assert_eq!(out.channel(0)[100], sample_at(0, 200));
        assert_eq!(player.position(), 250);
    }

    // Without such a loop the end is the end.
    for region in [None, Some((1_000u64, 2_000u64)), Some((7, 7))] {
        player.set_loop(region.map(|(start, end)| start..end));
        player.seek(u64::MAX);
        assert_eq!(player.position(), 1_000);
        worker.fill_ahead().unwrap();
        assert_eq!(play(&mut player, &mut out, 150), 0);
        assert!(player.is_finished());
        assert_eq!(player.position(), 1_000);
    }

    // A loop set while a seek to the end is still unanswered takes
    // playback back as well.
    player.set_loop(None);
    player.seek(1_000);
    assert_eq!(player.position(), 1_000);
    player.set_loop(Some(600..u64::MAX));
    assert_eq!(player.position(), 600);
    assert!(!player.is_finished());
    worker.fill_ahead().unwrap();
    assert_eq!(play(&mut player, &mut out, 150), 150);
    assert_plays(&out, 150, 600);
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));
}

#[test]
fn the_position_names_the_next_frame_whatever_is_asked_and_whenever() {
    // Seeks, loops and steps of the worker in any order, one frame played
    // at a time. Two things must hold all the way: the frame that is played
    // is the one `position` named just before, and nothing is played after
    // the player has said the file is finished.
    let len = 5_000u64;
    let config = PlayerConfig {
        block_frames: 64,
        blocks: 4,
    };
    for seed in 1..=8 {
        let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, len as usize), config);
        let mut out = AudioBuffer::<f32>::new(1, 1);
        let mut rng = Rng::new(seed);
        let mut played = 0;
        let mut ends = 0;
        for step in 0..60_000 {
            match rng.below(400) {
                0 => player.seek(rng.below(len + 500)),
                1 => player.seek(len - rng.below(3)),
                2 => player.seek(u64::MAX),
                3 | 4 => {
                    let start = rng.below(len + 100);
                    player.set_loop(Some(start..start + rng.below(600)));
                }
                5 => player.set_loop(Some(rng.below(len)..u64::MAX)),
                6 | 7 => player.set_loop(None),
                // The worker runs often, but not always when it is needed.
                8..=250 => {
                    worker.step().unwrap();
                }
                _ => {}
            }
            let named = player.position();
            let was_finished = player.is_finished();
            if player.process(&mut out) == 1 {
                assert!(
                    !was_finished,
                    "seed {seed}, step {step}: a frame was played after the end"
                );
                assert!(
                    out.channel(0)[0] == sample_at(0, named),
                    "seed {seed}, step {step}: the position was {named} and another frame came"
                );
                played += 1;
            }
            ends += u32::from(player.is_finished() && !was_finished);
        }
        // The run went through plenty of audio and reached the end often.
        assert!(played > 20_000, "seed {seed}: {played} frames");
        assert!(ends > 5, "seed {seed}: {ends} ends");
    }
}

#[test]
fn flac_is_played_seeked_and_looped_like_any_other_file() {
    // Blocks of varying size and no declared length: the player learns
    // where the file ends when the worker gets there.
    let mut spec = FlacSpec::new(2, 16, 1_024);
    spec.block_sizes = vec![1_024, 300, 77];
    spec.declare_total = false;
    let total = 20_000u64;
    let stream = write_flac(&spec, total, |channel, index| {
        flac_sample(16, channel, index)
    });
    let value = |channel: usize, frame: u64| flac_scale(flac_sample(16, channel, frame), 16);

    let reader = AudioReader::new(Cursor::new(stream.bytes)).unwrap();
    assert_eq!(reader.len_frames(), None);
    let config = PlayerConfig {
        block_frames: 333,
        blocks: 3,
    };
    let (mut player, mut worker) = StreamingPlayer::new(reader, config);
    assert_eq!(player.len_frames(), None);
    player.set_loop(Some(15_000..u64::MAX));
    player.seek(12_345);

    let mut out = AudioBuffer::<f32>::new(2, 200);
    let mut position = 12_345u64;
    for _ in 0..200 {
        worker.fill_ahead().unwrap();
        assert_eq!(play(&mut player, &mut out, 200), 200);
        for offset in 0..200 {
            for channel in 0..2 {
                assert!(out.channel(channel)[offset] == value(channel, position));
            }
            position = if position + 1 == total {
                15_000
            } else {
                position + 1
            };
        }
        assert_eq!(player.position(), position);
    }
    assert_eq!(player.underrun_frames(), 0);
}

#[test]
fn outputs_with_more_or_fewer_channels_than_the_file() {
    // A mono file into a stereo output: the second channel is silent.
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 500), SMALL);
    worker.fill_ahead().unwrap();
    let mut out = AudioBuffer::<f32>::new(2, 120);
    assert_eq!(play(&mut player, &mut out, 120), 120);
    for (offset, &sample) in out.channel(0).iter().enumerate() {
        assert_eq!(sample, sample_at(0, offset as u64));
    }
    assert_eq!(out.channel(1), &[0.0; 120]);

    // A file of three channels into a mono output: the first is played.
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(3, 500), SMALL);
    worker.fill_ahead().unwrap();
    let mut out = AudioBuffer::<f32>::new(1, 120);
    assert_eq!(play(&mut player, &mut out, 120), 120);
    assert_plays(&out, 120, 0);
}

#[test]
fn a_read_that_fails_is_reported_and_can_be_tried_again() {
    // Once the file is open, the disk lets a little over two blocks
    // through: 250 frames of eight bytes.
    let failing = Failing::new(Cursor::new(wav_bytes(2, 1_000)), u64::MAX);
    let budget = failing.budget.clone();
    let reader = AudioReader::new(failing).unwrap();
    budget.store(250 * 8, Ordering::Relaxed);
    let (mut player, mut worker) = StreamingPlayer::new(reader, SMALL);
    let mut out = AudioBuffer::<f32>::new(2, 300);

    assert!(worker.step().unwrap());
    assert!(worker.step().unwrap());
    for _ in 0..3 {
        assert!(matches!(worker.step(), Err(FormatError::Io(_))));
    }
    assert!(!player.worker_stopped());

    // What was read is played; then the player waits, and counts.
    assert_eq!(play(&mut player, &mut out, 300), 200);
    assert_plays(&out, 200, 0);
    assert_eq!((player.underruns(), player.underrun_frames()), (1, 100));

    // The disk comes back: the worker reads the block that failed, and
    // playback goes on with nothing missing and nothing twice.
    budget.store(u64::MAX, Ordering::Relaxed);
    let mut position = 200;
    while !player.is_finished() {
        worker.fill_ahead().unwrap();
        let played = play(&mut player, &mut out, 300);
        assert_plays(&out, played, position);
        position += played as u64;
    }
    assert_eq!(position, 1_000);

    // Without a worker, silence is not an underrun.
    let (mut player, worker) = StreamingPlayer::new(wav_reader(2, 1_000), SMALL);
    drop(worker);
    assert!(player.worker_stopped());
    assert_eq!(play(&mut player, &mut out, 300), 0);
    player.seek(10);
    assert_eq!(play(&mut player, &mut out, 300), 0);
    assert_eq!((player.underruns(), player.underrun_frames()), (0, 0));
}

#[test]
fn odd_configurations_are_brought_within_reason() {
    let config = PlayerConfig {
        block_frames: 0,
        blocks: 0,
    };
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(1, 10), config);
    let mut out = AudioBuffer::<f32>::new(1, 4);
    let mut position = 0;
    while !player.is_finished() {
        worker.fill_ahead().unwrap();
        let played = play(&mut player, &mut out, 4);
        assert_plays(&out, played, position);
        position += played as u64;
    }
    assert_eq!(position, 10);

    let config = RecorderConfig { capacity_frames: 0 };
    let writer = AudioWriter::wav(Cursor::new(Vec::new()), 2, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, mut worker) = RecordingWriter::new(writer, config);
    let block = ramp(2, 7);
    let mut taken = 0;
    while taken < 7 {
        taken += recorder.push(&slice(&block, taken..7));
        worker.drain().unwrap();
    }
    let bytes = worker.finish().unwrap().into_inner();
    assert_eq!(wav::read(bytes.as_slice()).unwrap().buffer, block);
}

/// A writer for a float file of `channels` channels held in memory.
fn float_writer(channels: usize) -> AudioWriter<Cursor<Vec<u8>>> {
    AudioWriter::wav(
        Cursor::new(Vec::new()),
        channels,
        48_000,
        WavFormat::Float32,
    )
    .unwrap()
}

#[test]
fn what_is_pushed_is_what_is_written() {
    for channels in [1usize, 2, 5] {
        let audio = ramp(channels, 20_000);
        let config = RecorderConfig {
            capacity_frames: 4_096,
        };
        let (mut recorder, mut worker) = RecordingWriter::new(float_writer(channels), config);
        assert_eq!(recorder.num_channels(), channels);
        let mut rng = Rng::new(channels as u64);
        let mut done = 0;
        while done < 20_000 {
            let size = (rng.between(0, 1_500) as usize).min(20_000 - done);
            // The queue always has room for the largest block here, as
            // long as the worker empties it now and then.
            if recorder.free_frames() < size {
                assert!(worker.drain().unwrap() > 0);
            }
            assert_eq!(recorder.push(&slice(&audio, done..done + size)), size);
            done += size;
            if rng.below(4) == 0 {
                worker.step().unwrap();
            }
        }
        assert_eq!((recorder.overruns(), recorder.overrun_frames()), (0, 0));
        assert!(!recorder.worker_stopped());

        // Finishing writes what is still queued and completes the header.
        assert!(worker.frames_written() <= 20_000);
        let bytes = worker.finish().unwrap().into_inner();
        assert!(recorder.worker_stopped());
        let file = wav::read(bytes.as_slice()).unwrap();
        assert_eq!(file.sample_rate, 48_000);
        assert!(file.buffer == audio, "{channels} channels: audio differs");
    }
}

#[test]
fn a_full_queue_loses_frames_and_counts_them() {
    // Room for 1024 stereo frames: 1000 rounded up to a power of two.
    let config = RecorderConfig {
        capacity_frames: 1_000,
    };
    let (mut recorder, mut worker) = RecordingWriter::new(float_writer(2), config);
    assert_eq!(recorder.free_frames(), 1_024);
    let audio = ramp(2, 3_000);

    assert_eq!(recorder.push(&slice(&audio, 0..700)), 700);
    assert_eq!(recorder.free_frames(), 324);
    // The worker is stalled. What fits is taken, the rest is lost.
    assert_eq!(recorder.push(&slice(&audio, 700..1_400)), 324);
    assert_eq!((recorder.overruns(), recorder.overrun_frames()), (1, 376));
    assert_eq!(recorder.push(&slice(&audio, 1_400..1_500)), 0);
    assert_eq!(recorder.push(&slice(&audio, 1_500..1_500)), 0);
    assert_eq!((recorder.overruns(), recorder.overrun_frames()), (1, 476));
    assert_eq!(worker.frames_written(), 0);

    // The worker writes; there is room again, and no further loss.
    assert!(worker.step().unwrap());
    assert_eq!(worker.frames_written(), 1_024);
    assert!(!worker.step().unwrap());
    assert_eq!(recorder.free_frames(), 1_024);
    assert_eq!(recorder.push(&slice(&audio, 2_000..2_100)), 100);
    assert_eq!((recorder.overruns(), recorder.overrun_frames()), (1, 476));

    // A second overflow is a second overrun.
    assert_eq!(recorder.push(&slice(&audio, 0..1_000)), 924);
    assert_eq!((recorder.overruns(), recorder.overrun_frames()), (2, 552));

    // The file holds exactly what was taken, in order.
    let bytes = worker.finish().unwrap().into_inner();
    let file = wav::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(file.num_frames(), 700 + 324 + 100 + 924);
    assert_eq!(slice(&file, 0..1_024), slice(&audio, 0..1_024));
    assert_eq!(slice(&file, 1_024..1_124), slice(&audio, 2_000..2_100));
    assert_eq!(slice(&file, 1_124..2_048), slice(&audio, 0..924));
}

#[test]
fn blocks_with_more_or_fewer_channels_than_the_file() {
    let (mut recorder, worker) = RecordingWriter::new(float_writer(2), RecorderConfig::default());
    let mono = ramp(1, 50);
    let wide = ramp(3, 50);
    assert_eq!(recorder.push(&mono), 50);
    assert_eq!(recorder.push(&wide), 50);
    let bytes = worker.finish().unwrap().into_inner();
    let file = wav::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(file.num_frames(), 100);
    // A missing channel is silent; a channel too many is left out.
    assert_eq!(&file.channel(0)[..50], mono.channel(0));
    assert_eq!(&file.channel(1)[..50], &[0.0; 50]);
    assert_eq!(&file.channel(0)[50..], wide.channel(0));
    assert_eq!(&file.channel(1)[50..], wide.channel(1));
}

#[test]
fn a_recording_is_the_file_the_whole_file_writer_gives() {
    let audio = ramp(2, 5_000);
    let mut whole = Vec::new();
    manguebyte_formats::aiff::write(&mut whole, &audio, 44_100, AiffFormat::Int24).unwrap();

    let writer = AudioWriter::aiff(Cursor::new(Vec::new()), 2, 44_100, AiffFormat::Int24).unwrap();
    let (mut recorder, mut worker) = RecordingWriter::new(writer, RecorderConfig::default());
    for start in (0..5_000).step_by(128) {
        let end = (start + 128).min(5_000);
        assert_eq!(recorder.push(&slice(&audio, start..end)), end - start);
        worker.step().unwrap();
    }
    let bytes = worker.finish().unwrap().into_inner();
    assert!(bytes == whole, "the recorded AIFF differs");
}

#[test]
fn a_write_that_fails_is_reported_and_costs_only_what_it_was_writing() {
    let audio = ramp(2, 2_000);
    let config = RecorderConfig {
        capacity_frames: 4_096,
    };

    // The disk takes the header and 600 frames and a half, then is full.
    let sink = Failing::new(Cursor::new(Vec::new()), 80 + 600 * 8 + 4);
    let budget = sink.budget.clone();
    let writer = AudioWriter::wav(sink, 2, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, mut worker) = RecordingWriter::new(writer, config);
    assert_eq!(recorder.push(&audio), 2_000);
    // The worker takes 1024 frames out of the queue and fails to write
    // them all.
    assert!(matches!(worker.step(), Err(FormatError::Io(_))));
    assert!(!recorder.worker_stopped());

    // Room is made on the disk. The rest of the queue is written after the
    // 600 frames that got through, and the file is a good one: only the
    // 424 frames the failed write was holding are missing.
    budget.store(u64::MAX, Ordering::Relaxed);
    let bytes = worker.finish().unwrap().into_inner().into_inner();
    assert!(recorder.worker_stopped());
    let file = wav::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(file.num_frames(), 600 + 976);
    assert_eq!(slice(&file, 0..600), slice(&audio, 0..600));
    assert_eq!(slice(&file, 600..1_576), slice(&audio, 1_024..2_000));

    // A disk that stays full: finishing fails too, and says so.
    let sink = Failing::new(Cursor::new(Vec::new()), 80 + 600 * 8);
    let writer = AudioWriter::wav(sink, 2, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, mut worker) = RecordingWriter::new(writer, config);
    assert_eq!(recorder.push(&audio), 2_000);
    assert!(matches!(worker.step(), Err(FormatError::Io(_))));
    assert!(matches!(worker.finish(), Err(FormatError::Io(_))));
    assert!(recorder.worker_stopped());

    // That disk refused everything, the header too. A real one that is full
    // still lets what is on it be written over, which is all completing a
    // header takes. Finishing still reports that audio was lost, and the
    // file it leaves is whole: the 600 frames that fitted, under a header
    // that says 600 frames.
    let disk = FullDisk::new(80 + 600 * 8 + 4);
    let contents = disk.contents();
    let writer = AudioWriter::wav(disk, 2, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, mut worker) = RecordingWriter::new(writer, config);
    assert_eq!(recorder.push(&audio), 2_000);
    assert!(matches!(worker.step(), Err(FormatError::Io(_))));
    assert!(matches!(worker.finish(), Err(FormatError::Io(_))));
    let bytes = contents.lock().unwrap().clone();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(bytes[76..80], (600u32 * 8).to_le_bytes());
    let file = wav::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(file, slice(&audio, 0..600));
}

/// Waits a moment for the other thread, and fails the test rather than
/// wait for ever.
fn wait(since: Instant, what: &str) {
    assert!(since.elapsed() < PATIENCE, "gave up waiting for {what}");
    std::thread::sleep(Duration::from_micros(200));
}

#[test]
fn a_real_thread_keeps_the_player_fed() {
    let frames = 60_000u64;
    let (mut player, worker) = StreamingPlayer::new(wav_reader(2, frames as usize), SMALL);
    // Round a stretch near the end three times, then on to the end.
    player.set_loop(Some(58_000..59_500));
    let thread = worker.spawn().unwrap();
    let started = Instant::now();
    let mut out = AudioBuffer::<f32>::new(2, 64);
    let mut rng = Rng::new(17);

    // Whatever the timing, the audio that comes is the audio of the file,
    // in order; a block that comes up short is only late.
    let mut expect = |player: &mut StreamingPlayer, position: &mut u64, upto: u64| {
        while *position < upto {
            let size = rng.between(1, 64).min(upto - *position) as usize;
            let played = play(player, &mut out, size);
            assert_plays(&out, played, *position);
            *position += played as u64;
            // At `upto` playback may be about to jump; the caller knows.
            if *position < upto {
                assert_eq!(player.position(), *position);
            }
            if played < size {
                wait(started, "the worker to read");
            }
        }
    };

    // The start of the file.
    let mut position = 0;
    expect(&mut player, &mut position, 10_000);

    // Seeks, forwards and backwards, each played from for a while.
    for target in [40_000u64, 123, 30_001, 29_999] {
        player.seek(target);
        position = target;
        expect(&mut player, &mut position, target + 3_000);
    }

    // Into the loop and round it.
    player.seek(57_000);
    position = 57_000;
    expect(&mut player, &mut position, 59_500);
    for _ in 0..3 {
        assert_eq!(player.position(), 58_000);
        position = 58_000;
        expect(&mut player, &mut position, 59_500);
    }

    // Out of the loop: seek to where playback is, so that what was read
    // ahead under the loop is read again without it.
    player.set_loop(None);
    player.seek(58_000);
    position = 58_000;
    expect(&mut player, &mut position, frames);
    while !player.is_finished() {
        assert_eq!(play(&mut player, &mut out, 64), 0);
        wait(started, "the end of the file");
    }
    assert_eq!(player.position(), frames);
    assert!(!player.worker_stopped());
    assert!(!thread.has_failed());

    thread.stop().unwrap();
    assert!(player.worker_stopped());
}

#[test]
fn a_real_thread_that_cannot_read_stops_and_says_why() {
    // Once the file is open, the disk gives a thousand frames and no more.
    let failing = Failing::new(Cursor::new(wav_bytes(1, 5_000)), u64::MAX);
    let budget = failing.budget.clone();
    let reader = AudioReader::new(failing).unwrap();
    budget.store(1_000 * 4, Ordering::Relaxed);
    let (mut player, worker) = StreamingPlayer::new(reader, SMALL);
    let thread = worker.spawn().unwrap();
    let started = Instant::now();

    // The thousand frames that could be read are played, however long the
    // thread takes to read them and to give up.
    let mut out = AudioBuffer::<f32>::new(1, 128);
    let mut position = 0;
    while !(player.worker_stopped() && player.buffered_frames() == 0) {
        let played = play(&mut player, &mut out, 128);
        assert_plays(&out, played, position);
        position += played as u64;
        if played == 0 {
            wait(started, "the worker to give up");
        }
    }
    assert_eq!(position, 1_000);
    // Silence from here on is not counted against a worker that is gone.
    let underruns = player.underrun_frames();
    assert_eq!(play(&mut player, &mut out, 128), 0);
    assert_eq!(player.underrun_frames(), underruns);

    while !thread.has_failed() {
        wait(started, "the thread to end");
    }
    assert!(matches!(thread.stop(), Err(FormatError::Io(_))));
}

#[test]
fn a_real_thread_writes_what_is_pushed() {
    let frames = 100_000;
    let audio = ramp(2, frames);
    let config = RecorderConfig {
        capacity_frames: 2_048,
    };
    let (mut recorder, worker) = RecordingWriter::new(float_writer(2), config);
    let thread = worker.spawn().unwrap();
    let started = Instant::now();
    let mut rng = Rng::new(23);

    // A caller that will not lose audio waits for room. That is for a test
    // to do, never for an audio callback.
    let mut done = 0;
    while done < frames {
        let size = (rng.between(1, 512) as usize).min(frames - done);
        while recorder.free_frames() < size {
            wait(started, "the worker to write");
        }
        assert_eq!(recorder.push(&slice(&audio, done..done + size)), size);
        done += size;
    }
    assert_eq!((recorder.overruns(), recorder.overrun_frames()), (0, 0));
    assert!(!thread.has_failed());

    let bytes = thread.finish().unwrap().into_inner();
    assert!(recorder.worker_stopped());
    let file = decode(&bytes).unwrap();
    assert!(
        file.buffer == audio,
        "the recording differs from what was pushed"
    );
}

#[test]
fn a_real_thread_that_is_outrun_loses_exactly_what_is_counted() {
    let frames = 200_000;
    let audio = ramp(2, frames);
    // A queue far too small for a producer that never waits.
    let config = RecorderConfig {
        capacity_frames: 256,
    };
    let (mut recorder, worker) = RecordingWriter::new(float_writer(2), config);
    let thread = worker.spawn().unwrap();

    // Push without ever waiting for room, as an audio callback would if
    // the disk could not keep up, and remember what was taken. The short
    // sleeps only spread the pushes over enough time for the other thread
    // to run among them; however the two interleave, the count must add up.
    let mut expected = vec![Vec::new(); 2];
    let mut done = 0;
    while done < frames {
        let size = 200.min(frames - done);
        let taken = recorder.push(&slice(&audio, done..done + size));
        for (channel, samples) in expected.iter_mut().enumerate() {
            samples.extend_from_slice(&audio.channel(channel)[done..done + taken]);
        }
        done += size;
        if done % 5_000 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let taken = expected[0].len();
    assert_eq!(taken as u64 + recorder.overrun_frames(), frames as u64);
    println!(
        "{taken} of {frames} frames were taken; {} lost in {} overruns",
        recorder.overrun_frames(),
        recorder.overruns()
    );

    // The file holds the frames that were taken, all of them, in order,
    // and nothing else.
    let bytes = thread.finish().unwrap().into_inner();
    let file = decode(&bytes).unwrap().buffer;
    assert_eq!(file.num_frames(), taken);
    for (channel, samples) in expected.iter().enumerate() {
        assert!(file.channel(channel) == samples.as_slice());
    }
}

#[test]
fn a_real_thread_that_cannot_write_stops_and_says_why() {
    // The disk takes the header and 300 frames.
    let sink = Failing::new(Cursor::new(Vec::new()), 80 + 300 * 4);
    let writer = AudioWriter::wav(sink, 1, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, worker) = RecordingWriter::new(writer, RecorderConfig::default());
    let thread = worker.spawn().unwrap();
    let started = Instant::now();

    recorder.push(&ramp(1, 2_000));
    while !(recorder.worker_stopped() && thread.has_failed()) {
        wait(started, "the worker to give up");
    }
    assert!(matches!(thread.finish(), Err(FormatError::Io(_))));
    // What is pushed now goes nowhere, and still does not block.
    recorder.push(&ramp(1, 100));
}

#[test]
fn a_real_thread_on_a_full_disk_leaves_a_file_with_its_header() {
    // Room for the header, 300 frames and half of another, and no more: a
    // full disk, on which what is there can still be written over.
    let disk = FullDisk::new(80 + 300 * 4 + 2);
    let contents = disk.contents();
    let writer = AudioWriter::wav(disk, 1, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, worker) = RecordingWriter::new(writer, RecorderConfig::default());
    let thread = worker.spawn().unwrap();
    let started = Instant::now();

    let audio = ramp(1, 2_000);
    assert_eq!(recorder.push(&audio), 2_000);
    while !(recorder.worker_stopped() && thread.has_failed()) {
        wait(started, "the worker to give up");
    }
    // The error that stopped the recording is the one reported.
    assert!(matches!(thread.finish(), Err(FormatError::Io(_))));

    // The thread is gone and so is the disk it owned. What it left is a
    // finished file of the 300 frames that fitted.
    let bytes = contents.lock().unwrap().clone();
    assert_eq!(bytes.len(), 80 + 300 * 4 + 2);
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(bytes[4..8], (72 + 300u32 * 4).to_le_bytes());
    assert_eq!(bytes[76..80], (300u32 * 4).to_le_bytes());
    let file = wav::read(bytes.as_slice()).unwrap().buffer;
    assert_eq!(file, slice(&audio, 0..300));
}

#[test]
fn dropping_the_handles_stops_the_threads() {
    let (player, worker) = StreamingPlayer::new(wav_reader(1, 1_000), SMALL);
    let thread = worker.spawn().unwrap();
    drop(thread);
    assert!(player.worker_stopped());

    let (mut recorder, worker) = RecordingWriter::new(float_writer(1), RecorderConfig::default());
    let thread = worker.spawn().unwrap();
    recorder.push(&ramp(1, 100));
    // Dropping finishes the file, though nothing can be said about how
    // that went.
    drop(thread);
    assert!(recorder.worker_stopped());
}
