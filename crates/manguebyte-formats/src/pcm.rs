//! Blocks of uncompressed samples, as WAV and AIFF both store them.
//!
//! The two formats differ in their headers and in how one sample is coded.
//! Everything else is the same: frames of fixed size, one after another.
//! This module is that common part: reading a stretch of frames from any
//! position, and writing frames out, through a scratch buffer of fixed size
//! so that memory does not grow with the length of the file.

use std::io::{self, Read, Seek, SeekFrom, Write};

use manguebyte_core::AudioBuffer;

/// How many bytes of samples are moved at a time. Large enough that an
/// unbuffered file is read with few system calls, small enough to stay in
/// the processor's cache.
const CHUNK_BYTES: usize = 32 * 1024;

/// Turns the bytes of one stored sample into an `f32`.
pub(crate) trait SampleDecoder: Copy {
    /// Decodes one sample. The length of `bytes` is the stream's sample
    /// width.
    fn decode(self, bytes: &[u8]) -> f32;
}

/// Turns an `f32` into the bytes of one stored sample.
pub(crate) trait SampleEncoder: Copy {
    /// Encodes one sample into `bytes`, whose length is the format's sample
    /// width.
    fn encode(self, sample: f32, bytes: &mut [u8]);
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
            for channel in 0..self.layout.channels {
                let destination = &mut out.channel_mut(channel)[done..done + count];
                let samples = bytes[channel * width..].chunks(self.frame_bytes);
                for (value, sample) in destination.iter_mut().zip(samples) {
                    *value = self.decoder.decode(&sample[..width]);
                }
            }
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
        for channel in 0..channels {
            let source = &buffer.channel(channel)[done..done + count];
            let samples = bytes[channel * width..].chunks_mut(frame_bytes);
            for (&value, sample) in source.iter().zip(samples) {
                encoder.encode(value, &mut sample[..width]);
            }
        }
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
        for (&value, sample) in chunk.iter().zip(bytes.chunks_mut(width)) {
            encoder.encode(value, sample);
        }
        writer.write_all(bytes)?;
    }
    Ok(())
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
