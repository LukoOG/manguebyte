//! A plugin's state as bytes: its control inputs and what it stored
//! through the `state:interface` extension.
//!
//! LV2 leaves the format of a saved state to the host. This one keeps
//! everything by URI and by port symbol, never by URID or port index, so
//! that it means the same in another process, on another machine, or for
//! a later version of the plugin that numbers its ports differently:
//!
//! ```text
//! "MBLV2ST" 0x01
//! u32 ports      then, for each: u16 length, the symbol, f32 value
//! u32 properties then, for each: u32 length, the key's URI,
//!                                u32 length, the type's URI,
//!                                u32 flags,
//!                                u32 length, the value
//! ```
//!
//! Numbers are little-endian. Reading never trusts a length: each one is
//! checked against what is left before anything is allocated for it.

use std::ffi::c_void;

use super::features::{map_str, unmap};
use super::sys::*;
use crate::threads::guard;

const MAGIC: &[u8; 8] = b"MBLV2ST\x01";

/// Largest state saved or read.
pub const MAX_STATE_BYTES: usize = 64 << 20;

/// Longest URI or symbol in a state.
const MAX_NAME_BYTES: usize = 4_096;

/// One value a plugin stored.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Property {
    pub(crate) key: String,
    pub(crate) type_: String,
    pub(crate) flags: u32,
    /// The value, in 8-byte words so that a plugin handed it back may read
    /// it as any type.
    words: Box<[u64]>,
    size: usize,
}

impl Property {
    pub(crate) fn new(key: String, type_: String, flags: u32, value: &[u8]) -> Self {
        let mut words = vec![0u64; value.len().div_ceil(8)].into_boxed_slice();
        // SAFETY: the words hold at least `value.len()` bytes, every byte
        // value is a valid `u8`, and the two do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(
                value.as_ptr(),
                words.as_mut_ptr().cast::<u8>(),
                value.len(),
            );
        }
        Self {
            key,
            type_,
            flags,
            words,
            size: value.len(),
        }
    }

    pub(crate) fn value(&self) -> &[u8] {
        // SAFETY: the words hold `size` initialised bytes.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.size) }
    }
}

/// A state: control input values by symbol, and the plugin's own values.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct State {
    pub(crate) ports: Vec<(String, f32)>,
    pub(crate) properties: Vec<Property>,
}

impl State {
    pub(crate) fn to_bytes(&self) -> Option<Vec<u8>> {
        let mut bytes = Vec::from(&MAGIC[..]);
        bytes.extend((self.ports.len() as u32).to_le_bytes());
        for (symbol, value) in &self.ports {
            let length = u16::try_from(symbol.len()).ok()?;
            bytes.extend(length.to_le_bytes());
            bytes.extend(symbol.as_bytes());
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend((self.properties.len() as u32).to_le_bytes());
        for property in &self.properties {
            for text in [&property.key, &property.type_] {
                bytes.extend((text.len() as u32).to_le_bytes());
                bytes.extend(text.as_bytes());
            }
            bytes.extend(property.flags.to_le_bytes());
            bytes.extend((property.size as u32).to_le_bytes());
            bytes.extend(property.value());
            if bytes.len() > MAX_STATE_BYTES {
                return None;
            }
        }
        (bytes.len() <= MAX_STATE_BYTES).then_some(bytes)
    }

    /// Reads a state written by [`to_bytes`](Self::to_bytes). `None` for
    /// anything else, whatever it holds.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_STATE_BYTES {
            return None;
        }
        let mut reader = Reader {
            bytes: bytes.strip_prefix(&MAGIC[..])?,
        };
        let mut state = Self::default();
        let ports = reader.u32()?;
        for _ in 0..ports {
            let length = usize::from(reader.u16()?);
            let symbol = reader.text(length)?;
            let value = f32::from_le_bytes(reader.take(4)?.try_into().ok()?);
            state.ports.push((symbol, value));
        }
        let properties = reader.u32()?;
        for _ in 0..properties {
            let key_length = reader.u32()? as usize;
            let key = reader.text(key_length)?;
            let type_length = reader.u32()? as usize;
            let type_ = reader.text(type_length)?;
            let flags = reader.u32()?;
            let size = reader.u32()? as usize;
            let value = reader.take(size)?;
            state
                .properties
                .push(Property::new(key, type_, flags, value));
        }
        reader.bytes.is_empty().then_some(state)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        if count > self.bytes.len() {
            return None;
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Some(taken)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn text(&mut self, length: usize) -> Option<String> {
        if length > MAX_NAME_BYTES {
            return None;
        }
        String::from_utf8(self.take(length)?.to_vec()).ok()
    }
}

// --- Callbacks handed to the plugin -------------------------------------------

/// Where the values go while a plugin saves.
pub(crate) struct Store {
    pub(crate) properties: Vec<Property>,
    pub(crate) bytes: usize,
    /// Set when the plugin stored more than [`MAX_STATE_BYTES`].
    pub(crate) too_large: bool,
}

pub(crate) unsafe extern "C" fn store(
    handle: LV2_State_Handle,
    key: u32,
    value: *const c_void,
    size: usize,
    type_: u32,
    flags: u32,
) -> LV2_State_Status {
    guard(LV2_STATE_ERR_UNKNOWN, || {
        // SAFETY: the handle is the `Store` the host passed to `save`,
        // which nothing else touches while the plugin saves.
        let Some(store) = (unsafe { handle.cast::<Store>().as_mut() }) else {
            return LV2_STATE_ERR_UNKNOWN;
        };
        // Only plain data can be copied as bytes and given back later.
        if flags & LV2_STATE_IS_POD == 0 {
            return LV2_STATE_ERR_BAD_FLAGS;
        }
        let (Some(key), Some(type_)) = (unmap(key), unmap(type_)) else {
            return LV2_STATE_ERR_BAD_TYPE;
        };
        if value.is_null() && size > 0 {
            return LV2_STATE_ERR_UNKNOWN;
        }
        if size > MAX_STATE_BYTES || store.bytes + size > MAX_STATE_BYTES {
            store.too_large = true;
            return LV2_STATE_ERR_NO_SPACE;
        }
        let value = if size == 0 {
            &[][..]
        } else {
            // SAFETY: the plugin passes `size` readable bytes at `value`,
            // which the host copies before returning.
            unsafe { std::slice::from_raw_parts(value.cast::<u8>(), size) }
        };
        store.bytes += size + key.len() + type_.len();
        // A key stored twice keeps its last value.
        store.properties.retain(|property| property.key != key);
        store
            .properties
            .push(Property::new(key, type_, flags, value));
        LV2_STATE_SUCCESS
    })
}

pub(crate) unsafe extern "C" fn retrieve(
    handle: LV2_State_Handle,
    key: u32,
    size: *mut usize,
    type_: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    guard(std::ptr::null(), || {
        // SAFETY: the handle is the list of properties the host passed to
        // `restore`, which lives until `restore` returns.
        let Some(properties) = (unsafe { handle.cast::<Vec<Property>>().as_ref() }) else {
            return std::ptr::null();
        };
        let Some(key) = unmap(key) else {
            return std::ptr::null();
        };
        let Some(property) = properties.iter().find(|property| property.key == key) else {
            return std::ptr::null();
        };
        let type_id = map_str(&property.type_);
        // SAFETY: the plugin passes places for the three answers, or null
        // for any it does not want.
        unsafe {
            if !size.is_null() {
                size.write(property.size);
            }
            if !type_.is_null() {
                type_.write(type_id);
            }
            if !flags.is_null() {
                flags.write(property.flags);
            }
        }
        property.words.as_ptr().cast()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> State {
        State {
            ports: vec![("gain".into(), -6.5), ("é".into(), f32::MIN_POSITIVE)],
            properties: vec![
                Property::new("urn:k".into(), "urn:t".into(), 3, b"hello\0"),
                Property::new("urn:empty".into(), "urn:t".into(), 1, b""),
            ],
        }
    }

    #[test]
    fn a_state_reads_back_as_it_was_written() {
        let state = sample();
        let bytes = state.to_bytes().unwrap();
        assert_eq!(State::from_bytes(&bytes), Some(state));
        let empty = State::default();
        assert_eq!(State::from_bytes(&empty.to_bytes().unwrap()), Some(empty));
    }

    #[test]
    fn every_truncation_and_every_damaged_byte_is_handled() {
        let bytes = sample().to_bytes().unwrap();
        for length in 0..bytes.len() {
            assert_eq!(State::from_bytes(&bytes[..length]), None, "cut at {length}");
        }
        for index in 0..bytes.len() {
            for damage in [0x00, 0xFF, 0x7F] {
                let mut copy = bytes.clone();
                copy[index] = damage;
                // A result either way; never a panic, and never an
                // allocation the size of a damaged length.
                let _ = State::from_bytes(&copy);
            }
        }
        // Lengths that claim more than there is.
        let mut huge = Vec::from(&MAGIC[..]);
        huge.extend(u32::MAX.to_le_bytes());
        assert_eq!(State::from_bytes(&huge), None);
        let mut trailing = sample().to_bytes().unwrap();
        trailing.push(0);
        assert_eq!(State::from_bytes(&trailing), None);
    }

    #[test]
    fn the_callbacks_store_and_hand_back_values() {
        let key = map_str("urn:manguebyte:test:state-key");
        let string = map_str("http://lv2plug.in/ns/ext/atom#String");
        let mut store = Store {
            properties: Vec::new(),
            bytes: 0,
            too_large: false,
        };
        let handle = (&raw mut store).cast();
        // SAFETY: called as a plugin would, with the handle it was given.
        unsafe {
            let value = b"first\0";
            assert_eq!(
                super::store(
                    handle,
                    key,
                    value.as_ptr().cast(),
                    6,
                    string,
                    LV2_STATE_IS_POD
                ),
                LV2_STATE_SUCCESS
            );
            let value = b"second\0";
            assert_eq!(
                super::store(
                    handle,
                    key,
                    value.as_ptr().cast(),
                    7,
                    string,
                    LV2_STATE_IS_POD
                ),
                LV2_STATE_SUCCESS
            );
            assert_eq!(
                super::store(handle, key, value.as_ptr().cast(), 7, string, 0),
                LV2_STATE_ERR_BAD_FLAGS
            );
            assert_eq!(
                super::store(
                    handle,
                    0,
                    value.as_ptr().cast(),
                    7,
                    string,
                    LV2_STATE_IS_POD
                ),
                LV2_STATE_ERR_BAD_TYPE
            );
        }
        assert_eq!(store.properties.len(), 1);

        let properties = store.properties;
        let handle = (&raw const properties).cast_mut().cast();
        let (mut size, mut type_, mut flags) = (0usize, 0u32, 0u32);
        // SAFETY: as above.
        let value = unsafe { super::retrieve(handle, key, &mut size, &mut type_, &mut flags) };
        assert!(!value.is_null() && value.cast::<u64>().is_aligned());
        // SAFETY: the value holds `size` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), size) };
        assert_eq!(bytes, b"second\0");
        assert_eq!((type_, flags), (string, LV2_STATE_IS_POD));
        let other = map_str("urn:manguebyte:test:never-stored");
        // SAFETY: as above.
        let missing = unsafe {
            super::retrieve(
                handle,
                other,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(missing.is_null());
    }
}
