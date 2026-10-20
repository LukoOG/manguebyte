use std::fmt;

use manguebyte_midi::file::{Division, MetaEvent, MidiFile, TrackEventKind};

use crate::Transport;

/// How close, in frames, a musical position may fall after a frame and
/// still count as being on it. Converting a beat to frames goes through
/// seconds and the sample rate, and the rounding of those steps must not
/// push a change that belongs on frame 48 000 onto frame 48 001.
pub(crate) const FRAME_TOLERANCE: f64 = 1e-4;

/// How close, in ticks, a position may fall before a tick and still count
/// as being on it, for the same reason.
const TICK_TOLERANCE: f64 = 1e-6;

/// The frame a musical position at fractional frame `frame` lands on: the
/// first whole frame at or after it, give or take [`FRAME_TOLERANCE`].
pub(crate) fn landing_frame(frame: f64) -> i64 {
    // `as` saturates, and turns a NaN into 0.
    (frame - FRAME_TOLERANCE).ceil() as i64
}

/// A time signature: `numerator` beats to the bar, each beat a
/// 1/`denominator` note, so `(6, 8)` is six eighth notes.
///
/// Lengths are in quarter notes, the unit of every musical position in
/// the framework ([`Transport::position_beats`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeSignature {
    /// Beats in a bar. At least 1.
    pub numerator: u16,
    /// The note value of a beat: 4 is a quarter note, 8 an eighth. At
    /// least 1; it need not be a power of two.
    pub denominator: u16,
}

impl TimeSignature {
    /// Four quarter notes to the bar.
    pub const FOUR_FOUR: Self = Self::new(4, 4);

    /// A time signature of `numerator` beats of 1/`denominator` notes.
    pub const fn new(numerator: u16, denominator: u16) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Whether both figures are at least 1, as a [`TempoMap`] requires.
    pub const fn is_valid(&self) -> bool {
        self.numerator >= 1 && self.denominator >= 1
    }

    /// The length of a beat in quarter notes: 4 over the denominator.
    pub fn beat_length(&self) -> f64 {
        4.0 / f64::from(self.denominator.max(1))
    }

    /// The length of a bar in quarter notes.
    pub fn bar_length(&self) -> f64 {
        f64::from(self.numerator) * self.beat_length()
    }
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self::FOUR_FOUR
    }
}

impl From<(u16, u16)> for TimeSignature {
    fn from((numerator, denominator): (u16, u16)) -> Self {
        Self::new(numerator, denominator)
    }
}

impl From<TimeSignature> for (u16, u16) {
    fn from(signature: TimeSignature) -> Self {
        (signature.numerator, signature.denominator)
    }
}

impl fmt::Display for TimeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

/// How the tempo goes from one tempo point to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TempoCurve {
    /// The tempo holds until the next point, and jumps there.
    #[default]
    Step,
    /// The tempo changes linearly *in time* until it reaches the next
    /// point's tempo, at the next point: every frame adds the same number
    /// of beats per minute, as CLAP's `tempo_inc` describes a ramp. On the
    /// last point of a map, which has no next point, it holds like
    /// [`Step`](Self::Step).
    Linear,
}

/// A tempo change on the musical timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoPoint {
    /// Where it happens, in quarter notes from the start of the timeline.
    pub beat: f64,
    /// The tempo there, in quarter notes per minute.
    pub bpm: f64,
    /// How the tempo goes from here to the next point.
    pub curve: TempoCurve,
}

/// A change of time signature, at the start of a bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignaturePoint {
    /// The bar it starts, counted from 0.
    pub bar: i64,
    /// Where that bar starts, in quarter notes. Worked out by the map from
    /// the signatures before it.
    pub beat: f64,
    /// The time signature from that bar on.
    pub signature: TimeSignature,
}

/// The bar a position falls in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarPosition {
    /// The bar, counted from 0. Negative before the start of the timeline,
    /// as in a count-in.
    pub bar: i64,
    /// Where the bar starts, in quarter notes.
    pub start_beat: f64,
    /// The time signature of the bar.
    pub signature: TimeSignature,
}

/// A position as bars, beats and ticks, all counted from 0.
///
/// `beat` counts beats of the bar's time signature (eighth notes in 6/8),
/// and `tick` counts fractions of that beat at a resolution the caller
/// chooses: [`TempoMap::bbt_at_beat`] takes it as `ticks_per_beat`. In a
/// signature whose beat is a quarter note this is the familiar PPQ
/// resolution. [`Display`](fmt::Display) counts bars and beats from 1, as
/// a DAW shows them: `1.1.0` is the start of the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BarBeatTick {
    /// The bar, from 0.
    pub bar: i64,
    /// The beat within the bar, from 0.
    pub beat: u32,
    /// The tick within the beat, from 0.
    pub tick: u32,
}

impl fmt::Display for BarBeatTick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bar = i128::from(self.bar) + 1;
        let beat = u64::from(self.beat) + 1;
        write!(f, "{bar}.{beat}.{}", self.tick)
    }
}

/// Everything the map says about one frame of the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MusicalPosition {
    /// The frame, counted from the start of the timeline.
    pub frame: i64,
    /// Its time, in seconds from the start of the timeline.
    pub seconds: f64,
    /// Its position, in quarter notes.
    pub beat: f64,
    /// The tempo on the frame, in quarter notes per minute.
    pub tempo_bpm: f64,
    /// How much the tempo changes from one frame to the next, in beats
    /// per minute per frame: zero except on a [linear
    /// ramp](TempoCurve::Linear).
    pub tempo_increment: f64,
    /// The time signature of its bar.
    pub signature: TimeSignature,
    /// Its bar, from 0.
    pub bar: i64,
    /// Where its bar starts, in quarter notes.
    pub bar_start_beat: f64,
}

/// Why a change to a [`TempoMap`] was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TempoMapError {
    /// A tempo that is not a positive, finite number of beats per minute.
    Tempo,
    /// A position that is negative or not finite.
    Position,
    /// A time signature with a zero in it.
    Signature,
}

impl fmt::Display for TempoMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tempo => "a tempo must be a positive, finite number of beats per minute",
            Self::Position => "a position must be finite and not negative",
            Self::Signature => {
                "a time signature needs at least one beat of a note value of 1 or more"
            }
        })
    }
}

impl std::error::Error for TempoMapError {}

/// One stretch of the map between tempo points, with what the map works
/// out about it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    /// Where it starts, in quarter notes.
    beat: f64,
    /// When it starts, in seconds.
    seconds: f64,
    /// The tempo at its start.
    bpm: f64,
    /// The tempo at its end: the next point's for a linear ramp, its own
    /// otherwise.
    end_bpm: f64,
    curve: TempoCurve,
    /// Its length in quarter notes; infinite for the last.
    beats: f64,
    /// Its length in seconds; infinite for the last.
    duration: f64,
}

impl Segment {
    fn new(beat: f64, bpm: f64, curve: TempoCurve) -> Self {
        Self {
            beat,
            seconds: 0.0,
            bpm,
            end_bpm: bpm,
            curve,
            beats: f64::INFINITY,
            duration: f64::INFINITY,
        }
    }

    fn point(&self) -> TempoPoint {
        TempoPoint {
            beat: self.beat,
            bpm: self.bpm,
            curve: self.curve,
        }
    }

    fn is_ramp(&self) -> bool {
        self.end_bpm != self.bpm
    }

    /// The tempo `beats` quarter notes into the segment.
    ///
    /// On a ramp linear in time, the square of the tempo is linear in
    /// beats: from `T(t) = T0 + a t` and `b(t) = (T0 t + a t² / 2) / 60`,
    /// `T(t)² = T0² + 120 a b(t)`, and over the whole segment
    /// `120 a = (T1² − T0²) / L`.
    fn tempo_after_beats(&self, beats: f64) -> f64 {
        if !self.is_ramp() {
            return self.bpm;
        }
        let fraction = (beats / self.beats).clamp(0.0, 1.0);
        let (start, end) = (self.bpm * self.bpm, self.end_bpm * self.end_bpm);
        (start + (end - start) * fraction).sqrt()
    }

    /// The tempo `seconds` into the segment: linear in time.
    fn tempo_after_seconds(&self, seconds: f64) -> f64 {
        if !self.is_ramp() {
            return self.bpm;
        }
        let fraction = (seconds / self.duration).clamp(0.0, 1.0);
        self.bpm + (self.end_bpm - self.bpm) * fraction
    }

    /// The seconds it takes to cover `beats` quarter notes from the start.
    ///
    /// The tempo is linear in time, so its mean over any stretch is the
    /// mean of its two ends, and the time is the beats over that mean:
    /// `t = 120 b / (T0 + T(b))`. Nothing is subtracted, so this stays
    /// exact to the last bits as the ramp flattens out, where the formula
    /// of a ramp linear in beats would divide by nearly zero.
    fn seconds_after_beats(&self, beats: f64) -> f64 {
        if !self.is_ramp() {
            return beats * 60.0 / self.bpm;
        }
        let tempo = self.tempo_after_beats(beats);
        120.0 * beats / (self.bpm + tempo)
    }

    /// The quarter notes covered `seconds` into the segment: the inverse of
    /// [`seconds_after_beats`](Self::seconds_after_beats),
    /// `b = t (T0 + T(t)) / 120`.
    fn beats_after_seconds(&self, seconds: f64) -> f64 {
        if !self.is_ramp() {
            return seconds * self.bpm / 60.0;
        }
        let tempo = self.tempo_after_seconds(seconds);
        seconds * (self.bpm + tempo) / 120.0
    }

    /// Beats per minute gained per second on a ramp.
    fn slope(&self) -> f64 {
        if self.is_ramp() {
            (self.end_bpm - self.bpm) / self.duration
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Meter {
    bar: i64,
    beat: f64,
    signature: TimeSignature,
}

/// Tempo and time-signature changes on a musical timeline, and exact
/// conversions between frames, seconds, quarter notes and bars, beats and
/// ticks across them.
///
/// # The timeline
///
/// Beat 0 is second 0 and frame 0. Musical positions are in quarter notes
/// (the "PPQ position" of the plugin formats; multiply by a resolution
/// with [`beats_to_ticks`](super::beats_to_ticks) for whole ticks).
///
/// - **Tempo points** sit on quarter notes. Each holds its tempo until the
///   next point ([`TempoCurve::Step`]) or ramps to the next point's tempo
///   ([`TempoCurve::Linear`]). A map always has a point at beat 0, and
///   before it, during a count-in, the first tempo holds.
/// - **Time signatures** start bars. A map always has one at bar 0; bar 0
///   starts at beat 0, and the first signature holds before it, so bars
///   before the start are negative.
///
/// # Ramps
///
/// A linear ramp is linear in *time*: the tempo gains the same number of
/// beats per minute on every frame, which is how CLAP's `tempo_inc`
/// describes a ramp to a plugin, and so how a host's ramp reaches one.
/// Every conversion has a closed form. With `T0` and `T1` the tempos at the
/// ends of a ramp `L` quarter notes long, its duration is
/// `D = 120 L / (T0 + T1)` seconds, and `x` beats into it:
///
/// - the tempo is `T(x) = √(T0² + (T1² − T0²) x / L)`;
/// - the time is `t = 120 x / (T0 + T(x))`;
/// - and back, `t` seconds in, `T(t) = T0 + (T1 − T0) t / D` and
///   `x = t (T0 + T(t)) / 120`.
///
/// None of these subtracts nearly equal numbers, so they stay exact as a
/// ramp flattens, and they reduce to `t = 60 x / T` for a constant tempo.
/// A ramp linear in beats instead gives `t = 60 L ln(T1 / T0) / (T1 − T0)`,
/// which needs a logarithm and an exponential and divides by nearly zero
/// for a gentle ramp; that is why the map does not use it.
///
/// # Frames
///
/// The conversions to and from seconds and quarter notes are exact
/// functions of the real line, rounded once. On frames, a musical position
/// *lands* on the first whole frame at or after it
/// ([`frame_at_beat`](Self::frame_at_beat)), give or take 1e-4 of a frame
/// so that rounding does not push it onto the next one. The frame-based
/// lookups ([`beat_at_frame`](Self::beat_at_frame),
/// [`position_at_frame`](Self::position_at_frame)) follow the same rule: a
/// tempo point or a bar line takes effect on the frame it lands on, so the
/// tempo and bar reported for a frame always agree with where the changes
/// land.
///
/// # Threads
///
/// Building or changing a map allocates; reading it never does. To change
/// the map a [`TransportClock`](super::TransportClock) plays, build a new
/// one and send it with
/// [`ClockControl::set_tempo_map`](super::ClockControl::set_tempo_map).
///
/// ```
/// use manguebyte_dsp::timeline::{TempoCurve, TempoMap, TimeSignature};
///
/// // 120 bpm in 4/4, ramping up to 180 over bars 2 and 3, then 6/8.
/// let mut map = TempoMap::new(120.0, TimeSignature::FOUR_FOUR)?;
/// map.set_tempo(4.0, 120.0, TempoCurve::Linear)?;
/// map.set_tempo(12.0, 180.0, TempoCurve::Step)?;
/// map.set_signature(3, TimeSignature::new(6, 8))?;
///
/// // A bar of 4/4 at 120 lasts two seconds; the ramp averages 150 bpm.
/// assert_eq!(map.seconds_at_beat(4.0), 2.0);
/// assert!((map.seconds_at_beat(12.0) - (2.0 + 8.0 * 60.0 / 150.0)).abs() < 1e-12);
///
/// // Bar 3 starts on beat 12, and its beats are eighth notes.
/// let bbt = map.bbt_at_beat(13.5, 960);
/// assert_eq!((bbt.bar, bbt.beat, bbt.tick), (3, 3, 0));
/// assert_eq!(bbt.to_string(), "4.4.0");
///
/// // At 48 kHz, frame 96 000 is the start of the ramp.
/// assert_eq!(map.frame_at_beat(4.0, 48_000.0), 96_000);
/// assert_eq!(map.beat_at_frame(96_000, 48_000.0), 4.0);
/// # Ok::<(), manguebyte_dsp::timeline::TempoMapError>(())
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    /// Sorted by beat, never empty, the first at beat 0.
    tempos: Vec<Segment>,
    /// Sorted by bar, never empty, the first at bar 0.
    meters: Vec<Meter>,
}

impl Default for TempoMap {
    /// 120 quarter notes per minute in 4/4, the defaults of a Standard MIDI
    /// File.
    fn default() -> Self {
        Self::from_valid(120.0, TimeSignature::FOUR_FOUR)
    }
}

impl TempoMap {
    /// A map with one tempo and one time signature throughout.
    pub fn new(bpm: f64, signature: TimeSignature) -> Result<Self, TempoMapError> {
        check_tempo(bpm)?;
        check_signature(signature)?;
        Ok(Self::from_valid(bpm, signature))
    }

    fn from_valid(bpm: f64, signature: TimeSignature) -> Self {
        Self {
            tempos: vec![Segment::new(0.0, bpm, TempoCurve::Step)],
            meters: vec![Meter {
                bar: 0,
                beat: 0.0,
                signature,
            }],
        }
    }

    /// Reads the tempo and time-signature changes of a Standard MIDI File,
    /// from every track. `None` for a file timed in SMPTE frames, which
    /// has no musical timeline.
    ///
    /// A tick becomes `tick / ticks_per_quarter` quarter notes. Before the
    /// first tempo event the tempo is 120 bpm, and before the first time
    /// signature 4/4, as the specification says. A time signature that
    /// does not fall on a bar line takes effect from the next one, and of
    /// several events at one tick the last read wins. Tempo events of 0
    /// microseconds, and signatures with a zero or a denominator beyond
    /// 2^15, are skipped.
    ///
    /// The map gives the same times as
    /// [`MidiFile::tick_to_seconds`](manguebyte_midi::file::MidiFile::tick_to_seconds).
    pub fn from_midi_file(file: &MidiFile) -> Option<Self> {
        let Division::TicksPerQuarter(ticks) = file.division else {
            return None;
        };
        let per_quarter = f64::from(ticks.max(1));
        let mut tempos = Vec::new();
        let mut signatures = Vec::new();
        for track in &file.tracks {
            for event in track.events() {
                match &event.kind {
                    TrackEventKind::Meta(MetaEvent::Tempo(micros)) if *micros > 0 => {
                        tempos.push((event.tick, 60_000_000.0 / f64::from(*micros)));
                    }
                    TrackEventKind::Meta(MetaEvent::TimeSignature {
                        numerator,
                        denominator_power,
                        ..
                    }) if *numerator > 0 && *denominator_power < 16 => {
                        let signature =
                            TimeSignature::new(u16::from(*numerator), 1 << denominator_power);
                        signatures.push((event.tick, signature));
                    }
                    _ => {}
                }
            }
        }
        // Stable, so that of two events at one tick the later one wins.
        tempos.sort_by_key(|&(tick, _)| tick);
        signatures.sort_by_key(|&(tick, _)| tick);

        let mut map = Self::default();
        for (tick, bpm) in tempos {
            // Valid by construction: positive ticks and tempos.
            let _ = map.set_tempo(tick as f64 / per_quarter, bpm, TempoCurve::Step);
        }
        for (tick, signature) in signatures {
            let beat = tick as f64 / per_quarter;
            let bar = map.bar_at_beat(beat);
            let bar = if bar.start_beat == beat {
                bar.bar
            } else {
                bar.bar + 1
            };
            let _ = map.set_signature(bar, signature);
        }
        Some(map)
    }

    // --- Editing ---------------------------------------------------------

    /// Puts a tempo point at `beat`, replacing any point already there.
    /// The point at beat 0 can be replaced but not removed. Allocates.
    pub fn set_tempo(
        &mut self,
        beat: f64,
        bpm: f64,
        curve: TempoCurve,
    ) -> Result<(), TempoMapError> {
        check_position(beat)?;
        check_tempo(bpm)?;
        let segment = Segment::new(beat, bpm, curve);
        let index = self.tempos.partition_point(|point| point.beat < beat);
        match self.tempos.get_mut(index) {
            Some(point) if point.beat == beat => *point = segment,
            _ => self.tempos.insert(index, segment),
        }
        self.rebuild_tempos();
        Ok(())
    }

    /// Removes the tempo point at exactly `beat`. Returns whether there was
    /// one; the point at beat 0 stays.
    pub fn remove_tempo(&mut self, beat: f64) -> bool {
        let index = self.tempos.partition_point(|point| point.beat < beat);
        if index == 0
            || self
                .tempos
                .get(index)
                .is_none_or(|point| point.beat != beat)
        {
            return false;
        }
        self.tempos.remove(index);
        self.rebuild_tempos();
        true
    }

    /// Puts a time signature at the start of `bar`, replacing any already
    /// there. Bars after it move to follow the new bar length. Allocates.
    pub fn set_signature(
        &mut self,
        bar: i64,
        signature: TimeSignature,
    ) -> Result<(), TempoMapError> {
        if bar < 0 {
            return Err(TempoMapError::Position);
        }
        check_signature(signature)?;
        let meter = Meter {
            bar,
            beat: 0.0,
            signature,
        };
        let index = self.meters.partition_point(|point| point.bar < bar);
        match self.meters.get_mut(index) {
            Some(point) if point.bar == bar => *point = meter,
            _ => self.meters.insert(index, meter),
        }
        self.rebuild_meters();
        Ok(())
    }

    /// Removes the time signature at `bar`. Returns whether there was one;
    /// the one at bar 0 stays.
    pub fn remove_signature(&mut self, bar: i64) -> bool {
        let index = self.meters.partition_point(|point| point.bar < bar);
        if index == 0 || self.meters.get(index).is_none_or(|point| point.bar != bar) {
            return false;
        }
        self.meters.remove(index);
        self.rebuild_meters();
        true
    }

    fn rebuild_tempos(&mut self) {
        let mut seconds = 0.0;
        for index in 0..self.tempos.len() {
            let next = self.tempos.get(index + 1).map(|next| (next.beat, next.bpm));
            let segment = &mut self.tempos[index];
            segment.seconds = seconds;
            match next {
                Some((beat, bpm)) => {
                    segment.beats = beat - segment.beat;
                    segment.end_bpm = match segment.curve {
                        TempoCurve::Linear => bpm,
                        TempoCurve::Step => segment.bpm,
                    };
                    segment.duration = 120.0 * segment.beats / (segment.bpm + segment.end_bpm);
                    seconds += segment.duration;
                }
                None => {
                    segment.beats = f64::INFINITY;
                    segment.end_bpm = segment.bpm;
                    segment.duration = f64::INFINITY;
                }
            }
        }
    }

    fn rebuild_meters(&mut self) {
        let mut beat = 0.0;
        for index in 0..self.meters.len() {
            if index > 0 {
                let previous = self.meters[index - 1];
                let bars = (self.meters[index].bar - previous.bar) as f64;
                beat += bars * previous.signature.bar_length();
            }
            self.meters[index].beat = beat;
        }
    }

    // --- Reading the points ----------------------------------------------

    /// The tempo points, in order. The first is at beat 0.
    pub fn tempo_points(&self) -> impl ExactSizeIterator<Item = TempoPoint> + '_ {
        self.tempos.iter().map(Segment::point)
    }

    /// The time-signature changes, in order. The first is at bar 0.
    pub fn signature_points(&self) -> impl ExactSizeIterator<Item = SignaturePoint> + '_ {
        self.meters.iter().map(|meter| SignaturePoint {
            bar: meter.bar,
            beat: meter.beat,
            signature: meter.signature,
        })
    }

    pub(crate) fn tempo_point(&self, index: usize) -> Option<TempoPoint> {
        self.tempos.get(index).map(Segment::point)
    }

    pub(crate) fn signature_point(&self, index: usize) -> Option<SignaturePoint> {
        self.meters.get(index).map(|meter| SignaturePoint {
            bar: meter.bar,
            beat: meter.beat,
            signature: meter.signature,
        })
    }

    /// The index of the first tempo point that lands on `frame` or later.
    pub(crate) fn first_tempo_from(&self, frame: i64, sample_rate: f64) -> usize {
        self.tempos
            .partition_point(|point| landing_frame(point.seconds * sample_rate) < frame)
    }

    /// The index of the first signature that lands on `frame` or later.
    pub(crate) fn first_signature_from(&self, frame: i64, sample_rate: f64) -> usize {
        self.meters
            .partition_point(|meter| self.frame_at_beat(meter.beat, sample_rate) < frame)
    }

    /// Whether the tempo never changes.
    pub fn is_constant(&self) -> bool {
        self.tempos.len() == 1
    }

    fn segment_at_beat(&self, beat: f64) -> &Segment {
        let index = self.tempos.partition_point(|point| point.beat <= beat);
        &self.tempos[index.saturating_sub(1)]
    }

    fn segment_at_seconds(&self, seconds: f64) -> &Segment {
        let index = self
            .tempos
            .partition_point(|point| point.seconds <= seconds);
        &self.tempos[index.saturating_sub(1)]
    }

    /// The segment in force on `frame`: the last whose start lands on it
    /// or before.
    fn segment_at_frame(&self, frame: i64, sample_rate: f64) -> &Segment {
        let index = self
            .tempos
            .partition_point(|point| landing_frame(point.seconds * sample_rate) <= frame);
        &self.tempos[index.saturating_sub(1)]
    }

    fn meter_at_beat(&self, beat: f64) -> (usize, &Meter) {
        let index = self
            .meters
            .partition_point(|meter| meter.beat <= beat)
            .saturating_sub(1);
        (index, &self.meters[index])
    }

    fn meter_at_bar(&self, bar: i64) -> &Meter {
        let index = self.meters.partition_point(|meter| meter.bar <= bar);
        &self.meters[index.saturating_sub(1)]
    }

    // --- Tempo -----------------------------------------------------------

    /// The tempo at `beat`, in quarter notes per minute.
    pub fn tempo_at_beat(&self, beat: f64) -> f64 {
        let segment = self.segment_at_beat(beat);
        segment.tempo_after_beats(beat - segment.beat)
    }

    /// The tempo `seconds` into the timeline.
    pub fn tempo_at_seconds(&self, seconds: f64) -> f64 {
        let segment = self.segment_at_seconds(seconds);
        segment.tempo_after_seconds(seconds - segment.seconds)
    }

    // --- Seconds and quarter notes -----------------------------------------

    /// The time of `beat`, in seconds from the start of the timeline.
    /// Negative before it.
    pub fn seconds_at_beat(&self, beat: f64) -> f64 {
        let segment = self.segment_at_beat(beat);
        segment.seconds + segment.seconds_after_beats(beat - segment.beat)
    }

    /// The position `seconds` into the timeline, in quarter notes. The
    /// inverse of [`seconds_at_beat`](Self::seconds_at_beat).
    pub fn beat_at_seconds(&self, seconds: f64) -> f64 {
        let segment = self.segment_at_seconds(seconds);
        segment.beat + segment.beats_after_seconds(seconds - segment.seconds)
    }

    // --- Samples and frames ------------------------------------------------

    /// The position of `beat` in samples at `sample_rate`, with the
    /// fraction: `seconds_at_beat(beat) * sample_rate`.
    pub fn sample_at_beat(&self, beat: f64, sample_rate: f64) -> f64 {
        self.seconds_at_beat(beat) * sample_rate
    }

    /// The position, in quarter notes, of `sample`, which may have a
    /// fraction. The inverse of [`sample_at_beat`](Self::sample_at_beat).
    pub fn beat_at_sample(&self, sample: f64, sample_rate: f64) -> f64 {
        self.beat_at_seconds(sample / sample_rate)
    }

    /// The frame something at `beat` lands on: the first whole frame at or
    /// after it, give or take 1e-4 of a frame. This is the frame on which
    /// a note, a tempo point or a bar line at `beat` takes effect.
    pub fn frame_at_beat(&self, beat: f64, sample_rate: f64) -> i64 {
        landing_frame(self.sample_at_beat(beat, sample_rate))
    }

    /// The position, in quarter notes, of the whole frame `frame`.
    ///
    /// The tempo segment is the one in force on the frame (see the type's
    /// documentation), so the beat of the frame a tempo point lands on is
    /// never before that point, even where rounding would put it a hair
    /// before.
    pub fn beat_at_frame(&self, frame: i64, sample_rate: f64) -> f64 {
        let segment = self.segment_at_frame(frame, sample_rate);
        segment.beat + segment.beats_after_seconds(self.seconds_into(segment, frame, sample_rate))
    }

    /// Seconds from the start of `segment` to `frame`, held inside it.
    fn seconds_into(&self, segment: &Segment, frame: i64, sample_rate: f64) -> f64 {
        let seconds = frame as f64 / sample_rate - segment.seconds;
        if segment.beat == 0.0 && seconds < 0.0 {
            // Before the timeline, where the first tempo holds.
            seconds
        } else {
            seconds.clamp(0.0, segment.duration)
        }
    }

    /// The tempo on `frame`.
    pub fn tempo_at_frame(&self, frame: i64, sample_rate: f64) -> f64 {
        let segment = self.segment_at_frame(frame, sample_rate);
        let seconds = self.seconds_into(segment, frame, sample_rate).max(0.0);
        segment.tempo_after_seconds(seconds)
    }

    /// Everything the map says about `frame`: time, beat, tempo and its
    /// rate of change, time signature and bar. A bar line takes effect on
    /// the frame it lands on, like a tempo point.
    pub fn position_at_frame(&self, frame: i64, sample_rate: f64) -> MusicalPosition {
        let segment = self.segment_at_frame(frame, sample_rate);
        let into = self.seconds_into(segment, frame, sample_rate);
        let beat = segment.beat + segment.beats_after_seconds(into);
        let tempo_bpm = segment.tempo_after_seconds(into.max(0.0));
        let tempo_increment = if into < segment.duration {
            segment.slope() / sample_rate
        } else {
            0.0
        };

        let mut bar = self.bar_at_beat(beat);
        // The next bar line may land on this frame though the beat falls
        // a hair before it.
        let next = self.beat_at_bar(bar.bar.saturating_add(1));
        if self.frame_at_beat(next, sample_rate) <= frame {
            bar = BarPosition {
                bar: bar.bar.saturating_add(1),
                start_beat: next,
                signature: self.meter_at_bar(bar.bar.saturating_add(1)).signature,
            };
        }
        MusicalPosition {
            frame,
            seconds: frame as f64 / sample_rate,
            beat,
            tempo_bpm,
            tempo_increment,
            signature: bar.signature,
            bar: bar.bar,
            bar_start_beat: bar.start_beat,
        }
    }

    /// The transport of a block that starts on `frame`: tempo, time
    /// signature, position in frames, seconds, beats and bars, and the
    /// tempo's rate of change. Neither playing nor looping; the caller
    /// fills those in.
    pub fn transport_at_frame(&self, frame: i64, sample_rate: f64) -> Transport {
        let position = self.position_at_frame(frame, sample_rate);
        Transport {
            tempo_bpm: Some(position.tempo_bpm),
            time_signature: Some(position.signature.into()),
            position_samples: Some(frame),
            position_beats: Some(position.beat),
            bar_start_beats: Some(position.bar_start_beat),
            position_seconds: Some(position.seconds),
            bar_number: Some(position.bar),
            tempo_increment: Some(position.tempo_increment),
            ..Transport::STOPPED
        }
    }

    // --- Bars, beats and ticks ---------------------------------------------

    /// The time signature in force at `beat`.
    pub fn signature_at_beat(&self, beat: f64) -> TimeSignature {
        self.meter_at_beat(beat).1.signature
    }

    /// The time signature of `bar`.
    pub fn signature_at_bar(&self, bar: i64) -> TimeSignature {
        self.meter_at_bar(bar).signature
    }

    /// The bar `beat` falls in. Exact on the real line: a beat a hair
    /// before a bar line is in the bar before it.
    pub fn bar_at_beat(&self, beat: f64) -> BarPosition {
        let (index, meter) = self.meter_at_beat(beat);
        let length = meter.signature.bar_length();
        let mut bars = ((beat - meter.beat) / length).floor();
        if let Some(next) = self.meters.get(index + 1) {
            // Rounding must not carry the bar into the next signature.
            bars = bars.min((next.bar - meter.bar - 1) as f64);
        }
        BarPosition {
            bar: meter.bar.saturating_add(bars as i64),
            start_beat: meter.beat + bars * length,
            signature: meter.signature,
        }
    }

    /// Where `bar` starts, in quarter notes.
    pub fn beat_at_bar(&self, bar: i64) -> f64 {
        let meter = self.meter_at_bar(bar);
        meter.beat + (bar - meter.bar) as f64 * meter.signature.bar_length()
    }

    /// `beat` as bars, beats and ticks, with `ticks_per_beat` ticks to a
    /// beat of the bar's signature (0 is taken as 1).
    ///
    /// The tick is the whole tick at or before the position, but a
    /// position within a millionth of a tick before a tick counts as on
    /// it, so that a beat read back from frames does not lose a tick to
    /// rounding. [`beat_at_bbt`](Self::beat_at_bbt) is the inverse:
    /// `bbt_at_beat(beat_at_bbt(x))` is `x` for every valid `x`.
    pub fn bbt_at_beat(&self, beat: f64, ticks_per_beat: u32) -> BarBeatTick {
        let resolution = u64::from(ticks_per_beat.max(1));
        let bar = self.bar_at_beat(beat);
        let ticks = (beat - bar.start_beat) / bar.signature.beat_length() * resolution as f64;
        let nearest = ticks.round();
        let ticks = if (ticks - nearest).abs() < TICK_TOLERANCE {
            nearest
        } else {
            ticks.floor()
        };
        // `as` saturates; the bar's own length bounds it below.
        let ticks = ticks.max(0.0) as u64;
        let in_bar = u64::from(bar.signature.numerator) * resolution;
        if ticks >= in_bar {
            return BarBeatTick {
                bar: bar.bar.saturating_add(1),
                beat: 0,
                tick: 0,
            };
        }
        BarBeatTick {
            bar: bar.bar,
            beat: (ticks / resolution) as u32,
            tick: (ticks % resolution) as u32,
        }
    }

    /// Where a position in bars, beats and ticks is, in quarter notes, with
    /// `ticks_per_beat` ticks to a beat (0 is taken as 1). A beat or tick
    /// past the end of its bar or beat carries on into the next.
    pub fn beat_at_bbt(&self, position: BarBeatTick, ticks_per_beat: u32) -> f64 {
        let resolution = f64::from(ticks_per_beat.max(1));
        let signature = self.signature_at_bar(position.bar);
        let beats = f64::from(position.beat) + f64::from(position.tick) / resolution;
        self.beat_at_bar(position.bar) + beats * signature.beat_length()
    }
}

/// `beats` quarter notes in ticks of a resolution of `ppq` ticks per
/// quarter note. A plugin format's "PPQ position" is the position in
/// quarter notes itself, [`Transport::position_beats`].
pub fn beats_to_ticks(beats: f64, ppq: u32) -> f64 {
    beats * f64::from(ppq)
}

/// `ticks` at `ppq` ticks per quarter note, in quarter notes (0 is taken as
/// 1). Exact when `ticks` fits in 53 bits and `ppq` is a power of two, and
/// otherwise within half a unit in the last place.
pub fn ticks_to_beats(ticks: i64, ppq: u32) -> f64 {
    ticks as f64 / f64::from(ppq.max(1))
}

fn check_tempo(bpm: f64) -> Result<(), TempoMapError> {
    if bpm.is_finite() && bpm > 0.0 {
        Ok(())
    } else {
        Err(TempoMapError::Tempo)
    }
}

fn check_position(beat: f64) -> Result<(), TempoMapError> {
    if beat.is_finite() && beat >= 0.0 {
        Ok(())
    } else {
        Err(TempoMapError::Position)
    }
}

fn check_signature(signature: TimeSignature) -> Result<(), TempoMapError> {
    if signature.is_valid() {
        Ok(())
    } else {
        Err(TempoMapError::Signature)
    }
}
