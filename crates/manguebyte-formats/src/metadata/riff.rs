//! The metadata chunks of WAV files: `LIST/INFO` (the RIFF specification
//! of 1991, "Multimedia Programming Interface and Data Specifications 1.0",
//! section 3), `cue `, `LIST/adtl`, `smpl` and `inst` (the same, and its
//! 1994 update), `bext` (EBU Tech 3285), `iXML` (the iXML specification),
//! and the tempo of `acid`.

use super::{
    Broadcast, Budget, ChunkId, CueLabels, CuePoint, Instrument, Loop, LoopKind, Loudness,
    Metadata, RawChunk, text, track_numbers,
};

/// The chunks the writers make themselves, or that hold nothing: never kept
/// as raw chunks, and never written from them.
const STRUCTURAL: [&[u8; 4]; 8] = [
    b"fmt ", b"data", b"ds64", b"fact", b"JUNK", b"junk", b"PAD ", b"FLLR",
];

/// Bytes of a `bext` chunk before its coding history.
const BEXT_FIXED: usize = 602;

/// Gathers the metadata of a WAV file while its chunks are read.
pub(crate) struct RiffMetadata {
    metadata: Metadata,
    labels: CueLabels,
    budget: Budget,
    /// The root note and fine tune of a `smpl` chunk.
    sampler: Option<(u8, f32)>,
}

impl RiffMetadata {
    pub(crate) fn new() -> Self {
        Self {
            metadata: Metadata::default(),
            labels: CueLabels::default(),
            budget: Budget::new(),
            sampler: None,
        }
    }

    /// Whether the body of a chunk is wanted, and if so counts it against
    /// what may be kept.
    pub(crate) fn wants(&mut self, id: &[u8; 4], len: u64) -> bool {
        !STRUCTURAL.contains(&id) && self.budget.take(len)
    }

    /// Takes in a chunk whose body [`wants`](Self::wants) asked for.
    pub(crate) fn chunk(&mut self, id: [u8; 4], body: &[u8]) {
        let understood = match &id {
            b"LIST" => self.list(body),
            b"bext" => self.bext(body),
            b"iXML" if self.metadata.ixml.is_none() => {
                self.metadata.ixml = Some(text(body));
                true
            }
            b"cue " if self.metadata.cue_points.is_empty() => self.cue(body),
            b"smpl" if self.metadata.loops.is_empty() && self.sampler.is_none() => self.smpl(body),
            b"inst" if self.metadata.instrument.is_none() => self.inst(body),
            b"acid" => {
                // The tempo is read, and the chunk kept whole for the rest.
                if let Some(tempo) = body.get(20..24) {
                    let tempo = f32::from_le_bytes([tempo[0], tempo[1], tempo[2], tempo[3]]);
                    if tempo.is_finite() && tempo > 0.0 && self.metadata.bpm.is_none() {
                        self.metadata.bpm = Some(tempo);
                    }
                }
                false
            }
            _ => false,
        };
        if !understood {
            self.metadata.chunks.push(RawChunk {
                id: ChunkId::Riff(id),
                data: body.to_vec(),
            });
        }
    }

    pub(crate) fn finish(mut self) -> Metadata {
        self.labels.apply(&mut self.metadata.cue_points);
        if let Some((root_note, fine_tune)) = self.sampler {
            if self.metadata.instrument.is_none() {
                self.metadata.instrument = Some(Instrument {
                    root_note,
                    fine_tune,
                    ..Instrument::default()
                });
            }
        }
        self.metadata
    }

    fn list(&mut self, body: &[u8]) -> bool {
        match body.get(..4) {
            Some(b"INFO") => {
                for (id, value) in sub_chunks(&body[4..]) {
                    self.info(id, &text(value));
                }
                true
            }
            Some(b"adtl") => {
                for (id, value) in sub_chunks(&body[4..]) {
                    let Some(cue) = value.get(..4).map(u32_le) else {
                        continue;
                    };
                    match &id {
                        b"labl" => {
                            self.labels.labels.entry(cue).or_insert(text(&value[4..]));
                        }
                        b"ltxt" => {
                            if let Some(length) = value.get(4..8).map(u32_le) {
                                self.labels.lengths.entry(cue).or_insert(u64::from(length));
                            }
                        }
                        _ => {}
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn info(&mut self, id: [u8; 4], value: &str) {
        let metadata = &mut self.metadata;
        // FFmpeg writes the track number as `IPRT`, which RIFF defines for
        // the part of a series, and others as `ITRK`.
        if (&id == b"ITRK" || &id == b"IPRT") && metadata.track.is_none() {
            let (track, total) = track_numbers(value);
            if track.is_some() {
                metadata.track = track;
                metadata.track_total = metadata.track_total.or(total);
                return;
            }
        }
        let slot = match &id {
            b"INAM" => Some(&mut metadata.title),
            b"IART" => Some(&mut metadata.artist),
            b"IPRD" => Some(&mut metadata.album),
            b"ICRD" => Some(&mut metadata.date),
            b"IGNR" => Some(&mut metadata.genre),
            b"ICMT" => Some(&mut metadata.comment),
            b"ICOP" => Some(&mut metadata.copyright),
            _ => None,
        };
        match slot {
            Some(slot) if slot.is_none() => *slot = Some(value.to_owned()),
            _ => {
                let name = String::from_utf8_lossy(&id).into_owned();
                metadata.tags.push((name, value.to_owned()));
            }
        }
    }

    fn bext(&mut self, body: &[u8]) -> bool {
        if self.metadata.broadcast.is_some() || body.len() < 348 {
            return false;
        }
        // A chunk of an early version may stop short of the reserved bytes.
        let mut fixed = [0u8; BEXT_FIXED];
        let have = body.len().min(BEXT_FIXED);
        fixed[..have].copy_from_slice(&body[..have]);
        let originator = text(&fixed[256..288]);
        let low = u32_le(&fixed[338..342]);
        let high = u32_le(&fixed[342..346]);
        let version = u16::from_le_bytes([fixed[346], fixed[347]]);
        let loudness = (version >= 2).then(|| {
            let value =
                |at: usize| f32::from(i16::from_le_bytes([fixed[at], fixed[at + 1]])) / 100.0;
            Loudness {
                integrated: value(412),
                range: value(414),
                max_true_peak: value(416),
                max_momentary: value(418),
                max_short_term: value(420),
            }
        });
        self.metadata.originator = (!originator.is_empty()).then_some(originator);
        self.metadata.time_reference = Some(u64::from(high) << 32 | u64::from(low));
        self.metadata.broadcast = Some(Broadcast {
            description: text(&fixed[..256]),
            originator_reference: text(&fixed[288..320]),
            origination_date: text(&fixed[320..330]),
            origination_time: text(&fixed[330..338]),
            version,
            umid: fixed[348..412].to_vec(),
            loudness,
            coding_history: body.get(BEXT_FIXED..).map(text).unwrap_or_default(),
        });
        true
    }

    fn cue(&mut self, body: &[u8]) -> bool {
        let Some(count) = body.get(..4).map(u32_le) else {
            return false;
        };
        // No more points are read than the chunk holds.
        for point in body[4..].chunks_exact(24).take(count as usize) {
            let id = u32_le(&point[0..4]);
            let offset = u32_le(&point[20..24]);
            self.metadata
                .cue_points
                .push(CuePoint::new(id, u64::from(offset)));
        }
        true
    }

    fn smpl(&mut self, body: &[u8]) -> bool {
        if body.len() < 36 {
            return false;
        }
        let unity = u32_le(&body[12..16]);
        let fraction = u32_le(&body[16..20]);
        let count = u32_le(&body[28..32]);
        for entry in body[36..].chunks_exact(24).take(count as usize) {
            let kind = match u32_le(&entry[4..8]) {
                0 => LoopKind::Forward,
                1 => LoopKind::PingPong,
                2 => LoopKind::Backward,
                other => LoopKind::Other(other),
            };
            let start = u64::from(u32_le(&entry[8..12]));
            // The end the chunk states is the last frame of the loop.
            let end = u64::from(u32_le(&entry[12..16])) + 1;
            if end > start {
                self.metadata.loops.push(Loop {
                    start,
                    end,
                    kind,
                    play_count: u32_le(&entry[20..24]),
                });
            }
        }
        let root = u8::try_from(unity).unwrap_or(60).min(127);
        // A fraction of a semitone in 32 bits: 0x8000_0000 is 50 cents.
        let cents = (f64::from(fraction) / 4_294_967_296.0 * 100.0) as f32;
        self.sampler = Some((root, cents));
        true
    }

    fn inst(&mut self, body: &[u8]) -> bool {
        let Some(
            &[
                note,
                tune,
                gain,
                low_note,
                high_note,
                low_velocity,
                high_velocity,
            ],
        ) = body.get(..7)
        else {
            return false;
        };
        self.metadata.instrument = Some(Instrument {
            root_note: note.min(127),
            fine_tune: f32::from(tune as i8),
            gain_db: f32::from(gain as i8),
            notes: (low_note, high_note),
            velocities: (low_velocity, high_velocity),
        });
        true
    }
}

fn u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The chunks inside a list, up to the end of the list or the first one
/// that runs past it.
fn sub_chunks(mut body: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let header = body.get(..8)?;
        let id = [header[0], header[1], header[2], header[3]];
        let len = u32_le(&header[4..8]) as usize;
        let value = body.get(8..8usize.checked_add(len)?)?;
        let next = (8 + len + len % 2).min(body.len());
        body = &body[next..];
        Some((id, value))
    })
}

/// Appends a chunk, padded to an even length.
fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) -> Result<(), &'static str> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&len| len < u32::MAX - 1)
        .ok_or("a metadata chunk is longer than 4 GiB")?;
    out.extend_from_slice(id);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    if len % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

/// Text for a chunk that ends it with a NUL.
fn terminated(text: &str) -> Result<Vec<u8>, &'static str> {
    if text.contains('\0') {
        return Err("text for a WAV file cannot hold a NUL character");
    }
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    Ok(bytes)
}

/// Text in a field of `len` bytes, padded with NULs.
fn fixed_text(out: &mut Vec<u8>, text: &str, len: usize) -> Result<(), &'static str> {
    if text.len() > len {
        return Err("text is too long for its field in the bext chunk");
    }
    if text.contains('\0') {
        return Err("text for a WAV file cannot hold a NUL character");
    }
    out.extend_from_slice(text.as_bytes());
    out.resize(out.len() + len - text.len(), 0);
    Ok(())
}

/// A frame number as the 32 bits of a WAV chunk.
fn frame32(frame: u64) -> Result<u32, &'static str> {
    u32::try_from(frame).map_err(|_| "a loop or cue point lies past frame 2^32 - 1")
}

/// The metadata chunks of a WAV file holding `metadata`, one after the
/// other, each padded to an even length; empty when there is nothing to
/// write. An error names what cannot be written.
pub(crate) fn build_riff(metadata: &Metadata, sample_rate: u32) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();

    // LIST/INFO.
    let mut info = b"INFO".to_vec();
    let fields: [(&[u8; 4], Option<String>); 8] = [
        (b"INAM", metadata.title.clone()),
        (b"IART", metadata.artist.clone()),
        (b"IPRD", metadata.album.clone()),
        (b"ICRD", metadata.date.clone()),
        (
            b"ITRK",
            metadata.track.map(|track| match metadata.track_total {
                Some(total) => format!("{track}/{total}"),
                None => track.to_string(),
            }),
        ),
        (b"IGNR", metadata.genre.clone()),
        (b"ICMT", metadata.comment.clone()),
        (b"ICOP", metadata.copyright.clone()),
    ];
    for (id, value) in fields {
        if let Some(value) = value {
            push_chunk(&mut info, id, &terminated(&value)?)?;
        }
    }
    for (name, value) in &metadata.tags {
        // Only names that are `INFO` identifiers belong here.
        let bytes = name.as_bytes();
        if let &[b'I', b, c, d] = bytes {
            if [b, c, d].iter().all(u8::is_ascii_alphanumeric) {
                push_chunk(&mut info, &[b'I', b, c, d], &terminated(value)?)?;
            }
        }
    }
    if info.len() > 4 {
        push_chunk(&mut out, b"LIST", &info)?;
    }

    // bext.
    if metadata.originator.is_some()
        || metadata.time_reference.is_some()
        || metadata.broadcast.is_some()
    {
        let default = Broadcast::default();
        let broadcast = metadata.broadcast.as_ref().unwrap_or(&default);
        let mut body = Vec::with_capacity(BEXT_FIXED + broadcast.coding_history.len());
        fixed_text(&mut body, &broadcast.description, 256)?;
        fixed_text(&mut body, metadata.originator.as_deref().unwrap_or(""), 32)?;
        fixed_text(&mut body, &broadcast.originator_reference, 32)?;
        fixed_text(&mut body, &broadcast.origination_date, 10)?;
        fixed_text(&mut body, &broadcast.origination_time, 8)?;
        let reference = metadata.time_reference.unwrap_or(0);
        body.extend_from_slice(&(reference as u32).to_le_bytes());
        body.extend_from_slice(&((reference >> 32) as u32).to_le_bytes());
        // Loudness needs version 2; without it, the bytes are reserved,
        // which versions 0 and 1 leave as zeros.
        let version = match broadcast.loudness {
            Some(_) => broadcast.version.max(2),
            None => broadcast.version,
        };
        body.extend_from_slice(&version.to_le_bytes());
        match broadcast.umid.len() {
            0 => body.extend_from_slice(&[0; 64]),
            64 => body.extend_from_slice(&broadcast.umid),
            _ => return Err("a UMID is 64 bytes"),
        }
        match broadcast.loudness {
            Some(loudness) => {
                for value in [
                    loudness.integrated,
                    loudness.range,
                    loudness.max_true_peak,
                    loudness.max_momentary,
                    loudness.max_short_term,
                ] {
                    let hundredths = (f64::from(value) * 100.0).round();
                    if !(-32_768.0..=32_767.0).contains(&hundredths) {
                        return Err("a loudness value is outside what bext can state");
                    }
                    body.extend_from_slice(&(hundredths as i16).to_le_bytes());
                }
            }
            None => body.extend_from_slice(&[0; 10]),
        }
        body.resize(BEXT_FIXED, 0);
        if broadcast.coding_history.contains('\0') {
            return Err("text for a WAV file cannot hold a NUL character");
        }
        body.extend_from_slice(broadcast.coding_history.as_bytes());
        push_chunk(&mut out, b"bext", &body)?;
    }

    if let Some(ixml) = &metadata.ixml {
        push_chunk(&mut out, b"iXML", ixml.as_bytes())?;
    }

    // cue, and the labels and lengths of its points in LIST/adtl.
    if !metadata.cue_points.is_empty() {
        let count = u32::try_from(metadata.cue_points.len()).map_err(|_| "too many cue points")?;
        let mut body = count.to_le_bytes().to_vec();
        let mut adtl = b"adtl".to_vec();
        for point in &metadata.cue_points {
            let position = frame32(point.position)?;
            body.extend_from_slice(&point.id.to_le_bytes());
            body.extend_from_slice(&position.to_le_bytes());
            body.extend_from_slice(b"data");
            // The chunk and block starts are zero in a file of one data
            // chunk of PCM; the sample offset is the frame.
            body.extend_from_slice(&[0; 8]);
            body.extend_from_slice(&position.to_le_bytes());
            if let Some(label) = &point.label {
                let mut entry = point.id.to_le_bytes().to_vec();
                entry.extend_from_slice(&terminated(label)?);
                push_chunk(&mut adtl, b"labl", &entry)?;
            }
            if point.length > 0 {
                let mut entry = point.id.to_le_bytes().to_vec();
                entry.extend_from_slice(&frame32(point.length)?.to_le_bytes());
                // A region: purpose, country, language, dialect and code
                // page, none of which is known.
                entry.extend_from_slice(b"rgn ");
                entry.extend_from_slice(&[0; 8]);
                push_chunk(&mut adtl, b"ltxt", &entry)?;
            }
        }
        push_chunk(&mut out, b"cue ", &body)?;
        if adtl.len() > 4 {
            push_chunk(&mut out, b"LIST", &adtl)?;
        }
    }

    // smpl: the loops, and the root note.
    if !metadata.loops.is_empty() {
        let instrument = metadata.instrument.unwrap_or_default();
        let mut body = Vec::with_capacity(36 + 24 * metadata.loops.len());
        let period = 1_000_000_000u32.checked_div(sample_rate).unwrap_or(0);
        let mut note = u32::from(instrument.root_note.min(127));
        let mut cents = f64::from(instrument.fine_tune);
        if cents < 0.0 {
            note = note.saturating_sub(1);
            cents += 100.0;
        }
        let fraction = (cents.clamp(0.0, 99.999) / 100.0 * 4_294_967_296.0) as u32;
        let count = u32::try_from(metadata.loops.len()).map_err(|_| "too many loops")?;
        for value in [0, 0, period, note, fraction, 0, 0, count, 0] {
            body.extend_from_slice(&u32::to_le_bytes(value));
        }
        for (index, entry) in metadata.loops.iter().enumerate() {
            if entry.end <= entry.start {
                return Err("a loop must end after it starts");
            }
            let kind = match entry.kind {
                LoopKind::Forward => 0,
                LoopKind::PingPong => 1,
                LoopKind::Backward => 2,
                LoopKind::Other(kind) => kind,
            };
            let start = frame32(entry.start)?;
            let last = frame32(entry.end - 1)?;
            for value in [index as u32, kind, start, last, 0, entry.play_count] {
                body.extend_from_slice(&value.to_le_bytes());
            }
        }
        push_chunk(&mut out, b"smpl", &body)?;
    }

    if let Some(instrument) = metadata.instrument {
        let tune = instrument.fine_tune.round().clamp(-50.0, 50.0) as i8;
        let gain = instrument.gain_db.round().clamp(-64.0, 64.0) as i8;
        push_chunk(
            &mut out,
            b"inst",
            &[
                instrument.root_note.min(127),
                tune as u8,
                gain as u8,
                instrument.notes.0,
                instrument.notes.1,
                instrument.velocities.0,
                instrument.velocities.1,
            ],
        )?;
    }

    for chunk in &metadata.chunks {
        let ChunkId::Riff(id) = chunk.id else {
            continue;
        };
        if STRUCTURAL.contains(&&id) {
            return Err("a raw chunk cannot be one the writer makes itself");
        }
        if &id == b"acid" && chunk.data.len() >= 24 {
            // The tempo is the field's, the rest the chunk's.
            let mut data = chunk.data.clone();
            if let Some(bpm) = metadata.bpm {
                data[20..24].copy_from_slice(&bpm.to_le_bytes());
            }
            push_chunk(&mut out, &id, &data)?;
        } else {
            push_chunk(&mut out, &id, &chunk.data)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads chunks back the way the WAV reader hands them over.
    fn read_back(chunks: &[u8]) -> Metadata {
        let mut reader = RiffMetadata::new();
        for (id, body) in sub_chunks(chunks) {
            if reader.wants(&id, body.len() as u64) {
                reader.chunk(id, body);
            }
        }
        reader.finish()
    }

    fn everything() -> Metadata {
        Metadata {
            title: Some("Maracatu".into()),
            artist: Some("Nação".into()),
            album: Some("Baque".into()),
            date: Some("2026-10-17".into()),
            track: Some(3),
            track_total: Some(12),
            genre: Some("Maracatu".into()),
            comment: Some("Odd length".into()),
            copyright: Some("(C) 2026".into()),
            originator: Some("Manguebyte".into()),
            time_reference: Some(0x1_2345_6789),
            broadcast: Some(Broadcast {
                description: "A take".into(),
                originator_reference: "MB-0001".into(),
                origination_date: "2026-10-17".into(),
                origination_time: "12:34:56".into(),
                version: 2,
                umid: (0..64).collect(),
                loudness: Some(Loudness {
                    integrated: -23.0,
                    range: 7.5,
                    max_true_peak: -1.25,
                    max_momentary: -15.0,
                    max_short_term: -18.5,
                }),
                coding_history: "A=PCM,F=48000,W=24,M=stereo\r\n".into(),
            }),
            ixml: Some("<BWFXML><PROJECT>Baque</PROJECT></BWFXML>".into()),
            instrument: Some(Instrument {
                root_note: 57,
                fine_tune: 12.0,
                gain_db: -3.0,
                notes: (40, 70),
                velocities: (1, 100),
            }),
            loops: vec![
                Loop::forward(100, 900),
                Loop {
                    start: 0,
                    end: 1,
                    kind: LoopKind::PingPong,
                    play_count: 4,
                },
                Loop {
                    start: 5,
                    end: 4_000_000_000,
                    kind: LoopKind::Other(40),
                    play_count: 0,
                },
            ],
            cue_points: vec![
                CuePoint::new(1, 400).with_label("Chorus"),
                CuePoint {
                    id: 7,
                    position: 1_000,
                    length: 250,
                    label: None,
                },
            ],
            tags: vec![
                ("ISFT".into(), "Manguebyte".into()),
                ("ISRC".into(), "the source".into()),
            ],
            chunks: vec![RawChunk {
                id: ChunkId::Riff(*b"umid"),
                data: vec![1, 2, 3],
            }],
            ..Metadata::default()
        }
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let metadata = everything();
        let chunks = build_riff(&metadata, 48_000).unwrap();
        assert_eq!(read_back(&chunks), metadata);
        // Nothing to write writes nothing.
        assert!(build_riff(&Metadata::default(), 48_000).unwrap().is_empty());
    }

    #[test]
    fn the_bext_chunk_is_laid_out_as_tech_3285_says() {
        let chunks = build_riff(&everything(), 48_000).unwrap();
        let (_, bext) = sub_chunks(&chunks).find(|(id, _)| id == b"bext").unwrap();
        assert_eq!(bext.len(), 602 + 29);
        assert_eq!(&bext[..6], b"A take");
        assert_eq!(&bext[256..266], b"Manguebyte");
        assert_eq!(&bext[320..330], b"2026-10-17");
        assert_eq!(u32_le(&bext[338..342]), 0x2345_6789);
        assert_eq!(u32_le(&bext[342..346]), 1);
        assert_eq!(u16::from_le_bytes([bext[346], bext[347]]), 2);
        assert_eq!(i16::from_le_bytes([bext[412], bext[413]]), -2_300);
        assert_eq!(i16::from_le_bytes([bext[416], bext[417]]), -125);
        assert!(bext[422..602].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn a_smpl_chunk_states_the_last_frame_of_each_loop() {
        let metadata = Metadata {
            loops: vec![Loop::forward(10, 90)],
            ..Metadata::default()
        };
        let chunks = build_riff(&metadata, 44_100).unwrap();
        let (_, smpl) = sub_chunks(&chunks).find(|(id, _)| id == b"smpl").unwrap();
        assert_eq!(u32_le(&smpl[8..12]), 22_675, "sample period in nanoseconds");
        assert_eq!(u32_le(&smpl[12..16]), 60);
        assert_eq!(u32_le(&smpl[28..32]), 1);
        assert_eq!(u32_le(&smpl[44..48]), 10);
        assert_eq!(u32_le(&smpl[48..52]), 89);
        // A loop alone gives the root note smpl always states.
        let again = read_back(&chunks);
        assert_eq!(again.loops, metadata.loops);
        assert_eq!(again.instrument.map(|i| i.root_note), Some(60));
    }

    #[test]
    fn what_cannot_be_written_is_an_error() {
        let bad = [
            Metadata {
                title: Some("a\0b".into()),
                ..Metadata::default()
            },
            Metadata {
                loops: vec![Loop::forward(5, 5)],
                ..Metadata::default()
            },
            Metadata {
                cue_points: vec![CuePoint::new(1, 1 << 32)],
                ..Metadata::default()
            },
            Metadata {
                originator: Some("x".repeat(33)),
                ..Metadata::default()
            },
            Metadata {
                chunks: vec![RawChunk {
                    id: ChunkId::Riff(*b"data"),
                    data: vec![],
                }],
                ..Metadata::default()
            },
        ];
        for metadata in bad {
            assert!(build_riff(&metadata, 48_000).is_err(), "{metadata:?}");
        }
    }

    #[test]
    fn damaged_chunks_are_kept_raw_or_cut_short_never_a_panic() {
        let chunks = build_riff(&everything(), 48_000).unwrap();
        for (id, body) in sub_chunks(&chunks) {
            for len in 0..body.len().min(700) {
                let mut reader = RiffMetadata::new();
                reader.chunk(id, &body[..len]);
                let _ = reader.finish();
            }
        }
        // A cue chunk claiming four billion points holds none.
        let mut reader = RiffMetadata::new();
        reader.chunk(*b"cue ", &u32::MAX.to_le_bytes());
        assert!(reader.finish().cue_points.is_empty());
    }
}
