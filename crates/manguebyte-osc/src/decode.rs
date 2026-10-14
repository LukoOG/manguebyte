//! Strict decoding.
//!
//! A packet from the network is untrusted. The decoder checks everything
//! the specification states: lengths are multiples of four, strings end in
//! a NUL, padding is zero, sizes stay inside what encloses them, and the
//! type tags account for every byte. Nesting is bounded by
//! [`DecodeLimits`]. Anything else is a [`DecodeError`]; nothing panics.
//!
//! There are two ways to read a packet. [`OscPacketRef::parse`] checks the
//! whole packet once and then hands out views into the bytes it was given,
//! without allocating: that is the one for a real-time path.
//! [`OscPacket::decode`] does the same and then copies the result into
//! owned values.

use crate::encode::BUNDLE_HEADER;
use crate::error::DecodeError;
use crate::time::OscTime;
use crate::types::{OscArg, OscArgRef, OscBundle, OscColor, OscMessage, OscMidi, OscPacket};

/// No limit can be set higher than this. Checking a packet recurses once
/// per level of nesting, and this keeps the recursion shallow whatever the
/// caller asks for.
const HARD_DEPTH_LIMIT: usize = 64;

/// How deeply a packet may be nested before the decoder refuses it.
///
/// A limit is needed because nesting costs the receiver much more than it
/// costs the sender: two bytes of type tags open another array.
///
/// ```
/// use manguebyte_osc::{DecodeError, DecodeLimits, OscBundle, OscPacket, OscTime};
///
/// let nested = OscBundle::default().with(OscBundle::default()).to_bytes()?;
/// let limits = DecodeLimits::default().with_max_bundle_depth(1);
/// assert_eq!(
///     OscPacket::decode_with(&nested, &limits),
///     Err(DecodeError::BundleTooDeep { max: 1 })
/// );
/// assert!(OscPacket::decode(&nested).is_ok());
/// # Ok::<(), manguebyte_osc::EncodeError>(())
/// ```
///
/// More limits may be added later, so a value is made from
/// [`DecodeLimits::default`] and changed with the `with_` methods or by
/// assigning to its fields, never written out field by field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeLimits {
    /// How many bundles may be inside each other, counting the outermost.
    /// Zero refuses every bundle. Values above 64 count as 64.
    pub max_bundle_depth: usize,
    /// How many arrays may be inside each other in one message. Zero
    /// refuses every array. Values above 64 count as 64.
    pub max_array_depth: usize,
}

impl Default for DecodeLimits {
    /// Sixteen levels of bundles and sixteen of arrays.
    fn default() -> Self {
        Self {
            max_bundle_depth: 16,
            max_array_depth: 16,
        }
    }
}

impl DecodeLimits {
    /// The same limits with [`max_bundle_depth`](Self::max_bundle_depth)
    /// changed.
    #[must_use]
    pub const fn with_max_bundle_depth(mut self, depth: usize) -> Self {
        self.max_bundle_depth = depth;
        self
    }

    /// The same limits with [`max_array_depth`](Self::max_array_depth)
    /// changed.
    #[must_use]
    pub const fn with_max_array_depth(mut self, depth: usize) -> Self {
        self.max_array_depth = depth;
        self
    }
}

/// A position in a slice of a packet, with the offset of that slice in the
/// whole packet so that errors can say where they happened.
#[derive(Debug, Clone, Copy)]
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    base: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], base: usize) -> Self {
        Self { bytes, at: 0, base }
    }

    fn offset(&self) -> usize {
        self.base.saturating_add(self.at)
    }

    fn rest(&self) -> &'a [u8] {
        self.bytes.get(self.at..).unwrap_or(&[])
    }

    fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn truncated(&self) -> DecodeError {
        DecodeError::Truncated {
            offset: self.base.saturating_add(self.bytes.len()),
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let taken = self.rest().get(..len).ok_or_else(|| self.truncated())?;
        self.at += len;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_be_bytes)
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        self.array().map(i32::from_be_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_be_bytes)
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        self.array().map(i64::from_be_bytes)
    }

    /// An OSC string: text, a NUL, and NULs up to a multiple of four.
    fn string(&mut self) -> Result<&'a str, DecodeError> {
        let offset = self.offset();
        let rest = self.rest();
        let len = rest
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(DecodeError::UnterminatedString { offset })?;
        let padded = (len / 4 + 1) * 4;
        let whole = rest.get(..padded).ok_or_else(|| self.truncated())?;
        // `len` is below `padded`, which is the length of `whole`.
        let (text, padding) = whole.split_at(len);
        if padding.iter().any(|&byte| byte != 0) {
            return Err(DecodeError::NonZeroPadding {
                offset: offset.saturating_add(len),
            });
        }
        let text = core::str::from_utf8(text).map_err(|_| DecodeError::InvalidString { offset })?;
        self.at += padded;
        Ok(text)
    }

    /// An OSC blob: a size, that many bytes, and zeros up to a multiple of
    /// four.
    fn blob(&mut self) -> Result<&'a [u8], DecodeError> {
        let offset = self.offset();
        let size = self.i32()?;
        let invalid = DecodeError::InvalidBlobSize { size, offset };
        let len = usize::try_from(size).map_err(|_| invalid)?;
        let padded = len.checked_next_multiple_of(4).ok_or(invalid)?;
        let whole = self.rest().get(..padded).ok_or(invalid)?;
        // `len` is at most `padded`, which is the length of `whole`.
        let (data, padding) = whole.split_at(len);
        if padding.iter().any(|&byte| byte != 0) {
            return Err(DecodeError::NonZeroPadding {
                offset: self.offset().saturating_add(len),
            });
        }
        self.at += padded;
        Ok(data)
    }
}

/// The arguments of a message, decoded one at a time without allocating.
///
/// Arrays come out flat, as described on [`OscArgRef`].
#[derive(Debug, Clone)]
pub struct OscArgs<'a> {
    tags: &'a [u8],
    next_tag: usize,
    data: Reader<'a>,
}

impl<'a> OscArgs<'a> {
    /// Arguments described by `tags` (without the comma) and stored in
    /// `data`, which begins `base` bytes into the packet.
    pub(crate) fn new(tags: &'a str, data: &'a [u8], base: usize) -> Self {
        Self {
            tags: tags.as_bytes(),
            next_tag: 0,
            data: Reader::new(data, base),
        }
    }

    fn try_next(&mut self) -> Result<Option<OscArgRef<'a>>, DecodeError> {
        let index = self.next_tag;
        let Some(&tag) = self.tags.get(index) else {
            return Ok(None);
        };
        let data = &mut self.data;
        let arg = match tag {
            b'i' => OscArgRef::Int(data.i32()?),
            b'f' => OscArgRef::Float(f32::from_bits(data.u32()?)),
            b's' => OscArgRef::String(data.string()?),
            b'b' => OscArgRef::Blob(data.blob()?),
            b'h' => OscArgRef::Long(data.i64()?),
            b'd' => OscArgRef::Double(f64::from_bits(data.u64()?)),
            b't' => OscArgRef::Time(OscTime::from_bits(data.u64()?)),
            b'S' => OscArgRef::Symbol(data.string()?),
            b'c' => {
                let offset = data.offset();
                let value = data.u32()?;
                OscArgRef::Char(
                    char::from_u32(value).ok_or(DecodeError::InvalidChar { value, offset })?,
                )
            }
            b'r' => OscArgRef::Color(OscColor::from_bytes(data.array()?)),
            b'm' => OscArgRef::Midi(OscMidi::from_bytes(data.array()?)),
            b'T' => OscArgRef::Bool(true),
            b'F' => OscArgRef::Bool(false),
            b'N' => OscArgRef::Nil,
            b'I' => OscArgRef::Impulse,
            b'[' => OscArgRef::ArrayStart,
            b']' => OscArgRef::ArrayEnd,
            _ => return Err(DecodeError::UnknownTypeTag { tag, index }),
        };
        self.next_tag = index + 1;
        Ok(Some(arg))
    }

    /// Checks every argument and that nothing is left over.
    fn validate(mut self, limits: &DecodeLimits) -> Result<(), DecodeError> {
        let max = limits.max_array_depth.min(HARD_DEPTH_LIMIT);
        let mut depth = 0usize;
        while let Some(arg) = self.try_next()? {
            match arg {
                OscArgRef::ArrayStart => {
                    depth += 1;
                    if depth > max {
                        return Err(DecodeError::ArrayTooDeep { max });
                    }
                }
                OscArgRef::ArrayEnd => {
                    depth = depth.checked_sub(1).ok_or(DecodeError::UnbalancedArray)?;
                }
                _ => {}
            }
        }
        if depth != 0 {
            return Err(DecodeError::UnbalancedArray);
        }
        if !self.data.is_empty() {
            return Err(DecodeError::TrailingData {
                offset: self.data.offset(),
            });
        }
        Ok(())
    }

    /// How many arguments come before the array that is open ends, or
    /// before the arguments do, counting an array inside it as one.
    fn items_ahead(&self) -> usize {
        let mut depth = 0usize;
        let mut items = 0;
        for &tag in self.tags.get(self.next_tag..).unwrap_or(&[]) {
            match tag {
                b']' if depth == 0 => break,
                b']' => depth -= 1,
                b'[' => {
                    items += usize::from(depth == 0);
                    depth += 1;
                }
                _ => items += usize::from(depth == 0),
            }
        }
        items
    }

    /// Gathers arguments into owned values until the array that is open
    /// ends, or the arguments do.
    fn collect_owned(&mut self) -> Vec<OscArg> {
        // Exactly the room that is needed: a vector left to grow by
        // itself takes up to twice that, and an array of one item four
        // times, which a sender could use to make a small packet costly.
        let mut out = Vec::with_capacity(self.items_ahead());
        while let Some(arg) = self.next() {
            match arg {
                OscArgRef::ArrayStart => out.push(OscArg::Array(self.collect_owned())),
                OscArgRef::ArrayEnd => break,
                scalar => out.extend(scalar.to_scalar()),
            }
        }
        out
    }
}

impl<'a> Iterator for OscArgs<'a> {
    type Item = OscArgRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.try_next() {
            Ok(arg) => arg,
            Err(_) => {
                // Not reachable for arguments that were checked when the
                // packet was parsed. Stop for good rather than skip one.
                self.next_tag = self.tags.len();
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.tags.len().saturating_sub(self.next_tag);
        (0, Some(left))
    }
}

/// A message inside a packet that has been checked, borrowing the packet's
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OscMessageRef<'a> {
    address: &'a str,
    tags: &'a str,
    data: &'a [u8],
}

impl<'a> OscMessageRef<'a> {
    /// The address pattern.
    pub fn address(&self) -> &'a str {
        self.address
    }

    /// The type tags, one character per argument, without the leading
    /// comma.
    pub fn type_tags(&self) -> &'a str {
        self.tags
    }

    /// The arguments. Decoding them does not allocate.
    pub fn args(&self) -> OscArgs<'a> {
        OscArgs::new(self.tags, self.data, 0)
    }

    /// The bytes that hold the arguments, exactly as they were sent.
    pub(crate) fn arg_data(&self) -> &'a [u8] {
        self.data
    }

    /// Copies the message into an owned value.
    pub fn to_message(&self) -> OscMessage {
        OscMessage {
            address: self.address.to_owned(),
            args: self.args().collect_owned(),
        }
    }
}

/// A bundle inside a packet that has been checked, borrowing the packet's
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OscBundleRef<'a> {
    time: OscTime,
    body: &'a [u8],
}

impl<'a> OscBundleRef<'a> {
    /// When the contents take effect.
    pub fn time(&self) -> OscTime {
        self.time
    }

    /// The messages and bundles inside, in order. Walking them does not
    /// allocate.
    pub fn elements(&self) -> OscElements<'a> {
        OscElements {
            body: Reader::new(self.body, 0),
        }
    }

    /// Copies the bundle into an owned value.
    pub fn to_bundle(&self) -> OscBundle {
        // Counted first, so that the vector is allocated once at its
        // final size.
        let mut elements = Vec::with_capacity(self.elements().count());
        elements.extend(self.elements().map(|element| element.to_packet()));
        OscBundle {
            time: self.time,
            elements,
        }
    }
}

/// The elements of a bundle, one at a time.
#[derive(Debug, Clone)]
pub struct OscElements<'a> {
    body: Reader<'a>,
}

impl<'a> OscElements<'a> {
    /// The contents of the next element and where they begin.
    fn try_next(&mut self) -> Result<Option<(&'a [u8], usize)>, DecodeError> {
        if self.body.is_empty() {
            return Ok(None);
        }
        let offset = self.body.offset();
        let size = self.body.i32()?;
        let invalid = DecodeError::InvalidElementSize { size, offset };
        let len = usize::try_from(size)
            .ok()
            .filter(|len| len % 4 == 0)
            .ok_or(invalid)?;
        let base = self.body.offset();
        let contents = self.body.take(len).map_err(|_| invalid)?;
        Ok(Some((contents, base)))
    }
}

impl<'a> Iterator for OscElements<'a> {
    type Item = OscPacketRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let element = self
            .try_next()
            .ok()
            .flatten()
            .and_then(|(contents, base)| split(contents, base).ok());
        if element.is_none() {
            // The end, or an error that a checked bundle cannot produce.
            // Either way there is nothing more to hand out.
            self.body.at = self.body.bytes.len();
        }
        element.map(|(packet, _)| packet)
    }
}

/// A packet that has been checked, borrowing the bytes it was parsed from.
///
/// ```
/// use manguebyte_osc::{OscArgRef, OscMessage, OscPacketRef};
///
/// let bytes = OscMessage::new("/filter/cutoff").with(1_200.0f32).to_bytes()?;
///
/// let OscPacketRef::Message(message) = OscPacketRef::parse(&bytes)? else {
///     unreachable!()
/// };
/// assert_eq!(message.address(), "/filter/cutoff");
/// assert_eq!(message.args().next(), Some(OscArgRef::Float(1_200.0)));
/// # Ok::<(), manguebyte_osc::OscError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscPacketRef<'a> {
    /// A single message.
    Message(OscMessageRef<'a>),
    /// A bundle.
    Bundle(OscBundleRef<'a>),
}

impl<'a> OscPacketRef<'a> {
    /// Checks a whole packet and returns a view of it.
    ///
    /// Does not allocate. Once this has succeeded, walking the packet
    /// cannot fail: the iterators hand out plain values, not results.
    ///
    /// A message that ends after its address, with no type tag string, is
    /// accepted as a message without arguments. The specification asks
    /// receivers to tolerate this because some old senders leave the type
    /// tag string out.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] for anything the specification does not allow,
    /// and for nesting beyond [`DecodeLimits::default`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        Self::parse_with(bytes, &DecodeLimits::default())
    }

    /// [`parse`](Self::parse) with limits of the caller's choosing.
    pub fn parse_with(bytes: &'a [u8], limits: &DecodeLimits) -> Result<Self, DecodeError> {
        if bytes.len() % 4 != 0 {
            return Err(DecodeError::Misaligned { len: bytes.len() });
        }
        check(bytes, 0, limits, 0)
    }

    /// Copies the packet into an owned value.
    pub fn to_packet(&self) -> OscPacket {
        match self {
            Self::Message(message) => OscPacket::Message(message.to_message()),
            Self::Bundle(bundle) => OscPacket::Bundle(bundle.to_bundle()),
        }
    }
}

/// Reads the outermost layer of a packet: a message's address and type
/// tags, or a bundle's header. Also returns where the rest begins. Neither
/// the arguments nor the elements are looked at.
fn split(bytes: &[u8], base: usize) -> Result<(OscPacketRef<'_>, usize), DecodeError> {
    let mut reader = Reader::new(bytes, base);
    match bytes.first() {
        None => Err(DecodeError::Empty { offset: base }),
        Some(b'/') => {
            let address = reader.string()?;
            if reader.is_empty() {
                let message = OscMessageRef {
                    address,
                    tags: "",
                    data: &[],
                };
                return Ok((OscPacketRef::Message(message), reader.offset()));
            }
            if reader.rest().first() != Some(&b',') {
                return Err(DecodeError::MissingTypeTags {
                    offset: reader.offset(),
                });
            }
            let tags = reader.string()?;
            let message = OscMessageRef {
                address,
                // Drops the comma, which is one byte long.
                tags: tags.get(1..).unwrap_or(""),
                data: reader.rest(),
            };
            Ok((OscPacketRef::Message(message), reader.offset()))
        }
        Some(b'#') => {
            let header: [u8; 8] = reader.array()?;
            if &header != BUNDLE_HEADER {
                return Err(DecodeError::InvalidBundleHeader { offset: base });
            }
            let bundle = OscBundleRef {
                time: OscTime::from_bits(reader.u64()?),
                body: reader.rest(),
            };
            Ok((OscPacketRef::Bundle(bundle), reader.offset()))
        }
        Some(&first) => Err(DecodeError::UnknownPacket {
            first,
            offset: base,
        }),
    }
}

/// Checks a packet completely: its own layer, then every argument or every
/// element. `depth` is the number of bundles around it.
fn check<'a>(
    bytes: &'a [u8],
    base: usize,
    limits: &DecodeLimits,
    depth: usize,
) -> Result<OscPacketRef<'a>, DecodeError> {
    let (packet, rest_base) = split(bytes, base)?;
    match packet {
        OscPacketRef::Message(message) => {
            OscArgs::new(message.tags, message.data, rest_base).validate(limits)?;
        }
        OscPacketRef::Bundle(bundle) => {
            let max = limits.max_bundle_depth.min(HARD_DEPTH_LIMIT);
            if depth >= max {
                return Err(DecodeError::BundleTooDeep { max });
            }
            let mut elements = OscElements {
                body: Reader::new(bundle.body, rest_base),
            };
            while let Some((contents, element_base)) = elements.try_next()? {
                check(contents, element_base, limits, depth + 1)?;
            }
        }
    }
    Ok(packet)
}

impl OscPacket {
    /// Decodes a packet into owned values.
    ///
    /// The packet is checked completely before anything is allocated, so a
    /// packet that is refused costs no memory. One that is accepted takes
    /// at most the size of one [`OscArg`] for each of its bytes: 32 bytes
    /// on a 64-bit system, reached by a message made of nothing but
    /// arguments that carry no data.
    ///
    /// ```
    /// use manguebyte_osc::{DecodeError, OscMessage, OscPacket};
    ///
    /// let bytes = b"/foo\0\0\0\0,i\0\0\0\0\x03\xe8";
    /// assert_eq!(
    ///     OscPacket::decode(bytes),
    ///     Ok(OscPacket::Message(OscMessage::new("/foo").with(1_000)))
    /// );
    /// assert_eq!(
    ///     OscPacket::decode(&bytes[..14]),
    ///     Err(DecodeError::Misaligned { len: 14 })
    /// );
    /// ```
    ///
    /// # Errors
    ///
    /// The same as [`OscPacketRef::parse`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_with(bytes, &DecodeLimits::default())
    }

    /// [`decode`](Self::decode) with limits of the caller's choosing.
    pub fn decode_with(bytes: &[u8], limits: &DecodeLimits) -> Result<Self, DecodeError> {
        Ok(OscPacketRef::parse_with(bytes, limits)?.to_packet())
    }
}
