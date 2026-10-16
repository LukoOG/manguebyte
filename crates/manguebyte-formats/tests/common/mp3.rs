//! Taking MP3 files apart and putting them together again, for the tests:
//! frames found by their headers, worked out here from the standard apart
//! from the decoder.

/// The length in bytes of the layer III frame whose header is `header`, or
/// `None` if it is not one: ISO/IEC 11172-3, 2.4.2.3, with the bitrates and
/// sample rates of MPEG-1, MPEG-2 and MPEG-2.5.
pub fn frame_len(header: &[u8]) -> Option<usize> {
    let word = u32::from_be_bytes(header.get(..4)?.try_into().ok()?);
    if word >> 21 != 0x7FF || (word >> 17) & 3 != 1 {
        return None;
    }
    let version = (word >> 19) & 3;
    let bitrate_index = ((word >> 12) & 15) as usize;
    let rate_index = ((word >> 10) & 3) as usize;
    if version == 1 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let bitrate = if mpeg1 {
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ][bitrate_index]
    } else {
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160][bitrate_index]
    } * 1_000;
    let rate = [
        [11_025, 12_000, 8_000],
        [0, 0, 0],
        [22_050, 24_000, 16_000],
        [44_100, 48_000, 32_000],
    ][version as usize][rate_index];
    let padding = ((word >> 9) & 1) as usize;
    Some(if mpeg1 { 144 } else { 72 } * bitrate / rate + padding)
}

/// Where the frames of an MP3 file are, from the first one after its ID3v2
/// tag: each where the one before ends.
pub fn frames(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut at = 0;
    if bytes.starts_with(b"ID3") {
        let size = bytes[6..10]
            .iter()
            .fold(0usize, |size, &byte| size << 7 | usize::from(byte));
        at = 10 + size;
    }
    let mut frames = Vec::new();
    while let Some(len) = bytes.get(at..).and_then(frame_len) {
        if at + len > bytes.len() {
            break;
        }
        frames.push(at..at + len);
        at += len;
    }
    frames
}

/// Whether a frame is the first frame of silence that holds a Xing or Info
/// header: the tag where the side information of an MPEG-1 stereo frame,
/// which is what the tests use it on, ends.
pub fn is_info_frame(frame: &[u8]) -> bool {
    matches!(frame.get(36..40), Some(b"Xing" | b"Info"))
}

/// The audio frames of an MP3 file `times` over, as one stream with no
/// header that says how long it is. LAME starts the main data of its first
/// frame in that frame, so the frames of one round need nothing of the
/// round before.
pub fn repeat_frames(bytes: &[u8], times: usize) -> Vec<u8> {
    let frames = frames(bytes);
    let audio: Vec<&[u8]> = frames
        .iter()
        .map(|range| &bytes[range.clone()])
        .filter(|frame| !is_info_frame(frame))
        .collect();
    let mut out = Vec::new();
    for _ in 0..times {
        for frame in &audio {
            out.extend_from_slice(frame);
        }
    }
    out
}

/// CRC-16 with polynomial 0x8005 from all ones, bit by bit, as layer III
/// frames carry it.
pub fn crc16(bytes: &[u8]) -> u16 {
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

/// The same MPEG-1 stereo stream with every frame that has two bytes to
/// spare carrying a CRC: the bit of the header that says so cleared, the
/// CRC of the last two bytes of the header and the side information after
/// the header, and the end of the frame, which its main data does not
/// reach, two bytes shorter. Only a stream without a bit reservoir has
/// frames whose end no later frame uses. Returns the stream and how many
/// frames carry a CRC.
pub fn protect_frames(bytes: &[u8]) -> (Vec<u8>, usize) {
    let mut out = bytes[..frames(bytes)[0].start].to_vec();
    let mut protected = 0;
    for range in frames(bytes) {
        let frame = &bytes[range];
        // The four part2_3_lengths: twelve bits at the start of each of the
        // four blocks of 59 bits that follow the first 20 of the side
        // information.
        let side = &frame[4..36];
        let bit = |at: usize| u32::from((side[at / 8] >> (7 - at % 8)) & 1);
        let used: u32 = (0..4)
            .map(|block| (0..12).fold(0, |value, i| value << 1 | bit(20 + 59 * block + i)))
            .sum();
        let spare = (frame.len() - 36) as i64 - i64::from(used.div_ceil(8));
        if spare < 2 || is_info_frame(frame) {
            out.extend_from_slice(frame);
            continue;
        }
        let header = [frame[0], frame[1] & !1, frame[2], frame[3]];
        let mut covered = header[2..].to_vec();
        covered.extend_from_slice(side);
        out.extend_from_slice(&header);
        out.extend_from_slice(&crc16(&covered).to_be_bytes());
        out.extend_from_slice(&frame[4..frame.len() - 2]);
        protected += 1;
    }
    (out, protected)
}
