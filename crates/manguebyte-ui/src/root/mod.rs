//! The root of an interface: the tree, its size and scale, the frame and
//! input.

use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use manguebyte_gfx::{Color, IRect, Pixmap, Point, Rect, Renderer, Scene, Size};
use manguebyte_plugin::editor::{EditorContext, MAX_SCALE, MIN_SCALE};

use crate::access::{AccessEntry, Action};
use crate::clipboard::{Clipboard, MemoryClipboard};
use crate::damage::{MAX_DAMAGE, tidy};
use crate::event::{Event, Handled, Key};
use crate::layout;
use crate::theme::Theme;
use crate::tree::{NodeId, Tree};
use crate::view::IntoView;
use crate::widget::{AccessCx, EventCx, MeasureCx, PaintCx, Requests, UpdateCx, Widget};

/// What one call to [`Root::frame`] did.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// The device rectangles of the back buffer that were drawn again, at
    /// most [`MAX_DAMAGE`]; a window copies these to the screen. Empty
    /// when nothing changed.
    pub damage: &'a [IRect],
    /// The back buffer, premultiplied RGBA, as large as the interface in
    /// device pixels.
    pub pixels: &'a Pixmap,
    /// Whether something is moving on its own and wants another frame
    /// soon, such as a meter still falling.
    pub animating: bool,
    /// How many widgets recorded a scene that differs from the one they
    /// had (every widget, when everything is drawn again).
    pub painted: usize,
    /// Whether layout ran.
    pub laid_out: bool,
}

/// An interface: the widget tree, its size and scale, and the back buffer
/// it is drawn into.
///
/// A window layer (or a test) drives it with two calls: [`event`]
/// for each piece of input, and [`frame`] on a clock, about 60 times a
/// second while the interface is visible. A frame
///
/// 1. calls every widget's [`update`](Widget::update), which polls the
///    sources and parameters it shows;
/// 2. lays the tree out again if a widget's size may have changed, or the
///    interface's size or scale did;
/// 3. records a new scene for each widget that asked, and compares it with
///    the one it had: the damage is the device bounds of each scene that
///    differs, before and after, merged into a few rectangles;
/// 4. draws the damaged rectangles of the back buffer again, through
///    `manguebyte-gfx`, and reports them.
///
/// When nothing changed a frame costs one poll per widget, draws nothing
/// and reports no damage. An expose from the system never needs a frame:
/// the back buffer is already up to date.
///
/// A widget's scene that reaches past the widget's rectangle, as a
/// control given less room than its natural size may draw, is clipped to
/// the rectangle, so the damage of any widget stays inside it.
///
/// Everything happens on the thread that made the root, which is the
/// host's main thread inside a plugin. The root holds the editor's
/// [`EditorContext`], through which widgets send their gestures; dropping
/// the root drops the widgets, which ends any gesture still open.
///
/// ```
/// use std::rc::Rc;
/// use std::sync::Arc;
/// use std::time::Duration;
/// use manguebyte_gfx::{IRect, Point, Size};
/// use manguebyte_params::{FloatParam, FloatRange, Param, param_set};
/// use manguebyte_plugin::editor::EditorContext;
/// use manguebyte_plugin::editor::testing::RecordingEditorHost;
/// use manguebyte_ui::{Button, Event, Handled, Modifiers, ParamHandle, Root, knob};
///
/// struct Params { mix: FloatParam }
/// param_set!(Params { mix });
/// let params = Arc::new(Params {
///     mix: FloatParam::new("mix", "Mix", FloatRange::linear(0.0, 100.0), 50.0),
/// });
/// // A host that stores what it is sent, as a standalone program does.
/// let host = Rc::new(RecordingEditorHost::new().with_params(params.clone()));
/// let (context, link) = EditorContext::attach(host);
/// let view = knob(ParamHandle::new(Arc::clone(&params), |p| &p.mix));
/// let mut root = Root::new(view, Size::new(80.0, 100.0), 1.5, context);
///
/// // The first frame lays out and draws everything.
/// assert_eq!(root.frame(Duration::ZERO).damage, [IRect::new(0, 0, 120, 150)]);
///
/// // A window layer turns what the system delivers into events.
/// let at = Point::new(40.0, 50.0);
/// let press = Event::PointerDown { at, button: Button::Primary, clicks: 1, modifiers: Modifiers::NONE };
/// assert_eq!(root.event(press), Handled::Yes);
/// let moved = Event::PointerMove { at: Point::new(40.0, 30.0), modifiers: Modifiers::NONE };
/// assert_eq!(root.event(moved), Handled::Yes);
/// let release = Event::PointerUp { at, button: Button::Primary, modifiers: Modifiers::NONE };
/// assert_eq!(root.event(release), Handled::Yes);
/// // Twenty pixels up is a tenth of the theme's 200-pixel drag range.
/// assert_eq!(params.mix.value(), 60.0);
///
/// // The next frame draws only the knob again.
/// let frame = root.frame(Duration::from_millis(16));
/// assert!(!frame.damage.is_empty() && frame.painted == 1);
///
/// drop(root);
/// drop(link);
/// ```
///
/// A root stays on the thread that made it:
///
/// ```compile_fail,E0277
/// fn close_on_another_thread(root: manguebyte_ui::Root) {
///     std::thread::spawn(move || drop(root));
/// }
/// ```
///
/// [`event`]: Root::event
/// [`frame`]: Root::frame
pub struct Root {
    tree: Tree,
    theme: Theme,
    size: Size,
    scale: f32,
    context: EditorContext,
    renderer: Renderer,
    pixels: Pixmap,
    composite: Scene,
    /// Where a widget's scene that reaches past its rectangle is clipped.
    cut: Scene,
    damage: Vec<IRect>,
    focus: Option<NodeId>,
    capture: Option<NodeId>,
    clipboard: Rc<dyn Clipboard>,
    /// Keys whose press a widget used and whose release has not come.
    keys_used: Vec<Key>,
    /// Set while a widget hears that it gained or lost the focus: what it
    /// asks for the focus then is not done, so that focus cannot bounce.
    notifying: bool,
    needs_layout: bool,
    /// Every widget is drawn again, over the whole buffer, next frame.
    full: bool,
    now: Duration,
    last_frame: Option<Duration>,
}

/// The most keys remembered as held after the interface used their press:
/// more than hands hold at once. A key held beyond these is forgotten,
/// and its release goes on as any other.
const MAX_KEYS_HELD: usize = 16;

/// A scale an interface may be drawn at.
fn clean_scale(scale: f32) -> Option<f32> {
    let (min, max) = (MIN_SCALE as f32, MAX_SCALE as f32);
    (scale.is_finite() && (min..=max).contains(&scale)).then_some(scale)
}

/// A logical size an interface may have: finite and not negative.
fn clean_size(size: Size) -> Size {
    let clean = |v: f32| if v.is_finite() && v > 0.0 { v } else { 0.0 };
    Size::new(clean(size.width), clean(size.height))
}

/// The most device pixels a side of the back buffer has. An interface
/// larger than this is drawn only up to it. It is the largest side for
/// which `manguebyte-gfx` promises that a partial render equals a whole
/// one, and still means a buffer of up to 256 MiB.
pub const MAX_BUFFER_SIDE: u32 = 8191;

/// The back buffer for `size` at `scale`: whole device pixels, at least
/// one each way and at most [`MAX_BUFFER_SIDE`].
fn buffer(size: Size, scale: f32) -> Pixmap {
    let side = |v: f32| -> u32 {
        let pixels = (f64::from(v) * f64::from(scale)).round();
        (pixels as u32).clamp(1, MAX_BUFFER_SIDE)
    };
    let (width, height) = (side(size.width), side(size.height));
    Pixmap::new(width, height)
        .or_else(|| Pixmap::new(1, 1))
        .unwrap_or_else(|| unreachable!("a pixmap of one pixel can always be made"))
}

impl Root {
    /// The interface `view` describes, `size` logical pixels large, drawn
    /// at `scale` device pixels per logical pixel, in the default
    /// [`Theme`], sending its gestures through `context`.
    ///
    /// A scale outside the editor contract's
    /// [`MIN_SCALE`]`..=`[`MAX_SCALE`], or not a number, counts as 1; a
    /// size that is negative or not finite counts as 0. The back buffer
    /// has at least one pixel each way and at most [`MAX_BUFFER_SIDE`].
    /// Nothing is drawn until the first [`frame`](Root::frame).
    pub fn new(view: impl IntoView, size: Size, scale: f32, context: EditorContext) -> Root {
        let size = clean_size(size);
        let scale = clean_scale(scale).unwrap_or(1.0);
        Root {
            tree: Tree::build(view.into_view()),
            theme: Theme::default(),
            size,
            scale,
            context,
            renderer: Renderer::new(),
            pixels: buffer(size, scale),
            composite: Scene::new(),
            cut: Scene::new(),
            damage: Vec::new(),
            focus: None,
            capture: None,
            clipboard: Rc::new(MemoryClipboard::new()),
            keys_used: Vec::with_capacity(MAX_KEYS_HELD),
            notifying: false,
            needs_layout: true,
            full: true,
            now: Duration::ZERO,
            last_frame: None,
        }
    }

    /// The same root drawn in `theme`.
    #[must_use]
    pub fn with_theme(mut self, theme: Theme) -> Root {
        self.set_theme(theme);
        self
    }

    /// Draws the interface in `theme` from the next frame on: everything
    /// is measured, laid out and drawn again.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.invalidate_all();
    }

    /// The theme.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The size, in logical pixels.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// How the editor reaches its host.
    pub fn context(&self) -> &EditorContext {
        &self.context
    }

    /// The same root, copying to and pasting from `clipboard`.
    #[must_use]
    pub fn with_clipboard(mut self, clipboard: Rc<dyn Clipboard>) -> Root {
        self.set_clipboard(clipboard);
        self
    }

    /// Copies to and pastes from `clipboard` from now on, in place of the
    /// [`MemoryClipboard`] a root starts with.
    pub fn set_clipboard(&mut self, clipboard: Rc<dyn Clipboard>) {
        self.clipboard = clipboard;
    }

    /// The clipboard widgets copy to and paste from.
    pub fn clipboard(&self) -> &Rc<dyn Clipboard> {
        &self.clipboard
    }

    /// Whether the widget with the keyboard focus takes typed text now: a
    /// text input, or a knob whose value is being typed. A window layer
    /// then asks the system for every key, Tab, Enter and Escape included,
    /// and keeps the keys that type from the host's shortcuts.
    pub fn wants_text(&self) -> bool {
        self.focus
            .and_then(|id| self.access(id))
            .is_some_and(|entry| entry.node.state.editable)
    }

    /// Where the text caret of the widget with the focus is, in logical
    /// pixels, while text is being typed into it (see [`Widget::caret`]).
    pub fn caret(&self) -> Option<Rect> {
        let id = self.focus?;
        let node = self.tree.get(id)?;
        let cx = AccessCx {
            theme: &self.theme,
            rect: node.content,
            focused: true,
        };
        node.widget.caret(&cx)
    }

    /// Changes the size and scale. The back buffer is made again and
    /// everything is laid out and drawn on the next frame. A scale that
    /// cannot be used keeps the one in use; a size that cannot counts as
    /// 0.
    pub fn resize(&mut self, size: Size, scale: f32) {
        let size = clean_size(size);
        let scale = clean_scale(scale).unwrap_or(self.scale);
        if size == self.size && scale == self.scale {
            return;
        }
        self.size = size;
        self.scale = scale;
        self.pixels = buffer(size, scale);
        self.invalidate_all();
    }

    fn invalidate_all(&mut self) {
        for node in &mut self.tree.nodes {
            node.measure = true;
            node.paint = true;
            node.painted = None;
        }
        self.needs_layout = true;
        self.full = true;
    }

    /// The back buffer, as the last frame left it.
    pub fn pixels(&self) -> &Pixmap {
        &self.pixels
    }

    /// The scene the back buffer was last drawn from: the background, then
    /// every widget's scene in tree order. Its [`dump`](Scene::dump) is an
    /// exact description of what is on screen.
    pub fn scene(&self) -> &Scene {
        &self.composite
    }

    /// The scene widget `id` last recorded.
    pub fn widget_scene(&self, id: NodeId) -> Option<&Scene> {
        self.tree.get(id).map(|node| &node.scene)
    }

    /// Where widget `id` is, in logical pixels and in device pixels.
    pub fn bounds(&self, id: NodeId) -> Option<(Rect, IRect)> {
        self.tree.get(id).map(|node| (node.rect, node.device))
    }

    /// The widget with the keyboard focus, if any.
    pub fn focus(&self) -> Option<NodeId> {
        self.focus
    }

    /// The widget holding the pointer during a drag, if any.
    pub fn pointer_holder(&self) -> Option<NodeId> {
        self.capture
    }

    /// The number of widgets in the tree.
    pub fn len(&self) -> usize {
        self.tree.nodes.len()
    }

    /// Whether the tree is empty; it never is, as it has at least the
    /// widget of the view it was made from.
    pub fn is_empty(&self) -> bool {
        self.tree.nodes.is_empty()
    }

    /// How widget `id` describes itself, with the focus filled in and the
    /// name given with [`IntoView::named`] in place of its own.
    pub fn access(&self, id: NodeId) -> Option<AccessEntry> {
        let node = self.tree.get(id)?;
        let focused = self.focus == Some(id);
        let cx = AccessCx {
            theme: &self.theme,
            rect: node.content,
            focused,
        };
        let mut access = node.widget.access(&cx);
        access.state.focused = focused;
        if let Some(name) = &node.name {
            access.name.clone_from(name);
        }
        Some(AccessEntry {
            id,
            parent: node.parent,
            node: access,
            bounds: node.rect,
            device: node.device,
        })
    }

    /// Every widget's description, in tree order.
    pub fn access_tree(&self) -> Vec<AccessEntry> {
        (0..self.tree.nodes.len())
            .filter_map(|index| self.access(NodeId::from_index(index)))
            .collect()
    }

    /// The topmost widget at `at`, if any.
    pub fn hit(&self, at: Point) -> Option<NodeId> {
        // Later widgets are drawn over earlier ones, and children over
        // their parents.
        (0..self.tree.nodes.len())
            .rev()
            .find(|&index| self.tree.nodes[index].rect.contains(at))
            .map(NodeId::from_index)
    }

    /// Carries out `action` on widget `id`, as assistive technology asks:
    /// only if the widget lists it among its actions. Returns whether it
    /// was done.
    pub fn act(&mut self, id: NodeId, action: Action) -> bool {
        let Some(entry) = self.access(id) else {
            return false;
        };
        if !entry.node.actions.allows(action) {
            return false;
        }
        if action == Action::Focus {
            self.set_focus(Some(id));
            return true;
        }
        self.deliver(id, |widget, cx| widget.act(cx, action))
    }

    /// Moves the keyboard focus to `id`, or takes it away. Only a widget
    /// that accepts [`Action::Focus`] can have it; returns whether the
    /// focus is where it was asked to be.
    pub fn set_focus_to(&mut self, id: Option<NodeId>) -> bool {
        match id {
            Some(id) if !self.focusable(id) => false,
            _ => {
                self.set_focus(id);
                true
            }
        }
    }

    fn focusable(&self, id: NodeId) -> bool {
        self.access(id)
            .is_some_and(|entry| entry.node.actions.focus)
    }

    fn set_focus(&mut self, id: Option<NodeId>) {
        if self.focus == id {
            return;
        }
        for changed in [self.focus, id].into_iter().flatten() {
            if let Some(node) = self.tree.nodes.get_mut(changed.index()) {
                node.paint = true;
            }
        }
        let old = std::mem::replace(&mut self.focus, id);
        // Each hears of it once the focus is where it now is, the one that
        // lost it first.
        let notifying = std::mem::replace(&mut self.notifying, true);
        for (changed, focused) in [(old, false), (id, true)] {
            if let Some(changed) = changed.filter(|at| at.index() < self.tree.nodes.len()) {
                self.deliver(changed, |widget, cx| widget.focus_changed(cx, focused));
            }
        }
        self.notifying = notifying;
    }

    /// Moves the focus to the next widget that takes it, in tree order, or
    /// the previous one; round to the first or last at the ends. Returns
    /// false, and changes nothing, if no widget takes the focus.
    fn move_focus(&mut self, forward: bool) -> bool {
        let focusable: Vec<NodeId> = (0..self.tree.nodes.len())
            .map(NodeId::from_index)
            .filter(|&id| self.focusable(id))
            .collect();
        if focusable.is_empty() {
            return false;
        }
        let at = self
            .focus
            .and_then(|focus| focusable.iter().position(|&id| id == focus));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => focusable.len() - 1,
            (Some(at), true) => (at + 1) % focusable.len(),
            (Some(at), false) => (at + focusable.len() - 1) % focusable.len(),
        };
        self.set_focus(Some(focusable[next]));
        true
    }

    /// Calls `call` on widget `id` with an event context, and acts on what
    /// it asked for.
    fn deliver<R>(
        &mut self,
        id: NodeId,
        call: impl FnOnce(&mut dyn Widget, &mut EventCx<'_>) -> R,
    ) -> R {
        let mut requests = Requests::default();
        let node = &mut self.tree.nodes[id.index()];
        let mut cx = EventCx {
            theme: &self.theme,
            rect: node.content,
            scale: self.scale,
            focused: self.focus == Some(id),
            captured: self.capture == Some(id),
            now: self.now,
            context: &self.context,
            clipboard: &*self.clipboard,
            requests: &mut requests,
        };
        let result = call(&mut *node.widget, &mut cx);
        if requests.paint {
            node.paint = true;
        }
        if requests.layout {
            node.measure = true;
            self.needs_layout = true;
        }
        match requests.capture {
            Some(true) => self.capture = Some(id),
            Some(false) if self.capture == Some(id) => self.capture = None,
            _ => {}
        }
        if requests.focus && !self.notifying && self.focusable(id) {
            self.set_focus(Some(id));
        }
        result
    }

    /// `id` and the widgets that contain it, innermost first.
    fn ancestry(&self, id: NodeId) -> Vec<NodeId> {
        let mut path = vec![id];
        let mut at = id;
        while let Some(parent) = self.tree.get(at).and_then(|node| node.parent) {
            path.push(parent);
            at = parent;
        }
        path
    }

    /// Offers `event` to the widget holding the pointer, then to the one
    /// with the focus, until one uses it.
    fn offer_to_keyboard(&mut self, event: &Event) -> Handled {
        let mut tried = None;
        for target in [self.capture, self.focus].into_iter().flatten() {
            if tried == Some(target) {
                continue;
            }
            tried = Some(target);
            if self.deliver(target, |widget, cx| widget.event(cx, event)) == Handled::Yes {
                return Handled::Yes;
            }
        }
        Handled::No
    }

    /// Offers `event` to `id` and then to the widgets containing it, until
    /// one uses it.
    fn bubble(&mut self, id: NodeId, event: &Event) -> Handled {
        for target in self.ancestry(id) {
            if self.deliver(target, |widget, cx| widget.event(cx, event)) == Handled::Yes {
                return Handled::Yes;
            }
        }
        Handled::No
    }

    /// Passes a piece of input to the widgets, and returns whether one
    /// used it.
    ///
    /// - A press goes to the topmost widget under the pointer, then to
    ///   those containing it. It takes the keyboard focus away from the
    ///   widget that has it, unless it lands on that widget or inside it;
    ///   a widget that should take the focus when pressed asks for it
    ///   ([`EventCx::request_focus`]), as knobs and sliders do and
    ///   switches do not, so that a click on a switch leaves Space to the
    ///   host. A widget that takes the pointer then receives every move
    ///   and the release, wherever they happen; nothing else receives
    ///   moves. A press while a widget holds the pointer sends that widget
    ///   [`Event::PointerCancel`] first.
    /// - The wheel goes to the widget under the pointer, and on outwards.
    /// - Keys go to the widget holding the pointer, then to the one with
    ///   the focus. Tab and Shift+Tab that no widget used move the focus
    ///   through the widgets that take it, in tree order, and are used
    ///   only if there is such a widget; otherwise they are left to the
    ///   host. The release of a key whose press was used is used too,
    ///   whatever the widget does with it, so the host never sees half a
    ///   keystroke.
    /// - Typed text and compositions go where keys go.
    /// - Losing the window's focus ends a drag where it is
    ///   ([`Event::PointerCancel`]) and takes the keyboard focus away.
    ///
    /// Positions that are not numbers hit nothing. Nothing is drawn until
    /// the next frame.
    pub fn event(&mut self, event: Event) -> Handled {
        match event {
            Event::PointerDown { at, .. } => {
                // A press while a widget still holds the pointer (a second
                // button, or a release the system never sent) first ends
                // that hold where it is.
                if self.capture.is_some() {
                    self.event(Event::PointerCancel);
                }
                let Some(target) = self.hit(at) else {
                    self.set_focus(None);
                    return Handled::No;
                };
                let inside_focus = self
                    .focus
                    .is_some_and(|focus| self.ancestry(target).contains(&focus));
                if !inside_focus {
                    self.set_focus(None);
                }
                self.bubble(target, &event)
            }
            Event::PointerMove { .. } => match self.capture {
                Some(holder) => self.deliver(holder, |widget, cx| widget.event(cx, &event)),
                None => Handled::No,
            },
            Event::PointerUp { .. } | Event::PointerCancel => match self.capture.take() {
                Some(holder) => {
                    // Delivered as the holder, then let go whatever it did.
                    self.capture = Some(holder);
                    let handled = self.deliver(holder, |widget, cx| widget.event(cx, &event));
                    self.capture = None;
                    handled
                }
                None => Handled::No,
            },
            Event::Wheel { at, .. } => match self.hit(at) {
                Some(target) => self.bubble(target, &event),
                None => Handled::No,
            },
            Event::Key(key) => {
                // A key whose press was used has its release used too,
                // whatever became of the widget that used it, so that the
                // host never sees half a keystroke.
                let held = self.keys_used.iter().position(|&used| used == key.key);
                if !key.pressed {
                    if let Some(at) = held {
                        self.keys_used.swap_remove(at);
                        self.offer_to_keyboard(&event);
                        return Handled::Yes;
                    }
                }
                let mut handled = self.offer_to_keyboard(&event);
                if handled == Handled::No
                    && key.pressed
                    && key.key == Key::Tab
                    && self.move_focus(!key.modifiers.shift)
                {
                    handled = Handled::Yes;
                }
                if handled == Handled::Yes && key.pressed && held.is_none() {
                    if self.keys_used.len() >= MAX_KEYS_HELD {
                        self.keys_used.remove(0);
                    }
                    self.keys_used.push(key.key);
                }
                handled
            }
            Event::Text(_) | Event::Composition(_) => self.offer_to_keyboard(&event),
            Event::Focus(gained) => {
                if !gained {
                    if self.capture.is_some() {
                        self.event(Event::PointerCancel);
                    }
                    self.set_focus(None);
                    // Their releases go wherever the keyboard went.
                    self.keys_used.clear();
                }
                Handled::Yes
            }
        }
    }

    /// Runs one frame at time `now` on the clock that drives the
    /// interface: polls, lays out, records and draws what changed (see
    /// [`Root`]). `now` should not go backwards; if it does, no time is
    /// taken to have passed.
    pub fn frame(&mut self, now: Duration) -> Frame<'_> {
        let elapsed = self
            .last_frame
            .map_or(Duration::ZERO, |last| now.saturating_sub(last));
        self.last_frame = Some(now);
        self.now = now;

        let mut animating = false;
        for node in &mut self.tree.nodes {
            let mut requests = Requests::default();
            let mut cx = UpdateCx {
                theme: &self.theme,
                rect: node.content,
                scale: self.scale,
                now,
                elapsed,
                requests: &mut requests,
            };
            node.widget.update(&mut cx);
            if requests.paint {
                node.paint = true;
            }
            if requests.layout {
                node.measure = true;
                self.needs_layout = true;
            }
            animating |= requests.frame;
        }

        let laid_out = self.needs_layout;
        if self.needs_layout {
            self.layout();
        }
        let painted = self.paint();
        if !self.damage.is_empty() {
            self.render();
        }
        Frame {
            damage: &self.damage,
            pixels: &self.pixels,
            animating,
            painted,
            laid_out,
        }
    }

    fn layout(&mut self) {
        self.needs_layout = false;
        let cx = MeasureCx {
            theme: &self.theme,
            scale: self.scale,
        };
        for (node, item) in self.tree.nodes.iter_mut().zip(&mut self.tree.layout) {
            if node.measure {
                node.measure = false;
                item.content = node.widget.measure(&cx);
            }
        }
        let area = Rect::new(0.0, 0.0, self.size.width, self.size.height);
        let placed = layout::solve(&self.tree.layout, 0, area, self.scale);
        let nodes = self.tree.nodes.iter_mut().zip(&self.tree.layout);
        for ((node, item), placement) in nodes.zip(placed) {
            // A widget with no children draws inside its padding; a
            // container's padding is around its children, and it draws
            // its background across the whole of its rectangle.
            let content = if item.style.arrange == layout::Arrange::Leaf {
                placement.inset(item.style.padding, self.scale).rect
            } else {
                placement.rect
            };
            if node.rect != placement.rect
                || node.device != placement.device
                || node.content != content
            {
                node.rect = placement.rect;
                node.device = placement.device;
                node.content = content;
                node.paint = true;
            }
        }
    }

    /// Records the scenes of the widgets that asked, and collects the
    /// damage. Returns how many recorded a different scene.
    fn paint(&mut self) -> usize {
        self.damage.clear();
        let full = std::mem::take(&mut self.full);
        let mut painted = 0;
        for (index, node) in self.tree.nodes.iter_mut().enumerate() {
            if !node.paint {
                continue;
            }
            node.paint = false;
            node.spare.clear();
            let cx = PaintCx {
                theme: &self.theme,
                rect: node.content,
                scale: self.scale,
                focused: self.focus == Some(NodeId::from_index(index)),
                now: self.now,
            };
            node.widget.paint(&cx, &mut node.spare);
            if !full && node.spare == node.scene {
                // Asked to paint, and drew the same thing: nothing to do.
                continue;
            }
            let scale = self.scale;
            let device = |scene: &Scene| {
                scene
                    .bounds()
                    .map(|bounds| bounds.to_device(scale))
                    .filter(|rect| !rect.is_empty())
            };
            let mut now = device(&node.spare);
            if now.is_some_and(|drawn| !node.device.contains_rect(&drawn)) {
                // It reached past its rectangle: cut it to the rectangle,
                // so that it covers nothing of its neighbours.
                self.cut.clear();
                let spare = &node.spare;
                self.cut.clipped(node.rect, |scene| scene.append(spare));
                std::mem::swap(&mut node.spare, &mut self.cut);
                // The clip's edges are the rectangle's device edges, give or
                // take the rounding of `f32`, which can move the bounds a
                // pixel out but leaves that pixel uncovered.
                now = device(&node.spare).and_then(|drawn| drawn.intersect(&node.device));
                if !full && node.spare == node.scene {
                    continue;
                }
            }
            std::mem::swap(&mut node.scene, &mut node.spare);
            painted += 1;
            let before = std::mem::replace(&mut node.painted, now);
            if !full {
                self.damage.extend(before);
                self.damage.extend(now);
            }
        }
        if full {
            self.damage.push(self.pixels.bounds());
        } else {
            tidy(&mut self.damage, self.pixels.bounds(), MAX_DAMAGE);
        }
        painted
    }

    /// Draws the damage into the back buffer.
    fn render(&mut self) {
        self.composite.clear();
        // The background covers the whole buffer, whatever rounding made
        // its size.
        let scale = f64::from(self.scale);
        let background = Rect::new(
            0.0,
            0.0,
            (f64::from(self.pixels.width()) / scale) as f32,
            (f64::from(self.pixels.height()) / scale) as f32,
        );
        self.composite
            .fill(background, self.theme.palette.background);
        for node in &self.tree.nodes {
            self.composite.append(&node.scene);
        }
        for &rect in &self.damage {
            self.pixels.fill_rect(rect, Color::TRANSPARENT);
        }
        self.renderer
            .render_damage(&self.composite, &mut self.pixels, self.scale, &self.damage);
    }
}

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Root")
            .field("widgets", &self.tree.nodes.len())
            .field("size", &self.size)
            .field("scale", &self.scale)
            .field("focus", &self.focus)
            .field("pointer_holder", &self.capture)
            .finish_non_exhaustive()
    }
}
