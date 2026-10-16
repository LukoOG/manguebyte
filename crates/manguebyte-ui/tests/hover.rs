//! Hover and tooltips: which widget the pointer rests on, and the help
//! text a view shows after a while.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use manguebyte_gfx::{Point, Scene, Size};
use manguebyte_ui::access::AccessNode;
use manguebyte_ui::layout::Align;
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{
    AccessCx, Event, EventCx, IntoView, Key, MeasureCx, Modifiers, PaintCx, Role, TOOLTIP_DELAY,
    Widget, column, label, row,
};

/// A square that logs when the pointer comes and goes, and draws itself
/// lighter while it is under it.
struct Spot {
    name: &'static str,
    log: Rc<RefCell<Vec<(&'static str, bool)>>>,
}

impl Widget for Spot {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Image, self.name)
    }

    fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
        Size::new(60.0, 40.0)
    }

    fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene) {
        let palette = &cx.theme().palette;
        let color = if cx.is_hovered() {
            palette.accent
        } else {
            palette.track
        };
        scene.fill(cx.rect(), color);
    }

    fn hover_changed(&mut self, cx: &mut EventCx<'_>, hovered: bool) {
        self.log.borrow_mut().push((self.name, hovered));
        cx.request_paint();
    }
}

type Log = Rc<RefCell<Vec<(&'static str, bool)>>>;

fn spots() -> (Harness, Log) {
    let log: Log = Rc::default();
    let spot = |name| Spot {
        name,
        log: Rc::clone(&log),
    };
    let view = column((
        row((
            spot("Left").tooltip("The left one"),
            spot("Right").tooltip("The right one, with a longer text"),
        ))
        .spacing(10.0),
        label("Plain"),
    ))
    .spacing(10.0)
    .padding(10.0)
    .align(Align::Start);
    let mut ui = Harness::new(view, Size::new(240.0, 160.0), 1.5);
    ui.frame();
    (ui, log)
}

fn tooltip(ui: &Harness) -> Option<String> {
    ui.everything()
        .into_iter()
        .find(|found| found.role() == Role::Tooltip)
        .map(|found| found.name().to_owned())
}

#[test]
fn the_widget_under_the_pointer_hears_it_come_and_go() {
    let (mut ui, log) = spots();
    let left = ui.get(Role::Image, "Left");
    let right = ui.get(Role::Image, "Right");
    ui.move_to(left.center());
    // Drawn lighter while hovered: only the widget that changed is drawn.
    assert_eq!(ui.frame().painted, 1);
    ui.move_to(left.center() + Point::new(3.0, 0.0));
    assert_eq!(ui.frame().painted, 0, "still the same widget");
    ui.move_to(right.center());
    assert_eq!(ui.frame().painted, 2);
    assert!(ui.render_fresh() == *ui.pixels());
    ui.send(Event::PointerLeave);
    assert_eq!(
        *log.borrow(),
        [
            ("Left", true),
            ("Left", false),
            ("Right", true),
            ("Right", false)
        ]
    );
    assert!(ui.root().hovered().is_none());
    // While a button is held the hovered widget does not change.
    ui.move_to(left.center());
    log.borrow_mut().clear();
    ui.press(left.center());
    ui.move_to(right.center());
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    ui.release(right.center());
    assert_eq!(*log.borrow(), [("Left", false), ("Right", true)]);
}

#[test]
fn a_tooltip_shows_after_the_pointer_rests_and_goes_when_it_leaves() {
    let (mut ui, _log) = spots();
    let left = ui.get(Role::Image, "Left");
    // The help text is the widget's description for assistive technology,
    // shown or not.
    assert_eq!(left.node.description.as_deref(), Some("The left one"));
    ui.move_to(left.center());
    let frame = ui.frame();
    assert!(frame.animating, "waiting for the delay asks for frames");
    ui.advance(TOOLTIP_DELAY - Duration::from_millis(20));
    assert_eq!(tooltip(&ui), None, "not before the delay");
    // Moving starts the wait again, until it shows.
    ui.move_to(left.center() + Point::new(2.0, 1.0));
    ui.advance(TOOLTIP_DELAY - Duration::from_millis(20));
    assert_eq!(tooltip(&ui), None);
    ui.advance_ms(40);
    assert_eq!(tooltip(&ui).as_deref(), Some("The left one"));
    let tip = ui
        .everything()
        .into_iter()
        .find(|found| found.role() == Role::Tooltip)
        .expect("shown");
    // Below the pointer, inside the interface.
    assert!(tip.bounds.y > left.center().y);
    assert!(tip.bounds.right() <= 240.0 + 0.5);
    assert!(ui.render_fresh() == *ui.pixels());
    // Shown, it asks for nothing more, and moving on the same widget keeps
    // it.
    assert!(!ui.frame().animating);
    ui.move_to(left.center() + Point::new(-4.0, 3.0));
    ui.frame();
    assert_eq!(tooltip(&ui).as_deref(), Some("The left one"));
    assert_eq!(ui.frame().damaged, 0);
    // Onto the other widget: this one goes, the other waits for its delay.
    let right = ui.get(Role::Image, "Right");
    ui.move_to(right.center());
    ui.frame();
    assert_eq!(tooltip(&ui), None);
    ui.advance(TOOLTIP_DELAY + Duration::from_millis(20));
    assert_eq!(
        tooltip(&ui).as_deref(),
        Some("The right one, with a longer text")
    );
    // Off every view with a tooltip: gone, and what it covered drawn
    // again.
    let plain = ui.get(Role::Label, "Plain");
    ui.move_to(plain.center());
    ui.frame();
    assert_eq!(tooltip(&ui), None);
    assert!(ui.render_fresh() == *ui.pixels());
    ui.send(Event::PointerLeave);
    ui.advance(TOOLTIP_DELAY * 2);
    assert_eq!(tooltip(&ui), None);
}

#[test]
fn a_press_or_a_key_takes_the_tooltip_away_until_the_pointer_comes_back() {
    let (mut ui, _log) = spots();
    let left = ui.get(Role::Image, "Left");
    let right = ui.get(Role::Image, "Right");
    ui.move_to(left.center());
    ui.advance(TOOLTIP_DELAY + Duration::from_millis(20));
    assert!(tooltip(&ui).is_some());
    ui.click(left.center());
    ui.frame();
    assert_eq!(tooltip(&ui), None);
    // Resting on it again shows nothing...
    ui.move_to(left.center() + Point::new(1.0, 0.0));
    ui.advance(TOOLTIP_DELAY * 2);
    assert_eq!(tooltip(&ui), None);
    // ...until the pointer has been elsewhere.
    ui.move_to(right.center());
    ui.move_to(left.center());
    ui.advance(TOOLTIP_DELAY + Duration::from_millis(20));
    assert!(tooltip(&ui).is_some());
    ui.key(Key::Tab);
    ui.frame();
    assert_eq!(tooltip(&ui), None);
    // The window losing the keyboard takes it too.
    ui.move_to(right.center());
    ui.advance(TOOLTIP_DELAY + Duration::from_millis(20));
    assert!(tooltip(&ui).is_some());
    ui.send(Event::Focus(false));
    ui.frame();
    assert_eq!(tooltip(&ui), None);
    assert!(ui.render_fresh() == *ui.pixels());
    let _ = Modifiers::NONE;
}

#[test]
fn a_widget_inside_a_view_with_a_tooltip_shows_the_views() {
    let log: Log = Rc::default();
    let view = row((column((
        label("Cutoff"),
        Spot {
            name: "Dial",
            log: Rc::clone(&log),
        },
    ))
    .named("Filter")
    .tooltip("Where the filter starts to cut"),))
    .padding(4.0)
    .align(Align::Start);
    let mut ui = Harness::new(view, Size::new(200.0, 120.0), 1.0);
    ui.frame();
    let dial = ui.get(Role::Image, "Dial");
    ui.move_to(dial.center());
    ui.advance(TOOLTIP_DELAY + Duration::from_millis(20));
    assert_eq!(
        tooltip(&ui).as_deref(),
        Some("Where the filter starts to cut")
    );
    // The group says it too; the widgets inside it do not repeat it.
    assert_eq!(
        ui.get(Role::Group, "Filter").node.description.as_deref(),
        Some("Where the filter starts to cut")
    );
    assert_eq!(dial.node.description, None);
    ui.assert_accessible();
}
