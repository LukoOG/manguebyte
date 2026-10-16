//! Typing through the glue: the window's keys and characters, the keys a
//! host delivers itself, the keyboard focus, the clipboard and the caret.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use manguebyte_gfx::Point;
use manguebyte_gui::{Editor, HeadlessEditor};
use manguebyte_params::{FloatParam, FloatRange, Param, param_set};
use manguebyte_plugin::editor::testing::{HostCall, RecordingEditorHost};
use manguebyte_plugin::editor::{
    EditorContext, EditorSpec, HostLink, HostSize, Key, KeyEvent, Modifiers, OpenEditor,
    ParentWindow,
};
use manguebyte_ui::{IntoView, ParamHandle, Role, State, column, knob, text_field};
use manguebyte_window::{Handled, RawInput};

struct Params {
    gain: FloatParam,
}
param_set!(Params { gain });

thread_local! {
    static OPENED: RefCell<Option<HeadlessEditor>> = const { RefCell::new(None) };
}

fn params() -> Arc<Params> {
    Arc::new(Params {
        gain: FloatParam::new("gain", "Gain", FloatRange::linear(0.0, 1.0), 0.5).with_precision(2),
    })
}

struct Open {
    host: Rc<RecordingEditorHost>,
    editor: Box<dyn OpenEditor>,
    link: HostLink,
    ui: HeadlessEditor,
}

/// An editor with a knob for the gain and a field for a preset's name,
/// open and shown at scale 1.5.
fn open(params: &Arc<Params>, name: &State<String>) -> Open {
    let (shared, name) = (Arc::clone(params), name.clone());
    // A `State` is not `Send`: the view is built from a copy kept on this
    // thread.
    thread_local! {
        static NAME: RefCell<Option<State<String>>> = const { RefCell::new(None) };
    }
    NAME.with(|slot| *slot.borrow_mut() = Some(name));
    let spec = Editor::new(HostSize::new(200, 140), move || {
        let name = NAME.with(|slot| slot.borrow().clone()).expect("a name");
        column((
            knob(ParamHandle::new(Arc::clone(&shared), |p| &p.gain)),
            text_field("Preset", name),
        ))
        .spacing(6.0)
        .padding(8.0)
    })
    .on_headless_open(|editor| OPENED.with(|slot| *slot.borrow_mut() = Some(editor)));
    let host = Rc::new(RecordingEditorHost::new().with_params(params.clone()));
    let (context, link) = EditorContext::attach(host.clone());
    let parent = ParentWindow::win32(0x1F00).expect("not null");
    let mut editor = spec
        .open(parent, Some(1.5), context)
        .expect("headless windows open");
    editor.set_visible(true);
    let ui = OPENED
        .with(|slot| slot.borrow_mut().take())
        .expect("the hook ran");
    ui.tick();
    Open {
        host,
        editor,
        link,
        ui,
    }
}

fn key(key: Key, pressed: bool) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: Modifiers::NONE,
        pressed,
    }
}

/// Presses and releases `key` as the host delivers it, and returns whether
/// the press was used.
fn host_key(editor: &mut Box<dyn OpenEditor>, k: Key) -> bool {
    let used = editor.key(key(k, true));
    editor.key(key(k, false));
    used
}

fn center(ui: &HeadlessEditor, role: Role, name: &str) -> Point {
    ui.find(role, name)
        .unwrap_or_else(|| panic!("no {role:?} {name:?}"))
        .bounds
        .center()
}

fn text_of(ui: &HeadlessEditor, role: Role, name: &str) -> Option<String> {
    ui.find(role, name)?.node.value
}

#[test]
fn a_value_typed_through_the_window_is_one_edit() {
    let params = params();
    let name = State::new(String::new());
    let open = open(&params, &name);
    let ui = &open.ui;
    assert_eq!(ui.window().system_caret(), None);
    ui.double_click(center(ui, Role::Slider, "Gain"));
    ui.tick();
    // The window asked for every key and has the keyboard; the system is
    // shown the caret, inside the knob.
    assert!(ui.window().wants_all_keys());
    let knob = ui.find(Role::TextInput, "Gain").expect("typing");
    assert_eq!(knob.node.value.as_deref(), Some("0.50"));
    let caret = ui.window().system_caret().expect("a caret");
    assert!(
        knob.device.contains_rect(&caret),
        "{caret:?} in {:?}",
        knob.device
    );
    open.host.take_calls();
    assert!(ui.type_text("0.25"));
    assert!(ui.key(Key::Enter));
    let id = params.gain.param_id();
    assert_eq!(
        open.host.take_calls(),
        [
            HostCall::BeginGesture(id),
            HostCall::Perform(id, 0.25),
            HostCall::EndGesture(id)
        ]
    );
    ui.tick();
    assert_eq!(text_of(ui, Role::Slider, "Gain").as_deref(), Some("0.25"));
    assert_eq!(
        ui.window().system_caret(),
        None,
        "no caret once typing ends"
    );
    assert!(!ui.window().wants_all_keys());
    // Keys the knob does not use go back to the host again.
    assert!(!ui.key(Key::Character('q')));
    drop(open.editor);
    drop(open.link);
}

#[test]
fn keys_the_host_delivers_type_too_and_each_keystroke_is_taken_once() {
    let params = params();
    let name = State::new(String::new());
    let mut open = open(&params, &name);
    // The host has the keyboard and delivers keys itself: Tab to the knob,
    // Enter to type its value.
    assert!(host_key(&mut open.editor, Key::Tab));
    let requests = open.ui.window().focus_requests();
    assert!(host_key(&mut open.editor, Key::Enter));
    // Typing started: the editor took the keyboard, so that the keys that
    // follow come to its window.
    assert_eq!(open.ui.window().focus_requests(), requests + 1);
    open.ui.tick();
    assert_eq!(
        text_of(&open.ui, Role::TextInput, "Gain").as_deref(),
        Some("0.50")
    );
    // Each character as the host delivers it, and then the same keystroke
    // through the window, as a host that does both: taken once.
    for c in "0.3".chars() {
        assert!(host_key(&mut open.editor, Key::Character(c)));
        let down = RawInput::Key {
            key: key(Key::Character(c), true),
            repeat: false,
        };
        assert_eq!(open.ui.window().input(down), Some(Handled::Yes));
        let mut units = [0u16; 2];
        for unit in c.encode_utf16(&mut units) {
            assert_eq!(
                open.ui.window().input(RawInput::Utf16(*unit)),
                Some(Handled::Yes)
            );
        }
    }
    assert_eq!(
        text_of(&open.ui, Role::TextInput, "Gain").as_deref(),
        Some("0.3")
    );
    // After a frame, a keystroke through the window alone is a new one.
    open.ui.tick();
    assert!(open.ui.type_text("5"));
    assert_eq!(
        text_of(&open.ui, Role::TextInput, "Gain").as_deref(),
        Some("0.35")
    );
    open.host.take_calls();
    assert!(host_key(&mut open.editor, Key::Enter));
    let id = params.gain.param_id();
    let calls = open.host.take_calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        matches!(calls[1], HostCall::Perform(param, v) if param == id && (v - 0.35).abs() < 1e-6)
    );
    // Control and a letter is a shortcut, not text: it goes back.
    let control = KeyEvent {
        key: Key::Character('s'),
        modifiers: Modifiers {
            control: true,
            ..Modifiers::NONE
        },
        pressed: true,
    };
    assert!(!open.editor.key(control));
    drop(open.editor);
    drop(open.link);
}

#[test]
fn a_field_copies_to_and_pastes_from_the_windows_clipboard() {
    let params = params();
    let name = State::new(String::from("Warm pad"));
    let open = open(&params, &name);
    let ui = &open.ui;
    ui.press(center(ui, Role::TextInput, "Preset"));
    ui.release(center(ui, Role::TextInput, "Preset"));
    let control = Modifiers {
        control: true,
        ..Modifiers::NONE
    };
    assert!(ui.key_with(Key::Character('a'), control));
    assert!(ui.key_with(Key::Character('c'), control));
    assert_eq!(ui.window().clipboard_text().as_deref(), Some("Warm pad"));
    ui.window().set_clipboard_text(Some("Cold\r\nlead"));
    assert!(ui.key_with(Key::Character('v'), control));
    assert_eq!(
        text_of(ui, Role::TextInput, "Preset").as_deref(),
        Some("Cold  lead")
    );
    // Escape throws the edit away; with nothing left to throw away, it is
    // the host's.
    assert!(ui.key(Key::Escape));
    assert_eq!(
        text_of(ui, Role::TextInput, "Preset").as_deref(),
        Some("Warm pad")
    );
    assert!(!ui.key(Key::Escape));
    // Typed text and Enter put it into the state.
    assert!(ui.key(Key::End));
    assert!(ui.type_text(" 2"));
    assert!(ui.key(Key::Enter));
    assert_eq!(name.get(), "Warm pad 2");
    drop(open.editor);
    drop(open.link);
}
