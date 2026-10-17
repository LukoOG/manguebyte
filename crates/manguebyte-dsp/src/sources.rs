//! Sources of audio built from other sources, and sources as processors.
//!
//! [`AudioSource`] is the pulled counterpart of [`Processor`]: something
//! that makes audio when asked. This module holds the sources that take
//! their audio from other sources, and the bridges between the two:
//!
//! - [`MixerSource`]: several sources added together, each with a gain.
//! - [`ResamplingSource`]: a source at one sample rate, pulled at another,
//!   through the band-limited [`Resampler`].
//! - [`ChannelRemapSource`]: a source's channels routed to other channels,
//!   one to many or not at all.
//! - [`ProcessedSource`]: a source through a processor, such as a filter
//!   or a chain.
//! - [`SourceProcessor`]: a source as a processor, to be a node of a graph
//!   or the whole of a device callback.
//!
//! A file played from disk is a `TransportSource` of `manguebyte-formats`,
//! and audio in memory a [`BufferSource`](manguebyte_core::source::BufferSource).
//!
//! ```
//! use manguebyte_core::AudioBuffer;
//! use manguebyte_core::source::{AudioSource, BufferSource};
//! use manguebyte_dsp::sources::{ChannelRemapSource, MixerSource, ResamplingSource};
//!
//! // A mono loop at 44.1 kHz, played on both speakers of a 48 kHz device,
//! // mixed with a stereo one at half level.
//! let mut mono = AudioBuffer::<f32>::new(1, 441);
//! mono.channel_mut(0).fill(0.5);
//! let mono = BufferSource::new(mono).looping(true);
//! let mono = ResamplingSource::new(mono, 44_100.0, 1);
//! let both = ChannelRemapSource::new(mono, 1, &[Some(0), Some(0)]);
//! let mut stereo = AudioBuffer::<f32>::new(2, 480);
//! stereo.channel_mut(1).fill(1.0);
//!
//! let mut mixer = MixerSource::new(2);
//! mixer.add(both, 1.0);
//! mixer.add(BufferSource::new(stereo).looping(true), 0.5);
//! mixer.prepare(48_000.0, 256);
//!
//! let mut out = AudioBuffer::<f32>::new(2, 256);
//! mixer.pull(&mut out);
//! mixer.pull(&mut out);
//! assert!((out.channel(0)[100] - 0.5).abs() < 1e-3);
//! assert!((out.channel(1)[100] - 1.0).abs() < 1e-3);
//! ```
//!
//! # Real-time safety
//!
//! Construction, [`MixerSource::add`] and `prepare` allocate. `pull`,
//! `reset` and the setters marked so do not.

use manguebyte_core::source::{AudioSource, PositionableSource};
use manguebyte_core::{AudioBuffer, Sample};

use crate::{ProcessSpec, Processor, Resampler};

/// One input of a [`MixerSource`].
struct MixerInput<S: Sample> {
    source: Box<dyn AudioSource<S> + Send>,
    gain: S,
}

/// Several sources added together, each scaled by its own gain.
///
/// Each input is pulled into a scratch buffer of the mixer's channel count
/// and added to the output. A mixer with no inputs is silent.
pub struct MixerSource<S: Sample = f32> {
    inputs: Vec<MixerInput<S>>,
    scratch: AudioBuffer<S>,
    channels: usize,
    /// What the mixer was prepared for, so that a source added later is
    /// prepared the same way.
    prepared: Option<(f64, usize)>,
}

impl<S: Sample> MixerSource<S> {
    /// A mixer of `channels` channels with no inputs.
    pub fn new(channels: usize) -> Self {
        Self {
            inputs: Vec::new(),
            scratch: AudioBuffer::new(channels, 0),
            channels,
            prepared: None,
        }
    }

    /// Adds an input at `gain` and returns its index. If the mixer has
    /// been prepared, the source is prepared the same way. Allocates.
    pub fn add(&mut self, source: impl AudioSource<S> + Send + 'static, gain: f32) -> usize {
        let mut source: Box<dyn AudioSource<S> + Send> = Box::new(source);
        if let Some((sample_rate, block)) = self.prepared {
            source.prepare(sample_rate, block);
        }
        self.inputs.push(MixerInput {
            source,
            gain: S::from_f64(f64::from(gain)),
        });
        self.inputs.len() - 1
    }

    /// Removes and returns the input at `index`, which shifts the inputs
    /// after it down by one. Frees nothing: the source is handed back, to
    /// be dropped off the audio thread.
    pub fn remove(&mut self, index: usize) -> Option<Box<dyn AudioSource<S> + Send>> {
        (index < self.inputs.len()).then(|| self.inputs.remove(index).source)
    }

    /// Sets the gain of input `index`. Does nothing for an index that is
    /// not an input. Real-time safe. The change is immediate: ramp it
    /// over a few blocks yourself to avoid a click.
    pub fn set_gain(&mut self, index: usize, gain: f32) {
        if let Some(input) = self.inputs.get_mut(index) {
            input.gain = S::from_f64(f64::from(gain));
        }
    }

    /// How many inputs there are.
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether there are no inputs.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// The input at `index`, to change its settings.
    pub fn input_mut(
        &mut self,
        index: usize,
    ) -> Option<&mut (dyn AudioSource<S> + Send + 'static)> {
        self.inputs.get_mut(index).map(|input| &mut *input.source)
    }
}

impl<S: Sample> AudioSource<S> for MixerSource<S> {
    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) {
        self.scratch = AudioBuffer::new(self.channels, max_block_size);
        for input in &mut self.inputs {
            input.source.prepare(sample_rate, max_block_size);
        }
        self.prepared = Some((sample_rate, max_block_size));
    }

    fn pull(&mut self, out: &mut AudioBuffer<S>) {
        out.clear();
        let frames = out.num_frames().min(self.scratch.capacity());
        self.scratch.set_num_frames(frames);
        let shared = out.num_channels().min(self.channels);
        for input in &mut self.inputs {
            input.source.pull(&mut self.scratch);
            for channel in 0..shared {
                let from = self.scratch.channel(channel);
                for (to, &sample) in out.channel_mut(channel)[..frames].iter_mut().zip(from) {
                    *to += sample * input.gain;
                }
            }
        }
    }

    fn reset(&mut self) {
        for input in &mut self.inputs {
            input.source.reset();
        }
    }
}

/// The largest factor a [`ResamplingSource`] converts by, up or down.
pub const MAX_RATIO: f64 = 256.0;

/// A source at one sample rate, pulled at another.
///
/// The source it wraps is pulled in blocks of a fixed size, whatever the
/// size of the blocks pulled from this one, and the conversion keeps its
/// position across blocks, so the output does not depend on how it is
/// pulled. The conversion is the band-limited interpolation of
/// [`Resampler`], lined up with the input: the output starts with the
/// source's first frame, with no delay.
///
/// The output rate is the sample rate given to
/// [`prepare`](AudioSource::prepare); the input rate is the source's own,
/// given to [`new`](Self::new). The ratio between them is kept between
/// 1/256 and 256.
///
/// # Seeking
///
/// The conversion holds some of the source's past audio, and some
/// converted audio not pulled yet. After seeking the source through
/// [`inner_mut`](Self::inner_mut), call [`reset`](AudioSource::reset) so
/// that what comes out next is the new position's. A transport that is
/// seeked from another thread is heard at its new place after at most one
/// block of the source and half the converter's kernel.
pub struct ResamplingSource<T, S: Sample = f32> {
    inner: T,
    input_rate: f64,
    channels: usize,
    resampler: Option<Resampler<S>>,
    /// One block pulled from the source.
    input: AudioBuffer<S>,
    /// What one block converts to.
    converted: AudioBuffer<S>,
    /// Converted audio not pulled yet, at its start.
    pending: AudioBuffer<S>,
    filled: usize,
    /// Frames pulled from the source at a time.
    chunk: usize,
    /// The most source blocks one pull may need, past which it gives
    /// silence rather than run on.
    max_rounds: usize,
}

impl<T, S: Sample> ResamplingSource<T, S> {
    /// Wraps `inner`, whose audio is at `input_rate` hertz and has
    /// `channels` channels.
    ///
    /// # Panics
    ///
    /// Panics unless `input_rate` is positive and finite.
    pub fn new(inner: T, input_rate: f64, channels: usize) -> Self {
        assert!(
            input_rate.is_finite() && input_rate > 0.0,
            "the input rate must be positive"
        );
        Self {
            inner,
            input_rate,
            channels,
            resampler: None,
            input: AudioBuffer::new(channels, 0),
            converted: AudioBuffer::new(channels, 0),
            pending: AudioBuffer::new(channels, 0),
            filled: 0,
            chunk: 0,
            max_rounds: 0,
        }
    }

    /// The source being converted.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// The source being converted, to change its settings or seek it.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// The source's sample rate, in hertz.
    pub fn input_rate(&self) -> f64 {
        self.input_rate
    }

    /// The source, unwrapped.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: AudioSource<S>, S: Sample> AudioSource<S> for ResamplingSource<T, S> {
    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) {
        let output_rate = sample_rate
            .max(self.input_rate / MAX_RATIO)
            .min(self.input_rate * MAX_RATIO);
        let ratio = output_rate / self.input_rate;
        // Enough source per block that a pull usually takes one or two
        // blocks of it, and never fewer than 64 frames at a time.
        let chunk = ((max_block_size.max(1) as f64 / ratio).ceil() as usize).clamp(64, 1 << 16);
        let mut resampler = Resampler::new(self.input_rate, output_rate, self.channels, chunk);
        resampler.align_to_input();
        let per_chunk = resampler.max_output_frames(chunk);
        // The converter needs its kernel's reach of input before it gives
        // anything, then about `chunk * ratio` frames per block.
        let reach = (resampler.latency_frames() / ratio).ceil() as usize * 3;
        self.max_rounds = max_block_size.div_ceil(((chunk as f64 * ratio).floor() as usize).max(1))
            + reach.div_ceil(chunk)
            + 2;
        self.input = AudioBuffer::new(self.channels, chunk);
        self.converted = AudioBuffer::new(self.channels, per_chunk);
        self.pending = AudioBuffer::new(self.channels, max_block_size + per_chunk);
        self.filled = 0;
        self.chunk = chunk;
        self.resampler = Some(resampler);
        self.inner.prepare(self.input_rate, chunk);
    }

    fn pull(&mut self, out: &mut AudioBuffer<S>) {
        let frames = out.num_frames();
        let Some(resampler) = self.resampler.as_mut() else {
            out.clear();
            return;
        };
        let wanted = frames.min(self.pending.capacity() - self.converted.capacity());
        let mut rounds = 0;
        while self.filled < wanted && rounds < self.max_rounds {
            rounds += 1;
            self.input.set_num_frames(self.chunk);
            self.inner.pull(&mut self.input);
            let produced = resampler.process(&self.input, &mut self.converted);
            self.pending.set_num_frames(self.pending.capacity());
            for channel in 0..self.channels {
                self.pending.channel_mut(channel)[self.filled..self.filled + produced]
                    .copy_from_slice(&self.converted.channel(channel)[..produced]);
            }
            self.filled += produced;
        }
        let given = self.filled.min(frames);
        self.pending.set_num_frames(self.pending.capacity());
        for (index, channel) in out.iter_channels_mut().enumerate() {
            if index < self.channels {
                channel[..given].copy_from_slice(&self.pending.channel(index)[..given]);
                channel[given..].fill(S::ZERO);
            } else {
                channel.fill(S::ZERO);
            }
        }
        // What is left moves to the front.
        for channel in 0..self.channels {
            self.pending
                .channel_mut(channel)
                .copy_within(given..self.filled, 0);
        }
        self.filled -= given;
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.filled = 0;
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
            resampler.align_to_input();
        }
    }
}

/// A source's channels, routed: each output channel takes one of the
/// source's channels, or is silent.
///
/// One source channel can feed several outputs, which plays a mono source
/// on both speakers; outputs past the routing are silent.
pub struct ChannelRemapSource<T, S: Sample = f32> {
    inner: T,
    /// For each output channel, the source channel it takes.
    routes: Vec<Option<usize>>,
    scratch: AudioBuffer<S>,
    inner_channels: usize,
}

impl<T, S: Sample> ChannelRemapSource<T, S> {
    /// Wraps `inner`, which has `inner_channels` channels, with output
    /// channel `n` taking source channel `routes[n]`. A route to a channel
    /// the source does not have is silent.
    pub fn new(inner: T, inner_channels: usize, routes: &[Option<usize>]) -> Self {
        Self {
            inner,
            routes: routes.to_vec(),
            scratch: AudioBuffer::new(inner_channels, 0),
            inner_channels,
        }
    }

    /// Routes output channel `output` from source channel `from`, or makes
    /// it silent with `None`. Does nothing for an output past those given
    /// to [`new`](Self::new). Real-time safe.
    pub fn set_route(&mut self, output: usize, from: Option<usize>) {
        if let Some(route) = self.routes.get_mut(output) {
            *route = from;
        }
    }

    /// The source channel output `output` takes.
    pub fn route(&self, output: usize) -> Option<usize> {
        self.routes.get(output).copied().flatten()
    }

    /// The wrapped source.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: AudioSource<S>, S: Sample> AudioSource<S> for ChannelRemapSource<T, S> {
    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) {
        self.scratch = AudioBuffer::new(self.inner_channels, max_block_size);
        self.inner.prepare(sample_rate, max_block_size);
    }

    fn pull(&mut self, out: &mut AudioBuffer<S>) {
        let frames = out.num_frames().min(self.scratch.capacity());
        self.scratch.set_num_frames(frames);
        self.inner.pull(&mut self.scratch);
        for (index, channel) in out.iter_channels_mut().enumerate() {
            match self.routes.get(index).copied().flatten() {
                Some(from) if from < self.inner_channels => {
                    channel[..frames].copy_from_slice(self.scratch.channel(from));
                    channel[frames..].fill(S::ZERO);
                }
                _ => channel.fill(S::ZERO),
            }
        }
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

impl<T: PositionableSource<S>, S: Sample> PositionableSource<S> for ChannelRemapSource<T, S> {
    fn position(&self) -> u64 {
        self.inner.position()
    }
    fn seek(&mut self, frame: u64) {
        self.inner.seek(frame);
    }
    fn len_frames(&self) -> Option<u64> {
        self.inner.len_frames()
    }
    fn is_looping(&self) -> bool {
        self.inner.is_looping()
    }
}

/// A source through a processor: the source is pulled, then the processor
/// transforms what it gave in place.
pub struct ProcessedSource<T, P> {
    inner: T,
    processor: P,
    channels: usize,
}

impl<T, P> ProcessedSource<T, P> {
    /// `inner` through `processor`, which is prepared for `channels`
    /// channels.
    pub fn new(inner: T, processor: P, channels: usize) -> Self {
        Self {
            inner,
            processor,
            channels,
        }
    }

    /// The processor, to change its settings.
    pub fn processor_mut(&mut self) -> &mut P {
        &mut self.processor
    }

    /// The source.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<S: Sample, T: AudioSource<S>, P: Processor<S>> AudioSource<S> for ProcessedSource<T, P> {
    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) {
        self.inner.prepare(sample_rate, max_block_size);
        self.processor.prepare(&ProcessSpec {
            sample_rate,
            max_block_size,
            num_channels: self.channels,
        });
    }

    fn pull(&mut self, out: &mut AudioBuffer<S>) {
        self.inner.pull(out);
        self.processor.process(out);
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.processor.reset();
    }
}

/// A source as a [`Processor`]: whatever the processor is handed is
/// replaced by what the source gives.
///
/// This is how a source becomes a node of a `manguebyte-graph` graph,
/// where it plays into whatever it is connected to, or the whole of a
/// device callback:
///
/// ```
/// use manguebyte_core::AudioBuffer;
/// use manguebyte_core::source::BufferSource;
/// use manguebyte_dsp::sources::SourceProcessor;
/// use manguebyte_dsp::{ProcessSpec, Processor};
///
/// let mut sample = AudioBuffer::<f32>::new(2, 4);
/// sample.channel_mut(0).fill(0.25);
/// let mut player = SourceProcessor::new(BufferSource::new(sample));
/// player.prepare(&ProcessSpec { sample_rate: 48_000.0, max_block_size: 8, num_channels: 2 });
///
/// // In a device callback: `move |buffer| player.process(buffer)`.
/// let mut block = AudioBuffer::<f32>::new(2, 8);
/// block.channel_mut(1).fill(9.0);
/// player.process(&mut block);
/// assert_eq!(block.channel(0), &[0.25, 0.25, 0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
/// assert_eq!(block.channel(1), &[0.0; 8]);
/// ```
pub struct SourceProcessor<T> {
    source: T,
}

impl<T> SourceProcessor<T> {
    /// Wraps `source`.
    pub fn new(source: T) -> Self {
        Self { source }
    }

    /// The source, to control it.
    pub fn source_mut(&mut self) -> &mut T {
        &mut self.source
    }

    /// The source, unwrapped.
    pub fn into_inner(self) -> T {
        self.source
    }
}

impl<S: Sample, T: AudioSource<S>> Processor<S> for SourceProcessor<T> {
    fn prepare(&mut self, spec: &ProcessSpec) {
        self.source.prepare(spec.sample_rate, spec.max_block_size);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<S>) {
        self.source.pull(buffer);
    }

    fn reset(&mut self) {
        self.source.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gain;
    use manguebyte_core::source::BufferSource;

    fn tone(frames: usize, rate: f64, hz: f64) -> AudioBuffer<f32> {
        let mut buffer = AudioBuffer::new(1, frames);
        for (index, sample) in buffer.channel_mut(0).iter_mut().enumerate() {
            *sample = (2.0 * core::f64::consts::PI * hz * index as f64 / rate).sin() as f32;
        }
        buffer
    }

    /// Pulls `total` frames in blocks of the sizes given, cycling.
    fn pull<T: AudioSource>(
        source: &mut T,
        channels: usize,
        sizes: &[usize],
        total: usize,
    ) -> Vec<Vec<f32>> {
        let mut out = AudioBuffer::new(channels, 512);
        let mut heard = vec![Vec::new(); channels];
        let mut size = sizes.iter().cycle();
        while heard[0].len() < total {
            out.set_num_frames((*size.next().unwrap()).min(total - heard[0].len()));
            source.pull(&mut out);
            for (channel, heard) in heard.iter_mut().enumerate() {
                heard.extend_from_slice(out.channel(channel));
            }
        }
        heard
    }

    #[test]
    fn a_mixer_adds_its_inputs_at_their_gains() {
        let mut first = AudioBuffer::new(2, 4);
        first.channel_mut(0).copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        let mut second = AudioBuffer::new(1, 2);
        second.channel_mut(0).fill(10.0);
        let mut mixer = MixerSource::new(2);
        assert!(mixer.is_empty());
        mixer.add(BufferSource::new(first), 1.0);
        mixer.prepare(48_000.0, 512);
        // Added after preparing: prepared on the way in.
        let index = mixer.add(BufferSource::new(second), 0.5);
        let heard = pull(&mut mixer, 2, &[3], 6);
        assert_eq!(heard[0], [6.0, 7.0, 3.0, 4.0, 0.0, 0.0]);
        assert_eq!(heard[1], [0.0; 6]);
        mixer.set_gain(index, 2.0);
        mixer.set_gain(99, 2.0);
        assert!(mixer.remove(index).is_some());
        assert!(mixer.remove(index).is_none());
        assert_eq!(mixer.len(), 1);
    }

    #[test]
    fn a_resampled_tone_keeps_its_pitch_and_level_whatever_the_blocks() {
        let rate = 44_100.0;
        let input = tone(44_100, rate, 1_000.0);
        let mut results = Vec::new();
        for sizes in [&[256][..], &[1, 7, 300], &[512, 33]] {
            let mut source = ResamplingSource::new(BufferSource::new(input.clone()), rate, 1);
            source.prepare(48_000.0, 512);
            results.push(pull(&mut source, 1, sizes, 24_000).remove(0));
        }
        // Bit for bit the same, whatever the blocks.
        assert_eq!(results[0], results[1]);
        assert_eq!(results[0], results[2]);
        // A 1 kHz sine at 48 kHz, lined up with the input's start: the
        // value at frame n is sin(2 pi 1000 n / 48000).
        let worst = results[0][100..23_000]
            .iter()
            .enumerate()
            .map(|(index, &sample)| {
                let n = (index + 100) as f64;
                (f64::from(sample) - (2.0 * core::f64::consts::PI * 1_000.0 * n / 48_000.0).sin())
                    .abs()
            })
            .fold(0.0, f64::max);
        // Measured: 3.0e-6, the size of f32 rounding.
        assert!(worst < 1e-5, "worst error {worst}");
    }

    #[test]
    fn resampling_down_and_up_and_reset() {
        for (from, to) in [
            (96_000.0, 44_100.0),
            (8_000.0, 48_000.0),
            (48_000.0, 48_000.0),
        ] {
            let input = tone(from as usize, from, 440.0);
            let mut source = ResamplingSource::new(BufferSource::new(input), from, 1);
            source.prepare(to, 128);
            let heard = pull(&mut source, 1, &[128, 5], (to / 4.0) as usize).remove(0);
            let rms = (heard[1_000..]
                .iter()
                .map(|x| f64::from(*x).powi(2))
                .sum::<f64>()
                / (heard.len() - 1_000) as f64)
                .sqrt();
            assert!(
                (rms - 0.5f64.sqrt()).abs() < 2e-3,
                "{from} to {to}: rms {rms}"
            );
            // Seeking the source and resetting starts again from there.
            source.inner_mut().seek(0);
            source.reset();
            let again = pull(&mut source, 1, &[128], 100).remove(0);
            assert_eq!(again[..100], heard[..100]);
        }
        // A rate 1000 times off is held to 256.
        let mut extreme = ResamplingSource::new(BufferSource::new(tone(100, 1.0, 0.1)), 1.0, 1);
        extreme.prepare(48_000.0, 64);
        let _ = pull(&mut extreme, 1, &[64], 128);
    }

    #[test]
    fn channels_are_routed_one_to_many_or_to_none() {
        let mut stereo = AudioBuffer::new(2, 3);
        stereo.channel_mut(0).fill(1.0);
        stereo.channel_mut(1).fill(2.0);
        let mut routed = ChannelRemapSource::new(
            BufferSource::new(stereo),
            2,
            &[Some(1), None, Some(0), Some(7)],
        );
        routed.prepare(48_000.0, 64);
        let heard = pull(&mut routed, 5, &[4], 4);
        assert_eq!(heard[0], [2.0, 2.0, 2.0, 0.0]);
        assert_eq!(heard[1], [0.0; 4]);
        assert_eq!(heard[2], [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(heard[3], [0.0; 4]);
        assert_eq!(heard[4], [0.0; 4]);
        routed.set_route(1, Some(0));
        assert_eq!(routed.route(1), Some(0));
        routed.seek(1);
        assert_eq!(routed.position(), 1);
        let heard = pull(&mut routed, 2, &[4], 4);
        assert_eq!(heard[1], [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn a_source_through_a_processor_and_as_one() {
        let mut ones = AudioBuffer::new(1, 8);
        ones.channel_mut(0).fill(1.0);
        let quiet = ProcessedSource::new(BufferSource::new(ones), Gain::<f32>::new(0.5), 1);
        let mut player = SourceProcessor::new(quiet);
        player.prepare(&ProcessSpec {
            sample_rate: 48_000.0,
            max_block_size: 8,
            num_channels: 1,
        });
        let mut block = AudioBuffer::<f32>::new(1, 8);
        block.channel_mut(0).fill(3.0);
        player.process(&mut block);
        assert_eq!(block.channel(0), &[0.5; 8]);
        player.reset();
    }
}
