//! An Opus decoder of one stream (RFC 6716, section 4) and of the several
//! streams of a multistream packet (RFC 7845, section 5.1.1): which layer
//! decodes each frame, how SILK and CELT add up in a hybrid frame, and how
//! a change of mode is smoothed over with the redundant CELT frames the
//! encoder puts in for it (section 4.5).
//!
//! Output is at 48 kHz. A frame lost, or one that a change of mode asks to
//! conceal, is filled with silence: packet loss concealment, which RFC
//! 6716 leaves to the decoder, is not done here.

use super::OpusError;
use super::celt::{CeltDecoder, CeltError, CeltScratch, MAX_FRAME};
use super::packet::{self, Bandwidth, Mode, Packet};
use super::range::RangeDecoder;
use super::silk::{SilkDecoder, SilkScratch};

/// The longest packet in samples: 120 ms at 48 kHz.
pub(crate) const MAX_PACKET_SAMPLES: usize = 5_760;
/// 2.5 ms at 48 kHz.
const F2_5: usize = 120;
const F5: usize = 240;

impl From<CeltError> for OpusError {
    fn from(error: CeltError) -> Self {
        match error {
            CeltError::FrameSize => Self::Malformed("CELT frame of an invalid size"),
            CeltError::Overrun => Self::Malformed("CELT frame reads past its end"),
        }
    }
}

/// Decodes the packets of one Opus stream of one or two channels
/// (`OpusDecoder`). Like the layers it holds, it holds only the state that
/// runs from packet to packet: a copy of it resumes the decoding where it
/// was taken.
#[derive(Clone)]
pub(crate) struct StreamDecoder {
    channels: usize,
    celt: CeltDecoder,
    silk: SilkDecoder,
    prev_mode: Option<Mode>,
    prev_redundancy: bool,
    /// What the last packet's table of contents said.
    mode: Mode,
    bandwidth: Bandwidth,
    frame_size: usize,
    stream_channels: usize,
}

/// The work space of the decoders of a stream, shared by all the streams
/// of a multistream packet, which are decoded one after the other.
pub(crate) struct Scratch {
    celt: CeltScratch,
    silk: SilkScratch,
    /// One frame of SILK output, and the 5 ms of a redundant CELT frame,
    /// per channel.
    pcm_silk: Vec<i16>,
    redundant: Vec<f32>,
}

impl Scratch {
    pub(crate) fn new() -> Self {
        Self {
            celt: CeltScratch::new(),
            silk: SilkScratch::new(),
            pcm_silk: vec![0; 2 * 2_880],
            redundant: vec![0.0; 2 * F5],
        }
    }
}

impl StreamDecoder {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            channels,
            celt: CeltDecoder::new(channels),
            silk: SilkDecoder::new(channels),
            prev_mode: None,
            prev_redundancy: false,
            mode: Mode::Celt,
            bandwidth: Bandwidth::Full,
            frame_size: F2_5,
            stream_channels: channels,
        }
    }

    /// Forgets everything, as at the start of a stream (`OPUS_RESET_STATE`).
    pub(crate) fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.prev_mode = None;
        self.prev_redundancy = false;
        self.stream_channels = self.channels;
        self.frame_size = F2_5;
    }

    /// Decodes a packet into `out`, which holds `stride` samples a channel,
    /// one channel after the other (`opus_decode_native`). Returns the
    /// samples decoded per channel and the bytes the packet took.
    pub(crate) fn decode(
        &mut self,
        scratch: &mut Scratch,
        data: &[u8],
        self_delimited: bool,
        out: &mut [f32],
        stride: usize,
    ) -> Result<(usize, usize), OpusError> {
        let packet = packet::parse(data, self_delimited).map_err(OpusError::Malformed)?;
        let samples = packet.samples();
        if samples > stride {
            return Err(OpusError::Malformed("packet longer than 120 ms"));
        }
        self.mode = packet.toc.mode;
        self.bandwidth = packet.toc.bandwidth;
        self.frame_size = packet.toc.frame_size;
        self.stream_channels = if packet.toc.stereo { 2 } else { 1 };
        let mut done = 0;
        for &(at, len) in &packet.frames[..packet.count] {
            let frame = &data[at..at + len];
            let decoded = self.decode_frame(scratch, frame, out, stride, done)?;
            done += decoded;
        }
        Ok((done, packet_len(&packet)))
    }

    /// Decodes one frame of the packet into `out` from sample `at`
    /// (`opus_decode_frame`).
    fn decode_frame(
        &mut self,
        scratch: &mut Scratch,
        data: &[u8],
        out: &mut [f32],
        stride: usize,
        at: usize,
    ) -> Result<usize, OpusError> {
        let channels = self.channels;
        let frame_size = self.frame_size;
        // A frame of one byte or none is a lost one, or the silence of
        // discontinuous transmission.
        if data.len() <= 1 {
            for channel in 0..channels {
                out[channel * stride + at..channel * stride + at + frame_size].fill(0.0);
            }
            return Ok(frame_size);
        }
        let mode = self.mode;
        let mut ec = RangeDecoder::new(data);
        let mut len = data.len();

        // A change of mode without a redundant frame is smoothed over with
        // what concealment of the old mode would give; that is silence
        // here.
        let mut transition = matches!(self.prev_mode, Some(previous)
            if (mode == Mode::Celt && previous != Mode::Celt && !self.prev_redundancy)
                || (mode != Mode::Celt && previous == Mode::Celt));

        // SILK.
        if mode != Mode::Celt {
            if self.prev_mode == Some(Mode::Celt) {
                self.silk.reset();
            }
            let payload_ms = (1_000 * frame_size / 48_000).max(10);
            let internal_rate = if mode == Mode::Silk {
                match self.bandwidth {
                    Bandwidth::Narrow => 8_000,
                    Bandwidth::Medium => 12_000,
                    _ => 16_000,
                }
            } else {
                16_000
            };
            let silk_stride = 2_880;
            let mut decoded = 0;
            while decoded < frame_size {
                let first = decoded == 0;
                let mut buffer = std::mem::take(&mut scratch.pcm_silk);
                let written = self
                    .silk
                    .decode(
                        &mut scratch.silk,
                        &mut ec,
                        first,
                        internal_rate,
                        payload_ms,
                        self.stream_channels,
                        &mut buffer[decoded..],
                        silk_stride,
                    )
                    .map_err(|_| OpusError::Malformed("invalid SILK frame"));
                scratch.pcm_silk = buffer;
                decoded += written?;
            }
        }

        // The redundant CELT frame of a change of mode.
        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut redundancy_bytes = 0usize;
        let hybrid_bits = if self.mode == Mode::Hybrid { 20 } else { 0 };
        if mode != Mode::Celt && ec.tell() + 17 + hybrid_bits <= (8 * len) as i32 {
            redundancy = if mode == Mode::Hybrid {
                ec.bit_logp(12)
            } else {
                true
            };
            if redundancy {
                celt_to_silk = ec.bit_logp(1);
                redundancy_bytes = if mode == Mode::Hybrid {
                    ec.uint(256) as usize + 2
                } else {
                    len.saturating_sub(((ec.tell() + 7) >> 3) as usize)
                };
                if redundancy_bytes > len || ((len - redundancy_bytes) * 8) < ec.tell() as usize {
                    redundancy = false;
                    redundancy_bytes = 0;
                    len = 0;
                } else {
                    len -= redundancy_bytes;
                }
                ec.shrink(redundancy_bytes);
            }
        }
        let start_band = if mode != Mode::Celt { 17 } else { 0 };
        if redundancy {
            transition = false;
        }

        let end_band = match self.bandwidth {
            Bandwidth::Narrow => 13,
            Bandwidth::Medium | Bandwidth::Wide => 17,
            Bandwidth::SuperWide => 19,
            Bandwidth::Full => 21,
        };
        self.celt.set_end_band(end_band);
        self.celt.set_stream_channels(self.stream_channels);

        let redundant_data = &data[len..len + redundancy_bytes];
        // The 5 ms of CELT that lead from CELT into SILK.
        if redundancy && celt_to_silk {
            self.celt.set_start_band(0);
            let mut red_ec = RangeDecoder::new(redundant_data);
            let redundant = &mut scratch.redundant;
            // A damaged redundant frame is not worth failing the frame for.
            let _ = self.celt.decode(
                &mut scratch.celt,
                &mut red_ec,
                redundancy_bytes,
                F5,
                redundant,
                F5,
                false,
            );
        }
        self.celt.set_start_band(start_band);

        // CELT, alone or above SILK.
        if mode != Mode::Silk {
            let celt_frame = frame_size.min(MAX_FRAME);
            if self.prev_mode.is_some_and(|previous| previous != mode) && !self.prev_redundancy {
                self.celt.reset();
            }
            let output = &mut out[at..];
            self.celt.decode(
                &mut scratch.celt,
                &mut ec,
                len,
                celt_frame,
                output,
                stride,
                false,
            )?;
        } else {
            for channel in 0..channels {
                out[channel * stride + at..channel * stride + at + frame_size].fill(0.0);
            }
            // From hybrid into SILK, CELT fades out through a frame of
            // silence.
            if self.prev_mode == Some(Mode::Hybrid)
                && !(redundancy && celt_to_silk && self.prev_redundancy)
            {
                self.celt.set_start_band(0);
                let silence = [0xFFu8, 0xFF];
                let mut silent_ec = RangeDecoder::new(&silence);
                let output = &mut out[at..];
                let _ = self.celt.decode(
                    &mut scratch.celt,
                    &mut silent_ec,
                    2,
                    F2_5,
                    output,
                    stride,
                    true,
                );
            }
        }

        if mode != Mode::Celt {
            let silk_channels = channels.min(2);
            for channel in 0..silk_channels {
                let source = &scratch.pcm_silk[channel * 2_880..channel * 2_880 + frame_size];
                let target = &mut out[channel * stride + at..channel * stride + at + frame_size];
                for (y, &x) in target.iter_mut().zip(source) {
                    *y += (1.0 / 32_768.0) * f32::from(x);
                }
            }
        }

        let window = self.celt.window();
        // The 5 ms of CELT that lead from SILK into CELT.
        if redundancy && !celt_to_silk {
            self.celt.reset();
            self.celt.set_start_band(0);
            let mut red_ec = RangeDecoder::new(redundant_data);
            let redundant = &mut scratch.redundant;
            let _ = self.celt.decode(
                &mut scratch.celt,
                &mut red_ec,
                redundancy_bytes,
                F5,
                redundant,
                F5,
                false,
            );
            for channel in 0..channels {
                let base = channel * stride + at + frame_size - F2_5;
                for i in 0..F2_5 {
                    let w = window[i] * window[i];
                    let first = out[base + i];
                    let second = scratch.redundant[channel * F5 + F2_5 + i];
                    out[base + i] = w * second + (1.0 - w) * first;
                }
            }
        }
        if redundancy
            && celt_to_silk
            && (self.prev_mode != Some(Mode::Silk) || self.prev_redundancy)
        {
            for channel in 0..channels {
                let base = channel * stride + at;
                out[base..base + F2_5]
                    .copy_from_slice(&scratch.redundant[channel * F5..channel * F5 + F2_5]);
                for i in 0..F2_5 {
                    let w = window[i] * window[i];
                    let first = scratch.redundant[channel * F5 + F2_5 + i];
                    let second = out[base + F2_5 + i];
                    out[base + F2_5 + i] = w * second + (1.0 - w) * first;
                }
            }
        }
        if transition {
            // Concealment would give the old mode's continuation, faded
            // into the new; silence fades in instead.
            for channel in 0..channels {
                let base = channel * stride + at;
                if frame_size >= F5 {
                    out[base..base + F2_5].fill(0.0);
                    for i in 0..F2_5 {
                        let w = window[i] * window[i];
                        out[base + F2_5 + i] *= w;
                    }
                } else {
                    for i in 0..F2_5.min(frame_size) {
                        let w = window[i] * window[i];
                        out[base + i] *= w;
                    }
                }
            }
        }

        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        Ok(frame_size)
    }
}

/// The bytes a packet takes, for the next stream of a multistream packet.
fn packet_len(packet: &Packet) -> usize {
    packet.len
}

/// Decodes the packets of a multistream Opus stream (RFC 7845, section
/// 5.1.1) into its output channels.
pub(crate) struct MultistreamDecoder {
    streams: Vec<StreamDecoder>,
    /// How many of the streams are coupled, two channels each.
    coupled: usize,
    /// For each output channel, the decoded channel it takes, or 255 for
    /// silence.
    mapping: Vec<u8>,
    /// What each stream decodes, two channels of 120 ms each.
    decoded: Vec<f32>,
    scratch: Scratch,
    /// The output gain of the header, as a factor.
    gain: f32,
}

impl MultistreamDecoder {
    pub(crate) fn new(streams: usize, coupled: usize, mapping: Vec<u8>, gain_q8: i16) -> Self {
        let decoders = (0..streams)
            .map(|stream| StreamDecoder::new(if stream < coupled { 2 } else { 1 }))
            .collect();
        // 20 log10(gain) = gain_q8 / 256 dB, as `opus_decoder_ctl` sets it.
        let gain = super::celt::exp2_gain(gain_q8);
        Self {
            streams: decoders,
            coupled,
            mapping,
            decoded: vec![0.0; streams * 2 * MAX_PACKET_SAMPLES],
            scratch: Scratch::new(),
            gain,
        }
    }

    pub(crate) fn reset(&mut self) {
        for stream in &mut self.streams {
            stream.reset();
        }
    }

    /// The state of every stream's decoder, from which [`restore`](Self::restore)
    /// resumes the decoding.
    pub(crate) fn state(&self) -> Vec<StreamDecoder> {
        self.streams.clone()
    }

    pub(crate) fn restore(&mut self, state: &[StreamDecoder]) {
        self.streams.clone_from_slice(state);
    }

    /// Decodes a packet into `out`, `stride` samples a channel. Returns the
    /// samples decoded per channel.
    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        out: &mut [f32],
        stride: usize,
    ) -> Result<usize, OpusError> {
        let mut at = 0usize;
        let mut samples = None;
        let count = self.streams.len();
        for (index, stream) in self.streams.iter_mut().enumerate() {
            let last = index + 1 == count;
            let target = &mut self.decoded
                [index * 2 * MAX_PACKET_SAMPLES..(index + 1) * 2 * MAX_PACKET_SAMPLES];
            let rest = data
                .get(at..)
                .ok_or(OpusError::Malformed("multistream packet cut short"))?;
            let (decoded, used) =
                stream.decode(&mut self.scratch, rest, !last, target, MAX_PACKET_SAMPLES)?;
            match samples {
                None => samples = Some(decoded),
                Some(previous) if previous != decoded => {
                    return Err(OpusError::Malformed("streams of different durations"));
                }
                _ => {}
            }
            at += used;
        }
        let samples = samples.unwrap_or(0);
        for (channel, &source) in self.mapping.iter().enumerate() {
            let target = &mut out[channel * stride..channel * stride + samples];
            if source == 255 {
                target.fill(0.0);
                continue;
            }
            let source = usize::from(source);
            // Coupled streams come first, two channels each.
            let (stream, within) = if source < 2 * self.coupled {
                (source / 2, source % 2)
            } else {
                (source - self.coupled, 0)
            };
            let base = stream * 2 * MAX_PACKET_SAMPLES + within * MAX_PACKET_SAMPLES;
            let decoded = &self.decoded[base..base + samples];
            if self.gain == 1.0 {
                target.copy_from_slice(decoded);
            } else {
                for (y, &x) in target.iter_mut().zip(decoded) {
                    *y = x * self.gain;
                }
            }
        }
        Ok(samples)
    }
}
