//! Adapters between the interleaved, device-format slices a backend hands out
//! and the planar `f32` blocks the rest of Manguebyte works with.
//!
//! Everything here runs on the audio thread and allocates only in `new`.

use cpal::{FromSample, Sample};
use manguebyte_core::{AudioBuffer, NoDenormals};

/// Feeds an output device from a planar `f32` callback, splitting oversized
/// requests into blocks.
pub(crate) struct BlockRenderer<F> {
    scratch: AudioBuffer<f32>,
    callback: F,
}

impl<F: FnMut(&mut AudioBuffer<f32>)> BlockRenderer<F> {
    pub(crate) fn new(channels: usize, max_block_size: usize, callback: F) -> Self {
        Self {
            scratch: AudioBuffer::new(channels, max_block_size),
            callback,
        }
    }

    pub(crate) fn render<T: Sample + FromSample<f32>>(&mut self, data: &mut [T]) {
        // A fading sound must not slow the audio thread down.
        let _quiet = NoDenormals::new();
        let channels = self.scratch.num_channels();
        if channels == 0 {
            return;
        }
        for chunk in data.chunks_mut(self.scratch.capacity() * channels) {
            self.scratch.set_num_frames(chunk.len() / channels);
            self.scratch.clear();
            (self.callback)(&mut self.scratch);
            interleave(&self.scratch, chunk);
        }
    }
}

/// Hands an input device's audio to a planar `f32` callback, splitting
/// oversized deliveries into blocks.
pub(crate) struct BlockCapturer<F> {
    scratch: AudioBuffer<f32>,
    callback: F,
}

impl<F: FnMut(&AudioBuffer<f32>)> BlockCapturer<F> {
    pub(crate) fn new(channels: usize, max_block_size: usize, callback: F) -> Self {
        Self {
            scratch: AudioBuffer::new(channels, max_block_size),
            callback,
        }
    }

    pub(crate) fn capture<T: Sample>(&mut self, data: &[T])
    where
        f32: FromSample<T>,
    {
        let _quiet = NoDenormals::new();
        let channels = self.scratch.num_channels();
        if channels == 0 {
            return;
        }
        for chunk in data.chunks(self.scratch.capacity() * channels) {
            self.scratch.set_num_frames(chunk.len() / channels);
            deinterleave(chunk, &mut self.scratch);
            (self.callback)(&self.scratch);
        }
    }
}

/// Writes `buffer` into `out` as interleaved frames in the device format.
pub(crate) fn interleave<T: Sample + FromSample<f32>>(buffer: &AudioBuffer<f32>, out: &mut [T]) {
    let channels = buffer.num_channels();
    for (index, channel) in buffer.iter_channels().enumerate() {
        for (slot, &sample) in out[index..].iter_mut().step_by(channels).zip(channel) {
            *slot = T::from_sample(sample);
        }
    }
}

/// Reads interleaved device-format frames from `data` into `buffer`.
pub(crate) fn deinterleave<T: Sample>(data: &[T], buffer: &mut AudioBuffer<f32>)
where
    f32: FromSample<T>,
{
    let channels = buffer.num_channels();
    for (index, channel) in buffer.iter_channels_mut().enumerate() {
        for (sample, &value) in channel
            .iter_mut()
            .zip(data[index..].iter().step_by(channels))
        {
            *sample = f32::from_sample(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_interleaves_and_splits_into_blocks() {
        let mut block_sizes = Vec::new();
        let mut renderer = BlockRenderer::new(2, 4, |buffer: &mut AudioBuffer<f32>| {
            block_sizes.push(buffer.num_frames());
            buffer.channel_mut(0).fill(0.25);
            buffer.channel_mut(1).fill(-0.5);
        });

        let mut data = [9.0f32; 20];
        renderer.render(&mut data);
        drop(renderer);

        assert_eq!(block_sizes, vec![4, 4, 2]);
        for frame in data.chunks_exact(2) {
            assert_eq!(frame, &[0.25, -0.5]);
        }
    }

    #[test]
    fn renderer_hands_the_callback_a_silent_buffer_every_time() {
        let mut renderer = BlockRenderer::new(1, 8, |buffer: &mut AudioBuffer<f32>| {
            assert_eq!(buffer.peak(), 0.0);
            buffer.channel_mut(0).fill(1.0);
        });
        let mut data = [0.0f32; 8];
        renderer.render(&mut data);
        renderer.render(&mut data);
        assert_eq!(data, [1.0; 8]);
    }

    #[test]
    fn renderer_converts_to_the_device_sample_format() {
        let mut renderer = BlockRenderer::new(1, 4, |buffer: &mut AudioBuffer<f32>| {
            buffer
                .channel_mut(0)
                .copy_from_slice(&[0.0, 0.5, -1.0, 1.0]);
        });

        let mut signed = [0i16; 4];
        renderer.render(&mut signed);
        assert_eq!(signed[0], 0);
        assert!((signed[1] - 16_384).abs() <= 1);
        assert_eq!(signed[2], i16::MIN);
        assert_eq!(signed[3], i16::MAX);

        let mut unsigned = [0u16; 4];
        renderer.render(&mut unsigned);
        assert_eq!(unsigned[0], 32_768);
        assert_eq!(unsigned[2], 0);
    }

    #[test]
    fn capturer_deinterleaves_converts_and_splits() {
        let mut blocks: Vec<(Vec<f32>, Vec<f32>)> = Vec::new();
        let mut capturer = BlockCapturer::new(2, 2, |buffer: &AudioBuffer<f32>| {
            blocks.push((buffer.channel(0).to_vec(), buffer.channel(1).to_vec()));
        });

        let data: [i16; 6] = [0, i16::MIN, 16_384, -16_384, i16::MAX, 0];
        capturer.capture(&data);
        drop(capturer);

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, vec![0.0, 0.5]);
        assert_eq!(blocks[0].1, vec![-1.0, -0.5]);
        assert_eq!(blocks[1].0.len(), 1);
        assert!((blocks[1].0[0] - 1.0).abs() < 1e-4);
        assert_eq!(blocks[1].1, vec![0.0]);
    }
}
