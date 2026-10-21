//! Opening a system library at run time, with `dlopen` and `dlsym`
//! (POSIX.1-2008, `<dlfcn.h>`).

use std::ffi::{CStr, c_void};
use std::mem::{size_of, transmute_copy};
use std::ptr::NonNull;

/// A library opened with `dlopen`. It is never closed: the functions read
/// from it are kept in statics for the life of the process.
pub(crate) struct Library(NonNull<c_void>);

// SAFETY: a handle from `dlopen` is an opaque token that `dlsym` accepts
// from any thread (POSIX requires both functions to be thread-safe), and
// this type never closes it.
unsafe impl Send for Library {}
// SAFETY: as above; nothing is ever written through the handle.
unsafe impl Sync for Library {}

impl Library {
    /// Opens the library at `path` (a file name, looked up as `dlopen`
    /// does, or a full path), or `None` if it is not there.
    pub(crate) fn open(path: &CStr) -> Option<Self> {
        // SAFETY: `path` is a NUL-terminated string that outlives the call;
        // `dlopen` has no other precondition. Opening a system library
        // runs its initialisers, which the system's own libraries are made
        // to have run in any process.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        NonNull::new(handle).map(Self)
    }

    /// The address of the symbol `name`, or `None` if the library has no
    /// such symbol.
    pub(crate) fn address(&self, name: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: the handle came from `dlopen` and is never closed, and
        // `name` is NUL-terminated.
        NonNull::new(unsafe { libc::dlsym(self.0.as_ptr(), name.as_ptr()) })
    }

    /// The address of the symbol `name` in any image the process has
    /// loaded (`RTLD_DEFAULT`), or `None`.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) fn global_address(name: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: `RTLD_DEFAULT` is a pseudo-handle `dlsym` accepts, and
        // `name` is NUL-terminated.
        NonNull::new(unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) })
    }

    /// The function `name`, as the function pointer type `F`.
    ///
    /// # Safety
    ///
    /// `F` must be an `unsafe extern "C" fn` type whose parameters and
    /// return value match the C declaration of `name`.
    pub(crate) unsafe fn function<F: Copy>(&self, name: &CStr) -> Option<F> {
        const {
            assert!(size_of::<F>() == size_of::<*mut c_void>());
        }
        let address = self.address(name)?;
        // SAFETY: `F` is a function pointer type as wide as a data
        // pointer (checked above), and the caller promises it matches the
        // symbol's C declaration; a non-null address from `dlsym` for a
        // function is that function's entry point.
        Some(unsafe { transmute_copy::<*mut c_void, F>(&address.as_ptr()) })
    }
}
