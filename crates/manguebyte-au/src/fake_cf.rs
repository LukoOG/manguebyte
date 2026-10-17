//! A stand-in, on systems other than macOS, for the CoreFoundation and
//! AudioToolbox functions the wrapper calls.
//!
//! The Audio Unit itself only runs on macOS. The wrapper is plain Rust
//! around a C interface, though, and what it does with that interface is
//! the same everywhere, so it is built and tested on every system against
//! these: objects with a reference count, as CoreFoundation's are, holding
//! a string, bytes, a number, a dictionary keyed by strings, or an array.
//! They follow CoreFoundation's rules where the wrapper relies on them: a
//! function with `Create` in its name returns an object the caller owns,
//! `CFRelease` gives one up, and a dictionary or an array holds on to what
//! is put in it. They are not CoreFoundation, and nothing outside this
//! crate's tests should use them.

#![allow(non_snake_case, non_upper_case_globals, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::sys::{
    AudioBufferList, AudioTimeStamp, AudioUnit, AudioUnitRenderActionFlags, Boolean,
    CFAllocatorRef, CFArrayCallBacks, CFArrayRef, CFDataRef, CFDictionaryKeyCallBacks,
    CFDictionaryRef, CFDictionaryValueCallBacks, CFIndex, CFMutableDictionaryRef, CFNumberRef,
    CFNumberType, CFStringEncoding, CFStringRef, CFTypeID, CFTypeRef, OSStatus,
    kAudioUnitErr_NoConnection, kCFNumberSInt32Type, kCFNumberSInt64Type, kCFStringEncodingUTF8,
};

const STRING: CFTypeID = 7;
const DATA: CFTypeID = 20;
const NUMBER: CFTypeID = 22;
const DICTIONARY: CFTypeID = 18;
const ARRAY: CFTypeID = 19;

enum Payload {
    String(String),
    Data(Vec<u8>),
    Number(i64),
    Dictionary(Vec<(CFTypeRef, CFTypeRef)>),
    Array(Vec<CFTypeRef>),
}

struct Object {
    kind: CFTypeID,
    count: AtomicUsize,
    payload: Payload,
}

/// How many objects are alive, for the tests to check that nothing leaks.
pub static LIVE: AtomicUsize = AtomicUsize::new(0);

fn create(kind: CFTypeID, payload: Payload) -> *const c_void {
    LIVE.fetch_add(1, Ordering::Relaxed);
    Box::into_raw(Box::new(Object {
        kind,
        count: AtomicUsize::new(1),
        payload,
    }))
    .cast()
}

/// # Safety
///
/// `cf` must be null or an object made here that is alive.
unsafe fn object<'a>(cf: CFTypeRef) -> Option<&'a Object> {
    // SAFETY: per this function's contract.
    unsafe { cf.cast::<Object>().as_ref() }
}

pub unsafe fn CFRelease(cf: CFTypeRef) {
    // SAFETY: the caller passes an object it owns a reference to.
    let Some(found) = (unsafe { object(cf) }) else {
        return;
    };
    if found.count.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    // SAFETY: that was the last reference: nothing else can reach it.
    let owned = unsafe { Box::from_raw(cf.cast::<Object>().cast_mut()) };
    LIVE.fetch_sub(1, Ordering::Relaxed);
    match owned.payload {
        Payload::Dictionary(entries) => {
            for (key, value) in entries {
                // SAFETY: the dictionary held a reference to each.
                unsafe {
                    CFRelease(key);
                    CFRelease(value);
                }
            }
        }
        Payload::Array(values) => {
            for value in values {
                // SAFETY: the array held a reference to each.
                unsafe { CFRelease(value) };
            }
        }
        _ => {}
    }
}

pub unsafe fn CFRetain(cf: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller passes a live object.
    if let Some(found) = unsafe { object(cf) } {
        found.count.fetch_add(1, Ordering::Relaxed);
    }
    cf
}

pub unsafe fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID {
    // SAFETY: the caller passes a live object.
    unsafe { object(cf) }.map_or(0, |found| found.kind)
}

pub unsafe fn CFStringGetTypeID() -> CFTypeID {
    STRING
}

pub unsafe fn CFDataGetTypeID() -> CFTypeID {
    DATA
}

pub unsafe fn CFNumberGetTypeID() -> CFTypeID {
    NUMBER
}

pub unsafe fn CFDictionaryGetTypeID() -> CFTypeID {
    DICTIONARY
}

/// # Safety
///
/// `bytes` must hold `length` readable bytes.
unsafe fn slice<'a>(bytes: *const u8, length: CFIndex) -> Option<&'a [u8]> {
    let length = usize::try_from(length).ok()?;
    if length == 0 {
        return Some(&[]);
    }
    // SAFETY: per this function's contract.
    (!bytes.is_null()).then(|| unsafe { std::slice::from_raw_parts(bytes, length) })
}

pub unsafe fn CFStringCreateWithBytes(
    _alloc: CFAllocatorRef,
    bytes: *const u8,
    numBytes: CFIndex,
    encoding: CFStringEncoding,
    _isExternalRepresentation: Boolean,
) -> CFStringRef {
    if encoding != kCFStringEncodingUTF8 {
        return std::ptr::null();
    }
    // SAFETY: the caller passes `numBytes` readable bytes.
    let Some(text) = (unsafe { slice(bytes, numBytes) }).and_then(|b| std::str::from_utf8(b).ok())
    else {
        return std::ptr::null();
    };
    create(STRING, Payload::String(text.to_owned()))
}

fn text_of(found: &Object) -> Option<&str> {
    match &found.payload {
        Payload::String(text) => Some(text),
        _ => None,
    }
}

pub unsafe fn CFStringGetLength(theString: CFStringRef) -> CFIndex {
    // SAFETY: the caller passes a live string.
    unsafe { object(theString) }
        .and_then(text_of)
        .map_or(0, |text| text.encode_utf16().count() as CFIndex)
}

pub unsafe fn CFStringGetMaximumSizeForEncoding(
    length: CFIndex,
    _encoding: CFStringEncoding,
) -> CFIndex {
    length.saturating_mul(3)
}

pub unsafe fn CFStringGetCString(
    theString: CFStringRef,
    buffer: *mut c_char,
    bufferSize: CFIndex,
    encoding: CFStringEncoding,
) -> Boolean {
    // SAFETY: the caller passes a live string.
    let Some(text) = (unsafe { object(theString) }).and_then(text_of) else {
        return 0;
    };
    let Ok(room) = usize::try_from(bufferSize) else {
        return 0;
    };
    if encoding != kCFStringEncodingUTF8 || buffer.is_null() || text.len() + 1 > room {
        return 0;
    }
    // SAFETY: the caller's buffer holds `room` bytes, enough for the text
    // and its NUL.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer.cast::<u8>(), text.len());
        buffer.add(text.len()).write(0);
    }
    1
}

pub unsafe fn CFDataCreate(_alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef {
    // SAFETY: the caller passes `length` readable bytes.
    match unsafe { slice(bytes, length) } {
        Some(bytes) => create(DATA, Payload::Data(bytes.to_vec())),
        None => std::ptr::null(),
    }
}

pub unsafe fn CFDataGetLength(theData: CFDataRef) -> CFIndex {
    // SAFETY: the caller passes live data.
    match unsafe { object(theData) }.map(|found| &found.payload) {
        Some(Payload::Data(bytes)) => bytes.len() as CFIndex,
        _ => 0,
    }
}

pub unsafe fn CFDataGetBytePtr(theData: CFDataRef) -> *const u8 {
    // SAFETY: the caller passes live data.
    match unsafe { object(theData) }.map(|found| &found.payload) {
        Some(Payload::Data(bytes)) => bytes.as_ptr(),
        _ => std::ptr::null(),
    }
}

pub unsafe fn CFNumberCreate(
    _allocator: CFAllocatorRef,
    theType: CFNumberType,
    valuePtr: *const c_void,
) -> CFNumberRef {
    if valuePtr.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a number of the type it names.
    let value = unsafe {
        match theType {
            kCFNumberSInt32Type => i64::from(valuePtr.cast::<i32>().read_unaligned()),
            kCFNumberSInt64Type => valuePtr.cast::<i64>().read_unaligned(),
            _ => return std::ptr::null(),
        }
    };
    create(NUMBER, Payload::Number(value))
}

pub unsafe fn CFNumberGetValue(
    number: CFNumberRef,
    theType: CFNumberType,
    valuePtr: *mut c_void,
) -> Boolean {
    // SAFETY: the caller passes a live number.
    let Some(Payload::Number(value)) = (unsafe { object(number) }).map(|found| &found.payload)
    else {
        return 0;
    };
    if valuePtr.is_null() {
        return 0;
    }
    // SAFETY: the caller passes room for a number of the type it names.
    unsafe {
        match theType {
            kCFNumberSInt32Type => match i32::try_from(*value) {
                Ok(small) => valuePtr.cast::<i32>().write_unaligned(small),
                Err(_) => return 0,
            },
            kCFNumberSInt64Type => valuePtr.cast::<i64>().write_unaligned(*value),
            _ => return 0,
        }
    }
    1
}

pub unsafe fn CFDictionaryCreateMutable(
    _allocator: CFAllocatorRef,
    _capacity: CFIndex,
    _keyCallBacks: *const CFDictionaryKeyCallBacks,
    _valueCallBacks: *const CFDictionaryValueCallBacks,
) -> CFMutableDictionaryRef {
    create(DICTIONARY, Payload::Dictionary(Vec::new())).cast_mut()
}

/// Whether two keys are equal: strings by their text, anything else by
/// identity.
///
/// # Safety
///
/// Both must be live objects.
unsafe fn same_key(a: CFTypeRef, b: CFTypeRef) -> bool {
    // SAFETY: per this function's contract.
    match unsafe { (object(a).and_then(text_of), object(b).and_then(text_of)) } {
        (Some(a), Some(b)) => a == b,
        _ => std::ptr::eq(a, b),
    }
}

pub unsafe fn CFDictionarySetValue(
    theDict: CFMutableDictionaryRef,
    key: *const c_void,
    value: *const c_void,
) {
    if theDict.is_null() {
        return;
    }
    // SAFETY: the caller passes a live dictionary it alone is changing,
    // and a live key and value, which the dictionary now holds too.
    unsafe {
        let dictionary = &mut *theDict.cast::<Object>();
        let Payload::Dictionary(entries) = &mut dictionary.payload else {
            return;
        };
        CFRetain(key);
        CFRetain(value);
        if let Some(at) = entries.iter().position(|&(known, _)| same_key(known, key)) {
            let (old_key, old_value) = std::mem::replace(&mut entries[at], (key, value));
            CFRelease(old_key);
            CFRelease(old_value);
        } else {
            entries.push((key, value));
        }
    }
}

pub unsafe fn CFDictionaryGetValue(theDict: CFDictionaryRef, key: *const c_void) -> *const c_void {
    // SAFETY: the caller passes a live dictionary and key.
    unsafe {
        let Some(Payload::Dictionary(entries)) = object(theDict).map(|found| &found.payload) else {
            return std::ptr::null();
        };
        entries
            .iter()
            .find(|&&(known, _)| same_key(known, key))
            .map_or(std::ptr::null(), |&(_, value)| value)
    }
}

pub unsafe fn CFArrayCreate(
    _allocator: CFAllocatorRef,
    values: *const *const c_void,
    numValues: CFIndex,
    _callBacks: *const CFArrayCallBacks,
) -> CFArrayRef {
    let Ok(count) = usize::try_from(numValues) else {
        return std::ptr::null();
    };
    let mut held = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: the caller passes `numValues` live objects.
        unsafe {
            let value = values.add(index).read();
            CFRetain(value);
            held.push(value);
        }
    }
    create(ARRAY, Payload::Array(held))
}

/// What an array made here holds, for the tests.
///
/// # Safety
///
/// `array` must be a live array made here.
pub unsafe fn array_values(array: CFArrayRef) -> Vec<CFTypeRef> {
    // SAFETY: per this function's contract.
    match unsafe { object(array) }.map(|found| &found.payload) {
        Some(Payload::Array(values)) => values.clone(),
        _ => Vec::new(),
    }
}

/// A stand-in has no other units to pull audio from: a connection made
/// with `kAudioUnitProperty_MakeConnection` always reports none.
pub unsafe fn AudioUnitRender(
    _inUnit: AudioUnit,
    _ioActionFlags: *mut AudioUnitRenderActionFlags,
    _inTimeStamp: *const AudioTimeStamp,
    _inOutputBusNumber: u32,
    _inNumberFrames: u32,
    _ioData: *mut AudioBufferList,
) -> OSStatus {
    kAudioUnitErr_NoConnection
}

static KEY_CALLBACKS: u8 = 0;
static VALUE_CALLBACKS: u8 = 0;
static ARRAY_CALLBACKS: u8 = 0;

/// Stands in for `&kCFTypeDictionaryKeyCallBacks`.
pub fn type_dictionary_key_callbacks() -> *const CFDictionaryKeyCallBacks {
    (&raw const KEY_CALLBACKS).cast()
}

/// Stands in for `&kCFTypeDictionaryValueCallBacks`.
pub fn type_dictionary_value_callbacks() -> *const CFDictionaryValueCallBacks {
    (&raw const VALUE_CALLBACKS).cast()
}

/// Stands in for `&kCFTypeArrayCallBacks`.
pub fn type_array_callbacks() -> *const CFArrayCallBacks {
    (&raw const ARRAY_CALLBACKS).cast()
}
