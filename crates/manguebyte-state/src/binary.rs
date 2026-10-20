//! The compact binary form of a document.
//!
//! ```text
//! magic    "MBSTATE\0"
//! version  varint            the format version, as in the text form
//! schema   varint
//! next     varint            the id the next new node gets
//! node     id varint, kind string, property count varint, properties,
//!          child count varint, children (nodes, in order)
//! property name string, type byte, payload length varint, payload
//! string   length varint, UTF-8
//! ```
//!
//! A varint is LEB128: seven bits a byte, low bits first. Payloads by type:
//! 1 bool (one byte), 2 int (zigzag varint), 3 float (8 bytes, little
//! endian), 4 text and 6 parameter id (UTF-8), 5 bytes, 7 node id (varint).
//! Every payload carries its length, so a reader skips a type it does not
//! know, as the text form's reader skips a line.

use crate::text::{FORMAT_VERSION, LoadError, LoadReport, Migrations, Parsed};
use crate::tree::MAX_DEPTH;
use crate::value::is_valid_name;
use crate::{Document, NodeId, NodeTree, ParamRef, Value};

const MAGIC: &[u8; 8] = b"MBSTATE\0";

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn encode_value(value: &Value) -> (u8, Vec<u8>) {
    let mut payload = Vec::new();
    let tag = match value {
        Value::Bool(on) => {
            payload.push(u8::from(*on));
            1
        }
        Value::Int(number) => {
            put_varint(&mut payload, ((number << 1) ^ (number >> 63)) as u64);
            2
        }
        Value::Float(number) => {
            payload.extend_from_slice(&number.to_le_bytes());
            3
        }
        Value::Text(text) => {
            payload.extend_from_slice(text.as_bytes());
            4
        }
        Value::Bytes(bytes) => {
            payload.extend_from_slice(bytes);
            5
        }
        Value::Param(param) => {
            payload.extend_from_slice(param.id().as_bytes());
            6
        }
        Value::Node(node) => {
            put_varint(&mut payload, node.get());
            7
        }
    };
    (tag, payload)
}

impl Document {
    /// The document in the compact binary form described in the
    /// [`binary`](crate::binary) module: the same content as
    /// [`to_text`](Self::to_text), for when size or speed matters more than
    /// reading it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::from(&MAGIC[..]);
        put_varint(&mut out, u64::from(FORMAT_VERSION));
        put_varint(&mut out, u64::from(self.schema()));
        put_varint(&mut out, self.next_id());
        let mut stack = vec![NodeId::ROOT];
        while let Some(id) = stack.pop() {
            put_varint(&mut out, id.get());
            put_bytes(&mut out, self.kind(id).unwrap_or("node").as_bytes());
            let props: Vec<_> = self.properties(id).collect();
            put_varint(&mut out, props.len() as u64);
            for (name, value) in props {
                put_bytes(&mut out, name.as_bytes());
                let (tag, payload) = encode_value(value);
                out.push(tag);
                put_bytes(&mut out, &payload);
            }
            let children = self.children(id);
            put_varint(&mut out, children.len() as u64);
            stack.extend(children.iter().rev().copied());
        }
        out
    }

    /// Reads a document written by [`to_bytes`](Self::to_bytes), as
    /// tolerantly as [`from_text`](Self::from_text) reads text.
    ///
    /// # Errors
    ///
    /// A [`LoadError`] if the bytes are not a document or end too early.
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, LoadReport), LoadError> {
        parse_bytes(bytes)?.finish(None)
    }

    /// [`from_bytes`](Self::from_bytes), bringing an older schema up to
    /// date with `migrations` first.
    ///
    /// # Errors
    ///
    /// As for [`from_bytes`](Self::from_bytes).
    pub fn from_bytes_with(
        bytes: &[u8],
        migrations: &Migrations,
    ) -> Result<(Self, LoadReport), LoadError> {
        parse_bytes(bytes)?.finish(Some(migrations))
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
        let end = self.at.checked_add(len).ok_or(LoadError::Truncated)?;
        let taken = self.bytes.get(self.at..end).ok_or(LoadError::Truncated)?;
        self.at = end;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> Result<u64, LoadError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(LoadError::Truncated)
    }

    fn length(&mut self) -> Result<usize, LoadError> {
        let len = usize::try_from(self.varint()?).map_err(|_| LoadError::Truncated)?;
        if len > self.bytes.len() - self.at {
            return Err(LoadError::Truncated);
        }
        Ok(len)
    }

    fn slice(&mut self) -> Result<&'a [u8], LoadError> {
        let len = self.length()?;
        self.take(len)
    }

    fn text(&mut self) -> Result<Option<&'a str>, LoadError> {
        Ok(std::str::from_utf8(self.slice()?).ok())
    }
}

fn decode_value(tag: u8, payload: &[u8]) -> Result<Value, String> {
    let bad = || format!("unreadable payload of type {tag}");
    let text = || std::str::from_utf8(payload).map_err(|_| bad());
    let varint = || {
        let mut reader = Reader {
            bytes: payload,
            at: 0,
        };
        let value = reader.varint().map_err(|_| bad())?;
        if reader.at == payload.len() {
            Ok(value)
        } else {
            Err(bad())
        }
    };
    match tag {
        1 => match payload {
            [0] => Ok(Value::Bool(false)),
            [1] => Ok(Value::Bool(true)),
            _ => Err(bad()),
        },
        2 => {
            let raw = varint()?;
            Ok(Value::Int(((raw >> 1) as i64) ^ -((raw & 1) as i64)))
        }
        3 => {
            let bits: [u8; 8] = payload.try_into().map_err(|_| bad())?;
            let number = f64::from_le_bytes(bits);
            if number.is_finite() {
                Ok(Value::Float(number))
            } else {
                Err(bad())
            }
        }
        4 => Ok(Value::from(text()?)),
        5 => Ok(Value::from(payload)),
        6 => ParamRef::new(text()?).map(Value::Param).ok_or_else(bad),
        7 => Ok(Value::Node(NodeId(varint()?))),
        other => Err(format!("unknown type {other}")),
    }
}

fn parse_bytes(bytes: &[u8]) -> Result<Parsed, LoadError> {
    let body = bytes
        .strip_prefix(&MAGIC[..])
        .ok_or(LoadError::MissingHeader)?;
    let mut reader = Reader { bytes: body, at: 0 };
    let at = |reader: &Reader<'_>| reader.at + MAGIC.len();
    let format_version = u32::try_from(reader.varint()?).unwrap_or(u32::MAX);
    let schema = u32::try_from(reader.varint()?).unwrap_or(u32::MAX);
    let next_id = reader.varint()?;
    let mut report = LoadReport {
        format_version,
        ..LoadReport::default()
    };

    // Nodes still waiting for children, with how many they still expect.
    let mut stack: Vec<(NodeTree, u64)> = Vec::new();
    loop {
        let start = at(&reader);
        let id = NodeId(reader.varint()?);
        let kind = reader.text()?.filter(|kind| is_valid_name(kind));
        let mut node = NodeTree::new(kind.unwrap_or("node"));
        node.id = Some(id);
        if kind.is_none() {
            report.skip(start, "an invalid kind, read as \"node\"");
        }
        let props = reader.varint()?;
        for _ in 0..props {
            let start = at(&reader);
            let name = reader.text()?;
            let tag = reader.byte()?;
            let payload = reader.slice()?;
            match name.filter(|name| is_valid_name(name)) {
                None => report.skip(start, "a property of invalid name"),
                Some(name) => match decode_value(tag, payload) {
                    Ok(value) => node.set(name, value),
                    Err(why) => report.skip(start, format!("the property {name:?}: {why}")),
                },
            }
        }
        let children = reader.varint()?;
        if children > (body.len() - reader.at) as u64 {
            return Err(LoadError::Truncated);
        }
        if children > 0 {
            if stack.len() >= MAX_DEPTH {
                return Err(LoadError::TooDeep);
            }
            stack.push((node, children));
            continue;
        }
        // A finished node: hand it to its parent, and every parent it
        // finishes to theirs.
        let mut finished = node;
        loop {
            let Some((parent, remaining)) = stack.last_mut() else {
                if reader.at != body.len() {
                    report.skip(at(&reader), "bytes after the root");
                }
                return Ok(Parsed {
                    root: finished,
                    next_id,
                    schema,
                    report,
                });
            };
            parent.children.push(finished);
            *remaining -= 1;
            if *remaining > 0 {
                break;
            }
            finished = stack.pop().expect("not empty").0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_zigzag_round_trip() {
        for value in [0, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            let mut out = Vec::new();
            put_varint(&mut out, value);
            let mut reader = Reader { bytes: &out, at: 0 };
            assert_eq!(reader.varint(), Ok(value));
            assert_eq!(reader.at, out.len());
        }
        for number in [0, -1, 1, i64::MIN, i64::MAX, -12345] {
            let (tag, payload) = encode_value(&Value::Int(number));
            assert_eq!(decode_value(tag, &payload), Ok(Value::Int(number)));
        }
    }
}
