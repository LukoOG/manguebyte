//! The metadata chunks of AIFF files (Apple's "Audio Interchange File
//! Format" 1.3 of 1989, sections on the marker, instrument and text
//! chunks): `NAME`, `AUTH`, `(c) `, `ANNO`, `MARK` and `INST`.

use super::{Budget, ChunkId, CuePoint, Instrument, Loop, LoopKind, Metadata, RawChunk, text};

/// The chunks the writer makes itself: never kept, and never written from
/// raw chunks.
const STRUCTURAL: [&[u8; 4]; 3] = [b"COMM", b"SSND", b"FVER"];

/// The text chunks, which a tag of the same name is written as.
const TEXT: [&[u8; 4]; 4] = [b"NAME", b"AUTH", b"(c) ", b"ANNO"];

/// A marker: its number, its place between frames, and its name.
type Marker = (u16, u32, String);

/// Gathers the metadata of an AIFF file while its chunks are read.
pub(crate) struct IffMetadata {
    metadata: Metadata,
    budget: Budget,
    markers: Option<Vec<Marker>>,
    instrument: Option<[u8; 20]>,
}

impl IffMetadata {
    pub(crate) fn new() -> Self {
        Self {
            metadata: Metadata::default(),
            budget: Budget::new(),
            markers: None,
            instrument: None,
        }
    }

    /// Whether the body of a chunk is wanted, and if so counts it against
    /// what may be kept.
    pub(crate) fn wants(&mut self, id: &[u8; 4], len: u64) -> bool {
        !STRUCTURAL.contains(&id) && self.budget.take(len)
    }

    /// Takes in a chunk whose body [`wants`](Self::wants) asked for.
    pub(crate) fn chunk(&mut self, id: [u8; 4], body: &[u8]) {
        let metadata = &mut self.metadata;
        let slot = match &id {
            b"NAME" => Some(&mut metadata.title),
            b"AUTH" => Some(&mut metadata.artist),
            b"(c) " => Some(&mut metadata.copyright),
            b"ANNO" => Some(&mut metadata.comment),
            _ => None,
        };
        if let Some(slot) = slot {
            let value = text(body);
            match slot {
                None => *slot = Some(value),
                Some(_) => {
                    let name = String::from_utf8_lossy(&id).into_owned();
                    metadata.tags.push((name, value));
                }
            }
            return;
        }
        let understood = match &id {
            b"MARK" if self.markers.is_none() => {
                self.markers = Some(markers(body));
                true
            }
            b"INST" if self.instrument.is_none() && body.len() >= 20 => {
                let mut fixed = [0u8; 20];
                fixed.copy_from_slice(&body[..20]);
                self.instrument = Some(fixed);
                true
            }
            _ => false,
        };
        if !understood {
            self.metadata.chunks.push(RawChunk {
                id: ChunkId::Aiff(id),
                data: body.to_vec(),
            });
        }
    }

    pub(crate) fn finish(mut self) -> Metadata {
        let markers = self.markers.unwrap_or_default();
        let position = |id: u16| {
            markers
                .iter()
                .find(|(marker, _, _)| *marker == id)
                .map(|(_, at, _)| u64::from(*at))
        };
        let mut used = Vec::new();
        if let Some(inst) = self.instrument {
            let be16 = |at: usize| u16::from_be_bytes([inst[at], inst[at + 1]]);
            self.metadata.instrument = Some(Instrument {
                root_note: inst[0].min(127),
                fine_tune: f32::from(inst[1] as i8),
                gain_db: f32::from(be16(6) as i16),
                notes: (inst[2], inst[3]),
                velocities: (inst[4], inst[5]),
            });
            // The sustain loop, then the release loop.
            for at in [8, 14] {
                let kind = match be16(at) {
                    1 => LoopKind::Forward,
                    2 => LoopKind::PingPong,
                    _ => continue,
                };
                let (begin, end) = (be16(at + 2), be16(at + 4));
                if let (Some(start), Some(stop)) = (position(begin), position(end)) {
                    if stop > start {
                        used.extend([begin, end]);
                        self.metadata.loops.push(Loop {
                            start,
                            end: stop,
                            kind,
                            play_count: 0,
                        });
                    }
                }
            }
        }
        for (id, at, name) in &markers {
            if !used.contains(id) {
                self.metadata.cue_points.push(CuePoint {
                    id: u32::from(*id),
                    position: u64::from(*at),
                    length: 0,
                    label: (!name.is_empty()).then(|| name.clone()),
                });
            }
        }
        self.metadata
    }
}

/// The markers of a `MARK` chunk, up to the end of the chunk.
fn markers(body: &[u8]) -> Vec<Marker> {
    let Some(&[high, low]) = body.get(..2) else {
        return Vec::new();
    };
    let count = u16::from_be_bytes([high, low]);
    let mut out = Vec::new();
    let mut at = 2;
    for _ in 0..count {
        let Some(fixed) = body.get(at..at + 7) else {
            break;
        };
        let id = u16::from_be_bytes([fixed[0], fixed[1]]);
        let position = u32::from_be_bytes([fixed[2], fixed[3], fixed[4], fixed[5]]);
        let len = usize::from(fixed[6]);
        let Some(name) = body.get(at + 7..at + 7 + len) else {
            break;
        };
        out.push((id, position, text(name)));
        // A count byte and the text, padded to an even length.
        at += 6 + (1 + len).next_multiple_of(2);
    }
    out
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) -> Result<(), &'static str> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&len| len < u32::MAX - 1)
        .ok_or("a metadata chunk is longer than 4 GiB")?;
    out.extend_from_slice(id);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    if len % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

/// The metadata chunks of an AIFF file holding `metadata`, each padded to
/// an even length; empty when there is nothing to write.
pub(crate) fn build_iff(metadata: &Metadata) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    let texts = [
        (b"NAME", &metadata.title),
        (b"AUTH", &metadata.artist),
        (b"(c) ", &metadata.copyright),
        (b"ANNO", &metadata.comment),
    ];
    for (id, value) in texts {
        if let Some(value) = value {
            push_chunk(&mut out, id, value.as_bytes())?;
        }
    }
    for (name, value) in &metadata.tags {
        if let Some(id) = TEXT.iter().find(|id| id.as_slice() == name.as_bytes()) {
            push_chunk(&mut out, id, value.as_bytes())?;
        }
    }

    // Markers: the cue points, numbered from 1 to 32767 as the format
    // wants, then the two ends of each loop.
    // A number a cue point has is kept if it is valid and the first of its
    // kind; the others get the lowest numbers left.
    let mut ids: Vec<Option<u16>> = Vec::with_capacity(metadata.cue_points.len());
    for point in &metadata.cue_points {
        let id = u16::try_from(point.id)
            .ok()
            .filter(|id| (1..=32_767).contains(id) && !ids.contains(&Some(*id)));
        ids.push(id);
    }
    let mut taken: Vec<u16> = ids.iter().flatten().copied().collect();
    let mut next = 1u16;
    let mut fresh = |taken: &mut Vec<u16>| -> Result<u16, &'static str> {
        while taken.contains(&next) {
            next = next
                .checked_add(1)
                .filter(|&id| id <= 32_767)
                .ok_or("too many markers")?;
        }
        taken.push(next);
        Ok(next)
    };
    let mut markers: Vec<(u16, u64, &str)> = Vec::new();
    for (point, id) in metadata.cue_points.iter().zip(ids) {
        let id = match id {
            Some(id) => id,
            None => fresh(&mut taken)?,
        };
        markers.push((id, point.position, point.label.as_deref().unwrap_or("")));
    }
    let loops: Vec<&Loop> = metadata.loops.iter().take(2).collect();
    let mut loop_markers = Vec::new();
    for entry in &loops {
        if entry.end <= entry.start {
            return Err("a loop must end after it starts");
        }
        let begin = fresh(&mut taken)?;
        markers.push((begin, entry.start, ""));
        let end = fresh(&mut taken)?;
        markers.push((end, entry.end, ""));
        loop_markers.push((entry.kind, begin, end));
    }
    if !markers.is_empty() {
        let mut body = (markers.len() as u16).to_be_bytes().to_vec();
        for (id, position, name) in &markers {
            let position =
                u32::try_from(*position).map_err(|_| "a marker lies past frame 2^32 - 1")?;
            let len = u8::try_from(name.len()).map_err(|_| "a marker name is over 255 bytes")?;
            body.extend_from_slice(&id.to_be_bytes());
            body.extend_from_slice(&position.to_be_bytes());
            body.push(len);
            body.extend_from_slice(name.as_bytes());
            if name.len() % 2 == 0 {
                body.push(0);
            }
        }
        push_chunk(&mut out, b"MARK", &body)?;
    }

    if metadata.instrument.is_some() || !loop_markers.is_empty() {
        let instrument = metadata.instrument.unwrap_or_default();
        let mut body = vec![
            instrument.root_note.min(127),
            instrument.fine_tune.round().clamp(-50.0, 50.0) as i8 as u8,
            instrument.notes.0,
            instrument.notes.1,
            instrument.velocities.0,
            instrument.velocities.1,
        ];
        let gain = instrument.gain_db.round().clamp(-32_768.0, 32_767.0) as i16;
        body.extend_from_slice(&gain.to_be_bytes());
        for index in 0..2 {
            match loop_markers.get(index) {
                Some(&(kind, begin, end)) => {
                    // AIFF plays loops forward or back and forth only.
                    let mode: u16 = if kind == LoopKind::PingPong { 2 } else { 1 };
                    for value in [mode, begin, end] {
                        body.extend_from_slice(&value.to_be_bytes());
                    }
                }
                None => body.extend_from_slice(&[0; 6]),
            }
        }
        push_chunk(&mut out, b"INST", &body)?;
    }

    for chunk in &metadata.chunks {
        let ChunkId::Aiff(id) = chunk.id else {
            continue;
        };
        if STRUCTURAL.contains(&&id) {
            return Err("a raw chunk cannot be one the writer makes itself");
        }
        push_chunk(&mut out, &id, &chunk.data)?;
    }
    Ok(out)
}

/// Hands a chunk to `reader` the way the AIFF reader does.
#[cfg(test)]
fn read_iff_chunk(reader: &mut IffMetadata, id: [u8; 4], body: &[u8]) {
    if reader.wants(&id, body.len() as u64) {
        reader.chunk(id, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_back(mut chunks: &[u8]) -> Metadata {
        let mut reader = IffMetadata::new();
        while chunks.len() >= 8 {
            let id = [chunks[0], chunks[1], chunks[2], chunks[3]];
            let len = u32::from_be_bytes([chunks[4], chunks[5], chunks[6], chunks[7]]) as usize;
            let Some(body) = chunks.get(8..8 + len) else {
                break;
            };
            read_iff_chunk(&mut reader, id, body);
            chunks = &chunks[(8 + len + len % 2).min(chunks.len())..];
        }
        reader.finish()
    }

    #[test]
    fn markers_loops_and_text_survive_a_round_trip() {
        let metadata = Metadata {
            title: Some("Ciranda".into()),
            artist: Some("Lia".into()),
            copyright: Some("(C) 2026".into()),
            comment: Some("first".into()),
            tags: vec![("ANNO".into(), "second".into())],
            instrument: Some(Instrument {
                root_note: 62,
                fine_tune: -7.0,
                gain_db: -6.0,
                notes: (50, 74),
                velocities: (10, 127),
            }),
            loops: vec![
                Loop::forward(1_000, 2_000),
                Loop {
                    start: 3_000,
                    end: 3_500,
                    kind: LoopKind::PingPong,
                    play_count: 0,
                },
            ],
            cue_points: vec![
                CuePoint::new(5, 10).with_label("Even"),
                CuePoint::new(9, 20).with_label("Odd"),
                CuePoint::new(3, 30),
            ],
            chunks: vec![RawChunk {
                id: ChunkId::Aiff(*b"APPL"),
                data: b"pdos1".to_vec(),
            }],
            ..Metadata::default()
        };
        let chunks = build_iff(&metadata).unwrap();
        assert_eq!(read_back(&chunks), metadata);
    }

    #[test]
    fn markers_are_numbered_as_aiff_wants() {
        let metadata = Metadata {
            cue_points: vec![
                CuePoint::new(0, 1),
                CuePoint::new(1, 2),
                CuePoint::new(1, 3),
            ],
            ..Metadata::default()
        };
        let again = read_back(&build_iff(&metadata).unwrap());
        let ids: Vec<u32> = again.cue_points.iter().map(|point| point.id).collect();
        assert_eq!(ids, [2, 1, 3]);
        let positions: Vec<u64> = again
            .cue_points
            .iter()
            .map(|point| point.position)
            .collect();
        assert_eq!(positions, [1, 2, 3]);
    }

    #[test]
    fn damaged_chunks_never_panic() {
        let metadata = Metadata {
            loops: vec![Loop::forward(1, 2)],
            cue_points: vec![CuePoint::new(5, 10).with_label("Name")],
            ..Metadata::default()
        };
        let chunks = build_iff(&metadata).unwrap();
        for cut in 0..chunks.len() {
            let mut damaged = chunks.clone();
            damaged[cut] ^= 0xA5;
            let _ = read_back(&damaged[..]);
        }
    }
}
