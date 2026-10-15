//! Input, in the form a window layer delivers it.
//!
//! Positions are logical pixels from the top-left corner of the interface:
//! a window layer divides the device pixels it receives by the scale.
//! Keys use the editor contract's types, so a key a VST 3 host delivers
//! through the plugin interface and one the window receives itself are the
//! same event.
//!
//! ```
//! use std::sync::Arc;
//! use manguebyte_gfx::{Point, Size};
//! use manguebyte_params::{FloatParam, FloatRange, Param, param_set};
//! use manguebyte_ui::testing::Harness;
//! use manguebyte_ui::{Event, Key, KeyEvent, Modifiers, ParamHandle, Role, knob};
//!
//! struct Params { pan: FloatParam }
//! param_set!(Params { pan });
//! let params = Arc::new(Params { pan: FloatParam::new("pan", "Pan", FloatRange::linear(-1.0, 1.0), 0.0) });
//! let view = knob(ParamHandle::new(Arc::clone(&params), |p| &p.pan));
//! let mut ui = Harness::with_params(view, Size::new(80.0, 100.0), 1.0, params.clone());
//! ui.frame();
//! let pan = ui.get(Role::Slider, "Pan");
//!
//! // One notch of the wheel away from the person raises the value a step.
//! let wheel = Event::Wheel { at: pan.center(), delta: Point::new(0.0, 1.0), modifiers: Modifiers::NONE };
//! assert_eq!(wheel.position(), Some(pan.center()));
//! assert!(ui.send(wheel));
//! assert!((params.pan.value() - 0.02).abs() < 1e-6);
//!
//! // Nothing has the keyboard focus, so a key is left to the host.
//! let end = Event::Key(KeyEvent { key: Key::End, modifiers: Modifiers::NONE, pressed: true });
//! assert!(!ui.send(end));
//! assert!(ui.tab());
//! assert!(ui.send(end));
//! assert_eq!(params.pan.value(), 1.0);
//! ```

use manguebyte_gfx::Point;

pub use manguebyte_plugin::editor::{Key, KeyEvent, Modifiers};

/// A pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Button {
    /// The main button: the left one, or a tap.
    Primary,
    /// The other button: usually the right one.
    Secondary,
    /// The middle button or wheel press.
    Middle,
}

/// Something a person did, as the interface receives it.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A button went down at `at`. `clicks` counts quick presses in the
    /// same place as the system counts them: 1 for a click, 2 for a double
    /// click.
    PointerDown {
        /// Where, in logical pixels.
        at: Point,
        /// Which button.
        button: Button,
        /// 1 for a click, 2 for a double click, and so on.
        clicks: u32,
        /// The modifier keys held.
        modifiers: Modifiers,
    },
    /// The pointer moved to `at`.
    PointerMove {
        /// Where, in logical pixels.
        at: Point,
        /// The modifier keys held.
        modifiers: Modifiers,
    },
    /// A button came up at `at`.
    PointerUp {
        /// Where, in logical pixels.
        at: Point,
        /// Which button.
        button: Button,
        /// The modifier keys held.
        modifiers: Modifiers,
    },
    /// The window lost the pointer in the middle of a press, as when the
    /// system takes it away: a drag ends where it is.
    PointerCancel,
    /// The wheel turned, or a touchpad scrolled, with the pointer at `at`.
    Wheel {
        /// Where the pointer is, in logical pixels.
        at: Point,
        /// How far, in notches (lines): `y` above 0 is away from the
        /// person, which raises a value; `x` above 0 is to the right.
        delta: Point,
        /// The modifier keys held.
        modifiers: Modifiers,
    },
    /// A key went down or came up.
    Key(KeyEvent),
    /// The window gained (`true`) or lost (`false`) the keyboard.
    Focus(bool),
}

impl Event {
    /// Where the pointer is, for pointer and wheel events.
    pub fn position(&self) -> Option<Point> {
        match *self {
            Event::PointerDown { at, .. }
            | Event::PointerMove { at, .. }
            | Event::PointerUp { at, .. }
            | Event::Wheel { at, .. } => Some(at),
            Event::PointerCancel | Event::Key(_) | Event::Focus(_) => None,
        }
    }
}

/// Whether a widget used an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Handled {
    /// It did; nothing else sees the event.
    Yes,
    /// It did not; the event goes to the widget that contains it.
    No,
}

impl Handled {
    /// Whether the event was used.
    pub fn is_yes(self) -> bool {
        self == Handled::Yes
    }
}
