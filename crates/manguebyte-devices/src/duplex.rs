use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use manguebyte_core::AudioBuffer;
use manguebyte_core::spsc::{self, Consumer, Producer};

use crate::blocks::interleave;
use crate::device::with_sample_type;
use crate::{DeviceError, InputDevice, OutputDevice};

/// How a [`DuplexStream`] should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplexSettings {
    /// Largest block the callback will be handed, in frames.
    pub max_block_size: usize,
    /// How much captured audio to keep in reserve between the two devices, in
    /// frames. This is added latency: lower is tighter, higher survives more
    /// scheduling jitter. The stream raises it by itself if it proves too low.
    pub latency_frames: usize,
}

impl DuplexSettings {
    /// Settings with a reserve of one block.
    pub fn new(max_block_size: usize) -> Self {
        Self {
            max_block_size,
            latency_frames: max_block_size,
        }
    }

    /// Sets the reserve of captured audio, in frames.
    #[must_use]
    pub fn with_latency_frames(mut self, frames: usize) -> Self {
        self.latency_frames = frames;
        self
    }
}

#[derive(Default)]
struct Stats {
    underruns: AtomicUsize,
    overruns: AtomicUsize,
}

/// An input and an output device running together, with captured audio handed
/// to the same callback that produces the output.
///
/// The two devices may be different pieces of hardware. Each has its own
/// clock, so over time one runs slightly ahead of the other; the stream
/// absorbs that by occasionally dropping or inserting a block of input, and
/// counts each time it does in [`overruns`](Self::overruns) and
/// [`underruns`](Self::underruns).
///
/// ```no_run
/// use manguebyte_devices::{DuplexStream, InputDevice, OutputDevice};
///
/// let input = InputDevice::open_default()?;
/// let output = OutputDevice::open_default()?;
///
/// // Monitor the first input channel on every output channel.
/// let _stream = DuplexStream::start(&input, &output, 256, |input, output| {
///     if input.num_channels() == 0 {
///         return;
///     }
///     for channel in output.iter_channels_mut() {
///         channel.copy_from_slice(input.channel(0));
///     }
/// })?;
/// std::thread::sleep(std::time::Duration::from_secs(5));
/// # Ok::<(), manguebyte_devices::DeviceError>(())
/// ```
pub struct DuplexStream {
    input: cpal::Stream,
    output: cpal::Stream,
    stats: Arc<Stats>,
}

impl DuplexStream {
    /// Starts both devices, reporting stream errors on standard error.
    ///
    /// See [`start_with`](Self::start_with).
    pub fn start<F>(
        input: &InputDevice,
        output: &OutputDevice,
        max_block_size: usize,
        callback: F,
    ) -> Result<Self, DeviceError>
    where
        F: FnMut(&AudioBuffer<f32>, &mut AudioBuffer<f32>) + Send + 'static,
    {
        Self::start_with(
            input,
            output,
            DuplexSettings::new(max_block_size),
            callback,
            |error| eprintln!("manguebyte: audio stream error: {error}"),
        )
    }

    /// Starts both devices.
    ///
    /// `callback` runs on the output device's audio thread. It receives the
    /// captured audio and a silent output buffer of the same length, at most
    /// `settings.max_block_size` frames, and must not allocate, lock or
    /// block. The two buffers have the channel counts of their devices, which
    /// may differ.
    ///
    /// `on_error` is told about problems on either device.
    ///
    /// Both devices must run at the same sample rate; use
    /// [`with_settings`](InputDevice::with_settings) to make them agree.
    ///
    /// # Panics
    ///
    /// Panics if `settings.max_block_size` is zero.
    pub fn start_with<F, E>(
        input: &InputDevice,
        output: &OutputDevice,
        settings: DuplexSettings,
        callback: F,
        on_error: E,
    ) -> Result<Self, DeviceError>
    where
        F: FnMut(&AudioBuffer<f32>, &mut AudioBuffer<f32>) + Send + 'static,
        E: FnMut(DeviceError) + Send + 'static,
    {
        assert!(
            settings.max_block_size > 0,
            "max_block_size must be non-zero"
        );
        let sample_rate = output.opened.sample_rate();
        if input.opened.sample_rate() != sample_rate {
            return Err(DeviceError::SampleRateMismatch {
                input: input.opened.sample_rate(),
                output: sample_rate,
            });
        }

        let stats = Arc::new(Stats::default());
        let (feeder, renderer) = bridge(
            input.num_channels(),
            output.num_channels(),
            sample_rate as usize,
            settings,
            callback,
            Arc::clone(&stats),
        );

        // Errors can arrive from either device's thread, so the handler is
        // shared. This lock is never taken on the path that moves audio.
        let on_error = Arc::new(Mutex::new(on_error));
        let report = move |error: cpal::Error| {
            if let Ok(mut handler) = on_error.lock() {
                handler(DeviceError::Backend(error));
            }
        };

        macro_rules! build_input {
            ($sample:ty) => {
                build_input::<$sample>(input, feeder, report.clone())
            };
        }
        let input_stream = with_sample_type!(input.opened.resolved.format, build_input)?;

        macro_rules! build_output {
            ($sample:ty) => {
                build_output::<$sample, _>(output, renderer, report)
            };
        }
        let output_stream = with_sample_type!(output.opened.resolved.format, build_output)?;

        input_stream.play()?;
        output_stream.play()?;
        Ok(Self {
            input: input_stream,
            output: output_stream,
            stats,
        })
    }

    /// Pauses both devices.
    pub fn pause(&self) -> Result<(), DeviceError> {
        self.output.pause()?;
        Ok(self.input.pause()?)
    }

    /// Resumes both devices after [`pause`](Self::pause).
    pub fn resume(&self) -> Result<(), DeviceError> {
        self.input.play()?;
        Ok(self.output.play()?)
    }

    /// How many times the output needed captured audio that had not arrived
    /// yet and played silence for the missing part.
    pub fn underruns(&self) -> usize {
        self.stats.underruns.load(Ordering::Relaxed)
    }

    /// How many times captured audio was thrown away because the output was
    /// not keeping up with the input.
    pub fn overruns(&self) -> usize {
        self.stats.overruns.load(Ordering::Relaxed)
    }
}

fn build_input<T>(
    device: &InputDevice,
    mut feeder: Feeder,
    report: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, DeviceError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    Ok(device.opened.device.build_input_stream(
        device.opened.config(),
        move |data: &[T], _: &cpal::InputCallbackInfo| feeder.feed(data),
        report,
        None,
    )?)
}

fn build_output<T, F>(
    device: &OutputDevice,
    mut renderer: Renderer<F>,
    report: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, DeviceError>
where
    T: SizedSample + FromSample<f32>,
    F: FnMut(&AudioBuffer<f32>, &mut AudioBuffer<f32>) + Send + 'static,
{
    Ok(device.opened.device.build_output_stream(
        device.opened.config(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| renderer.render(data),
        report,
        None,
    )?)
}

/// Builds the two halves that carry audio from the input device's thread to
/// the output device's thread.
fn bridge<F>(
    input_channels: usize,
    output_channels: usize,
    sample_rate: usize,
    settings: DuplexSettings,
    callback: F,
    stats: Arc<Stats>,
) -> (Feeder, Renderer<F>) {
    let block = settings.max_block_size;
    // One second of audio, and never less than a comfortable multiple of what
    // was asked for, so the reserve has room to grow.
    let capacity_frames = sample_rate.max(8 * (block + settings.latency_frames));
    let (producer, consumer) = spsc::channel(capacity_frames * input_channels.max(1));
    let capacity_frames = producer.capacity() / input_channels.max(1);

    let feeder = Feeder {
        producer,
        scratch: vec![0.0; block * input_channels],
        channels: input_channels,
        stats: Arc::clone(&stats),
    };
    let renderer = Renderer {
        consumer,
        interleaved: vec![0.0; block * input_channels],
        input: AudioBuffer::new(input_channels, block),
        output: AudioBuffer::new(output_channels, block),
        callback,
        primed: false,
        reserve: settings.latency_frames,
        max_reserve: capacity_frames / 4,
        high_water: capacity_frames / 2,
        stats,
    };
    (feeder, renderer)
}

/// Runs on the input device's thread: converts captured audio to `f32` and
/// queues it for the output side.
struct Feeder {
    producer: Producer<f32>,
    scratch: Vec<f32>,
    channels: usize,
    stats: Arc<Stats>,
}

impl Feeder {
    fn feed<T: Sample>(&mut self, data: &[T])
    where
        f32: FromSample<T>,
    {
        if self.channels == 0 {
            return;
        }
        for chunk in data.chunks(self.scratch.len()) {
            // Only whole frames go in, so the channels can never slip out of
            // step with each other.
            let frames =
                (chunk.len() / self.channels).min(self.producer.free_len() / self.channels);
            let samples = frames * self.channels;
            for (slot, &value) in self.scratch[..samples].iter_mut().zip(chunk) {
                *slot = f32::from_sample(value);
            }
            self.producer.push_slice(&self.scratch[..samples]);
            if samples < chunk.len() {
                self.stats.overruns.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Runs on the output device's thread: pairs queued input with the output
/// buffer and calls the user's callback.
struct Renderer<F> {
    consumer: Consumer<f32>,
    interleaved: Vec<f32>,
    input: AudioBuffer<f32>,
    output: AudioBuffer<f32>,
    callback: F,
    /// Whether the reserve has been built up and input is flowing.
    primed: bool,
    /// Frames of input to hold back before starting to consume.
    reserve: usize,
    max_reserve: usize,
    /// Queue fill, in frames, beyond which the excess is discarded.
    high_water: usize,
    stats: Arc<Stats>,
}

impl<F: FnMut(&AudioBuffer<f32>, &mut AudioBuffer<f32>)> Renderer<F> {
    fn render<T: Sample + FromSample<f32>>(&mut self, data: &mut [T]) {
        let channels = self.output.num_channels();
        if channels == 0 {
            return;
        }
        for chunk in data.chunks_mut(self.output.capacity() * channels) {
            let frames = chunk.len() / channels;
            self.input.set_num_frames(frames);
            self.output.set_num_frames(frames);
            self.output.clear();
            self.pull_input(frames);
            (self.callback)(&self.input, &mut self.output);
            interleave(&self.output, chunk);
        }
    }

    fn pull_input(&mut self, frames: usize) {
        let channels = self.input.num_channels();
        if channels == 0 {
            return;
        }
        let mut available = self.consumer.len() / channels;

        // The input device is running faster than the output, or the output
        // stalled. Drop the backlog rather than let latency pile up.
        if available > self.high_water {
            self.consumer.skip((available - self.reserve) * channels);
            available = self.reserve;
            self.stats.overruns.fetch_add(1, Ordering::Relaxed);
        }

        if !self.primed && available >= self.reserve.max(frames) {
            self.primed = true;
        }

        let take = if self.primed {
            frames.min(available)
        } else {
            0
        };
        let samples = &mut self.interleaved[..frames * channels];
        let filled = self.consumer.pop_slice(&mut samples[..take * channels]);
        samples[filled..].fill(0.0);
        self.input.copy_from_interleaved(samples);

        if self.primed && take < frames {
            // Ran dry: the reserve was too small for how these two devices
            // deliver audio. Rebuild it a little larger.
            self.primed = false;
            self.reserve = (self.reserve + frames).min(self.max_reserve);
            self.stats.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Echo = fn(&AudioBuffer<f32>, &mut AudioBuffer<f32>);

    fn echo(input: &AudioBuffer<f32>, output: &mut AudioBuffer<f32>) {
        for (index, channel) in output.iter_channels_mut().enumerate() {
            channel.copy_from_slice(input.channel(index % input.num_channels()));
        }
    }

    fn make(
        input_channels: usize,
        block: usize,
        latency: usize,
    ) -> (Feeder, Renderer<Echo>, Arc<Stats>) {
        let stats = Arc::new(Stats::default());
        let settings = DuplexSettings::new(block).with_latency_frames(latency);
        let (feeder, renderer) = bridge(
            input_channels,
            2,
            1_000,
            settings,
            echo as Echo,
            Arc::clone(&stats),
        );
        (feeder, renderer, stats)
    }

    /// A stereo ramp where the right channel is the negated left one.
    fn ramp(start: usize, frames: usize) -> Vec<f32> {
        (start..start + frames)
            .flat_map(|frame| {
                let value = (frame + 1) as f32;
                [value, -value]
            })
            .collect()
    }

    #[test]
    fn plays_silence_until_the_reserve_is_built() {
        let (mut feeder, mut renderer, stats) = make(2, 4, 8);
        let mut out = [9.0f32; 8];

        renderer.render(&mut out);
        assert_eq!(out, [0.0; 8]);

        feeder.feed(&ramp(0, 6));
        renderer.render(&mut out);
        assert_eq!(
            out, [0.0; 8],
            "six frames are fewer than the reserve of eight"
        );

        feeder.feed(&ramp(6, 2));
        renderer.render(&mut out);
        assert_eq!(out, ramp(0, 4).as_slice());
        renderer.render(&mut out);
        assert_eq!(out, ramp(4, 4).as_slice());
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_underrun_pads_with_silence_and_grows_the_reserve() {
        let (mut feeder, mut renderer, stats) = make(2, 4, 4);
        feeder.feed(&ramp(0, 6));

        let mut out = [9.0f32; 8];
        renderer.render(&mut out);
        assert_eq!(out, ramp(0, 4).as_slice());

        renderer.render(&mut out);
        assert_eq!(&out[..4], ramp(4, 2).as_slice());
        assert_eq!(&out[4..], &[0.0; 4]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(renderer.reserve, 8);

        // Input is held back again until the larger reserve is there.
        feeder.feed(&ramp(6, 4));
        renderer.render(&mut out);
        assert_eq!(out, [0.0; 8]);
        feeder.feed(&ramp(10, 4));
        renderer.render(&mut out);
        assert_eq!(out, ramp(6, 4).as_slice());
    }

    #[test]
    fn a_backlog_is_trimmed_to_the_reserve() {
        let (mut feeder, mut renderer, stats) = make(2, 4, 4);
        let capacity = feeder.producer.capacity() / 2;
        let backlog = renderer.high_water + 10;
        assert!(backlog < capacity);
        for start in (0..backlog).step_by(2) {
            feeder.feed(&ramp(start, 2));
        }

        let mut out = [0.0f32; 8];
        renderer.render(&mut out);
        assert_eq!(stats.overruns.load(Ordering::Relaxed), 1);
        // The newest `reserve` frames survive, so playback resumes there.
        assert_eq!(out, ramp(backlog - 4, 4).as_slice());
    }

    #[test]
    fn a_full_queue_drops_whole_frames_only() {
        let (mut feeder, mut renderer, stats) = make(2, 4, 4);
        let capacity = feeder.producer.capacity() / 2;
        for start in (0..capacity + 8).step_by(4) {
            feeder.feed(&ramp(start, 4));
        }
        assert!(stats.overruns.load(Ordering::Relaxed) >= 1);
        assert_eq!(renderer.consumer.len() % 2, 0);

        let mut out = [0.0f32; 8];
        renderer.render(&mut out);
        for frame in out.chunks_exact(2) {
            assert_eq!(frame[0], -frame[1], "channels slipped out of step");
        }
    }

    #[test]
    fn mono_input_can_feed_stereo_output() {
        let (mut feeder, mut renderer, _) = make(1, 4, 2);
        feeder.feed(&[0.1f32, 0.2, 0.3, 0.4]);
        let mut out = [0i16; 8];
        renderer.render(&mut out);
        assert_eq!(out[0], out[1]);
        assert!(out[6] > out[0]);
    }
}
