//! Encoding into a buffer the caller supplies.
//!
//! Nothing here allocates unless a method says so: the owned types write
//! themselves into a slice, and [`encode_message`] and [`BundleEncoder`]
//! build packets from borrowed arguments without an owned value existing at
//! all, which is what code on a real-time thread needs.

use crate::error::EncodeError;
use crate::time::OscTime;
use crate::types::{OscArg, OscArgRef, OscBundle, OscMessage, OscPacket};

/// The string every bundle begins with, including its terminating NUL.
pub(crate) const BUNDLE_HEADER: &[u8; 8] = b"#bundle\0";

/// Bytes taken by an OSC string holding `len` bytes of text: the text, at
/// least one NUL, and NULs up to a multiple of four.
const fn string_len(len: usize) -> usize {
    (len / 4 + 1) * 4
}

/// Bytes taken by an OSC blob holding `len` bytes: a 32-bit size, the data,
/// and zeros up to a multiple of four.
const fn blob_len(len: usize) -> usize {
    4 + len.div_ceil(4) * 4
}

/// Writes into a slice and keeps counting when the slice is full, so that
/// one pass both encodes and finds out how much room was needed.
struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        Self { out, len: 0 }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        let end = self.len.saturating_add(bytes.len());
        if let Some(target) = self.out.get_mut(self.len..end) {
            target.copy_from_slice(bytes);
        }
        self.len = end;
    }

    /// Zeros up to the next multiple of four.
    fn pad(&mut self) {
        let surplus = self.len % 4;
        if surplus != 0 {
            self.bytes(&[0; 3][..4 - surplus]);
        }
    }

    fn string(&mut self, text: &str) -> Result<(), EncodeError> {
        if text.as_bytes().contains(&0) {
            return Err(EncodeError::NulInString);
        }
        self.bytes(text.as_bytes());
        self.bytes(&[0]);
        self.pad();
        Ok(())
    }

    fn blob(&mut self, data: &[u8]) -> Result<(), EncodeError> {
        let size = i32::try_from(data.len()).map_err(|_| EncodeError::TooLarge)?;
        self.bytes(&size.to_be_bytes());
        self.bytes(data);
        self.pad();
        Ok(())
    }

    /// Reserves the four bytes of a size and returns where they are.
    fn reserve_size(&mut self) -> usize {
        let at = self.len;
        self.bytes(&[0; 4]);
        at
    }

    /// Fills in a size reserved at `at` with the number of bytes written
    /// since.
    fn fill_size(&mut self, at: usize) -> Result<(), EncodeError> {
        let start = at.saturating_add(4);
        let size =
            i32::try_from(self.len.saturating_sub(start)).map_err(|_| EncodeError::TooLarge)?;
        if let Some(target) = self.out.get_mut(at..start) {
            target.copy_from_slice(&size.to_be_bytes());
        }
        Ok(())
    }

    fn finish(self) -> Result<usize, EncodeError> {
        if self.len > self.out.len() {
            Err(EncodeError::BufferTooSmall { needed: self.len })
        } else {
            Ok(self.len)
        }
    }
}

fn check_address(address: &str) -> Result<(), EncodeError> {
    if address.starts_with('/') {
        Ok(())
    } else {
        Err(EncodeError::InvalidAddress)
    }
}

/// Bytes of argument data one borrowed argument takes.
fn scalar_len(arg: &OscArgRef<'_>) -> usize {
    match arg {
        OscArgRef::Int(_)
        | OscArgRef::Float(_)
        | OscArgRef::Char(_)
        | OscArgRef::Color(_)
        | OscArgRef::Midi(_) => 4,
        OscArgRef::Long(_) | OscArgRef::Double(_) | OscArgRef::Time(_) => 8,
        OscArgRef::String(text) | OscArgRef::Symbol(text) => string_len(text.len()),
        OscArgRef::Blob(data) => blob_len(data.len()),
        OscArgRef::Bool(_)
        | OscArgRef::Nil
        | OscArgRef::Impulse
        | OscArgRef::ArrayStart
        | OscArgRef::ArrayEnd => 0,
    }
}

fn write_scalar(writer: &mut Writer<'_>, arg: &OscArgRef<'_>) -> Result<(), EncodeError> {
    match *arg {
        OscArgRef::Int(value) => writer.bytes(&value.to_be_bytes()),
        OscArgRef::Float(value) => writer.bytes(&value.to_bits().to_be_bytes()),
        OscArgRef::String(text) | OscArgRef::Symbol(text) => writer.string(text)?,
        OscArgRef::Blob(data) => writer.blob(data)?,
        OscArgRef::Long(value) => writer.bytes(&value.to_be_bytes()),
        OscArgRef::Double(value) => writer.bytes(&value.to_bits().to_be_bytes()),
        OscArgRef::Time(value) => writer.bytes(&value.to_bits().to_be_bytes()),
        OscArgRef::Char(value) => writer.bytes(&u32::from(value).to_be_bytes()),
        OscArgRef::Color(value) => writer.bytes(&value.to_bytes()),
        OscArgRef::Midi(value) => writer.bytes(&value.to_bytes()),
        OscArgRef::Bool(_)
        | OscArgRef::Nil
        | OscArgRef::Impulse
        | OscArgRef::ArrayStart
        | OscArgRef::ArrayEnd => {}
    }
    Ok(())
}

fn tag_byte(arg: &OscArgRef<'_>) -> u8 {
    // Every tag is an ASCII character.
    arg.tag() as u8
}

fn write_flat_message(
    writer: &mut Writer<'_>,
    address: &str,
    args: &[OscArgRef<'_>],
) -> Result<(), EncodeError> {
    check_address(address)?;
    let mut depth = 0usize;
    for arg in args {
        match arg {
            OscArgRef::ArrayStart => depth += 1,
            OscArgRef::ArrayEnd => {
                depth = depth.checked_sub(1).ok_or(EncodeError::UnbalancedArray)?;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(EncodeError::UnbalancedArray);
    }

    writer.string(address)?;
    writer.bytes(b",");
    for arg in args {
        writer.bytes(&[tag_byte(arg)]);
    }
    writer.bytes(&[0]);
    writer.pad();
    for arg in args {
        write_scalar(writer, arg)?;
    }
    Ok(())
}

/// Bytes a message with these borrowed arguments takes once encoded.
///
/// ```
/// use manguebyte_osc::{OscArgRef, encoded_message_len};
///
/// let args = [OscArgRef::Int(1_000), OscArgRef::String("hello")];
/// assert_eq!(encoded_message_len("/foo", &args), 8 + 4 + 4 + 8);
/// ```
pub fn encoded_message_len(address: &str, args: &[OscArgRef<'_>]) -> usize {
    string_len(address.len())
        + string_len(1 + args.len())
        + args.iter().map(scalar_len).sum::<usize>()
}

/// Encodes one message from borrowed arguments into `out` and returns the
/// number of bytes written.
///
/// Does not allocate, so it may be called on a real-time thread. Arrays are
/// written with [`OscArgRef::ArrayStart`] and [`OscArgRef::ArrayEnd`]
/// around their items.
///
/// ```
/// use manguebyte_osc::{OscArgRef, encode_message};
///
/// let mut buffer = [0u8; 64];
/// let len = encode_message(
///     &mut buffer,
///     "/oscillator/4/frequency",
///     &[OscArgRef::Float(440.0)],
/// )?;
/// assert_eq!(len, 32);
/// assert_eq!(&buffer[24..32], b",f\0\0\x43\xdc\0\0");
/// # Ok::<(), manguebyte_osc::EncodeError>(())
/// ```
///
/// # Errors
///
/// [`EncodeError::BufferTooSmall`] says how many bytes are needed. The
/// other errors describe an address or argument that cannot be sent. On any
/// error the contents of `out` are unspecified.
pub fn encode_message(
    out: &mut [u8],
    address: &str,
    args: &[OscArgRef<'_>],
) -> Result<usize, EncodeError> {
    let mut writer = Writer::new(out);
    write_flat_message(&mut writer, address, args)?;
    writer.finish()
}

/// Builds a bundle in a buffer the caller supplies, one message at a time,
/// without allocating.
///
/// ```
/// use manguebyte_osc::{BundleEncoder, OscArgRef, OscPacket, OscTime};
///
/// let mut buffer = [0u8; 128];
/// let mut bundle = BundleEncoder::new(&mut buffer, OscTime::IMMEDIATELY);
/// bundle.message("/voice/1/gate", &[OscArgRef::Bool(true)])?;
/// bundle.message("/voice/1/pitch", &[OscArgRef::Float(60.0)])?;
/// let len = bundle.finish()?;
///
/// let OscPacket::Bundle(decoded) = OscPacket::decode(&buffer[..len])? else {
///     unreachable!()
/// };
/// assert_eq!(decoded.elements.len(), 2);
/// # Ok::<(), manguebyte_osc::OscError>(())
/// ```
pub struct BundleEncoder<'a> {
    writer: Writer<'a>,
    /// Where the size of each bundle that is still open was reserved.
    open: [usize; MAX_OPEN_BUNDLES],
    depth: usize,
}

const MAX_OPEN_BUNDLES: usize = 8;

impl<'a> BundleEncoder<'a> {
    /// How many bundles can be open inside the outermost one at once.
    pub const MAX_DEPTH: usize = MAX_OPEN_BUNDLES;

    /// Starts a bundle for `time` at the beginning of `out`.
    pub fn new(out: &'a mut [u8], time: OscTime) -> Self {
        let mut writer = Writer::new(out);
        writer.bytes(BUNDLE_HEADER);
        writer.bytes(&time.to_bits().to_be_bytes());
        Self {
            writer,
            open: [0; Self::MAX_DEPTH],
            depth: 0,
        }
    }

    /// Adds a message to the bundle that is open.
    ///
    /// If the message cannot be encoded, the bundle is left as it was
    /// before the call.
    pub fn message(&mut self, address: &str, args: &[OscArgRef<'_>]) -> Result<(), EncodeError> {
        let before = self.writer.len;
        let at = self.writer.reserve_size();
        let result = write_flat_message(&mut self.writer, address, args)
            .and_then(|()| self.writer.fill_size(at));
        if result.is_err() {
            self.writer.len = before;
        }
        result
    }

    /// Opens a bundle inside the one that is open. Close it with
    /// [`end_bundle`](Self::end_bundle).
    ///
    /// The specification requires `time` to be no earlier than the time of
    /// the enclosing bundle; this is not checked.
    pub fn begin_bundle(&mut self, time: OscTime) -> Result<(), EncodeError> {
        let slot = self.open.get_mut(self.depth).ok_or(EncodeError::TooDeep)?;
        *slot = self.writer.reserve_size();
        self.depth += 1;
        self.writer.bytes(BUNDLE_HEADER);
        self.writer.bytes(&time.to_bits().to_be_bytes());
        Ok(())
    }

    /// Closes the bundle opened last with
    /// [`begin_bundle`](Self::begin_bundle).
    pub fn end_bundle(&mut self) -> Result<(), EncodeError> {
        let depth = self
            .depth
            .checked_sub(1)
            .ok_or(EncodeError::UnbalancedBundle)?;
        let at = *self.open.get(depth).ok_or(EncodeError::UnbalancedBundle)?;
        self.writer.fill_size(at)?;
        self.depth = depth;
        Ok(())
    }

    /// Finishes the bundle and returns the number of bytes written.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] if the bundle did not fit, with the
    /// size it needs, and [`EncodeError::UnbalancedBundle`] if an inner
    /// bundle is still open.
    pub fn finish(self) -> Result<usize, EncodeError> {
        if self.depth != 0 {
            return Err(EncodeError::UnbalancedBundle);
        }
        self.writer.finish()
    }
}

fn owned_tag_count(args: &[OscArg]) -> usize {
    args.iter()
        .map(|arg| match arg {
            OscArg::Array(items) => 2 + owned_tag_count(items),
            _ => 1,
        })
        .sum()
}

fn owned_data_len(args: &[OscArg]) -> usize {
    args.iter()
        .map(|arg| match arg {
            OscArg::Array(items) => owned_data_len(items),
            other => other.as_scalar().as_ref().map_or(0, scalar_len),
        })
        .sum()
}

fn write_owned_tags(writer: &mut Writer<'_>, args: &[OscArg]) {
    for arg in args {
        match arg {
            OscArg::Array(items) => {
                writer.bytes(b"[");
                write_owned_tags(writer, items);
                writer.bytes(b"]");
            }
            other => {
                if let Some(scalar) = other.as_scalar() {
                    writer.bytes(&[tag_byte(&scalar)]);
                }
            }
        }
    }
}

fn write_owned_data(writer: &mut Writer<'_>, args: &[OscArg]) -> Result<(), EncodeError> {
    for arg in args {
        match arg {
            OscArg::Array(items) => write_owned_data(writer, items)?,
            other => {
                if let Some(scalar) = other.as_scalar() {
                    write_scalar(writer, &scalar)?;
                }
            }
        }
    }
    Ok(())
}

fn write_message(writer: &mut Writer<'_>, message: &OscMessage) -> Result<(), EncodeError> {
    check_address(&message.address)?;
    writer.string(&message.address)?;
    writer.bytes(b",");
    write_owned_tags(writer, &message.args);
    writer.bytes(&[0]);
    writer.pad();
    write_owned_data(writer, &message.args)
}

fn write_bundle(writer: &mut Writer<'_>, bundle: &OscBundle) -> Result<(), EncodeError> {
    writer.bytes(BUNDLE_HEADER);
    writer.bytes(&bundle.time.to_bits().to_be_bytes());
    for element in &bundle.elements {
        let at = writer.reserve_size();
        write_packet(writer, element)?;
        writer.fill_size(at)?;
    }
    Ok(())
}

fn write_packet(writer: &mut Writer<'_>, packet: &OscPacket) -> Result<(), EncodeError> {
    match packet {
        OscPacket::Message(message) => write_message(writer, message),
        OscPacket::Bundle(bundle) => write_bundle(writer, bundle),
    }
}

/// Appends an encoded packet to `out`, which grows only if its spare
/// capacity is too small.
fn append(
    out: &mut Vec<u8>,
    len: usize,
    encode: impl FnOnce(&mut [u8]) -> Result<usize, EncodeError>,
) -> Result<(), EncodeError> {
    let start = out.len();
    out.resize(start + len, 0);
    let result = encode(&mut out[start..]);
    match result {
        Ok(written) => {
            out.truncate(start + written);
            Ok(())
        }
        Err(error) => {
            out.truncate(start);
            Err(error)
        }
    }
}

impl OscMessage {
    /// Bytes this message takes once encoded.
    pub fn encoded_len(&self) -> usize {
        string_len(self.address.len())
            + string_len(1 + owned_tag_count(&self.args))
            + owned_data_len(&self.args)
    }

    /// Encodes the message into `out` and returns the number of bytes
    /// written. Does not allocate.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] says how many bytes are needed. On
    /// any error the contents of `out` are unspecified.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let mut writer = Writer::new(out);
        write_message(&mut writer, self)?;
        writer.finish()
    }

    /// Appends the encoded message to `out`.
    ///
    /// Allocates only if the spare capacity of `out` is smaller than
    /// [`encoded_len`](Self::encoded_len). On an error `out` is left as it
    /// was.
    pub fn encode_to_vec(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        append(out, self.encoded_len(), |target| self.encode(target))
    }

    /// The encoded message in a new vector.
    pub fn to_bytes(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::new();
        self.encode_to_vec(&mut out)?;
        Ok(out)
    }
}

impl OscBundle {
    /// Bytes this bundle takes once encoded.
    pub fn encoded_len(&self) -> usize {
        16 + self
            .elements
            .iter()
            .map(|element| 4 + element.encoded_len())
            .sum::<usize>()
    }

    /// Encodes the bundle into `out` and returns the number of bytes
    /// written. Does not allocate.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] says how many bytes are needed. On
    /// any error the contents of `out` are unspecified.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let mut writer = Writer::new(out);
        write_bundle(&mut writer, self)?;
        writer.finish()
    }

    /// Appends the encoded bundle to `out`.
    ///
    /// Allocates only if the spare capacity of `out` is smaller than
    /// [`encoded_len`](Self::encoded_len). On an error `out` is left as it
    /// was.
    pub fn encode_to_vec(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        append(out, self.encoded_len(), |target| self.encode(target))
    }

    /// The encoded bundle in a new vector.
    pub fn to_bytes(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::new();
        self.encode_to_vec(&mut out)?;
        Ok(out)
    }
}

impl OscPacket {
    /// Bytes this packet takes once encoded.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Message(message) => message.encoded_len(),
            Self::Bundle(bundle) => bundle.encoded_len(),
        }
    }

    /// Encodes the packet into `out` and returns the number of bytes
    /// written. Does not allocate.
    ///
    /// ```
    /// use manguebyte_osc::{EncodeError, OscMessage, OscPacket};
    ///
    /// let packet = OscPacket::from(OscMessage::new("/foo").with(1_000).with("hello"));
    /// let mut buffer = [0u8; 64];
    /// assert_eq!(packet.encode(&mut buffer), Ok(24));
    /// assert_eq!(
    ///     packet.encode(&mut buffer[..10]),
    ///     Err(EncodeError::BufferTooSmall { needed: 24 })
    /// );
    /// ```
    ///
    /// # Errors
    ///
    /// [`EncodeError::BufferTooSmall`] says how many bytes are needed. On
    /// any error the contents of `out` are unspecified.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let mut writer = Writer::new(out);
        write_packet(&mut writer, self)?;
        writer.finish()
    }

    /// Appends the encoded packet to `out`.
    ///
    /// Allocates only if the spare capacity of `out` is smaller than
    /// [`encoded_len`](Self::encoded_len). On an error `out` is left as it
    /// was.
    pub fn encode_to_vec(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        append(out, self.encoded_len(), |target| self.encode(target))
    }

    /// The encoded packet in a new vector.
    pub fn to_bytes(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::new();
        self.encode_to_vec(&mut out)?;
        Ok(out)
    }
}
