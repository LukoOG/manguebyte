//! Arguments, messages, bundles and packets.

use crate::time::OscTime;

/// An RGBA colour: the `r` argument type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OscColor {
    /// Red.
    pub red: u8,
    /// Green.
    pub green: u8,
    /// Blue.
    pub blue: u8,
    /// Alpha: 0 is transparent, 255 is opaque.
    pub alpha: u8,
}

impl OscColor {
    /// Builds a colour from its four components.
    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    /// The four bytes in the order they are sent: red, green, blue, alpha.
    pub const fn to_bytes(self) -> [u8; 4] {
        [self.red, self.green, self.blue, self.alpha]
    }

    /// The inverse of [`to_bytes`](Self::to_bytes).
    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        Self::new(bytes[0], bytes[1], bytes[2], bytes[3])
    }
}

/// A MIDI message with the port it belongs to: the `m` argument type.
///
/// The bytes are carried as they are; this crate does not interpret them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OscMidi {
    /// Port number.
    pub port: u8,
    /// Status byte.
    pub status: u8,
    /// First data byte.
    pub data1: u8,
    /// Second data byte.
    pub data2: u8,
}

impl OscMidi {
    /// Builds a MIDI argument from its four bytes.
    pub const fn new(port: u8, status: u8, data1: u8, data2: u8) -> Self {
        Self {
            port,
            status,
            data1,
            data2,
        }
    }

    /// The four bytes in the order they are sent: port, status, data 1,
    /// data 2.
    pub const fn to_bytes(self) -> [u8; 4] {
        [self.port, self.status, self.data1, self.data2]
    }

    /// The inverse of [`to_bytes`](Self::to_bytes).
    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        Self::new(bytes[0], bytes[1], bytes[2], bytes[3])
    }
}

/// One argument of a message, owning its data.
///
/// The first four variants are the types of OSC 1.0. The rest are the
/// "nonstandard" types that the specification lists and OSC 1.1 adopts.
#[derive(Debug, Clone, PartialEq)]
pub enum OscArg {
    /// `i`: a 32-bit integer.
    Int(i32),
    /// `f`: a 32-bit float.
    Float(f32),
    /// `s`: a string.
    String(String),
    /// `b`: a blob of bytes.
    Blob(Vec<u8>),
    /// `h`: a 64-bit integer.
    Long(i64),
    /// `d`: a 64-bit float.
    Double(f64),
    /// `t`: a time tag.
    Time(OscTime),
    /// `S`: a second kind of string, for systems that tell symbols from
    /// strings.
    Symbol(String),
    /// `c`: a character.
    Char(char),
    /// `r`: a colour.
    Color(OscColor),
    /// `m`: a MIDI message.
    Midi(OscMidi),
    /// `T` or `F`: true or false. Takes no bytes beyond its tag.
    Bool(bool),
    /// `N`: nil. Takes no bytes beyond its tag.
    Nil,
    /// `I`: an impulse, called "infinitum" in OSC 1.0. Takes no bytes
    /// beyond its tag.
    Impulse,
    /// `[` ... `]`: an array of arguments, which may contain arrays.
    Array(Vec<OscArg>),
}

impl OscArg {
    /// The same argument borrowing its data, or `None` for an array, which
    /// has no single borrowed form.
    pub(crate) fn as_scalar(&self) -> Option<OscArgRef<'_>> {
        Some(match self {
            Self::Int(value) => OscArgRef::Int(*value),
            Self::Float(value) => OscArgRef::Float(*value),
            Self::String(value) => OscArgRef::String(value),
            Self::Blob(value) => OscArgRef::Blob(value),
            Self::Long(value) => OscArgRef::Long(*value),
            Self::Double(value) => OscArgRef::Double(*value),
            Self::Time(value) => OscArgRef::Time(*value),
            Self::Symbol(value) => OscArgRef::Symbol(value),
            Self::Char(value) => OscArgRef::Char(*value),
            Self::Color(value) => OscArgRef::Color(*value),
            Self::Midi(value) => OscArgRef::Midi(*value),
            Self::Bool(value) => OscArgRef::Bool(*value),
            Self::Nil => OscArgRef::Nil,
            Self::Impulse => OscArgRef::Impulse,
            Self::Array(_) => return None,
        })
    }
}

macro_rules! arg_from {
    ($($source:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$source> for OscArg {
                fn from(value: $source) -> Self {
                    Self::$variant(value.into())
                }
            }
        )*
    };
}

arg_from! {
    i32 => Int,
    f32 => Float,
    String => String,
    &str => String,
    Vec<u8> => Blob,
    &[u8] => Blob,
    i64 => Long,
    f64 => Double,
    OscTime => Time,
    char => Char,
    OscColor => Color,
    OscMidi => Midi,
    bool => Bool,
    Vec<OscArg> => Array,
}

/// One argument of a message, borrowing its data.
///
/// This is what the decoder that does not allocate hands out, what
/// [`encode_message`](crate::encode_message) takes, and what the audio
/// thread sees in an [`OscEvent`](crate::OscEvent).
///
/// A borrowed argument cannot hold an array, so arrays are flattened: the
/// items of an array appear in order between [`ArrayStart`](Self::ArrayStart)
/// and [`ArrayEnd`](Self::ArrayEnd).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OscArgRef<'a> {
    /// `i`: a 32-bit integer.
    Int(i32),
    /// `f`: a 32-bit float.
    Float(f32),
    /// `s`: a string.
    String(&'a str),
    /// `b`: a blob of bytes.
    Blob(&'a [u8]),
    /// `h`: a 64-bit integer.
    Long(i64),
    /// `d`: a 64-bit float.
    Double(f64),
    /// `t`: a time tag.
    Time(OscTime),
    /// `S`: a symbol.
    Symbol(&'a str),
    /// `c`: a character.
    Char(char),
    /// `r`: a colour.
    Color(OscColor),
    /// `m`: a MIDI message.
    Midi(OscMidi),
    /// `T` or `F`: true or false.
    Bool(bool),
    /// `N`: nil.
    Nil,
    /// `I`: an impulse.
    Impulse,
    /// `[`: the arguments that follow belong to an array.
    ArrayStart,
    /// `]`: the array opened last is complete.
    ArrayEnd,
}

impl<'a> OscArgRef<'a> {
    /// The character that stands for this argument in a type tag string.
    pub const fn tag(&self) -> char {
        match self {
            Self::Int(_) => 'i',
            Self::Float(_) => 'f',
            Self::String(_) => 's',
            Self::Blob(_) => 'b',
            Self::Long(_) => 'h',
            Self::Double(_) => 'd',
            Self::Time(_) => 't',
            Self::Symbol(_) => 'S',
            Self::Char(_) => 'c',
            Self::Color(_) => 'r',
            Self::Midi(_) => 'm',
            Self::Bool(true) => 'T',
            Self::Bool(false) => 'F',
            Self::Nil => 'N',
            Self::Impulse => 'I',
            Self::ArrayStart => '[',
            Self::ArrayEnd => ']',
        }
    }

    /// The value, if this is an `i` argument.
    pub const fn as_i32(&self) -> Option<i32> {
        match *self {
            Self::Int(value) => Some(value),
            _ => None,
        }
    }

    /// The value, if this is an `f` argument.
    pub const fn as_f32(&self) -> Option<f32> {
        match *self {
            Self::Float(value) => Some(value),
            _ => None,
        }
    }

    /// The value, if this is an `h` argument.
    pub const fn as_i64(&self) -> Option<i64> {
        match *self {
            Self::Long(value) => Some(value),
            _ => None,
        }
    }

    /// The value, if this is a `d` argument.
    pub const fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Double(value) => Some(value),
            _ => None,
        }
    }

    /// The value of any numeric argument (`i`, `f`, `h` or `d`) as a
    /// double.
    ///
    /// Senders disagree about whether a control value is an integer or a
    /// float; this accepts both. A 64-bit integer beyond 2<sup>53</sup> is
    /// rounded to the nearest double.
    pub fn number(&self) -> Option<f64> {
        match *self {
            Self::Int(value) => Some(f64::from(value)),
            Self::Float(value) => Some(f64::from(value)),
            Self::Long(value) => Some(value as f64),
            Self::Double(value) => Some(value),
            _ => None,
        }
    }

    /// The text, if this is a string or a symbol.
    pub const fn as_str(&self) -> Option<&'a str> {
        match *self {
            Self::String(value) | Self::Symbol(value) => Some(value),
            _ => None,
        }
    }

    /// The bytes, if this is a blob.
    pub const fn as_blob(&self) -> Option<&'a [u8]> {
        match *self {
            Self::Blob(value) => Some(value),
            _ => None,
        }
    }

    /// The value, if this is a `T` or `F` argument.
    pub const fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    /// The owned form of this argument, or `None` for an array marker.
    pub(crate) fn to_scalar(self) -> Option<OscArg> {
        Some(match self {
            Self::Int(value) => OscArg::Int(value),
            Self::Float(value) => OscArg::Float(value),
            Self::String(value) => OscArg::String(value.to_owned()),
            Self::Blob(value) => OscArg::Blob(value.to_vec()),
            Self::Long(value) => OscArg::Long(value),
            Self::Double(value) => OscArg::Double(value),
            Self::Time(value) => OscArg::Time(value),
            Self::Symbol(value) => OscArg::Symbol(value.to_owned()),
            Self::Char(value) => OscArg::Char(value),
            Self::Color(value) => OscArg::Color(value),
            Self::Midi(value) => OscArg::Midi(value),
            Self::Bool(value) => OscArg::Bool(value),
            Self::Nil => OscArg::Nil,
            Self::Impulse => OscArg::Impulse,
            Self::ArrayStart | Self::ArrayEnd => return None,
        })
    }
}

macro_rules! arg_ref_from {
    ($($source:ty => $variant:ident),* $(,)?) => {
        $(
            impl<'a> From<$source> for OscArgRef<'a> {
                fn from(value: $source) -> Self {
                    Self::$variant(value)
                }
            }
        )*
    };
}

arg_ref_from! {
    i32 => Int,
    f32 => Float,
    &'a str => String,
    &'a [u8] => Blob,
    i64 => Long,
    f64 => Double,
    OscTime => Time,
    char => Char,
    OscColor => Color,
    OscMidi => Midi,
    bool => Bool,
}

/// A message: an address pattern and its arguments.
///
/// ```
/// use manguebyte_osc::{OscArg, OscMessage};
///
/// let message = OscMessage::new("/mixer/channel/3/gain").with(-6.0f32).with("dB");
/// assert_eq!(message.args, [OscArg::Float(-6.0), OscArg::String("dB".into())]);
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OscMessage {
    /// Where the message goes. A sender may write a pattern here to reach
    /// several addresses at once; see [`pattern`](crate::pattern).
    pub address: String,
    /// The arguments, in order.
    pub args: Vec<OscArg>,
}

impl OscMessage {
    /// A message to `address` with no arguments.
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            args: Vec::new(),
        }
    }

    /// Adds an argument and returns the message, for building one in a
    /// single expression.
    #[must_use]
    pub fn with(mut self, arg: impl Into<OscArg>) -> Self {
        self.args.push(arg.into());
        self
    }
}

/// A bundle: messages and bundles that take effect together, at a time.
///
/// ```
/// use manguebyte_osc::{OscBundle, OscMessage, OscTime};
///
/// let bundle = OscBundle::new(OscTime::IMMEDIATELY)
///     .with(OscMessage::new("/voice/1/note").with(60))
///     .with(OscMessage::new("/voice/2/note").with(64));
/// assert_eq!(bundle.elements.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct OscBundle {
    /// When the contents take effect.
    pub time: OscTime,
    /// The messages and bundles inside, in order.
    pub elements: Vec<OscPacket>,
}

impl OscBundle {
    /// An empty bundle for `time`.
    pub fn new(time: OscTime) -> Self {
        Self {
            time,
            elements: Vec::new(),
        }
    }

    /// Adds a message or a bundle and returns the bundle, for building one
    /// in a single expression.
    #[must_use]
    pub fn with(mut self, element: impl Into<OscPacket>) -> Self {
        self.elements.push(element.into());
        self
    }
}

impl Default for OscBundle {
    /// An empty bundle marked [`OscTime::IMMEDIATELY`].
    fn default() -> Self {
        Self::new(OscTime::IMMEDIATELY)
    }
}

/// The unit of transmission: a message or a bundle.
#[derive(Debug, Clone, PartialEq)]
pub enum OscPacket {
    /// A single message.
    Message(OscMessage),
    /// A bundle.
    Bundle(OscBundle),
}

impl From<OscMessage> for OscPacket {
    fn from(message: OscMessage) -> Self {
        Self::Message(message)
    }
}

impl From<OscBundle> for OscPacket {
    fn from(bundle: OscBundle) -> Self {
        Self::Bundle(bundle)
    }
}
