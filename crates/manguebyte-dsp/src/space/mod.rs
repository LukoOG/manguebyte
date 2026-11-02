//! The space around an organ's pipes.
//!
//! A pipe organ does not speak from one point. Each pipe stands at its own
//! spot on a windchest, the divisions stand in different parts of the case,
//! and the room answers with early reflections before its reverberant tail.
//! This module has the three blocks between a pipe's own signal and the
//! tail ([`Convolution`](crate::Convolution)):
//!
//! - [`Windchest`] places every sounding pipe, a mono signal, into a number
//!   of output channels: a pan between loudspeakers from the pipe's angle,
//!   a delay from its distance (fractional), a gain that falls as one over
//!   the distance, and a gentle low-pass for the air's absorption. Thousands
//!   of pipes may be placed; it costs only for the ones that sound.
//! - [`EarlyReflections`] gives the early reflections of a rectangular nave
//!   by the image-source method, for a division's bus.
//! - [`SwellBox`] is an enclosed division's swell shutters: one opening,
//!   from 0 to 1, that sets both a level and a low-pass cutoff, both
//!   smoothed at audio rate.
//!
//! Geometry is in metres, in one frame of reference for everything: `x`
//! across (positive to the right of a listener who looks along `+y`), `y`
//! depth, `z` height. A [`Listener`] says where the sound is heard and
//! through which [`Speakers`], and turns a source's [`Position`] into a
//! [`Placement`]. [`Chest`] lays a rank's pipes out across a windchest.
//!
//! # Putting them together
//!
//! For each division: render every sounding pipe as a mono block, add each
//! one to the division's windchest, and also sum them into one mono block,
//! the division's bus. Pass the bus through the division's
//! [`EarlyReflections`] (with the direct path off, since the windchest
//! gives it) and add the result to the windchest's output. An enclosed
//! division's [`SwellBox`] goes on that sum. The reverberant tail is a
//! [`Convolution`](crate::Convolution) fed with the sum of the divisions.
//! A measured response holds its own direct sound and early reflections,
//! which would then be heard twice: take its first part off with
//! [`keep_tail`], up to where the reflections here end (60 to 100 ms is
//! usual, the room's mixing time), with a fade of 10 to 20 ms. A response
//! that is a tail already, such as a synthetic one, is used as it is.
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_dsp::space::{
//!     EarlyReflections, Listener, Position, ReflectionSettings, Reflections, Room, Speakers,
//!     Wall, Windchest, WindchestSettings,
//! };
//!
//! let rate = 48_000.0;
//! let listener = Listener::new(Position::new(6.0, 4.0, 1.7), Speakers::stereo());
//!
//! // Two pipes on a chest 12 m away, one each side.
//! let mut chest = Windchest::new(&WindchestSettings::new(2, 2, rate, 128)).unwrap();
//! chest.set_placement(0, &listener.place(Position::new(4.0, 16.0, 3.0)));
//! chest.set_placement(1, &listener.place(Position::new(8.0, 16.0, 3.0)));
//!
//! // The nave's reflections, for the division's centre.
//! let room = Room::new(12.0, 30.0, 14.0, Wall::new(0.1));
//! let reflections = Reflections::compute(
//!     &room,
//!     Position::new(6.0, 16.0, 3.0),
//!     &listener,
//!     &ReflectionSettings::default(),
//! )
//! .unwrap();
//! let mut early = EarlyReflections::new(&reflections, rate, 128).unwrap();
//!
//! // On the audio thread, each block: each sounding pipe's block, rendered
//! // by the organ, goes to the windchest and into the bus.
//! let pipes = [[0.1f32; 128], [-0.05f32; 128]];
//! let mut bus = [0.0f32; 128];
//! for (p, pipe) in pipes.iter().enumerate() {
//!     chest.add(p, pipe);
//!     for (b, x) in bus.iter_mut().zip(pipe) {
//!         *b += x;
//!     }
//! }
//! let mut out = AudioBuffer::<f32>::new(2, 128);
//! chest.finish(&mut out);
//! early.process_add(&bus, &mut out);
//! ```

mod layout;
mod reflections;
mod swell;
mod windchest;

use core::f64::consts::{FRAC_PI_2, LN_10, PI};
use core::fmt;

use manguebyte_core::Sample;

pub use layout::{Arrangement, Chest, Height};
pub use reflections::{
    EarlyReflections, Reflection, ReflectionSettings, Reflections, Room, Surface, Wall,
};
pub use swell::{SwellBox, SwellLaw};
pub use windchest::{Windchest, WindchestSettings};

/// The most output channels a [`Windchest`] or [`EarlyReflections`] feeds,
/// and the most [`Speakers`] a [`Listener`] has.
pub const MAX_CHANNELS: usize = 16;

/// The speed of sound in air at 20 °C, in metres per second.
pub const SPEED_OF_SOUND: f64 = 343.0;

/// `10 / ln 10`: a first-order low-pass at cutoff `fc` takes
/// `10·log10(1 + (f/fc)²)` decibels at `f`, which is this times `(f/fc)²`
/// well below the cutoff.
const ONE_POLE_DB: f64 = 10.0 / LN_10;

/// The frequency the air's loss is given at, in hertz.
const AIR_REFERENCE_HZ: f64 = 8_000.0;

/// The highest cutoff a filter of this module runs at, as a fraction of the
/// sample rate. A cutoff at or above it is taken as no filter at all.
const HIGHEST_CUTOFF_RATIO: f64 = 0.49;

/// The lowest cutoff a filter of this module runs at, in hertz.
const LOWEST_CUTOFF_HZ: f64 = 10.0;

/// The loudest sample a windchest or the reflections take in. Louder ones
/// are clipped there, and anything that is not a number counts as silence,
/// so that a broken input never poisons a filter's memory.
const INPUT_LIMIT: f32 = 1_024.0;

/// The largest gain into one channel.
const GAIN_LIMIT: f64 = 256.0;

/// How long a change of placement or of reflections crossfades, in seconds.
const MOVE_SECONDS: f64 = 0.02;

/// What was wrong with a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceError {
    /// A channel count outside `1..=MAX_CHANNELS`.
    Channels(usize),
    /// A loudspeaker azimuth that is not a finite number, two loudspeakers
    /// at the same azimuth, or a stereo width outside 0 to 90 degrees.
    Speakers,
    /// A sample rate that is not a finite number above zero.
    SampleRate,
    /// A block size of zero.
    BlockSize,
    /// A maximum delay that is not a finite number of zero or more.
    Delay,
    /// A room dimension that is not a finite number above zero, or a wall
    /// absorption outside 0 to 1.
    Room,
    /// A source or a listener outside the room.
    OutsideRoom,
    /// A reflection order above [`ReflectionSettings::MAX_ORDER`].
    Order(usize),
    /// A set of reflections with more taps, more channels or a later tap
    /// than an [`EarlyReflections`] was made for.
    DoesNotFit,
}

impl fmt::Display for SpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channels(n) => {
                write!(f, "{n} channels; between 1 and {MAX_CHANNELS} are possible")
            }
            Self::Speakers => f.write_str(
                "loudspeaker azimuths must be finite and distinct, and a stereo width 0 to 90 degrees",
            ),
            Self::SampleRate => f.write_str("the sample rate must be a finite number above zero"),
            Self::BlockSize => f.write_str("the block size must be at least one frame"),
            Self::Delay => f.write_str("the maximum delay must be a finite number of zero or more"),
            Self::Room => f.write_str(
                "room dimensions must be finite and above zero, wall absorptions 0 to 1",
            ),
            Self::OutsideRoom => f.write_str("the source and the listener must be inside the room"),
            Self::Order(n) => write!(
                f,
                "reflection order {n}; at most {} is possible",
                ReflectionSettings::MAX_ORDER
            ),
            Self::DoesNotFit => f.write_str(
                "these reflections have more taps, more channels or a later tap than there is room for",
            ),
        }
    }
}

impl std::error::Error for SpaceError {}

/// A point in space, in metres.
///
/// `x` runs across, positive to the right of a listener who looks along
/// `+y`; `y` is depth, `z` height.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    /// Across, in metres.
    pub x: f64,
    /// Depth, in metres.
    pub y: f64,
    /// Height, in metres.
    pub z: f64,
}

impl Position {
    /// A point at `x`, `y`, `z` metres.
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// The distance to `other`, in metres.
    pub fn distance_to(self, other: Self) -> f64 {
        let (dx, dy, dz) = (other.x - self.x, other.y - self.y, other.z - self.z);
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

/// The loudspeakers a [`Listener`] hears through: one azimuth per output
/// channel, in degrees, 0 straight ahead and positive to the right.
///
/// A source between two neighbouring loudspeakers is panned between that
/// pair only, with the equal-power sine law on its angle: at a fraction `p`
/// of the way from one to the other, they get `cos(p·π/2)` and
/// `sin(p·π/2)`, so the squares of all the channel gains add up to one
/// wherever the source is. This is pairwise amplitude panning, the
/// two-dimensional case of V. Pulkki, "Virtual sound source positioning
/// using vector base amplitude panning", JAES 45(6), 1997, with the sine law
/// on the angle instead of the tangent law.
///
/// The loudspeakers may surround the listener or form an arc. If two
/// neighbours are more than 180° apart, the gap between them is outside
/// the arc: a source there goes wholly to the nearer of the two. A source
/// beyond the stereo pair's angle is therefore hard left or hard right.
/// Height is ignored: only the azimuth in the horizontal plane pans.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speakers {
    /// Each channel's azimuth, in degrees, in `(-180, 180]`.
    azimuths: [f64; MAX_CHANNELS],
    /// Channels sorted by azimuth.
    order: [u8; MAX_CHANNELS],
    count: usize,
}

impl Speakers {
    /// Half the angle between the loudspeakers of [`stereo`](Self::stereo),
    /// in degrees: the standard listening triangle.
    pub const STEREO_HALF_WIDTH: f64 = 30.0;

    /// One channel, which takes everything.
    pub fn mono() -> Self {
        Self::new(&[0.0]).expect("one loudspeaker ahead is valid")
    }

    /// A stereo pair at ±30°: channel 0 on the left, channel 1 on the right.
    pub fn stereo() -> Self {
        Self::stereo_with_width(Self::STEREO_HALF_WIDTH).expect("30 degrees is a valid width")
    }

    /// A stereo pair at `±half_width` degrees, which must be above 0 and at
    /// most 90.
    pub fn stereo_with_width(half_width: f64) -> Result<Self, SpaceError> {
        if !(half_width > 0.0 && half_width <= 90.0) {
            return Err(SpaceError::Speakers);
        }
        Self::new(&[-half_width, half_width])
    }

    /// Loudspeakers at the given azimuths in degrees, one per output
    /// channel, in channel order. Between 1 and [`MAX_CHANNELS`]; each must
    /// be finite, and no two may point the same way.
    pub fn new(azimuths: &[f64]) -> Result<Self, SpaceError> {
        let count = azimuths.len();
        if count == 0 || count > MAX_CHANNELS {
            return Err(SpaceError::Channels(count));
        }
        let mut speakers = Self {
            azimuths: [0.0; MAX_CHANNELS],
            order: [0; MAX_CHANNELS],
            count,
        };
        for (channel, &azimuth) in azimuths.iter().enumerate() {
            if !azimuth.is_finite() {
                return Err(SpaceError::Speakers);
            }
            speakers.azimuths[channel] = wrap_degrees(azimuth);
            speakers.order[channel] = channel as u8;
        }
        let sorted = &mut speakers.order[..count];
        sorted.sort_by(|&a, &b| {
            speakers.azimuths[a as usize].total_cmp(&speakers.azimuths[b as usize])
        });
        if sorted
            .windows(2)
            .any(|pair| speakers.azimuths[pair[0] as usize] == speakers.azimuths[pair[1] as usize])
        {
            return Err(SpaceError::Speakers);
        }
        Ok(speakers)
    }

    /// The number of channels.
    pub fn channels(&self) -> usize {
        self.count
    }

    /// The azimuth of a channel's loudspeaker in degrees, in `(-180, 180]`.
    ///
    /// # Panics
    ///
    /// Panics if `channel` is not below [`channels`](Self::channels).
    pub fn azimuth(&self, channel: usize) -> f64 {
        assert!(channel < self.count, "no channel {channel}");
        self.azimuths[channel]
    }

    /// Each channel's gain for a source at `azimuth` degrees; the entries
    /// past [`channels`](Self::channels) are zero. The squares add up to one.
    pub fn gains(&self, azimuth: f64) -> [f64; MAX_CHANNELS] {
        let mut gains = [0.0; MAX_CHANNELS];
        if self.count == 1 {
            gains[0] = 1.0;
            return gains;
        }
        let azimuth = if azimuth.is_finite() {
            wrap_degrees(azimuth)
        } else {
            0.0
        };
        let order = &self.order[..self.count];
        // The pair whose arc, going up in angle from the first of them,
        // holds the source; the last pair goes round through 180°.
        for (i, &from) in order.iter().enumerate() {
            let to = order[(i + 1) % self.count];
            let start = self.azimuths[from as usize];
            let mut gap = self.azimuths[to as usize] - start;
            if gap <= 0.0 {
                gap += 360.0;
            }
            let mut along = azimuth - start;
            if along < 0.0 {
                along += 360.0;
            }
            if along > gap {
                continue;
            }
            if gap > 180.0 {
                // Outside the arc: the nearer end takes it all.
                let nearer = if along <= gap - along { from } else { to };
                gains[nearer as usize] = 1.0;
            } else {
                let angle = along / gap * FRAC_PI_2;
                gains[from as usize] = angle.cos();
                gains[to as usize] = angle.sin();
            }
            return gains;
        }
        // Unreachable for finite angles; keep a valid answer anyway.
        gains[order[0] as usize] = 1.0;
        gains
    }
}

/// An angle in degrees brought into `(-180, 180]`.
fn wrap_degrees(degrees: f64) -> f64 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Where the sound is heard, through which loudspeakers, and the laws that
/// turn a source's position into a [`Placement`].
///
/// - **Pan:** the source's azimuth, seen from [`position`](Self::position)
///   looking along `+y`, panned by [`Speakers::gains`].
/// - **Delay:** the distance over the speed of sound.
/// - **Distance gain:** `reference_distance / r`, one at the reference
///   distance, with `r` held at `nearest_distance` or more so that a source
///   at the listener is not infinitely loud.
/// - **Air absorption:** a first-order low-pass whose cutoff falls as one
///   over the square root of the distance. The air takes more decibels per
///   metre the higher the frequency, roughly as the square of the frequency
///   in the upper range (ISO 9613-1, "Attenuation of sound during
///   propagation outdoors, part 1: calculation of the absorption of sound
///   by the atmosphere", 1993, gives the coefficients by temperature and
///   humidity). A loss of `a·f²` decibels per metre over `r` metres is
///   matched below the cutoff by a one-pole at
///   `fc = sqrt(10 / ln 10 / (a·r))`; above it the one-pole falls by 6 dB
///   per octave, where the air's loss keeps growing with the square of the
///   frequency. `a` comes from the loss at 8 kHz,
///   [`air_loss_db_per_metre`](Self::air_loss_db_per_metre), whose default,
///   0.077 dB per metre, is about what the standard gives at 20 °C and
///   70 % relative humidity. This is a simple approximation of the
///   physics, not an implementation of the standard: it has no temperature
///   or humidity, and nothing of the loss's lower range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    /// Where the sound is heard.
    pub position: Position,
    /// The loudspeakers, which set the number of output channels.
    pub speakers: Speakers,
    /// The distance at which the distance gain is one, in metres; 1 m
    /// unless set.
    pub reference_distance: f64,
    /// The distance the gain stops rising at, in metres; 0.5 m unless set.
    pub nearest_distance: f64,
    /// In metres per second; [`SPEED_OF_SOUND`] unless set.
    pub speed_of_sound: f64,
    /// The air's loss at 8 kHz, in decibels per metre; 0.077 unless set,
    /// and zero for none.
    pub air_loss_db_per_metre: f64,
}

impl Listener {
    /// The air's loss at 8 kHz that [`new`](Self::new) sets, in decibels per
    /// metre.
    pub const DEFAULT_AIR_LOSS: f64 = 0.077;

    /// A listener at `position`, hearing through `speakers`, with the
    /// default laws.
    pub fn new(position: Position, speakers: Speakers) -> Self {
        Self {
            position,
            speakers,
            reference_distance: 1.0,
            nearest_distance: 0.5,
            speed_of_sound: SPEED_OF_SOUND,
            air_loss_db_per_metre: Self::DEFAULT_AIR_LOSS,
        }
    }

    /// The distance gain at `distance` metres.
    pub fn distance_gain(&self, distance: f64) -> f64 {
        let nearest = self.nearest_distance.max(f64::MIN_POSITIVE);
        let r = if distance >= nearest {
            distance
        } else {
            nearest
        };
        self.reference_distance / r
    }

    /// The cutoff of the air's low-pass over `distance` metres, in hertz;
    /// infinite when there is no loss.
    pub fn air_cutoff_hz(&self, distance: f64) -> f64 {
        cutoff_for_loss(self.air_quadratic(distance))
    }

    /// The air's loss over `distance` metres, as the `q` of a loss of
    /// `q·f²` decibels.
    fn air_quadratic(&self, distance: f64) -> f64 {
        let a = self.air_loss_db_per_metre / (AIR_REFERENCE_HZ * AIR_REFERENCE_HZ);
        if a > 0.0 && distance > 0.0 {
            a * distance
        } else {
            0.0
        }
    }

    /// The delay, in seconds, of a path of `distance` metres.
    fn delay_seconds(&self, distance: f64) -> f64 {
        if self.speed_of_sound > 0.0 {
            distance / self.speed_of_sound
        } else {
            0.0
        }
    }

    /// Where a source at `source` is heard: its pan, delay, distance gain
    /// and air absorption. Real-time safe, though meant for another thread.
    pub fn place(&self, source: Position) -> Placement {
        let distance = self.position.distance_to(source);
        let azimuth = self.azimuth_of(source);
        Placement {
            pan: self.speakers.gains(azimuth),
            channels: self.speakers.channels(),
            distance,
            azimuth_degrees: azimuth,
            delay_seconds: self.delay_seconds(distance),
            distance_gain: self.distance_gain(distance),
            air_cutoff_hz: self.air_cutoff_hz(distance),
        }
    }

    /// The azimuth of `source` in degrees.
    fn azimuth_of(&self, source: Position) -> f64 {
        let (dx, dy) = (source.x - self.position.x, source.y - self.position.y);
        if dx == 0.0 && dy == 0.0 {
            0.0
        } else {
            dx.atan2(dy).to_degrees()
        }
    }
}

/// The cutoff of the one-pole that matches a loss of `q·f²` decibels below
/// it; infinite for no loss.
fn cutoff_for_loss(q: f64) -> f64 {
    if q > 0.0 {
        (ONE_POLE_DB / q).sqrt()
    } else {
        f64::INFINITY
    }
}

/// The `q` of a one-pole at `cutoff`: it takes about `q·f²` decibels below
/// the cutoff. Zero for an infinite cutoff.
fn loss_of_cutoff(cutoff: f64) -> f64 {
    if cutoff.is_finite() && cutoff > 0.0 {
        ONE_POLE_DB / (cutoff * cutoff)
    } else {
        0.0
    }
}

/// Where one source is heard: what [`Listener::place`] works out, for
/// [`Windchest::set_placement`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Each channel's equal-power share, before the distance gain; the
    /// entries past [`channels`](Self::channels) are zero.
    pub pan: [f64; MAX_CHANNELS],
    /// The number of output channels.
    pub channels: usize,
    /// The distance to the listener, in metres.
    pub distance: f64,
    /// The azimuth seen by the listener, in degrees.
    pub azimuth_degrees: f64,
    /// The delay, in seconds.
    pub delay_seconds: f64,
    /// The gain for the distance.
    pub distance_gain: f64,
    /// The cutoff of the air's low-pass in hertz; infinite for none.
    pub air_cutoff_hz: f64,
}

impl Placement {
    /// A source at the listener: no delay, a distance gain of one, no air,
    /// and the same share in every one of `channels` channels (the squares
    /// adding up to one). `channels` is held to `1..=MAX_CHANNELS`.
    pub fn centred(channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_CHANNELS);
        let mut pan = [0.0; MAX_CHANNELS];
        pan[..channels].fill((1.0 / channels as f64).sqrt());
        Self {
            pan,
            channels,
            distance: 0.0,
            azimuth_degrees: 0.0,
            delay_seconds: 0.0,
            distance_gain: 1.0,
            air_cutoff_hz: f64::INFINITY,
        }
    }

    /// The gain into `channel`: its pan share times the distance gain. Zero
    /// past [`channels`](Self::channels).
    pub fn channel_gain(&self, channel: usize) -> f64 {
        if channel < self.channels.min(MAX_CHANNELS) {
            self.pan[channel] * self.distance_gain
        } else {
            0.0
        }
    }
}

/// How a fractional delay is made, for a [`Windchest`] and
/// [`EarlyReflections`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Interpolation {
    /// The delay is rounded to the nearest whole sample: off by up to half
    /// a sample (10 µs at 48 kHz, 3.6 mm of path), and the cheapest.
    None,
    /// Between the two nearest samples. At half a sample past a whole one,
    /// the worst case, it takes 3.0 dB at a quarter of the sample rate
    /// (12 kHz at 48 kHz), 6.0 dB at a third and 11.7 dB at 20 kHz of 48.
    Linear,
    /// Third-order Lagrange interpolation over four samples: at the worst
    /// fraction, 1.1 dB at a quarter of the sample rate, 3.3 dB at a third
    /// and 8.4 dB at 20 kHz of 48. Twice the taps of a straight line, and
    /// the shortest delay is one sample.
    #[default]
    Cubic,
}

/// A delay of `samples` split into whole samples and the four taps of a
/// filter that makes the rest: `z[n] = Σ taps[k]·x[n - k]`, then delayed by
/// the whole part.
fn fractional_delay(samples: f64, interpolation: Interpolation) -> (usize, [f32; 4]) {
    let samples = samples.max(0.0);
    match interpolation {
        Interpolation::None => (samples.round() as usize, [1.0, 0.0, 0.0, 0.0]),
        Interpolation::Linear => {
            let whole = samples.floor();
            let f = samples - whole;
            (whole as usize, [(1.0 - f) as f32, f as f32, 0.0, 0.0])
        }
        Interpolation::Cubic => {
            // The taps make a delay `d` from 1 up to 2, where Lagrange
            // interpolation over four samples is at its best.
            let samples = samples.max(1.0);
            let whole = samples.floor();
            let d = samples - whole + 1.0;
            let taps = [
                -(d - 1.0) * (d - 2.0) * (d - 3.0) / 6.0,
                d * (d - 2.0) * (d - 3.0) / 2.0,
                -d * (d - 1.0) * (d - 3.0) / 2.0,
                d * (d - 1.0) * (d - 2.0) / 6.0,
            ];
            (whole as usize - 1, taps.map(|t| t as f32))
        }
    }
}

/// The shortest delay `interpolation` can make, in samples.
fn shortest_delay(interpolation: Interpolation) -> f64 {
    match interpolation {
        Interpolation::Cubic => 1.0,
        Interpolation::None | Interpolation::Linear => 0.0,
    }
}

/// A first-order low-pass: the topology-preserving one of Zavalishin, "The
/// Art of VA Filter Design", chapter 3, as [`OnePole`](crate::OnePole) has
/// it, with its integrator gain `G = g / (1 + g)`, `g = tan(π·fc/fs)`.
///
/// That filter's step, `v = (x − s)·G`, `y = v + s`, `s' = y + v`, is
/// written out as `y = (1 − G)·s + G·x` and `s' = (1 − 2G)·s + 2G·x`, which
/// is the same filter but leaves one multiplication and one addition
/// between a state and the next instead of four operations: the recursion
/// is what a pipe's block waits on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lowpass {
    keep: f32,
    take: f32,
    carry: f32,
    feed: f32,
}

/// The low-pass at `cutoff`, or `None` for no filter.
fn lowpass(cutoff: f64, sample_rate: f64) -> Option<Lowpass> {
    if cutoff.is_nan() || cutoff >= HIGHEST_CUTOFF_RATIO * sample_rate {
        return None;
    }
    let cutoff = cutoff.max(LOWEST_CUTOFF_HZ);
    let g = (PI * cutoff / sample_rate).tan();
    let gain = g / (1.0 + g);
    Some(Lowpass {
        keep: (1.0 - gain) as f32,
        take: gain as f32,
        carry: (1.0 - 2.0 * gain) as f32,
        feed: (2.0 * gain) as f32,
    })
}

/// One step of the low-pass.
#[inline]
fn lowpass_step(state: &mut f32, x: f32, filter: &Lowpass) -> f32 {
    let y = filter.keep * *state + filter.take * x;
    *state = filter.carry * *state + filter.feed * x;
    y
}

/// A sample held to `±INPUT_LIMIT`, with anything that is not a number made
/// silent.
#[inline]
fn sanitise(x: f32) -> f32 {
    if x.abs() <= INPUT_LIMIT {
        x
    } else if x > 0.0 {
        INPUT_LIMIT
    } else if x < 0.0 {
        -INPUT_LIMIT
    } else {
        0.0
    }
}

/// A gain held to `±GAIN_LIMIT`, with anything that is not a number made
/// zero.
fn limit_gain(gain: f64) -> f64 {
    if gain.is_nan() {
        0.0
    } else {
        gain.clamp(-GAIN_LIMIT, GAIN_LIMIT)
    }
}

/// The smallest power of two of at least `len`.
fn ring_len(len: usize) -> usize {
    len.max(2).next_power_of_two()
}

/// Takes the first part off an impulse response, so that it can follow
/// [`EarlyReflections`] as the reverberant tail: everything before
/// `from_seconds` becomes silent, and a raised-cosine fade of
/// `fade_seconds` brings the response in from there.
///
/// ```
/// use manguebyte_dsp::space::keep_tail;
///
/// let mut response = vec![1.0f32; 48_000];
/// keep_tail(&mut response, 48_000.0, 0.08, 0.02);
/// assert_eq!(response[3_839], 0.0);
/// assert!(response[4_320] > 0.0 && response[4_320] < 1.0);
/// assert_eq!(response[4_800], 1.0);
/// ```
pub fn keep_tail<S: Sample>(
    response: &mut [S],
    sample_rate: f64,
    from_seconds: f64,
    fade_seconds: f64,
) {
    let start = (from_seconds.max(0.0) * sample_rate).round();
    let start = if start.is_finite() { start as usize } else { 0 };
    let fade = (fade_seconds.max(0.0) * sample_rate).round();
    let fade = if fade.is_finite() { fade as usize } else { 0 };
    let start = start.min(response.len());
    response[..start].fill(S::ZERO);
    for (k, sample) in response[start..].iter_mut().take(fade).enumerate() {
        let w = 0.5 - 0.5 * (PI * k as f64 / fade as f64).cos();
        *sample = S::from_f64(sample.to_f64() * w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_wrap_into_a_half_open_turn() {
        assert_eq!(wrap_degrees(180.0), 180.0);
        assert_eq!(wrap_degrees(-180.0), 180.0);
        assert_eq!(wrap_degrees(190.0), -170.0);
        assert_eq!(wrap_degrees(-30.0), -30.0);
        assert_eq!(wrap_degrees(720.0), 0.0);
    }

    #[test]
    fn a_stereo_pair_pans_hard_beyond_its_angle() {
        let stereo = Speakers::stereo();
        assert_eq!(stereo.gains(-30.0)[..2], [1.0, 0.0]);
        assert_eq!(stereo.gains(-80.0)[..2], [1.0, 0.0]);
        assert_eq!(stereo.gains(120.0)[..2], [0.0, 1.0]);
        // Straight behind is equally far from both ends; it goes left.
        let behind = stereo.gains(180.0);
        assert_eq!(behind[0] * behind[0] + behind[1] * behind[1], 1.0);
        let centre = stereo.gains(0.0);
        assert!((centre[0] - core::f64::consts::FRAC_1_SQRT_2).abs() < 1e-15);
        assert!((centre[1] - core::f64::consts::FRAC_1_SQRT_2).abs() < 1e-15);
    }

    #[test]
    fn a_ring_pans_across_the_back_too() {
        // Five channels: L, R, C, LS, RS.
        let ring = Speakers::new(&[-30.0, 30.0, 0.0, -110.0, 110.0]).unwrap();
        let back = ring.gains(180.0);
        assert!((back[3] - back[4]).abs() < 1e-12 && back[3] > 0.7);
        assert_eq!(back[0] + back[1] + back[2], 0.0);
        let left_of_centre = ring.gains(-15.0);
        assert!(left_of_centre[0] > 0.7 && left_of_centre[2] > 0.7);
    }

    #[test]
    fn bad_speakers_are_refused() {
        assert_eq!(Speakers::new(&[]), Err(SpaceError::Channels(0)));
        assert_eq!(Speakers::new(&[0.0; 17]), Err(SpaceError::Channels(17)));
        assert_eq!(Speakers::new(&[10.0, 370.0]), Err(SpaceError::Speakers));
        assert_eq!(Speakers::new(&[f64::NAN]), Err(SpaceError::Speakers));
        assert_eq!(Speakers::stereo_with_width(0.0), Err(SpaceError::Speakers));
        assert_eq!(Speakers::stereo_with_width(91.0), Err(SpaceError::Speakers));
    }

    #[test]
    fn fractional_delays_add_up_to_the_delay() {
        for samples in [1.0, 1.25, 1.5, 7.999, 12.3] {
            for interpolation in [Interpolation::Linear, Interpolation::Cubic] {
                let (whole, taps) = fractional_delay(samples, interpolation);
                // The taps pass a steady signal, and their centre of mass
                // is the fractional part: Lagrange interpolation is exact
                // for a straight line.
                let sum: f64 = taps.iter().map(|&t| f64::from(t)).sum();
                let centre: f64 = taps
                    .iter()
                    .enumerate()
                    .map(|(k, &t)| k as f64 * f64::from(t))
                    .sum();
                assert!((sum - 1.0).abs() < 1e-6, "{samples} {interpolation:?}");
                assert!(
                    (whole as f64 + centre - samples).abs() < 1e-6,
                    "{samples} {interpolation:?}"
                );
            }
        }
        assert_eq!(fractional_delay(0.4, Interpolation::Cubic).0, 0);
        assert_eq!(
            fractional_delay(2.6, Interpolation::None),
            (3, [1.0, 0.0, 0.0, 0.0])
        );
    }

    #[test]
    fn broken_samples_are_clipped_or_silenced() {
        assert_eq!(sanitise(0.5), 0.5);
        assert_eq!(sanitise(5_000.0), INPUT_LIMIT);
        assert_eq!(sanitise(f32::NEG_INFINITY), -INPUT_LIMIT);
        assert_eq!(sanitise(f32::NAN), 0.0);
    }

    #[test]
    fn the_air_cutoff_matches_the_loss_below_it() {
        let listener = Listener::new(Position::default(), Speakers::stereo());
        for distance in [5.0, 20.0, 60.0] {
            let fc = listener.air_cutoff_hz(distance);
            // At a tenth of the cutoff the one-pole takes what the air does.
            let f = fc / 10.0;
            let one_pole = 10.0 * (1.0 + (f / fc).powi(2)).log10();
            let air = Listener::DEFAULT_AIR_LOSS * (f / 8_000.0).powi(2) * distance;
            assert!((one_pole - air).abs() < 0.01 * air, "{distance} m");
        }
        assert!(listener.air_cutoff_hz(20.0) < listener.air_cutoff_hz(10.0));
        let still = Listener {
            air_loss_db_per_metre: 0.0,
            ..listener
        };
        assert_eq!(still.air_cutoff_hz(100.0), f64::INFINITY);
    }
}
