//! Decoding an MP3 stream block by block, from any position.
//!
//! Frames carry no number and no time, so the only way to know which frame
//! is which is to count them from the start. The reader does, once: the
//! first time it goes past a place, by reading frame headers, four bytes a
//! frame, and remembering where every so many frames start. From there a
//! seek counts on to its frame, and decodes from a little before it:
//!
//! - the main data of a frame may start up to 511 bytes back, in the frames
//!   before it (the bit reservoir), so those are read first, without being
//!   decoded;
//! - the inverse transform overlaps each granule with the one before, and
//!   the synthesis filter bank remembers the last sixteen of its 18 time
//!   slots of a granule, so two granules decoded before the frame wanted
//!   leave the decoder exactly as a decode from the start would.
//!
//! What is read after a seek is then, bit for bit, what reading from the
//! start gives at that place.

use std::io::{self, Read, Seek, SeekFrom};

use manguebyte_core::AudioBuffer;

use super::Mp3Error;
use super::header::{FrameHeader, StreamInfo, crc16, id3v2_len, stream_info};
use super::layer3::{Layer3, side_info};
use super::layer12::Layer12;
use crate::window::Window;

/// The decoder of the stream's layer.
enum Decoder {
    Layer3(Box<Layer3>),
    Layer12(Box<Layer12>),
}

impl Decoder {
    fn reset(&mut self) {
        match self {
            Self::Layer3(decoder) => decoder.reset(),
            Self::Layer12(decoder) => decoder.reset(),
        }
    }

    /// The samples of a channel of the frame decoded last.
    fn pcm(&self, channel: usize) -> &[f32; 1_152] {
        match self {
            Self::Layer3(decoder) => &decoder.pcm[channel],
            Self::Layer12(decoder) => &decoder.pcm[channel],
        }
    }
}

/// How many bytes are read from the file at a time.
const READ_CHUNK: usize = 16 * 1_024;

/// The samples a decoder of layer III gives before the first sample of what
/// was encoded, beyond the delay the encoder states: the latency of the
/// filter banks, 528 samples, and one more. LAME counts its padding with
/// this in mind.
const DECODER_DELAY: u64 = 529;

/// Frames whose main data is read, without decoding them, before the
/// frames a seek decodes: enough to hold the 511 bytes a frame's main data
/// can start before it, at the lowest bitrate there is.
const RESERVOIR_FRAMES: u64 = 16;

/// The most frame positions kept. When there would be more, every other one
/// is dropped and twice as many frames lie between those left.
const MAX_INDEX: usize = 16_384;

/// How far ahead, in frames, a seek decodes its way rather than starting
/// again from a little before its target.
const SHORT_WALK: u64 = 4;

/// The largest frame of any layer: layer II at 384 kbit/s and 32 kHz,
/// padded. Layer III reaches 1441 bytes, layer I 676.
const MAX_FRAME: usize = 1_729;

/// Decodes an MP3 stream block by block, from any position.
///
/// Memory use does not depend on the length of the stream beyond the
/// frame positions it remembers, of which there are at most 16384.
pub(crate) struct Mp3Reader<R> {
    reader: R,
    window: Window,
    stream_len: u64,
    /// The first frame of the stream, which every other must match.
    format: FrameHeader,
    samples_per_frame: u64,
    /// Samples decoded before the first one handed out.
    skip: u64,
    /// The length of the audio in frames, when the stream says.
    total: Option<u64>,
    /// Where frame `k * stride` starts, for each `k` up to the furthest
    /// frame met.
    index: Vec<u64>,
    stride: u64,
    /// The furthest frame met, and where it starts.
    known: (u64, u64),
    /// How many frames the stream has, once its end has been met.
    frame_count: Option<u64>,
    decoder: Decoder,
    /// The main data of the last frames, for the frames after them to start
    /// in.
    reservoir: Vec<u8>,
    main: Vec<u8>,
    frame: Vec<u8>,
    /// The next frame to decode, and where it starts. `None` past the end.
    next: Option<(u64, u64)>,
    /// The frame whose samples the decoder holds.
    block: Option<u64>,
    /// The frame the next read starts at.
    position: u64,
    /// Set when a failure left the decoder somewhere unknown. The next
    /// read then finds its place again.
    lost: bool,
}

impl<R: Read + Seek> Mp3Reader<R> {
    /// Finds the first frame of the stream in `reader`, reads what a first
    /// frame of silence may say about the stream, and gets ready to decode
    /// from its first sample.
    pub(crate) fn new(mut reader: R) -> Result<Self, Mp3Error> {
        let stream_len = reader.seek(SeekFrom::End(0))?;
        let mut window = Window::with_chunk(0, stream_len, READ_CHUNK);

        // Tags first: one or more ID3v2 tags may come before the audio.
        let mut start = 0u64;
        loop {
            window.move_to(start);
            window.fill(&mut reader, 10)?;
            match id3v2_len(window.available()) {
                Some(len) => start = start.saturating_add(len),
                None => break,
            }
        }

        let mut this = Self {
            reader,
            window,
            stream_len,
            format: FrameHeader::parse([0xFF, 0xFB, 0x90, 0x00]).expect("a valid header"),
            samples_per_frame: 1_152,
            skip: 0,
            total: None,
            index: Vec::new(),
            stride: 1,
            known: (0, 0),
            frame_count: None,
            decoder: Decoder::Layer3(Box::new(Layer3::new())),
            reservoir: Vec::with_capacity(2 * 1_024),
            main: Vec::with_capacity(1_024 + MAX_FRAME),
            frame: Vec::with_capacity(MAX_FRAME),
            next: None,
            block: None,
            position: 0,
            lost: false,
        };

        let Some((offset, header)) = this.find_first_frame(start)? else {
            return Err(Mp3Error::NotMp3);
        };
        if header.bitrate == 0 {
            return Err(Mp3Error::Unsupported("free-format bitrate"));
        }
        this.format = header;
        this.samples_per_frame = header.samples() as u64;
        if header.layer != 3 {
            this.decoder = Decoder::Layer12(Box::new(Layer12::new()));
        }

        // A first frame of layer III may hold no audio but a header that
        // says how long the stream is and what the encoder put around the
        // audio.
        let mut first = offset;
        if header.layer == 3 && this.load_frame(offset, &header)? {
            if let Some(info) = stream_info(&this.frame, &header) {
                this.apply(info);
                match this.frame_after(offset, &header)? {
                    Some((next, _)) => first = next,
                    None => first = stream_len,
                }
            }
        }
        this.index.push(first);
        this.known = (0, first);
        this.next = (first < stream_len).then_some((0, first));
        if this.next.is_none() {
            this.frame_count = Some(0);
        }
        Ok(this)
    }

    /// Takes in what a header in the first frame says.
    fn apply(&mut self, info: StreamInfo) {
        let spf = self.samples_per_frame;
        let decoded = info.frames.map(|frames| u64::from(frames) * spf);
        match info.delay {
            Some((delay, padding)) => {
                self.skip = u64::from(delay) + DECODER_DELAY;
                self.total = decoded.map(|decoded| {
                    let stated = decoded.saturating_sub(u64::from(delay) + u64::from(padding));
                    stated.min(decoded.saturating_sub(self.skip))
                });
            }
            None => self.total = decoded,
        }
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.format.sample_rate
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.format.channels()
    }

    /// The length the stream states, if it states one.
    pub(crate) fn frames(&self) -> Option<u64> {
        self.total
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn into_inner(self) -> R {
        self.reader
    }

    /// The header at `offset`, if there is a frame header there.
    fn header_at(&mut self, offset: u64) -> io::Result<Option<FrameHeader>> {
        if offset.saturating_add(4) > self.stream_len {
            return Ok(None);
        }
        self.window.move_to(offset);
        self.window.fill(&mut self.reader, 4)?;
        let Some(bytes) = self.window.available().get(..4) else {
            return Ok(None);
        };
        Ok(FrameHeader::parse([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Whether a frame of this stream starts at `offset`: its header is
    /// one, it matches the stream, and the file holds all of it.
    fn frame_at(&mut self, offset: u64) -> io::Result<Option<FrameHeader>> {
        Ok(self.header_at(offset)?.filter(|header| {
            header.matches(&self.format)
                && header.bitrate != 0
                && offset.saturating_add(header.len() as u64) <= self.stream_len
        }))
    }

    /// Finds the first frame at or after `from`: a header there, or, if
    /// there is none, the first header further on that is followed by
    /// another of the same kind where the first ends.
    fn find_first_frame(&mut self, from: u64) -> io::Result<Option<(u64, FrameHeader)>> {
        if let Some(header) = self.header_at(from)? {
            if header.bitrate == 0 {
                // A frame this decoder does not read: the caller says so.
                return Ok(Some((from, header)));
            }
            self.format = header;
            if self.frame_at(from)?.is_some() {
                return Ok(Some((from, header)));
            }
        }
        let mut at = from;
        while at.saturating_add(4) <= self.stream_len {
            if let Some(header) = self.header_at(at)? {
                if header.bitrate != 0 {
                    self.format = header;
                    let end = at + header.len() as u64;
                    let followed = end == self.stream_len || self.frame_at(end)?.is_some();
                    if end <= self.stream_len && followed {
                        return Ok(Some((at, header)));
                    }
                }
            }
            at += 1;
        }
        Ok(None)
    }

    /// The frame after the one at `offset`: where it ends, if a frame of
    /// the stream starts there; otherwise the first frame further on that
    /// is followed by another, or by the end of the file. `None` when no
    /// frame is left.
    ///
    /// This depends on the bytes of the file alone, so that whichever frame
    /// counting starts from, the same frames are found.
    fn frame_after(
        &mut self,
        offset: u64,
        header: &FrameHeader,
    ) -> io::Result<Option<(u64, FrameHeader)>> {
        let expected = offset + header.len() as u64;
        if let Some(next) = self.frame_at(expected)? {
            return Ok(Some((expected, next)));
        }
        // Something that is not a frame: a tag, or damage. Look for where
        // the frames go on, a byte at a time.
        let mut at = expected + 1;
        while at.saturating_add(4) <= self.stream_len {
            if let Some(candidate) = self.frame_at(at)? {
                let end = at + candidate.len() as u64;
                if end == self.stream_len || self.frame_at(end)?.is_some() {
                    return Ok(Some((at, candidate)));
                }
            }
            at += 1;
        }
        Ok(None)
    }

    /// Reads the whole frame at `offset` into `self.frame`. Returns `false`
    /// if the file ends inside it.
    fn load_frame(&mut self, offset: u64, header: &FrameHeader) -> io::Result<bool> {
        let len = header.len();
        self.window.move_to(offset);
        self.window.fill(&mut self.reader, len)?;
        let Some(bytes) = self.window.available().get(..len) else {
            return Ok(false);
        };
        self.frame.clear();
        self.frame.extend_from_slice(bytes);
        Ok(true)
    }

    /// Notes that frame `number` starts at `offset`, if it is the next
    /// frame not met before.
    fn remember(&mut self, number: u64, offset: u64) {
        if number != self.known.0 + 1 {
            return;
        }
        self.known = (number, offset);
        if number % self.stride == 0 {
            self.index.push(offset);
            if self.index.len() > MAX_INDEX {
                // Keep every other one: frames 0, 2 * stride, 4 * stride...
                let kept: Vec<u64> = self.index.iter().copied().step_by(2).collect();
                self.index = kept;
                self.stride *= 2;
            }
        }
    }

    /// Where frame `number` starts, counting frames from the nearest one
    /// whose place is known. `None` if the stream has fewer frames.
    fn frame_offset(&mut self, number: u64) -> io::Result<Option<u64>> {
        if self.frame_count.is_some_and(|count| number >= count) {
            return Ok(None);
        }
        let (mut at_number, mut at) = if number <= self.known.0 {
            let point = number / self.stride;
            (point * self.stride, self.index[point as usize])
        } else {
            self.known
        };
        while at_number < number {
            let Some(header) = self.frame_at(at)? else {
                self.frame_count = Some(at_number);
                return Ok(None);
            };
            match self.frame_after(at, &header)? {
                Some((next, _)) => {
                    at_number += 1;
                    at = next;
                    self.remember(at_number, at);
                }
                None => {
                    self.frame_count = Some(at_number + 1);
                    return Ok(None);
                }
            }
        }
        Ok(Some(at))
    }

    /// Fills `out` from the current position and returns how many frames
    /// were decoded: fewer than `out` holds only at the end of the stream.
    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> Result<usize, Mp3Error> {
        if self.lost {
            self.reposition(self.position)?;
        }
        let spf = self.samples_per_frame;
        let want = out.num_frames();
        let mut done = 0;
        while done < want {
            let left = self
                .total
                .map_or(u64::MAX, |total| total.saturating_sub(self.position));
            if left == 0 {
                break;
            }
            let sample = self.position + self.skip;
            let frame = sample / spf;
            if self.block != Some(frame) {
                match self.next {
                    Some((next, _)) if next <= frame => {
                        if let Err(error) = self.decode_next() {
                            self.lost = true;
                            return Err(error);
                        }
                        continue;
                    }
                    // The frame wanted is behind the decoder, which only a
                    // seek that has not happened yet can bring about.
                    Some(_) => {
                        self.reposition(self.position)?;
                        continue;
                    }
                    None => break,
                }
            }
            let from = (sample - frame * spf) as usize;
            let count = (spf as usize - from)
                .min(want - done)
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            for (channel, samples) in out.iter_channels_mut().enumerate() {
                samples[done..done + count]
                    .copy_from_slice(&self.decoder.pcm(channel)[from..from + count]);
            }
            done += count;
            self.position += count as u64;
        }
        Ok(done)
    }

    /// Decodes the next frame. At the end of the stream there is none, and
    /// `next` becomes `None`.
    fn decode_next(&mut self) -> Result<(), Mp3Error> {
        let Some((number, offset)) = self.next else {
            return Ok(());
        };
        let Some(header) = self.frame_at(offset)? else {
            self.end_at(number);
            return Ok(());
        };
        if !self.load_frame(offset, &header)? {
            self.end_at(number);
            return Ok(());
        }
        let decoder = match &mut self.decoder {
            Decoder::Layer12(decoder) => {
                // Layers I and II keep everything a frame needs in the
                // frame, CRC included.
                decoder.decode(&header, &self.frame)?;
                None
            }
            Decoder::Layer3(decoder) => Some(decoder),
        };
        if let Some(decoder) = decoder {
            let side_start = header.side_info_start();
            let side_end = side_start + header.side_info_len();
            if header.protected {
                // The CRC covers the last two bytes of the header and the
                // side information.
                let stored = u16::from_be_bytes([self.frame[4], self.frame[5]]);
                let mut covered = [0u8; 34];
                covered[..2].copy_from_slice(&self.frame[2..4]);
                covered[2..2 + side_end - side_start]
                    .copy_from_slice(&self.frame[side_start..side_end]);
                if crc16(&covered[..2 + side_end - side_start]) != stored {
                    return Err(Mp3Error::Corrupt);
                }
            }
            let side = side_info(&header, &self.frame[side_start..side_end])?;

            // The main data: the end of what the frames before left, then
            // this frame's own. If the frames before are not there, as at
            // the start of a stream that was cut, the frame is decoded as
            // silence.
            self.main.clear();
            let begin = side.main_data_begin;
            if begin <= self.reservoir.len() {
                self.main
                    .extend_from_slice(&self.reservoir[self.reservoir.len() - begin..]);
                self.main.extend_from_slice(&self.frame[side_end..]);
                if side.main_data_bits(&header) > self.main.len() * 8 {
                    return Err(Mp3Error::Malformed("main data runs past the frame"));
                }
            }
            decoder.decode(&header, &side, &self.main, 0)?;
            self.keep_main_data(side_end);
        }
        self.block = Some(number);
        self.next = match self.frame_after(offset, &header)? {
            Some((next, _)) => {
                self.remember(number + 1, next);
                Some((number + 1, next))
            }
            None => {
                self.frame_count = Some(number + 1);
                None
            }
        };
        Ok(())
    }

    /// Adds the main data of the frame in `self.frame`, which starts at
    /// `side_end`, to the reservoir, keeping only as much as a later frame
    /// can reach back into.
    fn keep_main_data(&mut self, side_end: usize) {
        self.reservoir.extend_from_slice(&self.frame[side_end..]);
        let excess = self.reservoir.len().saturating_sub(1_024);
        self.reservoir.drain(..excess);
    }

    /// Notes that the stream ends before frame `number`.
    fn end_at(&mut self, number: u64) {
        self.frame_count = Some(number);
        self.next = None;
    }

    /// Moves to `frame`, or to the end if the stream is shorter, and returns
    /// where it landed.
    ///
    /// If this fails, the position is unchanged.
    pub(crate) fn seek(&mut self, frame: u64) -> Result<u64, Mp3Error> {
        let target = self.total.map_or(frame, |total| frame.min(total));
        if !self.lost {
            let wanted = (target.saturating_add(self.skip)) / self.samples_per_frame;
            let in_hand = self.block == Some(wanted);
            // A frame past the length the stream states is not there; past
            // the frames counted so far, in a stream that states none, it
            // may not be, and the end is found by counting on.
            let exists = self.total.is_some()
                || (wanted <= self.known.0 && self.frame_count.is_none_or(|count| wanted < count));
            // A short way ahead, decoding on costs less than starting again.
            let ahead = exists
                && match (self.block, self.next) {
                    (Some(block), Some(_)) => wanted > block && wanted - block <= SHORT_WALK,
                    (None, Some((next, _))) => wanted >= next && wanted - next < SHORT_WALK,
                    _ => false,
                };
            if in_hand || ahead {
                self.position = target;
                return Ok(target);
            }
        }
        self.reposition(target)
    }

    /// Starts decoding again a little before `target`. On failure the
    /// position is put back, and the reader is marked as lost so that the
    /// next read finds its place again.
    fn reposition(&mut self, target: u64) -> Result<u64, Mp3Error> {
        let previous = self.position;
        match self.start_before(target) {
            Ok(landed) => {
                self.lost = false;
                self.position = landed;
                Ok(landed)
            }
            Err(error) => {
                self.position = previous;
                self.lost = true;
                Err(error)
            }
        }
    }

    fn start_before(&mut self, target: u64) -> Result<u64, Mp3Error> {
        let spf = self.samples_per_frame;
        let wanted = target.saturating_add(self.skip) / spf;
        // In layer III, two granules decoded before the frame wanted set the
        // decoder's state as a decode from the start would. In layers I and
        // II only the synthesis filter bank remembers, its last sixteen time
        // slots: two frames of twelve in layer I, one of 36 in layer II.
        // Only layer III has a bit reservoir to fill first.
        let (preroll, reservoir) = match self.format.layer {
            1 => (2, 0),
            2 => (1, 0),
            _ => (2 / self.format.granules() as u64, RESERVOIR_FRAMES),
        };
        let first = wanted.saturating_sub(preroll);
        let load = first.saturating_sub(reservoir);

        self.decoder.reset();
        self.reservoir.clear();
        self.block = None;
        let Some(mut offset) = self.frame_offset(load)? else {
            // The target is past the last frame: stand at the end.
            let count = self.frame_count.unwrap_or(0);
            self.next = None;
            let end = (count * spf).saturating_sub(self.skip);
            return Ok(target.min(end));
        };
        let mut number = load;
        while number < first {
            let Some(header) = self.frame_at(offset)? else {
                break;
            };
            if !self.load_frame(offset, &header)? {
                break;
            }
            self.keep_main_data(header.side_info_start() + header.side_info_len());
            match self.frame_after(offset, &header)? {
                Some((next, _)) => {
                    number += 1;
                    offset = next;
                    self.remember(number, offset);
                }
                None => {
                    self.frame_count = Some(number + 1);
                    break;
                }
            }
        }
        self.next = (number == first).then_some((first, offset));
        while let Some((next, _)) = self.next {
            if next >= wanted {
                break;
            }
            self.decode_next()?;
        }
        // What those frames decoded to began from nothing, and is not what
        // a decode from the start gives: it is only their state that counts.
        self.block = None;
        let end = self
            .frame_count
            .map_or(u64::MAX, |count| (count * spf).saturating_sub(self.skip));
        Ok(target.min(end))
    }
}
