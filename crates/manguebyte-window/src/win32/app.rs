//! An application's side of the Win32 backend: OLE for its thread, the
//! thread's message loop, and the application's timers.
//!
//! An application's timers are thread timers (`SetTimer` with no window)
//! with a timer procedure: `DispatchMessageW` calls it for their
//! `WM_TIMER`, in the application's loop and in any other loop of the
//! thread, a modal one included. The system chooses their ids, and a table
//! of the thread finds the application of each.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::rc::{Rc, Weak};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, SetTimer, TranslateMessage, USER_TIMER_MINIMUM,
};

use super::last_error;
use crate::app::AppInner;
use crate::dispatch::drop_quietly;
use crate::window::WindowError;

thread_local! {
    /// The application of each of the thread's application timers, by the
    /// system's id.
    static TIMERS: RefCell<Vec<(usize, Weak<AppInner>)>> = const { RefCell::new(Vec::new()) };
}

/// Starts a thread timer of `app`, or restarts the one the system knows as
/// `running`, and returns the system's id for it.
pub(crate) fn set_timer(
    running: Option<usize>,
    period_ms: u32,
    app: Weak<AppInner>,
) -> Result<usize, WindowError> {
    // SAFETY: a thread timer (no window) whose procedure is ours. With an
    // id the system gave, it replaces that timer; with 0 it makes one.
    let system = unsafe {
        SetTimer(
            ptr::null_mut(),
            running.unwrap_or(0),
            period_ms.max(USER_TIMER_MINIMUM),
            Some(timer_procedure),
        )
    };
    if system == 0 {
        return Err(last_error("SetTimer"));
    }
    TIMERS.with_borrow_mut(|timers| {
        timers.retain(|(kept, _)| *kept != system);
        timers.push((system, app));
    });
    Ok(system)
}

/// Stops the thread timer the system knows as `system`.
pub(crate) fn kill_timer(system: usize) {
    // SAFETY: kills a thread timer; an id that is not one only fails.
    unsafe {
        KillTimer(ptr::null_mut(), system);
    }
    // While the thread ends the table may be gone already: nothing is left
    // to find then.
    let _ = TIMERS.try_with(|timers| {
        if let Ok(mut timers) = timers.try_borrow_mut() {
            timers.retain(|(kept, _)| *kept != system);
        }
    });
}

/// The procedure of every application timer.
unsafe extern "system" fn timer_procedure(_: HWND, _: u32, system: usize, _: u32) {
    let app = TIMERS.with_borrow(|timers| {
        timers
            .iter()
            .find(|(kept, _)| *kept == system)
            .map(|(_, app)| app.clone())
    });
    let Some(app) = app.as_ref().and_then(Weak::upgrade) else {
        // A timer whose application is gone.
        kill_timer(system);
        return;
    };
    let result = panic::catch_unwind(AssertUnwindSafe(|| AppInner::fire_system(&app, system)));
    if let Err(payload) = result {
        drop_quietly(payload);
        AppInner::kill_system(&app, system);
    }
    drop_last(app);
}

/// Lets the application go, if this was the last of it, without unwinding
/// out of the timer procedure.
fn drop_last(app: Rc<AppInner>) {
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(move || drop(app))) {
        drop_quietly(payload);
    }
}

/// OLE on the application's thread, and its loop.
pub(crate) struct Win32App {
    /// Whether this application initialised OLE, and so uninitialises it.
    ole: bool,
}

impl Win32App {
    /// Initialises OLE for this thread, which drag and drop needs and the
    /// file dialogs want: a single-threaded apartment. A thread that is
    /// already a multithreaded apartment keeps it; the application then
    /// takes no drops.
    pub(crate) fn new() -> Win32App {
        // SAFETY: the reserved argument is null, as documented. Every
        // success, `S_FALSE` (already initialised) included, is balanced by
        // the `OleUninitialize` in `drop`, on this thread, since the value
        // cannot leave it (an `App` is not `Send`).
        let result = unsafe { OleInitialize(ptr::null()) };
        Win32App { ole: result >= 0 }
    }

    /// Whether OLE is initialised for the thread by this application.
    pub(crate) fn has_ole(&self) -> bool {
        self.ole
    }

    /// Waits for the thread's next message and dispatches it. Returns the
    /// exit code when the message is `WM_QUIT`.
    pub(crate) fn next_message(&self) -> Result<Option<i32>, WindowError> {
        let mut message = MSG::default();
        // SAFETY: `message` is a valid place for a message of this thread.
        let got = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        match got {
            0 => Ok(Some(message.wParam as i32)),
            -1 => Err(last_error("GetMessageW")),
            _ => {
                // SAFETY: a message just taken from this thread's queue.
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                Ok(None)
            }
        }
    }
}

impl Drop for Win32App {
    fn drop(&mut self) {
        if self.ole {
            // SAFETY: balances the successful `OleInitialize` of `new`, on
            // the same thread. The application's windows hold it alive, so
            // none of them is still registered for drops.
            unsafe {
                OleUninitialize();
            }
        }
    }
}
