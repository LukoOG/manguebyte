//! Combo boxes and switches bound to a `State` instead of a parameter: a
//! list that changes while the interface runs, the keys, the wheel,
//! assistive technology, and children shown from the front of a list.

use manguebyte_gfx::Size;
use manguebyte_ui::layout::Style;
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{
    AccessCx, AccessNode, Action, Choices, ComboBox, IntoView, Key, MeasureCx, Modifiers, PaintCx,
    Role, Shown, State, Toggle, UpdateCx, View, Widget, column, label,
};

fn devices(selected: Option<usize>) -> State<Choices> {
    State::new(Choices::new(
        ["Speakers", "Headphones", "USB Interface"],
        selected,
    ))
}

fn open(view: impl IntoView) -> Harness {
    let mut ui = Harness::new(view, Size::new(240.0, 260.0), 1.0);
    ui.frame();
    ui
}

fn menu_items(ui: &Harness) -> Vec<String> {
    ui.everything()
        .into_iter()
        .filter(|found| found.role() == Role::MenuItem)
        .map(|found| found.name().to_owned())
        .collect()
}

#[test]
fn a_combo_box_lists_its_choices_and_a_click_sets_the_state_once() {
    let choices = devices(Some(0));
    let mut ui = open(column((ComboBox::from_state("Output", choices.clone()),)));
    let output = ui.get(Role::ComboBox, "Output");
    assert_eq!(output.value_text(), Some("Speakers"));
    assert_eq!(output.node.state.expanded, Some(false));
    ui.assert_accessible();

    ui.click(output.center());
    ui.frame();
    assert_eq!(menu_items(&ui), ["Speakers", "Headphones", "USB Interface"]);
    let speakers = ui.get(Role::MenuItem, "Speakers");
    assert_eq!(speakers.node.state.checked, Some(true));
    assert!(speakers.is_focused());
    ui.assert_accessible();

    let before = choices.version();
    ui.click(ui.get(Role::MenuItem, "USB Interface").center());
    assert_eq!(choices.get().selected, Some(2));
    assert_eq!(choices.version(), before + 1, "set once");
    ui.frame();
    let output = ui.get(Role::ComboBox, "Output");
    assert_eq!(output.value_text(), Some("USB Interface"));
    assert!(output.is_focused(), "the focus came back");
    // Nothing was sent to a host: no parameter is involved.
    assert!(ui.take_calls().is_empty());
}

#[test]
fn choosing_what_is_chosen_already_sets_nothing() {
    let choices = devices(Some(1));
    let mut ui = open(ComboBox::from_state("Output", choices.clone()));
    ui.click(ui.get(Role::ComboBox, "Output").center());
    ui.frame();
    let before = choices.version();
    ui.click(ui.get(Role::MenuItem, "Headphones").center());
    assert_eq!(choices.version(), before);
}

#[test]
fn the_keys_and_the_wheel_step_through_the_choices() {
    let choices = devices(Some(0));
    let mut ui = open(ComboBox::from_state("Output", choices.clone()));
    let output = ui.get(Role::ComboBox, "Output");
    assert!(ui.act(&output, Action::Focus));
    assert!(ui.key(Key::Down));
    assert_eq!(choices.get().selected, Some(1));
    assert!(ui.key(Key::End));
    assert_eq!(choices.get().selected, Some(2));
    // Past the end stays there, and sets nothing.
    let before = choices.version();
    assert!(ui.key(Key::Down));
    assert_eq!(choices.version(), before);
    assert!(ui.key(Key::Home));
    assert_eq!(choices.get().selected, Some(0));
    ui.frame();
    // Down the wheel, down the list.
    assert!(ui.wheel(output.center(), -1.0));
    assert_eq!(choices.get().selected, Some(1));
    assert!(ui.wheel(output.center(), 1.0));
    assert_eq!(choices.get().selected, Some(0));
    // Alt+Down opens the menu, and Escape closes it with nothing chosen.
    assert!(ui.key_with(
        Key::Down,
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        }
    ));
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Output").node.state.expanded,
        Some(true)
    );
    let before = choices.version();
    assert!(ui.key(Key::Escape));
    ui.frame();
    assert_eq!(choices.version(), before);
    assert_eq!(
        ui.get(Role::ComboBox, "Output").node.state.expanded,
        Some(false)
    );
}

#[test]
fn with_nothing_chosen_it_shows_its_placeholder_and_the_keys_start_at_an_end() {
    let choices = devices(None);
    let mut ui = open(ComboBox::from_state("Output", choices.clone()).placeholder("No device"));
    let output = ui.get(Role::ComboBox, "Output");
    assert_eq!(output.value_text(), Some("No device"));
    ui.act(&output, Action::Focus);
    assert!(ui.key(Key::Down));
    assert_eq!(
        choices.get().selected,
        Some(0),
        "down from before the first"
    );
    choices.update(|c| c.selected = None);
    ui.frame();
    assert!(ui.key(Key::Up));
    ui.frame();
    assert_eq!(choices.get().selected, Some(2), "up from after the last");
    // A choice past the end of the list counts as nothing chosen.
    choices.update(|c| c.selected = Some(7));
    ui.frame();
    assert_eq!(
        ui.get(Role::ComboBox, "Output").value_text(),
        Some("No device")
    );
}

#[test]
fn an_empty_list_opens_no_menu() {
    let choices = State::new(Choices::default());
    let mut ui = open(ComboBox::from_state("Output", choices.clone()));
    let output = ui.get(Role::ComboBox, "Output");
    assert_eq!(output.value_text(), Some("None"));
    assert!(!ui.act(&output, Action::Press));
    ui.click(output.center());
    ui.frame();
    assert!(ui.find(Role::Menu, "Output").is_none());
    assert!(ui.key(Key::Down), "the key is still the combo box's");
    assert_eq!(choices.get(), Choices::default(), "nothing was set");
}

#[test]
fn a_new_list_is_shown_and_laid_out_again() {
    let choices = State::new(Choices::new(["A"], Some(0)));
    let mut ui = open(column((
        ComboBox::from_state("Output", choices.clone()),
        label("Below"),
    )));
    let narrow = ui.get(Role::ComboBox, "Output").bounds;
    choices.set(Choices::new(["A", "A much longer device name"], Some(1)));
    let summary = ui.frame();
    assert!(summary.laid_out, "the box is as wide as its widest item");
    let output = ui.get(Role::ComboBox, "Output");
    assert_eq!(output.value_text(), Some("A much longer device name"));
    assert!(output.bounds.width >= narrow.width);
    assert!(
        ui.access_updates()
            .iter()
            .any(|found| found.role() == Role::ComboBox),
        "assistive technology hears of the new value"
    );
    // The same list again draws nothing.
    choices.set(choices.get());
    assert_eq!(ui.frame().damaged, 0);
}

#[test]
fn a_choice_from_a_menu_opened_before_the_list_changed_is_found_by_its_text() {
    let choices = devices(Some(0));
    let mut ui = open(ComboBox::from_state("Output", choices.clone()));
    ui.click(ui.get(Role::ComboBox, "Output").center());
    ui.frame();
    let usb = ui.get(Role::MenuItem, "USB Interface");
    // The headphones go away while the menu is open.
    choices.set(Choices::new(["Speakers", "USB Interface"], Some(0)));
    ui.click(usb.center());
    assert_eq!(choices.get().chosen_text(), Some("USB Interface"));
    assert_eq!(choices.get().selected, Some(1));
}

#[test]
fn a_choice_from_a_menu_whose_item_went_away_is_dropped() {
    let choices = devices(Some(0));
    let mut ui = open(ComboBox::from_state("Output", choices.clone()));
    ui.click(ui.get(Role::ComboBox, "Output").center());
    ui.frame();
    let usb = ui.get(Role::MenuItem, "USB Interface");
    choices.set(Choices::new(["Speakers", "Headphones"], Some(0)));
    let before = choices.version();
    ui.click(usb.center());
    assert_eq!(choices.version(), before);
    assert_eq!(choices.get().selected, Some(0));
}

#[test]
fn assistive_technology_opens_the_menu() {
    let choices = devices(Some(0));
    let mut ui = open(ComboBox::from_state("Output", choices.clone()));
    let output = ui.get(Role::ComboBox, "Output");
    assert!(output.node.actions.press);
    assert!(ui.act(&output, Action::Press));
    ui.frame();
    assert!(ui.find(Role::Menu, "Output").is_some());
    ui.assert_accessible();
}

#[test]
fn a_switch_bound_to_a_state_turns_it_over() {
    let open_port = State::new(false);
    let mut ui = open(Toggle::from_state("Keys", open_port.clone()));
    let keys = ui.get(Role::Switch, "Keys");
    assert_eq!(keys.node.state.checked, Some(false));
    assert_eq!(keys.value_text(), Some("Off"));
    assert!(keys.node.actions.press);
    ui.assert_accessible();
    ui.click(keys.center());
    assert!(open_port.get());
    ui.frame();
    let keys = ui.get(Role::Switch, "Keys");
    assert_eq!(keys.node.state.checked, Some(true));
    assert_eq!(keys.value_text(), Some("On"));
    // A click leaves Space to the host; Tab gives the focus.
    assert!(!ui.key(Key::Character(' ')));
    assert!(ui.tab());
    assert!(ui.key(Key::Character(' ')));
    assert!(!open_port.get());
    assert!(ui.key(Key::Enter));
    assert!(open_port.get());
    assert!(ui.act(&keys, Action::Press));
    assert!(!open_port.get());
    assert!(ui.take_calls().is_empty());
}

#[test]
fn a_switch_follows_its_state_and_its_name() {
    let (on, name) = (State::new(false), State::new(String::from("Keys")));
    let mut ui = open(Toggle::from_state(name.clone(), on.clone()));
    on.set(true);
    ui.frame();
    assert_eq!(ui.get(Role::Switch, "Keys").node.state.checked, Some(true));
    assert!(
        ui.access_updates()
            .iter()
            .any(|found| found.role() == Role::Switch)
    );
    name.set(String::from("Pads"));
    ui.frame();
    assert!(ui.find(Role::Switch, "Pads").is_some());
    // Settled, nothing is drawn.
    ui.advance_ms(1_000);
    assert_eq!(ui.frame().damaged, 0);
}

/// Shows the first `count` of its children.
struct Front(State<usize>);

impl Widget for Front {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Group, "")
    }
    fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
        Size::default()
    }
    fn paint(&mut self, _cx: &PaintCx<'_>, _scene: &mut manguebyte_gfx::Scene) {}
    fn update(&mut self, cx: &mut UpdateCx<'_>) {
        cx.show(Shown::First(self.0.get()));
    }
}

#[test]
fn a_container_can_show_the_first_of_its_children_and_spaces_only_those() {
    let count = State::new(1_usize);
    let view = column((
        View::with_children(
            Front(count.clone()),
            Style::column().with_spacing(10.0),
            (label("One"), label("Two"), label("Three")),
        ),
        label("After"),
    ));
    let mut ui = open(view);
    assert!(ui.find(Role::Label, "One").is_some());
    assert!(ui.find(Role::Label, "Two").is_none());
    let one = ui.get(Role::Label, "One").bounds;
    let after = ui.get(Role::Label, "After").bounds;
    assert!(
        (after.y - one.bottom()).abs() < 0.5,
        "no spacing for the hidden children: {one:?} {after:?}"
    );
    count.set(3);
    ui.frame();
    let names: Vec<String> = ui
        .everything()
        .into_iter()
        .filter(|found| found.role() == Role::Label)
        .map(|found| found.name().to_owned())
        .collect();
    assert_eq!(names, ["One", "Two", "Three", "After"]);
    let (two, three) = (
        ui.get(Role::Label, "Two").bounds,
        ui.get(Role::Label, "Three").bounds,
    );
    assert!((three.y - two.bottom() - 10.0).abs() < 0.5);
    // Past the end shows them all; none shows nothing.
    count.set(9);
    ui.frame();
    assert!(ui.find(Role::Label, "Three").is_some());
    count.set(0);
    ui.frame();
    assert!(ui.find(Role::Label, "One").is_none());
    assert!(ui.get(Role::Label, "After").bounds.y < 1.0);
    assert!(Shown::First(2).includes(1) && !Shown::First(2).includes(2));
}
