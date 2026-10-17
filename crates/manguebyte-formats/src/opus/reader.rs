//! Decoding an Ogg Opus stream (RFC 7845) block by block, from any
//! position.
//!
//! Granule positions count samples at 48 kHz, whatever the rate of the
//! audio that was encoded, and the duration of a packet is in its first
//! byte, so where every packet starts is known without decoding it. The
//! first `pre-skip` samples the stream decodes to are the encoder's delay
//! and are dropped, and the granule position of the last page says where
//! the audio ends.
//!
//! Opus packets depend on the ones before them, and not just through an
//! overlap: the energy of each CELT band is predicted from the frame
//! before, the long-term predictor of SILK feeds on what it gave out, and
//! the post-filter and the resampler run on. Decoding from anywhere but the
//! start gives samples that draw near those of a decode from the start,
//! but how soon they become the same, bit for bit, the format does not
//! bound: in the tests, CELT does within a second or so, and SILK, on a
//! steady voiced sound, may take until the next silence.
//!
//! So a seek lands exactly when it can afford to: the reader keeps the
//! state of the decoder, every so often, while it decodes from the start
//! (a [`Snapshot`], taken where a page ends with a packet), and a seek to a
//! place at most [`EXACT_REACH`] past such a state, or past the start, or
//! past where the reader stands, decodes on from there. Any other seek
//! starts [`PREROLL`] before its target, as RFC 7845 (section 4.6)
//! recommends, and gives what that decode gives, which is not bit for bit
//! what a decode from the start gives there; reading on from it keeps no
//! snapshots, until a seek takes the reader back to an exact place.

use std::io::{Read, Seek, SeekFrom};

use manguebyte_core::AudioBuffer;

use super::OpusError;
use super::decoder::{MAX_PACKET_SAMPLES, MultistreamDecoder, StreamDecoder};
use super::packet;
use crate::ogg::{self, FIRST, HEADER_LEN, NO_GRANULE, PacketError, PacketReader};
use crate::vorbis::{Comments, channel_order, parse_comments};
use crate::window::Window;

/// How far before its target a seek that cannot be exact starts to decode:
/// 800 ms.
pub(crate) const PREROLL: i64 = 38_400;

/// How often the state of a decode from the start is kept, at first: every
/// 10 s of the stream.
const SNAPSHOT_INTERVAL: i64 = 480_000;

/// How many states are kept. When there are more, every other one is let
/// go and the interval doubles.
const MAX_SNAPSHOTS: usize = 64;

/// How far past a place decoded from the start a seek decodes on to land
/// exactly rather than starting [`PREROLL`] before its target: 20 s of the
/// stream, on top of the interval between the states kept.
const EXACT_REACH: i64 = 960_000;

/// The longest comment header kept. Pictures can be carried in comments.
const COMMENT_LIMIT: usize = 64 << 20;

/// How many bytes are read from the file at a time.
const READ_CHUNK: usize = 16 * 1_024;

/// How close the search for a page narrows in before it reads its way on.
const SEARCH_SPAN: u64 = READ_CHUNK as u64;

/// How many packets may come before a page that says where it is.
const ANCHOR_LIMIT: usize = 4_096;

impl From<PacketError> for OpusError {
    fn from(error: PacketError) -> Self {
        match error {
            PacketError::Io(error) => Self::Io(error),
            PacketError::Corrupt => Self::Corrupt,
            PacketError::Malformed(what) => Self::Malformed(what),
        }
    }
}

/// What the identification header says (RFC 7845, section 5.1).
#[derive(Debug, Clone)]
pub(crate) struct Head {
    pub(crate) channels: usize,
    pub(crate) pre_skip: u64,
    pub(crate) input_rate: u32,
    pub(crate) gain_q8: i16,
    pub(crate) family: u8,
    pub(crate) streams: usize,
    pub(crate) coupled: usize,
    pub(crate) mapping: Vec<u8>,
}

/// Reads the identification header.
pub(crate) fn parse_head(packet: &[u8]) -> Result<Head, OpusError> {
    if !packet.starts_with(b"OpusHead") {
        return Err(OpusError::NotOpus);
    }
    if packet.len() < 19 {
        return Err(OpusError::Malformed("identification header too short"));
    }
    // A version of another major number is one this decoder cannot know.
    if packet[8] >> 4 != 0 {
        return Err(OpusError::Unsupported("an Ogg Opus version other than 0.x"));
    }
    let channels = usize::from(packet[9]);
    if channels == 0 {
        return Err(OpusError::Malformed("no channels"));
    }
    let pre_skip = u64::from(u16::from_le_bytes([packet[10], packet[11]]));
    let input_rate = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);
    let gain_q8 = i16::from_le_bytes([packet[16], packet[17]]);
    let family = packet[18];
    let (streams, coupled, mapping) = if family == 0 {
        if channels > 2 {
            return Err(OpusError::Malformed(
                "more than two channels in mapping family 0",
            ));
        }
        (1, channels - 1, (0..channels as u8).collect())
    } else {
        if packet.len() < 21 + channels {
            return Err(OpusError::Malformed("identification header too short"));
        }
        let streams = usize::from(packet[19]);
        let coupled = usize::from(packet[20]);
        if streams == 0 || coupled > streams || streams + coupled > 255 {
            return Err(OpusError::Malformed("invalid stream counts"));
        }
        let mapping = packet[21..21 + channels].to_vec();
        if mapping
            .iter()
            .any(|&index| index != 255 && usize::from(index) >= streams + coupled)
        {
            return Err(OpusError::Malformed("a channel mapped to no stream"));
        }
        if family == 1 && channels > 8 {
            return Err(OpusError::Malformed(
                "more than eight channels in mapping family 1",
            ));
        }
        (streams, coupled, mapping)
    };
    Ok(Head {
        channels,
        pre_skip,
        input_rate,
        gain_q8,
        family,
        streams,
        coupled,
        mapping,
    })
}

/// The samples a packet decodes to, from its first bytes; `None` for one
/// that is not a valid packet, which is then decoded as nothing.
fn packet_samples(data: &[u8]) -> Option<usize> {
    let toc = packet::Toc::parse(*data.first()?);
    let count = match data[0] & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => usize::from(*data.get(1)? & 0x3F),
    };
    let samples = count * toc.frame_size;
    (count > 0 && samples <= MAX_PACKET_SAMPLES).then_some(samples)
}

/// A page the search found: where it ends and the granule position it
/// states.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    start: u64,
    end: u64,
    granule: i64,
}

/// The state of the decoder at a page boundary of a decode from the start,
/// from which decoding resumes exactly.
struct Snapshot {
    /// The index of the first sample of the packet that comes next.
    index: i64,
    /// Where in the file the page with that packet starts.
    offset: u64,
    state: Vec<StreamDecoder>,
}

/// Decodes an Ogg Opus stream block by block, from any position.
pub(crate) struct OpusReader<R> {
    reader: R,
    window: Window,
    packets: PacketReader,
    decoder: MultistreamDecoder,
    head: Head,
    /// For each channel handed out, the decoded channel it is.
    order: Vec<usize>,
    audio_start: u64,
    stream_len: u64,
    /// The granule position of the first sample of the stream.
    first_granule: i64,
    total: Option<u64>,
    position: u64,
    /// The packet decoded last, `MAX_PACKET_SAMPLES` per channel; the
    /// index in the stream of its first sample, and how many it holds.
    pcm: Vec<f32>,
    block_start: i64,
    block_len: usize,
    /// The index of the first sample of the next packet.
    next_start: i64,
    /// A packet a seek read before it knew it wanted it.
    pending: Option<Vec<u8>>,
    lost: bool,
    comments: Comments,
    /// Whether the decoder is where a decode from the start would have it,
    /// bit for bit: it has decoded on from the start or from a snapshot.
    exact: bool,
    /// States of a decode from the start, in the order of the stream.
    snapshots: Vec<Snapshot>,
    snapshot_interval: i64,
    /// The index past which the next snapshot is taken.
    next_snapshot: i64,
    exact_reach: i64,
    max_snapshots: usize,
    search_span: u64,
    preroll: i64,
}

impl<R: Read + Seek> OpusReader<R> {
    /// Reads the headers of the stream in `reader` and gets ready to decode
    /// from its first sample.
    pub(crate) fn new(mut reader: R) -> Result<Self, OpusError> {
        let stream_len = reader.seek(SeekFrom::End(0))?;
        let mut window = Window::with_chunk(0, stream_len, READ_CHUNK);

        // The first page of the stream whose packet is an Opus header.
        let serial = loop {
            let page = match ogg::read_page(&mut window, &mut reader) {
                Ok(Some(page)) => page,
                Ok(None) if window.file_position() == 0 && stream_len > 0 => {
                    return Err(OpusError::Truncated);
                }
                Ok(None) => return Err(OpusError::NotOpus),
                Err(PacketError::Malformed(_)) => return Err(OpusError::NotOpus),
                Err(error) => return Err(error.into()),
            };
            if page.flags & FIRST == 0 {
                return Err(OpusError::NotOpus);
            }
            let bytes = &window.available()[..page.len];
            let body = &bytes[HEADER_LEN + usize::from(bytes[26])..];
            if body.starts_with(b"OpusHead") {
                break page.serial;
            }
            window.position += page.len;
        };

        let mut packets = PacketReader::new(serial, 64 * 1_024);
        if packets
            .next_packet(&mut window, &mut reader, true)?
            .is_none()
        {
            return Err(OpusError::Truncated);
        }
        let head = parse_head(packets.packet())?;
        if !packets.at_page_end() {
            return Err(OpusError::Malformed(
                "the identification header shares its page",
            ));
        }
        packets.set_max_packet(COMMENT_LIMIT);
        if packets
            .next_packet(&mut window, &mut reader, true)?
            .is_none()
        {
            return Err(OpusError::Truncated);
        }
        let tags = packets.packet();
        if !tags.starts_with(b"OpusTags") {
            return Err(OpusError::Malformed("no comment header"));
        }
        let comments = parse_comments(&tags[8..])
            .map_err(|_| OpusError::Malformed("comment header too short"))?;
        if !packets.at_page_end() {
            return Err(OpusError::Malformed(
                "audio starts on the page of the comment header",
            ));
        }
        let audio_start = window.file_position();
        packets.shrink();
        // The longest packet: 120 ms of the longest frames, in every
        // stream.
        packets.set_max_packet(head.streams * (48 * 1_275 + 100));

        let order = if head.family == 1 {
            channel_order(head.channels)
        } else {
            (0..head.channels).collect()
        };
        let decoder = MultistreamDecoder::new(
            head.streams,
            head.coupled,
            head.mapping.clone(),
            head.gain_q8,
        );
        let mut this = Self {
            reader,
            window,
            packets,
            decoder,
            order,
            audio_start,
            stream_len,
            first_granule: 0,
            total: None,
            position: 0,
            pcm: vec![0.0; head.channels * MAX_PACKET_SAMPLES],
            block_start: 0,
            block_len: 0,
            next_start: 0,
            pending: None,
            lost: false,
            comments,
            head,
            exact: true,
            snapshots: Vec::new(),
            snapshot_interval: SNAPSHOT_INTERVAL,
            next_snapshot: SNAPSHOT_INTERVAL,
            exact_reach: EXACT_REACH,
            max_snapshots: MAX_SNAPSHOTS,
            search_span: SEARCH_SPAN,
            preroll: PREROLL,
        };
        // Where the samples start: worked back from the first page that
        // says how many there are.
        this.first_granule = match this.anchor(audio_start)? {
            Some((granule, last)) if granule < 0 && last => 0,
            Some((granule, _)) if granule < 0 => {
                return Err(OpusError::Malformed(
                    "the first page ends before the samples of its packets",
                ));
            }
            Some((granule, _)) => granule,
            None => 0,
        };
        this.total = this
            .last_granule()?
            .map(|last| (last - this.first_granule - this.head.pre_skip as i64).max(0) as u64);
        this.restart();
        Ok(this)
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.head.channels
    }

    pub(crate) fn frames(&self) -> Option<u64> {
        self.total
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn into_inner(self) -> R {
        self.reader
    }

    /// The rate of the audio before it was encoded, which the header gives
    /// for information.
    pub(crate) fn input_rate(&self) -> u32 {
        self.head.input_rate
    }

    pub(crate) fn take_comments(&mut self) -> Comments {
        std::mem::take(&mut self.comments)
    }

    /// Back to the first page of audio.
    fn restart(&mut self) {
        self.window.move_to(self.audio_start);
        self.packets.restart();
        self.decoder.reset();
        self.block_start = 0;
        self.block_len = 0;
        self.next_start = 0;
        self.pending = None;
        self.exact = true;
        self.next_snapshot = self.snapshot_interval;
    }

    /// Back to the state of a snapshot.
    fn restore(&mut self, which: usize) {
        let snapshot = &self.snapshots[which];
        self.window.move_to(snapshot.offset);
        self.packets.restart();
        self.decoder.restore(&snapshot.state);
        self.block_start = snapshot.index;
        self.block_len = 0;
        self.next_start = snapshot.index;
        self.pending = None;
        self.exact = true;
        self.next_snapshot = snapshot.index + self.snapshot_interval;
    }

    /// Keeps the state of the decoder, if it is exact, the time has come,
    /// and the packet decoded last ended its page, so that decoding can
    /// resume at the page after it.
    fn keep_snapshot(&mut self) {
        if !self.exact || self.next_start < self.next_snapshot || !self.packets.at_page_end() {
            return;
        }
        if self
            .snapshots
            .last()
            .is_some_and(|last| last.index >= self.next_start)
        {
            self.next_snapshot = self.next_start + self.snapshot_interval;
            return;
        }
        self.snapshots.push(Snapshot {
            index: self.next_start,
            offset: self.window.file_position(),
            state: self.decoder.state(),
        });
        if self.snapshots.len() > self.max_snapshots {
            let mut keep = false;
            self.snapshots.retain(|_| {
                keep = !keep;
                keep
            });
            self.snapshot_interval *= 2;
        }
        self.next_snapshot = self.next_start + self.snapshot_interval;
    }

    /// The index in the stream of the frame `position`.
    fn index_of(&self, position: u64) -> i64 {
        (position.min(i64::MAX as u64 / 2) as i64).saturating_add(self.head.pre_skip as i64)
    }

    /// Fills `out` from the current position.
    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> Result<usize, OpusError> {
        if self.lost {
            self.reposition(self.position)?;
        }
        let want = out.num_frames();
        let mut done = 0;
        while done < want {
            let left = self
                .total
                .map_or(u64::MAX, |total| total.saturating_sub(self.position));
            if left == 0 {
                break;
            }
            let index = self.index_of(self.position);
            let block_end = self.block_start + self.block_len as i64;
            if index < self.block_start {
                // Samples of a packet that could not be decoded, or that
                // granule positions which contradict the packets skip:
                // silence.
                let gap = ((self.block_start - index) as usize)
                    .min(want - done)
                    .min(usize::try_from(left).unwrap_or(usize::MAX));
                for channel in out.iter_channels_mut() {
                    channel[done..done + gap].fill(0.0);
                }
                done += gap;
                self.position += gap as u64;
                continue;
            }
            if index >= block_end {
                if !self.next_block()? {
                    break;
                }
                continue;
            }
            let from = (index - self.block_start) as usize;
            let count = (self.block_len - from)
                .min(want - done)
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            for (output, &channel) in self.order.iter().enumerate() {
                let source = &self.pcm[channel * MAX_PACKET_SAMPLES + from
                    ..channel * MAX_PACKET_SAMPLES + from + count];
                out.channel_mut(output)[done..done + count].copy_from_slice(source);
            }
            done += count;
            self.position += count as u64;
        }
        Ok(done)
    }

    /// Decodes the next packet. Returns `false` at the end of the stream.
    fn next_block(&mut self) -> Result<bool, OpusError> {
        let pending = self.pending.take();
        let from_pending = pending.is_some();
        let data: &[u8] = match &pending {
            Some(packet) => packet,
            None => {
                if self
                    .packets
                    .next_packet(&mut self.window, &mut self.reader, true)?
                    .is_none()
                {
                    return Ok(false);
                }
                self.packets.packet()
            }
        };
        let start = self.next_start;
        let samples = if data.is_empty() {
            0
        } else {
            match self.decoder.decode(data, &mut self.pcm, MAX_PACKET_SAMPLES) {
                Ok(samples) => samples,
                Err(error) => {
                    // The packet's samples are skipped, as silence, so that
                    // the samples after it keep their places.
                    let skipped = packet_samples(data).unwrap_or(0) as i64;
                    self.block_start = start + skipped;
                    self.block_len = 0;
                    self.next_start = start + skipped;
                    return Err(error);
                }
            }
        };
        self.block_start = start;
        self.block_len = samples;
        self.next_start = start + samples as i64;
        if !from_pending {
            self.keep_snapshot();
        }
        Ok(true)
    }

    /// Moves to `frame`, or to the end, and returns where it landed.
    pub(crate) fn seek(&mut self, frame: u64) -> Result<u64, OpusError> {
        let target = self.total.map_or(frame, |total| frame.min(total));
        if !self.lost {
            let index = self.index_of(target);
            let block_end = self.block_start + self.block_len as i64;
            if (self.block_start..=block_end).contains(&index) && self.block_len > 0 {
                self.position = target;
                return Ok(target);
            }
        }
        self.reposition(target)
    }

    fn reposition(&mut self, target: u64) -> Result<u64, OpusError> {
        let previous = self.position;
        match self.search(target) {
            Ok(()) => {
                self.lost = false;
                self.position = target;
                Ok(target)
            }
            Err(error) => {
                self.position = previous;
                self.lost = true;
                Err(error)
            }
        }
    }

    /// Gets ready to decode up to `target`: exactly, from the latest place
    /// decoded from the start that is close enough, or else from
    /// [`PREROLL`] before it.
    fn search(&mut self, target: u64) -> Result<(), OpusError> {
        let index = self.index_of(target);
        // The latest exact place at or before the target: where the decoder
        // stands, a snapshot, or the start.
        let current = (self.exact && !self.lost && self.pending.is_none())
            .then_some(self.next_start)
            .filter(|&at| at <= index);
        let snapshot = self
            .snapshots
            .partition_point(|snapshot| snapshot.index <= index)
            .checked_sub(1);
        let snapshot_index = snapshot.map_or(0, |which| self.snapshots[which].index);
        let reach = self.exact_reach + self.snapshot_interval;
        if current.is_some_and(|at| at >= snapshot_index && index - at <= reach) {
            // Read on from where the decoder stands.
            return Ok(());
        }
        if index - snapshot_index <= reach || index <= self.preroll {
            match snapshot {
                Some(which) if index - snapshot_index <= reach => self.restore(which),
                _ => self.restart(),
            }
            return Ok(());
        }
        let start = index - self.preroll;
        let granule = self.first_granule.saturating_add(start);
        let mut lower: Option<Anchor> = None;
        let mut upper = self.stream_len;
        loop {
            let from = lower.map_or(self.audio_start, |anchor| anchor.end);
            if upper.saturating_sub(from) <= self.search_span {
                break;
            }
            let middle = from + (upper - from) / 2;
            match self.find_anchor(middle, upper)? {
                Some(found)
                    if found.granule <= granule
                        && lower.is_none_or(|lower| found.granule >= lower.granule) =>
                {
                    lower = Some(found);
                }
                _ => upper = middle,
            }
        }
        let Some(anchor) = lower else {
            // Within the first pages: from the start, exactly.
            self.restart();
            return Ok(());
        };
        let first = self.first_after(anchor)?;
        self.walk(anchor.end, first - self.first_granule, start)?;
        self.exact = false;
        Ok(())
    }

    /// The granule position at which the first packet that starts after
    /// the page of `anchor` begins: the page's own granule position, or, if
    /// a packet begins on the page and runs on past it, that position plus
    /// the duration of that packet, which the next page's packets do not
    /// include. Unlike working back from the next page that states a
    /// granule position, this holds also when that page is the last one,
    /// whose granule position may cut its packets short.
    fn first_after(&mut self, anchor: Anchor) -> Result<i64, OpusError> {
        self.window.move_to(anchor.start);
        let Some(page) = ogg::read_page(&mut self.window, &mut self.reader)? else {
            return Ok(anchor.granule);
        };
        let bytes = &self.window.available()[..page.len];
        let segments = usize::from(bytes[26]);
        let lacing = &bytes[HEADER_LEN..HEADER_LEN + segments];
        let Some(last_end) = lacing.iter().rposition(|&value| value < 255) else {
            return Ok(anchor.granule);
        };
        if last_end + 1 == segments {
            return Ok(anchor.granule);
        }
        let body = &bytes[HEADER_LEN + segments..];
        let open: usize = lacing[..=last_end]
            .iter()
            .map(|&value| usize::from(value))
            .sum();
        let samples = packet_samples(&body[open..]).unwrap_or(0) as i64;
        Ok(anchor.granule.saturating_add(samples))
    }

    /// Reads packets from the page at `offset`, the first of which starts
    /// at index `first` of the stream, up to the one that holds index
    /// `start`, which is kept for the next read.
    fn walk(&mut self, offset: u64, first: i64, start: i64) -> Result<(), OpusError> {
        self.window.move_to(offset);
        self.packets.restart();
        self.decoder.reset();
        self.block_start = first;
        self.block_len = 0;
        self.pending = None;
        let mut index = first;
        loop {
            if self
                .packets
                .next_packet(&mut self.window, &mut self.reader, true)?
                .is_none()
            {
                self.next_start = index;
                return Ok(());
            }
            let samples = packet_samples(self.packets.packet()).unwrap_or(0) as i64;
            if index + samples > start || index >= start {
                self.pending = Some(self.packets.packet().to_vec());
                self.next_start = index;
                self.block_start = index;
                return Ok(());
            }
            index += samples;
        }
    }

    /// Finds the first page of the stream in `from..limit` that states a
    /// granule position.
    fn find_anchor(&mut self, mut from: u64, limit: u64) -> Result<Option<Anchor>, OpusError> {
        loop {
            let Some(page) = ogg::find_page(&mut self.window, &mut self.reader, from, limit)?
            else {
                return Ok(None);
            };
            let offset = self.window.file_position();
            if page.serial == self.packets.serial() && page.granule != NO_GRANULE {
                let granule = i64::try_from(page.granule).unwrap_or(i64::MAX);
                return Ok(Some(Anchor {
                    start: offset,
                    end: offset + page.len as u64,
                    granule,
                }));
            }
            from = offset + 1;
        }
    }

    /// Reads on from the page at `start` to the first page that states a
    /// granule position, and works back from it to the granule position of
    /// the first sample of the first whole packet read, and whether that
    /// page ends the stream.
    fn anchor(&mut self, start: u64) -> Result<Option<(i64, bool)>, OpusError> {
        self.window.move_to(start);
        self.packets.restart();
        let mut samples = 0i64;
        let mut count = 0;
        loop {
            let Some(info) = self
                .packets
                .next_packet(&mut self.window, &mut self.reader, true)?
            else {
                return Ok(None);
            };
            samples += packet_samples(self.packets.packet()).unwrap_or(0) as i64;
            if let Some(granule) = info.granule {
                let granule = i64::try_from(granule).unwrap_or(i64::MAX);
                return Ok(Some((granule.saturating_sub(samples), info.last)));
            }
            count += 1;
            if count > ANCHOR_LIMIT {
                return Err(OpusError::Malformed("no granule position where one is due"));
            }
        }
    }

    /// The granule position of the last page that states one.
    fn last_granule(&mut self) -> Result<Option<i64>, OpusError> {
        let mut step = READ_CHUNK as u64;
        let mut end = self.stream_len;
        while end > self.audio_start {
            let from = end.saturating_sub(step).max(self.audio_start);
            let mut last = None;
            let mut at = from;
            while let Some(anchor) = self.find_anchor(at, end)? {
                last = Some(anchor.granule);
                at = anchor.end;
            }
            if last.is_some() {
                return Ok(last);
            }
            end = from;
            step = step.saturating_mul(2);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &[u8] = include_bytes!("../../tests/data/opus-celt-2ch-20ms-128k.opus");
    const SILK: &[u8] = include_bytes!("../../tests/data/opus-silk-1ch-nb-20ms.opus");

    fn whole(bytes: &[u8]) -> AudioBuffer<f32> {
        let mut reader = OpusReader::new(Cursor::new(bytes)).unwrap();
        let mut out = AudioBuffer::new(reader.num_channels(), reader.frames().unwrap() as usize);
        assert_eq!(reader.read(&mut out).unwrap(), out.num_frames());
        out
    }

    /// Seeks to `at` and reads `len` frames: the largest difference from
    /// `whole`.
    fn difference(
        reader: &mut OpusReader<Cursor<&[u8]>>,
        whole: &AudioBuffer<f32>,
        at: u64,
        len: usize,
    ) -> f32 {
        assert_eq!(reader.seek(at).unwrap(), at);
        let mut block = AudioBuffer::new(whole.num_channels(), len);
        let got = reader.read(&mut block).unwrap();
        let mut worst = 0.0f32;
        for channel in 0..whole.num_channels() {
            for i in 0..got {
                let want = whole.channel(channel)[at as usize + i];
                worst = worst.max((block.channel(channel)[i] - want).abs());
            }
        }
        worst
    }

    use std::io::Cursor;

    /// The same stream with its audio packets `per_page` to a page, the
    /// last page keeping the granule position that trims the end.
    fn repaged(bytes: &[u8], per_page: usize) -> Vec<u8> {
        let mut packets = Vec::new();
        let mut packet = Vec::new();
        let mut at = 0;
        let mut last_granule = 0i64;
        while at + 27 <= bytes.len() {
            let segments = usize::from(bytes[at + 26]);
            last_granule = i64::from_le_bytes(bytes[at + 6..at + 14].try_into().unwrap());
            let mut body = at + 27 + segments;
            for &lacing in &bytes[at + 27..at + 27 + segments] {
                packet.extend_from_slice(&bytes[body..body + usize::from(lacing)]);
                body += usize::from(lacing);
                if lacing < 255 {
                    packets.push(std::mem::take(&mut packet));
                }
            }
            at = body;
        }
        let page = |sequence: u32, granule: i64, flags: u8, group: &[Vec<u8>]| -> Vec<u8> {
            let mut lacing = Vec::new();
            let mut body = Vec::new();
            for packet in group {
                let mut left = packet.len();
                while left >= 255 {
                    lacing.push(255u8);
                    left -= 255;
                }
                lacing.push(left as u8);
                body.extend_from_slice(packet);
            }
            let mut out = b"OggS\0".to_vec();
            out.push(flags);
            out.extend_from_slice(&granule.to_le_bytes());
            out.extend_from_slice(&7u32.to_le_bytes());
            out.extend_from_slice(&sequence.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.push(lacing.len() as u8);
            out.extend_from_slice(&lacing);
            out.extend_from_slice(&body);
            let crc = ogg::crc32(0, &out);
            out[22..26].copy_from_slice(&crc.to_le_bytes());
            out
        };
        let mut out = page(0, 0, FIRST, &packets[..1]);
        out.extend(page(1, 0, 0, &packets[1..2]));
        let groups: Vec<&[Vec<u8>]> = packets[2..].chunks(per_page).collect();
        let mut granule = 0i64;
        for (index, group) in groups.iter().enumerate() {
            granule += group
                .iter()
                .map(|packet| packet_samples(packet).unwrap() as i64)
                .sum::<i64>();
            let last = index + 1 == groups.len();
            let stated = if last { last_granule } else { granule };
            out.extend(page(
                index as u32 + 2,
                stated,
                if last { 4 } else { 0 },
                group,
            ));
        }
        out
    }

    /// With the distances of a long file shrunk to fit a short one, every
    /// way a seek can go is taken: on from where the reader stands, from a
    /// snapshot, from the start, and from a pre-roll.
    #[test]
    fn seeks_go_every_way_and_land_where_asked() {
        for original in [FILE, SILK] {
            let stream = repaged(original, 3);
            let bytes = stream.as_slice();
            let whole = whole(bytes);
            assert_eq!(whole.channel(0), self::whole(original).channel(0));
            let len = whole.num_frames() as u64;
            let mut reader = OpusReader::new(Cursor::new(bytes)).unwrap();
            reader.snapshot_interval = 4_800;
            reader.next_snapshot = 4_800;
            reader.exact_reach = 2_400;
            reader.preroll = 3_840;
            reader.max_snapshots = 4;
            reader.search_span = 256;

            // Far from anything decoded: a pre-roll, which may not be exact
            // but lands in the right place.
            let far = len - 2_000;
            let off = difference(&mut reader, &whole, far, 2_000);
            assert!(!reader.exact);
            assert!(off < 0.5, "{off}");
            // Near the start: exact, from the start.
            assert_eq!(difference(&mut reader, &whole, 1_000, 2_000), 0.0);
            assert!(reader.exact);
            // On through the file: snapshots, thinned as they pass four.
            let mut block = AudioBuffer::new(whole.num_channels(), 3_000);
            while reader.read(&mut block).unwrap() > 0 {}
            assert!(!reader.snapshots.is_empty() && reader.snapshots.len() <= 4);
            assert!(reader.snapshot_interval > 4_800);
            for pair in reader.snapshots.windows(2) {
                assert!(pair[0].index < pair[1].index);
            }
            // Now every seek is exact, from a snapshot or on from here.
            let mut state = 0x5EEDu64;
            for _ in 0..40 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let at = state % len.saturating_sub(10);
                assert_eq!(
                    difference(&mut reader, &whole, at, 900),
                    0.0,
                    "seek to {at}"
                );
            }
        }
    }

    #[test]
    fn the_first_packet_after_a_page_is_found_without_the_page_after() {
        let mut reader = OpusReader::new(Cursor::new(FILE)).unwrap();
        // Every page of audio: the granule position where the next one's
        // first whole packet starts, worked out from the page itself, is
        // what reading on from it says.
        let mut from = reader.audio_start;
        while let Some(anchor) = reader.find_anchor(from, reader.stream_len).unwrap() {
            let first = reader.first_after(anchor).unwrap();
            if let Some((next, last)) = reader.anchor(anchor.end).unwrap() {
                if !last {
                    assert_eq!(first, next, "page at {}", anchor.start);
                }
            }
            from = anchor.end;
        }
    }
}
