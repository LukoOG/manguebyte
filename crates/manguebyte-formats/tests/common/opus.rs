//! Ogg Opus streams taken apart and put back together, for the tests that
//! change a file libopus wrote: its headers, its pages, its packets.

use super::ogg_crc;

/// The packets of the first logical stream of an Ogg file, headers first.
pub fn packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut packet = Vec::new();
    for page in super::ogg_pages(bytes) {
        let segments = usize::from(bytes[page.start + 26]);
        let mut at = page.start + 27 + segments;
        for &lacing in &bytes[page.start + 27..page.start + 27 + segments] {
            packet.extend_from_slice(&bytes[at..at + usize::from(lacing)]);
            at += usize::from(lacing);
            if lacing < 255 {
                packets.push(std::mem::take(&mut packet));
            }
        }
    }
    packets
}

/// The samples at 48 kHz an Opus packet decodes to, from its table of
/// contents (RFC 6716, section 3.1).
pub fn packet_samples(packet: &[u8]) -> i64 {
    let toc = packet[0];
    let config = toc >> 3;
    let size = match config {
        0..=11 => [480, 960, 1_920, 2_880][usize::from(config & 3)],
        12..=15 => [480, 960][usize::from(config & 1)],
        _ => [120, 240, 480, 960][usize::from(config & 3)],
    };
    let count = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => packet.get(1).map_or(0, |&byte| i64::from(byte & 0x3F)),
    };
    size * count
}

/// One Ogg page of `packets`, numbered `sequence`.
pub fn page(serial: u32, sequence: u32, granule: i64, flags: u8, packets: &[&[u8]]) -> Vec<u8> {
    let mut lacing = Vec::new();
    let mut body = Vec::new();
    for packet in packets {
        let mut left = packet.len();
        while left >= 255 {
            lacing.push(255);
            left -= 255;
        }
        lacing.push(left as u8);
        body.extend_from_slice(packet);
    }
    let mut out = b"OggS".to_vec();
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(&lacing);
    out.extend_from_slice(&body);
    let crc = ogg_crc(&out);
    out[22..26].copy_from_slice(&crc.to_le_bytes());
    out
}

/// An Ogg Opus stream of these headers and audio packets, `per_page`
/// packets to a page, each page's granule position worked out from the
/// durations of the packets; the last page states `end` instead if it is
/// given.
pub fn build(
    head: &[u8],
    tags: &[u8],
    audio: &[Vec<u8>],
    per_page: usize,
    end: Option<i64>,
) -> Vec<u8> {
    let serial = 0x4F50;
    let mut out = page(serial, 0, 0, 2, &[head]);
    out.extend(page(serial, 1, 0, 0, &[tags]));
    let mut granule = 0;
    let groups: Vec<&[Vec<u8>]> = audio.chunks(per_page.max(1)).collect();
    for (index, group) in groups.iter().enumerate() {
        granule += group
            .iter()
            .map(|packet| packet_samples(packet))
            .sum::<i64>();
        let last = index + 1 == groups.len();
        let stated = if last {
            end.unwrap_or(granule)
        } else {
            granule
        };
        let packets: Vec<&[u8]> = group.iter().map(Vec::as_slice).collect();
        out.extend(page(
            serial,
            index as u32 + 2,
            stated,
            if last { 4 } else { 0 },
            &packets,
        ));
    }
    out
}

/// A comment header with these comments (RFC 7845, section 5.2).
pub fn tags(vendor: &str, comments: &[&str]) -> Vec<u8> {
    let mut out = b"OpusTags".to_vec();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor.as_bytes());
    out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for comment in comments {
        out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        out.extend_from_slice(comment.as_bytes());
    }
    out
}

/// What libopus decoded a test file to, as `data/README.md` says: every
/// `step`-th frame of the whole decode, which is `frames` long.
pub struct Reference {
    pub step: usize,
    pub channels: usize,
    pub frames: usize,
    /// The frames kept, channels interleaved.
    pub samples: Vec<f32>,
}

impl Reference {
    pub fn parse(bytes: &[u8]) -> Self {
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let step = word(0) as usize;
        let channels = word(4) as usize;
        let frames = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
        let samples = bytes[16..]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        Self {
            step,
            channels,
            frames,
            samples,
        }
    }
}
