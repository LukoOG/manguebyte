//! The few Windows calls this crate makes: COM, the registry and the
//! message queue of the driver's thread. Declared by hand from the
//! Windows SDK's documentation, so the crate needs no bindings crate.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr;

use crate::Error;
use crate::registry::{Clsid, DriverEntry};
use crate::sys::{Guid, IAsio};

type Hkey = *mut c_void;

const COINIT_APARTMENTTHREADED: u32 = 0x2;
const CLSCTX_INPROC_SERVER: u32 = 0x1;
const S_OK: i32 = 0;
const S_FALSE: i32 = 1;

const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_MORE_DATA: i32 = 234;
const ERROR_NO_MORE_ITEMS: i32 = 259;
const KEY_READ: u32 = 0x2_0019;
const RRF_RT_ANY: u32 = 0x0000_ffff;
// The predefined keys are sign-extended 32-bit values.
const HKEY_CLASSES_ROOT: isize = 0x8000_0000_u32 as i32 as isize;
const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;

const PM_REMOVE: u32 = 0x1;

#[repr(C)]
struct Msg {
    hwnd: *mut c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
    private: u32,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        object: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Hkey,
        subkey: *const u16,
        options: u32,
        sam: u32,
        result: *mut Hkey,
    ) -> i32;
    fn RegEnumKeyExW(
        key: Hkey,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        last_write: *mut c_void,
    ) -> i32;
    fn RegGetValueW(
        key: Hkey,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        data_len: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: Hkey) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn PeekMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
}

/// Makes the calling thread a single-threaded COM apartment, as the SDK's
/// host code does before it creates a driver. Returns whether
/// [`uninitialize`] must be called when the thread is done.
pub(crate) fn initialize() -> bool {
    // SAFETY: no pointers are passed; COM documents the call as safe on
    // any thread.
    let result = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
    result == S_OK || result == S_FALSE
}

pub(crate) fn uninitialize() {
    // SAFETY: called once per successful `initialize`, on the same thread.
    unsafe { CoUninitialize() }
}

/// Creates a driver. Its class identifier is also the interface it is
/// asked for: a driver implements `IASIO` under its own identifier.
pub(crate) fn create_instance(clsid: &Clsid) -> Result<*mut IAsio, Error> {
    let mut object = ptr::null_mut();
    // SAFETY: both identifiers point at a live GUID and `object` at a
    // pointer COM fills in.
    let result = unsafe {
        CoCreateInstance(
            &clsid.0,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &clsid.0,
            &mut object,
        )
    };
    if result != S_OK || object.is_null() {
        return Err(Error::System {
            call: "CoCreateInstance",
            code: result,
        });
    }
    Ok(object.cast())
}

/// Hands every message waiting for the driver's thread to its window, as
/// a single-threaded apartment must: a driver may make windows or timers
/// of its own there.
pub(crate) fn pump_messages() {
    let mut msg = Msg {
        hwnd: ptr::null_mut(),
        message: 0,
        wparam: 0,
        lparam: 0,
        time: 0,
        pt_x: 0,
        pt_y: 0,
        private: 0,
    };
    // SAFETY: `msg` is a valid MSG for the system to fill in and read.
    unsafe {
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

fn from_wide(units: &[u16]) -> String {
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    OsString::from_wide(&units[..end])
        .to_string_lossy()
        .into_owned()
}

/// A registry key, closed when dropped.
struct Key(Hkey);

impl Key {
    fn open(parent: Hkey, path: &str) -> Result<Option<Self>, Error> {
        let path = wide(path);
        let mut key = ptr::null_mut();
        // SAFETY: `path` is NUL-terminated and `key` receives the handle.
        let status = unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &mut key) };
        match status {
            ERROR_SUCCESS => Ok(Some(Self(key))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            code => Err(Error::System {
                call: "RegOpenKeyExW",
                code,
            }),
        }
    }

    fn subkeys(&self) -> Result<Vec<String>, Error> {
        let mut names = Vec::new();
        for index in 0.. {
            // Key names are at most 255 characters.
            let mut name = [0u16; 256];
            let mut length = name.len() as u32;
            // SAFETY: `name` has room for `length` characters; the other
            // out-parameters are optional and passed as null.
            let status = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut length,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            match status {
                ERROR_SUCCESS => names.push(from_wide(&name[..length as usize])),
                ERROR_NO_MORE_ITEMS => break,
                code => {
                    return Err(Error::System {
                        call: "RegEnumKeyExW",
                        code,
                    });
                }
            }
        }
        Ok(names)
    }

    fn string(&self, subkey: &str, value: Option<&str>) -> Option<String> {
        string(self.0, subkey, value)
    }
}

/// A string value of a subkey, or `None` when either is missing.
fn string(key: Hkey, subkey: &str, value: Option<&str>) -> Option<String> {
    let subkey = wide(subkey);
    let value = value.map(wide);
    let value_ptr = value.as_ref().map_or(ptr::null(), |value| value.as_ptr());
    let mut buffer = vec![0u16; 260];
    loop {
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: the strings are NUL-terminated and `buffer` has room for
        // `bytes` bytes.
        let status = unsafe {
            RegGetValueW(
                key,
                subkey.as_ptr(),
                value_ptr,
                RRF_RT_ANY,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        match status {
            ERROR_SUCCESS => return Some(from_wide(&buffer[..bytes as usize / 2])),
            ERROR_MORE_DATA if buffer.len() < 1 << 16 => {
                buffer.resize((bytes as usize).div_ceil(2) + 1, 0);
            }
            _ => return None,
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from `RegOpenKeyExW` and is closed once.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

pub(crate) fn list_drivers() -> Result<Vec<DriverEntry>, Error> {
    let Some(asio) = Key::open(HKEY_LOCAL_MACHINE as Hkey, "SOFTWARE\\ASIO")? else {
        return Ok(Vec::new());
    };
    let mut drivers = Vec::new();
    for name in asio.subkeys()? {
        let Some(clsid) = asio
            .string(&name, Some("CLSID"))
            .and_then(|text| text.parse::<Clsid>().ok())
        else {
            continue;
        };
        let library = string(
            HKEY_CLASSES_ROOT as Hkey,
            &format!("CLSID\\{clsid}\\InprocServer32"),
            None,
        )
        .map(PathBuf::from);
        if !library.as_ref().is_some_and(|path| path.exists()) {
            continue;
        }
        let description = asio
            .string(&name, Some("Description"))
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| name.clone());
        drivers.push(DriverEntry {
            name,
            description,
            clsid,
            library,
        });
    }
    Ok(drivers)
}
