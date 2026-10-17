//! What the host gives a plugin when it creates it: a URID map and its
//! reverse, options with the block sizes and the sample rate, a bounded
//! block length, and a log.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use manguebyte_core::log::{LogLevel, LogQueue, MESSAGE_BYTES};

use super::sys::*;
use crate::threads::guard;

/// Most URIs the process maps. A plugin that keeps asking for new ones is
/// told no (URID zero) past this.
pub(crate) const MAX_URIDS: usize = 1 << 16;

/// Longest URI mapped.
pub(crate) const MAX_URI_BYTES: usize = 4_096;

/// Longest format string read from a plugin's log call.
const MAX_FORMAT_BYTES: usize = 1_024;

/// The URID table of the whole process: URID `n` is the `n`th URI mapped.
///
/// One table for every plugin keeps a URID meaning the same thing in every
/// instance, which is what LV2 asks of a host. Entries are never removed,
/// so the C string behind a URID stays where it is for the rest of the
/// process, as `urid:unmap` needs.
struct Table {
    uris: Vec<CString>,
    ids: HashMap<CString, u32>,
}

static TABLE: LazyLock<Mutex<Table>> = LazyLock::new(|| {
    Mutex::new(Table {
        uris: Vec::new(),
        ids: HashMap::new(),
    })
});

fn table() -> MutexGuard<'static, Table> {
    TABLE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The URID of `uri`, mapping it if it is new. Zero if the table is full.
pub(crate) fn map(uri: &CStr) -> u32 {
    if uri.to_bytes().len() > MAX_URI_BYTES {
        return 0;
    }
    let mut table = table();
    if let Some(&id) = table.ids.get(uri) {
        return id;
    }
    if table.uris.len() >= MAX_URIDS {
        return 0;
    }
    table.uris.push(uri.to_owned());
    let id = table.uris.len() as u32;
    table.ids.insert(uri.to_owned(), id);
    id
}

/// The URID of a URI given as text. Zero if it cannot be mapped.
pub(crate) fn map_str(uri: &str) -> u32 {
    CString::new(uri).map_or(0, |uri| map(&uri))
}

/// The URI behind a URID, if it has been mapped.
pub(crate) fn unmap(id: u32) -> Option<String> {
    let table = table();
    let index = usize::try_from(id).ok()?.checked_sub(1)?;
    table
        .uris
        .get(index)
        .map(|uri| uri.to_string_lossy().into_owned())
}

/// Reads a C string of at most `limit` bytes, or `None` if it is null or
/// longer.
///
/// # Safety
///
/// `text` must be null or point to readable bytes up to its NUL, or to at
/// least `limit` readable bytes.
pub(crate) unsafe fn bounded<'a>(text: *const c_char, limit: usize) -> Option<&'a [u8]> {
    if text.is_null() {
        return None;
    }
    let bytes = text.cast::<u8>();
    for length in 0..=limit {
        // SAFETY: no byte before this one was the NUL, so this one is part
        // of the string, or within the `limit` readable bytes.
        if unsafe { bytes.add(length).read() } == 0 {
            // SAFETY: the `length` bytes before the NUL are readable.
            return Some(unsafe { std::slice::from_raw_parts(bytes, length) });
        }
    }
    None
}

unsafe extern "C" fn map_uri(_handle: *mut c_void, uri: *const c_char) -> LV2_URID {
    guard(0, || {
        // SAFETY: a plugin passes a NUL-terminated URI.
        let Some(bytes) = (unsafe { bounded(uri, MAX_URI_BYTES) }) else {
            return 0;
        };
        CString::new(bytes).map_or(0, |uri| map(&uri))
    })
}

unsafe extern "C" fn unmap_urid(_handle: *mut c_void, urid: LV2_URID) -> *const c_char {
    guard(ptr::null(), || {
        let table = table();
        let Some(index) = usize::try_from(urid).ok().and_then(|id| id.checked_sub(1)) else {
            return ptr::null();
        };
        // The string's bytes stay where they are for the rest of the
        // process: entries are never removed, and moving the list moves the
        // `CString`s, not what they point to.
        table
            .uris
            .get(index)
            .map_or(ptr::null(), |uri| uri.as_ptr())
    })
}

// --- Log --------------------------------------------------------------------

/// What the log functions are handed back as their `handle`.
struct LogTarget {
    queue: LogQueue,
    error: u32,
    warning: u32,
    trace: u32,
}

impl LogTarget {
    fn level(&self, type_: u32) -> LogLevel {
        if type_ == self.error {
            LogLevel::Error
        } else if type_ == self.warning {
            LogLevel::Warning
        } else if type_ == self.trace {
            LogLevel::Trace
        } else {
            // `log:Note`, and any type LV2 does not define.
            LogLevel::Info
        }
    }
}

/// Posts a message the plugin logged. Real-time safe: the text is copied
/// into a buffer on the stack and posted to a wait-free queue, as LV2 lets
/// a plugin log from its audio thread.
///
/// # Safety
///
/// `handle` must be the `LogTarget` of a live instance, and `format` and
/// `argument` null or NUL-terminated.
unsafe fn post(handle: *mut c_void, type_: u32, format: *const c_char, argument: *const c_char) {
    // SAFETY: the handle given with the feature, which lives as long as the
    // instance that may call it.
    let Some(target) = (unsafe { handle.cast::<LogTarget>().as_ref() }) else {
        return;
    };
    // SAFETY: per this function's contract.
    let Some(format) = (unsafe { bounded(format, MAX_FORMAT_BYTES) }) else {
        return;
    };
    let mut text = [0u8; MESSAGE_BYTES];
    let mut length = 0;
    let mut push = |bytes: &[u8]| {
        for &byte in bytes {
            if length < text.len() {
                text[length] = byte;
                length += 1;
            }
        }
    };
    // A format that is nothing but one string, as most plugins log, prints
    // that string when it has been handed over. Any other format is shown
    // as it is, its conversions unexpanded: the values behind them cannot
    // be read portably without C's variadic machinery.
    let lone_string = matches!(format, b"%s" | b"%s\n");
    // SAFETY: per this function's contract.
    match (lone_string, unsafe { bounded(argument, MESSAGE_BYTES) }) {
        (true, Some(argument)) => push(argument),
        _ => {
            let mut rest = format;
            while let Some((&byte, after)) = rest.split_first() {
                if byte == b'%' && after.first() == Some(&b'%') {
                    push(b"%");
                    rest = &after[1..];
                } else {
                    push(&[byte]);
                    rest = after;
                }
            }
        }
    }
    let mut bytes = &text[..length];
    while let Some((&last, before)) = bytes.split_last() {
        if last == b'\n' || last == b'\r' {
            bytes = before;
        } else {
            break;
        }
    }
    let message = match std::str::from_utf8(bytes) {
        Ok(message) => message,
        Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default(),
    };
    target.queue.post(target.level(type_), message);
}

// `printf` in LV2 is a C variadic function, which stable Rust cannot
// define. A function with the same fixed arguments is called exactly like
// a variadic one on every platform Rust supports: what differs is only
// where the arguments after the format go. On the platforms below, the
// first of those is passed as a fourth ordinary argument would be (in a
// register on x86-64, AArch64 outside Apple's systems and 32-bit Arm, on
// the stack after the others on 32-bit x86), so this function takes it as
// one, and reads it only when the format says there is a string there. On
// the others, Apple's AArch64 among them, variadic arguments go somewhere
// else, so the function takes the three fixed ones and shows the format.

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "x86",
    target_arch = "arm",
    all(target_arch = "aarch64", not(target_vendor = "apple"))
))]
unsafe extern "C" fn log_printf(
    handle: *mut c_void,
    type_: u32,
    format: *const c_char,
    argument: *const c_char,
) -> c_int {
    guard(0, || {
        // SAFETY: the plugin passes the handle it was given and a format.
        // `argument` is the first variadic argument, which `post` reads
        // only when the format says it is a string.
        unsafe { post(handle, type_, format, argument) };
        0
    })
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "x86",
    target_arch = "arm",
    all(target_arch = "aarch64", not(target_vendor = "apple"))
)))]
unsafe extern "C" fn log_printf(handle: *mut c_void, type_: u32, format: *const c_char) -> c_int {
    guard(0, || {
        // SAFETY: the plugin passes the handle it was given and a format.
        unsafe { post(handle, type_, format, ptr::null()) };
        0
    })
}

unsafe extern "C" fn log_vprintf(
    handle: *mut c_void,
    type_: u32,
    format: *const c_char,
    _arguments: *mut c_void,
) -> c_int {
    guard(0, || {
        // SAFETY: the plugin passes the handle it was given and a format.
        // A `va_list` cannot be read portably, so the format is shown.
        unsafe { post(handle, type_, format, ptr::null()) };
        0
    })
}

/// `log_printf` as the variadic type the feature's table declares.
fn printf_entry() -> LV2_Log_Printf {
    // SAFETY: see the comment above `log_printf`: the plugin calls it as a
    // C variadic function, which on every platform passes the fixed
    // arguments as this function takes them, and the fourth argument, where
    // it is declared, as the first variadic one.
    unsafe {
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "x86",
            target_arch = "arm",
            all(target_arch = "aarch64", not(target_vendor = "apple"))
        ))]
        type Fixed = unsafe extern "C" fn(*mut c_void, u32, *const c_char, *const c_char) -> c_int;
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "x86",
            target_arch = "arm",
            all(target_arch = "aarch64", not(target_vendor = "apple"))
        )))]
        type Fixed = unsafe extern "C" fn(*mut c_void, u32, *const c_char) -> c_int;
        std::mem::transmute::<Fixed, LV2_Log_Printf>(log_printf)
    }
}

// --- The features -------------------------------------------------------------

/// How many options the host gives, and room for the list's end.
const OPTIONS: usize = 6;
/// How many features the host gives, and room for the list's null.
const FEATURES: usize = 5;

/// Everything the features point at, in one allocation that does not move.
struct Block {
    map: LV2_URID_Map,
    unmap: LV2_URID_Unmap,
    log: LV2_Log_Log,
    target: LogTarget,
    min_block: i32,
    max_block: i32,
    nominal_block: i32,
    sequence_size: i32,
    sample_rate: f32,
    options: [LV2_Options_Option; OPTIONS],
    features: [LV2_Feature; FEATURES],
    list: [*const LV2_Feature; FEATURES + 1],
}

/// The features one instance is created with, and the log it writes to.
///
/// The plugin keeps pointers into them for as long as it lives, so they
/// are held through a raw pointer and only ever reached through it: moving
/// this value moves the pointer, and nothing the plugin points at.
pub(crate) struct Features {
    block: *mut Block,
}

// SAFETY: after `new` the block is only read, through shared references,
// and its log queue is made to be posted to from any thread.
unsafe impl Send for Features {}
// SAFETY: as above.
unsafe impl Sync for Features {}

impl Features {
    /// The features for an instance at `sample_rate`, run in blocks of
    /// 1 to `max_block` frames, with atom buffers of `sequence_size`
    /// bytes.
    pub(crate) fn new(sample_rate: f64, max_block: usize, sequence_size: usize) -> Self {
        let int = map(LV2_ATOM__Int);
        let float = map(LV2_ATOM__Float);
        let max_block = i32::try_from(max_block).unwrap_or(i32::MAX);
        let empty_option = LV2_Options_Option {
            context: 0,
            subject: 0,
            key: 0,
            size: 0,
            type_: 0,
            value: ptr::null(),
        };
        let empty_feature = LV2_Feature {
            URI: ptr::null(),
            data: ptr::null_mut(),
        };
        let block = Box::into_raw(Box::new(Block {
            map: LV2_URID_Map {
                handle: ptr::null_mut(),
                map: Some(map_uri),
            },
            unmap: LV2_URID_Unmap {
                handle: ptr::null_mut(),
                unmap: Some(unmap_urid),
            },
            log: LV2_Log_Log {
                handle: ptr::null_mut(),
                printf: Some(printf_entry()),
                vprintf: Some(log_vprintf),
            },
            target: LogTarget {
                queue: LogQueue::new(),
                error: map(LV2_LOG__Error),
                warning: map(LV2_LOG__Warning),
                trace: map(LV2_LOG__Trace),
            },
            min_block: 1,
            max_block,
            nominal_block: max_block,
            sequence_size: i32::try_from(sequence_size).unwrap_or(i32::MAX),
            sample_rate: sample_rate as f32,
            options: [empty_option; OPTIONS],
            features: [empty_feature; FEATURES],
            list: [ptr::null(); FEATURES + 1],
        }));
        // SAFETY: the block was just allocated and nothing else points at it
        // yet. Every pointer below is to a field of it, made from the raw
        // pointer, and the block stays where it is until `drop`.
        unsafe {
            let option =
                |key: &CStr, type_: u32, size: u32, value: *const c_void| LV2_Options_Option {
                    context: LV2_OPTIONS_INSTANCE,
                    subject: 0,
                    key: map(key),
                    size,
                    type_,
                    value,
                };
            (*block).log.handle = (&raw mut (*block).target).cast();
            (*block).options = [
                option(
                    LV2_BUF_SIZE__minBlockLength,
                    int,
                    4,
                    (&raw const (*block).min_block).cast(),
                ),
                option(
                    LV2_BUF_SIZE__maxBlockLength,
                    int,
                    4,
                    (&raw const (*block).max_block).cast(),
                ),
                option(
                    LV2_BUF_SIZE__nominalBlockLength,
                    int,
                    4,
                    (&raw const (*block).nominal_block).cast(),
                ),
                option(
                    LV2_BUF_SIZE__sequenceSize,
                    int,
                    4,
                    (&raw const (*block).sequence_size).cast(),
                ),
                option(
                    LV2_PARAMETERS__sampleRate,
                    float,
                    4,
                    (&raw const (*block).sample_rate).cast(),
                ),
                empty_option,
            ];
            (*block).features = [
                LV2_Feature {
                    URI: LV2_URID__map.as_ptr(),
                    data: (&raw mut (*block).map).cast(),
                },
                LV2_Feature {
                    URI: LV2_URID__unmap.as_ptr(),
                    data: (&raw mut (*block).unmap).cast(),
                },
                LV2_Feature {
                    URI: LV2_OPTIONS__options.as_ptr(),
                    data: (&raw mut (*block).options).cast(),
                },
                LV2_Feature {
                    URI: LV2_BUF_SIZE__boundedBlockLength.as_ptr(),
                    data: ptr::null_mut(),
                },
                LV2_Feature {
                    URI: LV2_LOG__log.as_ptr(),
                    data: (&raw mut (*block).log).cast(),
                },
            ];
            let first = (&raw const (*block).features).cast::<LV2_Feature>();
            for index in 0..FEATURES {
                (*block).list[index] = first.add(index);
            }
        }
        Self { block }
    }

    /// The null-terminated list `instantiate` takes.
    pub(crate) fn list(&self) -> *const *const LV2_Feature {
        // SAFETY: the block lives until `drop`.
        unsafe { (&raw const (*self.block).list).cast() }
    }

    /// The messages the plugin has logged.
    pub(crate) fn log(&self) -> &LogQueue {
        // SAFETY: the block lives until `drop`, and the queue is only ever
        // used through shared references.
        unsafe { &(*self.block).target.queue }
    }
}

impl Drop for Features {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::into_raw` and is freed once,
        // after the instance that used it has been cleaned up.
        drop(unsafe { Box::from_raw(self.block) });
    }
}

/// The URIs of the features this host gives. A plugin that requires any
/// other cannot be hosted.
pub(crate) const PROVIDED: &[&str] = &[
    "http://lv2plug.in/ns/ext/urid#map",
    "http://lv2plug.in/ns/ext/urid#unmap",
    "http://lv2plug.in/ns/ext/options#options",
    "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength",
    "http://lv2plug.in/ns/ext/log#log",
    // Not features a host gives but properties of the plugin that some
    // bundles list as required; this host meets them as it is: it does not
    // run plugins in place, and it runs them in the audio thread.
    LV2_CORE__hardRTCapable,
    LV2_CORE__inPlaceBroken,
    LV2_CORE__isLive,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urids_are_stable_and_go_back_to_their_uri() {
        let first = map(c"urn:manguebyte:test:one");
        let second = map(c"urn:manguebyte:test:two");
        assert!(first != 0 && second != 0 && first != second);
        assert_eq!(map(c"urn:manguebyte:test:one"), first);
        assert_eq!(map_str("urn:manguebyte:test:two"), second);
        assert_eq!(unmap(first).as_deref(), Some("urn:manguebyte:test:one"));
        assert_eq!(unmap(0), None);
        assert_eq!(unmap(u32::MAX), None);
        let long = CString::new("u".repeat(MAX_URI_BYTES + 1)).unwrap();
        assert_eq!(map(&long), 0);

        // Through the functions a plugin is given.
        // SAFETY: called as a plugin would, with C strings.
        unsafe {
            assert_eq!(
                map_uri(ptr::null_mut(), c"urn:manguebyte:test:one".as_ptr()),
                first
            );
            assert_eq!(map_uri(ptr::null_mut(), ptr::null()), 0);
            let back = unmap_urid(ptr::null_mut(), second);
            assert_eq!(CStr::from_ptr(back), c"urn:manguebyte:test:two");
            assert!(unmap_urid(ptr::null_mut(), 0).is_null());
        }
    }

    #[test]
    fn the_feature_list_points_into_itself_and_ends() {
        let features = Features::new(44_100.0, 512, 8_192);
        let list = features.list();
        let mut uris = Vec::new();
        // SAFETY: the list is null-terminated and its features live as long
        // as `features`.
        unsafe {
            let mut index = 0;
            while !(*list.add(index)).is_null() {
                let feature = &**list.add(index);
                uris.push(CStr::from_ptr(feature.URI).to_str().unwrap().to_owned());
                if CStr::from_ptr(feature.URI) == LV2_OPTIONS__options {
                    let options = feature.data.cast::<LV2_Options_Option>();
                    let mut found = Vec::new();
                    let mut at = 0;
                    while (*options.add(at)).key != 0 {
                        let option = *options.add(at);
                        found.push((unmap(option.key).unwrap(), option.size));
                        if option.key == map(LV2_BUF_SIZE__maxBlockLength) {
                            assert_eq!(option.value.cast::<i32>().read(), 512);
                        }
                        if option.key == map(LV2_PARAMETERS__sampleRate) {
                            assert_eq!(option.value.cast::<f32>().read(), 44_100.0);
                        }
                        at += 1;
                    }
                    assert_eq!(found.len(), OPTIONS - 1);
                }
                index += 1;
            }
        }
        for uri in &uris {
            assert!(PROVIDED.contains(&uri.as_str()), "{uri}");
        }
        assert_eq!(uris.len(), FEATURES);
    }

    #[test]
    fn a_log_message_is_posted_without_its_conversions_expanded() {
        let features = Features::new(48_000.0, 64, 1_024);
        // SAFETY: the block lives as long as `features`.
        let handle: *mut c_void = unsafe { (&raw mut (*features.block).target).cast() };
        let warning = map(LV2_LOG__Warning);
        // SAFETY: called as a plugin would, with the handle it was given.
        unsafe {
            post(
                handle,
                warning,
                c"%d voices, 100%% busy\n".as_ptr(),
                ptr::null(),
            );
            post(
                handle,
                map(LV2_LOG__Error),
                c"%s\n".as_ptr(),
                c"it broke".as_ptr(),
            );
            post(handle, warning, ptr::null(), ptr::null());
            vprintf_like(handle);
        }
        let drained = features.log().drain();
        let found: Vec<(LogLevel, &str)> = drained
            .records
            .iter()
            .map(|record| (record.level, record.text.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (LogLevel::Warning, "%d voices, 100% busy"),
                (LogLevel::Error, "it broke"),
                (LogLevel::Info, "plain"),
            ]
        );
    }

    /// Calls the `vprintf` of the feature as a plugin would.
    unsafe fn vprintf_like(handle: *mut c_void) {
        // SAFETY: passed on from the caller.
        unsafe { log_vprintf(handle, 0, c"plain".as_ptr(), ptr::null_mut()) };
    }
}
