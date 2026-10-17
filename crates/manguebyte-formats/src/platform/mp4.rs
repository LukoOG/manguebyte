//! The edit list of the first sound track of an MPEG-4 file (ISO/IEC
//! 14496-12, sections 8.2.2, 8.4.2, 8.4.3 and 8.6.6): where the audio starts
//! in what the decoder gives, and how long it is. An AAC encoder puts a
//! block or two of priming before the audio and says so here; Media
//! Foundation's MPEG-4 reader does not apply it.

use std::io::{Read, Seek, SeekFrom};

/// The most of a movie box that is read: the boxes that describe the
/// tracks of an audio file take a few kilobytes, a sample table a little
/// more for each minute.
const MAX_MOVIE: u64 = 64 << 20;

/// Where the audio of the sound track starts and how long it is, in frames
/// at the track's own rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edit {
    /// Frames the decoder gives before the audio.
    pub(crate) delay: u64,
    /// Frames of audio.
    pub(crate) length: u64,
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes inside `bytes`, as their type and body.
fn boxes(mut bytes: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let size = u64::from(be32(bytes, 0)?);
        let kind: [u8; 4] = bytes.get(4..8)?.try_into().ok()?;
        let (header, size) = match size {
            0 => (8, bytes.len() as u64),
            1 => (16, be64(bytes, 8)?),
            _ => (8, size),
        };
        let size = usize::try_from(size).ok()?;
        let body = bytes.get(header..size)?;
        bytes = &bytes[size..];
        Some((kind, body))
    })
}

fn find<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(bytes)
        .find(|(found, _)| found == kind)
        .map(|(_, body)| body)
}

/// The timescale of a movie or media header: its version, then two times
/// of 32 or 64 bits, then the timescale.
fn timescale(header: &[u8]) -> Option<u32> {
    match header.first()? {
        0 => be32(header, 12),
        _ => be32(header, 20),
    }
    .filter(|&scale| scale > 0)
}

/// Reads the edit of the first sound track of the MPEG-4 file in `reader`,
/// in frames at `rate`. `None` when there is no such track or edit list,
/// or when the file is not what it should be.
pub(crate) fn sound_edit<R: Read + Seek>(reader: &mut R, rate: u32) -> Option<Edit> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let mut at = 0u64;
    // The movie box, among the boxes at the top of the file.
    let movie = loop {
        if at.checked_add(8)? > len {
            return None;
        }
        reader.seek(SeekFrom::Start(at)).ok()?;
        let mut header = [0u8; 16];
        reader.read_exact(&mut header[..8]).ok()?;
        let (start, size) = match u32::from_be_bytes(header[..4].try_into().ok()?) {
            0 => (8, len - at),
            1 => {
                reader.read_exact(&mut header[8..]).ok()?;
                (16, be64(&header, 8)?)
            }
            size => (8, u64::from(size)),
        };
        if size < start || at.checked_add(size)? > len {
            return None;
        }
        if &header[4..8] == b"moov" {
            let body = size - start;
            if body > MAX_MOVIE {
                return None;
            }
            let mut movie = vec![0u8; body as usize];
            reader.read_exact(&mut movie).ok()?;
            break movie;
        }
        at += size;
    };
    let movie_scale = timescale(find(&movie, b"mvhd")?)?;
    for (kind, track) in boxes(&movie) {
        if &kind != b"trak" {
            continue;
        }
        let media = find(track, b"mdia")?;
        let handler = find(media, b"hdlr")?;
        if handler.get(8..12)? != b"soun" {
            continue;
        }
        let media_scale = timescale(find(media, b"mdhd")?)?;
        let list = find(find(track, b"edts")?, b"elst")?;
        let wide = *list.first()? == 1;
        let count = be32(list, 4)?;
        let entry = if wide { 20 } else { 12 };
        for index in 0..count.min(64) as usize {
            let at = 8 + index * entry;
            let (duration, time) = if wide {
                (be64(list, at)?, be64(list, at + 8)? as i64)
            } else {
                (
                    u64::from(be32(list, at)?),
                    i64::from(be32(list, at + 4)? as i32),
                )
            };
            // An empty edit, a stretch of silence before the media, is
            // not the audio.
            if time < 0 {
                continue;
            }
            let to_frames = |value: u64, scale: u32| -> u64 {
                (u128::from(value) * u128::from(rate) / u128::from(scale)) as u64
            };
            return Some(Edit {
                delay: to_frames(time as u64, media_scale),
                length: to_frames(duration, movie_scale),
            });
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn header(version: u8, scale: u32) -> Vec<u8> {
        let mut body = vec![version, 0, 0, 0];
        let times = if version == 0 { 8 } else { 16 };
        body.extend(std::iter::repeat_n(0, times));
        body.extend_from_slice(&scale.to_be_bytes());
        body.extend_from_slice(&[0; 8]);
        body
    }

    fn file(edits: &[(i64, u64)], wide: bool) -> Vec<u8> {
        let mut list = vec![u8::from(wide), 0, 0, 0];
        list.extend_from_slice(&(edits.len() as u32).to_be_bytes());
        for &(time, duration) in edits {
            if wide {
                list.extend_from_slice(&duration.to_be_bytes());
                list.extend_from_slice(&time.to_be_bytes());
            } else {
                list.extend_from_slice(&(duration as u32).to_be_bytes());
                list.extend_from_slice(&(time as i32).to_be_bytes());
            }
            list.extend_from_slice(&[0, 1, 0, 0]);
        }
        let mut hdlr = vec![0; 8];
        hdlr.extend_from_slice(b"soun");
        hdlr.extend_from_slice(&[0; 12]);
        let media = [boxed(b"mdhd", &header(1, 48_000)), boxed(b"hdlr", &hdlr)].concat();
        let track = [
            boxed(b"tkhd", &[0; 84]),
            boxed(b"edts", &boxed(b"elst", &list)),
            boxed(b"mdia", &media),
        ]
        .concat();
        let movie = [boxed(b"mvhd", &header(0, 1_000)), boxed(b"trak", &track)].concat();
        [
            boxed(b"ftyp", b"M4A \0\0\0\0"),
            boxed(b"mdat", &[0; 100]),
            boxed(b"moov", &movie),
        ]
        .concat()
    }

    #[test]
    fn the_first_edit_of_the_sound_track_is_read() {
        let bytes = file(&[(-1, 500), (2_112, 2_000)], false);
        assert_eq!(
            sound_edit(&mut Cursor::new(&bytes), 48_000),
            Some(Edit {
                delay: 2_112,
                length: 96_000
            })
        );
        let bytes = file(&[(1_024, 1_500)], true);
        assert_eq!(
            sound_edit(&mut Cursor::new(&bytes), 48_000),
            Some(Edit {
                delay: 1_024,
                length: 72_000
            })
        );
        for len in 0..bytes.len() {
            let _ = sound_edit(&mut Cursor::new(&bytes[..len]), 48_000);
        }
        assert_eq!(
            sound_edit(&mut Cursor::new(b"not an mp4 file"), 48_000),
            None
        );
    }
}
