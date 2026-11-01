//! A control that picks one value from a menu: one of a parameter's
//! values, or one of a list the application keeps in a state.

use manguebyte_gfx::{Align, Path, Point, Rect, Scene, Size, Stroke, TextStyle};

use super::text_field::{PAD_X, PAD_Y, field_box};
use super::{Cached, GAP, MARGIN, line, line_height, name_style, text_width};
use crate::access::{AccessNode, Action, Actions, Role};
use crate::event::{Button, Event, Handled, Key, KeyEvent, MenuEvent};
use crate::menu::{Menu, MenuItem};
use crate::param::{ParamBinding, ParamHandle};
use crate::source::{State, TextSource};
use crate::theme::Theme;
use crate::widget::{AccessCx, EventCx, MeasureCx, PaintCx, UpdateCx, Widget};

/// The most values a combo box lists. A parameter with more steps, or none,
/// lists nothing, and its combo box opens no menu: a knob or a slider suits
/// it better. A list of [`Choices`] longer than this lists its first
/// `MAX_CHOICES` items.
pub const MAX_CHOICES: u32 = 128;

/// The room on the right of the box for its arrow.
const ARROW: f32 = 16.0;

/// A combo box: a name, and a box showing a value, which opens a menu of
/// every value it can take.
///
/// [`combo`] (or [`ComboBox::new`]) binds it to a parameter, and
/// [`ComboBox::from_state`] to a list of [`Choices`] the application keeps
/// in a [`State`] and may change while the interface runs: the devices of
/// a settings screen, which come and go.
///
/// Bound to a parameter, it is for parameters with a few named values: a
/// [`ChoiceParam`](manguebyte_params::ChoiceParam), a switch, an integer
/// with a short range (at most [`MAX_CHOICES`] values). Each value is
/// listed in the parameter's own words, the current one checked.
///
/// - A press, or Enter, Space, F4 or Alt+Down with the keyboard focus
///   (which a press gives it), opens the menu below the box, or above it
///   where there is no room below (see [`crate::menu`]). Choosing a value
///   sends it as one edit: begin, the value, end. Closing the menu with
///   nothing chosen sends nothing.
/// - With the focus and the menu closed, Up and Down choose the previous
///   and next value, Home and End the first and the last, and the wheel
///   steps through them, each one edit.
///
/// For assistive technology it is a [`ComboBox`](Role::ComboBox) whose
/// value is the value's text and which says whether its menu is open;
/// [`Action::Press`] opens the menu.
///
/// ```
/// use std::sync::Arc;
/// use manguebyte_gfx::Size;
/// use manguebyte_params::{ChoiceParam, param_set};
/// use manguebyte_ui::testing::Harness;
/// use manguebyte_ui::{ParamHandle, Role, combo};
///
/// struct Params { shape: ChoiceParam }
/// param_set!(Params { shape });
/// let params = Arc::new(Params {
///     shape: ChoiceParam::new("shape", "Shape", &["Sine", "Saw", "Square"], 0),
/// });
/// let view = combo(ParamHandle::new(Arc::clone(&params), |p| &p.shape));
/// let mut ui = Harness::with_params(view, Size::new(160.0, 160.0), 1.0, params.clone());
/// ui.frame();
/// let shape = ui.get(Role::ComboBox, "Shape");
/// assert_eq!(shape.value_text(), Some("Sine"));
///
/// ui.click(shape.center());
/// ui.frame();
/// assert_eq!(ui.get(Role::ComboBox, "Shape").node.state.expanded, Some(true));
/// // The current value is checked, and has the focus.
/// let sine = ui.get(Role::MenuItem, "Sine");
/// assert!(sine.node.state.checked == Some(true) && sine.is_focused());
/// ui.click(ui.get(Role::MenuItem, "Square").center());
/// assert_eq!(params.shape.index(), 2);
/// ui.frame();
/// assert!(ui.find(Role::Menu, "Shape").is_none(), "the menu closed");
/// ```
pub fn combo(param: ParamHandle) -> ComboBox {
    ComboBox::new(param)
}

/// A list of texts to choose from and the one chosen, shared through a
/// [`State`] by a [`ComboBox::from_state`] and the application.
///
/// The application sets the list and the choice whenever it likes; the
/// combo box sets [`selected`](Choices::selected) when the person chooses,
/// and the application polls the state. A `selected` past the end of the
/// list counts as nothing chosen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Choices {
    /// The texts listed, in order.
    pub items: Vec<String>,
    /// The position of the one chosen in [`items`](Choices::items), if
    /// any.
    pub selected: Option<usize>,
}

impl Choices {
    /// A list of `items` with the one at `selected` chosen; nothing is
    /// chosen if `selected` is past the end.
    pub fn new<S: Into<String>>(
        items: impl IntoIterator<Item = S>,
        selected: Option<usize>,
    ) -> Choices {
        let items: Vec<String> = items.into_iter().map(Into::into).collect();
        let selected = selected.filter(|&at| at < items.len());
        Choices { items, selected }
    }

    /// The position of the one chosen, if it is in the list.
    pub fn chosen(&self) -> Option<usize> {
        self.selected.filter(|&at| at < self.items.len())
    }

    /// The text of the one chosen, if it is in the list.
    pub fn chosen_text(&self) -> Option<&str> {
        self.chosen().map(|at| self.items[at].as_str())
    }
}

/// What a combo box shows and changes.
enum Bound {
    Param(ParamBinding),
    Choices(ChoiceBinding),
}

/// A combo box's hold on a [`State`] of [`Choices`].
struct ChoiceBinding {
    state: State<Choices>,
    /// The version of the state last read.
    seen: Option<u64>,
    /// A copy of the state at that version.
    shown: Choices,
    /// The texts of the menu open, as it was opened.
    listed: Vec<String>,
    /// Shown when nothing is chosen.
    placeholder: String,
}

impl ChoiceBinding {
    /// Reads the state again if it changed. Returns whether what it holds
    /// did.
    fn poll(&mut self) -> bool {
        let version = self.state.version();
        if self.seen == Some(version) {
            return false;
        }
        self.seen = Some(version);
        let fresh = self.state.get();
        let changed = fresh != self.shown;
        self.shown = fresh;
        changed
    }

    /// How many items it lists.
    fn count(&self) -> usize {
        self.shown.items.len().min(MAX_CHOICES as usize)
    }

    fn text(&self) -> &str {
        self.shown.chosen_text().unwrap_or(&self.placeholder)
    }

    /// Chooses `text`, which was at `at` when the person saw it, where it
    /// is now: at `at` if it still is, since the list may have changed in
    /// between, or else the first item with that text. Nothing if it is
    /// gone. Returns whether the choice changed.
    fn choose(&mut self, at: usize, text: &str) -> bool {
        let (found, current) = self.state.with(|choices| {
            let found = if choices.items.get(at).is_some_and(|item| item == text) {
                Some(at)
            } else {
                choices.items.iter().position(|item| item == text)
            };
            (found, choices.chosen())
        });
        let Some(found) = found else {
            return false;
        };
        if current != Some(found) {
            self.state.update(|choices| choices.selected = Some(found));
        }
        self.poll();
        current != Some(found)
    }
}

/// The widget [`combo`] and [`ComboBox::from_state`] make.
pub struct ComboBox {
    bound: Bound,
    label: Option<TextSource>,
    /// Whether its menu is open.
    open: bool,
    name: Cached<TextStyle>,
    /// Made for the value shown: as bits, or the state's version; whether
    /// it is a value sent; and the style.
    value: Cached<(u64, bool, TextStyle)>,
}

impl ComboBox {
    /// A combo box for `param`, named by the parameter.
    pub fn new(param: ParamHandle) -> ComboBox {
        ComboBox {
            bound: Bound::Param(ParamBinding::new(param)),
            label: None,
            open: false,
            name: Cached::new(),
            value: Cached::new(),
        }
    }

    /// A combo box named `name`, fixed or read once per frame (see
    /// [`TextSource`]), listing the [`Choices`] in `choices` and setting
    /// their [`selected`](Choices::selected) when the person chooses.
    ///
    /// The list is read once per frame, and copied only when the state's
    /// version changed: a new list lays the interface out again (the box
    /// is as wide as its widest item) and is drawn. A choice from a menu
    /// opened before the list changed is found again by its text, and
    /// dropped if it is gone. With nothing chosen the box shows its
    /// [`placeholder`](ComboBox::placeholder); with an empty list it opens
    /// no menu. The keys, the wheel and assistive technology work as for a
    /// parameter, and each change sets the state once; choosing what is
    /// chosen already does not set it.
    ///
    /// ```
    /// use manguebyte_gfx::Size;
    /// use manguebyte_ui::testing::Harness;
    /// use manguebyte_ui::{Choices, ComboBox, Key, Role, State};
    ///
    /// let devices = State::new(Choices::new(["Speakers", "Headphones"], Some(0)));
    /// let view = ComboBox::from_state("Output", devices.clone());
    /// let mut ui = Harness::new(view, Size::new(200.0, 160.0), 1.0);
    /// ui.frame();
    /// assert_eq!(ui.get(Role::ComboBox, "Output").value_text(), Some("Speakers"));
    ///
    /// // The person picks from the menu; the application polls the state.
    /// ui.click(ui.get(Role::ComboBox, "Output").center());
    /// ui.frame();
    /// ui.click(ui.get(Role::MenuItem, "Headphones").center());
    /// assert_eq!(devices.get().selected, Some(1));
    ///
    /// // A device comes: the application sets a new list.
    /// devices.set(Choices::new(["Speakers", "Headphones", "Interface"], Some(1)));
    /// ui.frame();
    /// assert!(ui.key(Key::Down), "the focus came with the press");
    /// assert_eq!(devices.get().chosen_text(), Some("Interface"));
    /// ```
    pub fn from_state(name: impl Into<TextSource>, choices: State<Choices>) -> ComboBox {
        ComboBox {
            bound: Bound::Choices(ChoiceBinding {
                state: choices,
                seen: None,
                shown: Choices::default(),
                listed: Vec::new(),
                placeholder: String::from("None"),
            }),
            label: Some(name.into()),
            open: false,
            name: Cached::new(),
            value: Cached::new(),
        }
    }

    /// The same combo box showing `text` while nothing is chosen, for one
    /// made by [`from_state`](ComboBox::from_state); "None" unless set. A
    /// combo box bound to a parameter always shows its value.
    #[must_use]
    pub fn placeholder(mut self, text: impl Into<String>) -> ComboBox {
        if let Bound::Choices(choices) = &mut self.bound {
            choices.placeholder = text.into();
        }
        self
    }

    /// The same combo box showing `text` as its name instead of the
    /// parameter's name.
    #[must_use]
    pub fn label(mut self, text: impl Into<String>) -> ComboBox {
        self.label = Some(TextSource::fixed(text));
        self
    }

    /// The same combo box named by `text`, fixed or read once per frame (see
    /// [`TextSource`]): a combo box whose name follows the language, say. A
    /// new name lays the interface out again, draws the combo box again and
    /// is sent to assistive technology.
    #[must_use]
    pub fn label_source(mut self, text: impl Into<TextSource>) -> ComboBox {
        self.label = Some(text.into());
        self
    }

    fn name_text(&self) -> &str {
        name_of(self.label.as_ref(), &self.bound)
    }

    /// How many values it lists; `None` when it lists nothing and opens no
    /// menu.
    fn count(&self) -> Option<usize> {
        match &self.bound {
            Bound::Param(binding) => binding
                .handle()
                .step_count()
                .filter(|&steps| steps > 0 && steps < MAX_CHOICES)
                .map(|steps| steps as usize + 1),
            Bound::Choices(choices) => Some(choices.count()).filter(|&count| count > 0),
        }
    }

    /// The position of the value shown, if it is one of those listed.
    fn current(&mut self) -> Option<usize> {
        let count = self.count()?;
        match &mut self.bound {
            Bound::Param(binding) => {
                let steps = (count - 1) as f64;
                let value = binding.current();
                Some(((value * steps).round().max(0.0) as usize).min(count - 1))
            }
            Bound::Choices(choices) => choices.shown.chosen().filter(|&at| at < count),
        }
    }

    /// The text of the value at `at`, among `count`.
    fn item_text(&self, at: usize, count: usize) -> String {
        match &self.bound {
            Bound::Param(binding) => {
                let steps = count.saturating_sub(1).max(1) as f64;
                binding.handle().text(at as f64 / steps)
            }
            Bound::Choices(choices) => choices.shown.items.get(at).cloned().unwrap_or_default(),
        }
    }

    /// The text of the value shown.
    fn value_text(&self) -> String {
        match &self.bound {
            Bound::Param(binding) => binding.text(),
            Bound::Choices(choices) => choices.text().to_owned(),
        }
    }

    fn open_menu(&mut self, cx: &mut EventCx<'_>) {
        let Some(count) = self.count() else {
            return;
        };
        let current = self.current();
        let texts: Vec<String> = (0..count).map(|at| self.item_text(at, count)).collect();
        let mut menu = Menu::new(self.name_text());
        for (at, text) in texts.iter().enumerate() {
            menu.push(MenuItem::new(at, text.clone()).checked(Some(at) == current));
        }
        if let Bound::Choices(choices) = &mut self.bound {
            choices.listed = texts;
        }
        self.open = true;
        cx.open_menu(menu);
        cx.request_paint();
    }

    /// Chooses the value at `at` as one edit: among those of the menu when
    /// it comes `from_menu`, else among those shown.
    fn choose(&mut self, cx: &mut EventCx<'_>, at: usize, from_menu: bool) {
        let Some(count) = self.count() else {
            return;
        };
        match &mut self.bound {
            Bound::Param(binding) => {
                let steps = (count - 1) as f64;
                let at = at.min(count - 1);
                binding.set_once(cx.context(), at as f64 / steps);
                cx.request_paint();
            }
            Bound::Choices(choices) => {
                let seen = if from_menu {
                    choices.listed.get(at).cloned()
                } else {
                    choices.shown.items.get(at).cloned()
                };
                if let Some(text) = seen {
                    if choices.choose(at, &text) {
                        // The value shown may be wider or narrower.
                        cx.request_layout();
                    }
                }
            }
        }
    }

    /// Moves `by` values from the value shown, as one edit.
    fn step(&mut self, cx: &mut EventCx<'_>, by: i64) {
        let Some(count) = self.count() else {
            return;
        };
        let last = count as i64 - 1;
        let current = self.current();
        let next = match current {
            Some(current) => (current as i64 + by).clamp(0, last),
            // Nothing chosen: down from before the first, up from after
            // the last.
            None if by > 0 => (by - 1).clamp(0, last),
            None => (last + 1 + by).clamp(0, last),
        };
        if Some(next as usize) != current {
            self.choose(cx, next as usize, false);
        }
    }
}

/// A combo box's name: its label, or else its parameter's name.
fn name_of<'a>(label: Option<&'a TextSource>, bound: &'a Bound) -> &'a str {
    match (label, bound) {
        (Some(label), _) => label.as_str(),
        (None, Bound::Param(binding)) => binding.handle().name(),
        (None, Bound::Choices(_)) => "",
    }
}

/// The box's rectangle inside the combo box's.
fn value_box(rect: Rect, theme: &Theme) -> Rect {
    let name = line_height(&name_style(theme));
    let top = rect.y + MARGIN + name + GAP;
    Rect::new(
        rect.x + MARGIN,
        top,
        (rect.width - 2.0 * MARGIN).max(0.0),
        (rect.bottom() - MARGIN - top).max(0.0),
    )
}

/// The style of the value in the box.
fn box_style(theme: &Theme) -> TextStyle {
    theme
        .typography
        .label
        .with_color(theme.palette.text)
        .with_align(Align::Start)
        .with_max_lines(1)
}

impl Widget for ComboBox {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::ComboBox, self.name_text())
            .with_value(self.value_text())
            .with_expanded(self.open)
            .with_actions(Actions::PRESS)
    }

    fn measure(&mut self, cx: &MeasureCx<'_>) -> Size {
        let theme = cx.theme();
        let name = name_style(theme);
        let value = box_style(theme);
        // A list of choices may show its placeholder, which is no item.
        let least = match &self.bound {
            Bound::Param(_) => 0.0,
            Bound::Choices(choices) => text_width(&choices.placeholder, &value),
        };
        let widest = match self.count() {
            Some(count) => (0..count)
                .map(|at| text_width(&self.item_text(at, count), &value))
                .fold(least, f32::max),
            None => text_width(&self.value_text(), &value),
        };
        let width = text_width(self.name_text(), &name).max(widest + 2.0 * PAD_X + ARROW);
        Size::new(
            width + 2.0 * MARGIN,
            2.0 * MARGIN + line_height(&name) + GAP + line_height(&value) + 2.0 * PAD_Y,
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
            Bound::Choices(choices) => {
                if choices.poll() {
                    // The box is as wide as its widest item.
                    cx.request_layout();
                    cx.request_access_update();
                }
            }
        }
    }

    fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene) {
        let theme = cx.theme();
        let rect = cx.rect();
        let width = (rect.width - 2.0 * MARGIN).max(0.0);
        let name_style = name_style(theme);
        let name_text = name_of(self.label.as_ref(), &self.bound);
        let name = self
            .name
            .get(width, name_style, || line(name_text, &name_style, width));
        scene.text(name, Point::new(rect.x + MARGIN, rect.y + MARGIN));

        let outer = value_box(rect, theme);
        let open = if self.open { 1.0 } else { 0.0 };
        field_box(
            scene,
            outer,
            theme,
            false,
            (cx.focus_ring().max(open), cx.hover().max(open)),
        );
        let style = box_style(theme);
        let room = (outer.width - 2.0 * PAD_X - ARROW).max(0.0);
        let key = match &self.bound {
            Bound::Param(binding) => (binding.shown().to_bits(), binding.shows_sent(), style),
            Bound::Choices(choices) => (choices.seen.unwrap_or(u64::MAX), false, style),
        };
        let text = self.value_text();
        let value = self.value.get(room, key, || line(&text, &style, room));
        let y = outer.y + (outer.height - line_height(&style)) * 0.5;
        scene.text(value, Point::new(outer.x + PAD_X, y));
        // A chevron pointing down.
        let x = outer.right() - ARROW * 0.5 - 2.0;
        let middle = outer.y + outer.height * 0.5;
        let mut arrow = Path::new();
        arrow
            .move_to(Point::new(x - 4.0, middle - 2.0))
            .line_to(Point::new(x, middle + 2.0))
            .line_to(Point::new(x + 4.0, middle - 2.0));
        scene.stroke(&arrow, Stroke::round(1.5), theme.palette.text_secondary);
    }

    fn event(&mut self, cx: &mut EventCx<'_>, event: &Event) -> Handled {
        match *event {
            Event::PointerDown {
                button: Button::Primary,
                ..
            } => {
                cx.request_focus();
                self.open_menu(cx);
                Handled::Yes
            }
            Event::Wheel { delta, .. } => {
                let notches = delta.y + delta.x;
                if !(notches.is_finite() && notches != 0.0) {
                    return Handled::No;
                }
                // Up the wheel, up the list: to the value before.
                self.step(cx, if notches > 0.0 { -1 } else { 1 });
                Handled::Yes
            }
            Event::Key(KeyEvent {
                key,
                modifiers,
                pressed: true,
            }) => {
                match key {
                    Key::Enter | Key::Character(' ') | Key::Function(4) => self.open_menu(cx),
                    Key::Down if modifiers.alt => self.open_menu(cx),
                    Key::Up | Key::Left => self.step(cx, -1),
                    Key::Down | Key::Right => self.step(cx, 1),
                    Key::Home => self.step(cx, -i64::from(MAX_CHOICES)),
                    Key::End => self.step(cx, i64::from(MAX_CHOICES)),
                    _ => return Handled::No,
                }
                Handled::Yes
            }
            Event::Menu(result) => {
                self.open = false;
                if let MenuEvent::Chosen(at) = result {
                    self.choose(cx, at, true);
                }
                cx.request_paint();
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn act(&mut self, cx: &mut EventCx<'_>, action: Action) -> bool {
        if action != Action::Press || self.count().is_none() {
            return false;
        }
        self.open_menu(cx);
        true
    }
}
