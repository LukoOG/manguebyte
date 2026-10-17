//! ID3 tags, which MP3 files carry: ID3v2.2, 2.3 and 2.4 at the start
//! (the informal standards of id3.org: "ID3 tag version 2", "ID3 tag
//! version 2.3.0", "ID3 tag version 2.4.0 - Main Structure" and "- Native
//! Frames"), and ID3v1 in the last 128 bytes.

use super::{Budget, Metadata, Picture, bpm, latin1_or_utf8, track_numbers};

/// The genres ID3v1 numbers, which ID3v2 refers to by number too.
const GENRES: [&str; 80] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychadelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
];

/// The three-character frames of ID3v2.2 with the names ID3v2.3 gave them.
const V22_NAMES: [(&[u8; 3], &[u8; 4]); 22] = [
    (b"TT1", b"TIT1"),
    (b"TT2", b"TIT2"),
    (b"TT3", b"TIT3"),
    (b"TP1", b"TPE1"),
    (b"TP2", b"TPE2"),
    (b"TP3", b"TPE3"),
    (b"TP4", b"TPE4"),
    (b"TCM", b"TCOM"),
    (b"TAL", b"TALB"),
    (b"TYE", b"TYER"),
    (b"TRK", b"TRCK"),
    (b"TPA", b"TPOS"),
    (b"TCO", b"TCON"),
    (b"TCR", b"TCOP"),
    (b"TBP", b"TBPM"),
    (b"TKE", b"TKEY"),
    (b"TRC", b"TSRC"),
    (b"TEN", b"TENC"),
    (b"TSS", b"TSSE"),
    (b"TXX", b"TXXX"),
    (b"COM", b"COMM"),
    (b"PIC", b"APIC"),
];

/// Reads the ID3v2 tag that `tag` holds, from its ten-byte header on, into
/// `metadata`. A tag of a version this does not know, or damaged past its
/// header, gives what could be read before the damage.
pub(crate) fn read_id3v2(tag: &[u8], metadata: &mut Metadata, budget: &mut Budget) {
    let Some(header) = tag.get(..10) else {
        return;
    };
    if &header[..3] != b"ID3" {
        return;
    }
    let (version, flags) = (header[3], header[5]);
    if !(2..=4).contains(&version) {
        return;
    }
    let size = syncsafe(&header[6..10]).unwrap_or(0) as usize;
    let mut body: &[u8] = &tag[10..tag.len().min(10 + size)];
    // ID3v2.2 used this flag for a compression it never defined.
    if version == 2 && flags & 0x40 != 0 {
        return;
    }
    // Before 2.4, unsynchronisation covers the whole tag.
    let resynchronised;
    if version < 4 && flags & 0x80 != 0 {
        resynchronised = resynchronise(body);
        body = &resynchronised;
    }
    if version >= 3 && flags & 0x40 != 0 {
        // An extended header: 2.3 gives its size without the four bytes of
        // the size, 2.4 as a synchsafe number that counts them.
        let Some(len) = body.get(..4) else {
            return;
        };
        let skip = if version == 3 {
            u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize + 4
        } else {
            syncsafe(len).unwrap_or(u32::MAX) as usize
        };
        body = body.get(skip..).unwrap_or_default();
    }
    let all_unsynchronised = version == 4 && flags & 0x80 != 0;

    let header_len = if version == 2 { 6 } else { 10 };
    let mut at = 0;
    while let Some(frame) = body.get(at..at + header_len) {
        let id_len = if version == 2 { 3 } else { 4 };
        let id = &frame[..id_len];
        // Padding, or something that is not a frame.
        if !id
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            break;
        }
        let size = match version {
            2 => u32::from_be_bytes([0, frame[3], frame[4], frame[5]]),
            3 => u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]),
            // Some writers put plain sizes in 2.4 tags; a size that is not
            // synchsafe can only be one of those.
            _ => syncsafe(&frame[4..8])
                .unwrap_or_else(|| u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]])),
        } as usize;
        let Some(data) = body.get(at + header_len..(at + header_len).saturating_add(size)) else {
            break;
        };
        at += header_len + size;

        let mut data: &[u8] = data;
        let resynchronised;
        if version >= 3 {
            let format = frame[9];
            let (compressed, encrypted, grouped) = if version == 3 {
                (format & 0x80 != 0, format & 0x40 != 0, format & 0x20 != 0)
            } else {
                (format & 0x08 != 0, format & 0x04 != 0, format & 0x40 != 0)
            };
            if compressed || encrypted {
                continue;
            }
            if grouped {
                data = data.get(1..).unwrap_or_default();
            }
            if version == 4 {
                if format & 0x01 != 0 {
                    data = data.get(4..).unwrap_or_default();
                }
                if all_unsynchronised || format & 0x02 != 0 {
                    resynchronised = resynchronise(data);
                    data = &resynchronised;
                }
            }
        }
        let name: [u8; 4] = if version == 2 {
            match V22_NAMES.iter().find(|(old, _)| old.as_slice() == id) {
                Some((_, new)) => **new,
                None => [id[0], id[1], id[2], b' '],
            }
        } else {
            [id[0], id[1], id[2], id[3]]
        };
        frame_into(&name, version, data, metadata, budget);
    }
}

/// Takes in one frame.
fn frame_into(
    id: &[u8; 4],
    version: u8,
    data: &[u8],
    metadata: &mut Metadata,
    budget: &mut Budget,
) {
    let Some((&encoding, rest)) = data.split_first() else {
        return;
    };
    let tag_name = || String::from_utf8_lossy(id).trim_end().to_owned();
    match id {
        b"APIC" => {
            if !budget.take(data.len() as u64) {
                return;
            }
            let (mime, rest) = if version == 2 {
                let Some((format, rest)) = rest.split_at_checked(3) else {
                    return;
                };
                let mime = match format {
                    b"PNG" => "image/png".to_owned(),
                    b"JPG" => "image/jpeg".to_owned(),
                    other => format!("image/{}", latin1_or_utf8(other).to_ascii_lowercase()),
                };
                (mime, rest)
            } else {
                let (mime, rest) = terminated(0, rest);
                (latin1_or_utf8(mime), rest)
            };
            let Some((&kind, rest)) = rest.split_first() else {
                return;
            };
            let (description, image) = terminated(encoding, rest);
            metadata.pictures.push(Picture {
                kind: u32::from(kind),
                mime,
                description: decode(encoding, description),
                data: image.to_vec(),
                ..Picture::default()
            });
        }
        b"COMM" => {
            let Some(rest) = rest.get(3..) else {
                return;
            };
            let (description, text) = terminated(encoding, rest);
            let (description, text) = (decode(encoding, description), decode(encoding, text));
            if !budget.take(text.len() as u64) {
                return;
            }
            if description.is_empty() && metadata.comment.is_none() {
                metadata.comment = Some(text);
            } else if description.is_empty() {
                metadata.tags.push(("COMM".to_owned(), text));
            } else {
                metadata.tags.push((format!("COMM:{description}"), text));
            }
        }
        b"TXXX" => {
            let (description, text) = terminated(encoding, rest);
            let (description, text) = (decode(encoding, description), decode(encoding, text));
            if budget.take((description.len() + text.len()) as u64) {
                metadata.tags.push((description, text));
            }
        }
        [b'T', ..] => {
            let text = decode(encoding, rest);
            if !budget.take(text.len() as u64) {
                return;
            }
            let set = |slot: &mut Option<String>, value: String| -> Option<String> {
                match slot {
                    None => {
                        *slot = Some(value);
                        None
                    }
                    Some(_) => Some(value),
                }
            };
            let left = match id {
                b"TIT2" => set(&mut metadata.title, text),
                b"TPE1" => set(&mut metadata.artist, text),
                b"TALB" => set(&mut metadata.album, text),
                // The recording time of 2.4 is preferred to the year of
                // 2.3, whichever comes first.
                b"TDRC" => {
                    metadata.date = Some(text);
                    None
                }
                b"TYER" => set(&mut metadata.date, text),
                b"TCON" => set(&mut metadata.genre, genre(&text)),
                b"TCOP" => set(&mut metadata.copyright, text),
                b"TKEY" => set(&mut metadata.key, text),
                b"TSRC" => set(&mut metadata.isrc, text),
                b"TRCK" if metadata.track.is_none() => {
                    let (track, total) = track_numbers(&text);
                    metadata.track = track;
                    metadata.track_total = metadata.track_total.or(total);
                    track.is_none().then_some(text)
                }
                b"TBPM" if metadata.bpm.is_none() => {
                    metadata.bpm = bpm(&text);
                    metadata.bpm.is_none().then_some(text)
                }
                _ => Some(text),
            };
            if let Some(text) = left {
                metadata.tags.push((tag_name(), text));
            }
        }
        _ => {}
    }
}

/// A synchsafe number: 28 bits in four bytes of seven, or `None` if a byte
/// has its top bit set.
fn syncsafe(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0u32, |value, &byte| {
        (byte & 0x80 == 0).then_some(value << 7 | u32::from(byte))
    })
}

/// Undoes unsynchronisation: a zero after every 0xFF was put there by the
/// writer.
fn resynchronise(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut after_ff = false;
    for &byte in bytes {
        if !(after_ff && byte == 0) {
            out.push(byte);
        }
        after_ff = byte == 0xFF;
    }
    out
}

/// Splits `bytes` after the first string in `encoding`, ended by a NUL of
/// one byte, or of two at an even place in UTF-16; the NUL is dropped.
fn terminated(encoding: u8, bytes: &[u8]) -> (&[u8], &[u8]) {
    let end = if matches!(encoding, 1 | 2) {
        (0..bytes.len() / 2)
            .map(|pair| pair * 2)
            .find(|&at| bytes[at] == 0 && bytes[at + 1] == 0)
            .map(|at| (at, at + 2))
    } else {
        bytes
            .iter()
            .position(|&byte| byte == 0)
            .map(|at| (at, at + 1))
    };
    match end {
        Some((end, next)) => (&bytes[..end], &bytes[next..]),
        None => (bytes, &[]),
    }
}

/// Decodes text in `encoding`: 0 Latin-1, 1 UTF-16 with a byte-order mark,
/// 2 UTF-16 big-endian, 3 UTF-8. Several strings separated by NULs, as 2.4
/// lists values, are joined with `"; "`.
fn decode(encoding: u8, mut bytes: &[u8]) -> String {
    let mut parts = Vec::new();
    while !bytes.is_empty() {
        let (part, rest) = terminated(encoding, bytes);
        bytes = rest;
        let text = match encoding {
            0 => part.iter().map(|&byte| char::from(byte)).collect(),
            1 | 2 => {
                let mut big = encoding == 2;
                let mut part = part;
                match part {
                    [0xFF, 0xFE, rest @ ..] => {
                        big = false;
                        part = rest;
                    }
                    [0xFE, 0xFF, rest @ ..] => {
                        big = true;
                        part = rest;
                    }
                    _ => {}
                }
                let units = part.chunks_exact(2).map(|pair| {
                    if big {
                        u16::from_be_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_le_bytes([pair[0], pair[1]])
                    }
                });
                char::decode_utf16(units)
                    .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect()
            }
            _ => String::from_utf8_lossy(part).into_owned(),
        };
        parts.push(text);
    }
    parts.retain(|part: &String| !part.is_empty());
    parts.join("; ")
}

/// A genre as ID3 writes it: a name, a number of the ID3v1 list, or a
/// number in brackets followed by a refinement.
fn genre(text: &str) -> String {
    let by_number = |number: &str| -> Option<String> {
        let index: usize = number.parse().ok()?;
        GENRES.get(index).map(|name| (*name).to_owned())
    };
    let one = |part: &str| -> String {
        if let Some(name) = by_number(part) {
            return name;
        }
        if let Some(inner) = part.strip_prefix('(') {
            if let Some((number, rest)) = inner.split_once(')') {
                if !rest.is_empty() {
                    return rest.to_owned();
                }
                match number {
                    "RX" => return "Remix".to_owned(),
                    "CR" => return "Cover".to_owned(),
                    _ => {
                        if let Some(name) = by_number(number) {
                            return name;
                        }
                    }
                }
            }
        }
        part.to_owned()
    };
    text.split("; ").map(one).collect::<Vec<_>>().join("; ")
}

/// Reads an ID3v1 tag from the last 128 bytes of a file, or gives `None`
/// if they are not one. ID3v1.1 puts the track number at the end of the
/// comment.
pub(crate) fn read_id3v1(last: &[u8]) -> Option<Metadata> {
    if last.len() != 128 || &last[..3] != b"TAG" {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<String> {
        let bytes = &last[range];
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        let text: String = bytes[..end].iter().map(|&byte| char::from(byte)).collect();
        let text = text.trim_end().to_owned();
        (!text.is_empty()).then_some(text)
    };
    let v11 = last[125] == 0 && last[126] != 0;
    Some(Metadata {
        title: field(3..33),
        artist: field(33..63),
        album: field(63..93),
        date: field(93..97),
        comment: field(97..if v11 { 125 } else { 127 }),
        track: v11.then_some(u32::from(last[126])),
        genre: GENRES
            .get(usize::from(last[127]))
            .map(|name| (*name).to_owned()),
        ..Metadata::default()
    })
}

/// The most an ID3v2 size can say: 28 bits.
const MAX_ID3V2: usize = (1 << 28) - 1;

/// An ID3v2.4 tag of what `metadata` holds that ID3 has frames for, text
/// in UTF-8: the fields as `TIT2`, `TPE1`, `TALB`, `TDRC`, `TRCK`, `TCON`,
/// `COMM`, `TCOP`, `TBPM`, `TKEY` and `TSRC`; each tag as the text frame it
/// names when its name is one (four capitals or digits starting with `T`),
/// as a comment when it is `COMM` or `COMM:` and a description, and as a
/// `TXXX` frame described by its name otherwise; each picture as an
/// `APIC` frame, which keeps its kind, type, description and data. `None`
/// when there is nothing to write, and `Err` when the tag would pass the
/// 256 MiB an ID3v2 size can state.
pub(crate) fn write_id3v2(metadata: &Metadata) -> Result<Option<Vec<u8>>, &'static str> {
    let mut body = Vec::new();
    let mut put = |id: &[u8; 4], data: &[u8]| -> Result<(), &'static str> {
        if data.len() > MAX_ID3V2 {
            return Err("an ID3 frame larger than 256 MiB");
        }
        body.extend_from_slice(id);
        body.extend_from_slice(&syncsafe_bytes(data.len() as u32));
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(data);
        Ok(())
    };
    let text = |value: &str| -> Vec<u8> {
        let mut data = vec![3];
        data.extend_from_slice(value.as_bytes());
        data
    };
    let comment = |description: &str, value: &str| -> Vec<u8> {
        let mut data = vec![3];
        data.extend_from_slice(b"XXX");
        data.extend_from_slice(description.as_bytes());
        data.push(0);
        data.extend_from_slice(value.as_bytes());
        data
    };
    let fields: [(&[u8; 4], Option<&String>); 8] = [
        (b"TIT2", metadata.title.as_ref()),
        (b"TPE1", metadata.artist.as_ref()),
        (b"TALB", metadata.album.as_ref()),
        (b"TDRC", metadata.date.as_ref()),
        (b"TCON", metadata.genre.as_ref()),
        (b"TCOP", metadata.copyright.as_ref()),
        (b"TKEY", metadata.key.as_ref()),
        (b"TSRC", metadata.isrc.as_ref()),
    ];
    for (id, value) in fields {
        if let Some(value) = value {
            put(id, &text(value))?;
        }
    }
    if let Some(track) = metadata.track {
        let value = match metadata.track_total {
            Some(total) => format!("{track}/{total}"),
            None => track.to_string(),
        };
        put(b"TRCK", &text(&value))?;
    }
    if let Some(bpm) = metadata.bpm {
        put(b"TBPM", &text(&bpm.to_string()))?;
    }
    if let Some(value) = &metadata.comment {
        put(b"COMM", &comment("", value))?;
    }
    for (name, value) in &metadata.tags {
        let id = name.as_bytes();
        if name == "COMM" {
            put(b"COMM", &comment("", value))?;
        } else if let Some(description) = name.strip_prefix("COMM:") {
            put(b"COMM", &comment(description, value))?;
        } else if id.len() == 4
            && id[0] == b'T'
            && name != "TXXX"
            && id
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            put(&[id[0], id[1], id[2], id[3]], &text(value))?;
        } else {
            let mut data = vec![3];
            data.extend_from_slice(name.as_bytes());
            data.push(0);
            data.extend_from_slice(value.as_bytes());
            put(b"TXXX", &data)?;
        }
    }
    for picture in &metadata.pictures {
        let mut data = vec![3];
        data.extend_from_slice(picture.mime.as_bytes());
        data.push(0);
        data.push(picture.kind.min(255) as u8);
        data.extend_from_slice(picture.description.as_bytes());
        data.push(0);
        data.extend_from_slice(&picture.data);
        put(b"APIC", &data)?;
    }
    if body.is_empty() {
        return Ok(None);
    }
    if body.len() > MAX_ID3V2 {
        return Err("an ID3 tag larger than 256 MiB");
    }
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend_from_slice(&syncsafe_bytes(body.len() as u32));
    tag.extend_from_slice(&body);
    Ok(Some(tag))
}

/// A number below 2^28 as four bytes of seven bits.
fn syncsafe_bytes(value: u32) -> [u8; 4] {
    [
        (value >> 21) as u8 & 0x7F,
        (value >> 14) as u8 & 0x7F,
        (value >> 7) as u8 & 0x7F,
        value as u8 & 0x7F,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_is_read_back() {
        let metadata = Metadata {
            title: Some("Coco de roda".to_owned()),
            artist: Some("Selma do Coco".to_owned()),
            album: Some("Olinda".to_owned()),
            date: Some("1998".to_owned()),
            track: Some(4),
            track_total: Some(11),
            genre: Some("Coco".to_owned()),
            comment: Some("ao vivo".to_owned()),
            copyright: Some("(C) 1998".to_owned()),
            bpm: Some(126.5),
            key: Some("G".to_owned()),
            isrc: Some("BRXYZ9800004".to_owned()),
            tags: vec![
                ("TPE2".to_owned(), "Banda".to_owned()),
                ("MOOD".to_owned(), "festa".to_owned()),
                ("COMM:faixa".to_owned(), "quatro".to_owned()),
            ],
            pictures: vec![Picture {
                kind: 3,
                mime: "image/png".to_owned(),
                description: "capa".to_owned(),
                data: vec![0x89, b'P', b'N', b'G', 0, 0xFF],
                ..Picture::default()
            }],
            ..Metadata::default()
        };
        let tag = write_id3v2(&metadata).unwrap().unwrap();
        assert_eq!(read(&tag), metadata);
        assert_eq!(write_id3v2(&Metadata::default()), Ok(None));
    }

    fn frame(version: u8, id: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        let len = data.len() as u32;
        match version {
            2 => out.extend_from_slice(&len.to_be_bytes()[1..]),
            3 => out.extend_from_slice(&len.to_be_bytes()),
            _ => out.extend_from_slice(&[
                (len >> 21) as u8 & 0x7F,
                (len >> 14) as u8 & 0x7F,
                (len >> 7) as u8 & 0x7F,
                len as u8 & 0x7F,
            ]),
        }
        if version > 2 {
            out.extend_from_slice(&[0, 0]);
        }
        out.extend_from_slice(data);
        out
    }

    fn tag(version: u8, flags: u8, frames: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = frames.concat();
        let len = body.len() as u32 + 10;
        let mut out = b"ID3".to_vec();
        out.extend_from_slice(&[version, 0, flags]);
        out.extend_from_slice(&[
            (len >> 21) as u8 & 0x7F,
            (len >> 14) as u8 & 0x7F,
            (len >> 7) as u8 & 0x7F,
            len as u8 & 0x7F,
        ]);
        out.extend_from_slice(&body);
        // Padding.
        out.extend_from_slice(&[0; 10]);
        out
    }

    fn read(bytes: &[u8]) -> Metadata {
        let mut metadata = Metadata::default();
        read_id3v2(bytes, &mut metadata, &mut Budget::new());
        metadata
    }

    #[test]
    fn frames_of_every_version_and_encoding_are_read() {
        let utf16 = |text: &str| -> Vec<u8> {
            let mut out = vec![1, 0xFF, 0xFE];
            out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            out
        };
        let v4 = tag(
            4,
            0,
            &[
                frame(4, b"TIT2", b"\x03Ciranda de Lia"),
                frame(4, b"TPE1", &utf16("Lia de Itamaracá")),
                frame(4, b"TALB", b"\x00Ciranda\xE7"),
                frame(4, b"TDRC", b"\x032026-10-17"),
                frame(4, b"TRCK", b"\x033/12"),
                frame(4, b"TCON", b"\x0317\x00(9)"),
                frame(4, b"TBPM", b"\x03104"),
                frame(4, b"TKEY", b"\x03Am"),
                frame(4, b"TSRC", b"\x03BRMBY2600001"),
                frame(4, b"TCOP", b"\x03(C) 2026"),
                frame(4, b"COMM", b"\x03porfirst\x00Good"),
                frame(4, b"COMM", b"\x03por\x00Second"),
                frame(4, b"TXXX", b"\x03MOOD\x00calm"),
                frame(4, b"TPE2", b"\x03Band"),
                frame(4, b"APIC", b"\x03image/png\x00\x03Cover\x00\x89PNG"),
            ],
        );
        let metadata = read(&v4);
        assert_eq!(metadata.title.as_deref(), Some("Ciranda de Lia"));
        assert_eq!(metadata.artist.as_deref(), Some("Lia de Itamaracá"));
        assert_eq!(metadata.album.as_deref(), Some("Cirandaç"));
        assert_eq!(metadata.date.as_deref(), Some("2026-10-17"));
        assert_eq!((metadata.track, metadata.track_total), (Some(3), Some(12)));
        assert_eq!(metadata.genre.as_deref(), Some("Rock; Metal"));
        assert_eq!(metadata.bpm, Some(104.0));
        assert_eq!(metadata.key.as_deref(), Some("Am"));
        assert_eq!(metadata.isrc.as_deref(), Some("BRMBY2600001"));
        assert_eq!(metadata.copyright.as_deref(), Some("(C) 2026"));
        assert_eq!(metadata.comment.as_deref(), Some("Second"));
        assert_eq!(metadata.tag("COMM:first"), Some("Good"));
        assert_eq!(metadata.tag("MOOD"), Some("calm"));
        assert_eq!(metadata.tag("TPE2"), Some("Band"));
        assert_eq!(metadata.pictures.len(), 1);
        let picture = &metadata.pictures[0];
        assert_eq!((picture.kind, picture.mime.as_str()), (3, "image/png"));
        assert_eq!(picture.description, "Cover");
        assert_eq!(picture.data, b"\x89PNG");

        let v3 = tag(
            3,
            0,
            &[
                frame(3, b"TIT2", b"\x00Old"),
                frame(3, b"TYER", b"\x001999"),
                frame(3, b"TCON", b"\x00(13)Synthpop"),
            ],
        );
        let metadata = read(&v3);
        assert_eq!(metadata.title.as_deref(), Some("Old"));
        assert_eq!(metadata.date.as_deref(), Some("1999"));
        assert_eq!(metadata.genre.as_deref(), Some("Synthpop"));

        let v2 = tag(
            2,
            0,
            &[
                frame(2, b"TT2", b"\x00Older"),
                frame(2, b"TCO", b"\x00(13)"),
                frame(2, b"PIC", b"\x00JPG\x04back\x00\xFF\xD8"),
            ],
        );
        let metadata = read(&v2);
        assert_eq!(metadata.title.as_deref(), Some("Older"));
        assert_eq!(metadata.genre.as_deref(), Some("Pop"));
        assert_eq!(metadata.pictures[0].mime, "image/jpeg");
        assert_eq!(metadata.pictures[0].kind, 4);
    }

    #[test]
    fn unsynchronised_tags_are_read() {
        // 0xFF 0xE0 in a title, which unsynchronisation writes 0xFF 0x00 0xE0.
        let mut v3 = tag(3, 0x80, &[frame(3, b"TIT2", b"\x00a\xFF\x00\xE0b")]);
        // The size of the frame counts the bytes before resynchronisation;
        // ID3v2.3 says the frame header comes after it.
        v3[10 + 7] = 5;
        let metadata = read(&v3);
        assert_eq!(metadata.title.as_deref(), Some("a\u{FF}\u{E0}b"));
    }

    #[test]
    fn id3v1_and_its_track_number_are_read() {
        let mut last = vec![0u8; 128];
        last[..3].copy_from_slice(b"TAG");
        last[3..8].copy_from_slice(b"Title");
        last[33..39].copy_from_slice(b"Artist");
        last[93..97].copy_from_slice(b"2001");
        last[97..101].copy_from_slice(b"Note");
        last[126] = 7;
        last[127] = 8;
        let metadata = read_id3v1(&last).unwrap();
        assert_eq!(metadata.title.as_deref(), Some("Title"));
        assert_eq!(metadata.artist.as_deref(), Some("Artist"));
        assert_eq!(metadata.album, None);
        assert_eq!(metadata.date.as_deref(), Some("2001"));
        assert_eq!(metadata.comment.as_deref(), Some("Note"));
        assert_eq!(metadata.track, Some(7));
        assert_eq!(metadata.genre.as_deref(), Some("Jazz"));
        last[127] = 255;
        assert_eq!(read_id3v1(&last).unwrap().genre, None);
        assert!(read_id3v1(&last[1..]).is_none());
    }

    #[test]
    fn damaged_tags_never_panic() {
        let whole = tag(
            4,
            0x80,
            &[
                frame(4, b"TIT2", b"\x01\xFF\xFEa\x00b"),
                frame(
                    4,
                    b"APIC",
                    b"\x01image/png\x00\x03\xFF\xFEC\x00\x00\x00\x89PNG",
                ),
                frame(4, b"COMM", b"\x02en"),
            ],
        );
        for len in 0..whole.len() {
            let _ = read(&whole[..len]);
            let mut damaged = whole.clone();
            damaged[len] ^= 0x5A;
            let _ = read(&damaged);
        }
    }
}
