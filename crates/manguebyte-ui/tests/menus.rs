//! Popup menus and the combo box: placement, the pointer, the keys,
//! submenus, closing, accessibility and damage.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use manguebyte_gfx::{Point, Scene, Size};
use manguebyte_params::{BoolParam, ChoiceParam, IntParam, Param, param_set};
use manguebyte_plugin::editor::testing::HostCall::{BeginGesture, EndGesture, Perform};
use manguebyte_ui::access::{AccessNode, Actions};
use manguebyte_ui::layout::Align;
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{
    AccessCx, Action, Event, EventCx, Handled, IntoView, Key, KeyEvent, MeasureCx, Menu, MenuEvent,
    MenuItem, Modifiers, PaintCx, ParamHandle, Role, Widget, column, combo, knob, label, row,
};

struct Params {
    shape: ChoiceParam,
    voices: IntParam,
    gain: manguebyte_params::FloatParam,
    mono: BoolParam,
}
param_set!(Params {
    shape,
    voices,
    gain,
    mono
});

fn params() -> Arc<Params> {
    Arc::new(Params {
        shape: ChoiceParam::new("shape", "Shape", &["Sine", "Saw", "Square", "Noise"], 1),
        voices: IntParam::new("voices", "Voices", 1, 8, 4),
        gain: manguebyte_params::FloatParam::new(
            "gain",
            "Gain",
            manguebyte_params::FloatRange::linear(0.0, 1.0),
            0.5,
        ),
        mono: BoolParam::new("mono", "Mono", false),
    })
}

/// A combo box for the shape at the top of a tall window, and a knob
/// below it.
fn tall(params: &Arc<Params>) -> Harness {
    let view = column((
        combo(ParamHandle::new(Arc::clone(params), |p| &p.shape)),
        knob(ParamHandle::new(Arc::clone(params), |p| &p.gain)),
    ))
    .spacing(4.0)
    .padding(8.0)
    .align(Align::Start);
    let set: Arc<dyn manguebyte_params::ParamSet> = Arc::clone(params) as _;
    let mut ui = Harness::with_params(view, Size::new(240.0, 260.0), 1.0, set);
    ui.frame();
    ui
}

fn key(key: Key) -> Event {
    Event::Key(KeyEvent {
        key,
        modifiers: Modifiers::NONE,
        pressed: true,
    })
}

#[test]
fn a_combo_box_opens_below_and_a_click_chooses() {
    let params = params();
    let mut ui = tall(&params);
    let shape = ui.get(Role::ComboBox, "Shape");
    assert_eq!(shape.value_text(), Some("Saw"));
    assert_eq!(shape.node.state.expanded, Some(false));
    ui.assert_accessible();
    ui.click(shape.center());
    ui.frame();
    let menu = ui.get(Role::Menu, "Shape");
    assert!(menu.bounds.y >= shape.bounds.bottom() - 0.5, "below");
    assert!(menu.bounds.width >= shape.bounds.width - 0.5, "as wide");
    let names: Vec<String> = ui
        .everything()
        .into_iter()
        .filter(|found| found.role() == Role::MenuItem)
        .map(|found| found.name().to_owned())
        .collect();
    assert_eq!(names, ["Sine", "Saw", "Square", "Noise"]);
    // The current value is checked and has the focus.
    let saw = ui.get(Role::MenuItem, "Saw");
    assert_eq!(saw.node.state.checked, Some(true));
    assert!(saw.is_focused());
    assert_eq!(
        ui.get(Role::ComboBox, "Shape").node.state.expanded,
        Some(true)
    );
    ui.assert_accessible();
    assert!(ui.take_calls().is_empty());
    ui.click(ui.get(Role::MenuItem, "Noise").center());
    let id = params.shape.param_id();
    assert_eq!(
        ui.take_calls(),
        [BeginGesture(id), Perform(id, 1.0), EndGesture(id)]
    );
    assert_eq!(params.shape.index(), 3);
    ui.frame();
    assert!(ui.find(Role::Menu, "Shape").is_none());
    let shape = ui.get(Role::ComboBox, "Shape");
    assert_eq!(shape.value_text(), Some("Noise"));
    assert!(shape.is_focused(), "the focus came back");
    assert_eq!(shape.node.state.expanded, Some(false));
}

#[test]
fn a_combo_box_without_room_below_opens_above() {
    let params = params();
    let view = column((
        label("Oscillator"),
        combo(ParamHandle::new(Arc::clone(&params), |p| &p.shape)),
    ))
    .padding(4.0)
    .align(Align::Start);
    let mut ui = Harness::new(view, Size::new(200.0, 150.0), 1.5);
    ui.frame();
    let shape = ui.get(Role::ComboBox, "Shape");
    ui.click(shape.center());
    ui.frame();
    let menu = ui.get(Role::Menu, "Shape");
    if menu.bounds.height <= shape.bounds.y {
        assert!(
            menu.bounds.bottom() <= shape.bounds.y + 0.5,
            "above: {menu:?}"
        );
    } else {
        assert_eq!(menu.bounds.y, 0.0, "against the top");
    }
    assert!(menu.bounds.right() <= 200.0 + 0.5);
    assert!(ui.render_fresh() == *ui.pixels());
}

#[test]
fn the_keys_move_choose_and_close() {
    let params = params();
    let mut ui = tall(&params);
    assert!(ui.tab());
    assert_eq!(ui.focused().map(|f| f.role()), Some(Role::ComboBox));
    // Up and Down choose values directly while the menu is closed.
    assert!(ui.key(Key::Down));
    assert_eq!(params.shape.index(), 2);
    assert!(ui.key(Key::Up));
    assert!(ui.key(Key::Up));
    assert_eq!(params.shape.index(), 0);
    ui.take_calls();
    // Alt+Down opens it at the checked value.
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    assert!(ui.key_with(Key::Down, alt));
    ui.frame();
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Sine")
    );
    // Up goes round to the end; Home back to the start; a letter jumps.
    ui.key(Key::Up);
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Noise")
    );
    ui.key(Key::Home);
    ui.send(Event::Text('s'));
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Saw")
    );
    ui.send(Event::Text('S'));
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Square")
    );
    // Every key is the menu's: none goes to the host.
    assert!(ui.key(Key::Character('q')));
    assert!(ui.key(Key::Function(5)));
    assert!(ui.take_calls().is_empty());
    // Escape closes it and sends nothing.
    assert!(ui.key(Key::Escape));
    ui.frame();
    assert!(ui.find(Role::Menu, "Shape").is_none());
    assert!(ui.take_calls().is_empty());
    // Enter opens it again; Down and Enter choose.
    assert!(ui.key(Key::Enter));
    ui.key(Key::Down);
    assert!(ui.key(Key::Enter));
    assert_eq!(params.shape.index(), 1);
    let id = params.shape.param_id();
    assert_eq!(
        ui.take_calls(),
        [BeginGesture(id), Perform(id, 1.0 / 3.0), EndGesture(id)]
    );
    // Tab closes it too, and leaves the focus where it was.
    ui.key(Key::Character(' '));
    assert!(ui.find(Role::Menu, "Shape").is_some());
    assert!(ui.tab());
    assert!(ui.find(Role::Menu, "Shape").is_none());
    assert_eq!(ui.focused().map(|f| f.role()), Some(Role::ComboBox));
}

#[test]
fn a_press_outside_or_the_window_losing_the_keyboard_closes_it_and_nothing_else() {
    let params = params();
    let mut ui = tall(&params);
    let shape = ui.get(Role::ComboBox, "Shape");
    let gain = ui.get(Role::Slider, "Gain");
    ui.click(shape.center());
    ui.frame();
    let menu = ui.get(Role::Menu, "Shape");
    assert!(!menu.bounds.contains(Point::new(230.0, 250.0)));
    // A press outside: the menu closes, and the knob under it hears
    // nothing.
    ui.drag(Point::new(230.0, 250.0), (0.0, -40.0), 2);
    assert!(ui.find(Role::Menu, "Shape").is_none());
    assert!(ui.take_calls().is_empty());
    assert_eq!(params.shape.index(), 1);
    // A press inside the menu's panel but on no item does nothing either.
    ui.click(shape.center());
    ui.frame();
    let menu = ui.get(Role::Menu, "Shape");
    ui.click(Point::new(menu.bounds.x + 1.5, menu.bounds.y + 1.5));
    assert!(ui.find(Role::Menu, "Shape").is_some());
    // The window losing the keyboard closes it.
    ui.send(Event::Focus(false));
    ui.frame();
    assert!(ui.find(Role::Menu, "Shape").is_none());
    assert_eq!(
        ui.get(Role::ComboBox, "Shape").node.state.expanded,
        Some(false)
    );
    assert!(ui.take_calls().is_empty());
    // The knob is reachable again.
    ui.drag(gain.center(), (0.0, -20.0), 2);
    assert!(!ui.take_calls().is_empty());
}

#[test]
fn press_drag_and_release_chooses_and_a_still_release_does_not() {
    let params = params();
    let mut ui = tall(&params);
    let shape = ui.get(Role::ComboBox, "Shape");
    // Press and let go where it was: the menu stays open.
    ui.press(shape.center());
    ui.release(shape.center());
    ui.frame();
    assert!(ui.find(Role::Menu, "Shape").is_some());
    ui.key(Key::Escape);
    // Press, drag down onto an item, release: chosen.
    ui.press(shape.center());
    ui.frame();
    let square = ui.get(Role::MenuItem, "Square");
    ui.move_to(square.center());
    // Hovering highlights.
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Square")
    );
    ui.release(square.center());
    assert_eq!(params.shape.index(), 2);
    ui.frame();
    assert!(ui.find(Role::Menu, "Shape").is_none());
}

#[test]
fn opening_and_closing_draws_only_the_menu() {
    let params = params();
    let mut ui = tall(&params);
    let shape = ui.get(Role::ComboBox, "Shape");
    ui.click(shape.center());
    ui.frame();
    let menu = ui.get(Role::Menu, "Shape");
    // The box, and the menu over the rest.
    let opened = ui.last_damage_bounds().expect("drawn");
    assert!(
        opened.contains_rect(&menu.device),
        "{opened:?} holds {:?}",
        menu.device
    );
    assert!(ui.render_fresh() == *ui.pixels());
    // Moving the highlight draws two items.
    ui.key(Key::Down);
    let summary = ui.frame();
    assert_eq!(summary.painted, 2);
    let damage = ui.last_damage_bounds().expect("drawn");
    assert!(menu.device.contains_rect(&damage));
    // Closing draws again what the menu covered, from what is under it.
    ui.key(Key::Escape);
    ui.frame();
    let closed = ui.last_damage_bounds().expect("drawn");
    assert!(closed.contains_rect(&menu.device));
    assert!(ui.render_fresh() == *ui.pixels());
    assert_eq!(ui.frame().damaged, 0);
}

#[test]
fn assistive_technology_opens_and_chooses() {
    let params = params();
    let view = row((
        combo(ParamHandle::new(Arc::clone(&params), |p| &p.voices)),
        combo(ParamHandle::new(Arc::clone(&params), |p| &p.mono)).label("Mode"),
    ))
    .spacing(8.0)
    .align(Align::Start);
    let set: Arc<dyn manguebyte_params::ParamSet> = Arc::clone(&params) as _;
    let mut ui = Harness::with_params(view, Size::new(300.0, 300.0), 2.0, set);
    ui.frame();
    let voices = ui.get(Role::ComboBox, "Voices");
    assert_eq!(voices.value_text(), Some("4"));
    assert!(ui.act(&voices, Action::Press));
    ui.frame();
    assert_eq!(ui.get(Role::Menu, "Voices").role(), Role::Menu);
    let seven = ui.get(Role::MenuItem, "7");
    assert!(ui.act(&seven, Action::Press));
    assert_eq!(params.voices.value(), 7);
    ui.frame();
    // A switch's values are its labels.
    let mode = ui.get(Role::ComboBox, "Mode");
    ui.click(mode.center());
    ui.frame();
    assert!(ui.find(Role::MenuItem, "Off").is_some());
    let on = ui.get(Role::MenuItem, "On");
    // Acting on something outside the menu closes it first.
    assert!(ui.act(&voices, Action::Focus));
    ui.frame();
    assert!(ui.find(Role::MenuItem, "On").is_none());
    assert!(!ui.act(&on, Action::Press), "gone with its menu");
    assert!(!params.mono.value());
}

// A widget of one's own that opens a context menu with a submenu.

/// What the widget heard from its menus.
type Heard = Rc<RefCell<Vec<MenuEvent>>>;

struct Pad {
    heard: Heard,
}

fn pad_menu() -> Menu {
    Menu::new("Pad")
        .item(MenuItem::new(1, "Copy"))
        .item(MenuItem::new(2, "Paste").disabled())
        .item(MenuItem::separator())
        .item(MenuItem::submenu(
            "Colour",
            Menu::new("Colour")
                .item(MenuItem::new(10, "Red"))
                .item(MenuItem::new(11, "Green").checked(true))
                .item(MenuItem::submenu(
                    "More",
                    Menu::new("More").item(MenuItem::new(20, "Teal")),
                )),
        ))
        .item(MenuItem::new(3, "Delete"))
}

impl Widget for Pad {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Button, "Pad").with_actions(Actions::PRESS)
    }

    fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
        Size::new(60.0, 40.0)
    }

    fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene) {
        scene.fill(cx.rect(), cx.theme().palette.accent);
    }

    fn event(&mut self, cx: &mut EventCx<'_>, event: &Event) -> Handled {
        match *event {
            Event::PointerDown {
                at,
                button: manguebyte_ui::Button::Secondary,
                ..
            } => {
                cx.open_menu_at(pad_menu(), at);
                Handled::Yes
            }
            Event::Menu(result) => {
                self.heard.borrow_mut().push(result);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn act(&mut self, cx: &mut EventCx<'_>, action: Action) -> bool {
        if action == Action::Press {
            cx.open_menu(pad_menu());
            return true;
        }
        false
    }
}

fn pad() -> (Harness, Heard) {
    let heard: Heard = Rc::default();
    let view = row((Pad {
        heard: Rc::clone(&heard),
    },))
    .padding(20.0)
    .align(Align::Start);
    let mut ui = Harness::new(view, Size::new(400.0, 300.0), 1.25);
    ui.frame();
    (ui, heard)
}

fn right_click(ui: &mut Harness, at: Point) {
    ui.send(Event::PointerDown {
        at,
        button: manguebyte_ui::Button::Secondary,
        clicks: 1,
        modifiers: Modifiers::NONE,
    });
    ui.send(Event::PointerUp {
        at,
        button: manguebyte_ui::Button::Secondary,
        modifiers: Modifiers::NONE,
    });
}

#[test]
fn a_context_menu_opens_at_the_pointer_and_its_submenus_beside_their_items() {
    let (mut ui, heard) = pad();
    let at = ui.get(Role::Button, "Pad").center();
    right_click(&mut ui, at);
    ui.frame();
    let menu = ui.get(Role::Menu, "Pad");
    // At the pointer, to the nearest device pixel (at 1.25, 0.8 logical
    // pixels).
    let near = |a: f32, b: f32| (a - b).abs() <= 0.8;
    assert!(
        near(menu.bounds.x, at.x) && near(menu.bounds.y, at.y),
        "{menu:?} at {at:?}"
    );
    // The disabled item is shown, and is neither focused nor chosen.
    let paste = ui.get(Role::MenuItem, "Paste");
    assert!(paste.node.state.disabled);
    assert_eq!(ui.find_all(Role::Separator, "").len(), 1);
    ui.click(paste.center());
    assert!(ui.find(Role::Menu, "Pad").is_some());
    assert!(heard.borrow().is_empty());
    // The keys skip it.
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Copy")
    );
    ui.key(Key::Down);
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Colour")
    );
    // Right opens the submenu beside it, at its first item.
    ui.key(Key::Right);
    ui.frame();
    let colour = ui.get(Role::Menu, "Colour");
    let item = ui.get(Role::MenuItem, "Colour");
    assert_eq!(item.node.state.expanded, Some(true));
    assert!(colour.bounds.x >= menu.bounds.right() - 4.0);
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Red")
    );
    // And a level deeper.
    ui.key(Key::Up);
    ui.key(Key::Enter);
    ui.frame();
    assert!(ui.find(Role::Menu, "More").is_some());
    // Left closes a level, back to its item; Escape another.
    ui.key(Key::Left);
    ui.frame();
    assert!(ui.find(Role::Menu, "More").is_none());
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("More")
    );
    ui.send(key(Key::Escape));
    ui.frame();
    assert!(ui.find(Role::Menu, "Colour").is_none());
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Colour")
    );
    assert_eq!(
        ui.get(Role::MenuItem, "Colour").node.state.expanded,
        Some(false)
    );
    assert!(ui.render_fresh() == *ui.pixels());
    // The pointer: hovering opens the submenu, and a click in it chooses.
    ui.move_to(ui.get(Role::MenuItem, "Colour").center());
    ui.frame();
    ui.move_to(ui.get(Role::MenuItem, "More").center());
    ui.frame();
    let teal = ui.get(Role::MenuItem, "Teal");
    // Hovering another item of the outer menu closes the inner ones.
    ui.move_to(ui.get(Role::MenuItem, "Copy").center());
    ui.frame();
    assert!(ui.find(Role::Menu, "Colour").is_none());
    ui.move_to(ui.get(Role::MenuItem, "Colour").center());
    // A submenu is laid out, and so can be reached, from the next frame.
    ui.frame();
    ui.move_to(ui.get(Role::MenuItem, "More").center());
    ui.frame();
    ui.click(teal.center());
    assert_eq!(*heard.borrow(), [MenuEvent::Chosen(20)]);
    ui.frame();
    assert!(
        ui.everything()
            .iter()
            .all(|found| found.role() != Role::Menu)
    );
    assert!(ui.render_fresh() == *ui.pixels());
}

#[test]
fn moving_on_closes_a_submenu_and_the_choosing_key_is_kept_whole() {
    let (mut ui, heard) = pad();
    let pad = ui.get(Role::Button, "Pad");
    right_click(&mut ui, pad.center());
    ui.frame();
    // Hover opens the submenu without moving the keys into it.
    ui.move_to(ui.get(Role::MenuItem, "Colour").center());
    ui.frame();
    assert!(ui.find(Role::Menu, "Colour").is_some());
    // Down moves on in the outer menu, and the submenu closes.
    assert!(ui.key(Key::Down));
    ui.frame();
    assert!(ui.find(Role::Menu, "Colour").is_none());
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Delete")
    );
    // Enter chooses and closes; its release, which comes after, is not
    // left for the host either.
    let enter = |pressed| {
        Event::Key(KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::NONE,
            pressed,
        })
    };
    assert!(ui.send(enter(true)));
    assert_eq!(*heard.borrow(), [MenuEvent::Chosen(3)]);
    assert!(ui.send(enter(false)));
}

#[test]
fn a_menu_opened_again_closes_the_first_and_says_so() {
    let (mut ui, heard) = pad();
    let pad = ui.get(Role::Button, "Pad");
    assert!(ui.act(&pad, Action::Press));
    ui.frame();
    // Below the pad, at least as wide.
    let menu = ui.get(Role::Menu, "Pad");
    assert!(menu.bounds.y >= pad.bounds.bottom() - 0.5);
    // A press on the pad closes it; the press does nothing else.
    right_click(&mut ui, pad.center());
    assert_eq!(*heard.borrow(), [MenuEvent::Closed]);
    assert!(ui.find(Role::Menu, "Pad").is_none());
    right_click(&mut ui, pad.center());
    right_click(&mut ui, Point::new(5.0, 5.0));
    assert_eq!(*heard.borrow(), [MenuEvent::Closed, MenuEvent::Closed]);
    assert_eq!(ui.root().len(), 2, "the menus' widgets are gone");
}

#[test]
fn a_menu_at_the_far_corner_stays_inside() {
    let (mut ui, _heard) = pad();
    // The pad is at 20,20: a menu there fits. Move the pointer's point to
    // the bottom right through a wide resize, and open the menu there.
    ui.resize(Size::new(120.0, 400.0), 1.0);
    ui.frame();
    let pad = ui.get(Role::Button, "Pad");
    right_click(
        &mut ui,
        Point::new(pad.bounds.right() - 1.0, pad.bounds.bottom() - 1.0),
    );
    ui.frame();
    let menu = ui.get(Role::Menu, "Pad");
    assert!(menu.bounds.right() <= 120.0 + 0.5, "{menu:?}");
    assert!(menu.bounds.x >= 0.0);
    // Its submenu has no room on the right, and goes on the left.
    ui.key(Key::Down);
    ui.key(Key::Right);
    ui.frame();
    let colour = ui.get(Role::Menu, "Colour");
    assert!(colour.bounds.right() <= 120.0 + 0.5, "{colour:?}");
    assert!(ui.render_fresh() == *ui.pixels());
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn unit(&mut self) -> f32 {
        self.below(1 << 20) as f32 / (1 << 20) as f32
    }
}

/// Random input in random order over combo boxes, a context menu with
/// submenus, a knob and a field: nothing panics, every gesture is well
/// formed, the buffer is always a whole render, and at most one menu
/// opener is ever told it closed per menu.
#[test]
fn random_input_with_menus_open_keeps_every_promise() {
    const KEYS: [Key; 12] = [
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Home,
        Key::End,
        Key::Enter,
        Key::Escape,
        Key::Tab,
        Key::Character(' '),
        Key::Character('s'),
        Key::Function(4),
    ];
    let mut menus_seen = 0;
    let mut tips_seen = 0;
    for seed in 1..=6_u64 {
        let params = params();
        let heard: Heard = Rc::default();
        let name = manguebyte_ui::State::new(String::from("x"));
        let view = column((
            row((
                combo(ParamHandle::new(Arc::clone(&params), |p| &p.shape)).tooltip("The shape"),
                combo(ParamHandle::new(Arc::clone(&params), |p| &p.voices)),
                Pad {
                    heard: Rc::clone(&heard),
                }
                .tooltip("A pad with a menu"),
            ))
            .spacing(6.0),
            row((
                knob(ParamHandle::new(Arc::clone(&params), |p| &p.gain)).tooltip("The gain"),
                manguebyte_ui::text_field("Name", name.clone()),
            ))
            .tooltip("Gain and name"),
        ))
        .padding(6.0);
        let host = Rc::new(
            manguebyte_plugin::editor::testing::RecordingEditorHost::new()
                .with_params(Arc::clone(&params) as Arc<dyn manguebyte_params::ParamSet>),
        );
        let scale = [1.0, 1.25, 1.5][seed as usize % 3];
        let mut ui = Harness::with_host(view, Size::new(320.0, 220.0), scale, host.clone());
        ui.frame();
        let mut random = Random(0xC0FF_EE00 ^ seed.wrapping_mul(0x9E37_79B9));
        for step in 0..400 {
            let widgets = ui.everything();
            let at = if random.below(4) != 0 && !widgets.is_empty() {
                let w = &widgets[random.below(widgets.len() as u64) as usize];
                Point::new(
                    w.bounds.x + w.bounds.width * random.unit(),
                    w.bounds.y + w.bounds.height * random.unit(),
                )
            } else {
                Point::new(random.unit() * 340.0 - 10.0, random.unit() * 240.0 - 10.0)
            };
            let button = if random.below(4) == 0 {
                manguebyte_ui::Button::Secondary
            } else {
                manguebyte_ui::Button::Primary
            };
            match random.below(10) {
                0 | 1 => {
                    ui.send(Event::PointerDown {
                        at,
                        button,
                        clicks: 1 + random.below(2) as u32,
                        modifiers: Modifiers::NONE,
                    });
                }
                2 | 3 => {
                    ui.send(Event::PointerMove {
                        at,
                        modifiers: Modifiers::NONE,
                    });
                }
                4 => {
                    if random.below(4) == 0 {
                        ui.send(Event::PointerLeave);
                    } else {
                        ui.send(Event::PointerUp {
                            at,
                            button,
                            modifiers: Modifiers::NONE,
                        });
                    }
                }
                5 | 6 => {
                    let key = KEYS[random.below(KEYS.len() as u64) as usize];
                    ui.send(Event::Key(KeyEvent {
                        key,
                        modifiers: Modifiers::NONE,
                        pressed: random.below(3) != 0,
                    }));
                }
                7 => {
                    ui.send(Event::Text(['s', 'n', '1', 'é'][random.below(4) as usize]));
                }
                8 => {
                    if !widgets.is_empty() {
                        let widget = &widgets[random.below(widgets.len() as u64) as usize];
                        let action = [Action::Press, Action::Focus, Action::Increment]
                            [random.below(3) as usize];
                        ui.act(widget, action);
                    }
                }
                _ => {
                    if random.below(6) == 0 {
                        ui.send(Event::Focus(random.below(2) == 0));
                    }
                    // Now and then long enough for a tooltip.
                    ui.advance_ms(if random.below(5) == 0 { 600 } else { 16 });
                    if random.below(4) == 0 {
                        assert!(
                            ui.render_fresh() == *ui.pixels(),
                            "seed {seed}, step {step}: the buffer is not a whole render"
                        );
                    }
                }
            }
            if let Err(problem) = host.well_formed() {
                panic!("seed {seed}, step {step}: {problem}");
            }
            let everything = ui.everything();
            if everything.iter().any(|found| found.role() == Role::Menu) {
                menus_seen += 1;
            }
            let tips = everything
                .iter()
                .filter(|found| found.role() == Role::Tooltip)
                .count();
            assert!(tips <= 1, "seed {seed}, step {step}: {tips} tooltips");
            tips_seen += tips;
        }
        ui.close();
        assert_eq!(host.well_formed(), Ok(()), "seed {seed}");
    }
    eprintln!(
        "a menu was open after {menus_seen} of 2400 steps, a tooltip shown after {tips_seen}"
    );
    assert!(menus_seen > 0);
    assert!(tips_seen > 0);
}
