//! A switch for a parameter, or a state, that is on or off.

use manguebyte_gfx::{Align, Circle, Point, Rect, RoundedRect, Scene, Size, TextStyle};

use super::{
    Cached, MARGIN, draw_text, drop_shadow, focus_ring, hairline, line, line_height, name_style,
    text_width, vertical,
};
use crate::access::{AccessNode, Action, Actions, Role};
use crate::event::{Button, Event, Handled, Key, KeyEvent};
use crate::motion::{Animated, Lerp};
use crate::param::{ParamBinding, ParamHandle};
use crate::source::{State, TextSource};
use crate::widget::{AccessCx, EventCx, MeasureCx, PaintCx, UpdateCx, Widget};

/// Space between the switch and its name.
const SPACE: f32 = 6.0;

/// Space between the edge and the switch, beyond the margin, for the focus
/// ring around it.
const INSET: f32 = 3.0;

/// A switch and its name beside it, for a parameter ([`toggle`]) or for a
/// `State<bool>` the application keeps ([`Toggle::from_state`]).
///
/// The parameter is on when its normalized value is at least one half, as
/// a [`BoolParam`](manguebyte_params::BoolParam) is. A click, or Space or
/// Enter with the keyboard focus, turns it over as one edit: begin, the
/// new value, end. Each turn starts from the value shown at that moment,
/// so two turns before the host has echoed the first switch it back.
///
/// A click does not give the switch the keyboard focus: Space and Enter
/// stay with the host, which may use Space to start and stop its
/// transport. Tab and [`Action::Focus`] give it the focus.
pub struct Toggle {
    bound: Bound,
    label: Option<TextSource>,
    name: Cached<TextStyle>,
    /// Where the thumb is, from 0 (off) to 1 (on), moving on a spring;
    /// `None` until the first frame puts it where the value is.
    thumb: Option<Animated<f32>>,
}

/// A switch for `param`.
///
/// ```
/// use std::sync::Arc;
/// use manguebyte_gfx::Size;
/// use manguebyte_params::{BoolParam, param_set};
/// use manguebyte_ui::testing::Harness;
/// use manguebyte_ui::{Key, ParamHandle, Role, toggle};
///
/// struct Params { bypass: BoolParam }
/// param_set!(Params { bypass });
/// let params = Arc::new(Params { bypass: BoolParam::new("bypass", "Bypass", false) });
///
/// let view = toggle(ParamHandle::new(Arc::clone(&params), |p| &p.bypass));
/// let mut ui = Harness::with_params(view, Size::new(120.0, 30.0), 1.0, params.clone());
/// ui.frame();
/// let switch = ui.get(Role::Switch, "Bypass");
/// assert_eq!(switch.node.state.checked, Some(false));
///
/// ui.click(switch.center());
/// assert!(params.bypass.value());
/// // The click left Space to the host.
/// assert!(!ui.key(Key::Character(' ')));
/// // With the focus from Tab, Space turns it over.
/// assert!(ui.tab());
/// assert!(ui.key(Key::Character(' ')));
/// assert!(!params.bypass.value());
/// ```
pub fn toggle(param: ParamHandle) -> Toggle {
    Toggle::new(param)
}

/// What a switch shows and changes.
enum Bound {
    Param(ParamBinding),
    State {
        state: State<bool>,
        /// The value last read.
        on: bool,
    },
}

/// A switch's name: its label, or else its parameter's name.
fn name_of<'a>(label: Option<&'a TextSource>, bound: &'a Bound) -> &'a str {
    match (label, bound) {
        (Some(label), _) => label.as_str(),
        (None, Bound::Param(binding)) => binding.handle().name(),
        (None, Bound::State { .. }) => "",
    }
}

impl Toggle {
    /// A switch for `param`, named by the parameter.
    pub fn new(param: ParamHandle) -> Toggle {
        Toggle {
            bound: Bound::Param(ParamBinding::new(param)),
            label: None,
            name: Cached::new(),
            thumb: None,
        }
    }

    /// A switch named `name`, fixed or read once per frame (see
    /// [`TextSource`]), showing and turning over the value in `on`: a
    /// choice of the application's own that is no parameter, such as
    /// whether a MIDI port is open. The state is read once per frame; a
    /// click, Space, Enter or [`Action::Press`] sets it to the other value
    /// once, starting from what it holds at that moment. Its value for
    /// assistive technology is "On" or "Off".
    ///
    /// ```
    /// use manguebyte_gfx::Size;
    /// use manguebyte_ui::testing::Harness;
    /// use manguebyte_ui::{Role, State, Toggle};
    ///
    /// let open = State::new(false);
    /// let view = Toggle::from_state("Keys 49", open.clone());
    /// let mut ui = Harness::new(view, Size::new(160.0, 30.0), 1.0);
    /// ui.frame();
    /// ui.click(ui.get(Role::Switch, "Keys 49").center());
    /// assert!(open.get());
    /// // The application changes it too.
    /// open.set(false);
    /// ui.frame();
    /// assert_eq!(ui.get(Role::Switch, "Keys 49").node.state.checked, Some(false));
    /// ```
    pub fn from_state(name: impl Into<TextSource>, on: State<bool>) -> Toggle {
        Toggle {
            bound: Bound::State {
                on: on.get(),
                state: on,
            },
            label: Some(name.into()),
            name: Cached::new(),
            thumb: None,
        }
    }

    /// The same switch showing `text` as its name instead of the
    /// parameter's name.
    #[must_use]
    pub fn label(mut self, text: impl Into<String>) -> Toggle {
        self.label = Some(TextSource::fixed(text));
        self
    }

    /// The same switch named by `text`, fixed or read once per frame (see
    /// [`TextSource`]): a switch whose name follows the language, say. A
    /// new name lays the interface out again, draws the switch again and
    /// is sent to assistive technology.
    #[must_use]
    pub fn label_source(mut self, text: impl Into<TextSource>) -> Toggle {
        self.label = Some(text.into());
        self
    }

    fn name_text(&self) -> &str {
        name_of(self.label.as_ref(), &self.bound)
    }

    fn is_on(&self) -> bool {
        match &self.bound {
            Bound::Param(binding) => binding.shown() >= 0.5,
            Bound::State { on, .. } => *on,
        }
    }

    fn value_text(&self) -> String {
        match &self.bound {
            Bound::Param(binding) => binding.text(),
            Bound::State { on: true, .. } => String::from("On"),
            Bound::State { on: false, .. } => String::from("Off"),
        }
    }

    fn press(&mut self, cx: &mut EventCx<'_>) {
        match &mut self.bound {
            Bound::Param(binding) => {
                let next = if binding.current() >= 0.5 { 0.0 } else { 1.0 };
                binding.set_once(cx.context(), next);
            }
            Bound::State { state, on } => {
                *on = !state.get();
                state.set(*on);
            }
        }
        cx.request_paint();
    }
}

impl Widget for Toggle {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Switch, self.name_text())
            .with_value(self.value_text())
            .with_checked(self.is_on())
            .with_actions(Actions::PRESS)
    }

    fn measure(&mut self, cx: &MeasureCx<'_>) -> Size {
        let theme = cx.theme();
        let style = name_style(theme);
        let sizes = theme.sizes;
        Size::new(
            2.0 * MARGIN
                + INSET
                + sizes.toggle_width.max(sizes.toggle_height)
                + SPACE
                + text_width(self.name_text(), &style),
            2.0 * (MARGIN + INSET) + sizes.toggle_height.max(line_height(&style)),
        )
    }

    fn update(&mut self, cx: &mut UpdateCx<'_>) {
        super::poll_label(&mut self.label, &mut self.name, cx);
        match &mut self.bound {
            Bound::Param(binding) => {
                if binding.poll(cx.now()) {
                    cx.request_paint();
                }
            }
            Bound::State { state, on } => {
                let now = state.get();
                if now != *on {
                    *on = now;
                    cx.request_paint();
                    cx.request_access_update();
                }
            }
        }
        // The thumb slides to where the value is, on a spring.
        let target = if self.is_on() { 1.0 } else { 0.0 };
        let (now, spring) = (cx.now(), cx.motion().springy());
        let thumb = self.thumb.get_or_insert_with(|| Animated::new(target));
        if thumb.transition(target, now, spring) {
            cx.request_paint();
        }
        cx.animate(thumb);
    }

    fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene) {
        let theme = cx.theme();
        let rect = cx.rect();
        let sizes = theme.sizes;
        let style = name_style(theme).with_align(Align::Start);
        let height = sizes.toggle_height.max(0.0);
        let width = sizes.toggle_width.max(height);
        let middle = rect.y + rect.height * 0.5;
        let pill = Rect::new(
            rect.x + MARGIN + INSET,
            middle - height * 0.5,
            width,
            height,
        );
        let palette = &theme.palette;
        let on = self
            .thumb
            .map_or(if self.is_on() { 1.0 } else { 0.0 }, |thumb| thumb.value());
        let shape = RoundedRect::new(pill, height * 0.5);
        // Off: the track, a hairline, lighter under the pointer. On: the
        // accent's gradient, faded in as the thumb travels.
        let off = palette.track.lerp(palette.hairline_strong, cx.hover());
        scene.fill(shape, off);
        hairline(scene, pill, height * 0.5, cx.scale(), palette.hairline);
        let lit = on.clamp(0.0, 1.0);
        if lit > 0.0 {
            vertical(
                scene,
                shape,
                (pill.y, pill.bottom()),
                palette.accent_bright.with_opacity(lit),
                palette.accent.with_opacity(lit),
            );
        }
        let radius = (height * 0.5 - 2.0).max(0.0);
        let left = pill.x + height * 0.5;
        let right = pill.right() - height * 0.5;
        let thumb = Circle::new(Point::new(left.lerp(right, on), middle), radius);
        drop_shadow(scene, thumb, super::thumb_shadow(theme));
        scene.fill(thumb, palette.thumb);
        focus_ring(
            scene,
            RoundedRect::new(
                pill.outset(theme.sizes.focus_gap),
                height * 0.5 + theme.sizes.focus_gap,
            ),
            theme,
            cx.focus_ring(),
        );

        let text_x = pill.right() + SPACE;
        let room = (rect.right() - MARGIN - text_x).max(0.0);
        let text = name_of(self.label.as_ref(), &self.bound);
        let name = self.name.get(room, style, || line(text, &style, room));
        let y = middle - line_height(&style) * 0.5;
        draw_text(scene, name, Point::new(text_x, y));
    }

    fn event(&mut self, cx: &mut EventCx<'_>, event: &Event) -> Handled {
        match *event {
            Event::PointerDown {
                button: Button::Primary,
                ..
            } => {
                // No focus asked for: see the type's documentation.
                self.press(cx);
                Handled::Yes
            }
            Event::Key(KeyEvent {
                key: Key::Character(' ') | Key::Enter,
                pressed: true,
                ..
            }) => {
                self.press(cx);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn act(&mut self, cx: &mut EventCx<'_>, action: Action) -> bool {
        if action != Action::Press {
            return false;
        }
        self.press(cx);
        true
    }
}
