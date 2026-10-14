//! Telling threads apart, and keeping panics on this side of a plugin.
//!
//! Every plugin format has rules about which thread may call what. Both
//! hosts in this crate hold a plugin to them with the same two tools.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};

/// A number that stands for the calling thread and for no other thread this
/// process has had or will have, and that is never zero. Reading it does
/// not allocate.
///
/// It is a serial number, handed out the first time a thread asks. The
/// address of a thread-local would do to tell apart threads that are alive
/// at the same time, but the system gives the address of a thread that has
/// ended to a later one, and a plugin's main thread may end while its audio
/// half lives on: the later thread would then pass for the main thread.
pub(crate) fn thread_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static TOKEN: Cell<u64> = const { Cell::new(0) };
    }
    // The cell has no destructor, so it can be read for as long as the
    // thread runs. Should a system ever refuse, the thread is given the
    // token no thread has.
    TOKEN
        .try_with(|token| {
            let mut own = token.get();
            if own == 0 {
                // 2^64 threads are not started in the life of a process, so
                // the count does not come round to a number already given.
                own = NEXT.fetch_add(1, Ordering::Relaxed);
                token.set(own);
            }
            own
        })
        .unwrap_or(0)
}

/// Runs `work`, turning a panic into `fallback`.
///
/// A panic must not unwind into the plugin. None of the callbacks a plugin
/// is given is expected to panic; this makes sure that a mistake here costs
/// one wrong answer and not the process.
pub(crate) fn guard<R>(fallback: R, work: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or(fallback)
}
