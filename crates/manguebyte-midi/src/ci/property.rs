//! Property exchange data: cut into chunks that fit the receiver, put back
//! together on arrival, and binary data carried in seven bits.
//!
//! A property exchange message carries a JSON header and a stretch of
//! data. When the data does not fit in one system exclusive message of
//! the size the receiver announced at discovery, it goes in chunks,
//! numbered from 1, the header in the first only ("Common Rules for
//! MIDI-CI Property Exchange", M2-103-UM).
//!
//! - [`chunks`] cuts data for a receiver's maximum message size.
//! - [`Assembler`] puts chunks back together, with a bound on what it
//!   holds, and tells a sender who skipped or repeated a chunk.
//! - [`mcoded7_encode`] and [`mcoded7_decode`] carry 8-bit data in 7-bit
//!   bytes, as the "Mcoded7" encoding of M2-103-UM does.

use super::{CiError, Muid, PropertyChunk, PropertyKind};

/// Bytes of a property exchange message besides its header and data:
/// `0xF0`, the universal id, the device id, the two sub-ids, the version,
/// two MUIDs, the request id, three 14-bit lengths and counts... and the
/// `0xF7`.
pub const OVERHEAD: usize = 1 + 1 + 1 + 1 + 1 + 1 + 4 + 4 + 1 + 2 + 2 + 2 + 2 + 1;

/// The largest header or chunk of data a message can carry: its length is
/// fourteen bits.
pub const MAX_FIELD: usize = 0x3FFF;

/// Cuts `data` into the chunks of one property exchange message for a
/// receiver whose system exclusive messages may be up to `max_sysex`
/// bytes long, the header in the first chunk.
///
/// Every chunk, framed, fits in `max_sysex`. An empty `data` is one chunk
/// with no data, as an inquiry is. Fails if the header alone does not fit,
/// or if the data needs more than 16383 chunks.
pub fn chunks<'a>(
    request_id: u8,
    header: &'a [u8],
    data: &'a [u8],
    max_sysex: usize,
) -> Result<impl Iterator<Item = PropertyChunk<'a>> + 'a, CiError> {
    if header.len() > MAX_FIELD || header.len() + OVERHEAD >= max_sysex {
        return Err(CiError::Invalid(
            "the property header does not fit in a message",
        ));
    }
    // The first chunk has room for less data than the others, which have
    // no header.
    let first = (max_sysex - OVERHEAD - header.len()).min(MAX_FIELD);
    let rest = (max_sysex - OVERHEAD).min(MAX_FIELD);
    let after_first = data.len().saturating_sub(first);
    let count = 1 + after_first.div_ceil(rest);
    let count = u16::try_from(count)
        .ok()
        .filter(|count| usize::from(*count) <= MAX_FIELD)
        .ok_or(CiError::Invalid(
            "property data needs more than 16383 chunks",
        ))?;
    let (head, tail) = data.split_at(first.min(data.len()));
    let first_chunk = PropertyChunk {
        request_id,
        header,
        chunks: count,
        chunk: 1,
        data: head,
    };
    let others = tail
        .chunks(rest)
        .enumerate()
        .map(move |(index, piece)| PropertyChunk {
            request_id,
            header: &[],
            chunks: count,
            chunk: index as u16 + 2,
            data: piece,
        });
    Ok(core::iter::once(first_chunk).chain(others))
}

/// A whole property exchange message, put back together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// Who sent it.
    pub source: Muid,
    /// Which message it is.
    pub kind: PropertyKind,
    /// The request it belongs to.
    pub request_id: u8,
    /// The JSON header.
    pub header: Vec<u8>,
    /// All of the data.
    pub data: Vec<u8>,
}

/// What [`Assembler::push`] made of a chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// More chunks are to come.
    Pending,
    /// That was the last chunk.
    Complete(Assembled),
    /// The chunk did not follow the one before it, or came with no first
    /// chunk; what had arrived of the message is dropped. The sender
    /// should be told with [`nak::CHUNK_OUT_OF_SEQUENCE`](super::nak::CHUNK_OUT_OF_SEQUENCE).
    OutOfSequence,
    /// The message would be larger than the assembler takes; what had
    /// arrived of it is dropped.
    TooLarge,
}

#[derive(Debug, Clone)]
struct Partial {
    source: Muid,
    kind: PropertyKind,
    request_id: u8,
    header: Vec<u8>,
    data: Vec<u8>,
    next: u16,
    chunks: u16,
    /// When it was started, in the assembler's own count of messages, to
    /// make room by dropping the oldest.
    started: u64,
}

/// Puts the chunks of property exchange messages back together, several
/// at once, each keyed by its sender, kind and request id.
///
/// It holds at most `max_messages` messages at a time, dropping the oldest
/// unfinished one to start another, and at most `max_bytes` of header and
/// data in each, so hostile input cannot make it grow without bound.
#[derive(Debug, Clone)]
pub struct Assembler {
    partial: Vec<Partial>,
    max_messages: usize,
    max_bytes: usize,
    clock: u64,
}

impl Assembler {
    /// An assembler for up to `max_messages` messages at a time (at least
    /// one) of up to `max_bytes` each.
    pub fn new(max_messages: usize, max_bytes: usize) -> Self {
        Self {
            partial: Vec::new(),
            max_messages: max_messages.max(1),
            max_bytes,
            clock: 0,
        }
    }

    /// How many messages are partly received.
    pub fn pending(&self) -> usize {
        self.partial.len()
    }

    /// Drops whatever `source` was sending.
    pub fn forget(&mut self, source: Muid) {
        self.partial.retain(|partial| partial.source != source);
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.partial.clear();
    }

    /// Takes one chunk from `source`.
    pub fn push(
        &mut self,
        source: Muid,
        kind: PropertyKind,
        chunk: &PropertyChunk<'_>,
    ) -> Progress {
        self.clock += 1;
        let key = |partial: &Partial| {
            partial.source == source
                && partial.kind == kind
                && partial.request_id == chunk.request_id
        };
        let found = self.partial.iter().position(key);
        if chunk.chunk == 1 {
            // A first chunk starts the message again, whatever came before.
            if let Some(index) = found {
                self.partial.swap_remove(index);
            }
            if chunk.header.len() + chunk.data.len() > self.max_bytes {
                return Progress::TooLarge;
            }
            let partial = Partial {
                source,
                kind,
                request_id: chunk.request_id,
                header: chunk.header.to_vec(),
                data: chunk.data.to_vec(),
                next: 2,
                chunks: chunk.chunks,
                started: self.clock,
            };
            if is_last(chunk) {
                return Progress::Complete(partial.into());
            }
            if self.partial.len() >= self.max_messages {
                if let Some(oldest) =
                    (0..self.partial.len()).min_by_key(|&index| self.partial[index].started)
                {
                    self.partial.swap_remove(oldest);
                }
            }
            self.partial.push(partial);
            return Progress::Pending;
        }
        let Some(index) = found else {
            return Progress::OutOfSequence;
        };
        let partial = &mut self.partial[index];
        // The count may be learnt only on the last chunk, but once known it
        // may not change.
        let count_changed =
            partial.chunks != 0 && chunk.chunks != 0 && chunk.chunks != partial.chunks;
        if chunk.chunk != partial.next || count_changed {
            self.partial.swap_remove(index);
            return Progress::OutOfSequence;
        }
        if partial.header.len() + partial.data.len() + chunk.data.len() > self.max_bytes {
            self.partial.swap_remove(index);
            return Progress::TooLarge;
        }
        partial.data.extend_from_slice(chunk.data);
        partial.next = partial.next.saturating_add(1);
        if chunk.chunks != 0 {
            partial.chunks = chunk.chunks;
        }
        if is_last(chunk) {
            return Progress::Complete(self.partial.swap_remove(index).into());
        }
        Progress::Pending
    }
}

/// Whether this is the last chunk: its number is the count. A count of
/// zero means the sender does not know yet.
fn is_last(chunk: &PropertyChunk<'_>) -> bool {
    chunk.chunks != 0 && chunk.chunk == chunk.chunks
}

impl From<Partial> for Assembled {
    fn from(partial: Partial) -> Self {
        Self {
            source: partial.source,
            kind: partial.kind,
            request_id: partial.request_id,
            header: partial.header,
            data: partial.data,
        }
    }
}

/// Encodes 8-bit data in 7-bit bytes with Mcoded7: each group of up to
/// seven bytes is preceded by a byte holding their top bits, the first
/// byte's in bit 6 (M2-103-UM).
///
/// ```
/// use manguebyte_midi::ci::property::{mcoded7_decode, mcoded7_encode};
///
/// let data = [0x80, 0x01, 0xFF];
/// let encoded = mcoded7_encode(&data);
/// assert_eq!(encoded, [0b0101_0000, 0x00, 0x01, 0x7F]);
/// assert_eq!(mcoded7_decode(&encoded).unwrap(), data);
/// ```
pub fn mcoded7_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len().div_ceil(7));
    for group in data.chunks(7) {
        let high = group.iter().enumerate().fold(0u8, |bits, (index, byte)| {
            bits | ((byte >> 7) << (6 - index))
        });
        out.push(high);
        out.extend(group.iter().map(|byte| byte & 0x7F));
    }
    out
}

/// Decodes what [`mcoded7_encode`] encodes. Fails on a byte above 127, or
/// on a group whose top-bit byte has bits for bytes that are not there.
pub fn mcoded7_decode(encoded: &[u8]) -> Result<Vec<u8>, CiError> {
    if encoded.iter().any(|byte| *byte > 0x7F) {
        return Err(CiError::NotSevenBit);
    }
    let mut out = Vec::with_capacity(encoded.len());
    for group in encoded.chunks(8) {
        let (&high, bytes) = group.split_first().ok_or(CiError::Truncated)?;
        if bytes.is_empty() || high & (0x7F >> bytes.len()) != 0 {
            return Err(CiError::Invalid("Mcoded7 group"));
        }
        out.extend(
            bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| byte | (((high >> (6 - index)) & 1) << 7)),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{self, Address, Header, Message};

    const SENDER: Muid = Muid(0x0055_5555);

    #[test]
    fn chunks_fit_the_receiver_and_come_back_whole() {
        let header = br#"{"resource":"ProgramList"}"#;
        let data: Vec<u8> = (0..5_000u32).map(|index| (index % 127) as u8).collect();
        for max_sysex in [header.len() + OVERHEAD + 1, 64, 128, 512, 20_000, 100_000] {
            let pieces: Vec<_> = chunks(3, header, &data, max_sysex).unwrap().collect();
            let mut assembler = Assembler::new(4, 1 << 20);
            let mut whole = None;
            for (index, piece) in pieces.iter().enumerate() {
                assert_eq!(usize::from(piece.chunk), index + 1);
                assert_eq!(usize::from(piece.chunks), pieces.len());
                assert_eq!(piece.header.is_empty(), index > 0);
                let mut bytes = Vec::new();
                let message = Message::Property(PropertyKind::GetReply, *piece);
                ci::encode_sysex(
                    &Header::new(Address::FunctionBlock, SENDER, Muid::BROADCAST),
                    &message,
                    &mut bytes,
                )
                .unwrap();
                assert!(bytes.len() <= max_sysex, "{} > {max_sysex}", bytes.len());
                let (_, Message::Property(kind, read)) = ci::parse(&bytes).unwrap() else {
                    panic!()
                };
                match assembler.push(SENDER, kind, &read) {
                    Progress::Pending => assert!(index + 1 < pieces.len()),
                    Progress::Complete(assembled) => whole = Some(assembled),
                    other => panic!("{other:?}"),
                }
            }
            let whole = whole.unwrap();
            assert_eq!(
                (whole.header.as_slice(), whole.data),
                (&header[..], data.clone())
            );
            assert_eq!(assembler.pending(), 0);
        }
        // An inquiry: one chunk, no data.
        let inquiry: Vec<_> = chunks(0, b"{}", &[], 128).unwrap().collect();
        assert_eq!(inquiry.len(), 1);
        assert_eq!((inquiry[0].chunks, inquiry[0].chunk), (1, 1));
        // A header that does not fit in a message.
        assert!(chunks(0, &[b' '; 100], &[], 100).is_err());
    }

    #[test]
    fn chunks_out_of_order_or_without_a_start_are_refused() {
        let data = vec![1u8; 300];
        let pieces: Vec<_> = chunks(1, b"{}", &data, 64).unwrap().collect();
        assert!(pieces.len() > 3);
        let mut assembler = Assembler::new(4, 1 << 20);
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Set, &pieces[1]),
            Progress::OutOfSequence
        );
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Set, &pieces[0]),
            Progress::Pending
        );
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Set, &pieces[2]),
            Progress::OutOfSequence
        );
        assert_eq!(assembler.pending(), 0);
        // A repeated first chunk starts again.
        assembler.push(SENDER, PropertyKind::Set, &pieces[0]);
        assembler.push(SENDER, PropertyKind::Set, &pieces[1]);
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Set, &pieces[0]),
            Progress::Pending
        );
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Set, &pieces[1]),
            Progress::Pending
        );
        // The same request id from another sender, or of another kind, is
        // another message.
        assert_eq!(
            assembler.push(Muid(1), PropertyKind::Set, &pieces[1]),
            Progress::OutOfSequence
        );
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Notify, &pieces[1]),
            Progress::OutOfSequence
        );
        assert_eq!(assembler.pending(), 1);
    }

    #[test]
    fn a_count_learnt_at_the_end_and_the_bounds() {
        let mut assembler = Assembler::new(2, 10);
        let chunk = |chunk, chunks, data: &'static [u8]| PropertyChunk {
            request_id: 7,
            header: if chunk == 1 { b"{}" } else { b"" },
            chunks,
            chunk,
            data,
        };
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Notify, &chunk(1, 0, b"ab")),
            Progress::Pending
        );
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Notify, &chunk(2, 0, b"cd")),
            Progress::Pending
        );
        let Progress::Complete(done) =
            assembler.push(SENDER, PropertyKind::Notify, &chunk(3, 3, b"ef"))
        else {
            panic!()
        };
        assert_eq!(done.data, b"abcdef");
        // Past the limit of ten bytes the message is dropped.
        assembler.push(SENDER, PropertyKind::Notify, &chunk(1, 0, b"abcd"));
        assert_eq!(
            assembler.push(SENDER, PropertyKind::Notify, &chunk(2, 0, b"efghi")),
            Progress::TooLarge
        );
        // At most two messages are held; a third pushes out the oldest.
        for (sender, kind) in [
            (1, PropertyKind::Set),
            (2, PropertyKind::Set),
            (3, PropertyKind::Set),
        ] {
            assembler.push(Muid(sender), kind, &chunk(1, 2, b"x"));
        }
        assert_eq!(assembler.pending(), 2);
        assert_eq!(
            assembler.push(Muid(1), PropertyKind::Set, &chunk(2, 2, b"y")),
            Progress::OutOfSequence
        );
        assembler.forget(Muid(3));
        assert_eq!(assembler.pending(), 1);
    }

    #[test]
    fn mcoded7_round_trips_and_refuses_nonsense() {
        let data: Vec<u8> = (0..=255)
            .chain((0..100u8).map(|x| x.wrapping_mul(3)))
            .collect();
        for length in 0..data.len() {
            let encoded = mcoded7_encode(&data[..length]);
            assert!(encoded.iter().all(|byte| *byte < 0x80));
            assert_eq!(mcoded7_decode(&encoded).unwrap(), &data[..length]);
        }
        assert!(mcoded7_decode(&[0x80]).is_err());
        assert!(mcoded7_decode(&[0x01]).is_err());
        // A top bit for a fourth byte where there are three.
        assert!(mcoded7_decode(&[0b0000_1000, 1, 2, 3]).is_err());
    }
}
