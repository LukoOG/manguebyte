//! The widget trait and the contexts its methods are given.

use std::time::Duration;

use manguebyte_gfx::{Rect, Scene, Size};
use manguebyte_plugin::editor::EditorContext;

use crate::access::{AccessNode, Action};
use crate::clipboard::Clipboard;
use crate::event::{Event, Handled};
use crate::theme::Theme;

/// A node of the interface: something that is laid out, drawn, receives
/// input and describes itself.
///
/// The [`Root`](crate::Root) owns every widget in an arena, in tree order;
/// containers do not own their children, so hit testing, focus, damage,
/// the accessibility tree and test queries are generic code that no widget
/// has to take part in. A widget keeps its own state in its struct and is
/// called on the interface's thread only:
///
/// - [`update`](Widget::update) once per frame, to poll the
///   [`Source`](crate::Source)s and parameters it shows and to ask to be
///   drawn again if one changed;
/// - [`measure`](Widget::measure) when layout runs, for its natural size;
/// - [`paint`](Widget::paint) when it asked to be drawn again or moved,
///   to record what it looks like into a [`Scene`];
/// - [`event`](Widget::event) and [`act`](Widget::act) for input and
///   for requests from assistive technology;
/// - [`access`](Widget::access) whenever its description is wanted.
///
/// `access`, `measure` and `paint` have no default: a widget cannot be
/// written without saying what it is, how large it wants to be and how it
/// looks.
///
/// A widget of one's own: a light that shows whether a value is above a
/// threshold, read from a [`Source`](crate::Source) once per frame.
///
/// ```
/// use std::cell::Cell;
/// use std::rc::Rc;
/// use manguebyte_gfx::{Circle, Scene, Size};
/// use manguebyte_ui::access::{AccessNode, Role};
/// use manguebyte_ui::testing::Harness;
/// use manguebyte_ui::{AccessCx, MeasureCx, PaintCx, Source, UpdateCx, Widget};
///
/// struct Light<S> { name: &'static str, level: S, lit: bool }
///
/// impl<S: Source<f32>> Widget for Light<S> {
///     fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
///         AccessNode::new(Role::Image, self.name).with_value(if self.lit { "on" } else { "off" })
///     }
///
///     fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
///         Size::new(16.0, 16.0)
///     }
///
///     fn update(&mut self, cx: &mut UpdateCx<'_>) {
///         let lit = self.level.read() > 0.5;
///         if lit != self.lit {
///             self.lit = lit;
///             cx.request_paint();
///         }
///     }
///
///     fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene) {
///         let palette = &cx.theme().palette;
///         let color = if self.lit { palette.accent } else { palette.track };
///         let rect = cx.rect();
///         scene.fill(Circle::new(rect.center(), rect.width.min(rect.height) * 0.4), color);
///     }
/// }
///
/// let level = Rc::new(Cell::new(0.0_f32));
/// let source = Rc::clone(&level);
/// let light = Light { name: "Clip", level: move || source.get(), lit: false };
/// let mut ui = Harness::new(light, Size::new(16.0, 16.0), 1.0);
/// ui.frame();
/// assert_eq!(ui.get(Role::Image, "Clip").value_text(), Some("off"));
/// level.set(0.9);
/// assert_eq!(ui.frame().damaged, 1);
/// assert_eq!(ui.get(Role::Image, "Clip").value_text(), Some("on"));
/// assert_eq!(ui.frame().damaged, 0, "nothing changed since");
/// ```
///
/// Leaving out the description does not compile:
///
/// ```compile_fail,E0046
/// use manguebyte_gfx::{Scene, Size};
/// use manguebyte_ui::{MeasureCx, PaintCx, Widget};
///
/// struct Silent;
///
/// impl Widget for Silent {
///     fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
///         Size::new(10.0, 10.0)
///     }
///     fn paint(&mut self, _cx: &PaintCx<'_>, _scene: &mut Scene) {}
/// }
/// ```
pub trait Widget: 'static {
    /// How the widget describes itself to assistive technology and to
    /// tests. The root fills in [`focused`](crate::access::AccessState).
    fn access(&self, cx: &AccessCx<'_>) -> AccessNode;

    /// The natural size of the widget's content, in logical pixels.
    /// Containers take their size from their children, so theirs is not
    /// used.
    fn measure(&mut self, cx: &MeasureCx<'_>) -> Size;

    /// Records how the widget looks into `scene`, which is empty, in the
    /// interface's logical coordinates, inside [`PaintCx::rect`]. The root
    /// clips whatever reaches past the widget's place in the layout, so it
    /// never covers a neighbour, and repaints the device pixels the
    /// scene's [`bounds`](Scene::bounds) cover, before and after a change.
    fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene);

    /// Called once per frame before anything is laid out or drawn. Polls
    /// what the widget shows and asks for a repaint if it changed. Nothing
    /// unless overridden.
    fn update(&mut self, cx: &mut UpdateCx<'_>) {
        let _ = cx;
    }

    /// Handles input. Pointer events reach the widget under the pointer
    /// first and then the widgets that contain it, until one returns
    /// [`Handled::Yes`]; while a widget holds the pointer (see
    /// [`EventCx::capture_pointer`]) every move and the release go to it.
    /// Keys, typed text and an input method's compositions go to the
    /// widget that holds the pointer, then to the one with the focus.
    /// Ignores everything unless overridden.
    fn event(&mut self, cx: &mut EventCx<'_>, event: &Event) -> Handled {
        let _ = (cx, event);
        Handled::No
    }

    /// Carries out `action`, which the widget's
    /// [`AccessNode::actions`] allow (the root checks), and returns whether
    /// it did. The root itself handles [`Action::Focus`]. Does nothing
    /// unless overridden.
    fn act(&mut self, cx: &mut EventCx<'_>, action: Action) -> bool {
        let _ = (cx, action);
        false
    }

    /// The widget gained (`true`) or lost (`false`) the keyboard focus:
    /// by a press, Tab, assistive technology, a menu opening over it, or
    /// the window losing the keyboard. A text input takes what was typed
    /// when it loses it. Asking for the focus from here does nothing.
    /// Nothing unless overridden.
    fn focus_changed(&mut self, cx: &mut EventCx<'_>, focused: bool) {
        let _ = (cx, focused);
    }

    /// Where the text caret is, in logical pixels, while the widget is
    /// being typed into: the window layer tells the system, so that an
    /// input method opens its window beside it and a screen magnifier
    /// follows it. `None`, the default, when the widget takes no text now.
    fn caret(&self, cx: &AccessCx<'_>) -> Option<Rect> {
        let _ = cx;
        None
    }
}

/// What a widget asked for during a call; the root acts on it afterwards.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Requests {
    pub(crate) paint: bool,
    pub(crate) layout: bool,
    pub(crate) frame: bool,
    /// `Some(true)` to take the pointer, `Some(false)` to let it go.
    pub(crate) capture: Option<bool>,
    pub(crate) focus: bool,
}

/// What [`Widget::update`] is given.
pub struct UpdateCx<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) rect: Rect,
    pub(crate) scale: f32,
    pub(crate) now: Duration,
    pub(crate) elapsed: Duration,
    pub(crate) requests: &'a mut Requests,
}

impl UpdateCx<'_> {
    /// The theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// The rectangle the widget draws in, in logical pixels (see
    /// [`PaintCx::rect`]).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The time of this frame, on the clock the root is driven by.
    pub fn now(&self) -> Duration {
        self.now
    }

    /// How long since the previous frame; zero on the first.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Asks for [`Widget::paint`] to be called this frame.
    pub fn request_paint(&mut self) {
        self.requests.paint = true;
    }

    /// Asks for [`Widget::measure`] and layout to run this frame, because
    /// the widget's natural size may have changed. It is painted again too.
    pub fn request_layout(&mut self) {
        self.requests.layout = true;
        self.requests.paint = true;
    }

    /// Says the widget is moving on its own (an animation, a falling
    /// meter), so the window should keep delivering frames.
    pub fn request_frame(&mut self) {
        self.requests.frame = true;
    }
}

/// What [`Widget::measure`] is given.
pub struct MeasureCx<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) scale: f32,
}

impl MeasureCx<'_> {
    /// The theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }
}

/// What [`Widget::paint`] is given.
pub struct PaintCx<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) rect: Rect,
    pub(crate) scale: f32,
    pub(crate) focused: bool,
    pub(crate) now: Duration,
}

impl PaintCx<'_> {
    /// The theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// The rectangle the widget draws in, in logical pixels, with its
    /// edges on device pixels: its place in the layout, less the
    /// [`padding`](crate::IntoView::padding) a widget with no children was
    /// given.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Whether the widget has the keyboard focus, and should show it.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// The time of this frame.
    pub fn now(&self) -> Duration {
        self.now
    }
}

/// What [`Widget::access`] is given.
pub struct AccessCx<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) rect: Rect,
    pub(crate) focused: bool,
}

impl AccessCx<'_> {
    /// The theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// The rectangle the widget draws in, in logical pixels (see
    /// [`PaintCx::rect`]).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Whether the widget has the keyboard focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }
}

/// What [`Widget::event`] and [`Widget::act`] are given.
pub struct EventCx<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) rect: Rect,
    pub(crate) scale: f32,
    pub(crate) focused: bool,
    pub(crate) captured: bool,
    pub(crate) now: Duration,
    pub(crate) context: &'a EditorContext,
    pub(crate) clipboard: &'a dyn Clipboard,
    pub(crate) requests: &'a mut Requests,
}

impl EventCx<'_> {
    /// The theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// The rectangle the widget draws in, in logical pixels (see
    /// [`PaintCx::rect`]).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The time of the last frame.
    pub fn now(&self) -> Duration {
        self.now
    }

    /// Whether the widget has the keyboard focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Whether the widget holds the pointer.
    pub fn has_pointer(&self) -> bool {
        self.captured
    }

    /// How the editor reaches its host: what
    /// [`Gesture`](manguebyte_plugin::editor::Gesture)s are made on.
    pub fn context(&self) -> &EditorContext {
        self.context
    }

    /// The clipboard: the system's when the window layer gave the root
    /// one, the root's own otherwise (see [`crate::clipboard`]).
    pub fn clipboard(&self) -> &dyn Clipboard {
        self.clipboard
    }

    /// Asks for [`Widget::paint`] to be called on the next frame.
    pub fn request_paint(&mut self) {
        self.requests.paint = true;
    }

    /// Asks for layout to run on the next frame.
    pub fn request_layout(&mut self) {
        self.requests.layout = true;
        self.requests.paint = true;
    }

    /// Takes the pointer: every move and the release go to this widget
    /// until it is released, or until it lets go with
    /// [`release_pointer`](Self::release_pointer).
    pub fn capture_pointer(&mut self) {
        self.requests.capture = Some(true);
    }

    /// Lets go of the pointer.
    pub fn release_pointer(&mut self) {
        self.requests.capture = Some(false);
    }

    /// Takes the keyboard focus, if the widget accepts
    /// [`Action::Focus`]. A press takes the focus away from the widget that
    /// has it unless it lands on that widget; a widget that should have
    /// the focus once pressed asks for it here.
    pub fn request_focus(&mut self) {
        self.requests.focus = true;
    }
}
