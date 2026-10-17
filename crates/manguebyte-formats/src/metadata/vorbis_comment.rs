//! Vorbis comments (the Vorbis I specification, section 5, and RFC 9639,
//! section 8.6) and FLAC picture blocks (RFC 9639, section 8.8), which Ogg
//! files carry inside a comment, in base64 (RFC 4648, section 4).

use super::{Budget, Metadata, Picture, bpm, bpm_text, track_numbers};

/// Takes one comment into `metadata`: into the field it names, or, if it
/// names none or the field is already set, into the tags.
pub(super) fn apply(metadata: &mut Metadata, name: &str, value: &str, budget: &mut Budget) {
    let upper = name.to_ascii_uppercase();
    let set = |slot: &mut Option<String>| -> bool {
        if slot.is_none() {
            *slot = Some(value.to_owned());
            true
        } else {
            false
        }
    };
    let taken = match upper.as_str() {
        "TITLE" => set(&mut metadata.title),
        "ARTIST" => set(&mut metadata.artist),
        "ALBUM" => set(&mut metadata.album),
        "DATE" => set(&mut metadata.date),
        "GENRE" => set(&mut metadata.genre),
        "COMMENT" => set(&mut metadata.comment),
        "COPYRIGHT" => set(&mut metadata.copyright),
        "KEY" => set(&mut metadata.key),
        "ISRC" => set(&mut metadata.isrc),
        "TRACKNUMBER" if metadata.track.is_none() => {
            let (track, total) = track_numbers(value);
            metadata.track = track;
            if metadata.track_total.is_none() {
                metadata.track_total = total;
            }
            track.is_some()
        }
        "TRACKTOTAL" if metadata.track_total.is_none() => {
            metadata.track_total = track_numbers(value).0;
            metadata.track_total.is_some()
        }
        "BPM" if metadata.bpm.is_none() => {
            metadata.bpm = bpm(value);
            metadata.bpm.is_some()
        }
        "METADATA_BLOCK_PICTURE" => match base64(value) {
            Some(block) if budget.take(block.len() as u64) => match parse_flac_picture(&block) {
                Some(picture) => {
                    metadata.pictures.push(picture);
                    true
                }
                None => false,
            },
            // A picture that is not one, or too large to keep, is dropped
            // rather than kept as text of megabytes.
            _ => true,
        },
        _ => false,
    };
    if !taken && budget.take((name.len() + value.len()) as u64) {
        metadata.tags.push((name.to_owned(), value.to_owned()));
    }
}

/// The fields of `metadata` as Vorbis comments, followed by its tags.
pub(crate) fn vorbis_tags(metadata: &Metadata) -> Vec<(String, String)> {
    let mut tags = Vec::new();
    let mut push = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            tags.push((name.to_owned(), value));
        }
    };
    push("TITLE", metadata.title.clone());
    push("ARTIST", metadata.artist.clone());
    push("ALBUM", metadata.album.clone());
    push("DATE", metadata.date.clone());
    push("TRACKNUMBER", metadata.track.map(|track| track.to_string()));
    push(
        "TRACKTOTAL",
        metadata.track_total.map(|total| total.to_string()),
    );
    push("GENRE", metadata.genre.clone());
    push("COMMENT", metadata.comment.clone());
    push("COPYRIGHT", metadata.copyright.clone());
    push("BPM", metadata.bpm.map(bpm_text));
    push("KEY", metadata.key.clone());
    push("ISRC", metadata.isrc.clone());
    tags.extend(metadata.tags.iter().cloned());
    tags
}

/// The comments of a FLAC Vorbis comment block, whose lengths are
/// little-endian: the vendor string and then the comments.
pub(crate) fn parse_vorbis_comment_block(
    body: &[u8],
    metadata: &mut Metadata,
    budget: &mut Budget,
) {
    let mut at = 0usize;
    let mut take = |len: usize| -> Option<&[u8]> {
        let end = at.checked_add(len).filter(|&end| end <= body.len())?;
        let bytes = &body[at..end];
        at = end;
        Some(bytes)
    };
    let length = |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let Some(vendor_len) = take(4).map(length) else {
        return;
    };
    if take(vendor_len as usize).is_none() {
        return;
    }
    let Some(count) = take(4).map(length) else {
        return;
    };
    // Each comment takes at least its four bytes of length, so a count
    // larger than the block can hold ends at the end of the block.
    for _ in 0..count {
        let Some(len) = take(4).map(length) else {
            return;
        };
        let Some(comment) = take(len as usize) else {
            return;
        };
        if let Some(split) = comment.iter().position(|&byte| byte == b'=') {
            let name = String::from_utf8_lossy(&comment[..split]);
            let value = String::from_utf8_lossy(&comment[split + 1..]);
            apply(metadata, &name, &value, budget);
        }
    }
}

/// Reads the body of a FLAC picture block, or `None` if it is not one.
pub(crate) fn parse_flac_picture(body: &[u8]) -> Option<Picture> {
    struct Fields<'a> {
        bytes: &'a [u8],
    }
    impl<'a> Fields<'a> {
        fn take(&mut self, len: usize) -> Option<&'a [u8]> {
            let (taken, rest) = self.bytes.split_at_checked(len)?;
            self.bytes = rest;
            Some(taken)
        }
        fn number(&mut self) -> Option<u32> {
            self.take(4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        }
    }
    let mut fields = Fields { bytes: body };
    let kind = fields.number()?;
    let mime_len = fields.number()? as usize;
    let mime = String::from_utf8_lossy(fields.take(mime_len)?).into_owned();
    let description_len = fields.number()? as usize;
    let description = String::from_utf8_lossy(fields.take(description_len)?).into_owned();
    let width = fields.number()?;
    let height = fields.number()?;
    let depth = fields.number()?;
    let colors = fields.number()?;
    let data_len = fields.number()? as usize;
    let data = fields.take(data_len)?.to_vec();
    Some(Picture {
        kind,
        mime,
        description,
        width,
        height,
        depth,
        colors,
        data,
    })
}

/// The body of a FLAC picture block holding `picture`, or `None` if a part
/// of it is longer than 32 bits can state.
pub(crate) fn flac_picture_block(picture: &Picture) -> Option<Vec<u8>> {
    let len = |bytes: &[u8]| u32::try_from(bytes.len()).ok().map(u32::to_be_bytes);
    let mut body = Vec::with_capacity(32 + picture.mime.len() + picture.data.len());
    body.extend_from_slice(&picture.kind.to_be_bytes());
    body.extend_from_slice(&len(picture.mime.as_bytes())?);
    body.extend_from_slice(picture.mime.as_bytes());
    body.extend_from_slice(&len(picture.description.as_bytes())?);
    body.extend_from_slice(picture.description.as_bytes());
    for value in [picture.width, picture.height, picture.depth, picture.colors] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.extend_from_slice(&len(&picture.data)?);
    body.extend_from_slice(&picture.data);
    Some(body)
}

/// Decodes base64 with the standard alphabet and its padding, or `None` if
/// `text` is not that.
fn base64(text: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| -> Option<u32> {
        Some(u32::from(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    };
    let bytes = text.trim_end_matches('=').as_bytes();
    if text.len() % 4 != 0 || text.len() - bytes.len() > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for group in bytes.chunks(4) {
        let mut value = 0u32;
        for &byte in group {
            value = value << 6 | digit(byte)?;
        }
        value <<= 6 * (4 - group.len() as u32);
        let produced = match group.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        out.extend_from_slice(&value.to_be_bytes()[1..1 + produced]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_the_examples_of_rfc_4648() {
        for (text, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("Zm9vYg==", b"foob"),
            ("Zm9vYmE=", b"fooba"),
            ("Zm9vYmFy", b"foobar"),
        ] {
            assert_eq!(base64(text).as_deref(), Some(bytes), "{text}");
        }
        for text in ["Zg=", "Z===", "Zm9v!A==", "Zm9vY"] {
            assert_eq!(base64(text), None, "{text}");
        }
    }

    #[test]
    fn a_picture_block_reads_back() {
        let picture = Picture {
            kind: 3,
            mime: "image/png".into(),
            description: "Capa".into(),
            width: 2,
            height: 3,
            depth: 24,
            colors: 0,
            data: vec![0x89, b'P', b'N', b'G'],
        };
        let block = flac_picture_block(&picture).unwrap();
        assert_eq!(parse_flac_picture(&block), Some(picture));
        for len in 0..block.len() {
            assert_eq!(parse_flac_picture(&block[..len]), None, "{len}");
        }
    }

    #[test]
    fn comments_go_to_their_fields_and_the_rest_to_the_tags() {
        let metadata = Metadata::from_vorbis_comments([
            ("title", "Frevo"),
            ("TRACKNUMBER", "4/9"),
            ("BPM", "152"),
            ("ARTIST", "One"),
            ("ARTIST", "Two"),
            ("ALBUMARTIST", "Both"),
            ("METADATA_BLOCK_PICTURE", "not base64!"),
        ]);
        assert_eq!(metadata.title.as_deref(), Some("Frevo"));
        assert_eq!((metadata.track, metadata.track_total), (Some(4), Some(9)));
        assert_eq!(metadata.bpm, Some(152.0));
        assert_eq!(metadata.artist.as_deref(), Some("One"));
        assert_eq!(metadata.tag("artist"), Some("Two"));
        assert_eq!(metadata.tag("ALBUMARTIST"), Some("Both"));
        assert!(metadata.pictures.is_empty());
        assert_eq!(metadata.tags.len(), 2);

        let tags = metadata.to_vorbis_comments();
        let again = Metadata::from_vorbis_comments(
            tags.iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        assert_eq!(again, metadata);
    }
}
