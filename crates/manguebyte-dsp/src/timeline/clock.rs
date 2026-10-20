use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use manguebyte_core::AtomicF64;
use manguebyte_core::spsc::{self, Consumer, Producer};

use super::map::{MusicalPosition, SignaturePoint, TempoMap, TempoPoint};
use crate::Transport;

/// How many commands, other than tempo maps, can wait for the clock.
const COMMANDS: usize = 64;

/// How many tempo maps can be sent and not yet collected, besides the one
/// playing.
const MAPS_IN_FLIGHT: usize = 4;

/// A loop as it was asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LoopSpec {
    /// In quarter notes: the frames follow the tempo map.
    Beats(f64, f64),
    /// In frames, whatever the map says.
    Frames(i64, i64),
}

/// What a [`ClockControl`] asks of its [`TransportClock`].
enum Command {
    Play,
    Stop,
    Recording(bool),
    SeekFrame(i64),
    SeekBeat(f64),
    Loop(Option<LoopSpec>),
    Map(Box<TempoMap>),
}

/// What the clock says about itself, for any thread to read.
#[derive(Debug)]
struct Shared {
    playing: AtomicBool,
    recording: AtomicBool,
    frame: AtomicI64,
    beat: AtomicF64,
    tempo: AtomicF64,
}

/// The queues to a [`ClockControl`], when there is one.
struct Link {
    commands: Consumer<Command>,
    retired: Producer<Box<TempoMap>>,
    shared: Arc<Shared>,
}

/// A transport an application owns and drives, block by block: play and
/// stop, a position, a loop and seeking, on the timeline of a
/// [`TempoMap`].
///
/// Each call to [`next_block`](Self::next_block) describes the coming block
/// as a [`ClockBlock`] and moves the position on. The position is a whole
/// frame, and everything musical about a frame is worked out from the map
/// afresh, never added up block by block, so the beat, tempo and bar of
/// every frame are the same whatever lengths the blocks have.
///
/// # Loops
///
/// A loop is a range of frames `[start, end)`, given in frames or in
/// quarter notes, which [land](TempoMap::frame_at_beat) on frames. When
/// playback reaches `end` it goes on from `start`, on the very next frame,
/// as many times as a block needs. A loop applies once the position is
/// before its end: started after it, playback runs on. An empty or
/// backwards range is no loop.
///
/// # Threads
///
/// Every method that takes `&mut self` is real-time safe: no allocation,
/// lock or system call. Another thread works the clock through the
/// [`ClockControl`] of [`with_control`](Self::with_control), whose commands
/// reach the clock at the start of its next block, in order. A new tempo
/// map travels in a box, and the one it replaces travels back to the
/// control to be freed there, so the audio thread never frees one.
/// Dropping the clock frees its map: do it off the audio thread.
///
/// ```
/// use manguebyte_dsp::timeline::{TempoCurve, TempoMap, TimeSignature, TransportClock};
///
/// let mut map = TempoMap::new(120.0, TimeSignature::FOUR_FOUR)?;
/// map.set_tempo(4.0, 60.0, TempoCurve::Step)?;
/// let mut clock = TransportClock::new(map, 48_000.0);
/// clock.set_loop_beats(Some((2.0, 6.0)));
/// clock.play();
///
/// // Beat 2 is frame 48 000, the tempo halves on beat 4 (frame 96 000),
/// // and beat 6 is frame 192 000. One block of 200 000 frames goes round.
/// let block = clock.next_block(200_000);
/// let pieces: Vec<_> = block.pieces().map(|p| (p.offset, p.frames, p.start_frame)).collect();
/// assert_eq!(pieces, [(0, 192_000, 0), (192_000, 8_000, 48_000)]);
///
/// // The tempo change and the loop are on their frames.
/// let timeline = block.with_map(clock.map());
/// assert_eq!(timeline.tempo_at(95_999), 120.0);
/// assert_eq!(timeline.tempo_at(96_000), 60.0);
/// assert_eq!(timeline.beat_at(192_000), 2.0);
/// assert_eq!(clock.position(), 56_000);
/// # Ok::<(), manguebyte_dsp::timeline::TempoMapError>(())
/// ```
pub struct TransportClock {
    map: Box<TempoMap>,
    sample_rate: f64,
    playing: bool,
    recording: bool,
    /// The frame the next block starts on.
    position: i64,
    looping: Option<LoopSpec>,
    /// The loop in frames, when it is a loop.
    loop_frames: Option<(i64, i64)>,
    /// Whether the position moved other than by playing since the last
    /// block.
    jumped: bool,
    /// Whether the last block ended on the loop's end, so that the next
    /// starts at its start.
    wrapped: bool,
    link: Option<Link>,
}

impl fmt::Debug for TransportClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportClock")
            .field("sample_rate", &self.sample_rate)
            .field("playing", &self.playing)
            .field("recording", &self.recording)
            .field("position", &self.position)
            .field("loop_frames", &self.loop_frames)
            .finish_non_exhaustive()
    }
}

impl TransportClock {
    /// A stopped clock at frame 0 of `map`, at `sample_rate`, worked only
    /// through its own methods.
    ///
    /// # Panics
    ///
    /// Panics if `sample_rate` is not a positive, finite number.
    pub fn new(map: TempoMap, sample_rate: f64) -> Self {
        assert!(
            sample_rate.is_finite() && sample_rate > 0.0,
            "a sample rate must be positive and finite, not {sample_rate}"
        );
        Self {
            map: Box::new(map),
            sample_rate,
            playing: false,
            recording: false,
            position: 0,
            looping: None,
            loop_frames: None,
            jumped: false,
            wrapped: false,
            link: None,
        }
    }

    /// A stopped clock, and the [`ClockControl`] with which other threads
    /// work it and read where it is.
    ///
    /// # Panics
    ///
    /// As [`new`](Self::new).
    pub fn with_control(map: TempoMap, sample_rate: f64) -> (Self, ClockControl) {
        let mut clock = Self::new(map, sample_rate);
        // The map playing counts as in flight: it comes back when replaced.
        let limit = MAPS_IN_FLIGHT + 1;
        let (commands, command_receiver) = spsc::channel(COMMANDS + limit);
        let (retired_sender, retired) = spsc::channel(limit);
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            frame: AtomicI64::new(0),
            beat: AtomicF64::new(0.0),
            tempo: AtomicF64::new(clock.map.tempo_at_beat(0.0)),
        });
        clock.link = Some(Link {
            commands: command_receiver,
            retired: retired_sender,
            shared: Arc::clone(&shared),
        });
        let control = ClockControl {
            inner: Mutex::new(ControlInner {
                commands,
                retired,
                in_flight: 1,
                limit,
            }),
            shared,
        };
        (clock, control)
    }

    /// The tempo map being played.
    pub fn map(&self) -> &TempoMap {
        &self.map
    }

    /// The sample rate.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Whether the clock is playing.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Whether the clock says it records.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// The frame the next block starts on.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// The loop in frames, if one is set and not empty.
    pub fn loop_frames(&self) -> Option<(i64, i64)> {
        self.loop_frames
    }

    /// Starts playing from the position. Real-time safe.
    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stops, keeping the position. Real-time safe.
    pub fn stop(&mut self) {
        self.playing = false;
    }

    /// Sets whether the clock says it records; it changes nothing else.
    /// Real-time safe.
    pub fn set_recording(&mut self, recording: bool) {
        self.recording = recording;
    }

    /// Moves to `frame`. Real-time safe.
    pub fn seek_frame(&mut self, frame: i64) {
        if frame != self.position {
            self.jumped = true;
        }
        self.position = frame;
        self.wrapped = false;
    }

    /// Moves to the frame `beat` lands on. A beat that is not finite is
    /// ignored. Real-time safe.
    pub fn seek_beat(&mut self, beat: f64) {
        if beat.is_finite() {
            self.seek_frame(self.map.frame_at_beat(beat, self.sample_rate));
        }
    }

    /// Loops between two positions in quarter notes, or stops looping with
    /// `None`. The loop's frames follow the map when it changes. A range
    /// that is not finite is ignored. Real-time safe.
    pub fn set_loop_beats(&mut self, range: Option<(f64, f64)>) {
        if range.is_some_and(|(start, end)| !start.is_finite() || !end.is_finite()) {
            return;
        }
        self.looping = range.map(|(start, end)| LoopSpec::Beats(start, end));
        self.update_loop();
    }

    /// Loops between two frames, `[start, end)`, or stops looping with
    /// `None`. Real-time safe.
    pub fn set_loop_frames(&mut self, range: Option<(i64, i64)>) {
        self.looping = range.map(|(start, end)| LoopSpec::Frames(start, end));
        self.update_loop();
    }

    fn update_loop(&mut self) {
        self.loop_frames = match self.looping {
            Some(LoopSpec::Beats(start, end)) => Some((
                self.map.frame_at_beat(start, self.sample_rate),
                self.map.frame_at_beat(end, self.sample_rate),
            )),
            Some(LoopSpec::Frames(start, end)) => Some((start, end)),
            None => None,
        }
        // A loop must have a length, and one that fits in an `i64`.
        .filter(|(start, end)| end.checked_sub(*start).is_some_and(|length| length > 0));
    }

    /// Plays `map` from the next block, and returns the map it replaces
    /// for the caller to drop off the audio thread. Real-time safe.
    ///
    /// The musical position stays: the frame moves to where the current
    /// beat lands on the new map, and a loop given in quarter notes moves
    /// with it. A move counts as a jump ([`ClockBlock::jumped`]).
    pub fn replace_map(&mut self, map: Box<TempoMap>) -> Box<TempoMap> {
        let beat = self.map.beat_at_frame(self.position, self.sample_rate);
        let old = std::mem::replace(&mut self.map, map);
        let frame = self.map.frame_at_beat(beat, self.sample_rate);
        if frame != self.position {
            self.position = frame;
            self.jumped = true;
            self.wrapped = false;
        }
        self.update_loop();
        old
    }

    /// Changes the sample rate, keeping the musical position as
    /// [`replace_map`](Self::replace_map) does. A rate that is not
    /// positive and finite is ignored. Real-time safe.
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        if !(sample_rate.is_finite() && sample_rate > 0.0) || sample_rate == self.sample_rate {
            return;
        }
        let beat = self.map.beat_at_frame(self.position, self.sample_rate);
        self.sample_rate = sample_rate;
        let frame = self.map.frame_at_beat(beat, sample_rate);
        if frame != self.position {
            self.position = frame;
            self.jumped = true;
            self.wrapped = false;
        }
        // A loop given in quarter notes moves to its new frames; one given
        // in frames stays on them.
        self.update_loop();
    }

    /// The transport the next block will start with, if nothing changes
    /// first.
    pub fn transport(&self) -> Transport {
        self.peek(0).transport(&self.map)
    }

    fn peek(&self, frames: usize) -> ClockBlock {
        ClockBlock {
            start_frame: self.position,
            frames,
            playing: self.playing,
            recording: self.recording,
            loop_frames: self.loop_frames,
            loop_beats: self.loop_beats(),
            jumped: self.jumped,
            wrapped: self.wrapped,
            sample_rate: self.sample_rate,
        }
    }

    fn loop_beats(&self) -> Option<(f64, f64)> {
        match (self.looping, self.loop_frames) {
            (Some(LoopSpec::Beats(start, end)), Some(_)) => Some((start, end)),
            (_, Some((start, end))) => Some((
                self.map.beat_at_frame(start, self.sample_rate),
                self.map.beat_at_frame(end, self.sample_rate),
            )),
            _ => None,
        }
    }

    /// Takes the commands a [`ClockControl`] sent, describes the next
    /// `frames` frames, and moves the position past them. Real-time safe.
    ///
    /// While stopped, the position does not move and the block has no
    /// [pieces](ClockBlock::pieces).
    pub fn next_block(&mut self, frames: usize) -> ClockBlock {
        self.take_commands();
        let block = self.peek(frames);
        self.jumped = false;
        self.wrapped = false;
        if self.playing {
            let (next, wrapped) = block.end();
            self.position = next;
            self.wrapped = wrapped;
        }
        if let Some(link) = &self.link {
            let shared = &link.shared;
            shared.playing.store(self.playing, Ordering::Relaxed);
            shared.recording.store(self.recording, Ordering::Relaxed);
            shared.frame.store(self.position, Ordering::Relaxed);
            let position = self.map.position_at_frame(self.position, self.sample_rate);
            shared.beat.store(position.beat, Ordering::Relaxed);
            shared.tempo.store(position.tempo_bpm, Ordering::Relaxed);
        }
        block
    }

    fn take_commands(&mut self) {
        let Some(mut link) = self.link.take() else {
            return;
        };
        while let Some(command) = link.commands.pop() {
            match command {
                Command::Play => self.play(),
                Command::Stop => self.stop(),
                Command::Recording(recording) => self.set_recording(recording),
                Command::SeekFrame(frame) => self.seek_frame(frame),
                Command::SeekBeat(beat) => self.seek_beat(beat),
                Command::Loop(spec) => match spec {
                    Some(LoopSpec::Beats(start, end)) => {
                        self.set_loop_beats(Some((start, end)));
                    }
                    Some(LoopSpec::Frames(start, end)) => {
                        self.set_loop_frames(Some((start, end)));
                    }
                    None => self.set_loop_frames(None),
                },
                Command::Map(map) => {
                    let old = self.replace_map(map);
                    if let Err(old) = link.retired.push(old) {
                        // The control sends no more maps than this queue
                        // holds, so this cannot happen. Should it, freeing
                        // here could stall the audio: the map is abandoned.
                        std::mem::forget(old);
                    }
                }
            }
        }
        self.link = Some(link);
    }
}

/// Works a [`TransportClock`] from another thread, and tells where it is.
///
/// Commands reach the clock at the start of its next block, in the order
/// they were given. Each returns `false` if too many are already waiting,
/// which takes a clock that has not run for a while. The control can be
/// shared between threads: its methods take `&self`, and lock a mutex of
/// the control's own, never one the audio thread takes.
pub struct ClockControl {
    inner: Mutex<ControlInner>,
    shared: Arc<Shared>,
}

struct ControlInner {
    commands: Producer<Command>,
    retired: Consumer<Box<TempoMap>>,
    /// Maps sent and not yet collected, the one playing included.
    in_flight: usize,
    limit: usize,
}

impl fmt::Debug for ClockControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClockControl")
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl ClockControl {
    fn send(&self, command: Command) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.collect();
        inner.commands.push(command).is_ok()
    }

    /// Starts playing.
    pub fn play(&self) -> bool {
        self.send(Command::Play)
    }

    /// Stops, keeping the position.
    pub fn stop(&self) -> bool {
        self.send(Command::Stop)
    }

    /// Sets whether the transport says it records.
    pub fn set_recording(&self, recording: bool) -> bool {
        self.send(Command::Recording(recording))
    }

    /// Moves to `frame`.
    pub fn seek_frame(&self, frame: i64) -> bool {
        self.send(Command::SeekFrame(frame))
    }

    /// Moves to the frame `beat` lands on.
    pub fn seek_beat(&self, beat: f64) -> bool {
        self.send(Command::SeekBeat(beat))
    }

    /// Loops between two positions in quarter notes, or stops looping.
    pub fn set_loop_beats(&self, range: Option<(f64, f64)>) -> bool {
        self.send(Command::Loop(
            range.map(|(start, end)| LoopSpec::Beats(start, end)),
        ))
    }

    /// Loops between two frames, or stops looping.
    pub fn set_loop_frames(&self, range: Option<(i64, i64)>) -> bool {
        self.send(Command::Loop(
            range.map(|(start, end)| LoopSpec::Frames(start, end)),
        ))
    }

    /// Sends `map` for the clock to play from its next block, keeping the
    /// musical position (see [`TransportClock::replace_map`]). Allocates,
    /// and frees the maps the clock has sent back.
    ///
    /// Gives the map back if four maps are already on their way and the
    /// clock has not taken them, or if the command queue is full.
    pub fn set_tempo_map(&self, map: TempoMap) -> Result<(), TempoMap> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.collect();
        if inner.in_flight >= inner.limit {
            return Err(map);
        }
        match inner.commands.push(Command::Map(Box::new(map))) {
            Ok(()) => {
                inner.in_flight += 1;
                Ok(())
            }
            Err(Command::Map(map)) => Err(*map),
            Err(_) => unreachable!("the queue gives back what it was given"),
        }
    }

    /// Frees the maps the clock has sent back, and returns how many. Every
    /// other method does this too.
    pub fn collect(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .collect()
    }

    /// Whether the clock was playing after its last block.
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Whether the clock said it recorded after its last block.
    pub fn is_recording(&self) -> bool {
        self.shared.recording.load(Ordering::Relaxed)
    }

    /// The frame the clock's next block starts on, as of its last block.
    pub fn position_frames(&self) -> i64 {
        self.shared.frame.load(Ordering::Relaxed)
    }

    /// That frame's position, in quarter notes.
    pub fn position_beats(&self) -> f64 {
        self.shared.beat.load(Ordering::Relaxed)
    }

    /// The tempo on that frame.
    pub fn tempo_bpm(&self) -> f64 {
        self.shared.tempo.load(Ordering::Relaxed)
    }
}

impl ControlInner {
    fn collect(&mut self) -> usize {
        let mut count = 0;
        while let Some(map) = self.retired.pop() {
            drop(map);
            count += 1;
        }
        self.in_flight = self.in_flight.saturating_sub(count);
        count
    }
}

/// A stretch of a block over which the timeline runs straight on: the
/// block is cut where a loop goes round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Its first frame in the block.
    pub offset: usize,
    /// How many frames it has; never zero.
    pub frames: usize,
    /// The timeline frame of its first frame.
    pub start_frame: i64,
}

/// One block of a [`TransportClock`]: where it starts on the timeline,
/// whether it plays, and where it goes round its loop.
///
/// It holds no reference to the map; [`with_map`](Self::with_map) pairs it
/// with one for musical questions, and [`transport`](Self::transport)
/// gives the [`Transport`] a processor is handed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockBlock {
    /// The timeline frame of the block's first frame.
    pub start_frame: i64,
    /// How long the block is.
    pub frames: usize,
    /// Whether it plays. A stopped block stays on `start_frame`.
    pub playing: bool,
    /// Whether the transport says it records.
    pub recording: bool,
    /// The loop, in frames, if there is one.
    pub loop_frames: Option<(i64, i64)>,
    /// The loop, in quarter notes: as it was given, or worked out from its
    /// frames.
    pub loop_beats: Option<(f64, f64)>,
    /// Whether the position moved since the last block other than by
    /// playing: a seek, or a new map or sample rate that moved the frame.
    pub jumped: bool,
    /// Whether the last block ended on the loop's end, so that this one
    /// starts on its start.
    pub wrapped: bool,
    /// The sample rate.
    pub sample_rate: f64,
}

impl ClockBlock {
    /// The loop, if playing could go round it from `frame`.
    fn active_loop(&self, frame: i64) -> Option<(i64, i64)> {
        self.loop_frames.filter(|&(start, end)| {
            frame < end && end.checked_sub(start).is_some_and(|length| length > 0)
        })
    }

    /// The timeline frame the block's frame `offset` plays. While stopped,
    /// every frame is `start_frame`.
    pub fn frame_at(&self, offset: usize) -> i64 {
        if !self.playing {
            return self.start_frame;
        }
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let ahead = self.start_frame.saturating_add(offset);
        match self.active_loop(self.start_frame) {
            Some((start, end)) if ahead >= end => start + ahead.saturating_sub(end) % (end - start),
            _ => ahead,
        }
    }

    /// Where the next block starts, and whether it starts on the loop's
    /// start because this one ended on the loop's end.
    fn end(&self) -> (i64, bool) {
        let frames = i64::try_from(self.frames).unwrap_or(i64::MAX);
        let ahead = self.start_frame.saturating_add(frames);
        match self.active_loop(self.start_frame) {
            Some((start, end)) if ahead >= end => {
                let over = ahead.saturating_sub(end) % (end - start);
                (start + over, over == 0)
            }
            _ => (ahead, false),
        }
    }

    /// The stretches of the block over which the timeline runs straight,
    /// in order. While stopped there are none.
    pub fn pieces(&self) -> Pieces {
        Pieces {
            block: *self,
            offset: if self.playing { 0 } else { self.frames },
            frame: self.start_frame,
        }
    }

    /// The offsets in the block where the loop goes round: the first frame
    /// of every piece after the first. [`wrapped`](Self::wrapped) tells
    /// whether it went round just before the block.
    pub fn wraps(&self) -> impl Iterator<Item = usize> + use<> {
        self.pieces().skip(1).map(|piece| piece.offset)
    }

    /// Every offset in the block that plays the timeline frame `frame`, in
    /// order: none, one, or several when a short loop goes round more than
    /// once in the block.
    pub fn offsets_of_frame(&self, frame: i64) -> impl Iterator<Item = usize> + use<> {
        self.pieces().filter_map(move |piece| {
            let end = piece
                .start_frame
                .saturating_add(i64::try_from(piece.frames).unwrap_or(i64::MAX));
            (piece.start_frame..end)
                .contains(&frame)
                .then(|| piece.offset + (frame - piece.start_frame) as usize)
        })
    }

    /// The block with the map it was played on, to ask musical questions.
    pub fn with_map<'a>(&self, map: &'a TempoMap) -> BlockTimeline<'a> {
        BlockTimeline { map, block: *self }
    }

    /// The transport at the block's first frame, from `map`: tempo, time
    /// signature, position in frames, seconds and quarter notes, bar,
    /// tempo increment, loop, and whether it plays and records.
    pub fn transport(&self, map: &TempoMap) -> Transport {
        Transport {
            playing: self.playing,
            recording: self.recording,
            loop_beats: self.loop_beats,
            ..map.transport_at_frame(self.start_frame, self.sample_rate)
        }
    }
}

/// The [pieces](ClockBlock::pieces) of a block.
#[derive(Debug, Clone)]
pub struct Pieces {
    block: ClockBlock,
    offset: usize,
    frame: i64,
}

impl Iterator for Pieces {
    type Item = Piece;

    fn next(&mut self) -> Option<Piece> {
        let remaining = self.block.frames.checked_sub(self.offset)?;
        if remaining == 0 {
            return None;
        }
        let looping = self.block.active_loop(self.frame);
        let frames = match looping {
            Some((_, end)) => {
                let to_end = usize::try_from(end.saturating_sub(self.frame)).unwrap_or(usize::MAX);
                remaining.min(to_end)
            }
            None => remaining,
        };
        let piece = Piece {
            offset: self.offset,
            frames,
            start_frame: self.frame,
        };
        self.offset += frames;
        self.frame = self
            .frame
            .saturating_add(i64::try_from(frames).unwrap_or(i64::MAX));
        if let Some((start, end)) = looping {
            if self.frame == end {
                self.frame = start;
            }
        }
        Some(piece)
    }
}

/// A [`ClockBlock`] with its [`TempoMap`]: where each frame of the block is
/// in the music, and where tempo and time-signature changes fall inside
/// it, so that events can go on exact frames.
///
/// A processor finds one in
/// [`ProcessContext::timeline`](crate::ProcessContext::timeline) when its
/// transport comes from a tempo map, as in the standalone runner and the
/// `Harness`. Like the transport, it describes the whole block the host
/// delivered: a processor handed a piece of it reads offset
/// [`ProcessContext::offset`](crate::ProcessContext::offset)` + n` for its
/// frame `n`.
#[derive(Debug, Clone, Copy)]
pub struct BlockTimeline<'a> {
    /// The map.
    pub map: &'a TempoMap,
    /// The block.
    pub block: ClockBlock,
}

impl<'a> BlockTimeline<'a> {
    /// The transport at the block's first frame.
    pub fn transport(&self) -> Transport {
        self.block.transport(self.map)
    }

    /// Everything the map says about the block's frame `offset`.
    pub fn position_at(&self, offset: usize) -> MusicalPosition {
        self.map
            .position_at_frame(self.block.frame_at(offset), self.block.sample_rate)
    }

    /// The position of the block's frame `offset`, in quarter notes.
    pub fn beat_at(&self, offset: usize) -> f64 {
        self.map
            .beat_at_frame(self.block.frame_at(offset), self.block.sample_rate)
    }

    /// The tempo on the block's frame `offset`.
    pub fn tempo_at(&self, offset: usize) -> f64 {
        self.map
            .tempo_at_frame(self.block.frame_at(offset), self.block.sample_rate)
    }

    /// Every offset in the block on which something at `beat` lands (see
    /// [`TempoMap::frame_at_beat`]), in order.
    pub fn offsets_of_beat(&self, beat: f64) -> impl Iterator<Item = usize> + use<> {
        let frame = self.map.frame_at_beat(beat, self.block.sample_rate);
        self.block.offsets_of_frame(frame)
    }

    /// The first offset in the block on which something at `beat` lands.
    pub fn offset_of_beat(&self, beat: f64) -> Option<usize> {
        self.offsets_of_beat(beat).next()
    }

    /// The tempo points and time signatures that land on a frame the block
    /// plays, in the block's order. On one frame, a time signature comes
    /// before a tempo point. While stopped there are none.
    pub fn changes(&self) -> Changes<'a> {
        Changes {
            map: self.map,
            sample_rate: self.block.sample_rate,
            pieces: self.block.pieces(),
            piece: None,
            tempo: 0,
            signature: 0,
        }
    }
}

/// What changes on a frame of a block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChangeKind {
    /// A tempo point lands here.
    Tempo(TempoPoint),
    /// A bar with a new time signature starts here.
    Signature(SignaturePoint),
}

/// A change of tempo or time signature inside a block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockChange {
    /// The frame of the block it lands on.
    pub offset: usize,
    /// The timeline frame it lands on.
    pub frame: i64,
    /// What changes.
    pub kind: ChangeKind,
}

/// The [changes](BlockTimeline::changes) inside a block.
#[derive(Debug, Clone)]
pub struct Changes<'a> {
    map: &'a TempoMap,
    sample_rate: f64,
    pieces: Pieces,
    piece: Option<Piece>,
    tempo: usize,
    signature: usize,
}

impl Iterator for Changes<'_> {
    type Item = BlockChange;

    fn next(&mut self) -> Option<BlockChange> {
        loop {
            let piece = match self.piece {
                Some(piece) => piece,
                None => {
                    let piece = self.pieces.next()?;
                    self.tempo = self
                        .map
                        .first_tempo_from(piece.start_frame, self.sample_rate);
                    self.signature = self
                        .map
                        .first_signature_from(piece.start_frame, self.sample_rate);
                    self.piece = Some(piece);
                    piece
                }
            };
            let end = piece
                .start_frame
                .saturating_add(i64::try_from(piece.frames).unwrap_or(i64::MAX));
            let tempo = self.map.tempo_point(self.tempo).and_then(|point| {
                let frame = self.map.frame_at_beat(point.beat, self.sample_rate);
                (frame < end).then_some((frame, point))
            });
            let signature = self.map.signature_point(self.signature).and_then(|point| {
                let frame = self.map.frame_at_beat(point.beat, self.sample_rate);
                (frame < end).then_some((frame, point))
            });
            let (frame, kind) = match (tempo, signature) {
                (Some((tempo_frame, tempo)), Some((signature_frame, _)))
                    if tempo_frame < signature_frame =>
                {
                    self.tempo += 1;
                    (tempo_frame, ChangeKind::Tempo(tempo))
                }
                (_, Some((frame, signature))) => {
                    self.signature += 1;
                    (frame, ChangeKind::Signature(signature))
                }
                (Some((frame, tempo)), None) => {
                    self.tempo += 1;
                    (frame, ChangeKind::Tempo(tempo))
                }
                (None, None) => {
                    self.piece = None;
                    continue;
                }
            };
            let into = usize::try_from(frame - piece.start_frame).unwrap_or(usize::MAX);
            return Some(BlockChange {
                offset: piece.offset + into,
                frame,
                kind,
            });
        }
    }
}
