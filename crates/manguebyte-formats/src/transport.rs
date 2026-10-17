//! A file as a source with a transport: play, pause, stop, seek and loop,
//! controlled from any thread.

use std::io::{Read, Seek};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use manguebyte_core::AudioBuffer;
use manguebyte_core::source::{AudioSource, PositionableSource};
use manguebyte_core::spsc::{self, Consumer, Producer};

use crate::{AudioReader, PlayerConfig, PlayerWorker, StreamingPlayer};

/// How many commands can wait for the audio thread.
const COMMANDS: usize = 64;

/// What a [`TransportControl`] asks of its [`TransportSource`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    Play,
    Pause,
    Stop,
    Seek(u64),
    Loop(Option<(u64, u64)>),
    Gain(f32),
    Declick(bool),
}

/// What the audio side says about itself, for any thread to read.
#[derive(Debug)]
struct Shared {
    playing: AtomicBool,
    finished: AtomicBool,
    seeking: AtomicBool,
    position: AtomicU64,
    underruns: AtomicU64,
    underrun_frames: AtomicU64,
    gain: AtomicU32,
}

/// Plays an audio file as an [`AudioSource`], with a transport: play,
/// pause, stop, seek and loop.
///
/// It is the audio thread's half of three. [`new`](Self::new) also gives
/// the [`PlayerWorker`], which reads the file ahead on a thread of its own
/// (start it with [`PlayerWorker::spawn`]), and a [`TransportControl`],
/// with which any other thread works the transport and reads where it is.
/// The worker hands audio over through a wait-free queue, and the control
/// its commands through another, so pulling never waits, allocates or does
/// I/O. When the worker falls behind, the missing audio is silence,
/// counted in [`TransportControl::underrun_frames`], and playback carries
/// on from where it was once the audio arrives: nothing is skipped.
///
/// Seeking lands on the exact frame: the first frame played afterwards is
/// the one asked for. Until the worker has read from there, the source
/// plays silence, which is not counted as an underrun. A loop goes round
/// without a gap, sample for sample, when it is set before playback reads
/// past its end.
///
/// The file plays at its own sample rate, whatever rate the source is
/// prepared for: put it in a `ResamplingSource` of `manguebyte-dsp` to
/// play it at another.
///
/// ```
/// use std::io::Cursor;
///
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_core::source::AudioSource;
/// use manguebyte_formats::wav::{self, WavFormat};
/// use manguebyte_formats::{AudioReader, PlayerConfig, TransportSource};
///
/// let mut audio = AudioBuffer::<f32>::new(1, 1_000);
/// audio.channel_mut(0).fill(0.25);
/// let mut bytes = Vec::new();
/// wav::write(&mut bytes, &audio, 48_000, WavFormat::Float32).unwrap();
///
/// let reader = AudioReader::new(Cursor::new(bytes))?;
/// let (mut transport, mut worker, control) = TransportSource::new(reader, PlayerConfig::default());
/// control.set_declick(false);
/// control.seek(100);
/// control.play();
///
/// // The commands arrive with the next pull, which plays silence while
/// // the worker has not read from frame 100 yet...
/// let mut block = AudioBuffer::<f32>::new(1, 256);
/// transport.pull(&mut block);
/// assert!(control.is_seeking());
/// // ...which it would do on its own thread, started with `worker.spawn()`.
/// worker.fill_ahead()?;
/// transport.pull(&mut block);
/// assert_eq!(block.channel(0), &[0.25; 256]);
/// assert_eq!(control.position(), 356);
/// assert!(control.is_playing());
/// # Ok::<(), manguebyte_formats::FormatError>(())
/// ```
///
/// # Clicks
///
/// With declicking on, which is the default, starting and pausing fade in
/// and out over one block, and a change of gain glides over one block, so
/// that nothing clicks. A pause therefore stops a block after it is asked
/// for, and the position moves on by that block. A seek, a loop's seam
/// and an underrun are cuts.
///
/// # Real-time safety
///
/// [`pull`](AudioSource::pull) and every method of
/// [`PositionableSource`] are real-time safe. Dropping the source frees
/// memory, so drop it elsewhere.
pub struct TransportSource {
    player: StreamingPlayer,
    commands: Consumer<Command>,
    shared: Arc<Shared>,
    playing: bool,
    /// A pause asked for while declicking: the next block fades out.
    fading_out: bool,
    /// Where to go once the fade out is over, for a stop.
    seek_after_fade: Option<u64>,
    gain: f32,
    /// The gain the last block ended on.
    heard_gain: f32,
    declick: bool,
    looping: bool,
}

/// Works a [`TransportSource`] from any thread, and tells where it is.
///
/// Commands reach the source at the start of its next pull, in the order
/// they were given; each returns `false` if too many are already waiting,
/// which takes a source that has not been pulled for a while. Cloning the
/// control gives another handle on the same source.
#[derive(Clone)]
pub struct TransportControl {
    commands: Arc<Mutex<Producer<Command>>>,
    shared: Arc<Shared>,
    sample_rate: u32,
    channels: usize,
    len_frames: Option<u64>,
}

impl TransportSource {
    /// Splits the playing of `reader` in three: the source for the audio
    /// thread, the worker that reads for it, and the control. Playback is
    /// stopped at the reader's position. Allocates every block the source
    /// and the worker will use.
    pub fn new<R: Read + Seek>(
        reader: AudioReader<R>,
        config: PlayerConfig,
    ) -> (Self, PlayerWorker<R>, TransportControl) {
        let (player, worker) = StreamingPlayer::new(reader, config);
        let (producer, consumer) = spsc::channel(COMMANDS);
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            seeking: AtomicBool::new(true),
            position: AtomicU64::new(player.position()),
            underruns: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            gain: AtomicU32::new(1.0f32.to_bits()),
        });
        let control = TransportControl {
            commands: Arc::new(Mutex::new(producer)),
            shared: Arc::clone(&shared),
            sample_rate: player.sample_rate(),
            channels: player.num_channels(),
            len_frames: player.len_frames(),
        };
        let source = Self {
            player,
            commands: consumer,
            shared,
            playing: false,
            fading_out: false,
            seek_after_fade: None,
            gain: 1.0,
            heard_gain: 0.0,
            declick: true,
            looping: false,
        };
        (source, worker, control)
    }

    /// Starts playing, at once. If the file has been played to its end it
    /// starts again from the beginning. Real-time safe.
    pub fn play(&mut self) {
        if self.player.is_finished() {
            self.player.seek(0);
        }
        if !self.playing || self.fading_out {
            if !self.playing {
                self.heard_gain = if self.declick { 0.0 } else { self.gain };
            }
            self.playing = true;
            self.fading_out = false;
            self.seek_after_fade = None;
        }
    }

    /// Pauses, keeping the position. Real-time safe.
    pub fn pause(&mut self) {
        if self.playing {
            if self.declick {
                self.fading_out = true;
            } else {
                self.playing = false;
            }
        }
    }

    /// Pauses and goes back to the start. Real-time safe.
    pub fn stop(&mut self) {
        if self.playing && self.declick {
            self.fading_out = true;
            self.seek_after_fade = Some(0);
        } else {
            self.playing = false;
            self.player.seek(0);
        }
    }

    /// Whether it is playing.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Repeats `start..end` of the file, or stops repeating with `None`;
    /// see [`StreamingPlayer::set_loop`] for what a range means. Real-time
    /// safe.
    pub fn set_loop(&mut self, region: Option<Range<u64>>) {
        self.looping = region
            .as_ref()
            .is_some_and(|region| region.start < region.end);
        self.player.set_loop(region);
    }

    /// Sets the gain, a factor where 1 leaves the file as it is. With
    /// declicking on it glides there over the next block. Real-time safe.
    pub fn set_gain(&mut self, gain: f32) {
        if gain.is_finite() {
            self.gain = gain.max(0.0);
        }
    }

    /// Turns declicking on or off; see the type's documentation.
    /// Real-time safe.
    pub fn set_declick(&mut self, declick: bool) {
        self.declick = declick;
    }

    /// The file's sample rate, in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.player.sample_rate()
    }

    /// The file's channel count.
    pub fn num_channels(&self) -> usize {
        self.player.num_channels()
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Stop => self.stop(),
            Command::Seek(frame) => self.player.seek(frame),
            Command::Loop(region) => self.set_loop(region.map(|(start, end)| start..end)),
            Command::Gain(gain) => self.set_gain(gain),
            Command::Declick(declick) => self.set_declick(declick),
        }
    }

    fn publish(&self) {
        let shared = &self.shared;
        shared.playing.store(self.playing, Ordering::Relaxed);
        shared
            .finished
            .store(self.player.is_finished(), Ordering::Relaxed);
        shared
            .seeking
            .store(self.player.is_seeking(), Ordering::Relaxed);
        shared
            .position
            .store(self.player.position(), Ordering::Relaxed);
        shared
            .underruns
            .store(self.player.underruns(), Ordering::Relaxed);
        shared
            .underrun_frames
            .store(self.player.underrun_frames(), Ordering::Relaxed);
        shared.gain.store(self.gain.to_bits(), Ordering::Relaxed);
    }
}

impl AudioSource for TransportSource {
    /// Nothing to prepare: the blocks were allocated by
    /// [`new`](TransportSource::new), and the file plays at its own rate.
    fn prepare(&mut self, _sample_rate: f64, _max_block_size: usize) {}

    fn pull(&mut self, out: &mut AudioBuffer<f32>) {
        // At most as many commands as the queue holds, so the call stays
        // bounded whatever the other threads do meanwhile.
        for _ in 0..COMMANDS {
            let Some(command) = self.commands.pop() else {
                break;
            };
            self.apply(command);
        }
        if !self.playing {
            out.clear();
            self.publish();
            return;
        }
        self.player.process(out);
        let target = if self.fading_out { 0.0 } else { self.gain };
        let start = if self.declick {
            self.heard_gain
        } else {
            target
        };
        if start != target {
            out.apply_gain_ramp(start, target);
        } else if target != 1.0 {
            out.apply_gain(target);
        }
        self.heard_gain = target;
        if self.fading_out {
            self.fading_out = false;
            self.playing = false;
            if let Some(frame) = self.seek_after_fade.take() {
                self.player.seek(frame);
            }
        }
        if self.player.is_finished() {
            self.playing = false;
        }
        self.publish();
    }
}

impl PositionableSource for TransportSource {
    fn position(&self) -> u64 {
        self.player.position()
    }

    fn seek(&mut self, frame: u64) {
        self.player.seek(frame);
    }

    fn len_frames(&self) -> Option<u64> {
        self.player.len_frames()
    }

    fn is_looping(&self) -> bool {
        self.looping
    }
}

impl TransportControl {
    fn send(&self, command: Command) -> bool {
        let mut commands = self
            .commands
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        commands.push(command).is_ok()
    }

    /// Starts playing; from the beginning if the file had been played to
    /// its end.
    pub fn play(&self) -> bool {
        self.send(Command::Play)
    }

    /// Pauses, keeping the position.
    pub fn pause(&self) -> bool {
        self.send(Command::Pause)
    }

    /// Pauses and goes back to the start.
    pub fn stop(&self) -> bool {
        self.send(Command::Stop)
    }

    /// Moves playback to `frame`, playing or not.
    pub fn seek(&self, frame: u64) -> bool {
        self.send(Command::Seek(frame))
    }

    /// Repeats a stretch of the file, or stops repeating.
    pub fn set_loop(&self, region: Option<Range<u64>>) -> bool {
        self.send(Command::Loop(
            region.map(|region| (region.start, region.end)),
        ))
    }

    /// Sets the gain, a factor where 1 leaves the file as it is.
    pub fn set_gain(&self, gain: f32) -> bool {
        self.send(Command::Gain(gain))
    }

    /// Turns declicking on or off.
    pub fn set_declick(&self, declick: bool) -> bool {
        self.send(Command::Declick(declick))
    }

    /// Whether the source was playing at the end of its last pull.
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Whether the file had been played to its end, with no loop to go
    /// round, at the end of the last pull.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    /// Whether a seek was still waiting for the worker at the end of the
    /// last pull.
    pub fn is_seeking(&self) -> bool {
        self.shared.seeking.load(Ordering::Relaxed)
    }

    /// The frame the next pull plays from, as of the end of the last pull.
    pub fn position(&self) -> u64 {
        self.shared.position.load(Ordering::Relaxed)
    }

    /// The position in seconds.
    pub fn position_seconds(&self) -> f64 {
        self.position() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// How many times playback ran dry because the worker was late.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// How many frames of silence were played because the worker was late.
    pub fn underrun_frames(&self) -> u64 {
        self.shared.underrun_frames.load(Ordering::Relaxed)
    }

    /// The gain as of the last pull.
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
    }

    /// The file's sample rate, in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The file's channel count.
    pub fn num_channels(&self) -> usize {
        self.channels
    }

    /// The file's length in frames, when it says.
    pub fn len_frames(&self) -> Option<u64> {
        self.len_frames
    }
}
