//! Decoding an Ogg Vorbis stream block by block, from any position.
//!
//! Positions are granule positions, which in Vorbis count samples. The
//! samples a packet finishes run from the middle of the block before it to
//! the middle of its own, so a packet's share is a quarter of each of the
//! two block sizes, and each page says how many samples have been finished
//! once its last packet has been decoded. Those two facts are all a seek
//! needs: a page found by bisection gives a granule position, the block
//! sizes of the packets after it carry it on, and decoding starts one
//! packet before the one that holds the target, which is all the overlap
//! of the transform needs to give back exactly the samples a decode from
//! the start gives.

use std::io::{Read, Seek, SeekFrom};

use manguebyte_core::AudioBuffer;

use super::VorbisError;
use super::decoder::Decoder;
use super::setup::{self, Comments};
use crate::ogg::{self, FIRST, HEADER_LEN, NO_GRANULE, PacketError, PacketReader};
use crate::window::Window;

/// The longest identification and setup headers accepted. libvorbis writes
/// a few kilobytes of setup.
const SETUP_LIMIT: usize = 4 << 20;

/// The longest comment header kept when the comments are wanted. Pictures
/// can be carried in comments, which makes them the longest packets there
/// are.
const COMMENT_LIMIT: usize = 64 << 20;

/// How many packets a seek may read past a page before it meets one that
/// says where it is. A page holds at most 255 packets.
const ANCHOR_LIMIT: usize = 4_096;

/// How many bytes are read from the file at a time: a few pages. A search
/// for a position reads this much for each page it looks at.
const READ_CHUNK: usize = 16 * 1_024;

/// How close the search for a page narrows in before it reads its way on
/// to the target. Reading on costs a look at the first bits of each packet,
/// not their decoding.
const SEARCH_SPAN: u64 = READ_CHUNK as u64;

impl From<PacketError> for VorbisError {
    fn from(error: PacketError) -> Self {
        match error {
            PacketError::Io(error) => Self::Io(error),
            PacketError::Corrupt => Self::Corrupt,
            PacketError::Malformed(what) => Self::Malformed(what),
        }
    }
}

/// A page the search for a position found: where it ends in the file and
/// the granule position it states.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    end: u64,
    granule: i64,
}

/// What happened when a seek went looking for its target.
enum Walk {
    /// It is there.
    Found,
    /// The stream ends first.
    End,
    /// The place the walk started from is already past the target, which
    /// only granule positions that contradict each other can bring about.
    TooLate,
}

/// Decodes an Ogg Vorbis stream block by block, from any position.
///
/// Memory use does not depend on the length of the stream: the setup, a
/// window of the file of a page or two, one block per channel and the
/// overlap with the block before.
pub(crate) struct VorbisReader<R> {
    reader: R,
    window: Window,
    packets: PacketReader,
    decoder: Decoder,
    sample_rate: u32,
    /// For each channel handed out, the channel of the stream that it is.
    order: Vec<usize>,
    /// Offset of the first page of audio.
    audio_start: u64,
    stream_len: u64,
    long_block: usize,
    /// The granule position of the first sample that decoding from the
    /// start of the stream finishes. Below zero when the stream says to
    /// drop samples from its start.
    first_granule: i64,
    /// The granule position of frame 0.
    base: i64,
    /// The length of the audio in frames, when the stream says.
    total: Option<u64>,
    /// The frame the next read starts at.
    position: u64,
    /// The granule position of the first sample of the packet decoded
    /// last, and how many samples it finished.
    block_granule: i64,
    block_len: usize,
    /// The granule position the next packet's samples start at, once the
    /// decoder has a block to overlap them with.
    next_granule: Option<i64>,
    /// The granule position the samples after the first packet decoded
    /// from a fresh start begin at.
    prime_granule: i64,
    /// The last two packets a seek read, kept so that it can decode them
    /// once it knows the second holds its target.
    held: [Vec<u8>; 2],
    /// The block sizes a seek has met before it knows where it is.
    sizes: Vec<usize>,
    /// Set when a failed seek left the reader somewhere unknown. The next
    /// read then finds its place again.
    lost: bool,
    comments: Comments,
}

impl<R: Read + Seek> VorbisReader<R> {
    /// Reads the headers of the stream in `reader`, keeping the comments,
    /// and gets ready to decode from its first sample.
    pub(crate) fn with_comments(reader: R) -> Result<Self, VorbisError> {
        Self::open(reader, true)
    }

    fn open(mut reader: R, keep_comments: bool) -> Result<Self, VorbisError> {
        let stream_len = reader.seek(SeekFrom::End(0))?;
        let mut window = Window::with_chunk(0, stream_len, READ_CHUNK);

        // A file starts with the first page of each of its streams. The one
        // whose first packet is a Vorbis identification header is read.
        let serial = loop {
            let page = match ogg::read_page(&mut window, &mut reader) {
                Ok(Some(page)) => page,
                // A first page cut short, or no page that starts Vorbis.
                Ok(None) if window.file_position() == 0 && stream_len > 0 => {
                    return Err(VorbisError::Truncated);
                }
                Ok(None) => return Err(VorbisError::NotVorbis),
                Err(PacketError::Malformed(_)) => return Err(VorbisError::NotVorbis),
                Err(error) => return Err(error.into()),
            };
            if page.flags & FIRST == 0 {
                return Err(VorbisError::NotVorbis);
            }
            let bytes = &window.available()[..page.len];
            let body = &bytes[HEADER_LEN + usize::from(bytes[26])..];
            if body.starts_with(b"\x01vorbis") {
                break page.serial;
            }
            window.position += page.len;
        };

        let mut packets = PacketReader::new(serial, SETUP_LIMIT);
        let mut header = |packets: &mut PacketReader, keep: bool| -> Result<(), VorbisError> {
            match packets.next_packet(&mut window, &mut reader, keep)? {
                Some(_) => Ok(()),
                None => Err(VorbisError::Truncated),
            }
        };
        header(&mut packets, true)?;
        let ident = setup::identification(packets.packet())?;
        packets.set_max_packet(COMMENT_LIMIT);
        header(&mut packets, keep_comments)?;
        let comments = if keep_comments {
            setup::comments(packets.packet())?
        } else {
            Comments::default()
        };
        packets.set_max_packet(SETUP_LIMIT);
        header(&mut packets, true)?;
        let setup = setup::setup(packets.packet(), &ident)?;
        // The setup header ends its page, and the audio starts on the next.
        if !packets.at_page_end() {
            return Err(VorbisError::Malformed(
                "audio starts on the page of the setup header",
            ));
        }
        let audio_start = window.file_position();
        packets.shrink();
        let [_, long_block] = ident.block_sizes;
        packets.set_max_packet((64 << 10) + ident.channels * long_block);

        let order = channel_order(ident.channels);
        let decoder = Decoder::new(ident, setup);
        let mut this = Self {
            reader,
            window,
            packets,
            decoder,
            sample_rate: ident.sample_rate,
            order,
            audio_start,
            stream_len,
            long_block,
            first_granule: 0,
            base: 0,
            total: None,
            position: 0,
            block_granule: 0,
            block_len: 0,
            next_granule: None,
            prime_granule: 0,
            held: [Vec::new(), Vec::new()],
            sizes: Vec::new(),
            lost: false,
            comments,
        };

        // Where the samples start: worked back from the first page that says
        // how many there are. A first page that is also the last says where
        // the audio ends, not where it starts.
        this.first_granule = match this.anchor(audio_start)? {
            Some((granule, last)) if granule < 0 && last => 0,
            Some((granule, _)) => granule,
            None => 0,
        };
        this.base = this.first_granule.max(0);
        this.total = this
            .last_granule()?
            .map(|last| last.saturating_sub(this.base).max(0) as u64);
        this.restart();
        Ok(this)
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn num_channels(&self) -> usize {
        self.order.len()
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

    /// The vendor string and the comments, if they were kept.
    pub(crate) fn take_comments(&mut self) -> Comments {
        std::mem::take(&mut self.comments)
    }

    /// Goes back to the first page of audio, to decode from the start.
    fn restart(&mut self) {
        self.window.move_to(self.audio_start);
        self.packets.restart();
        self.decoder.reset();
        self.next_granule = None;
        self.prime_granule = self.first_granule;
        self.block_granule = self.first_granule;
        self.block_len = 0;
    }

    /// The granule position of the frame `position`.
    fn granule_of(&self, position: u64) -> i64 {
        self.base
            .saturating_add(position.min(i64::MAX as u64) as i64)
    }

    /// Fills `out` from the current position and returns how many frames
    /// were decoded: fewer than `out` holds only at the end of the stream.
    pub(crate) fn read(&mut self, out: &mut AudioBuffer<f32>) -> Result<usize, VorbisError> {
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
            let granule = self.granule_of(self.position);
            let block_end = self.block_granule.saturating_add(self.block_len as i64);
            if granule >= block_end {
                if !self.next_block()? {
                    break;
                }
                continue;
            }
            let count = ((block_end - granule) as usize)
                .min(want - done)
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            if granule < self.block_granule {
                // Samples the stream skips over, which only a stream whose
                // granule positions contradict its blocks has: silence.
                let gap = ((self.block_granule - granule) as usize).min(count);
                for channel in out.iter_channels_mut() {
                    channel[done..done + gap].fill(0.0);
                }
                done += gap;
                self.position += gap as u64;
                continue;
            }
            let from = (granule - self.block_granule) as usize;
            for (output, &channel) in self.order.iter().enumerate() {
                let source = &self.decoder.pcm(channel)[from..from + count];
                out.channel_mut(output)[done..done + count].copy_from_slice(source);
            }
            done += count;
            self.position += count as u64;
        }
        Ok(done)
    }

    /// Decodes the next packet that finishes samples. Returns `false` at
    /// the end of the stream.
    fn next_block(&mut self) -> Result<bool, VorbisError> {
        loop {
            if self
                .packets
                .next_packet(&mut self.window, &mut self.reader, true)?
                .is_none()
            {
                return Ok(false);
            }
            let Some(count) = self.decoder.decode(self.packets.packet())? else {
                continue;
            };
            match self.next_granule {
                None => {
                    // The first block after a fresh start only begins the
                    // overlap.
                    self.next_granule = Some(self.prime_granule);
                    self.block_granule = self.prime_granule;
                    self.block_len = 0;
                }
                Some(granule) => {
                    self.block_granule = granule;
                    self.block_len = count;
                    self.next_granule = Some(granule.saturating_add(count as i64));
                    return Ok(true);
                }
            }
        }
    }

    /// Moves to `frame`, or to the end if the stream is shorter, and returns
    /// where it landed.
    ///
    /// If this fails, the position is unchanged.
    pub(crate) fn seek(&mut self, frame: u64) -> Result<u64, VorbisError> {
        let target = self.total.map_or(frame, |total| frame.min(total));
        if !self.lost {
            let granule = self.granule_of(target);
            let block_end = self.block_granule.saturating_add(self.block_len as i64);
            // Within the packet at hand, or just past it where reading
            // would carry on anyway: nothing to look for.
            if (self.block_granule..=block_end).contains(&granule) {
                self.position = target;
                return Ok(target);
            }
        }
        self.reposition(target)
    }

    /// Moves to `target` by a search of the file. On failure the position
    /// is put back, and the reader is marked as lost so that the next read
    /// finds its place again.
    fn reposition(&mut self, target: u64) -> Result<u64, VorbisError> {
        let previous = self.position;
        match self.search(target) {
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

    /// Finds the packet that holds `target` and decodes up to it.
    fn search(&mut self, target: u64) -> Result<u64, VorbisError> {
        let granule = self.granule_of(target);
        // Decoding can start right after a page whose granule position is
        // at least a long block before the target: at most two packets are
        // spent beginning the overlap, the one the next page continues and
        // the one after it, and each finishes at most half a long block.
        let margin = self.long_block as i64;
        let mut lower: Option<Anchor> = None;
        let mut upper = self.stream_len;
        loop {
            let from = lower.map_or(self.audio_start, |anchor| anchor.end);
            // Close enough to walk: reading pages costs little next to
            // decoding them, and a walk decodes only two packets.
            if upper.saturating_sub(from) <= SEARCH_SPAN {
                break;
            }
            let middle = from + (upper - from) / 2;
            match self.find_anchor(middle, upper)? {
                Some(found)
                    if found.granule <= granule.saturating_sub(margin)
                        && lower.is_none_or(|lower| found.granule >= lower.granule) =>
                {
                    lower = Some(found);
                }
                _ => upper = middle,
            }
        }

        // Read on from the page found, or from the start. A damaged page on
        // the way is stepped over: reading starts again at the next page
        // that is whole, so that a seek fails only if the packets it is
        // aimed at cannot be read, not because of damage somewhere before
        // them.
        let mut start = lower.map(|anchor| anchor.end);
        let mut damage = None;
        loop {
            let walked = match start {
                None => self.walk(self.audio_start, self.first_granule, granule),
                Some(offset) => match self.anchor(offset) {
                    // Where the packets after the page start counting from.
                    Ok(Some((first, _))) => self.walk(offset, first, granule),
                    // No page after it says where it is: the stream ends.
                    Ok(None) => {
                        if let Some(error) = damage.take() {
                            return Err(error);
                        }
                        if let Some(anchor) = lower {
                            self.block_granule = anchor.granule;
                        }
                        self.block_len = 0;
                        self.next_granule = None;
                        Ok(Walk::End)
                    }
                    Err(error) => Err(error),
                },
            };
            match walked {
                Ok(Walk::Found | Walk::End) => return Ok(self.landed(target)),
                Ok(Walk::TooLate) => {
                    // The packets the target is in are the damaged ones.
                    if let Some(error) = damage.take() {
                        return Err(error);
                    }
                    if start.is_none() {
                        return Ok(self.landed(target));
                    }
                    start = None;
                }
                Err(error @ (VorbisError::Corrupt | VorbisError::Malformed(_))) => {
                    let after = self.window.file_position() + 1;
                    match self.find_stream_page(after)? {
                        Some(next) if start.is_none_or(|start| next > start) => {
                            start = Some(next);
                            damage = Some(error);
                        }
                        _ => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Where the first whole page of the stream at or after `from` starts.
    fn find_stream_page(&mut self, mut from: u64) -> Result<Option<u64>, VorbisError> {
        while let Some(page) =
            ogg::find_page(&mut self.window, &mut self.reader, from, self.stream_len)?
        {
            let offset = self.window.file_position();
            if page.serial == self.packets.serial() {
                return Ok(Some(offset));
            }
            from = offset + 1;
        }
        Ok(None)
    }

    /// Where a seek to `target` has landed once its packet is at hand: the
    /// target, unless the stream ended first.
    fn landed(&self, target: u64) -> u64 {
        let end = self.block_granule.saturating_add(self.block_len as i64);
        let granule = self.granule_of(target);
        if granule <= end {
            target
        } else {
            (end - self.base).max(0) as u64
        }
    }

    /// Finds the first page of the stream that starts in `from..limit` and
    /// states a granule position.
    fn find_anchor(&mut self, mut from: u64, limit: u64) -> Result<Option<Anchor>, VorbisError> {
        loop {
            let Some(page) = ogg::find_page(&mut self.window, &mut self.reader, from, limit)?
            else {
                return Ok(None);
            };
            let offset = self.window.file_position();
            if page.serial == self.packets.serial() && page.granule != NO_GRANULE {
                // A granule position past what 63 bits hold is no use.
                let granule = i64::try_from(page.granule).unwrap_or(i64::MAX);
                return Ok(Some(Anchor {
                    end: offset + page.len as u64,
                    granule,
                }));
            }
            from = offset + 1;
        }
    }

    /// Reads on from the page at `start` to the first page that states a
    /// granule position, and works back from it: returns the granule
    /// position at which the samples after the first whole packet read
    /// begin, and whether that page ends the stream. `None` if no such page
    /// comes before the stream ends.
    ///
    /// The first whole packet is the first that starts at `start` or after
    /// it: one that a page at `start` continues is not whole.
    fn anchor(&mut self, start: u64) -> Result<Option<(i64, bool)>, VorbisError> {
        self.window.move_to(start);
        self.packets.restart();
        self.sizes.clear();
        loop {
            let Some(info) = self
                .packets
                .next_packet(&mut self.window, &mut self.reader, true)?
            else {
                return Ok(None);
            };
            let Some(size) = self.decoder.block_size(self.packets.packet()) else {
                continue;
            };
            self.sizes.push(size);
            if let Some(granule) = info.granule {
                let mut granule = i64::try_from(granule).unwrap_or(i64::MAX);
                for pair in self.sizes.windows(2).rev() {
                    granule = granule.saturating_sub((pair[0] / 4 + pair[1] / 4) as i64);
                }
                return Ok(Some((granule, info.last)));
            }
            if self.sizes.len() > ANCHOR_LIMIT {
                return Err(VorbisError::Malformed(
                    "no granule position where one is due",
                ));
            }
        }
    }

    /// Reads packets from the page at `start`, the samples after the first
    /// of them beginning at granule position `first`, until the one whose
    /// samples hold `target`; then decodes it and the packet before it.
    ///
    /// Only block sizes are looked at on the way, so a walk costs two
    /// packets' decoding however far it goes.
    fn walk(&mut self, start: u64, first: i64, target: i64) -> Result<Walk, VorbisError> {
        self.window.move_to(start);
        self.packets.restart();
        self.decoder.reset();
        self.next_granule = None;
        self.block_granule = first;
        self.block_len = 0;
        // The granule position after the packet held in `held[0]`, and its
        // block size.
        let mut previous: Option<(usize, i64)> = None;
        loop {
            let Some(_) = self
                .packets
                .next_packet(&mut self.window, &mut self.reader, true)?
            else {
                // The stream ends before the target. Decode what was held,
                // so that the reader stands at the end.
                if let Some((_, granule)) = previous {
                    self.block_granule = granule;
                    self.next_granule = None;
                }
                return Ok(Walk::End);
            };
            let packet = self.packets.packet();
            let Some(size) = self.decoder.block_size(packet) else {
                continue;
            };
            let Some((previous_size, granule)) = previous else {
                if target < first {
                    return Ok(Walk::TooLate);
                }
                previous = Some((size, first));
                self.held[0].clear();
                self.held[0].extend_from_slice(packet);
                continue;
            };
            let count = (previous_size / 4 + size / 4) as i64;
            if target < granule.saturating_add(count) {
                // Decode the packet before, to begin the overlap, and then
                // this one.
                self.held[1].clear();
                self.held[1].extend_from_slice(packet);
                self.decoder.decode(&self.held[0])?;
                let finished = self.decoder.decode(&self.held[1])?.unwrap_or(0);
                self.block_granule = granule;
                self.block_len = finished;
                self.next_granule = Some(granule.saturating_add(finished as i64));
                return Ok(Walk::Found);
            }
            previous = Some((size, granule.saturating_add(count)));
            self.held[0].clear();
            self.held[0].extend_from_slice(packet);
        }
    }

    /// The granule position of the last page of the stream that states
    /// one, found by reading the file backwards from its end.
    fn last_granule(&mut self) -> Result<Option<i64>, VorbisError> {
        // The last page is usually a few kilobytes from the end; the stretch
        // looked at grows until it holds one.
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

/// For each channel handed out, the channel of the stream it is: the
/// order of section 4.3.9 of the specification turned into the order of
/// WAV and FLAC.
pub(crate) fn channel_order(channels: usize) -> Vec<usize> {
    match channels {
        // Left, centre, right.
        3 => vec![0, 2, 1],
        // Front left, centre, front right, rear left, rear right.
        5 => vec![0, 2, 1, 3, 4],
        // The same and LFE.
        6 => vec![0, 2, 1, 5, 3, 4],
        // Front left, centre, front right, side left, side right, rear
        // centre, LFE.
        7 => vec![0, 2, 1, 6, 5, 3, 4],
        // Front left, centre, front right, side left, side right, rear
        // left, rear right, LFE.
        8 => vec![0, 2, 1, 7, 5, 6, 3, 4],
        _ => (0..channels).collect(),
    }
}
