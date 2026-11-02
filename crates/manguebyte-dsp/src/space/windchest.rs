//! Placing many pipes into a few output channels through shared rings.

use manguebyte_core::AudioBuffer;

use super::{
    Interpolation, Lowpass, MAX_CHANNELS, MOVE_SECONDS, Placement, SpaceError, fractional_delay,
    limit_gain, lowpass, lowpass_step, ring_len, sanitise, shortest_delay,
};

/// One in the fixed-point sums of the rings: 2^32.
const FIXED_ONE: f64 = 4_294_967_296.0;

/// What a [`Windchest`] is built for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindchestSettings {
    /// How many pipes can be placed.
    pub pipes: usize,
    /// Output channels, 1 to [`MAX_CHANNELS`].
    pub channels: usize,
    /// In hertz.
    pub sample_rate: f64,
    /// The most frames one [`Windchest::add`] or [`Windchest::finish`]
    /// handles.
    pub max_block_size: usize,
    /// The longest delay a placement can have, in seconds; longer ones are
    /// held to it. 0.25 s (86 m) unless set.
    pub max_delay_seconds: f64,
    /// How fractional delays are made; [`Interpolation::Cubic`] unless set.
    pub interpolation: Interpolation,
}

impl WindchestSettings {
    /// Settings for `pipes` pipes into `channels` channels, with the
    /// defaults for the rest.
    pub fn new(pipes: usize, channels: usize, sample_rate: f64, max_block_size: usize) -> Self {
        Self {
            pipes,
            channels,
            sample_rate,
            max_block_size,
            max_delay_seconds: 0.25,
            interpolation: Interpolation::Cubic,
        }
    }
}

/// A placement as the audio thread uses it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Route {
    /// Whole samples of delay, after the taps.
    delay: usize,
    /// The fractional delay's taps: `z[n] = Σ taps[k]·x[n - k]`.
    taps: [f32; 4],
    /// The air's low-pass, or `None` for no filter.
    air: Option<Lowpass>,
    /// Each channel's gain times [`FIXED_ONE`].
    weights: [f64; MAX_CHANNELS],
}

/// A pipe's signal path: the air filter's memory and the last three samples
/// it gave, which the fractional delay reads.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Chain {
    state: f32,
    history: [f32; 3],
}

/// What the windchest keeps for one pipe.
#[derive(Debug, Clone, Copy)]
struct Pipe {
    route: Route,
    chain: Chain,
    /// While a move crossfades: the route and the chain it leaves.
    old_route: Route,
    old_chain: Chain,
    /// Samples of the crossfade done; at or above the fade's length when
    /// none runs.
    faded: usize,
    /// A placement waiting for the next block.
    pending: Option<Route>,
    /// The frame this pipe's next sample falls on if it keeps sounding.
    next_frame: u64,
}

/// Places every sounding pipe, a mono signal, into a number of output
/// channels, each with its own pan, delay, distance gain and air
/// absorption, from a [`Placement`].
///
/// # How it works
///
/// There is one accumulation ring per output channel, not a delay line per
/// pipe. [`add`](Self::add) runs a pipe's block through the air's one-pole
/// low-pass and the taps of its fractional delay, then writes each sample,
/// times each channel's gain, ahead into the rings at `now + delay`.
/// [`finish`](Self::finish) reads the block that is due, clears it and moves
/// on. A pipe costs memory for its placement and five numbers of state (the
/// low-pass's memory and the last three samples the taps read); it costs
/// time only in a block it is added to.
///
/// The rings hold fixed-point sums: each contribution (a sample times a
/// channel's gain) is rounded to the nearest multiple of 2^-32 and added
/// as a 64-bit integer. Integer sums do not depend on their order, so the
/// output is the same bit for bit however the blocks are cut and whatever
/// order the pipes are added in, which sums of floating-point numbers
/// written ahead could not promise: a cell gets its contributions from
/// different blocks depending on where the blocks begin. The rounding is
/// at most 2^-33 per contribution, about -200 dB; a frame's sum can reach
/// ±2^31 before it wraps. Input samples are held to ±1024 (anything that
/// is not a number counts as silence), channel gains to ±256 and each
/// contribution to ±2^18.
///
/// # Timing
///
/// Each sounding pipe is added once per block, with the block's frames,
/// before `finish` reads it out. A pipe that was not added in the previous
/// block counts as having been silent since: its filter and delay start
/// from silence, and what was still in its taps when it stopped is
/// dropped (three samples). The shortest delay is one sample with the
/// default [`Interpolation::Cubic`], zero with the others; the longest is
/// [`WindchestSettings::max_delay_seconds`].
///
/// # Moving a pipe
///
/// [`set_placement`](Self::set_placement) takes effect from the next block
/// the pipe is added in. A pipe that is not sounding moves at once. A
/// sounding one crossfades over 20 ms: its old placement fades out
/// linearly while the new one fades in, each with its own delay, gains and
/// filter, so nothing clicks; while it lasts the pipe costs about three
/// times as much (both paths, and the fades). A
/// placement set during a crossfade waits for it to end (only the latest
/// one is kept), so a pipe dragged around moves by one crossfade after
/// another. A move is a crossfade, not a glide: there is no Doppler shift.
///
/// # Real time
///
/// [`new`](Self::new) allocates; nothing else does. `set_placement`,
/// `add`, `finish` and `reset` are real-time safe.
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_dsp::space::{Listener, Position, Speakers, Windchest, WindchestSettings};
///
/// let listener = Listener::new(Position::new(0.0, 0.0, 0.0), Speakers::stereo());
/// let mut chest = Windchest::new(&WindchestSettings::new(64, 2, 48_000.0, 256)).unwrap();
/// // Pipe 0 stands 3.43 m ahead and 1 m to the left: about 10 ms away.
/// let placement = listener.place(Position::new(-1.0, 3.28, 0.0));
/// chest.set_placement(0, &placement);
///
/// let mut pipe = [0.0f32; 256];
/// pipe[0] = 1.0;
/// let mut out = AudioBuffer::<f32>::new(2, 256);
/// let (mut left, mut right) = (Vec::new(), Vec::new());
/// for _ in 0..3 {
///     chest.add(0, &pipe);
///     chest.finish(&mut out);
///     left.extend_from_slice(out.channel(0));
///     right.extend_from_slice(out.channel(1));
///     pipe[0] = 0.0;
/// }
/// // The impulse came out 480 frames later, louder on the left.
/// let at = (0..768).max_by(|&a, &b| left[a].total_cmp(&left[b])).unwrap();
/// assert!((at as f64 - placement.delay_seconds * 48_000.0).abs() < 1.0);
/// assert!(left[at] > right[at]);
/// ```
#[derive(Debug, Clone)]
pub struct Windchest {
    channels: usize,
    sample_rate: f64,
    max_block: usize,
    interpolation: Interpolation,
    /// The longest delay, in samples.
    max_delay: f64,
    fade_len: usize,
    /// The rings, one after another, each `mask + 1` long.
    rings: Vec<i64>,
    mask: usize,
    /// The frame `finish` reads out next.
    now: u64,
    pipes: Vec<Pipe>,
    /// The air filter's output, after the three samples of history.
    filtered: Vec<f32>,
    /// The fractional delay's output.
    delayed: Vec<f32>,
}

impl Windchest {
    /// Makes a windchest with every pipe at the listener
    /// ([`Placement::centred`]).
    pub fn new(settings: &WindchestSettings) -> Result<Self, SpaceError> {
        let channels = settings.channels;
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(SpaceError::Channels(channels));
        }
        let rate = settings.sample_rate;
        if !(rate.is_finite() && rate > 0.0) {
            return Err(SpaceError::SampleRate);
        }
        if settings.max_block_size == 0 {
            return Err(SpaceError::BlockSize);
        }
        let max_delay = settings.max_delay_seconds * rate;
        if !(max_delay.is_finite() && max_delay >= 0.0) {
            return Err(SpaceError::Delay);
        }
        let max_delay = max_delay.max(shortest_delay(settings.interpolation));
        let len = ring_len(max_delay.ceil() as usize + settings.max_block_size + 4);
        let fade_len = ((MOVE_SECONDS * rate).round() as usize).max(1);
        let mut chest = Self {
            channels,
            sample_rate: rate,
            max_block: settings.max_block_size,
            interpolation: settings.interpolation,
            max_delay,
            fade_len,
            rings: vec![0; len * channels],
            mask: len - 1,
            now: 0,
            pipes: Vec::new(),
            filtered: vec![0.0; settings.max_block_size + 3],
            delayed: vec![0.0; settings.max_block_size],
        };
        let centred = chest.route(&Placement::centred(channels));
        chest.pipes = vec![
            Pipe {
                route: centred,
                chain: Chain::default(),
                old_route: centred,
                old_chain: Chain::default(),
                faded: fade_len,
                pending: None,
                next_frame: u64::MAX,
            };
            settings.pipes
        ];
        Ok(chest)
    }

    /// How many pipes can be placed.
    pub fn pipes(&self) -> usize {
        self.pipes.len()
    }

    /// The number of output channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The sample rate, in hertz.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The most frames one `add` or `finish` handles.
    pub fn max_block_size(&self) -> usize {
        self.max_block
    }

    /// Whether `pipe` is crossfading to a new placement, or has one waiting.
    ///
    /// # Panics
    ///
    /// Panics if there is no such pipe.
    pub fn is_moving(&self, pipe: usize) -> bool {
        let pipe = &self.pipes[pipe];
        pipe.faded < self.fade_len || pipe.pending.is_some()
    }

    /// The route the audio thread runs for `placement`.
    fn route(&self, placement: &Placement) -> Route {
        let shortest = shortest_delay(self.interpolation);
        let samples = placement.delay_seconds * self.sample_rate;
        let samples = if samples >= shortest {
            samples.min(self.max_delay)
        } else {
            shortest
        };
        let (delay, taps) = fractional_delay(samples, self.interpolation);
        let mut weights = [0.0; MAX_CHANNELS];
        for (channel, weight) in weights.iter_mut().enumerate().take(self.channels) {
            *weight = limit_gain(placement.channel_gain(channel)) * FIXED_ONE;
        }
        Route {
            delay,
            taps,
            air: lowpass(placement.air_cutoff_hz, self.sample_rate),
            weights,
        }
    }

    /// Places `pipe`. Real-time safe. The placement takes effect from the
    /// next block the pipe is added in: at once if it was not sounding,
    /// through a 20 ms crossfade if it was. Channels the placement does not
    /// have get nothing from the pipe.
    ///
    /// # Panics
    ///
    /// Panics if there is no such pipe.
    pub fn set_placement(&mut self, pipe: usize, placement: &Placement) {
        let route = self.route(placement);
        self.pipes[pipe].pending = Some(route);
    }

    /// Adds `pipe`'s block, `input`, to the output. Call it once per block
    /// for each sounding pipe, with the block's frames, then
    /// [`finish`](Self::finish). Frames past
    /// [`max_block_size`](Self::max_block_size) are ignored. Real-time
    /// safe.
    ///
    /// # Panics
    ///
    /// Panics if there is no such pipe.
    pub fn add(&mut self, pipe: usize, input: &[f32]) {
        let frames = input.len().min(self.max_block);
        let input = &input[..frames];
        let now = self.now;
        let fade_len = self.fade_len;
        let Self {
            pipes,
            rings,
            filtered,
            delayed,
            interpolation,
            mask,
            channels,
            ..
        } = self;
        let mut rings = Rings {
            cells: rings,
            mask: *mask,
            channels: *channels,
            now,
        };
        let state = &mut pipes[pipe];
        if state.next_frame != now {
            // It was silent: start from silence, in its latest place.
            state.chain = Chain::default();
            state.faded = fade_len;
            if let Some(route) = state.pending.take() {
                state.route = route;
            }
        } else if state.faded >= fade_len {
            if let Some(route) = state.pending.take() {
                state.old_route = state.route;
                state.old_chain = state.chain;
                state.route = route;
                state.faded = 0;
            }
        }
        state.next_frame = now + frames as u64;

        // The new (or only) placement, faded in while a move lasts.
        let done = state.faded;
        let delayed = &mut delayed[..frames];
        let route = &state.route;
        run(
            filtered,
            delayed,
            *interpolation,
            &mut state.chain,
            route,
            input,
        );
        if done < fade_len {
            for (n, z) in delayed.iter_mut().enumerate() {
                *z *= fade_in(done + n, fade_len);
            }
        }
        rings.scatter(&state.route, delayed);

        // The old one, faded out.
        if done < fade_len {
            let left = (fade_len - done).min(frames);
            let delayed = &mut delayed[..left];
            let route = &state.old_route;
            let chain = &mut state.old_chain;
            run(
                filtered,
                delayed,
                *interpolation,
                chain,
                route,
                &input[..left],
            );
            for (n, z) in delayed.iter_mut().enumerate() {
                *z *= 1.0 - fade_in(done + n, fade_len);
            }
            rings.scatter(&state.old_route, delayed);
            state.faded = (done + frames).min(fade_len);
        }
    }

    /// Writes the block that is due into `output`, as many frames as it
    /// has (at most [`max_block_size`](Self::max_block_size); any beyond
    /// are silent), clears it from the rings and moves on by that many
    /// frames. Output channels past [`channels`](Self::channels) are
    /// cleared. Real-time safe.
    pub fn finish(&mut self, output: &mut AudioBuffer<f32>) {
        let frames = output.num_frames().min(self.max_block);
        let len = self.mask + 1;
        let start = self.now as usize & self.mask;
        let first = (len - start).min(frames);
        let mut rings = self.rings.chunks_exact_mut(len);
        for samples in output.iter_channels_mut() {
            match rings.next() {
                Some(ring) => {
                    let (head, tail) = samples[..frames].split_at_mut(first);
                    drain(&mut ring[start..start + first], head);
                    drain(&mut ring[..frames - first], tail);
                    samples[frames..].fill(0.0);
                }
                None => samples.fill(0.0),
            }
        }
        // Rings with no output channel are cleared all the same.
        for ring in rings {
            ring[start..start + first].fill(0);
            ring[..frames - first].fill(0);
        }
        self.now += frames as u64;
    }

    /// Clears the rings and every pipe's memory, and finishes every move.
    /// Placements are kept. Real-time safe.
    pub fn reset(&mut self) {
        self.rings.fill(0);
        for pipe in &mut self.pipes {
            pipe.chain = Chain::default();
            pipe.faded = self.fade_len;
            if let Some(route) = pipe.pending.take() {
                pipe.route = route;
            }
            pipe.next_frame = u64::MAX;
        }
        self.now = 0;
    }
}

/// The rings of a windchest, as one `add` writes them.
struct Rings<'a> {
    cells: &'a mut [i64],
    mask: usize,
    channels: usize,
    now: u64,
}

impl Rings<'_> {
    /// Writes `delayed` ahead into every ring, at `route`'s delay and with
    /// its gains.
    fn scatter(&mut self, route: &Route, delayed: &[f32]) {
        let len = self.mask + 1;
        let frames = delayed.len();
        let start = (self.now as usize).wrapping_add(route.delay) & self.mask;
        let first = (len - start).min(frames);
        for (ring, &weight) in self
            .cells
            .chunks_exact_mut(len)
            .zip(&route.weights[..self.channels])
        {
            if weight == 0.0 {
                continue;
            }
            accumulate(&mut ring[start..start + first], &delayed[..first], weight);
            accumulate(&mut ring[..frames - first], &delayed[first..], weight);
        }
    }
}

/// Runs `input` through the air filter and the fractional delay of
/// `route`, into `delayed`, which is as long as `input`. `filtered` is
/// scratch room of at least three samples more.
fn run(
    filtered: &mut [f32],
    delayed: &mut [f32],
    interpolation: Interpolation,
    chain: &mut Chain,
    route: &Route,
    input: &[f32],
) {
    let frames = input.len();
    let filtered = &mut filtered[..frames + 3];
    filtered[..3].copy_from_slice(&chain.history);
    match route.air {
        Some(filter) => {
            for (y, &x) in filtered[3..].iter_mut().zip(input) {
                *y = lowpass_step(&mut chain.state, sanitise(x), &filter);
            }
        }
        None => {
            for (y, &x) in filtered[3..].iter_mut().zip(input) {
                *y = sanitise(x);
            }
            // Where the memory of a filter that starts here would be.
            chain.state = filtered[frames + 2];
        }
    }
    chain.history.copy_from_slice(&filtered[frames..frames + 3]);
    let [h0, h1, h2, h3] = route.taps;
    match interpolation {
        Interpolation::None => delayed.copy_from_slice(&filtered[3..]),
        Interpolation::Linear => {
            for (n, z) in delayed.iter_mut().enumerate() {
                *z = h0 * filtered[n + 3] + h1 * filtered[n + 2];
            }
        }
        Interpolation::Cubic => {
            for (n, z) in delayed.iter_mut().enumerate() {
                *z = h0 * filtered[n + 3]
                    + h1 * filtered[n + 2]
                    + h2 * filtered[n + 1]
                    + h3 * filtered[n];
            }
        }
    }
}

/// The new placement's share at `done` samples into a crossfade of `len`.
#[inline]
fn fade_in(done: usize, len: usize) -> f32 {
    if done >= len {
        1.0
    } else {
        (done + 1) as f32 / len as f32
    }
}

/// Adds `samples` times `weight`, rounded to whole numbers, into `cells`.
///
/// Each product is held to ±2^50 and rounded to the nearest whole number,
/// ties to even, by adding `1.5·2^52`: the sum then has no bits below the
/// units, and its bits less those of `1.5·2^52` are the whole number. This
/// rounds exactly as `round_ties_even` would, on every processor, and runs
/// in vector registers where a conversion to a 64-bit integer has no
/// vector instruction before AVX-512.
#[inline]
fn accumulate(cells: &mut [i64], samples: &[f32], weight: f64) {
    const ROUNDER: f64 = 6_755_399_441_055_744.0;
    const LARGEST: f64 = 1_125_899_906_842_624.0;
    let offset = ROUNDER.to_bits() as i64;
    for (cell, &sample) in cells.iter_mut().zip(samples) {
        let product = (f64::from(sample) * weight).clamp(-LARGEST, LARGEST);
        let whole = (product + ROUNDER).to_bits() as i64 - offset;
        *cell = cell.wrapping_add(whole);
    }
}

/// Moves `cells` out as samples, leaving zeros.
#[inline]
fn drain(cells: &mut [i64], samples: &mut [f32]) {
    for (cell, sample) in cells.iter_mut().zip(samples) {
        *sample = (*cell as f64 * (1.0 / FIXED_ONE)) as f32;
        *cell = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contributions_round_to_the_nearest_whole_number_ties_to_even() {
        let samples = [0.5f32, 1.5, 2.5, -0.5, -1.5, -2.5, 0.75, -1.25, 1e30, -1e30];
        let mut cells = [0i64; 10];
        accumulate(&mut cells, &samples, 1.0);
        let largest = 1i64 << 50;
        assert_eq!(cells, [0, 2, 2, 0, -2, -2, 1, -1, largest, -largest]);
        // The sum wraps, and is exact again once it is back in range.
        let mut cell = [i64::MAX];
        accumulate(&mut cell, &[1.0], 1.0);
        assert_eq!(cell[0], i64::MIN);
        accumulate(&mut cell, &[-1.0], 1.0);
        assert_eq!(cell[0], i64::MAX);
    }

    #[test]
    fn a_pipe_that_fell_silent_starts_from_silence_in_its_new_place() {
        let mut chest = Windchest::new(&WindchestSettings::new(1, 1, 1_000.0, 8)).unwrap();
        let mut out = AudioBuffer::<f32>::new(1, 8);
        chest.add(0, &[1.0; 8]);
        chest.finish(&mut out);
        // Not added in the next block, so it moves at once when it sounds
        // again, with nothing left of its first block in its taps.
        let mut placement = Placement::centred(1);
        placement.delay_seconds = 0.004;
        chest.set_placement(0, &placement);
        chest.finish(&mut out);
        chest.finish(&mut out);
        assert!(chest.is_moving(0));
        chest.add(0, &[1.0; 8]);
        assert!(!chest.is_moving(0));
        chest.finish(&mut out);
        assert_eq!(&out.channel(0)[..4], &[0.0; 4]);
        assert!(out.channel(0)[4..].iter().all(|&x| (x - 1.0).abs() < 1e-6));
    }
}
