//! CoreFoundation objects, owned and read with care.

use std::ffi::c_void;

use crate::sys::*;

/// Longest string read from a host.
pub(crate) const MAX_STRING_BYTES: usize = 4_096;

/// A CoreFoundation object this code holds a reference to, given up when
/// it is dropped.
pub(crate) struct Owned(CFTypeRef);

impl Owned {
    /// Takes over a reference returned by a `Create` function. `None` if
    /// there is none.
    pub(crate) fn new(object: CFTypeRef) -> Option<Self> {
        (!object.is_null()).then_some(Self(object))
    }

    pub(crate) fn as_ptr(&self) -> CFTypeRef {
        self.0
    }

    /// Hands the reference over to the caller, who must release it.
    pub(crate) fn into_raw(self) -> CFTypeRef {
        let object = self.0;
        std::mem::forget(self);
        object
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: this value holds one reference to a live object.
        unsafe { CFRelease(self.0) };
    }
}

/// A new string with `text`.
pub(crate) fn string(text: &str) -> Option<Owned> {
    // SAFETY: the bytes are `text`'s own, of the length given, in UTF-8.
    Owned::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            text.as_ptr(),
            text.len() as CFIndex,
            kCFStringEncodingUTF8,
            0,
        )
    })
}

/// New data holding `bytes`.
pub(crate) fn data(bytes: &[u8]) -> Option<Owned> {
    // SAFETY: the bytes are readable for the length given.
    Owned::new(unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex) })
}

/// A new number holding `value`.
pub(crate) fn number(value: i32) -> Option<Owned> {
    // SAFETY: the value is an `i32`, the type named.
    Owned::new(unsafe {
        CFNumberCreate(
            std::ptr::null(),
            kCFNumberSInt32Type,
            (&raw const value).cast(),
        )
    })
}

/// A new array of `values`, which it holds references to.
pub(crate) fn array(values: &[Owned]) -> Option<Owned> {
    let pointers: Vec<*const c_void> = values.iter().map(Owned::as_ptr).collect();
    // SAFETY: the list holds `values.len()` live objects, and the callbacks
    // are CoreFoundation's own for objects.
    Owned::new(unsafe {
        CFArrayCreate(
            std::ptr::null(),
            pointers.as_ptr(),
            pointers.len() as CFIndex,
            type_array_callbacks(),
        )
    })
}

/// A new, empty dictionary of objects.
pub(crate) fn dictionary() -> Option<Owned> {
    // SAFETY: CoreFoundation's own callbacks for a dictionary of objects.
    Owned::new(
        unsafe {
            CFDictionaryCreateMutable(
                std::ptr::null(),
                0,
                type_dictionary_key_callbacks(),
                type_dictionary_value_callbacks(),
            )
        }
        .cast_const(),
    )
}

/// Puts `value` in `dictionary` under `key`. Returns whether it could.
pub(crate) fn set(dictionary: &Owned, key: &str, value: &Owned) -> bool {
    let Some(key) = string(key) else {
        return false;
    };
    // SAFETY: a dictionary this code made, and live objects, which it now
    // holds references to.
    unsafe { CFDictionarySetValue(dictionary.as_ptr().cast_mut(), key.as_ptr(), value.as_ptr()) };
    true
}

/// Whether `object` is of the type `type_id` gives.
///
/// # Safety
///
/// `object` must be null or a live CoreFoundation object.
pub(crate) unsafe fn is(object: CFTypeRef, type_id: CFTypeID) -> bool {
    // SAFETY: per this function's contract.
    !object.is_null() && unsafe { CFGetTypeID(object) } == type_id
}

/// The value under `key` in `dictionary`, if there is one. The dictionary
/// holds the reference.
///
/// # Safety
///
/// `dictionary` must be a live dictionary.
pub(crate) unsafe fn get(dictionary: CFDictionaryRef, key: &str) -> CFTypeRef {
    let Some(key) = string(key) else {
        return std::ptr::null();
    };
    // SAFETY: per this function's contract, and a live key.
    unsafe { CFDictionaryGetValue(dictionary, key.as_ptr()) }
}

/// The text of a string, if it is one and no longer than
/// [`MAX_STRING_BYTES`] in UTF-8.
///
/// # Safety
///
/// `text` must be null or a live CoreFoundation object.
pub(crate) unsafe fn read_string(text: CFStringRef) -> Option<String> {
    // SAFETY: per this function's contract.
    if !unsafe { is(text, CFStringGetTypeID()) } {
        return None;
    }
    // SAFETY: a live string.
    let length = unsafe { CFStringGetLength(text) };
    // SAFETY: a plain computation.
    let room = unsafe { CFStringGetMaximumSizeForEncoding(length, kCFStringEncodingUTF8) };
    let room = usize::try_from(room).ok()?.min(MAX_STRING_BYTES * 2) + 1;
    let mut buffer = vec![0u8; room];
    // SAFETY: a live string and a buffer of `room` writable bytes.
    let done = unsafe {
        CFStringGetCString(
            text,
            buffer.as_mut_ptr().cast(),
            room as CFIndex,
            kCFStringEncodingUTF8,
        )
    };
    if done == 0 {
        return None;
    }
    let end = buffer.iter().position(|&byte| byte == 0)?;
    buffer.truncate(end);
    String::from_utf8(buffer)
        .ok()
        .filter(|text| text.len() <= MAX_STRING_BYTES)
}

/// The bytes of data, if it is data and holds at most `limit` bytes.
///
/// # Safety
///
/// `data` must be null or a live CoreFoundation object, which outlives the
/// slice.
pub(crate) unsafe fn read_data<'a>(data: CFDataRef, limit: usize) -> Option<&'a [u8]> {
    // SAFETY: per this function's contract.
    if !unsafe { is(data, CFDataGetTypeID()) } {
        return None;
    }
    // SAFETY: live data.
    let length = usize::try_from(unsafe { CFDataGetLength(data) }).ok()?;
    if length > limit {
        return None;
    }
    if length == 0 {
        return Some(&[]);
    }
    // SAFETY: live data, whose bytes are readable for its length.
    let bytes = unsafe { CFDataGetBytePtr(data) };
    // SAFETY: as above.
    (!bytes.is_null()).then(|| unsafe { std::slice::from_raw_parts(bytes, length) })
}

/// A number's value, if it is a number that fits in an `i64`.
///
/// # Safety
///
/// `number` must be null or a live CoreFoundation object.
pub(crate) unsafe fn read_number(number: CFNumberRef) -> Option<i64> {
    // SAFETY: per this function's contract.
    if !unsafe { is(number, CFNumberGetTypeID()) } {
        return None;
    }
    let mut value = 0i64;
    // SAFETY: a live number, and room for an `i64`.
    let read = unsafe { CFNumberGetValue(number, kCFNumberSInt64Type, (&raw mut value).cast()) };
    (read != 0).then_some(value)
}
