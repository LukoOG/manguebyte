//! The ALSA sequencer's Universal MIDI Packet events (alsa-lib 1.2.10 and
//! Linux 6.5 or newer).
//!
//! `libasound.so.2` is opened with `dlopen` and its functions looked up by
//! name the first time they are needed, so a system with an older
//! alsa-lib, or none, reports what is missing instead of failing to start
//! the program. The kernel's part is checked when a client is opened: a
//! sequencer before Linux 6.5 refuses to make a client speak MIDI 2.0
//! (`snd_seq_set_client_midi_version`).
//!
//! A sequencer port is addressed as `midir` names it, `client:port`
//! ([`parse_address`](crate::seq_event::parse_address)). It speaks MIDI 2.0
//! when its client is a UMP client in the MIDI 2.0 protocol
//! (`snd_seq_client_info_get_midi_version` gives
//! [`CLIENT_UMP_MIDI_2_0`]); the sequencer itself converts between UMP
//! and MIDI 1.0 events for the others, which `manguebyte-devices` reaches
//! through `midir` instead.
//!
//! # Timing
//!
//! Events are sent direct ([`QUEUE_DIRECT`](crate::seq_event::QUEUE_DIRECT)),
//! not through a sequencer queue: each is delivered when it is written,
//! and the caller writes each packet when it is due, as
//! `manguebyte-devices`' output thread does.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::dl::Library;
use crate::seq_event::{CLIENT_UMP_MIDI_2_0, SeqUmpEvent};

/// `SND_SEQ_OPEN_OUTPUT`.
const OPEN_OUTPUT: c_int = 1;
/// `SND_SEQ_OPEN_INPUT`.
const OPEN_INPUT: c_int = 2;
/// `SND_SEQ_NONBLOCK`.
const NONBLOCK: c_int = 1;
/// `SND_SEQ_PORT_CAP_READ`.
const CAP_READ: c_uint = 1 << 0;
/// `SND_SEQ_PORT_CAP_WRITE`.
const CAP_WRITE: c_uint = 1 << 1;
/// `SND_SEQ_PORT_CAP_SUBS_READ`.
const CAP_SUBS_READ: c_uint = 1 << 5;
/// `SND_SEQ_PORT_CAP_SUBS_WRITE`.
const CAP_SUBS_WRITE: c_uint = 1 << 6;
/// `SND_SEQ_PORT_TYPE_MIDI_GENERIC`.
const TYPE_MIDI_GENERIC: c_uint = 1 << 1;
/// `SND_SEQ_PORT_TYPE_APPLICATION`.
const TYPE_APPLICATION: c_uint = 1 << 20;

type Seq = c_void;
type ClientInfo = c_void;

/// The calls used, as declared in alsa-lib's `seq.h`, `seqmid.h` and
/// `error.h`.
struct Api {
    open: unsafe extern "C" fn(
        handle: *mut *mut Seq,
        name: *const c_char,
        streams: c_int,
        mode: c_int,
    ) -> c_int,
    close: unsafe extern "C" fn(handle: *mut Seq) -> c_int,
    client_id: unsafe extern "C" fn(handle: *mut Seq) -> c_int,
    set_client_name: unsafe extern "C" fn(handle: *mut Seq, name: *const c_char) -> c_int,
    create_simple_port: unsafe extern "C" fn(
        handle: *mut Seq,
        name: *const c_char,
        caps: c_uint,
        kind: c_uint,
    ) -> c_int,
    connect_to: unsafe extern "C" fn(
        handle: *mut Seq,
        port: c_int,
        dest_client: c_int,
        dest_port: c_int,
    ) -> c_int,
    client_info_malloc: unsafe extern "C" fn(info: *mut *mut ClientInfo) -> c_int,
    client_info_free: unsafe extern "C" fn(info: *mut ClientInfo),
    any_client_info:
        unsafe extern "C" fn(handle: *mut Seq, client: c_int, info: *mut ClientInfo) -> c_int,
    strerror: unsafe extern "C" fn(error: c_int) -> *const c_char,
    /// alsa-lib 1.2.10.
    set_client_midi_version: unsafe extern "C" fn(handle: *mut Seq, version: c_int) -> c_int,
    /// alsa-lib 1.2.10.
    client_info_midi_version: unsafe extern "C" fn(info: *const ClientInfo) -> c_int,
    /// alsa-lib 1.2.10.
    ump_event_output_direct:
        unsafe extern "C" fn(handle: *mut Seq, event: *mut SeqUmpEvent) -> c_int,
    /// alsa-lib 1.2.10.
    ump_event_input: unsafe extern "C" fn(handle: *mut Seq, event: *mut *mut SeqUmpEvent) -> c_int,
}

impl Api {
    fn load() -> Result<Self, String> {
        let library = Library::open(c"libasound.so.2")
            .ok_or_else(|| String::from("libasound.so.2, the ALSA library, is not installed"))?;
        let missing = |name: &CStr| format!("libasound.so.2 has no {}", name.to_string_lossy());
        let too_old = |name: &CStr| {
            format!(
                "alsa-lib is older than 1.2.10: it has no {}",
                name.to_string_lossy()
            )
        };
        macro_rules! function {
            ($name:expr, $error:expr) => {
                // SAFETY: each field's type is the C declaration of the
                // function it is looked up by, in alsa-lib's headers;
                // `snd_seq_t`, `snd_seq_client_info_t` are opaque, and
                // `snd_seq_ump_event_t` is `SeqUmpEvent`.
                unsafe { library.function($name) }.ok_or_else(|| $error($name))?
            };
        }
        Ok(Self {
            open: function!(c"snd_seq_open", missing),
            close: function!(c"snd_seq_close", missing),
            client_id: function!(c"snd_seq_client_id", missing),
            set_client_name: function!(c"snd_seq_set_client_name", missing),
            create_simple_port: function!(c"snd_seq_create_simple_port", missing),
            connect_to: function!(c"snd_seq_connect_to", missing),
            client_info_malloc: function!(c"snd_seq_client_info_malloc", missing),
            client_info_free: function!(c"snd_seq_client_info_free", missing),
            any_client_info: function!(c"snd_seq_get_any_client_info", missing),
            strerror: function!(c"snd_strerror", missing),
            set_client_midi_version: function!(c"snd_seq_set_client_midi_version", too_old),
            client_info_midi_version: function!(c"snd_seq_client_info_get_midi_version", too_old),
            ump_event_output_direct: function!(c"snd_seq_ump_event_output_direct", too_old),
            ump_event_input: function!(c"snd_seq_ump_event_input", too_old),
        })
    }

    /// alsa-lib's words for the negative error code `error`.
    fn error(&self, error: c_int) -> String {
        // SAFETY: `snd_strerror` takes any number and returns a static,
        // NUL-terminated string, or null.
        let text = unsafe { (self.strerror)(error) };
        if text.is_null() {
            return format!("error {error}");
        }
        // SAFETY: a non-null result is a static NUL-terminated string.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(Api::load).as_ref().map_err(Clone::clone)
}

/// An open sequencer client in the MIDI 2.0 protocol, closed when
/// dropped.
struct Client {
    api: &'static Api,
    handle: NonNull<Seq>,
}

// SAFETY: an alsa-lib sequencer handle is not tied to the thread that
// opened it; it must only not be used by two threads at once, which
// `&mut self` on every call that writes through it guarantees.
unsafe impl Send for Client {}

impl Client {
    /// Opens a client called `name` for `streams`, without blocking, and
    /// makes it a UMP client in the MIDI 2.0 protocol.
    fn open(name: &str, streams: c_int) -> Result<Self, String> {
        let api = api()?;
        let name = CString::new(name).map_err(|_| String::from("a name with a NUL byte"))?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: `handle` is a valid place for the handle the call
        // writes, and "default" names the system's sequencer.
        let result = unsafe { (api.open)(&mut handle, c"default".as_ptr(), streams, NONBLOCK) };
        let handle = match NonNull::new(handle) {
            Some(handle) if result >= 0 => handle,
            _ => {
                return Err(format!(
                    "the ALSA sequencer could not be opened: {}",
                    api.error(result)
                ));
            }
        };
        let client = Self { api, handle };
        // SAFETY: the handle is open.
        let result = unsafe { (api.set_client_midi_version)(handle.as_ptr(), CLIENT_UMP_MIDI_2_0) };
        if result < 0 {
            return Err(format!(
                "the ALSA sequencer refused a MIDI 2.0 client, which needs Linux 6.5 or newer: {}",
                api.error(result)
            ));
        }
        let own = client.midi_version(client.id())?;
        if own != CLIENT_UMP_MIDI_2_0 {
            return Err(format!(
                "the ALSA sequencer kept the client at MIDI version {own}, not 2: \
                 Universal MIDI Packets need Linux 6.5 or newer"
            ));
        }
        // SAFETY: the handle is open and `name` is NUL-terminated.
        let result = unsafe { (api.set_client_name)(handle.as_ptr(), name.as_ptr()) };
        if result < 0 {
            return Err(format!("could not name the client: {}", api.error(result)));
        }
        Ok(client)
    }

    /// This client's number.
    fn id(&self) -> c_int {
        // SAFETY: the handle is open.
        unsafe { (self.api.client_id)(self.handle.as_ptr()) }
    }

    /// The MIDI version of client `client`: one of the `CLIENT_` values of
    /// [`seq_event`](crate::seq_event).
    fn midi_version(&self, client: c_int) -> Result<c_int, String> {
        let mut info = std::ptr::null_mut();
        // SAFETY: `info` is a valid place for the pointer the call writes.
        let result = unsafe { (self.api.client_info_malloc)(&mut info) };
        if result < 0 || info.is_null() {
            return Err(format!("out of memory: {}", self.api.error(result)));
        }
        // SAFETY: the handle is open and `info` was just allocated.
        let result = unsafe { (self.api.any_client_info)(self.handle.as_ptr(), client, info) };
        let version = if result < 0 {
            Err(format!(
                "no sequencer client {client}: {}",
                self.api.error(result)
            ))
        } else {
            // SAFETY: `info` holds what the call above filled in.
            Ok(unsafe { (self.api.client_info_midi_version)(info) })
        };
        // SAFETY: `info` was allocated above and is freed once.
        unsafe { (self.api.client_info_free)(info) };
        version
    }

    /// Creates a port called `name` with `caps`.
    fn create_port(&mut self, name: &str, caps: c_uint) -> Result<c_int, String> {
        let name = CString::new(name).map_err(|_| String::from("a name with a NUL byte"))?;
        // SAFETY: the handle is open and `name` is NUL-terminated.
        let port = unsafe {
            (self.api.create_simple_port)(
                self.handle.as_ptr(),
                name.as_ptr(),
                caps,
                TYPE_MIDI_GENERIC | TYPE_APPLICATION,
            )
        };
        if port < 0 {
            return Err(format!("could not create a port: {}", self.api.error(port)));
        }
        Ok(port)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: the handle is open and closed once; closing it deletes
        // the client's ports and subscriptions.
        unsafe { (self.api.close)(self.handle.as_ptr()) };
    }
}

/// Whether this system can send Universal MIDI Packets through the ALSA
/// sequencer, and, if not, why: alsa-lib and the kernel are both checked,
/// by opening a MIDI 2.0 client for a moment.
pub fn available() -> Result<(), String> {
    Client::open("Manguebyte probe", OPEN_OUTPUT).map(|_| ())
}

/// A MIDI 2.0 sequencer client, open to ask which protocol other clients
/// speak.
pub struct Sequencer {
    client: Client,
}

impl Sequencer {
    /// Opens a client called `name`. Fails, saying why, where the system
    /// cannot send Universal MIDI Packets.
    pub fn open(name: &str) -> Result<Self, String> {
        Ok(Self {
            client: Client::open(name, OPEN_OUTPUT)?,
        })
    }

    /// The MIDI version of client `client`:
    /// [`CLIENT_LEGACY_MIDI`](crate::seq_event::CLIENT_LEGACY_MIDI),
    /// [`CLIENT_UMP_MIDI_1_0`](crate::seq_event::CLIENT_UMP_MIDI_1_0) or
    /// [`CLIENT_UMP_MIDI_2_0`].
    pub fn client_midi_version(&self, client: u8) -> Result<i32, String> {
        self.client.midi_version(c_int::from(client))
    }
}

/// A port of a MIDI 2.0 client of its own, connected to one sequencer
/// port, sending UMP events to it. Nothing allocates after
/// [`open`](Self::open).
pub struct Output {
    client: Client,
    port: u8,
}

impl Output {
    /// Opens a client called `client_name` with a port called
    /// `port_name`, connected to port `dest_port` of client `dest_client`.
    pub fn open(
        client_name: &str,
        port_name: &str,
        dest_client: u8,
        dest_port: u8,
    ) -> Result<Self, String> {
        let mut client = Client::open(client_name, OPEN_OUTPUT)?;
        let port = client.create_port(port_name, CAP_READ | CAP_SUBS_READ)?;
        // SAFETY: the handle is open and `port` is one of its ports.
        let result = unsafe {
            (client.api.connect_to)(
                client.handle.as_ptr(),
                port,
                c_int::from(dest_client),
                c_int::from(dest_port),
            )
        };
        if result < 0 {
            return Err(format!(
                "could not connect to {dest_client}:{dest_port}: {}",
                client.api.error(result)
            ));
        }
        let port = u8::try_from(port).map_err(|_| format!("port number {port} out of range"))?;
        Ok(Self { client, port })
    }

    /// Sends one whole Universal MIDI Packet, unchanged, at once.
    pub fn send(&mut self, message: &[u32]) -> Result<(), String> {
        let mut event = SeqUmpEvent::to_subscribers(self.port, message)
            .ok_or_else(|| String::from("not one whole Universal MIDI Packet"))?;
        // SAFETY: the handle is open and `event` is a complete
        // `snd_seq_ump_event_t` (see `seq_event`), which the call reads.
        let result = unsafe {
            (self.client.api.ump_event_output_direct)(self.client.handle.as_ptr(), &mut event)
        };
        if result < 0 {
            Err(format!(
                "snd_seq_ump_event_output_direct failed: {}",
                self.client.api.error(result)
            ))
        } else {
            Ok(())
        }
    }
}

/// A port of a MIDI 2.0 client of its own that other ports can send UMP
/// events to: the receiving end of a loopback, for tests.
pub struct Input {
    client: Client,
    port: u8,
}

impl Input {
    /// Opens a client called `client_name` with a port called
    /// `port_name` that others can write and subscribe to.
    pub fn open(client_name: &str, port_name: &str) -> Result<Self, String> {
        let mut client = Client::open(client_name, OPEN_INPUT)?;
        let port = client.create_port(port_name, CAP_WRITE | CAP_SUBS_WRITE)?;
        let port = u8::try_from(port).map_err(|_| format!("port number {port} out of range"))?;
        Ok(Self { client, port })
    }

    /// The port's address, `(client, port)`, as `midir` writes it
    /// `client:port`.
    pub fn address(&self) -> (u8, u8) {
        (self.client.id() as u8, self.port)
    }

    /// The next event that has arrived, or `None` if there is none yet.
    pub fn read(&mut self) -> Result<Option<SeqUmpEvent>, String> {
        let mut event = std::ptr::null_mut();
        // SAFETY: the handle is open and `event` is a valid place for the
        // pointer the call writes.
        let result =
            unsafe { (self.client.api.ump_event_input)(self.client.handle.as_ptr(), &mut event) };
        if result == -libc::EAGAIN {
            return Ok(None);
        }
        if result < 0 {
            return Err(format!(
                "snd_seq_ump_event_input failed: {}",
                self.client.api.error(result)
            ));
        }
        if event.is_null() {
            return Ok(None);
        }
        // SAFETY: on success the pointer is to an event in alsa-lib's own
        // input buffer, valid until the next read; it is copied out here,
        // without assuming the buffer keeps it aligned.
        Ok(Some(unsafe { event.read_unaligned() }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Opens alsa-lib and looks every call up. CI sets
    /// `MANGUEBYTE_UMP_LIBRARY_REQUIRED=1` on runners whose alsa-lib has
    /// the UMP calls, where their absence is a failure. Where the
    /// sequencer is there too, a packet goes from one client of this
    /// process to another and back unchanged; nothing reaches any other
    /// program or device.
    #[test]
    fn alsa_is_found_or_says_what_is_missing() {
        if let Err(why) = api() {
            println!("no UMP output: {why}");
            assert!(
                std::env::var_os("MANGUEBYTE_UMP_LIBRARY_REQUIRED").is_none(),
                "{why}"
            );
            return;
        }
        let mut input = match Input::open("Manguebyte test", "in") {
            Ok(input) => input,
            Err(why) => {
                println!("no UMP output: {why}");
                assert!(why.contains("sequencer"), "{why}");
                return;
            }
        };
        let (client, port) = input.address();
        let sequencer = Sequencer::open("Manguebyte test probe").unwrap();
        assert_eq!(
            sequencer.client_midi_version(client),
            Ok(CLIENT_UMP_MIDI_2_0)
        );
        assert!(sequencer.client_midi_version(255).is_err());

        let mut output = Output::open("Manguebyte test", "out", client, port).unwrap();
        let note_on = [0x4290_3C00, 0x1234_0000];
        output.send(&note_on).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let event = loop {
            match input.read().unwrap() {
                Some(event) if event.is_ump() => break event,
                Some(_) => {}
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                None => panic!("the packet did not arrive"),
            }
        };
        assert_eq!(event.words(), note_on);
    }
}
