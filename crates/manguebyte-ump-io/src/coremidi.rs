//! CoreMIDI's Universal MIDI Packet output: `MIDISendEventList` with
//! event lists in the MIDI 2.0 protocol (macOS 11 and iOS 14 or newer).
//!
//! CoreMIDI and CoreFoundation are opened with `dlopen` and their
//! functions looked up by name the first time they are needed
//! ([`available`]), so a system before `MIDISendEventList` reports what is
//! missing instead of failing to start the program.
//!
//! A destination is found by its `kMIDIPropertyUniqueID`, which is the
//! identifier `midir` gives a CoreMIDI port, and speaks MIDI 2.0 when its
//! `kMIDIPropertyProtocolID` is `kMIDIProtocol_2_0`. A destination that
//! does not set the property speaks MIDI 1.0.
//!
//! # Timing
//!
//! Every list is sent with the time stamp zero, "now": the caller sends
//! each packet when it is due, as `manguebyte-devices`' output thread
//! does. CoreMIDI could schedule ahead on its host clock
//! (`mach_absolute_time`), which this module does not use yet.

use std::ffi::{CStr, c_ulong, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::dl::Library;
use crate::event_list::{DEFAULT_CAPACITY, EventList, PROTOCOL_1_0, PROTOCOL_2_0};

type OsStatus = i32;
type MidiObjectRef = u32;
type ItemCount = c_ulong;
type CfStringRef = *const c_void;

/// `kCFStringEncodingUTF8`.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
/// `kMIDIUnknownProperty`: the object does not have the property.
const MIDI_UNKNOWN_PROPERTY: OsStatus = -10835;

const COREMIDI: &CStr = c"/System/Library/Frameworks/CoreMIDI.framework/CoreMIDI";
const COREFOUNDATION: &CStr = c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

/// The calls used, as declared in `MIDIServices.h` and `CFString.h`.
struct Api {
    client_create: unsafe extern "C" fn(
        name: CfStringRef,
        notify: *const c_void,
        context: *mut c_void,
        client: *mut MidiObjectRef,
    ) -> OsStatus,
    client_dispose: unsafe extern "C" fn(client: MidiObjectRef) -> OsStatus,
    output_port_create: unsafe extern "C" fn(
        client: MidiObjectRef,
        name: CfStringRef,
        port: *mut MidiObjectRef,
    ) -> OsStatus,
    port_dispose: unsafe extern "C" fn(port: MidiObjectRef) -> OsStatus,
    destination_count: unsafe extern "C" fn() -> ItemCount,
    destination: unsafe extern "C" fn(index: ItemCount) -> MidiObjectRef,
    integer_property: unsafe extern "C" fn(
        object: MidiObjectRef,
        property: CfStringRef,
        value: *mut i32,
    ) -> OsStatus,
    /// macOS 11, iOS 14.
    send_event_list: unsafe extern "C" fn(
        port: MidiObjectRef,
        destination: MidiObjectRef,
        list: *const u32,
    ) -> OsStatus,
    string_create: unsafe extern "C" fn(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CfStringRef,
    release: unsafe extern "C" fn(object: *const c_void),
    /// Where `kMIDIPropertyUniqueID` lives. Its value is read when it is
    /// used: CoreMIDI fills its property names in once it is in use, so
    /// they can still be null just after the library is opened.
    unique_id: Constant,
    /// Where `kMIDIPropertyProtocolID` (macOS 11, iOS 14) lives.
    protocol_id: Constant,
}

/// The address of a `const CFStringRef` a library exports, and its name.
struct Constant {
    address: NonNull<CfStringRef>,
    name: &'static CStr,
}

// SAFETY: the function pointers are plain code addresses, and the two
// addresses are those of CoreMIDI's property names, which CoreMIDI writes
// once and which live as long as the process; CoreMIDI's calls may be made
// from any thread.
unsafe impl Send for Api {}
// SAFETY: as above; nothing in `Api` is ever written after it is loaded.
unsafe impl Sync for Api {}

/// Finds a `const CFStringRef` that a library exports, in the library and
/// then among every loaded image.
fn constant(library: &Library, name: &'static CStr) -> Option<Constant> {
    let address = library
        .address(name)
        .or_else(|| Library::global_address(name))?;
    Some(Constant {
        address: address.cast::<CfStringRef>(),
        name,
    })
}

impl Api {
    fn load() -> Result<Self, String> {
        let coremidi =
            Library::open(COREMIDI).ok_or_else(|| String::from("CoreMIDI could not be opened"))?;
        let foundation = Library::open(COREFOUNDATION)
            .ok_or_else(|| String::from("CoreFoundation could not be opened"))?;
        let missing = |name: &CStr| format!("CoreMIDI has no {}", name.to_string_lossy());
        let too_old = |name: &CStr| {
            format!(
                "CoreMIDI has no {}: Universal MIDI Packets need macOS 11 or iOS 14",
                name.to_string_lossy()
            )
        };
        macro_rules! function {
            ($library:expr, $name:expr, $error:expr) => {
                // SAFETY: each field's type is the C declaration of the
                // function it is looked up by, in `MIDIServices.h` or
                // `CFString.h`/`CFBase.h`.
                unsafe { $library.function($name) }.ok_or_else(|| $error($name))?
            };
        }
        Ok(Self {
            client_create: function!(coremidi, c"MIDIClientCreate", missing),
            client_dispose: function!(coremidi, c"MIDIClientDispose", missing),
            output_port_create: function!(coremidi, c"MIDIOutputPortCreate", missing),
            port_dispose: function!(coremidi, c"MIDIPortDispose", missing),
            destination_count: function!(coremidi, c"MIDIGetNumberOfDestinations", missing),
            destination: function!(coremidi, c"MIDIGetDestination", missing),
            integer_property: function!(coremidi, c"MIDIObjectGetIntegerProperty", missing),
            send_event_list: function!(coremidi, c"MIDISendEventList", too_old),
            string_create: function!(foundation, c"CFStringCreateWithBytes", missing),
            release: function!(foundation, c"CFRelease", missing),
            unique_id: constant(&coremidi, c"kMIDIPropertyUniqueID")
                .ok_or_else(|| missing(c"kMIDIPropertyUniqueID"))?,
            protocol_id: constant(&coremidi, c"kMIDIPropertyProtocolID")
                .ok_or_else(|| too_old(c"kMIDIPropertyProtocolID"))?,
        })
    }

    /// The value of a property name, read now. If CoreMIDI has not filled
    /// it in yet, a call that puts CoreMIDI in use is made first.
    fn key(&self, constant: &Constant) -> Result<CfStringRef, String> {
        let read = || {
            // SAFETY: the address is that of an exported `const
            // CFStringRef`, so it points at one pointer-sized value.
            unsafe { constant.address.as_ptr().read_volatile() }
        };
        let mut value = read();
        if value.is_null() {
            // SAFETY: no arguments; it makes CoreMIDI set itself up.
            unsafe { (self.destination_count)() };
            value = read();
        }
        if value.is_null() {
            Err(format!(
                "CoreMIDI left {} null",
                constant.name.to_string_lossy()
            ))
        } else {
            Ok(value)
        }
    }

    /// An integer property of `object`, or `None` if it does not have it.
    fn integer(&self, object: MidiObjectRef, property: CfStringRef) -> Result<Option<i32>, String> {
        let mut value = 0;
        // SAFETY: `property` is one of CoreMIDI's property names and
        // `value` a valid place for the `SInt32` the call writes.
        let status = unsafe { (self.integer_property)(object, property, &mut value) };
        match status {
            0 => Ok(Some(value)),
            MIDI_UNKNOWN_PROPERTY => Ok(None),
            status => Err(format!(
                "MIDIObjectGetIntegerProperty failed: OSStatus {status}"
            )),
        }
    }

    /// The destination whose unique identifier is `unique_id`.
    fn find_destination(&self, unique_id: u32) -> Result<Option<MidiObjectRef>, String> {
        // SAFETY: no arguments; CoreMIDI counts its destinations.
        let count = unsafe { (self.destination_count)() };
        for index in 0..count {
            // SAFETY: `index` is below the count just read; a destination
            // removed since is returned as 0, which is skipped.
            let destination = unsafe { (self.destination)(index) };
            if destination == 0 {
                continue;
            }
            if self.integer(destination, self.key(&self.unique_id)?)? == Some(unique_id as i32) {
                return Ok(Some(destination));
            }
        }
        Ok(None)
    }

    /// A new CoreFoundation string holding `text`, released when dropped.
    fn string(&self, text: &str) -> Result<CfString<'_>, String> {
        // SAFETY: `text` is valid UTF-8 of the length given, and the
        // default allocator (null) is used.
        let string = unsafe {
            (self.string_create)(
                std::ptr::null(),
                text.as_ptr(),
                text.len() as isize,
                CF_STRING_ENCODING_UTF8,
                0,
            )
        };
        if string.is_null() {
            return Err(format!("could not make a CFString of \"{text}\""));
        }
        Ok(CfString { api: self, string })
    }
}

/// A CoreFoundation string this module made, released when dropped.
struct CfString<'a> {
    api: &'a Api,
    string: CfStringRef,
}

impl Drop for CfString<'_> {
    fn drop(&mut self) {
        // SAFETY: the string was returned by a `Create` call, so this
        // module owns one reference to it, released once here.
        unsafe { (self.api.release)(self.string) };
    }
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(Api::load).as_ref().map_err(Clone::clone)
}

/// Whether this system can send Universal MIDI Packets through CoreMIDI,
/// and, if not, why.
pub fn available() -> Result<(), String> {
    api().map(|_| ())
}

/// The protocol the destination with this unique identifier speaks, as
/// a `MIDIProtocolID` ([`PROTOCOL_1_0`] or [`PROTOCOL_2_0`]), or `None`
/// if there is no such destination. A destination that does not say is
/// taken to speak MIDI 1.0.
pub fn destination_protocol(unique_id: u32) -> Result<Option<i32>, String> {
    let api = api()?;
    let Some(destination) = api.find_destination(unique_id)? else {
        return Ok(None);
    };
    Ok(Some(
        api.integer(destination, api.key(&api.protocol_id)?)?
            .unwrap_or(PROTOCOL_1_0),
    ))
}

/// An output port of a client of its own, sending event lists in the
/// MIDI 2.0 protocol to one destination.
///
/// Messages are gathered in an [`EventList`] by [`send`](Self::send) and
/// handed to CoreMIDI by [`flush`](Self::flush), or by `send` when the
/// list is full. After [`open`](Self::open), nothing allocates.
pub struct Output {
    api: &'static Api,
    client: MidiObjectRef,
    port: MidiObjectRef,
    destination: MidiObjectRef,
    list: EventList,
}

impl Output {
    /// Opens a client called `client_name` with an output port called
    /// `port_name`, to send to the destination whose unique identifier is
    /// `unique_id`. Allocates.
    pub fn open(client_name: &str, port_name: &str, unique_id: u32) -> Result<Self, String> {
        let api = api()?;
        let destination = api
            .find_destination(unique_id)?
            .ok_or_else(|| format!("no CoreMIDI destination has the unique id {unique_id}"))?;
        let mut client = 0;
        {
            let name = api.string(client_name)?;
            // SAFETY: `name` is a live CFString, no notification callback
            // is given (null, with a null context), and `client` is a
            // valid place for the reference the call writes.
            let status = unsafe {
                (api.client_create)(
                    name.string,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    &mut client,
                )
            };
            if status != 0 {
                return Err(format!("MIDIClientCreate failed: OSStatus {status}"));
            }
        }
        let mut port = 0;
        let status = {
            let name = api.string(port_name)?;
            // SAFETY: `client` was just created, `name` is a live
            // CFString and `port` a valid place for the reference.
            unsafe { (api.output_port_create)(client, name.string, &mut port) }
        };
        if status != 0 {
            // SAFETY: the client was created above and is disposed once.
            unsafe { (api.client_dispose)(client) };
            return Err(format!("MIDIOutputPortCreate failed: OSStatus {status}"));
        }
        Ok(Self {
            api,
            client,
            port,
            destination,
            list: EventList::new(PROTOCOL_2_0, DEFAULT_CAPACITY),
        })
    }

    /// Adds one whole Universal MIDI Packet to the list, unchanged, and
    /// hands the list to CoreMIDI first if it is full.
    pub fn send(&mut self, message: &[u32]) -> Result<(), String> {
        if !crate::is_one_packet(message) {
            return Err(String::from("not one whole Universal MIDI Packet"));
        }
        if self.list.push(0, message) {
            return Ok(());
        }
        self.flush()?;
        if self.list.push(0, message) {
            Ok(())
        } else {
            Err(String::from("the packet does not fit an empty event list"))
        }
    }

    /// Hands what [`send`](Self::send) gathered to CoreMIDI, now.
    pub fn flush(&mut self) -> Result<(), String> {
        if self.list.is_empty() {
            return Ok(());
        }
        // SAFETY: the port and destination are live references, and the
        // words are a complete `MIDIEventList` (see `event_list`), four
        // bytes aligned, which CoreMIDI only reads during the call.
        let status = unsafe {
            (self.api.send_event_list)(self.port, self.destination, self.list.as_words().as_ptr())
        };
        self.list.clear();
        if status == 0 {
            Ok(())
        } else {
            Err(format!("MIDISendEventList failed: OSStatus {status}"))
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.flush();
        // SAFETY: the port and the client were created in `open` and are
        // disposed of once, the port first.
        unsafe {
            (self.api.port_dispose)(self.port);
            (self.api.client_dispose)(self.client);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens CoreMIDI and looks every call up, and asks for a destination
    /// that cannot exist. Sends nothing. CI sets
    /// `MANGUEBYTE_UMP_LIBRARY_REQUIRED=1` on runners recent enough to
    /// have the calls, where their absence is a failure.
    #[test]
    fn coremidi_is_found_or_says_what_is_missing() {
        match available() {
            Ok(()) => {
                assert_eq!(destination_protocol(u32::MAX), Ok(None));
                let Err(why) = Output::open("Manguebyte test", "out", u32::MAX) else {
                    panic!("no destination has that unique id");
                };
                assert!(why.contains("no CoreMIDI destination"), "{why}");
            }
            Err(why) => {
                println!("no UMP output: {why}");
                assert!(
                    std::env::var_os("MANGUEBYTE_UMP_LIBRARY_REQUIRED").is_none(),
                    "{why}"
                );
            }
        }
    }
}
