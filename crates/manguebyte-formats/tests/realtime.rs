//! Proves what the streaming types promise about memory.
//!
//! Two promises are made. The audio-side calls of [`StreamingPlayer`] and
//! [`RecordingWriter`] never touch the heap, because a call to the
//! allocator can outlast the few milliseconds an audio callback has. And a
//! reader's memory is bounded by the block it reads, not by the length of
//! the file or by what a damaged header claims.
//!
//! A FLAC writer adds a third: it allocates when it is made and not while
//! it encodes, so a recording of any length takes the same memory.
//!
//! Neither can be seen from outside, so this test replaces the global
//! allocator with one that counts what the current thread does: how many
//! calls it makes, and the most memory it holds at once.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use common::{FlacSpec, Rng, flac_sample, flac_scale, ramp, sample_at, write_flac};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::flac::FlacOptions;
use manguebyte_formats::wav::{self, WavFormat};
use manguebyte_formats::{
    AudioReader, AudioWriter, PlayerConfig, RecorderConfig, RecordingWriter, StreamingPlayer,
};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALLS: Cell<usize> = const { Cell::new(0) };
    /// Bytes allocated less bytes freed since watching began.
    static HELD: Cell<isize> = const { Cell::new(0) };
    static MOST_HELD: Cell<isize> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn record(change: isize) {
        // `try_with` because the allocator is still called while a thread's
        // locals are being torn down.
        let _ = WATCHING.try_with(|watching| {
            if watching.get() {
                let _ = HEAP_CALLS.try_with(|calls| calls.set(calls.get() + 1));
                let _ = HELD.try_with(|held| {
                    held.set(held.get() + change);
                    let _ = MOST_HELD.try_with(|most| most.set(most.get().max(held.get())));
                });
            }
        });
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is updating thread-local counters, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size() as isize);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::record(-(layout.size() as isize));
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size as isize - layout.size() as isize);
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// What a piece of work did with the heap.
struct HeapUse {
    /// Calls to allocate, free or resize.
    calls: usize,
    /// The most bytes held at one time beyond what was held before.
    most_held: usize,
}

/// Runs `work` on this thread and reports its use of the heap.
fn watch<T>(work: impl FnOnce() -> T) -> (T, HeapUse) {
    HEAP_CALLS.with(|calls| calls.set(0));
    HELD.with(|held| held.set(0));
    MOST_HELD.with(|most| most.set(0));
    WATCHING.with(|watching| watching.set(true));
    let result = work();
    WATCHING.with(|watching| watching.set(false));
    let heap_use = HeapUse {
        calls: HEAP_CALLS.with(Cell::get),
        most_held: MOST_HELD.with(Cell::get).max(0) as usize,
    };
    (result, heap_use)
}

/// Runs `work` and panics if it used the heap in any way.
fn assert_no_heap_use<T>(what: &str, work: impl FnOnce() -> T) -> T {
    let (result, heap_use) = watch(work);
    assert_eq!(
        heap_use.calls, 0,
        "{what} used the heap {} time(s)",
        heap_use.calls
    );
    result
}

#[test]
fn the_guard_itself_catches_an_allocation() {
    let caught = std::panic::catch_unwind(|| {
        assert_no_heap_use("a Vec", || drop(std::hint::black_box(vec![0u8; 64])));
    });
    WATCHING.with(|watching| watching.set(false));
    assert!(caught.is_err(), "the allocation guard is not working");

    let (_, heap_use) = watch(|| {
        let first = std::hint::black_box(vec![0u8; 1_000]);
        drop(first);
        drop(std::hint::black_box(vec![0u8; 3_000]));
    });
    assert_eq!(heap_use.calls, 4);
    assert_eq!(heap_use.most_held, 3_000);
}

fn wav_reader(channels: usize, frames: usize) -> AudioReader<Cursor<Vec<u8>>> {
    let mut bytes = Vec::new();
    wav::write(
        &mut bytes,
        &ramp(channels, frames),
        48_000,
        WavFormat::Float32,
    )
    .unwrap();
    AudioReader::new(Cursor::new(bytes)).unwrap()
}

/// Whether the first `count` frames of `out` are the file's from `position`.
/// Written without `assert!`, whose message would allocate.
fn plays(out: &AudioBuffer<f32>, count: usize, position: u64) -> bool {
    out.iter_channels().enumerate().all(|(channel, samples)| {
        samples[..count]
            .iter()
            .enumerate()
            .all(|(offset, &sample)| sample == sample_at(channel, position + offset as u64))
    })
}

#[test]
fn the_player_never_uses_the_heap_on_the_audio_side() {
    let config = PlayerConfig {
        block_frames: 256,
        blocks: 4,
    };
    let (mut player, mut worker) = StreamingPlayer::new(wav_reader(2, 20_000), config);
    let mut out = AudioBuffer::<f32>::new(2, 512);
    let mut rng = Rng::new(3);

    // The worker runs between the audio-side calls, unwatched: it may use
    // the heap as it likes. Everything the audio thread can call is called
    // with the allocator watched. First, running dry: one block of 256
    // frames is read and 1024 are asked for.
    assert!(worker.step().unwrap());
    let played = assert_no_heap_use("StreamingPlayer running dry", || {
        player.process(&mut out) + player.process(&mut out)
    });
    assert_eq!(played, 256);
    assert_eq!(player.underrun_frames(), 768);

    // Then playing in blocks of every size, seeking, looping, reaching the
    // end, running dry now and then, asking where things stand.
    let mut right = true;
    for round in 0..400u64 {
        if round % 5 != 0 {
            worker.fill_ahead().unwrap();
        }
        let size = rng.between(0, 512) as usize;
        let target = rng.below(21_000);
        right &= assert_no_heap_use("StreamingPlayer", || {
            out.set_num_frames(size);
            let mut right = player.process(&mut out) <= size;
            match round % 37 {
                7 => player.seek(target),
                11 => player.set_loop(Some(target..target + 700)),
                23 => player.set_loop(None),
                _ => {}
            }
            right &= player.buffered_frames() <= 4 * 256;
            right &= player.underruns() <= player.underrun_frames();
            right &= player.position() <= 20_000;
            let _ = (player.is_finished(), player.is_seeking());
            let _ = (player.len_frames(), player.worker_stopped());
            right
        });
    }
    assert!(right, "the player reported something impossible");

    // Far more commands than the queue to the worker holds.
    assert_no_heap_use("StreamingPlayer commands", || {
        for frame in 0..1_000 {
            player.seek(frame);
            player.set_loop(Some(frame..frame + 10));
            player.process(&mut out);
        }
    });
}

#[test]
fn the_player_never_uses_the_heap_while_a_real_thread_feeds_it() {
    let frames = 200_000u64;
    let (mut player, worker) = StreamingPlayer::new(
        wav_reader(2, frames as usize),
        PlayerConfig {
            block_frames: 512,
            blocks: 8,
        },
    );
    let thread = worker.spawn().unwrap();
    let mut out = AudioBuffer::<f32>::new(2, 256);
    let started = Instant::now();

    // The whole of playback is watched: the file from start to end, with a
    // seek back to the middle on the way. The other thread's allocations
    // are its own and are not counted here.
    let (right, played) = assert_no_heap_use("StreamingPlayer beside its thread", || {
        let mut right = true;
        let mut played_in_all = 0u64;
        let mut position = 0u64;
        let mut has_seeked = false;
        while !player.is_finished() && started.elapsed() < Duration::from_secs(120) {
            let played = player.process(&mut out);
            right &= plays(&out, played, position);
            position += played as u64;
            played_in_all += played as u64;
            if position >= 150_000 && !has_seeked {
                has_seeked = true;
                player.seek(100_000);
                position = 100_000;
            }
            if played == 0 {
                std::thread::yield_now();
            }
        }
        (right && position == frames, played_in_all)
    });
    assert!(right, "playback went wrong or never finished");
    assert!(played >= frames + 50_000);
    thread.stop().unwrap();
}

#[test]
fn the_recorder_never_uses_the_heap_on_the_audio_side() {
    let writer = AudioWriter::wav(Cursor::new(Vec::new()), 2, 48_000, WavFormat::Int24).unwrap();
    let config = RecorderConfig {
        capacity_frames: 2_048,
    };
    let (mut recorder, mut worker) = RecordingWriter::new(writer, config);
    let stereo = ramp(2, 1_500);
    let mono = ramp(1, 700);
    let wide = ramp(4, 300);
    let mut rng = Rng::new(4);

    let mut pushed = 0u64;
    let mut taken = 0u64;
    for round in 0..300 {
        // Often enough the worker does not run, and the queue overflows.
        if round % 4 == 0 {
            worker.drain().unwrap();
        }
        let block = [&stereo, &mono, &wide][rng.below(3) as usize];
        pushed += block.num_frames() as u64;
        taken += assert_no_heap_use("RecordingWriter", || {
            let taken = recorder.push(block);
            let _ = (recorder.free_frames(), recorder.num_channels());
            let _ = (recorder.overruns(), recorder.worker_stopped());
            taken
        }) as u64;
    }
    assert!(recorder.overrun_frames() > 0, "the test never overflowed");
    assert_eq!(taken + recorder.overrun_frames(), pushed);

    let bytes = worker.finish().unwrap().into_inner();
    let file = wav::read(bytes.as_slice()).unwrap();
    assert_eq!(file.buffer.num_frames() as u64, taken);
}

#[test]
fn the_recorder_never_uses_the_heap_while_a_real_thread_drains_it() {
    let writer = AudioWriter::wav(Cursor::new(Vec::new()), 2, 48_000, WavFormat::Float32).unwrap();
    let (mut recorder, worker) = RecordingWriter::new(writer, RecorderConfig::default());
    let thread = worker.spawn().unwrap();
    let block = ramp(2, 480);

    let taken = assert_no_heap_use("RecordingWriter beside its thread", || {
        let mut taken = 0u64;
        for _ in 0..2_000 {
            taken += recorder.push(&block) as u64;
        }
        taken
    });
    assert_eq!(taken + recorder.overrun_frames(), 2_000 * 480);

    let bytes = thread.finish().unwrap().into_inner();
    let file = wav::read(bytes.as_slice()).unwrap();
    assert_eq!(file.buffer.num_frames() as u64, taken);
}

/// A 16-bit stereo WAV file of any length that exists only as a rule: the
/// sample at each place is worked out when it is read. Past 4 GiB it is
/// RF64, as it has to be.
struct EndlessWav {
    frames: u64,
    position: u64,
}

impl EndlessWav {
    const HEADER: u64 = 80;

    fn len(&self) -> u64 {
        Self::HEADER + self.frames * 4
    }

    /// The value of a sample, by its number counted across both channels.
    fn sample(index: u64) -> i16 {
        (index.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 48) as i16
    }

    fn header(&self) -> [u8; 80] {
        let data_len = self.frames * 4;
        let mut header = [0u8; 80];
        header[0..4].copy_from_slice(b"RF64");
        header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        header[8..16].copy_from_slice(b"WAVEds64");
        header[16..20].copy_from_slice(&28u32.to_le_bytes());
        header[20..28].copy_from_slice(&(self.len() - 8).to_le_bytes());
        header[28..36].copy_from_slice(&data_len.to_le_bytes());
        header[36..44].copy_from_slice(&self.frames.to_le_bytes());
        header[48..56].copy_from_slice(b"fmt \x10\0\0\0");
        // PCM, two channels, 48 kHz, 192000 bytes a second, four bytes a
        // frame, 16 bits.
        header[56..72].copy_from_slice(&[
            1, 0, 2, 0, 0x80, 0xBB, 0, 0, 0x00, 0xEE, 0x02, 0, 4, 0, 16, 0,
        ]);
        header[72..76].copy_from_slice(b"data");
        header[76..80].copy_from_slice(&u32::MAX.to_le_bytes());
        header
    }
}

impl Read for EndlessWav {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = (self.len() - self.position.min(self.len())).min(out.len() as u64) as usize;
        let header = self.header();
        for (offset, byte) in out[..count].iter_mut().enumerate() {
            let at = self.position + offset as u64;
            *byte = if at < Self::HEADER {
                header[at as usize]
            } else {
                let at = at - Self::HEADER;
                Self::sample(at / 2).to_le_bytes()[(at % 2) as usize]
            };
        }
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for EndlessWav {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.position = match to {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(delta) => self.len().checked_add_signed(delta).unwrap(),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta).unwrap(),
        };
        Ok(self.position)
    }
}

#[test]
fn a_wav_reader_holds_one_block_however_long_the_file() {
    // A minute, and a month, of stereo audio at 48 kHz: 11 megabytes and
    // 498 gigabytes.
    let mut held = Vec::new();
    for frames in [48_000 * 60, 48_000 * 60 * 60 * 24 * 30u64] {
        let mut block = AudioBuffer::<f32>::new(2, 4_096);
        let mut rng = Rng::new(frames);

        let (mut reader, opening) = watch(|| {
            AudioReader::new(EndlessWav {
                frames,
                position: 0,
            })
            .unwrap()
        });
        assert_eq!(reader.len_frames(), Some(frames));

        // After opening, reading and seeking do not touch the heap at all.
        let right = assert_no_heap_use("reading a WAV file", || {
            let mut right = true;
            let mut check = |reader: &mut AudioReader<EndlessWav>, at: u64| {
                right &= reader.seek(at).is_ok_and(|landed| landed == at);
                let got = reader.read(&mut block).unwrap_or(0);
                right &= got as u64 == 4_096.min(frames - at);
                for (channel, samples) in block.iter_channels().enumerate() {
                    for (offset, &sample) in samples[..got].iter().enumerate() {
                        let index = (at + offset as u64) * 2 + channel as u64;
                        right &= sample == f32::from(EndlessWav::sample(index)) / 32_768.0;
                    }
                }
            };
            // From the start, all over the file, and up to its last frame.
            for index in 0..40 {
                check(&mut reader, index * 4_096);
            }
            for _ in 0..40 {
                check(&mut reader, rng.below(frames));
            }
            check(&mut reader, frames - 4_096);
            check(&mut reader, frames - 7);
            check(&mut reader, frames);
            right
        });
        assert!(right, "a frame of the {frames}-frame file was read wrong");
        println!(
            "WAV of {frames} frames: the reader holds {} bytes, allocated in {} calls",
            opening.most_held, opening.calls
        );
        held.push(opening.most_held);
    }
    // The same for both, and about the 32 KiB the reader moves at a time.
    assert_eq!(held[0], held[1]);
    assert!(held[0] < 40 * 1_024, "{} bytes", held[0]);
}

/// A FLAC stream of `frames` frames of 4096 mono samples, every eighth one
/// noise and the others a held value.
fn long_flac(frames: u64) -> (Vec<u8>, impl Fn(u64) -> i32) {
    let sample = |index: u64| {
        if (index / 4_096) % 8 == 3 {
            flac_sample(16, 0, index * 3 + 1) | 1
        } else {
            (index / 4_096 % 251) as i32 - 125
        }
    };
    let spec = FlacSpec::new(1, 16, 4_096);
    let stream = write_flac(&spec, frames * 4_096, |_, index| sample(index));
    (stream.bytes, sample)
}

#[test]
fn a_flac_reader_holds_one_frame_however_long_the_file() {
    // Seven seconds, and two and a quarter minutes, at 48 kHz.
    let mut held = Vec::new();
    for frames in [80u64, 1_600] {
        let (bytes, sample) = long_flac(frames);
        let total = frames * 4_096;
        let mut block = AudioBuffer::<f32>::new(1, 4_096);
        let mut rng = Rng::new(frames);

        let (right, heap_use) = watch(|| {
            let mut reader = AudioReader::new(Cursor::new(bytes.as_slice())).unwrap();
            let mut right = reader.len_frames() == Some(total);
            // All of it in order, then seeks all over it.
            let mut position = 0;
            loop {
                let got = reader.read(&mut block).unwrap();
                if got == 0 {
                    break;
                }
                right &= block.channel(0)[0] == flac_scale(sample(position), 16);
                right &=
                    block.channel(0)[got - 1] == flac_scale(sample(position + got as u64 - 1), 16);
                position += got as u64;
            }
            right &= position == total;
            for _ in 0..200 {
                let at = rng.below(total);
                right &= reader.seek(at).unwrap() == at;
                let got = reader.read(&mut block).unwrap();
                right &= got as u64 == 4_096.min(total - at);
                right &= block.channel(0)[0] == flac_scale(sample(at), 16);
            }
            right
        });
        assert!(right, "the stream of {frames} frames was read wrong");
        println!(
            "FLAC of {} bytes, {total} samples: the reader held at most {} bytes",
            bytes.len(),
            heap_use.most_held
        );
        held.push(heap_use.most_held);
    }
    // Twenty times the audio, the same memory: a window of the file, one
    // decoded frame, and nothing that grows.
    assert_eq!(held[0], held[1]);
    assert!(held[0] < 128 * 1_024, "{} bytes", held[0]);
}

/// Opens data that may claim anything, reads from it, seeks in it, and
/// reports the most memory the reader took for that. `None` if the data
/// was refused.
fn memory_for(bytes: &[u8]) -> Option<usize> {
    let (reader, opening) = watch(|| AudioReader::new(Cursor::new(bytes)).ok());
    let mut reader = reader?;
    // The block is the caller's, and its size the caller's choice.
    let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 16);
    let (_, reading) = watch(|| {
        for target in [0, 1_000, u64::MAX, 5] {
            let _ = reader.seek(target);
            let _ = reader.read(&mut block);
        }
    });
    // What was held after opening, and the most that came on top of it.
    Some(opening.most_held + reading.most_held)
}

#[test]
fn a_header_cannot_ask_for_memory_without_bound() {
    let measure = |what: &str, bytes: &[u8], opens: bool| {
        let held = memory_for(bytes);
        println!("{what}: {held:?} bytes for a file of {}", bytes.len());
        assert_eq!(held.is_some(), opens, "{what}");
        // Every case here is a file of a few kilobytes. Whatever it claims,
        // reading it takes no more than a megabyte or so, set by the limits
        // of the format and not by the numbers in the header.
        assert!(held.unwrap_or(0) < 2 << 20, "{what}: {held:?} bytes");
    };

    // WAV: 8191 channels of 64-bit float, the most a frame of 65535 bytes
    // holds, and a data chunk of four gigabytes that is not there.
    let mut wav = b"RIFF\xFF\xFF\xFF\xFFWAVEfmt \x10\0\0\0".to_vec();
    wav.extend_from_slice(&3u16.to_le_bytes());
    wav.extend_from_slice(&8_191u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&u32::MAX.to_le_bytes());
    wav.extend_from_slice(&(8_191u16 * 8).to_le_bytes());
    wav.extend_from_slice(&64u16.to_le_bytes());
    wav.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
    wav.resize(wav.len() + 3_000, 7);
    measure("WAV with 8191 channels", &wav, true);

    // RF64: a data size of a million terabytes.
    let mut rf64 = b"RF64\xFF\xFF\xFF\xFFWAVEds64\x1C\0\0\0".to_vec();
    rf64.extend_from_slice(&u64::MAX.to_le_bytes());
    rf64.extend_from_slice(&(1u64 << 60).to_le_bytes());
    rf64.extend_from_slice(&u64::MAX.to_le_bytes());
    rf64.extend_from_slice(&0u32.to_le_bytes());
    rf64.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x02\0\x80\xBB\0\0\0\xEE\x02\0\x04\0\x10\0");
    rf64.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
    rf64.resize(rf64.len() + 3_000, 7);
    measure("RF64 that claims an exabyte", &rf64, true);

    // AIFF: 65535 channels of 32 bits and four thousand million frames.
    let mut aiff = b"FORM\xFF\xFF\xFF\xFFAIFFCOMM\0\0\0\x12".to_vec();
    aiff.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 32]);
    aiff.extend_from_slice(&[0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]);
    aiff.extend_from_slice(b"SSND\xFF\xFF\xFF\xFF\0\0\0\0\0\0\0\0");
    aiff.resize(aiff.len() + 3_000, 7);
    measure("AIFF with 65535 channels", &aiff, true);

    // FLAC: eight channels of 32 bits, the largest frame size the header
    // can state, the longest stream it can state, and frames that are not
    // there. Then the same with a frame header for 65536 samples, which
    // promises eight megabits a channel and delivers nothing.
    let mut flac = b"fLaC\x80\0\0\x22".to_vec();
    flac.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0xFF, 0xFF, 0xFF]);
    // 48 kHz, eight channels, 32 bits, 2^36 - 1 samples.
    flac.extend_from_slice(&[0x0B, 0xB8, 0x0F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    flac.extend_from_slice(&[0; 16]);
    let info_only = flac.clone();
    measure("FLAC with no frames", &info_only, true);

    let mut header = vec![0xFF, 0xF8, 0x70, 0x70, 0x00, 0xFF, 0xFF];
    header.push(common::crc8(&header));
    flac.extend_from_slice(&header);
    // Eight verbatim subframes begin; the file ends long before they do.
    flac.resize(flac.len() + 3_000, 0x02);
    measure(
        "FLAC with a frame of 65536 samples that is not there",
        &flac,
        true,
    );

    // A seek table of sixteen megabytes, by its header: only the header is
    // there.
    let mut table = info_only.clone();
    table[4] = 0;
    table.extend_from_slice(&[0x83, 0xFF, 0xFF, 0xFE]);
    table.resize(table.len() + 3_000, 1);
    measure("FLAC with a seek table that is not there", &table, false);

    // And one that is there: a hundred thousand points in 1.8 megabytes,
    // of which no more are kept than seeking can use.
    let spec = FlacSpec::new(1, 16, 256);
    let stream = write_flac(&spec, 2_000, |_, index| flac_sample(16, 0, index));
    let points: Vec<(u64, u64, u16)> = (0..100_000u64).map(|index| (index, index, 1)).collect();
    let crowded = common::with_seek_table(&stream.bytes, &points);
    assert!(crowded.len() > 1_800_000);
    let held = memory_for(&crowded).unwrap();
    println!("FLAC with 100000 seek points: {held} bytes");
    assert!(held < 700 * 1_024, "{held} bytes");
}

#[test]
fn a_flac_writer_allocates_when_it_is_made_and_never_again() {
    // The destination is a slice of fixed size, so that whatever touches
    // the heap is the encoder.
    let mut disk = vec![0u8; 4 << 20];
    let stereo = ramp(2, 100_000);
    let mut interleaved = vec![0.0f32; stereo.num_frames() * 2];
    stereo.copy_to_interleaved(&mut interleaved);
    let pieces: Vec<AudioBuffer<f32>> = [1, 700, 4_096, 9_999]
        .iter()
        .map(|&len| common::slice(&stereo, 0..len))
        .collect();

    for level in [0, 5, 8] {
        let options = FlacOptions::new(24).with_level(level);
        let destination = Cursor::new(disk.as_mut_slice());
        let (mut writer, made) =
            watch(|| AudioWriter::flac(destination, 2, 48_000, &options).unwrap());
        println!("FLAC writer at level {level}: {} bytes", made.most_held);

        assert_no_heap_use("writing FLAC", || {
            for chunk in interleaved.chunks(3_002) {
                writer.write_interleaved(chunk).unwrap();
            }
            for piece in &pieces {
                writer.write(piece).unwrap();
            }
            writer.write_silence(50_000).unwrap();
        });
        let written = writer.finish().unwrap().position() as usize;
        let expected = 100_000 + 1 + 700 + 4_096 + 9_999 + 50_000;
        let file = manguebyte_formats::flac::read(&disk[..written]).unwrap();
        assert_eq!(file.buffer.num_frames(), expected);
    }
}

/// Opens `bytes`, reads all of it in blocks of 4096 frames and then seeks
/// all over it, and reports the most memory the reader took and the frames
/// it read.
fn read_and_seek(bytes: &[u8]) -> (u64, usize) {
    let (frames, heap_use) = watch(|| {
        let mut reader = AudioReader::new(Cursor::new(bytes)).unwrap();
        let mut block = AudioBuffer::<f32>::new(reader.num_channels(), 4_096);
        let mut frames = 0u64;
        loop {
            let got = reader.read(&mut block).unwrap();
            if got == 0 {
                break;
            }
            frames += got as u64;
        }
        let mut rng = Rng::new(3);
        for _ in 0..50 {
            reader.seek(rng.below(frames)).unwrap();
            reader.read(&mut block).unwrap();
        }
        frames
    });
    (frames, heap_use.most_held)
}

#[test]
fn a_vorbis_reader_holds_the_same_memory_however_long_the_file() {
    let (_, stereo) = read_and_seek(include_bytes!("data/vorbis-2ch-44100-q3.ogg"));
    println!("Vorbis, 44.1 kHz stereo at quality 3: {stereo} bytes");
    // Files longer than the stretch of the file the reader reads at a time,
    // which is less for a shorter file.
    let file = include_bytes!("data/vorbis-1ch-8000-q5.ogg").as_slice();
    let mut held = Vec::new();
    for times in [12, 36] {
        let bytes = common::repeat_vorbis(file, times);
        let (frames, most) = read_and_seek(&bytes);
        println!(
            "Vorbis of {} bytes, {frames} frames: the reader held at most {most} bytes",
            bytes.len()
        );
        held.push(most);
    }
    // Three times the audio, the same memory: the codebooks, a window of
    // the file, a block per channel and the packets at hand. Which packets
    // the seeks land on, and so how much room was made for the longest of
    // them, is all that differs.
    assert!(held[0].abs_diff(held[1]) < 4_096, "{held:?}");
    assert!(held[0] < 512 * 1_024, "{} bytes", held[0]);
}

#[test]
fn a_vorbis_header_cannot_ask_for_memory_without_bound() {
    use common::vorbis::{Coding, headers_with_setup, setup_with_large_codebook, stream};

    // 255 channels and blocks of 8192 samples, the most the format has:
    // what that takes is set by the format, and is the most a Vorbis
    // stream can ask for.
    let silence = vec![vec![vec![0; 4_096]; 255]; 3];
    let bytes = stream(
        255,
        &silence,
        Coding::Residue {
            kind: 1,
            listed: false,
        },
    );
    let held = memory_for(&bytes).unwrap();
    println!("Vorbis of 255 channels and blocks of 8192: {held} bytes");
    assert!(held < 14 << 20, "{held} bytes");

    // A codebook of sixteen million entries in a few bytes is refused
    // before anything is made of it.
    let bytes = headers_with_setup(&setup_with_large_codebook(0xFF_FFFF, 24));
    let (refused, opening) = watch(|| AudioReader::new(Cursor::new(bytes.as_slice())).is_err());
    assert!(refused);
    println!(
        "Vorbis with a codebook of 2^24 entries: {} bytes",
        opening.most_held
    );
    assert!(opening.most_held < 256 * 1_024);

    // One of 262144 entries of 18 bits, a whole code and the most entries
    // allowed, takes the most while the setup is read.
    let bytes = headers_with_setup(&setup_with_large_codebook(1 << 18, 18));
    let (_, opening) = watch(|| AudioReader::new(Cursor::new(bytes.as_slice())).is_err());
    println!(
        "Vorbis with a codebook of 2^18 entries: {} bytes",
        opening.most_held
    );
    assert!(opening.most_held < 8 << 20);
}
