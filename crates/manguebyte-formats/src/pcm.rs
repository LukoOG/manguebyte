//! Blocks of uncompressed samples, as WAV and AIFF both store them.
//!
//! The two formats differ in their headers and in how one sample is coded.
//! Everything else is the same: frames of fixed size, one after another.
//! This module is that common part: reading a stretch of frames from any
//! position, and writing frames out, through a scratch buffer of fixed size
//! so that memory does not grow with the length of the file.

use std::io::{self, Read, Seek, SeekFrom, Write};

use manguebyte_core::{AudioBuffer, vector};

/// How many bytes of samples are moved at a time. Large enough that an
/// unbuffered file is read with few system calls, small enough to stay in
/// the processor's cache.
const CHUNK_BYTES: usize = 32 * 1024;

/// How many samples are decoded or encoded at a time between the bytes and
/// the channels, in a buffer on the stack. Frames of more channels than this
/// are moved one sample at a time.
const RUN_SAMPLES: usize = 1024;

/// Turns the bytes of one stored sample into an `f32`.
pub(crate) trait SampleDecoder: Copy {
    /// Decodes one sample. The length of `bytes` is the stream's sample
    /// width.
    fn decode(self, bytes: &[u8]) -> f32;

    /// Decodes `out.len()` samples of `width` bytes each, stored one after
    /// another in `bytes`. A format whose samples a vector of the
    /// processor can decode overrides this.
    fn decode_run(self, width: usize, bytes: &[u8], out: &mut [f32]) {
        for (value, sample) in out.iter_mut().zip(bytes.chunks_exact(width)) {
            *value = self.decode(sample);
        }
    }
}

/// Turns an `f32` into the bytes of one stored sample.
pub(crate) trait SampleEncoder: Copy {
    /// Encodes one sample into `bytes`, whose length is the format's sample
    /// width.
    fn encode(self, sample: f32, bytes: &mut [u8]);

    /// Encodes `samples` one after another into `bytes`, `width` bytes
    /// each. A format whose samples a vector of the processor can encode
    /// overrides this.
    fn encode_run(self, width: usize, samples: &[f32], bytes: &mut [u8]) {
        for (&value, sample) in samples.iter().zip(bytes.chunks_exact_mut(width)) {
            self.encode(value, sample);
        }
    }
}

/// Where the samples of a stream are and how they are laid out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PcmLayout {
    /// Channels in each frame. Never zero.
    pub channels: usize,
    /// Bytes each sample takes. Never zero.
    pub bytes_per_sample: usize,
    /// Offset of the first sample from the start of the stream.
    pub data_start: u64,
    /// Whole frames the stream holds.
    pub frames: u64,
}

/// The length of a scratch buffer for frames of `frame_bytes` bytes: a whole
/// number of frames, at least one, and no more than `frames` of them.
pub(crate) fn scratch_len(frame_bytes: usize, frames: u64) -> usize {
    let wanted = (CHUNK_BYTES / frame_bytes.max(1)).max(1);
    let useful = usize::try_from(frames).unwrap_or(usize::MAX).max(1);
    wanted.min(useful) * frame_bytes
}

/// Reads frames of uncompressed samples from any position of a stream.
pub(crate) struct PcmReader<R, D> {
    reader: R,
    decoder: D,
    layout: PcmLayout,
    frame_bytes: usize,
    /// The frame the next read starts at.
    position: u64,
    /// Whether `reader` is positioned at `position`. A seek only clears
    /// this; the stream is moved when the next read needs it.
    in_place: bool,
    scratch: Vec<u8>,
}

impl<R: Read + Seek, D: SampleDecoder> PcmReader<R, D> {
    /// Wraps `reader`, whose samples are described by `layout`.
    pub(crate) fn new(reader: R, decoder: D, layout: PcmLayout) -> Self {
        let frame_bytes = layout.channels * layout.bytes_per_sample;
        Self {
            reader,
            decoder,
            layout,
            frame_bytes,
            position: 0,
            in_place: false,
            scratch: vec![0; scratch_len(frame_bytes, layout.frames)],
        }
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.layout.channels
    }

    pub(crate) fn frames(&self) -> u64 {
        self.layout.frames
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn into_inner(self) -> R {
        self.reader
    }

    /// Moves to `frame`, or to the end if the stream is shorter, and returns
    /// where it landed. Costs nothing until the next read.
    pub(crate) fn seek(&mut self, frame: u64) -> u64 {
        let frame = frame.min(self.layout.frames);
        if frame != self.position {
            self.position = frame;
            self.in_place = false;
        }
        frame
    }

    /// Fills `out` from the current position and returns how many frames
    /// were read: fewer than `out` holds only at the end of the stream.
    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> io::Result<usize> {
        let left = self.layout.frames - self.position;
        let want = usize::try_from(left)
            .unwrap_or(usize::MAX)
            .min(out.num_frames());
        if want == 0 {
            return Ok(0);
        }
        if !self.in_place {
            let offset = self.layout.data_start + self.position * self.frame_bytes as u64;
            self.reader.seek(SeekFrom::Start(offset))?;
            self.in_place = true;
        }

        let width = self.layout.bytes_per_sample;
        let chunk_frames = self.scratch.len() / self.frame_bytes;
        let mut done = 0;
        while done < want {
            let count = (want - done).min(chunk_frames);
            let bytes = &mut self.scratch[..count * self.frame_bytes];
            if let Err(error) = self.reader.read_exact(bytes) {
                // Where a failed read leaves the stream is not defined.
                self.in_place = false;
                return Err(error);
            }
            decode_frames(
                self.decoder,
                self.layout.channels,
                width,
                bytes,
                out,
                done..done + count,
            );
            done += count;
            self.position += count as u64;
        }
        Ok(done)
    }
}

/// Encodes every frame of `buffer` and writes the bytes to `writer`.
///
/// `scratch` holds a whole number of frames and bounds how much is encoded
/// before each write.
pub(crate) fn write_planar<W: Write, E: SampleEncoder>(
    writer: &mut W,
    encoder: E,
    width: usize,
    buffer: &AudioBuffer<f32>,
    scratch: &mut [u8],
) -> io::Result<()> {
    let channels = buffer.num_channels();
    let frame_bytes = channels * width;
    if frame_bytes == 0 {
        return Ok(());
    }
    let chunk_frames = scratch.len() / frame_bytes;
    let mut done = 0;
    while done < buffer.num_frames() {
        let count = (buffer.num_frames() - done).min(chunk_frames);
        let bytes = &mut scratch[..count * frame_bytes];
        encode_frames(encoder, width, buffer, done..done + count, bytes);
        writer.write_all(bytes)?;
        done += count;
    }
    Ok(())
}

/// Encodes interleaved samples and writes the bytes to `writer`.
pub(crate) fn write_interleaved<W: Write, E: SampleEncoder>(
    writer: &mut W,
    encoder: E,
    width: usize,
    samples: &[f32],
    scratch: &mut [u8],
) -> io::Result<()> {
    let chunk_samples = scratch.len() / width;
    for chunk in samples.chunks(chunk_samples.max(1)) {
        let bytes = &mut scratch[..chunk.len() * width];
        encoder.encode_run(width, chunk, bytes);
        writer.write_all(bytes)?;
    }
    Ok(())
}

/// Decodes the frames of `channels` channels held in `bytes` into `frames`
/// of the first `channels` channels of `out`: a mono stream straight into
/// its channel, others through a buffer of interleaved samples on the
/// stack, split into the channels after.
fn decode_frames<D: SampleDecoder>(
    decoder: D,
    channels: usize,
    width: usize,
    bytes: &[u8],
    out: &mut AudioBuffer<f32>,
    frames: core::ops::Range<usize>,
) {
    let frame_bytes = channels * width;
    if channels == 1 {
        decoder.decode_run(width, bytes, &mut out.channel_mut(0)[frames]);
        return;
    }
    if channels > RUN_SAMPLES {
        for channel in 0..channels {
            let destination = &mut out.channel_mut(channel)[frames.clone()];
            let samples = bytes[channel * width..].chunks(frame_bytes);
            for (value, sample) in destination.iter_mut().zip(samples) {
                *value = decoder.decode(&sample[..width]);
            }
        }
        return;
    }
    let mut run = [0.0f32; RUN_SAMPLES];
    let per_run = RUN_SAMPLES / channels;
    let mut start = frames.start;
    for piece in bytes.chunks(per_run * frame_bytes) {
        let count = piece.len() / frame_bytes;
        let samples = &mut run[..count * channels];
        decoder.decode_run(width, piece, samples);
        if channels == 2 {
            let (left, right) = out.channel_pair_mut(0, 1);
            vector::deinterleave_stereo(
                &mut left[start..start + count],
                &mut right[start..start + count],
                samples,
            );
        } else {
            for channel in 0..channels {
                let destination = &mut out.channel_mut(channel)[start..start + count];
                for (value, &sample) in destination
                    .iter_mut()
                    .zip(samples[channel..].iter().step_by(channels))
                {
                    *value = sample;
                }
            }
        }
        start += count;
    }
}

/// Encodes `frames` of every channel of `buffer` into `bytes`, as
/// interleaved frames: the counterpart of `decode_frames`.
fn encode_frames<E: SampleEncoder>(
    encoder: E,
    width: usize,
    buffer: &AudioBuffer<f32>,
    frames: core::ops::Range<usize>,
    bytes: &mut [u8],
) {
    let channels = buffer.num_channels();
    let frame_bytes = channels * width;
    if channels == 1 {
        encoder.encode_run(width, &buffer.channel(0)[frames], bytes);
        return;
    }
    if channels > RUN_SAMPLES {
        for channel in 0..channels {
            let source = &buffer.channel(channel)[frames.clone()];
            let samples = bytes[channel * width..].chunks_mut(frame_bytes);
            for (&value, sample) in source.iter().zip(samples) {
                encoder.encode(value, &mut sample[..width]);
            }
        }
        return;
    }
    let mut run = [0.0f32; RUN_SAMPLES];
    let per_run = RUN_SAMPLES / channels;
    let mut start = frames.start;
    for piece in bytes.chunks_mut(per_run * frame_bytes) {
        let count = piece.len() / frame_bytes;
        let samples = &mut run[..count * channels];
        if channels == 2 {
            vector::interleave_stereo(
                samples,
                &buffer.channel(0)[start..start + count],
                &buffer.channel(1)[start..start + count],
            );
        } else {
            for channel in 0..channels {
                let source = &buffer.channel(channel)[start..start + count];
                for (slot, &value) in samples[channel..].iter_mut().step_by(channels).zip(source) {
                    *slot = value;
                }
            }
        }
        encoder.encode_run(width, samples, piece);
        start += count;
    }
}

/// Reads into `bytes` until it is full or the stream ends, and returns how
/// many bytes were read.
pub(crate) fn read_up_to<R: Read>(reader: &mut R, bytes: &mut [u8]) -> io::Result<usize> {
    let mut have = 0;
    while have < bytes.len() {
        match reader.read(&mut bytes[have..]) {
            Ok(0) => break,
            Ok(count) => have += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(have)
}
