//! Ogg, the container Vorbis travels in, as RFC 3533 defines it.
//!
//! An Ogg stream is a run of pages. Each page starts with the capture
//! pattern `OggS` and a header that carries a checksum over the whole page,
//! the serial number of the logical stream it belongs to, its number in that
//! stream, and a granule position: for Vorbis, the number of samples that
//! have been returned once the last packet that ends on the page has been
//! decoded. The body is cut into segments of up to 255 bytes; a segment
//! shorter than 255 bytes ends a packet, so a packet may run across pages.
//!
//! This module reads pages out of a [`Window`] of the file and puts the
//! packets of one logical stream back together.

use std::io::{self, Read, Seek};

use crate::window::Window;

/// The four bytes every page starts with.
pub(crate) const CAPTURE: &[u8; 4] = b"OggS";

/// Bytes in a page header before its segment table.
pub(crate) const HEADER_LEN: usize = 27;

/// The page continues a packet begun on an earlier page.
pub(crate) const CONTINUED: u8 = 0x01;
/// The page is the first of its logical stream.
pub(crate) const FIRST: u8 = 0x02;
/// The page is the last of its logical stream.
pub(crate) const LAST: u8 = 0x04;

/// The granule position of a page on which no packet ends.
pub(crate) const NO_GRANULE: u64 = u64::MAX;

/// One step of the checksum for every possible byte: CRC-32 with the
/// polynomial 0x04C11DB7, most significant bit first, starting from zero
/// and with nothing added at the end, as RFC 3533 defines it.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = (byte as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};

/// Carries the page checksum on over `bytes`.
pub(crate) fn crc32(crc: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(crc, |crc, &byte| {
        (crc << 8) ^ CRC_TABLE[usize::from((crc >> 24) as u8 ^ byte)]
    })
}

/// What the header of a page says.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PageInfo {
    pub(crate) flags: u8,
    /// [`NO_GRANULE`] when no packet ends on the page.
    pub(crate) granule: u64,
    pub(crate) serial: u32,
    pub(crate) sequence: u32,
    /// Bytes the whole page takes, header included.
    pub(crate) len: usize,
}

/// What [`parse_page`] made of the bytes it was given.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Parsed {
    /// A whole page whose checksum matches.
    Page(PageInfo),
    /// The bytes start like a page, but this many are needed to hold it.
    NeedMore(usize),
    /// The bytes are not the start of a page.
    NotPage,
    /// A page whose checksum does not match.
    Corrupt,
}

/// Parses the page at the start of `bytes`, checksum and all.
pub(crate) fn parse_page(bytes: &[u8]) -> Parsed {
    let prefix = bytes.len().min(5);
    if bytes[..prefix] != b"OggS\0"[..prefix] {
        return Parsed::NotPage;
    }
    if bytes.len() < HEADER_LEN {
        return Parsed::NeedMore(HEADER_LEN);
    }
    let segments = usize::from(bytes[26]);
    let header_len = HEADER_LEN + segments;
    if bytes.len() < header_len {
        return Parsed::NeedMore(header_len);
    }
    let body_len: usize = bytes[HEADER_LEN..header_len]
        .iter()
        .map(|&lacing| usize::from(lacing))
        .sum();
    let len = header_len + body_len;
    if bytes.len() < len {
        return Parsed::NeedMore(len);
    }
    // The checksum is worked out with its own field taken as zero.
    let stored = u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    let mut crc = crc32(0, &bytes[..22]);
    crc = crc32(crc, &[0; 4]);
    crc = crc32(crc, &bytes[26..len]);
    if crc != stored {
        return Parsed::Corrupt;
    }
    let field =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Parsed::Page(PageInfo {
        flags: bytes[5],
        granule: u64::from(field(6)) | (u64::from(field(10)) << 32),
        serial: field(14),
        sequence: field(18),
        len,
    })
}

/// Why the next packet could not be had.
#[derive(Debug)]
pub(crate) enum PacketError {
    Io(io::Error),
    /// A page whose checksum does not match.
    Corrupt,
    /// Something that is not a page where a page of the stream should be,
    /// a page missing from the stream, or a packet too long to be one.
    Malformed(&'static str),
}

impl From<io::Error> for PacketError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// What came with a packet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PacketInfo {
    /// The granule position of the page the packet ends on, if it is the
    /// last packet to end there.
    pub(crate) granule: Option<u64>,
    /// Whether the packet ends the logical stream.
    pub(crate) last: bool,
}

/// Puts back together the packets of one logical stream.
///
/// Pages of other logical streams are stepped over. The bytes of the packet
/// handed out last are in [`packet`](Self::packet).
pub(crate) struct PacketReader {
    serial: u32,
    /// Packets longer than this are refused.
    max_packet: usize,
    /// The body of the page being taken apart, and its segment table.
    body: Vec<u8>,
    lacing: [u8; 255],
    segments: usize,
    /// The next segment of the page to take, and where it is in `body`.
    next_segment: usize,
    next_byte: usize,
    /// The last segment of the page that ends a packet.
    last_end: Option<usize>,
    page: PageInfo,
    /// The sequence number the next page of the stream must carry, once
    /// one page has been read.
    expected_sequence: Option<u32>,
    /// The packet being put together, or the one handed out last.
    packet: Vec<u8>,
    /// Bytes in that packet, kept or not.
    len: usize,
    /// Whether `packet` holds the start of a packet that has not ended yet.
    open: bool,
    /// Whether the stream has ended: a page marked last has been used up.
    ended: bool,
}

impl PacketReader {
    /// A reader of the packets of the stream `serial`, refusing any longer
    /// than `max_packet` bytes.
    pub(crate) fn new(serial: u32, max_packet: usize) -> Self {
        Self {
            serial,
            max_packet,
            body: Vec::new(),
            lacing: [0; 255],
            segments: 0,
            next_segment: 0,
            next_byte: 0,
            last_end: None,
            page: PageInfo {
                flags: 0,
                granule: NO_GRANULE,
                serial,
                sequence: 0,
                len: 0,
            },
            expected_sequence: None,
            packet: Vec::new(),
            len: 0,
            open: false,
            ended: false,
        }
    }

    /// The bytes of the packet handed out last.
    pub(crate) fn packet(&self) -> &[u8] {
        &self.packet
    }

    /// Changes the longest packet accepted.
    pub(crate) fn set_max_packet(&mut self, max_packet: usize) {
        self.max_packet = max_packet;
    }

    /// Forgets everything, so that the next page read is taken as a fresh
    /// start: a packet that it continues is dropped. Used after the window
    /// has been moved to the start of a page.
    pub(crate) fn restart(&mut self) {
        self.segments = 0;
        self.next_segment = 0;
        self.next_byte = 0;
        self.last_end = None;
        self.expected_sequence = None;
        self.packet.clear();
        self.len = 0;
        self.open = false;
        self.ended = false;
    }

    /// The serial number of the stream.
    pub(crate) fn serial(&self) -> u32 {
        self.serial
    }

    /// Whether the packet handed out last was the last on its page.
    pub(crate) fn at_page_end(&self) -> bool {
        self.next_segment == self.segments
    }

    /// Gives back the memory a long packet took.
    pub(crate) fn shrink(&mut self) {
        self.packet = Vec::new();
    }

    /// Takes the next packet of the stream, reading pages from `window` as
    /// it needs them. `Ok(None)` is the end of the stream or of the file.
    ///
    /// With `keep` unset the packet's bytes are counted but not kept, so
    /// that a packet of no interest costs no memory however long it is.
    ///
    /// A failure to read a page loses nothing: the window stays at that
    /// page, so that trying again tries it again. A packet longer than the
    /// limit is read to its end and then refused.
    pub(crate) fn next_packet<R: Read + Seek>(
        &mut self,
        window: &mut Window,
        reader: &mut R,
        keep: bool,
    ) -> Result<Option<PacketInfo>, PacketError> {
        if !self.open {
            self.packet.clear();
            self.len = 0;
        }
        loop {
            if self.next_segment == self.segments {
                if self.ended || !self.next_page(window, reader)? {
                    return Ok(None);
                }
                continue;
            }
            let lacing = usize::from(self.lacing[self.next_segment]);
            let index = self.next_segment;
            self.next_segment += 1;
            let bytes = &self.body[self.next_byte..self.next_byte + lacing];
            self.next_byte += lacing;
            self.len = self.len.saturating_add(lacing);
            if keep && self.len <= self.max_packet {
                self.packet.extend_from_slice(bytes);
            }
            self.open = true;
            if lacing < 255 {
                self.open = false;
                if keep && self.len > self.max_packet {
                    return Err(PacketError::Malformed("packet too long"));
                }
                let is_last_end = self.last_end == Some(index);
                return Ok(Some(PacketInfo {
                    granule: (is_last_end && self.page.granule != NO_GRANULE)
                        .then_some(self.page.granule),
                    last: is_last_end && self.page.flags & LAST != 0,
                }));
            }
        }
    }

    /// Reads the next page of the stream into the reader. Returns `false`
    /// at the end of the file.
    fn next_page<R: Read + Seek>(
        &mut self,
        window: &mut Window,
        reader: &mut R,
    ) -> Result<bool, PacketError> {
        loop {
            let Some(page) = read_page(window, reader)? else {
                return Ok(false);
            };
            let bytes = &window.available()[..page.len];
            if page.serial != self.serial {
                window.position += page.len;
                continue;
            }
            if let Some(expected) = self.expected_sequence {
                if page.sequence != expected {
                    return Err(PacketError::Malformed("a page of the stream is missing"));
                }
            }
            let continued = page.flags & CONTINUED != 0;
            if self.open && !continued {
                return Err(PacketError::Malformed("a packet breaks off before its end"));
            }
            let segments = usize::from(bytes[26]);
            self.lacing[..segments].copy_from_slice(&bytes[HEADER_LEN..HEADER_LEN + segments]);
            self.body.clear();
            self.body
                .extend_from_slice(&bytes[HEADER_LEN + segments..page.len]);
            self.segments = segments;
            self.next_segment = 0;
            self.next_byte = 0;
            self.last_end = self.lacing[..segments]
                .iter()
                .rposition(|&lacing| lacing < 255);
            self.page = page;
            self.expected_sequence = Some(page.sequence.wrapping_add(1));
            self.ended = page.flags & LAST != 0;
            window.position += page.len;

            // The first page read after a fresh start may carry the end of a
            // packet whose start is not at hand: it is dropped.
            if continued && !self.open {
                while self.next_segment < self.segments {
                    let lacing = usize::from(self.lacing[self.next_segment]);
                    self.next_segment += 1;
                    self.next_byte += lacing;
                    if lacing < 255 {
                        break;
                    }
                }
            }
            return Ok(true);
        }
    }
}

/// Makes the page at the window's position available whole and checks it.
///
/// `Ok(None)` is the end of the file, or a page that the file ends in the
/// middle of. Anything else that is not a whole page with a matching
/// checksum is an error, and the window stays where it was.
pub(crate) fn read_page<R: Read + Seek>(
    window: &mut Window,
    reader: &mut R,
) -> Result<Option<PageInfo>, PacketError> {
    let mut need = HEADER_LEN;
    loop {
        window.fill(reader, need)?;
        let available = window.available();
        if available.is_empty() {
            return Ok(None);
        }
        match parse_page(available) {
            Parsed::Page(page) => return Ok(Some(page)),
            Parsed::NeedMore(more) if window.exhausted && available.len() < more => {
                // A page cut short by the end of the file.
                return Ok(None);
            }
            Parsed::NeedMore(more) => need = more,
            Parsed::NotPage => return Err(PacketError::Malformed("expected an Ogg page")),
            Parsed::Corrupt => return Err(PacketError::Corrupt),
        }
    }
}

/// Finds the first whole page with a matching checksum that starts in
/// `from..limit`, and leaves the window at it.
pub(crate) fn find_page<R: Read + Seek>(
    window: &mut Window,
    reader: &mut R,
    from: u64,
    limit: u64,
) -> io::Result<Option<PageInfo>> {
    window.move_to(from);
    loop {
        window.fill(reader, HEADER_LEN)?;
        let offset = window.file_position();
        let available = window.available();
        if offset >= limit || available.len() < HEADER_LEN {
            return Ok(None);
        }
        let span = usize::try_from(limit - offset)
            .unwrap_or(usize::MAX)
            .min(available.len() - 3);
        match available[..span + 3]
            .windows(4)
            .position(|bytes| bytes == CAPTURE)
        {
            None => window.position += span,
            Some(index) => {
                window.position += index;
                // Only as much of the file is read as the page needs.
                let mut need = HEADER_LEN;
                loop {
                    window.fill(reader, need)?;
                    match parse_page(window.available()) {
                        Parsed::Page(page) => return Ok(Some(page)),
                        Parsed::NeedMore(more)
                            if more > window.available().len() && !window.exhausted =>
                        {
                            need = more;
                        }
                        _ => break,
                    }
                }
                window.position += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_matches_a_known_value() {
        // The standard check string, under CRC-32/POSIX less its final
        // inversion: the same polynomial, starting from zero.
        assert_eq!(crc32(0, b"123456789"), 0x89A1_897F);
    }
}
