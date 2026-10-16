//! Typing: the text field, and a value typed into a knob or a slider.

mod common;

use std::sync::Arc;

use manguebyte_gfx::{Point, Size};
use manguebyte_params::{ChoiceParam, Param, param_set};
use manguebyte_plugin::editor::testing::HostCall::{BeginGesture, EndGesture, Perform};
use manguebyte_ui::testing::{Found, Harness};
use manguebyte_ui::{
    Composition, Event, IntoView, Key, KeyEvent, Modifiers, ParamHandle, Role, State, column, knob,
    row, text_field,
};

use common::{PANEL, levels, panel_view, params};

const SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::NONE
};
const CONTROL: Modifiers = Modifiers {
    control: true,
    ..Modifiers::NONE
};
const SHIFT_CONTROL: Modifiers = Modifiers {
    shift: true,
    control: true,
    ..Modifiers::NONE
};

/// Two fields, one named "Name" over "Notes", in a column.
fn two_fields(name: &State<String>, notes: &State<String>) -> Harness {
    let view = column((
        text_field("Name", name.clone()),
        text_field("Notes", notes.clone()),
    ))
    .spacing(4.0)
    .padding(4.0);
    let mut ui = Harness::new(view, Size::new(200.0, 80.0), 1.0);
    ui.frame();
    ui
}

fn value(ui: &Harness, name: &str) -> String {
    ui.get(Role::TextInput, name)
        .value_text()
        .unwrap_or_default()
        .to_owned()
}

/// A point inside `field`, just right of its left edge or left of its
/// right one.
fn left_of(field: &Found) -> Point {
    Point::new(field.bounds.x + 7.0, field.bounds.center().y)
}

fn right_of(field: &Found) -> Point {
    Point::new(field.bounds.right() - 7.0, field.bounds.center().y)
}

#[test]
fn typing_edits_a_copy_that_enter_takes_and_escape_throws_away() {
    let name = State::new(String::from("Pad"));
    let notes = State::new(String::new());
    let mut ui = two_fields(&name, &notes);
    let field = ui.get(Role::TextInput, "Name");
    assert_eq!(field.value_text(), Some("Pad"));
    assert!(field.node.state.editable);
    ui.assert_accessible();

    // A click at the right places the caret at the end.
    ui.click(right_of(&field));
    assert!(ui.root().wants_text());
    assert!(ui.type_text(" 2"));
    assert_eq!(value(&ui, "Name"), "Pad 2");
    assert_eq!(name.get(), "Pad", "nothing taken before Enter");
    assert!(ui.key(Key::Enter));
    assert_eq!(name.get(), "Pad 2");

    // Escape puts the state's text back.
    ui.type_text("!!");
    assert_eq!(value(&ui, "Name"), "Pad 2!!");
    assert!(ui.key(Key::Escape));
    assert_eq!(value(&ui, "Name"), "Pad 2");
    assert_eq!(name.get(), "Pad 2");
    // With nothing to throw away, Escape is the host's.
    assert!(!ui.key(Key::Escape));
}

#[test]
fn losing_the_focus_takes_the_edit() {
    let name = State::new(String::from("A"));
    let notes = State::new(String::from("B"));
    let mut ui = two_fields(&name, &notes);
    ui.click(right_of(&ui.get(Role::TextInput, "Name")));
    ui.type_text("1");
    // Tab goes on to the next field, which takes the focus with everything
    // selected, and the first takes its edit.
    assert!(ui.tab());
    assert_eq!(name.get(), "A1");
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Notes")
    );
    ui.type_text("C");
    assert_eq!(value(&ui, "Notes"), "C", "typing replaced the selection");
    // A press outside every field takes the focus away.
    ui.click(Point::new(199.0, 79.0));
    assert_eq!(notes.get(), "C");
    assert!(!ui.root().wants_text());
    // So does the window losing the keyboard.
    ui.click(right_of(&ui.get(Role::TextInput, "Name")));
    ui.type_text("2");
    ui.send(Event::Focus(false));
    assert_eq!(name.get(), "A12");
}

#[test]
fn the_caret_moves_by_cluster_and_by_word_and_shift_selects() {
    let text = State::new(String::from("gain -6 dB"));
    let mut ui = Harness::new(
        text_field("Gain", text.clone()),
        Size::new(200.0, 30.0),
        1.0,
    );
    ui.frame();
    let field = ui.get(Role::TextInput, "Gain");
    ui.click(right_of(&field));
    // Control+Left twice: before "dB", then before "6".
    ui.key_with(Key::Left, CONTROL);
    ui.key_with(Key::Left, CONTROL);
    ui.key_with(Key::Right, SHIFT);
    ui.key_with(Key::Character('c'), CONTROL);
    assert_eq!(ui.clipboard().text().as_deref(), Some("6"));
    // Shift+Control+Home selects from where the selection began back to
    // the start.
    ui.key_with(Key::Home, SHIFT_CONTROL);
    ui.key_with(Key::Character('x'), CONTROL);
    assert_eq!(ui.clipboard().text().as_deref(), Some("gain -"));
    assert_eq!(value(&ui, "Gain"), "6 dB");
    ui.key(Key::End);
    ui.key_with(Key::Character('v'), CONTROL);
    assert_eq!(value(&ui, "Gain"), "6 dBgain -");
    // Control+Backspace takes a word, or a run of other characters, and
    // the space before; Backspace a character.
    ui.key_with(Key::Backspace, CONTROL);
    assert_eq!(value(&ui, "Gain"), "6 dBgain ");
    ui.key_with(Key::Backspace, CONTROL);
    assert_eq!(value(&ui, "Gain"), "6 ");
    ui.key(Key::Backspace);
    ui.key(Key::Home);
    ui.key(Key::Delete);
    assert_eq!(value(&ui, "Gain"), "");
    // Control+A, then typing, replaces everything.
    ui.key_with(Key::Character('a'), CONTROL);
    ui.type_text("é👍🏽");
    assert_eq!(value(&ui, "Gain"), "é👍🏽");
    // A thumb and its skin tone go as one.
    ui.key(Key::Backspace);
    assert_eq!(value(&ui, "Gain"), "é");
    // An e with a combining accent goes as one too.
    ui.type_text("e\u{301}");
    ui.key(Key::Left);
    ui.key(Key::Delete);
    assert_eq!(value(&ui, "Gain"), "é");
}

#[test]
fn the_pointer_places_the_caret_and_selects() {
    let text = State::new(String::from("low shelf"));
    let mut ui = Harness::new(
        text_field("Band", text.clone()),
        Size::new(200.0, 30.0),
        1.0,
    );
    ui.frame();
    let field = ui.get(Role::TextInput, "Band");
    // A drag from the left edge to the right one selects everything.
    ui.press(left_of(&field));
    ui.move_to(right_of(&field));
    ui.release(right_of(&field));
    ui.frame();
    ui.key_with(Key::Character('c'), CONTROL);
    assert_eq!(ui.clipboard().text().as_deref(), Some("low shelf"));
    // A double click on the first word selects it.
    ui.double_click(left_of(&field));
    ui.key_with(Key::Character('c'), CONTROL);
    assert_eq!(ui.clipboard().text().as_deref(), Some("low"));
    // Shift and a click extend from the caret.
    ui.click(left_of(&field));
    ui.set_modifiers(SHIFT);
    ui.click(right_of(&field));
    ui.set_modifiers(Modifiers::NONE);
    ui.key_with(Key::Insert, CONTROL);
    assert_eq!(ui.clipboard().text().as_deref(), Some("low shelf"));
}

#[test]
fn keys_that_type_are_kept_from_the_host_and_shortcuts_are_not() {
    let text = State::new(String::new());
    let mut ui = Harness::new(text_field("Tag", text.clone()), Size::new(200.0, 30.0), 1.0);
    ui.frame();
    // Without the focus, a letter and the space bar are the host's.
    assert!(!ui.key(Key::Character('r')));
    assert!(!ui.key(Key::Character(' ')));
    assert!(ui.tab());
    assert!(ui.key(Key::Character('r')), "the field keeps a letter");
    assert!(ui.key(Key::Character(' ')), "and the space bar");
    assert!(ui.send(Event::Text('r')));
    // Control with a key the field has no use for, function keys and Tab
    // go on.
    assert!(!ui.key_with(Key::Character('s'), CONTROL));
    assert!(!ui.key(Key::Function(5)));
    assert_eq!(value(&ui, "Tag"), "r");
}

#[test]
fn filters_validation_and_limits_shape_what_goes_in() {
    let bpm = State::new(String::from("120"));
    let view = text_field("Tempo", bpm.clone())
        .filter(|c| c.is_ascii_digit() || c == '.')
        .validate(|text| {
            text.parse::<f64>()
                .is_ok_and(|v| (20.0..=999.0).contains(&v))
        })
        .max_chars(5);
    let mut ui = Harness::new(view, Size::new(120.0, 30.0), 1.0);
    ui.frame();
    assert!(ui.tab());
    // Pasted text is filtered and cut to the limit.
    ui.clipboard().set_text(Some("12a3.45\n67"));
    ui.key_with(Key::Character('v'), CONTROL);
    assert_eq!(value(&ui, "Tempo"), "123.4");
    // Enter on a text the validation refuses keeps the edit open.
    ui.key_with(Key::Character('a'), CONTROL);
    ui.type_text("5");
    assert!(ui.key(Key::Enter));
    assert_eq!(bpm.get(), "120");
    assert_eq!(value(&ui, "Tempo"), "5");
    assert!(ui.root().wants_text());
    ui.type_text("0");
    assert!(ui.key(Key::Enter));
    assert_eq!(bpm.get(), "50");
    // Losing the focus with a text it refuses throws the edit away.
    ui.key(Key::Backspace);
    ui.send(Event::Focus(false));
    assert_eq!(bpm.get(), "50");
    assert_eq!(value(&ui, "Tempo"), "50");
}

#[test]
fn the_state_shows_through_until_an_edit_starts() {
    let name = State::new(String::from("One"));
    let notes = State::new(String::new());
    let mut ui = two_fields(&name, &notes);
    name.set(String::from("Two"));
    ui.frame();
    assert_eq!(value(&ui, "Name"), "Two");
    // Focused but not changed: it still follows.
    ui.click(right_of(&ui.get(Role::TextInput, "Name")));
    name.set(String::from("Three"));
    ui.frame();
    assert_eq!(value(&ui, "Name"), "Three");
    // Changed: the edit stays until it is taken.
    ui.type_text("!");
    name.set(String::from("Four"));
    ui.frame();
    assert_eq!(value(&ui, "Name"), "Three!");
    ui.key(Key::Enter);
    assert_eq!(name.get(), "Three!");
}

#[test]
fn a_composition_is_shown_at_the_caret_and_typed_when_it_ends() {
    let text = State::new(String::from("ab"));
    let mut ui = Harness::new(
        text_field("Word", text.clone()),
        Size::new(200.0, 30.0),
        1.0,
    );
    ui.frame();
    ui.click(right_of(&ui.get(Role::TextInput, "Word")));
    ui.key(Key::Left);
    ui.frame();
    let before = ui.dump();
    for step in [
        Composition::Start,
        Composition::Clear,
        Composition::Push('か'),
        Composition::Push('な'),
        Composition::Caret(1),
    ] {
        assert!(ui.send(Event::Composition(step)));
    }
    ui.frame();
    // Drawn, marked, between "a" and "b", but not part of the text.
    assert_ne!(ui.dump(), before);
    assert_eq!(value(&ui, "Word"), "ab");
    assert!(ui.render_fresh() == *ui.pixels());
    // While composing, keys belong to the input method.
    assert!(ui.key(Key::Backspace));
    assert_eq!(value(&ui, "Word"), "ab");
    ui.send(Event::Composition(Composition::End));
    ui.send(Event::Text('仮'));
    ui.send(Event::Text('名'));
    assert_eq!(value(&ui, "Word"), "a仮名b");
}

#[test]
fn typing_draws_only_the_field_and_the_buffer_stays_exact() {
    let name = State::new(String::from("x"));
    let notes = State::new(String::new());
    let mut ui = two_fields(&name, &notes);
    let field = ui.get(Role::TextInput, "Name");
    ui.click(right_of(&field));
    ui.frame();
    for c in "a long name that scrolls past the end of the field".chars() {
        ui.type_text(&c.to_string());
        let frame = ui.frame();
        assert_eq!(frame.painted, 1);
        let damage = ui.last_damage_bounds().expect("drawn");
        assert!(field.device.contains_rect(&damage), "{damage:?}");
    }
    assert!(ui.render_fresh() == *ui.pixels());
    let caret = ui.root().caret().expect("a caret while typing");
    assert!(
        field.bounds.contains(caret.center()),
        "{caret:?} scrolled into the field"
    );
    // Nothing changed: nothing drawn, and the caret does not blink.
    assert_eq!(ui.frame().damaged, 0);
    ui.advance_ms(2000);
    assert_eq!(ui.frames_rendered_during(|ui| ui.advance_ms(1000)), 0);
}

// Values typed into knobs and sliders.

fn panel() -> (Arc<common::PanelParams>, Harness) {
    let params = params();
    let mut ui = Harness::with_params(
        panel_view(&params, &levels()),
        PANEL,
        1.0,
        Arc::clone(&params) as Arc<dyn manguebyte_params::ParamSet>,
    );
    ui.frame();
    (params, ui)
}

#[test]
fn a_double_click_on_a_knob_types_a_value_sent_as_one_edit() {
    let (params, mut ui) = panel();
    let knob = ui.get(Role::Slider, "Cutoff");
    let id = params.cutoff.param_id();
    ui.double_click(knob.center());
    // The first click was a gesture of its own; typing has sent nothing.
    assert_eq!(ui.take_calls(), [BeginGesture(id), EndGesture(id)]);
    ui.frame();
    let entry = ui.get(Role::TextInput, "Cutoff");
    assert_eq!(entry.value_text(), Some("1000 Hz"));
    assert!(entry.is_focused() && entry.node.state.editable);
    assert!(ui.root().wants_text());
    ui.assert_accessible();
    // The value is selected: typing replaces it.
    ui.type_text("250");
    assert!(ui.take_calls().is_empty());
    assert!(ui.key(Key::Enter));
    let expected = params.cutoff.plain_to_normalized(250.0);
    assert_eq!(
        ui.take_calls(),
        [BeginGesture(id), Perform(id, expected), EndGesture(id)]
    );
    assert!((params.cutoff.value() - 250.0).abs() < 1e-3);
    ui.frame();
    // A knob again, with the new value, still focused.
    let knob = ui.get(Role::Slider, "Cutoff");
    assert_eq!(knob.value_text(), Some("250 Hz"));
    assert!(knob.is_focused());
    assert!(!ui.root().wants_text());
}

#[test]
fn a_value_that_does_not_read_keeps_the_box_open_and_escape_sends_nothing() {
    let (params, mut ui) = panel();
    ui.tab();
    assert_eq!(
        ui.focused().map(|f| f.name().to_owned()).as_deref(),
        Some("Bypass")
    );
    ui.tab();
    // Enter on a focused knob opens the value too.
    assert!(ui.key(Key::Enter));
    ui.frame();
    let entry = ui.get(Role::TextInput, "Cutoff");
    assert!(entry.is_focused());
    ui.type_text("loud");
    assert!(ui.key(Key::Enter));
    ui.frame();
    assert_eq!(ui.get(Role::TextInput, "Cutoff").value_text(), Some("loud"));
    assert!(ui.take_calls().is_empty());
    // With and without the unit, in any case.
    ui.key_with(Key::Character('a'), CONTROL);
    ui.type_text("400 hz");
    ui.key(Key::Escape);
    assert!(ui.take_calls().is_empty(), "Escape sends nothing");
    ui.frame();
    assert_eq!(ui.get(Role::Slider, "Cutoff").value_text(), Some("1000 Hz"));
    assert_eq!(params.cutoff.value(), 1000.0);
    ui.key(Key::Enter);
    ui.type_text("400 hz");
    ui.key(Key::Enter);
    assert!((params.cutoff.value() - 400.0).abs() < 1e-3);
}

#[test]
fn going_elsewhere_takes_a_value_that_reads_and_drops_one_that_does_not() {
    let (params, mut ui) = panel();
    let drive = ui.get(Role::Slider, "Drive");
    let resonance = ui.get(Role::Slider, "Resonance");
    ui.double_click(drive.center());
    ui.type_text("12");
    // A press on another knob takes it.
    ui.click(resonance.center());
    assert_eq!(params.drive.value(), 12.0);
    ui.take_calls();
    ui.double_click(drive.center());
    ui.type_text("much");
    ui.click(resonance.center());
    assert_eq!(params.drive.value(), 12.0);
    ui.frame();
    assert!(ui.find(Role::TextInput, "Drive").is_none());
    // A press on the knob outside the box, the dial, takes it and drags.
    ui.double_click(drive.center());
    ui.type_text("3");
    ui.drag(drive.center(), (0.0, -20.0), 2);
    let value = params.drive.value();
    assert!((value - (3.0 + 24.0 * 0.1)).abs() < 1e-3, "{value}");
}

#[test]
fn the_release_of_a_key_whose_press_was_used_is_used_too() {
    let (_, mut ui) = panel();
    let key = |key: Key, pressed: bool| {
        Event::Key(KeyEvent {
            key,
            modifiers: Modifiers::NONE,
            pressed,
        })
    };
    // Nothing has the focus: the host gets the whole keystroke.
    assert!(!ui.send(key(Key::Up, true)));
    assert!(!ui.send(key(Key::Up, false)));
    ui.tab();
    ui.tab();
    // Enter takes the typed value and ends typing; its release is used
    // all the same, though nothing is typed into any more.
    assert!(ui.send(key(Key::Enter, true)));
    assert!(ui.send(key(Key::Enter, false)));
    ui.type_text("300");
    assert!(ui.send(key(Key::Enter, true)));
    assert!(!ui.root().wants_text());
    assert!(ui.send(key(Key::Enter, false)));
    // A release whose press the host got is the host's.
    assert!(!ui.send(key(Key::Character('q'), true)));
    assert!(!ui.send(key(Key::Character('q'), false)));
}

#[test]
fn a_slider_and_a_choice_take_typed_values_too() {
    struct Choice {
        shape: ChoiceParam,
    }
    param_set!(Choice { shape });
    let choice = Arc::new(Choice {
        shape: ChoiceParam::new("shape", "Shape", &["Sine", "Saw", "Square"], 0),
    });
    let (params, _) = panel();
    let view = row((
        knob(ParamHandle::new(Arc::clone(&choice), |p| &p.shape)),
        manguebyte_ui::slider(ParamHandle::new(Arc::clone(&params), |p| &p.mix)),
    ))
    .spacing(8.0);
    let set: Arc<dyn manguebyte_params::ParamSet> = Arc::clone(&choice) as _;
    let mut ui = Harness::with_params(view, Size::new(300.0, 120.0), 1.5, set);
    ui.frame();
    let shape = ui.get(Role::Slider, "Shape");
    ui.double_click(shape.center());
    ui.type_text("square");
    ui.key(Key::Enter);
    assert_eq!(choice.shape.index(), 2);
    let mix = ui.get(Role::Slider, "Mix");
    ui.double_click(mix.center());
    ui.frame();
    assert_eq!(ui.get(Role::TextInput, "Mix").value_text(), Some("100 %"));
    ui.type_text("25");
    ui.key(Key::Enter);
    let id = params.mix.param_id();
    let calls = ui.take_calls();
    assert_eq!(
        &calls[calls.len() - 3..],
        [BeginGesture(id), Perform(id, 0.25), EndGesture(id)]
    );
    ui.frame();
    assert!(ui.render_fresh() == *ui.pixels());
}
