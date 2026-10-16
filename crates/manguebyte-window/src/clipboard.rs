//! The system's clipboard, as a window reaches it.

use std::fmt;
use std::rc::{Rc, Weak};

use crate::window::{Inner, WindowError};

/// The most UTF-16 units of text read from the clipboard: 64 Ki, more than
/// any field of an editor takes. Text beyond this is not read, so that
/// another program's huge clipboard costs no more than this.
pub const MAX_CLIPBOARD_UNITS: usize = 1 << 16;

/// The clipboard, for text, through a window: what an interface's copy and
/// paste use.
///
/// A handle from [`Window::clipboard`](crate::Window::clipboard). It holds
/// the window weakly: once the window is gone, nothing can be read and
/// writing fails with [`WindowError::Closed`]. It belongs to the window's
/// thread.
///
/// On Win32 it is the system's clipboard, in its Unicode text format, opened
/// for each call with the window as its owner. On a headless window it is a
/// clipboard of the window's own, which the test reads and fills through
/// [`Headless`](crate::Headless).
///
/// ```
/// use manguebyte_window::{Handled, PhysicalSize, Window, WindowCx, WindowOptions};
///
/// let (window, headless) = Window::headless(
///     WindowOptions::new(PhysicalSize::new(10, 10)),
///     |_: &WindowCx<'_>, _| Handled::Yes,
/// )
/// .expect("valid options");
/// let clipboard = window.clipboard();
/// clipboard.write_text("-6.0 dB").expect("open");
/// assert_eq!(headless.clipboard_text().as_deref(), Some("-6.0 dB"));
/// headless.set_clipboard_text(Some("440 Hz"));
/// assert_eq!(clipboard.read_text().as_deref(), Some("440 Hz"));
/// drop(window);
/// assert_eq!(clipboard.read_text(), None);
/// ```
#[derive(Clone)]
pub struct Clipboard {
    inner: Weak<Inner>,
}

impl Clipboard {
    pub(crate) fn new(inner: &Rc<Inner>) -> Clipboard {
        Clipboard {
            inner: Rc::downgrade(inner),
        }
    }

    /// The text on the clipboard, at most [`MAX_CLIPBOARD_UNITS`] UTF-16
    /// units of it, with what is not valid UTF-16 replaced by U+FFFD; or
    /// `None` if there is no text, the clipboard cannot be opened now
    /// (another program holds it), or the window is gone.
    pub fn read_text(&self) -> Option<String> {
        let inner = self.inner.upgrade()?;
        inner.read_clipboard().ok().flatten()
    }

    /// Puts `text` on the clipboard, in place of what was there.
    ///
    /// # Errors
    ///
    /// [`WindowError::Closed`] once the window is gone, and
    /// [`WindowError::Platform`] if the system refuses, as when another
    /// program holds the clipboard open.
    pub fn write_text(&self, text: &str) -> Result<(), WindowError> {
        let inner = self.inner.upgrade().ok_or(WindowError::Closed)?;
        inner.write_clipboard(text)
    }
}

impl fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clipboard")
            .field("window", &(self.inner.strong_count() > 0))
            .finish()
    }
}
