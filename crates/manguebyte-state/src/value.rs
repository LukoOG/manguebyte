//! What a property holds, and the names of kinds and properties.

use std::fmt;
use std::mem;
use std::sync::Arc;

use manguebyte_core::ParamId;

/// Identifies a node of a [`Document`](crate::Document) for as long as the
/// document exists, across edits, undo, redo, saving and loading.
///
/// A document hands out ids in increasing order and never gives the same
/// one twice, not even after the node it named is removed and the removal
/// can no longer be undone. A [`Value::Node`] holding an id is therefore a
/// reference that survives every edit: once the node is gone it simply
/// finds nothing, and once an undo brings it back it finds it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub(crate) u64);

impl NodeId {
    /// The root of every document.
    pub const ROOT: Self = Self(0);

    /// The number behind the id, as saved state writes it.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Whether `name` can name a node kind, a property or a setting: printable
/// ASCII with no spaces and no `=`, the rule of parameter ids, so that any
/// parameter id is also a property name.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'=')
}

/// A reference to a plugin parameter by its text id, with the compact
/// [`ParamId`] events carry worked out once.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParamRef {
    id: Arc<str>,
    param_id: ParamId,
}

impl ParamRef {
    /// A reference to the parameter `id`, or `None` if `id` is not a valid
    /// parameter id (see [`is_valid_name`]).
    pub fn new(id: &str) -> Option<Self> {
        is_valid_name(id).then(|| Self {
            id: Arc::from(id),
            param_id: ParamId::from_name(id),
        })
    }

    /// The parameter's text id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The parameter's compact id.
    pub fn param_id(&self) -> ParamId {
        self.param_id
    }
}

/// What a property holds.
///
/// Text and bytes are shared, so copying a value (which undo history and
/// snapshots do) never copies a sample or a long text. A float is always
/// finite: [`Tx::set`](crate::Tx::set) refuses NaN and the infinities, so
/// that every value can be saved and compared.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// A switch.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A real number, always finite.
    Float(f64),
    /// Text.
    Text(Arc<str>),
    /// Binary data: a wavetable, an image, anything.
    Bytes(Arc<[u8]>),
    /// A reference to a plugin parameter.
    Param(ParamRef),
    /// A reference to another node of the same document.
    Node(NodeId),
}

impl Value {
    /// The name of the value's type, as the text format writes it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
            Self::Param(_) => "param",
            Self::Node(_) => "node",
        }
    }

    /// The switch, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    /// The whole number, if this is one.
    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Self::Int(value) => Some(value),
            _ => None,
        }
    }

    /// The number as a float, if this is a float or a whole number.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Float(value) => Some(value),
            Self::Int(value) => Some(value as f64),
            _ => None,
        }
    }

    /// The text, if this is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The bytes, if this is binary data.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// The parameter, if this refers to one.
    pub fn as_param(&self) -> Option<&ParamRef> {
        match self {
            Self::Param(param) => Some(param),
            _ => None,
        }
    }

    /// The node, if this refers to one.
    pub fn as_node(&self) -> Option<NodeId> {
        match *self {
            Self::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Whether the value can be stored: a float must be finite.
    pub(crate) fn is_storable(&self) -> bool {
        !matches!(self, Self::Float(value) if !value.is_finite())
    }

    /// Roughly how many bytes the value keeps alive, for history limits.
    /// Shared text and bytes count in full, which errs on the safe side.
    pub(crate) fn heap_size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Bytes(bytes) => bytes.len(),
            Self::Param(param) => param.id.len(),
            _ => 0,
        }
    }

    /// Whether two values are the same, telling `0.0` from `-0.0`, which
    /// saved state also tells apart.
    pub(crate) fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            _ => self == other,
        }
    }
}

/// The size of a value in a property list, for history limits.
pub(crate) const VALUE_SIZE: usize = mem::size_of::<Value>();

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::Int(value.into())
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Self::Int(value.into())
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Self::Float(value.into())
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(Arc::from(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(Arc::from(value))
    }
}

impl From<Arc<str>> for Value {
    fn from(value: Arc<str>) -> Self {
        Self::Text(value)
    }
}

impl From<&[u8]> for Value {
    fn from(value: &[u8]) -> Self {
        Self::Bytes(Arc::from(value))
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(Arc::from(value))
    }
}

impl From<Arc<[u8]>> for Value {
    fn from(value: Arc<[u8]>) -> Self {
        Self::Bytes(value)
    }
}

impl From<ParamRef> for Value {
    fn from(value: ParamRef) -> Self {
        Self::Param(value)
    }
}

impl From<NodeId> for Value {
    fn from(value: NodeId) -> Self {
        Self::Node(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_rule_of_parameter_ids() {
        for good in ["cutoff", "env.attack", "a", "zone-1", "x/y", "{"] {
            assert!(is_valid_name(good), "{good:?}");
        }
        for bad in ["", "a b", "a=b", "tab\t", "é", "\n"] {
            assert!(!is_valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn values_convert_and_compare() {
        assert_eq!(Value::from(3).as_int(), Some(3));
        assert_eq!(Value::from(3).as_f64(), Some(3.0));
        assert_eq!(Value::from(0.5f32).as_f64(), Some(0.5));
        assert_eq!(Value::from("hi").as_text(), Some("hi"));
        assert_eq!(Value::from(vec![1u8, 2]).as_bytes(), Some(&[1u8, 2][..]));
        assert_eq!(Value::from(true).as_bool(), Some(true));
        assert_eq!(Value::from(NodeId(4)).as_node(), Some(NodeId(4)));
        let param = ParamRef::new("cutoff").unwrap();
        assert_eq!(param.param_id(), ParamId::from_name("cutoff"));
        assert_eq!(Value::from(param.clone()).as_param(), Some(&param));
        assert!(ParamRef::new("a b").is_none());
        assert_eq!(Value::from(1.0).as_int(), None);
        assert!(!Value::Float(0.0).same(&Value::Float(-0.0)));
        assert!(!Value::Float(f64::NAN).is_storable());
        assert_eq!(NodeId(7).to_string(), "#7");
    }
}
