//! Recording from the audio thread, with the writing done elsewhere.

use std::io::{self, Seek, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use manguebyte_core::AudioBuffer;
use manguebyte_core::spsc::{self, Consumer, Producer};

use crate::{AudioWriter, FormatError};

/// How much audio a [`RecordingWriter`] can hold while the disk is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecorderConfig {
    /// Frames the queue between the audio thread and the worker holds. It
    /// is rounded up so that the queue's size in samples is a power of two.
    /// At least 1.
    pub capacity_frames: usize,
}

impl Default for RecorderConfig {
    /// 65536 frames: well over a second at 48 kHz, which rides out the
    /// pauses of an ordinary disk.
    fn default() -> Self {
        Self {
            capacity_frames: 65_536,
        }
    }
}

/// How many frames the audio side interleaves at a time, and the worker
/// writes at a time.
const CHUNK_FRAMES: usize = 1_024;

/// What the two sides know about each other without a queue.
struct Shared {
    /// Set when the worker is gone and nothing will be written any more.
    worker_gone: AtomicBool,
}

/// Records audio from the audio thread without ever writing a file there.
///
/// An audio callback must not do I/O. So [`push`](Self::push), on the audio
/// thread, only copies each block into a wait-free queue, and a
/// [`RecorderWorker`], on a thread of its own, takes the audio out of the
/// queue and writes it with an [`AudioWriter`].
///
/// If the disk falls behind and the queue fills up, `push` does not wait.
/// It drops the frames that do not fit and counts them in
/// [`overrun_frames`](Self::overrun_frames).
///
/// ```
/// use std::io::Cursor;
///
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_formats::wav::{self, WavFormat};
/// use manguebyte_formats::{AudioWriter, RecorderConfig, RecordingWriter};
///
/// let writer = AudioWriter::wav(Cursor::new(Vec::new()), 2, 48_000, WavFormat::Float32)?;
/// let (mut recorder, worker) = RecordingWriter::new(writer, RecorderConfig::default());
/// // The worker writes on its own thread until it is told to finish.
/// let thread = worker.spawn()?;
///
/// // On the audio thread: never blocks, never allocates.
/// let mut block = AudioBuffer::<f32>::new(2, 128);
/// block.channel_mut(1).fill(0.5);
/// for _ in 0..10 {
///     recorder.push(&block);
/// }
/// assert_eq!(recorder.overrun_frames(), 0);
///
/// // Elsewhere, once recording is over: write what is queued and complete
/// // the file.
/// let bytes = thread.finish()?.into_inner();
/// let file = wav::read(bytes.as_slice()).unwrap();
/// assert_eq!(file.buffer.num_frames(), 1_280);
/// # Ok::<(), manguebyte_formats::FormatError>(())
/// ```
///
/// # Real-time safety
///
/// Every method of this type is safe to call from the audio thread: none
/// allocates, locks, blocks or does I/O. Dropping it frees memory, so drop
/// it elsewhere.
///
/// # Limits
///
/// Frames lost to an overrun are left out of the file, not replaced by
/// silence, so what follows them moves earlier. A recording that must stay
/// aligned with something else needs a queue large enough never to overrun.
pub struct RecordingWriter {
    samples: Producer<f32>,
    shared: Arc<Shared>,
    /// Where a block is interleaved before it enters the queue.
    scratch: Vec<f32>,
    channels: usize,
    /// Whether the previous push lost frames.
    overrunning: bool,
    overruns: u64,
    overrun_frames: u64,
}

impl RecordingWriter {
    /// Splits recording to `writer` in two: the recorder for the audio
    /// thread, and the worker that writes for it.
    ///
    /// Nothing is written until the worker runs: put it on a thread with
    /// [`RecorderWorker::spawn`], or call [`RecorderWorker::step`] yourself.
    /// This allocates everything the two will ever use.
    pub fn new<W: Write + Seek>(
        writer: AudioWriter<W>,
        config: RecorderConfig,
    ) -> (Self, RecorderWorker<W>) {
        let channels = writer.num_channels();
        let capacity = config.capacity_frames.max(1).saturating_mul(channels);
        let (samples_tx, samples_rx) = spsc::channel(capacity);
        // How long the queue takes to fill, which sets how often a worker
        // thread has to look at it.
        let queue_frames = samples_tx.capacity() / channels;
        let queue_seconds = queue_frames as f64 / f64::from(writer.sample_rate().max(1));
        let shared = Arc::new(Shared {
            worker_gone: AtomicBool::new(false),
        });
        let recorder = Self {
            samples: samples_tx,
            shared: Arc::clone(&shared),
            scratch: vec![0.0; CHUNK_FRAMES * channels],
            channels,
            overrunning: false,
            overruns: 0,
            overrun_frames: 0,
        };
        let worker = RecorderWorker {
            writer: Some(writer),
            samples: samples_rx,
            shared,
            scratch: vec![0.0; CHUNK_FRAMES * channels],
            channels,
            queue_seconds,
        };
        (recorder, worker)
    }

    /// Channels of the file being written.
    pub fn num_channels(&self) -> usize {
        self.channels
    }

    /// Queues the frames of `block` to be written and returns how many were
    /// taken. Frames that do not fit, always the last ones of the block, are
    /// lost and counted.
    ///
    /// If `block` has fewer channels than the file, the missing ones are
    /// recorded as silence; if it has more, the extra ones are left out.
    pub fn push(&mut self, block: &AudioBuffer<f32>) -> usize {
        let frames = block.num_frames();
        // Whole frames only. The worker can only make more room, so what
        // fits now still fits when it is pushed.
        let room = self.samples.free_len() / self.channels;
        let taken = frames.min(room);
        let shared_channels = self.channels.min(block.num_channels());

        let mut done = 0;
        while done < taken {
            let count = (taken - done).min(CHUNK_FRAMES);
            let chunk = &mut self.scratch[..count * self.channels];
            if shared_channels < self.channels {
                chunk.fill(0.0);
            }
            for index in 0..shared_channels {
                let source = &block.channel(index)[done..done + count];
                let slots = chunk[index..].iter_mut().step_by(self.channels);
                for (slot, &sample) in slots.zip(source) {
                    *slot = sample;
                }
            }
            self.samples.push_slice(chunk);
            done += count;
        }

        let lost = frames - taken;
        if lost > 0 {
            self.overrun_frames += lost as u64;
            if !self.overrunning {
                self.overruns += 1;
            }
        }
        // A block of no frames says nothing about the queue either way.
        if frames > 0 {
            self.overrunning = lost > 0;
        }
        taken
    }

    /// How many frames can be pushed right now without any being lost.
    pub fn free_frames(&self) -> usize {
        self.samples.free_len() / self.channels
    }

    /// How many frames were lost because the queue was full.
    pub fn overrun_frames(&self) -> u64 {
        self.overrun_frames
    }

    /// How many times the queue overflowed: each run of consecutive
    /// [`push`](Self::push) calls that lost frames counts once.
    pub fn overruns(&self) -> u64 {
        self.overruns
    }

    /// Whether the worker has stopped, so that nothing pushed from now on
    /// will be written: the file was finished, or writing failed.
    pub fn worker_stopped(&self) -> bool {
        self.shared.worker_gone.load(Ordering::Relaxed)
    }
}

/// The half of a [`RecordingWriter`] that writes the file.
///
/// It belongs on a thread that may block: [`spawn`](Self::spawn) starts one.
/// A program with its own worker threads, or a test, calls
/// [`step`](Self::step) and [`finish`](Self::finish) instead.
pub struct RecorderWorker<W: Write + Seek> {
    /// Taken out when the file is finished.
    writer: Option<AudioWriter<W>>,
    samples: Consumer<f32>,
    shared: Arc<Shared>,
    scratch: Vec<f32>,
    channels: usize,
    /// How long the queue takes to fill at the file's sample rate.
    queue_seconds: f64,
}

impl<W: Write + Seek> RecorderWorker<W> {
    /// Does one piece of work: writes up to 1024 of the frames that are
    /// waiting.
    ///
    /// Returns `true` if anything was written and `false` if the queue was
    /// empty. If writing fails, those of the frames taken out of the queue
    /// that the destination did not accept are lost; the next step carries
    /// on after the ones it did accept.
    pub fn step(&mut self) -> Result<bool, FormatError> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(false);
        };
        // The audio thread pushes whole frames, so whole frames are here.
        let waiting = self.samples.len() / self.channels * self.channels;
        let count = waiting.min(self.scratch.len());
        if count == 0 {
            return Ok(false);
        }
        let chunk = &mut self.scratch[..count];
        self.samples.pop_slice(chunk);
        writer.write_interleaved(chunk)?;
        Ok(true)
    }

    /// Writes everything that is waiting, and returns how many frames that
    /// was.
    pub fn drain(&mut self) -> Result<u64, FormatError> {
        let before = self.frames_written();
        while self.step()? {}
        Ok(self.frames_written() - before)
    }

    /// Frames written to the file so far.
    pub fn frames_written(&self) -> u64 {
        self.writer.as_ref().map_or(0, AudioWriter::frames_written)
    }

    /// Writes everything that is waiting, completes the file's header and
    /// gives back the underlying writer.
    ///
    /// Stop pushing first: what the audio thread pushes after this has
    /// looked at the queue is not written.
    pub fn finish(mut self) -> Result<W, FormatError> {
        self.complete()
    }

    fn complete(&mut self) -> Result<W, FormatError> {
        let drained = self.drain();
        let writer = self.writer.take();
        self.shared.worker_gone.store(true, Ordering::Relaxed);
        // The header is completed even if the last write failed, so that
        // what did reach the file can be read.
        let finished = writer
            .ok_or_else(|| io::Error::other("the file was already finished"))?
            .finish();
        drained?;
        finished
    }
}

impl<W: Write + Seek + Send + 'static> RecorderWorker<W> {
    /// Moves the worker to a new thread, which writes what is pushed until
    /// the returned handle is told to finish, or is dropped.
    ///
    /// The thread sleeps briefly whenever the queue is empty. If writing
    /// fails it ends, and [`RecorderThread::finish`] then returns the
    /// error. Before it ends it tries to complete the header for the frames
    /// the destination took, so that they can be read. On a disk that is
    /// full this works when the destination is the file itself, as
    /// [`AudioWriter::create_wav`], [`AudioWriter::create_aiff`] and
    /// [`AudioWriter::create_flac`] give it, and not behind a buffer: see
    /// [If writing fails](AudioWriter#if-writing-fails). Whether it worked
    /// is not reported: the error returned is the one that stopped the
    /// recording.
    pub fn spawn(mut self) -> io::Result<RecorderThread<W>> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        // Often enough that the queue does not fill between two rounds: a
        // quarter of the time it takes to fill, within reason.
        let pause = Duration::from_secs_f64((self.queue_seconds / 4.0).clamp(0.001, 0.02));
        let handle = thread::Builder::new()
            .name("manguebyte-recorder".into())
            .spawn(move || -> Result<W, FormatError> {
                loop {
                    // Read the flag before looking at the queue, so that
                    // whatever was pushed before the request to finish is
                    // written.
                    let stopping = stopping.load(Ordering::Acquire);
                    match self.step() {
                        Ok(true) => {}
                        Ok(false) if stopping => return self.complete(),
                        Ok(false) => thread::park_timeout(pause),
                        Err(error) => {
                            // Leave a readable file behind; the first error
                            // is the one worth reporting.
                            if let Some(writer) = self.writer.take() {
                                let _ = writer.finish();
                            }
                            return Err(error);
                        }
                    }
                }
            })?;
        Ok(RecorderThread {
            stop,
            handle: Some(handle),
        })
    }
}

impl<W: Write + Seek> Drop for RecorderWorker<W> {
    fn drop(&mut self) {
        self.shared.worker_gone.store(true, Ordering::Relaxed);
    }
}

/// A thread running a [`RecorderWorker`].
///
/// Dropping it finishes the file as [`finish`](Self::finish) does, with any
/// error lost. Call `finish` to learn whether the recording is complete.
pub struct RecorderThread<W> {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<W, FormatError>>>,
}

impl<W> RecorderThread<W> {
    /// Has the thread write what is still queued and complete the file's
    /// header, waits for it, and gives back the underlying writer.
    ///
    /// Stop pushing first: what the audio thread pushes while this runs may
    /// or may not be written.
    pub fn finish(mut self) -> Result<W, FormatError> {
        self.join()
            .unwrap_or_else(|| Err(io::Error::other("the file was already finished").into()))
    }

    /// Whether the thread has ended on its own, which it only does when
    /// writing fails.
    pub fn has_failed(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn join(&mut self) -> Option<Result<W, FormatError>> {
        let handle = self.handle.take()?;
        self.stop.store(true, Ordering::Release);
        handle.thread().unpark();
        Some(
            handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the writing thread panicked").into())),
        )
    }
}

impl<W> Drop for RecorderThread<W> {
    fn drop(&mut self) {
        let _ = self.join();
    }
}
