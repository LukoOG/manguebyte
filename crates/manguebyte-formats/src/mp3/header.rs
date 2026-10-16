//! Frame headers (ISO/IEC 11172-3, 2.4.1.3), and the tags that come before
//! and among the frames of a file: ID3v2 at the start, and the Xing, Info,
//! LAME and VBRI headers that encoders put in a first frame of silence.

use super::tables::{BITRATES, SAMPLE_RATES};

/// Which standard a frame follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    /// ISO/IEC 11172-3: 32, 44.1 and 48 kHz.
    Mpeg1,
    /// ISO/IEC 13818-3, the lower sample frequencies: 16, 22.05, 24 kHz.
    Mpeg2,
    /// The same halved again, an extension in common use: 8 to 12 kHz.
    Mpeg25,
}

/// How a frame's two channels are coded, or that it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Stereo,
    JointStereo,
    DualChannel,
    Mono,
}

/// What the four bytes of a frame header say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    pub(crate) version: Version,
    /// 1, 2 or 3.
    pub(crate) layer: u8,
    /// Whether a CRC follows the header.
    pub(crate) protected: bool,
    /// In bits per second. Zero is free format, which states no bitrate.
    pub(crate) bitrate: u32,
    pub(crate) sample_rate: u32,
    /// Index of the sample rate among the nine, for the tables.
    pub(crate) rate_index: usize,
    pub(crate) padding: bool,
    pub(crate) mode: Mode,
    pub(crate) mode_extension: u8,
}

impl FrameHeader {
    /// Parses a frame header, or returns `None` for four bytes that are not
    /// one: no sync word, or a field with a value the standard reserves.
    pub(crate) fn parse(bytes: [u8; 4]) -> Option<Self> {
        let word = u32::from_be_bytes(bytes);
        if word >> 21 != 0x7FF {
            return None;
        }
        let version = match (word >> 19) & 3 {
            0 => Version::Mpeg25,
            2 => Version::Mpeg2,
            3 => Version::Mpeg1,
            _ => return None,
        };
        let layer = match (word >> 17) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let protected = (word >> 16) & 1 == 0;
        let bitrate_index = ((word >> 12) & 0xF) as usize;
        let rate_code = ((word >> 10) & 3) as usize;
        if bitrate_index == 15 || rate_code == 3 {
            return None;
        }
        // Reserved emphasis.
        if word & 3 == 2 {
            return None;
        }
        let (row, rates) = match version {
            Version::Mpeg1 => (0, 0),
            Version::Mpeg2 => (1, 1),
            Version::Mpeg25 => (1, 2),
        };
        // Only layer III bitrates are tabled; the layers this decoder does
        // not read are recognised as frames and refused as a whole.
        let bitrate = u32::from(BITRATES[row][bitrate_index]) * 1_000;
        let mode = match (word >> 6) & 3 {
            0 => Mode::Stereo,
            1 => Mode::JointStereo,
            2 => Mode::DualChannel,
            _ => Mode::Mono,
        };
        Some(Self {
            version,
            layer,
            protected,
            bitrate,
            sample_rate: SAMPLE_RATES[rates][rate_code],
            rate_index: rates * 3 + rate_code,
            padding: (word >> 9) & 1 == 1,
            mode,
            mode_extension: ((word >> 4) & 3) as u8,
        })
    }

    pub(crate) fn channels(&self) -> usize {
        if self.mode == Mode::Mono { 1 } else { 2 }
    }

    /// Samples in each channel of a frame.
    pub(crate) fn samples(&self) -> usize {
        if self.version == Version::Mpeg1 {
            1_152
        } else {
            576
        }
    }

    /// Granules in a frame: two in MPEG-1, one in the others.
    pub(crate) fn granules(&self) -> usize {
        if self.version == Version::Mpeg1 { 2 } else { 1 }
    }

    /// Bytes the whole frame takes, header included. Zero for free format.
    pub(crate) fn len(&self) -> usize {
        let per_slot = if self.version == Version::Mpeg1 {
            144
        } else {
            72
        };
        (per_slot * self.bitrate / self.sample_rate) as usize + usize::from(self.padding)
    }

    /// Bytes of side information after the header and its CRC.
    pub(crate) fn side_info_len(&self) -> usize {
        match (self.version == Version::Mpeg1, self.channels()) {
            (true, 1) => 17,
            (true, _) => 32,
            (false, 1) => 9,
            (false, _) => 17,
        }
    }

    /// Where the side information starts.
    pub(crate) fn side_info_start(&self) -> usize {
        4 + if self.protected { 2 } else { 0 }
    }

    /// Whether two frames can belong to one stream: the same standard,
    /// layer and sample rate, and the same number of channels.
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.version == other.version
            && self.layer == other.layer
            && self.sample_rate == other.sample_rate
            && self.channels() == other.channels()
    }
}

/// CRC-16 with the polynomial 0x8005, from all ones, as frame headers carry
/// it (2.4.3.1).
pub(crate) fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The length of the ID3v2 tag at the start of `bytes`, header, footer
/// and all, or `None` if there is none. Ten bytes are enough to tell.
pub(crate) fn id3v2_len(bytes: &[u8]) -> Option<u64> {
    let header = bytes.get(..10)?;
    if &header[..3] != b"ID3" {
        return None;
    }
    let (version, revision, flags) = (header[3], header[4], header[5]);
    // The flags each version defines; the others must be clear.
    let undefined = match version {
        2 => 0x3F,
        3 => 0x1F,
        4 => 0x0F,
        _ => return None,
    };
    if revision == 0xFF || flags & undefined != 0 {
        return None;
    }
    // Four bytes of seven bits each.
    let mut size = 0u64;
    for &byte in &header[6..10] {
        if byte & 0x80 != 0 {
            return None;
        }
        size = size << 7 | u64::from(byte);
    }
    let footer = if version == 4 && flags & 0x10 != 0 {
        10
    } else {
        0
    };
    Some(10 + size + footer)
}

/// What a header in a first frame of silence says about the stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StreamInfo {
    /// Frames of audio that follow the frame holding the header.
    pub(crate) frames: Option<u32>,
    /// From a LAME tag: samples the encoder put before the audio, and after.
    pub(crate) delay: Option<(u32, u32)>,
}

/// Reads a Xing or Info header, with the LAME tag that may follow it, or a
/// VBRI header, from the bytes of a whole first frame. `None` if the frame
/// holds none, and is then audio like any other.
pub(crate) fn stream_info(frame: &[u8], header: &FrameHeader) -> Option<StreamInfo> {
    let at = header.side_info_start() + header.side_info_len();
    if let Some(tag) = frame.get(at..at + 8) {
        if &tag[..4] == b"Xing" || &tag[..4] == b"Info" {
            let flags = u32::from_be_bytes([tag[4], tag[5], tag[6], tag[7]]);
            let mut next = at + 8;
            let mut info = StreamInfo::default();
            if flags & 1 != 0 {
                let field = frame.get(next..next + 4)?;
                info.frames = Some(u32::from_be_bytes([field[0], field[1], field[2], field[3]]));
                next += 4;
            }
            // Bytes, a table of contents and a quality, none of which a
            // decoder needs.
            for (flag, len) in [(2, 4), (4, 100), (8, 4)] {
                if flags & flag != 0 {
                    next += len;
                }
            }
            // The LAME tag, which the encoders that write one name
            // themselves in: LAME, and FFmpeg's libavcodec and libavformat.
            if let Some(lame) = frame.get(next..next + 24) {
                if [&b"LAME"[..], b"Lavc", b"Lavf"].contains(&&lame[..4]) {
                    let delay = (u32::from(lame[21]) << 4) | u32::from(lame[22] >> 4);
                    let padding = (u32::from(lame[22] & 0x0F) << 8) | u32::from(lame[23]);
                    info.delay = Some((delay, padding));
                }
            }
            return Some(info);
        }
    }
    // A VBRI header is always 32 bytes after the frame header.
    let tag = frame.get(36..54)?;
    if &tag[..4] == b"VBRI" {
        let frames = u32::from_be_bytes([tag[14], tag[15], tag[16], tag[17]]);
        return Some(StreamInfo {
            frames: Some(frames),
            delay: None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_read_as_the_standard_lays_them_out() {
        // MPEG-1 layer III, no CRC, 128 kbit/s, 44.1 kHz, no padding, joint
        // stereo with M/S: the most common header there is.
        let header = FrameHeader::parse([0xFF, 0xFB, 0x90, 0x64]).unwrap();
        assert_eq!(header.version, Version::Mpeg1);
        assert_eq!(header.layer, 3);
        assert!(!header.protected);
        assert_eq!((header.bitrate, header.sample_rate), (128_000, 44_100));
        assert_eq!(header.mode, Mode::JointStereo);
        assert_eq!(header.mode_extension, 2);
        assert_eq!(header.len(), 417);
        assert_eq!(header.side_info_len(), 32);
        // MPEG-2.5, 8 kHz, 8 kbit/s, mono, padded, with a CRC.
        let header = FrameHeader::parse([0xFF, 0xE2, 0x1A, 0xC0]).unwrap();
        assert_eq!(header.version, Version::Mpeg25);
        assert!(header.protected);
        assert_eq!((header.bitrate, header.sample_rate), (8_000, 8_000));
        assert_eq!(header.mode, Mode::Mono);
        assert_eq!(header.len(), 73);
        assert_eq!(header.samples(), 576);
        // A reserved version, layer, bitrate, rate and emphasis.
        for bytes in [
            [0xFF, 0xEB, 0x90, 0x64],
            [0xFF, 0xF9, 0x90, 0x64],
            [0xFF, 0xFB, 0xF0, 0x64],
            [0xFF, 0xFB, 0x9C, 0x64],
            [0xFF, 0xFB, 0x90, 0x66],
            [0xFF, 0x7B, 0x90, 0x64],
        ] {
            assert_eq!(FrameHeader::parse(bytes), None, "{bytes:02X?}");
        }
    }

    #[test]
    fn id3v2_tags_are_measured_and_told_from_text() {
        assert_eq!(
            id3v2_len(b"ID3\x04\x00\x00\x00\x00\x02\x01rest"),
            Some(10 + 257)
        );
        assert_eq!(
            id3v2_len(b"ID3\x04\x00\x10\x00\x00\x00\x05"),
            Some(10 + 5 + 10)
        );
        assert_eq!(
            id3v2_len(b"ID3\x03\x00\x00\x7F\x7F\x7F\x7F"),
            Some(10 + (1 << 28) - 1)
        );
        assert_eq!(id3v2_len(b"ID3\x04 not audio we know"), None);
        assert_eq!(id3v2_len(b"ID3\x05\x00\x00\x00\x00\x00\x00"), None);
        assert_eq!(id3v2_len(b"ID3\x04\x00\x00\x00\x80\x00\x00"), None);
        assert_eq!(id3v2_len(b"ID3\x04\x00"), None);
    }

    #[test]
    fn the_crc_matches_a_known_value() {
        // CRC-16/CMS: polynomial 0x8005 from all ones, not reflected.
        assert_eq!(crc16(b"123456789"), 0xAEE7);
    }
}
