//! Where copied text goes and where pasted text comes from.
//!
//! The interface has no window system of its own, so a [`Root`](crate::Root)
//! is given a [`Clipboard`] by whoever drives it: `manguebyte-gui` gives it
//! the system's, through its window, and a root that is given none keeps a
//! [`MemoryClipboard`] of its own, so that copy and paste still work within
//! the interface. Widgets reach it through
//! [`EventCx::clipboard`](crate::EventCx::clipboard).
//!
//! ```
//! use std::rc::Rc;
//! use manguebyte_gfx::Size;
//! use manguebyte_ui::testing::Harness;
//! use manguebyte_ui::{Clipboard, Key, MemoryClipboard, Modifiers, Role, State, text_field};
//!
//! let name = State::new(String::from("Warm pad"));
//! let mut ui = Harness::new(text_field("Preset", name.clone()), Size::new(160.0, 30.0), 1.0);
//! ui.frame();
//! assert!(ui.tab());
//! let control = Modifiers { control: true, ..Modifiers::NONE };
//! // The focus selected everything; Control+C copies it.
//! assert!(ui.key_with(Key::Character('c'), control));
//! assert_eq!(ui.clipboard().text().as_deref(), Some("Warm pad"));
//! ```

use std::cell::RefCell;
use std::fmt;

/// A clipboard for text.
///
/// Implementations are called on the interface's thread, from inside a
/// widget's event handling. They should not block for long: the system's
/// clipboard may belong to another program for a moment, and then reading
/// or writing fails rather than waits.
pub trait Clipboard {
    /// The text on the clipboard, or `None` if there is none or it cannot
    /// be read now.
    fn read_text(&self) -> Option<String>;

    /// Puts `text` on the clipboard, in place of what was there. Returns
    /// whether it did.
    fn write_text(&self, text: &str) -> bool;
}

/// A clipboard kept in memory: what a [`Root`](crate::Root) uses when it is
/// given no other, and what the test harness gives it, so that a test can
/// read what was copied and choose what is pasted.
#[derive(Default)]
pub struct MemoryClipboard {
    text: RefCell<Option<String>>,
}

impl MemoryClipboard {
    /// An empty clipboard.
    pub fn new() -> MemoryClipboard {
        MemoryClipboard::default()
    }

    /// What was last written, if anything.
    pub fn text(&self) -> Option<String> {
        self.text.borrow().clone()
    }

    /// Puts `text` on the clipboard, or empties it.
    pub fn set_text(&self, text: Option<&str>) {
        *self.text.borrow_mut() = text.map(str::to_owned);
    }
}

impl Clipboard for MemoryClipboard {
    fn read_text(&self) -> Option<String> {
        self.text()
    }

    fn write_text(&self, text: &str) -> bool {
        self.set_text(Some(text));
        true
    }
}

impl fmt::Debug for MemoryClipboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryClipboard")
            .field("text", &*self.text.borrow())
            .finish()
    }
}
