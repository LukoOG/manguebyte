//! A combo box's menu through the glue: opened by the window's pointer and
//! by keys the host delivers, drawn in the editor's own window.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use manguebyte_gfx::Point;
use manguebyte_gui::{Editor, HeadlessEditor};
use manguebyte_params::{ChoiceParam, Param, param_set};
use manguebyte_plugin::editor::testing::{HostCall, RecordingEditorHost};
use manguebyte_plugin::editor::{
    EditorContext, EditorSpec, HostLink, HostSize, Key, KeyEvent, Modifiers, OpenEditor,
    ParentWindow,
};
use manguebyte_ui::{IntoView, ParamHandle, Role, column, combo, label};
use manguebyte_window::{Button, PhysicalPoint, RawInput};

struct Params {
    mode: ChoiceParam,
}
param_set!(Params { mode });

thread_local! {
    static OPENED: RefCell<Option<HeadlessEditor>> = const { RefCell::new(None) };
}

fn params() -> Arc<Params> {
    Arc::new(Params {
        mode: ChoiceParam::new("mode", "Mode", &["Clean", "Warm", "Hot"], 0),
    })
}

fn open(
    params: &Arc<Params>,
) -> (
    Rc<RecordingEditorHost>,
    Box<dyn OpenEditor>,
    HostLink,
    HeadlessEditor,
) {
    let shared = Arc::clone(params);
    let spec = Editor::new(HostSize::new(200, 160), move || {
        column((
            label("Drive").title(),
            combo(ParamHandle::new(Arc::clone(&shared), |p| &p.mode)),
        ))
        .spacing(6.0)
        .padding(8.0)
    })
    .on_headless_open(|editor| OPENED.with(|slot| *slot.borrow_mut() = Some(editor)));
    let host = Rc::new(RecordingEditorHost::new().with_params(params.clone()));
    let (context, link) = EditorContext::attach(host.clone());
    let parent = ParentWindow::win32(0x1F00).expect("not null");
    let mut editor = spec
        .open(parent, Some(1.25), context)
        .expect("headless windows open");
    editor.set_visible(true);
    let ui = OPENED
        .with(|slot| slot.borrow_mut().take())
        .expect("the hook ran");
    ui.tick();
    (host, editor, link, ui)
}

fn center(ui: &HeadlessEditor, role: Role, name: &str) -> Point {
    ui.find(role, name)
        .unwrap_or_else(|| panic!("no {role:?} {name:?}"))
        .bounds
        .center()
}

#[test]
fn the_pointer_opens_the_menu_hovers_and_chooses() {
    let params = params();
    let (host, editor, link, ui) = open(&params);
    ui.press(center(&ui, Role::ComboBox, "Mode"));
    ui.release(center(&ui, Role::ComboBox, "Mode"));
    ui.tick();
    assert!(ui.find(Role::Menu, "Mode").is_some());
    // Drawn in the editor's window, over everything.
    let menu = ui.find(Role::Menu, "Mode").expect("open");
    let shown = ui.shown().expect("open");
    let pixels = ui.pixels().expect("open");
    assert!(shown == pixels, "the window shows the back buffer");
    assert!(menu.device.bottom() <= i64::from(shown.height()));
    // Hovering with no button held moves the highlight.
    ui.move_to(center(&ui, Role::MenuItem, "Hot"));
    assert!(
        ui.find(Role::MenuItem, "Hot")
            .expect("open")
            .node
            .state
            .focused
    );
    ui.press(center(&ui, Role::MenuItem, "Hot"));
    ui.release(center(&ui, Role::MenuItem, "Hot"));
    let id = params.mode.param_id();
    assert_eq!(
        host.take_calls(),
        [
            HostCall::BeginGesture(id),
            HostCall::Perform(id, 1.0),
            HostCall::EndGesture(id)
        ]
    );
    ui.tick();
    assert!(ui.find(Role::Menu, "Mode").is_none());
    assert!(ui.shown().expect("open") == ui.pixels().expect("open"));
    drop(editor);
    drop(link);
}

#[test]
fn keys_the_host_delivers_open_and_choose() {
    let params = params();
    let (host, mut editor, link, ui) = open(&params);
    let key = |key: Key| KeyEvent {
        key,
        modifiers: Modifiers::NONE,
        pressed: true,
    };
    assert!(editor.key(key(Key::Tab)));
    assert!(editor.key(key(Key::Enter)));
    ui.tick();
    assert!(ui.find(Role::Menu, "Mode").is_some());
    // The window asks a host's dialog for every key while it is open.
    assert!(ui.window().wants_all_keys());
    // While it is open every key is the menu's, even one it has no use for.
    assert!(editor.key(key(Key::Character('x'))));
    assert!(editor.key(key(Key::Down)));
    assert!(editor.key(key(Key::Down)));
    assert!(editor.key(key(Key::Enter)));
    assert_eq!(params.mode.index(), 2);
    assert_eq!(host.take_calls().len(), 3);
    assert!(!ui.window().wants_all_keys());
    ui.tick();
    // The window losing the keyboard closes an open menu.
    assert!(editor.key(key(Key::Enter)));
    ui.window().input(RawInput::Focus(true));
    ui.window().input(RawInput::Focus(false));
    ui.tick();
    assert!(ui.find(Role::Menu, "Mode").is_none());
    // A press outside it, through the window, closes it and nothing else.
    // (Losing the keyboard took the focus away: Tab gives it back.)
    assert!(editor.key(key(Key::Tab)));
    assert!(editor.key(key(Key::Enter)));
    ui.tick();
    ui.window().input(RawInput::PointerDown {
        at: PhysicalPoint::new(240, 190),
        button: Button::Primary,
        modifiers: Modifiers::NONE,
        time_ms: 50_000,
    });
    ui.window().input(RawInput::PointerUp {
        at: PhysicalPoint::new(240, 190),
        button: Button::Primary,
        modifiers: Modifiers::NONE,
    });
    ui.tick();
    assert!(ui.find(Role::Menu, "Mode").is_none());
    assert!(host.take_calls().is_empty());
    drop(editor);
    drop(link);
}
