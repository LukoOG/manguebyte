//! Early reflections of a rectangular room by the image-source method.

use manguebyte_core::{AudioBuffer, vector};

use super::{
    Interpolation, Listener, Lowpass, MAX_CHANNELS, MOVE_SECONDS, Position, SpaceError,
    cutoff_for_loss, fractional_delay, limit_gain, loss_of_cutoff, lowpass, lowpass_step, ring_len,
    sanitise, shortest_delay,
};

/// One of the six surfaces of a [`Room`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The wall at `x = 0`.
    Left,
    /// The wall at `x = width`.
    Right,
    /// The wall at `y = 0`.
    Back,
    /// The wall at `y = length`.
    Front,
    /// The floor, at `z = 0`.
    Floor,
    /// The ceiling, at `z = height`.
    Ceiling,
}

impl Surface {
    /// Every surface, in the order of [`Room::walls`] and
    /// [`Reflection::hits`].
    pub const ALL: [Self; 6] = [
        Self::Left,
        Self::Right,
        Self::Back,
        Self::Front,
        Self::Floor,
        Self::Ceiling,
    ];

    /// The surface's place in [`ALL`](Self::ALL).
    pub fn index(self) -> usize {
        self as usize
    }
}

/// What one surface does to the sound it reflects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    /// The share of the sound's energy the surface absorbs, from 0 (none)
    /// to 1 (all), the same at every frequency. A reflection keeps
    /// `sqrt(1 - absorption)` of the pressure.
    pub absorption: f64,
    /// The cutoff of a first-order low-pass that one reflection from the
    /// surface applies, in hertz, for the extra loss of high frequencies of
    /// a soft or rough surface; infinite for none.
    pub high_cut_hz: f64,
}

impl Wall {
    /// A surface that absorbs `absorption` of the energy at every
    /// frequency.
    pub const fn new(absorption: f64) -> Self {
        Self {
            absorption,
            high_cut_hz: f64::INFINITY,
        }
    }

    /// The same surface, also taking high frequencies off through a
    /// one-pole at `hertz` on each reflection.
    pub const fn with_high_cut(mut self, hertz: f64) -> Self {
        self.high_cut_hz = hertz;
        self
    }

    /// The share of the pressure a reflection keeps, `sqrt(1 - absorption)`.
    pub fn reflection(&self) -> f64 {
        (1.0 - self.absorption).max(0.0).sqrt()
    }
}

/// A rectangular room, from `(0, 0, 0)` to `(width, length, height)` in
/// metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Room {
    /// Along `x`, in metres.
    pub width: f64,
    /// Along `y`, in metres.
    pub length: f64,
    /// Along `z`, in metres.
    pub height: f64,
    /// The surfaces, in the order of [`Surface::ALL`].
    pub walls: [Wall; 6],
}

impl Room {
    /// A room whose six surfaces are all `wall`.
    pub fn new(width: f64, length: f64, height: f64, wall: Wall) -> Self {
        Self {
            width,
            length,
            height,
            walls: [wall; 6],
        }
    }

    /// The same room with `surface` made `wall`.
    pub fn with_wall(mut self, surface: Surface, wall: Wall) -> Self {
        self.walls[surface.index()] = wall;
        self
    }

    /// The wall at `surface`.
    pub fn wall(&self, surface: Surface) -> Wall {
        self.walls[surface.index()]
    }

    /// Whether `position` is inside the room or on a wall.
    pub fn contains(&self, position: Position) -> bool {
        (0.0..=self.width).contains(&position.x)
            && (0.0..=self.length).contains(&position.y)
            && (0.0..=self.height).contains(&position.z)
    }

    fn check(&self) -> Result<(), SpaceError> {
        let sizes = [self.width, self.length, self.height];
        if !sizes.iter().all(|&size| size.is_finite() && size > 0.0) {
            return Err(SpaceError::Room);
        }
        if !self
            .walls
            .iter()
            .all(|wall| (0.0..=1.0).contains(&wall.absorption) && !wall.high_cut_hz.is_nan())
        {
            return Err(SpaceError::Room);
        }
        Ok(())
    }
}

/// Which reflections [`Reflections::compute`] keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflectionSettings {
    /// The most reflections on one path, 0 to
    /// [`MAX_ORDER`](Self::MAX_ORDER); 3 unless set.
    pub max_order: usize,
    /// Whether the direct path is one of the taps; not unless set, since a
    /// [`Windchest`](super::Windchest) gives it.
    pub direct: bool,
    /// Paths that arrive later than this, in seconds, are left out;
    /// infinite (none) unless set.
    pub max_delay_seconds: f64,
}

impl ReflectionSettings {
    /// The highest reflection order: 377 paths in all, the direct one
    /// included.
    pub const MAX_ORDER: usize = 6;

    /// How many paths there are of `order` reflections or fewer, the direct
    /// one included: one, and `4k² + 2` of each order `k`.
    pub fn paths_up_to(order: usize) -> usize {
        1 + (1..=order).map(|k| 4 * k * k + 2).sum::<usize>()
    }
}

impl Default for ReflectionSettings {
    fn default() -> Self {
        Self {
            max_order: 3,
            direct: false,
            max_delay_seconds: f64::INFINITY,
        }
    }
}

/// One path from the source to the listener: the image source it seems to
/// come from and what the walls and the distance do to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reflection {
    /// How many reflections it takes, 0 for the direct path.
    pub order: usize,
    /// How many of them are off each surface, in the order of
    /// [`Surface::ALL`].
    pub hits: [u8; 6],
    /// The image source.
    pub image: Position,
    /// The path's length, from the image source to the listener, in metres.
    pub distance: f64,
    /// The delay, in seconds.
    pub delay_seconds: f64,
    /// The walls' reflection factors, multiplied together, times the
    /// distance gain.
    pub gain: f64,
    /// Each channel's equal-power share, from the image source's azimuth.
    pub pan: [f64; MAX_CHANNELS],
    /// The cutoff of the one low-pass that stands for the walls' and the
    /// air's loss of high frequencies, in hertz; infinite for none.
    pub cutoff_hz: f64,
}

impl Reflection {
    /// The gain into `channel`: its pan share times [`gain`](Self::gain).
    pub fn channel_gain(&self, channel: usize) -> f64 {
        if channel < MAX_CHANNELS {
            self.pan[channel] * self.gain
        } else {
            0.0
        }
    }
}

/// The early reflections of a rectangular room for one source and one
/// listener, by the image-source method of J. B. Allen and D. A. Berkley,
/// "Image method for efficiently simulating small-room acoustics", JASA
/// 65(4), 1979.
///
/// Each wall mirrors the source; mirroring the mirrors gives the paths of
/// more reflections. Along `x`, with the source at `s` in a room of width
/// `w`, the images are at `(1 - 2p)·s + 2n·w` for every whole `n` and `p`
/// of 0 or 1, and the path reflects `|n - p|` times off the wall at 0 and
/// `|n|` times off the wall at `w`; the same along `y` and `z`. A path's
/// gain is the product of the reflection factors `sqrt(1 - absorption)` of
/// the walls it meets, times the listener's distance gain for its length;
/// its delay is its length over the speed of sound; it is panned from the
/// image's azimuth by the listener's [`Speakers`](super::Speakers).
///
/// The high frequencies a path loses to its walls' one-poles
/// ([`Wall::high_cut_hz`]) and to the air ([`Listener`]) are lumped into
/// one first-order low-pass, with the same low-frequency loss: each
/// one-pole at `fc` takes about `4.34·(f/fc)²` decibels below its cutoff,
/// and the lumped cutoff is the one whose loss is the sum.
///
/// The taps are sorted by delay. Computing them allocates: do it on
/// another thread and hand them to
/// [`EarlyReflections::set_reflections`].
///
/// ```
/// use manguebyte_dsp::space::{
///     Listener, Position, ReflectionSettings, Reflections, Room, Speakers, Wall,
/// };
///
/// let room = Room::new(10.0, 20.0, 8.0, Wall::new(0.19));
/// let listener = Listener::new(Position::new(5.0, 5.0, 1.5), Speakers::stereo());
/// let settings = ReflectionSettings { max_order: 1, ..ReflectionSettings::default() };
/// let reflections =
///     Reflections::compute(&room, Position::new(5.0, 15.0, 3.0), &listener, &settings).unwrap();
/// assert_eq!(reflections.taps().len(), 6);
/// // The floor's reflection: the image is 3 m under the floor.
/// let floor = reflections.taps().iter().find(|tap| tap.hits[4] == 1).unwrap();
/// assert_eq!(floor.image, Position::new(5.0, 15.0, -3.0));
/// assert!((floor.gain - 0.9 / floor.distance).abs() < 1e-12);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Reflections {
    taps: Vec<Reflection>,
    channels: usize,
}

impl Reflections {
    /// The reflections of `room` from `source` to `listener`.
    pub fn compute(
        room: &Room,
        source: Position,
        listener: &Listener,
        settings: &ReflectionSettings,
    ) -> Result<Self, SpaceError> {
        room.check()?;
        if settings.max_order > ReflectionSettings::MAX_ORDER {
            return Err(SpaceError::Order(settings.max_order));
        }
        if !room.contains(source) || !room.contains(listener.position) {
            return Err(SpaceError::OutsideRoom);
        }
        let order = settings.max_order;
        let axes = [
            images(source.x, room.width, order),
            images(source.y, room.length, order),
            images(source.z, room.height, order),
        ];
        let factors = room.walls.map(|wall| wall.reflection());
        let losses = room.walls.map(|wall| loss_of_cutoff(wall.high_cut_hz));
        let mut taps = Vec::with_capacity(ReflectionSettings::paths_up_to(order));
        for x in &axes[0] {
            for y in &axes[1] {
                for z in &axes[2] {
                    let hits = [x.1, x.2, y.1, y.2, z.1, z.2];
                    let total: usize = hits.iter().map(|&h| usize::from(h)).sum();
                    if total > order || (total == 0 && !settings.direct) {
                        continue;
                    }
                    let image = Position::new(x.0, y.0, z.0);
                    let distance = listener.position.distance_to(image);
                    let delay_seconds = listener.delay_seconds(distance);
                    if delay_seconds > settings.max_delay_seconds {
                        continue;
                    }
                    let mut gain = listener.distance_gain(distance);
                    let mut loss = listener.air_quadratic(distance);
                    for ((&count, &factor), &wall_loss) in hits.iter().zip(&factors).zip(&losses) {
                        gain *= factor.powi(i32::from(count));
                        loss += f64::from(count) * wall_loss;
                    }
                    taps.push(Reflection {
                        order: total,
                        hits,
                        image,
                        distance,
                        delay_seconds,
                        gain,
                        pan: listener.speakers.gains(listener.azimuth_of(image)),
                        cutoff_hz: cutoff_for_loss(loss),
                    });
                }
            }
        }
        taps.sort_by(|a, b| a.delay_seconds.total_cmp(&b.delay_seconds));
        Ok(Self {
            taps,
            channels: listener.speakers.channels(),
        })
    }

    /// The paths, earliest first.
    pub fn taps(&self) -> &[Reflection] {
        &self.taps
    }

    /// The number of output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The delay of the last path, in seconds; zero if there are none.
    pub fn latest_seconds(&self) -> f64 {
        self.taps.last().map_or(0.0, |tap| tap.delay_seconds)
    }
}

/// The images of a source at `s` along an axis of length `len`, with at
/// most `order` reflections: the coordinate and the reflections off the
/// low and the high wall.
fn images(s: f64, len: f64, order: usize) -> Vec<(f64, u8, u8)> {
    let reach = order as i64;
    let mut images = Vec::new();
    for n in -reach..=reach {
        for p in 0..=1i64 {
            let low = (n - p).unsigned_abs() as usize;
            let high = n.unsigned_abs() as usize;
            if low + high > order {
                continue;
            }
            let coordinate = (1 - 2 * p) as f64 * s + 2.0 * n as f64 * len;
            images.push((coordinate, low as u8, high as u8));
        }
    }
    images
}

/// One path as the audio thread plays it.
#[derive(Debug, Clone, Copy)]
struct Tap {
    delay: usize,
    taps: [f32; 4],
    air: Option<Lowpass>,
    gains: [f32; MAX_CHANNELS],
    state: f32,
}

/// Plays a set of [`Reflections`]: a mono input (a division's bus) into
/// its output channels, each path delayed, filtered and panned.
///
/// Each path reads the input's history at its delay, through third-order
/// Lagrange interpolation for the fraction of a sample (as
/// [`Interpolation::Cubic`]), runs its own one-pole low-pass if it has
/// one, and adds into every channel with its gains. The paths are added
/// in a fixed order, so the output is the same bit for bit however the
/// blocks are cut. Input samples are held to ±1024, and anything that is
/// not a number counts as silence.
///
/// A new set of reflections (a listener or a division that moved, other
/// walls) crossfades in over 20 ms; a set handed over during a crossfade
/// waits for its end, and only the latest is kept.
///
/// # Real time
///
/// [`new`](Self::new) and [`with_capacity`](Self::with_capacity) allocate;
/// [`set_reflections`](Self::set_reflections), the processing and
/// [`reset`](Self::reset) do not.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::space::{
///     EarlyReflections, Listener, Position, ReflectionSettings, Reflections, Room, Speakers,
///     Wall,
/// };
///
/// let room = Room::new(10.0, 20.0, 8.0, Wall::new(0.2).with_high_cut(6_000.0));
/// let listener = Listener::new(Position::new(5.0, 5.0, 1.5), Speakers::stereo());
/// let source = Position::new(5.0, 15.0, 3.0);
/// let reflections =
///     Reflections::compute(&room, source, &listener, &ReflectionSettings::default()).unwrap();
/// let mut early = EarlyReflections::new(&reflections, 48_000.0, 512).unwrap();
///
/// let mut impulse = vec![0.0f32; 4_096];
/// impulse[0] = 1.0;
/// let mut out = AudioBuffer::<f32>::new(2, 4_096);
/// early.process(&impulse, &mut out);
/// let first = reflections.taps()[0].delay_seconds * 48_000.0;
/// let heard = out.channel(0).iter().position(|s| s.abs() > 1e-4).unwrap();
/// assert!((heard as f64 - first).abs() < 2.0);
/// ```
#[derive(Debug, Clone)]
pub struct EarlyReflections {
    channels: usize,
    sample_rate: f64,
    max_block: usize,
    max_taps: usize,
    /// The longest delay, in samples.
    max_delay: f64,
    fade_len: usize,
    /// The input's history.
    history: Vec<f32>,
    mask: usize,
    now: u64,
    /// The playing set, the one fading out, and a spare.
    banks: [Vec<Tap>; 3],
    current: usize,
    old: usize,
    spare: usize,
    /// Whether the spare holds a set waiting to be played.
    pending: bool,
    /// Samples of the crossfade done; at or above its length when none
    /// runs.
    faded: usize,
    /// One path's output for a block.
    path: Vec<f32>,
}

impl EarlyReflections {
    /// Makes a player for `reflections`, with room for as many paths as it
    /// has and delays up to its latest.
    pub fn new(
        reflections: &Reflections,
        sample_rate: f64,
        max_block_size: usize,
    ) -> Result<Self, SpaceError> {
        Self::with_capacity(
            reflections,
            sample_rate,
            max_block_size,
            reflections.taps.len(),
            reflections.latest_seconds(),
        )
    }

    /// Makes a player for `reflections`, with room for sets of up to
    /// `max_taps` paths (at least as many as it has) and delays up to
    /// `max_delay_seconds` (at least its latest), so that other sets can
    /// follow: [`ReflectionSettings::paths_up_to`] gives the most paths of
    /// an order.
    pub fn with_capacity(
        reflections: &Reflections,
        sample_rate: f64,
        max_block_size: usize,
        max_taps: usize,
        max_delay_seconds: f64,
    ) -> Result<Self, SpaceError> {
        let channels = reflections.channels;
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(SpaceError::Channels(channels));
        }
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(SpaceError::SampleRate);
        }
        if max_block_size == 0 {
            return Err(SpaceError::BlockSize);
        }
        let latest = max_delay_seconds.max(reflections.latest_seconds()) * sample_rate;
        if !(latest.is_finite() && latest >= 0.0) {
            return Err(SpaceError::Delay);
        }
        let max_delay = latest.max(shortest_delay(Interpolation::Cubic));
        let len = ring_len(max_delay.ceil() as usize + max_block_size + 4);
        let max_taps = max_taps.max(reflections.taps.len());
        let bank = || Vec::with_capacity(max_taps);
        let mut early = Self {
            channels,
            sample_rate,
            max_block: max_block_size,
            max_taps,
            max_delay,
            fade_len: ((MOVE_SECONDS * sample_rate).round() as usize).max(1),
            history: vec![0.0; len],
            mask: len - 1,
            now: 0,
            banks: [bank(), bank(), bank()],
            current: 0,
            old: 1,
            spare: 2,
            pending: false,
            faded: 0,
            path: vec![0.0; max_block_size],
        };
        early.faded = early.fade_len;
        early.fill(early.current, reflections);
        Ok(early)
    }

    /// The number of output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The paths of the set playing (or about to, if one is waiting).
    pub fn len(&self) -> usize {
        let bank = if self.pending {
            self.spare
        } else {
            self.current
        };
        self.banks[bank].len()
    }

    /// Whether there are no paths.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a new set is crossfading in or waiting to.
    pub fn is_changing(&self) -> bool {
        self.pending || self.faded < self.fade_len
    }

    /// Writes `reflections` into bank `bank`, which must have room.
    fn fill(&mut self, bank: usize, reflections: &Reflections) {
        let (rate, max_delay, channels) = (self.sample_rate, self.max_delay, self.channels);
        let taps = &mut self.banks[bank];
        taps.clear();
        taps.extend(reflections.taps.iter().map(|path| {
            let samples = (path.delay_seconds * rate).clamp(1.0, max_delay);
            let (delay, interpolation) = fractional_delay(samples, Interpolation::Cubic);
            let mut gains = [0.0; MAX_CHANNELS];
            for (channel, gain) in gains.iter_mut().enumerate().take(channels) {
                *gain = limit_gain(path.channel_gain(channel)) as f32;
            }
            Tap {
                delay,
                taps: interpolation,
                air: lowpass(path.cutoff_hz, rate),
                gains,
                state: 0.0,
            }
        }));
    }

    /// Hands over a new set of paths, to crossfade to from the next block.
    /// Real-time safe. Refused if it has more paths or channels, or a later
    /// path, than there is room for.
    pub fn set_reflections(&mut self, reflections: &Reflections) -> Result<(), SpaceError> {
        if reflections.taps.len() > self.max_taps
            || reflections.channels != self.channels
            || reflections.latest_seconds() * self.sample_rate > self.max_delay
        {
            return Err(SpaceError::DoesNotFit);
        }
        self.fill(self.spare, reflections);
        self.pending = true;
        Ok(())
    }

    /// Writes the reflections of `input` into `output`, replacing what it
    /// held, for as many frames as both have. Channels past
    /// [`channels`](Self::channels) are cleared. Real-time safe.
    pub fn process(&mut self, input: &[f32], output: &mut AudioBuffer<f32>) {
        output.clear();
        self.process_add(input, output);
    }

    /// Adds the reflections of `input` to `output`, for as many frames as
    /// both have; channels past [`channels`](Self::channels) are left
    /// alone. Real-time safe.
    pub fn process_add(&mut self, input: &[f32], output: &mut AudioBuffer<f32>) {
        let frames = input.len().min(output.num_frames());
        let mut start = 0;
        while start < frames {
            let end = (start + self.max_block).min(frames);
            self.block(&input[start..end], output, start);
            start = end;
        }
    }

    /// One piece of at most `max_block` frames, added to `output` from
    /// frame `offset`.
    fn block(&mut self, input: &[f32], output: &mut AudioBuffer<f32>, offset: usize) {
        let frames = input.len();
        for (k, &x) in input.iter().enumerate() {
            self.history[(self.now as usize).wrapping_add(k) & self.mask] = sanitise(x);
        }
        if self.pending && self.faded >= self.fade_len {
            (self.old, self.current, self.spare) = (self.current, self.spare, self.old);
            self.pending = false;
            self.faded = 0;
        }
        let done = self.faded;
        let fade_len = self.fade_len;
        let channels = self.channels.min(output.num_channels());
        let mut playing = Player {
            history: &self.history,
            mask: self.mask,
            now: self.now,
            path: &mut self.path[..frames],
        };
        for tap in &mut self.banks[self.current] {
            playing.read(tap);
            if done < fade_len {
                for (n, z) in playing.path.iter_mut().enumerate() {
                    *z *= fade_in(done + n, fade_len);
                }
            }
            for (channel, &gain) in tap.gains[..channels].iter().enumerate() {
                let samples = &mut output.channel_mut(channel)[offset..offset + frames];
                vector::add_scaled(samples, playing.path, gain);
            }
        }
        if done < fade_len {
            let left = (fade_len - done).min(frames);
            for tap in &mut self.banks[self.old] {
                playing.read(tap);
                let path = &mut playing.path[..left];
                for (n, z) in path.iter_mut().enumerate() {
                    *z *= 1.0 - fade_in(done + n, fade_len);
                }
                for (channel, &gain) in tap.gains[..channels].iter().enumerate() {
                    let samples = &mut output.channel_mut(channel)[offset..offset + left];
                    vector::add_scaled(samples, path, gain);
                }
            }
            self.faded = (done + frames).min(fade_len);
        }
        self.now += frames as u64;
    }

    /// Clears the input's history and the paths' filters, and finishes any
    /// change of set. Real-time safe.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        if self.pending {
            (self.old, self.current, self.spare) = (self.current, self.spare, self.old);
            self.pending = false;
        }
        self.faded = self.fade_len;
        for bank in &mut self.banks {
            for tap in bank {
                tap.state = 0.0;
            }
        }
        self.now = 0;
    }
}

/// Reads paths out of the input's history for one block.
struct Player<'a> {
    history: &'a [f32],
    mask: usize,
    now: u64,
    path: &'a mut [f32],
}

impl Player<'_> {
    /// Fills `path` with what `tap` gives over the block.
    fn read(&mut self, tap: &mut Tap) {
        let [h0, h1, h2, h3] = tap.taps;
        let history = self.history;
        let mask = self.mask;
        // The oldest sample the block reads, and whether the samples from
        // there on lie in one piece of the ring.
        let first = (self.now as usize).wrapping_sub(tap.delay + 3) & mask;
        let frames = self.path.len();
        if first + frames + 3 <= history.len() {
            let x = &history[first..first + frames + 3];
            for (n, z) in self.path.iter_mut().enumerate() {
                *z = h0 * x[n + 3] + h1 * x[n + 2] + h2 * x[n + 1] + h3 * x[n];
            }
        } else {
            for (n, z) in self.path.iter_mut().enumerate() {
                let t = first.wrapping_add(n);
                *z = h0 * history[t.wrapping_add(3) & mask]
                    + h1 * history[t.wrapping_add(2) & mask]
                    + h2 * history[t.wrapping_add(1) & mask]
                    + h3 * history[t & mask];
            }
        }
        if let Some(filter) = tap.air {
            for z in self.path.iter_mut() {
                *z = lowpass_step(&mut tap.state, *z, &filter);
            }
        }
    }
}

/// The new set's share at `done` samples into a crossfade of `len`.
#[inline]
fn fade_in(done: usize, len: usize) -> f32 {
    if done >= len {
        1.0
    } else {
        (done + 1) as f32 / len as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Speakers;

    #[test]
    fn the_lattice_has_four_k_squared_plus_two_images_of_each_order() {
        let room = Room::new(7.0, 11.0, 5.0, Wall::new(0.3));
        let listener = Listener::new(Position::new(2.0, 3.0, 1.5), Speakers::stereo());
        for order in 0..=ReflectionSettings::MAX_ORDER {
            let settings = ReflectionSettings {
                max_order: order,
                direct: true,
                ..ReflectionSettings::default()
            };
            let reflections =
                Reflections::compute(&room, Position::new(5.0, 8.0, 2.0), &listener, &settings)
                    .unwrap();
            assert_eq!(
                reflections.taps().len(),
                ReflectionSettings::paths_up_to(order)
            );
            for k in 1..=order {
                let of_order = reflections.taps().iter().filter(|t| t.order == k).count();
                assert_eq!(of_order, 4 * k * k + 2);
            }
        }
    }

    #[test]
    fn bad_rooms_are_refused() {
        let listener = Listener::new(Position::new(1.0, 1.0, 1.0), Speakers::stereo());
        let source = Position::new(2.0, 2.0, 1.0);
        let settings = ReflectionSettings::default();
        let room = Room::new(5.0, 5.0, 3.0, Wall::new(0.2));
        assert!(Reflections::compute(&room, source, &listener, &settings).is_ok());
        let flat = Room::new(5.0, 5.0, 0.0, Wall::new(0.2));
        assert_eq!(
            Reflections::compute(&flat, source, &listener, &settings),
            Err(SpaceError::Room)
        );
        let odd = room.with_wall(Surface::Floor, Wall::new(1.5));
        assert_eq!(
            Reflections::compute(&odd, source, &listener, &settings),
            Err(SpaceError::Room)
        );
        let outside = Position::new(6.0, 2.0, 1.0);
        assert_eq!(
            Reflections::compute(&room, outside, &listener, &settings),
            Err(SpaceError::OutsideRoom)
        );
        let deep = ReflectionSettings {
            max_order: 7,
            ..settings
        };
        assert_eq!(
            Reflections::compute(&room, source, &listener, &deep),
            Err(SpaceError::Order(7))
        );
    }
}
