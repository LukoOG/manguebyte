//! What knobs and sliders share: how a person changes a parameter with the
//! pointer, the wheel, the keys and assistive technology.

use manguebyte_gfx::Point;

use crate::access::{AccessNode, AccessRange, Action, Actions, Role};
use crate::event::{Button, Event, Handled, Key, KeyEvent};
use crate::param::{ParamBinding, ParamHandle};
use crate::widget::EventCx;

/// Which way the pointer moves to raise the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    /// Up.
    Vertical,
    /// To the right.
    Horizontal,
}

/// A drag in progress.
#[derive(Debug, Clone, Copy)]
struct Drag {
    /// Where the pointer was when the value was `start`.
    origin: Point,
    start: f64,
    /// Whether Shift was held, for fine changes.
    fine: bool,
}

/// A parameter and the drag changing it.
pub(crate) struct Adjust {
    pub(crate) binding: ParamBinding,
    axis: Axis,
    drag: Option<Drag>,
}

impl Adjust {
    pub(crate) fn new(handle: ParamHandle, axis: Axis) -> Self {
        Adjust {
            binding: ParamBinding::new(handle),
            axis,
            drag: None,
        }
    }

    /// One step, as the arrow keys and one notch of the wheel take it: one
    /// of the parameter's steps, or a hundredth of the range.
    fn step(&self, fine: bool) -> f64 {
        match self.binding.handle().step_count() {
            Some(steps) if steps > 0 => 1.0 / f64::from(steps),
            _ if fine => 0.001,
            _ => 0.01,
        }
    }

    /// A large step, as Page Up and Page Down take it: a tenth of the
    /// range, or one step if that is larger.
    fn big_step(&self) -> f64 {
        self.step(false).max(0.1)
    }

    /// Handles `event`. `span` is how far, in logical pixels, the pointer
    /// moves to cross the whole range; `fine_factor` how many times
    /// further it moves while Shift is held.
    ///
    /// Every edit that moves on from the value shown takes it from
    /// [`ParamBinding::current`], read at that moment: host automation may
    /// have moved the parameter since the last frame, and a value sent a
    /// moment ago may not have been echoed yet.
    pub(crate) fn event(
        &mut self,
        cx: &mut EventCx<'_>,
        event: &Event,
        span: f32,
        fine_factor: f32,
    ) -> Handled {
        match *event {
            Event::PointerDown {
                at,
                button: Button::Primary,
                clicks,
                modifiers,
            } => {
                if clicks >= 2 {
                    // A double click puts the default back, as one edit.
                    self.drag = None;
                    self.binding.end();
                    let default = self.binding.handle().default_normalized();
                    self.binding.set_once(cx.context(), default);
                    cx.release_pointer();
                } else {
                    let start = self.binding.current();
                    self.drag = Some(Drag {
                        origin: at,
                        start,
                        fine: modifiers.shift,
                    });
                    self.binding.begin(cx.context());
                    cx.capture_pointer();
                }
                // A control pressed takes the keyboard focus, so the arrow
                // keys go on from where the pointer left it.
                cx.request_focus();
                cx.request_paint();
                Handled::Yes
            }
            Event::PointerMove { at, modifiers } => {
                let Some(drag) = &mut self.drag else {
                    return Handled::No;
                };
                if modifiers.shift != drag.fine {
                    // Changing speed starts again from here, so the value
                    // does not jump.
                    *drag = Drag {
                        origin: at,
                        start: self.binding.shown(),
                        fine: modifiers.shift,
                    };
                }
                let travel = match self.axis {
                    Axis::Vertical => drag.origin.y - at.y,
                    Axis::Horizontal => at.x - drag.origin.x,
                };
                let factor = if drag.fine && fine_factor.is_finite() {
                    fine_factor.max(1.0)
                } else {
                    1.0
                };
                let span = f64::from(span.max(1.0)) * f64::from(factor);
                let value = drag.start + f64::from(travel) / span;
                // A position that is not a number gives a value that is
                // not one, which the gesture ignores.
                self.binding.set(value);
                cx.request_paint();
                Handled::Yes
            }
            Event::PointerUp { .. } | Event::PointerCancel => {
                if self.drag.take().is_none() {
                    return Handled::No;
                }
                self.binding.end();
                cx.release_pointer();
                cx.request_paint();
                Handled::Yes
            }
            Event::Wheel { delta, .. } => {
                let notches = match self.axis {
                    Axis::Vertical => delta.y,
                    Axis::Horizontal => delta.y + delta.x,
                };
                if !(notches.is_finite() && notches != 0.0) {
                    return Handled::No;
                }
                let value = self.binding.current() + self.step(false) * f64::from(notches);
                if self.drag.is_some() {
                    self.binding.set(value);
                } else {
                    self.binding.set_once(cx.context(), value);
                }
                cx.request_paint();
                Handled::Yes
            }
            Event::Key(KeyEvent {
                key,
                modifiers,
                pressed: true,
            }) => {
                if self.drag.is_some() {
                    if key != Key::Escape {
                        return Handled::No;
                    }
                    // Escape during a drag puts back the value it began at.
                    self.drag = None;
                    self.binding.cancel();
                    cx.release_pointer();
                    cx.request_paint();
                    return Handled::Yes;
                }
                let step = self.step(modifiers.shift);
                let value = match key {
                    Key::Up | Key::Right => self.binding.current() + step,
                    Key::Down | Key::Left => self.binding.current() - step,
                    Key::PageUp => self.binding.current() + self.big_step(),
                    Key::PageDown => self.binding.current() - self.big_step(),
                    Key::Home => 0.0,
                    Key::End => 1.0,
                    Key::Delete | Key::Backspace => self.binding.handle().default_normalized(),
                    _ => return Handled::No,
                };
                self.binding.set_once(cx.context(), value);
                cx.request_paint();
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    /// Carries out an action from assistive technology.
    pub(crate) fn act(&mut self, cx: &mut EventCx<'_>, action: Action) -> bool {
        let value = match action {
            Action::Increment => self.binding.current() + self.step(false),
            Action::Decrement => self.binding.current() - self.step(false),
            Action::SetValue(plain) if plain.is_finite() => {
                self.binding.handle().to_normalized(plain)
            }
            Action::SetValue(_) | Action::Focus | Action::Press => return false,
        };
        self.binding.set_once(cx.context(), value);
        cx.request_paint();
        true
    }

    /// The description of a control with this value.
    pub(crate) fn access(&self, role: Role, name: &str) -> AccessNode {
        let handle = self.binding.handle();
        let (min, max) = (handle.plain(0.0), handle.plain(1.0));
        let step = handle
            .step_count()
            .filter(|&steps| steps > 0)
            .map(|steps| (max - min) / f64::from(steps));
        AccessNode::new(role, name)
            .with_value(self.binding.text())
            .with_numeric(self.binding.plain(), AccessRange::new(min, max, step))
            .with_actions(Actions::ADJUST)
    }
}
