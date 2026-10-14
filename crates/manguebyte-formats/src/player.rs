//! Playing a file from the audio thread, with the reading done elsewhere.

use std::io::{self, Read, Seek};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use manguebyte_core::AudioBuffer;
use manguebyte_core::spsc::{self, Consumer, Producer};

use crate::{AudioReader, FormatError};

/// How much a [`StreamingPlayer`] reads ahead, and in what pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerConfig {
    /// Frames the worker reads at a time. At least 1.
    pub block_frames: usize,
    /// How many such blocks exist. All but the one being played can wait
    /// ahead of the audio thread, so the read-ahead is close to
    /// `block_frames * blocks` frames. At least 2.
    pub blocks: usize,
}

impl Default for PlayerConfig {
    /// Sixteen blocks of 4096 frames: about a second and a third at 48 kHz,
    /// and half a megabyte for a stereo file.
    fn default() -> Self {
        Self {
            block_frames: 4_096,
            blocks: 16,
        }
    }
}

/// A stretch of the file on its way from the worker to the audio thread.
///
/// Blocks are allocated once and then travel in a circle: the worker fills
/// one and sends it, the audio thread plays it and sends it back.
struct Block {
    buffer: AudioBuffer<f32>,
    /// Which seek this audio was read after. The audio thread drops blocks
    /// from before its latest seek.
    generation: u64,
    /// Position in the file of the block's first frame.
    start: u64,
    /// Position at which the audio in this block first jumps back to
    /// `loop_start`. `u64::MAX` if it never does.
    first_wrap: u64,
    loop_start: u64,
    /// Frames between later jumps.
    loop_len: u64,
    /// Whether the file ended with this block.
    last: bool,
}

impl Block {
    /// Position in the file of the frame `offset` frames into the block.
    fn position_at(&self, offset: usize) -> u64 {
        let straight = self.start + offset as u64;
        if straight < self.first_wrap {
            straight
        } else {
            self.loop_start + (straight - self.first_wrap) % self.loop_len.max(1)
        }
    }
}

/// What the audio side asks of the worker.
#[derive(Debug, Clone, Copy)]
enum Command {
    Seek { frame: u64, generation: u64 },
    Loop(Option<(u64, u64)>),
}

/// What the two sides know about each other without a queue.
struct Shared {
    /// Set when the worker is gone, so that silence after that is not
    /// counted as the worker being late.
    worker_gone: AtomicBool,
}

/// Plays an audio file from the audio thread without ever reading it there.
///
/// An audio callback must not do I/O: a disk can take longer than the few
/// milliseconds a block has. So the reading is split off. A
/// [`PlayerWorker`], on a thread of its own, reads the file ahead of time
/// into blocks and passes them through a wait-free queue; this type, on the
/// audio thread, only copies from the blocks that have arrived.
///
/// If the worker falls behind, [`process`](Self::process) does not wait for
/// it. It fills what is missing with silence, counts the missing frames in
/// [`underrun_frames`](Self::underrun_frames), and plays on from the same
/// place once the audio arrives: nothing is skipped.
///
/// ```
/// use std::io::Cursor;
///
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_formats::wav::{self, WavFormat};
/// use manguebyte_formats::{AudioReader, PlayerConfig, StreamingPlayer};
///
/// // A short file, held in memory for the example.
/// let mut source = AudioBuffer::<f32>::new(1, 1_000);
/// source.channel_mut(0).fill(0.25);
/// let mut bytes = Vec::new();
/// wav::write(&mut bytes, &source, 48_000, WavFormat::Float32).unwrap();
///
/// let reader = AudioReader::new(Cursor::new(bytes))?;
/// let (mut player, worker) = StreamingPlayer::new(reader, PlayerConfig::default());
/// // The worker reads on its own thread until it is stopped or dropped.
/// let thread = worker.spawn()?;
///
/// // On the audio thread: never blocks, never allocates.
/// let mut block = AudioBuffer::<f32>::new(1, 128);
/// let mut played = 0;
/// while !player.is_finished() {
///     played += player.process(&mut block);
/// #   std::thread::yield_now();
/// }
/// assert_eq!(played, 1_000);
/// thread.stop()?;
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
/// A change of loop applies to audio that has not been read ahead yet, so it
/// is heard up to one read-ahead later. Call [`seek`](Self::seek) with the
/// current [`position`](Self::position) to apply it at once, at the cost of
/// a gap while the worker reads again. The same gap comes by itself in one
/// case: a loop set when the end of the file had already been read ahead
/// still takes playback back to its start, but only once the end has been
/// played and the worker has read again. There is no fade at a seek, at the
/// seam of a loop or around an underrun: what the file holds is what is
/// played.
pub struct StreamingPlayer {
    filled: Consumer<Box<Block>>,
    free: Producer<Box<Block>>,
    commands: Producer<Command>,
    shared: Arc<Shared>,
    /// The block being played, and how far into it.
    current: Option<Box<Block>>,
    offset: usize,
    generation: u64,
    /// Whether nothing has arrived yet since the start or the last seek.
    awaiting: bool,
    /// Whether the last block of the file has been played to its end.
    finished: bool,
    /// Whether the previous call ran out of audio.
    starving: bool,
    position: u64,
    /// The loop asked for last, as `(start, end)`.
    looping: Option<(u64, u64)>,
    /// Commands the queue had no room for; sent on the next call.
    pending_seek: Option<u64>,
    pending_loop: Option<Option<(u64, u64)>>,
    underruns: u64,
    underrun_frames: u64,
    channels: usize,
    sample_rate: u32,
    len_frames: Option<u64>,
    block_frames: usize,
    /// How many blocks there are in all.
    blocks: usize,
}

impl StreamingPlayer {
    /// Splits the playing of `reader` in two: the player for the audio
    /// thread, and the worker that reads for it.
    ///
    /// Nothing is read until the worker runs: put it on a thread with
    /// [`PlayerWorker::spawn`], or call [`PlayerWorker::step`] yourself.
    /// This allocates every block the two will ever use.
    pub fn new<R: Read + Seek>(
        reader: AudioReader<R>,
        config: PlayerConfig,
    ) -> (Self, PlayerWorker<R>) {
        let block_frames = config.block_frames.max(1);
        let blocks = config.blocks.max(2);
        let channels = reader.num_channels();

        let (filled_tx, filled_rx) = spsc::channel(blocks);
        let (mut free_tx, free_rx) = spsc::channel(blocks);
        let (commands_tx, commands_rx) = spsc::channel(32);
        for _ in 0..blocks {
            let block = Box::new(Block {
                buffer: AudioBuffer::new(channels, block_frames),
                generation: 0,
                start: 0,
                first_wrap: u64::MAX,
                loop_start: 0,
                loop_len: 0,
                last: false,
            });
            // The queue holds at least `blocks` items, so this cannot fail.
            let _ = free_tx.push(block);
        }
        let shared = Arc::new(Shared {
            worker_gone: AtomicBool::new(false),
        });

        let player = Self {
            filled: filled_rx,
            free: free_tx,
            commands: commands_tx,
            shared: Arc::clone(&shared),
            current: None,
            offset: 0,
            generation: 0,
            awaiting: true,
            finished: false,
            starving: false,
            position: reader.position(),
            looping: None,
            pending_seek: None,
            pending_loop: None,
            underruns: 0,
            underrun_frames: 0,
            channels,
            sample_rate: reader.sample_rate(),
            len_frames: reader.len_frames(),
            block_frames,
            blocks,
        };
        let worker = PlayerWorker {
            reader,
            filled: filled_tx,
            free: free_rx,
            commands: commands_rx,
            shared,
            generation: 0,
            seek_to: None,
            looping: None,
            ended: false,
            spare: None,
            block_frames,
        };
        (player, worker)
    }

    /// Channels of the file.
    pub fn num_channels(&self) -> usize {
        self.channels
    }

    /// Sample rate of the file in hertz. The player does not resample.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Length of the file in frames, when the file says.
    pub fn len_frames(&self) -> Option<u64> {
        self.len_frames
    }

    /// Fills `out` with the next frames of the file and returns how many
    /// frames of audio it wrote. The rest of `out`, always at its end, is
    /// silence.
    ///
    /// The count is short when the file has ended, when a seek has not been
    /// answered yet, when the end of the file was reached before the worker
    /// had heard of a loop that leads back from it, or when the worker is
    /// late. Only the last is an underrun.
    ///
    /// If `out` has more channels than the file, the extra ones are
    /// silent; if it has fewer, the file's extra channels are not played.
    pub fn process(&mut self, out: &mut AudioBuffer<f32>) -> usize {
        self.send_commands();
        let frames = out.num_frames();
        let mut done = 0;
        let mut stale = 0;
        // The loop goes on for as long as blocks are used up, even once
        // `out` is full: the block after the last frame may be the empty one
        // that says the file has ended.
        while let Some(block) = self.current.take().or_else(|| self.filled.pop()) {
            if block.generation != self.generation {
                // Read before the latest seek. No more of these are dropped
                // in one call than there are blocks, so that the call is
                // bounded whatever the worker does meanwhile.
                self.recycle(block);
                stale += 1;
                if stale > self.blocks {
                    break;
                }
                continue;
            }
            if self.awaiting {
                self.awaiting = false;
                self.position = block.start;
            }
            self.finished = false;

            let available = block.buffer.num_frames() - self.offset;
            let count = available.min(frames - done);
            for (index, channel) in out.iter_channels_mut().enumerate() {
                let destination = &mut channel[done..done + count];
                if index < block.buffer.num_channels() {
                    let source = &block.buffer.channel(index)[self.offset..self.offset + count];
                    destination.copy_from_slice(source);
                } else {
                    destination.fill(0.0);
                }
            }
            done += count;
            self.offset += count;
            self.position = block.position_at(self.offset);

            if self.offset < block.buffer.num_frames() {
                // `out` is full and the block is not used up.
                self.current = Some(block);
                break;
            }
            self.finished = block.last;
            self.offset = 0;
            self.recycle(block);
        }
        // The worker read the end of the file before it knew of the loop
        // that is set now, and has sent nothing since.
        self.go_round_from_the_end();

        for channel in out.iter_channels_mut() {
            channel[done..].fill(0.0);
        }
        if frames > 0 {
            // Silence is the worker's fault only if audio was due: not after
            // the end, not while a seek is being answered, not without a
            // worker.
            let late = done < frames
                && !self.finished
                && !self.awaiting
                && !self.shared.worker_gone.load(Ordering::Relaxed);
            if late {
                self.underrun_frames += (frames - done) as u64;
                if !self.starving {
                    self.underruns += 1;
                }
            }
            self.starving = late;
        }
        done
    }

    /// Moves playback to `frame`.
    ///
    /// What was read ahead is thrown away, and [`process`](Self::process)
    /// plays silence until the worker has read from the new place. That
    /// silence is not counted as an underrun. A frame past the end moves to
    /// the end, and from the end a loop that is set leads back to its start
    /// as it does in playback: with such a loop, a seek to the end is a seek
    /// to the start of the loop.
    pub fn seek(&mut self, frame: u64) {
        let frame = self.landing(frame);
        if let Some(block) = self.current.take() {
            self.recycle(block);
        }
        // Everything already queued was read for the old position. Blocks
        // the worker sends before it hears of the seek carry the old
        // generation, and `process` drops them.
        for _ in 0..self.blocks {
            let Some(block) = self.filled.pop() else {
                break;
            };
            self.recycle(block);
        }
        self.generation += 1;
        self.offset = 0;
        self.awaiting = true;
        self.finished = false;
        self.starving = false;
        self.position = frame;
        self.pending_seek = Some(frame);
        self.send_commands();
    }

    /// Where a seek to `frame` takes playback: to the frame itself, to the
    /// end of the file if the frame lies past it, and from the end to the
    /// start of the loop, if a loop is set that leads back from there.
    ///
    /// The worker would work out the same, but only when it gets to it. By
    /// working it out here the player can say where it is in the meantime.
    fn landing(&self, frame: u64) -> u64 {
        let Some(len) = self.len_frames else {
            // Only the worker finds out where such a file ends.
            return frame;
        };
        match self.looping {
            Some((start, end)) if frame >= len && start < end && start < len => start,
            _ => frame.min(len),
        }
    }

    /// Makes playback repeat a stretch of the file, or stop repeating.
    ///
    /// With `Some(start..end)`, playback that reaches `end` carries on from
    /// `start` without a gap. An `end` past the end of the file means the
    /// end of the file, so `0..u64::MAX` loops the whole file whatever its
    /// length. Playback that is already past `end` runs to the end of the
    /// file and loops from there. An empty range does not loop, and neither
    /// does one that starts at or past the end of the file.
    ///
    /// The change applies to audio not read ahead yet: see the limits of
    /// this type.
    ///
    /// If the file has been played to its end, or a seek to its end has not
    /// been answered yet, there is nothing read ahead, and a loop that leads
    /// back from the end starts playback again at once: this is then the
    /// same as setting the loop and calling [`seek`](Self::seek) with its
    /// start.
    pub fn set_loop(&mut self, region: Option<Range<u64>>) {
        let region = region.map(|region| (region.start, region.end));
        self.looping = region;
        self.pending_loop = Some(region);
        self.send_commands();
        self.go_round_from_the_end();
    }

    /// If playback stands at the end of the file with nothing read ahead,
    /// and the loop that is set leads back from there, goes back to the
    /// start of the loop as a seek does.
    ///
    /// The worker goes round by itself when it reads the end of the file
    /// with a loop set. This is for the end it read without one: it cannot
    /// be told when playback gets there, and until it has read on, nothing
    /// would say that there is more to come.
    fn go_round_from_the_end(&mut self) {
        // Playback stands at the end when it has finished, the position
        // then being the end whether or not the file says how long it is,
        // and when it waits for a seek that went there.
        let at_end = self.finished || (self.awaiting && self.len_frames == Some(self.position));
        match self.looping {
            Some((start, end)) if at_end && start < end && start < self.position => {
                self.seek(start);
            }
            _ => {}
        }
    }

    /// Position in the file of the next frame [`process`](Self::process)
    /// will play. It follows loops, and after a seek it is the frame
    /// playback will start from.
    ///
    /// One case cannot be told in advance. In a file that does not say how
    /// long it is, only the worker finds out that a seek went past the end:
    /// until it has answered, this is the frame that was asked for.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Whether the file has been played to its end and playback has nowhere
    /// to go from there.
    ///
    /// With a loop set that leads back from the end of the file this is
    /// never true. Seeking makes it false again, and so does setting such a
    /// loop, both at once: neither waits for the worker.
    ///
    /// For a file that says how long it is, this is true from the call that
    /// played its last frame. For one that does not, it is true once the
    /// worker has read on and found the end, which may be a call or two
    /// later.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Whether a seek, or the start of playback, is still waiting for the
    /// worker's first block. Going back to the start of a loop from an end
    /// of the file that was read without the loop is such a seek.
    pub fn is_seeking(&self) -> bool {
        self.awaiting
    }

    /// About how many frames are ready to be played without the worker
    /// doing anything more.
    ///
    /// This counts whole blocks, so it is high by up to a block when the
    /// end of the file has been read ahead.
    pub fn buffered_frames(&self) -> usize {
        let current = self
            .current
            .as_ref()
            .map_or(0, |block| block.buffer.num_frames() - self.offset);
        current + self.filled.len() * self.block_frames
    }

    /// How many frames of silence were played because the worker was late.
    pub fn underrun_frames(&self) -> u64 {
        self.underrun_frames
    }

    /// How many times playback ran dry: each run of consecutive
    /// [`process`](Self::process) calls that came up short counts once.
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// Whether the worker has stopped: it was dropped, or its thread ended
    /// after an error. What it had already read still plays.
    pub fn worker_stopped(&self) -> bool {
        self.shared.worker_gone.load(Ordering::Relaxed)
    }

    /// Sends the block back to the worker to be filled again.
    fn recycle(&mut self, block: Box<Block>) {
        if let Err(block) = self.free.push(block) {
            // Unreachable, because the queue has room for every block there
            // is. Were it to happen, the block must still not be freed on
            // the audio thread, so it is leaked instead.
            std::mem::forget(block);
        }
    }

    /// Hands the worker whatever it has not been told yet. If the command
    /// queue is full, which takes a worker that has not run for dozens of
    /// calls, the latest of each kind is kept and sent later.
    fn send_commands(&mut self) {
        if let Some(region) = self.pending_loop {
            if self.commands.push(Command::Loop(region)).is_err() {
                return;
            }
            self.pending_loop = None;
        }
        if let Some(frame) = self.pending_seek {
            let generation = self.generation;
            if self
                .commands
                .push(Command::Seek { frame, generation })
                .is_err()
            {
                return;
            }
            self.pending_seek = None;
        }
    }
}

/// The half of a [`StreamingPlayer`] that reads the file.
///
/// It belongs on a thread that may block: [`spawn`](Self::spawn) starts one.
/// A program with its own worker threads, or a test, calls
/// [`step`](Self::step) instead.
pub struct PlayerWorker<R> {
    reader: AudioReader<R>,
    filled: Producer<Box<Block>>,
    free: Consumer<Box<Block>>,
    commands: Consumer<Command>,
    shared: Arc<Shared>,
    generation: u64,
    seek_to: Option<u64>,
    looping: Option<(u64, u64)>,
    /// Whether the end of the file has been read and sent.
    ended: bool,
    /// A block taken out of circulation by a read that failed.
    spare: Option<Box<Block>>,
    block_frames: usize,
}

impl<R: Read + Seek> PlayerWorker<R> {
    /// Does one piece of work: answers what the player has asked, then reads
    /// one block and sends it.
    ///
    /// Returns `true` if a block was sent, and `false` if there was nothing
    /// to do, because the read-ahead is full or the file has ended. After an
    /// error the worker can be stepped again; it will try the same read.
    pub fn step(&mut self) -> Result<bool, FormatError> {
        while let Some(command) = self.commands.pop() {
            match command {
                Command::Seek { frame, generation } => {
                    self.generation = generation;
                    self.seek_to = Some(frame);
                    self.ended = false;
                }
                Command::Loop(region) => {
                    self.looping = region;
                    // A file that had ended may have somewhere to go again.
                    self.ended &= region.is_none();
                }
            }
        }
        if let Some(frame) = self.seek_to {
            self.reader.seek(frame)?;
            self.seek_to = None;
        }
        if self.ended {
            return Ok(false);
        }
        let Some(mut block) = self.spare.take().or_else(|| self.free.pop()) else {
            return Ok(false);
        };
        if let Err(error) = self.fill(&mut block) {
            self.spare = Some(block);
            return Err(error);
        }
        if let Err(block) = self.filled.push(block) {
            // Unreachable: the queue has room for every block there is.
            self.spare = Some(block);
        }
        Ok(true)
    }

    /// Steps until there is nothing left to do, and returns how many blocks
    /// were sent.
    pub fn fill_ahead(&mut self) -> Result<usize, FormatError> {
        let mut blocks = 0;
        while self.step()? {
            blocks += 1;
        }
        Ok(blocks)
    }

    /// Reads the next stretch of the file into `block`, following the loop.
    fn fill(&mut self, block: &mut Block) -> Result<(), FormatError> {
        let start = self.reader.position();
        block.generation = self.generation;
        block.start = start;
        block.first_wrap = u64::MAX;
        block.loop_start = 0;
        block.loop_len = 0;
        block.last = false;
        block.buffer.set_num_frames(self.block_frames);

        // A range that holds nothing is no loop at all.
        let looping = self.looping.filter(|&(start, end)| start < end);
        let mut filled = 0;
        // Whether the loop has just sent the reader back. If nothing can be
        // read from there either, going round again would never end.
        let mut wrapped = false;
        let outcome = loop {
            if filled == self.block_frames {
                break Ok(());
            }
            let before = self.reader.position();
            // How far this stretch may run before the loop sends it back.
            let room = match looping {
                Some((_, end)) if before < end => end - before,
                _ => u64::MAX,
            };
            let want = usize::try_from(room)
                .unwrap_or(usize::MAX)
                .min(self.block_frames - filled);
            let reader = &mut self.reader;
            let got = match block
                .buffer
                .windowed(filled..filled + want, |window| reader.read(window))
            {
                Ok(got) => got,
                Err(error) => break Err(error),
            };
            filled += got;

            let after = self.reader.position();
            let at_loop_end = matches!(looping, Some((_, end)) if before < end && after == end);
            // A file that says how long it is ends with the block that
            // reaches its end. One that does not ends with a read that
            // comes back short, which may be a block later.
            let at_file_end = got < want || self.reader.len_frames() == Some(after);
            if !at_loop_end && !at_file_end {
                wrapped = false;
                continue;
            }
            let stuck = wrapped && got == 0;
            match looping {
                // Somewhere to go back to that is not where we are.
                Some((loop_start, loop_end)) if loop_start < after && !stuck => {
                    wrapped = true;
                    if block.first_wrap == u64::MAX {
                        block.first_wrap = after;
                        block.loop_start = loop_start;
                        // Later rounds end at the loop's end, or at the end
                        // of the file if the loop's end lies beyond it.
                        block.loop_len = after.min(loop_end) - loop_start;
                    }
                    if let Err(error) = self.reader.seek(loop_start) {
                        break Err(error);
                    }
                }
                _ => {
                    block.last = true;
                    self.ended = true;
                    break Ok(());
                }
            }
        };
        block.buffer.set_num_frames(filled);
        if outcome.is_err() {
            // Read the same stretch again next time, not what follows it.
            self.seek_to = Some(start);
        }
        outcome
    }
}

impl<R: Read + Seek + Send + 'static> PlayerWorker<R> {
    /// Moves the worker to a new thread, which reads ahead until the
    /// returned handle is stopped or dropped.
    ///
    /// The thread sleeps briefly whenever the read-ahead is full. It ends by
    /// itself if reading fails; [`PlayerThread::stop`] then returns the
    /// error.
    pub fn spawn(mut self) -> io::Result<PlayerThread> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        // A quarter of the time one block takes to play, within reason.
        let block_seconds = self.block_frames as f64 / f64::from(self.reader.sample_rate().max(1));
        let pause = Duration::from_secs_f64((block_seconds / 4.0).clamp(0.001, 0.02));
        let handle = thread::Builder::new()
            .name("manguebyte-player".into())
            .spawn(move || -> Result<(), FormatError> {
                while !stopping.load(Ordering::Relaxed) {
                    if !self.step()? {
                        thread::park_timeout(pause);
                    }
                }
                Ok(())
            })?;
        Ok(PlayerThread {
            stop,
            handle: Some(handle),
        })
    }
}

impl<R> Drop for PlayerWorker<R> {
    fn drop(&mut self) {
        self.shared.worker_gone.store(true, Ordering::Relaxed);
    }
}

/// A thread running a [`PlayerWorker`].
///
/// Dropping it stops the thread and waits for it to end.
pub struct PlayerThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), FormatError>>>,
}

impl PlayerThread {
    /// Stops the thread, waits for it to end, and reports the error that
    /// ended it early, if one did.
    pub fn stop(mut self) -> Result<(), FormatError> {
        self.join()
    }

    /// Whether the thread has ended on its own, which it only does when
    /// reading fails.
    pub fn has_failed(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn join(&mut self) -> Result<(), FormatError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::Relaxed);
        handle.thread().unpark();
        handle
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the reading thread panicked").into()))
    }
}

impl Drop for PlayerThread {
    fn drop(&mut self) {
        let _ = self.join();
    }
}
