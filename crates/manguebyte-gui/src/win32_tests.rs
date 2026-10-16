//! The glue against real Win32 windows: a top-level window owned by the
//! test plays the host's window, and the editor is opened inside it with
//! the system's backend, whatever the crate's features say. Input is sent
//! with `SendMessageW`; pixels are read back from the window's surface.
//!
//! Everything is in one test because it counts the process's GDI and USER
//! objects, which windows made by tests on other threads would change.
//!
//! A read-back fails when this session cannot read windows back, unless
//! `MANGUEBYTE_WINDOW_SKIP_READ_BACK` is set (CI never sets it); then it
//! says so on stderr and skips those checks.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use manguebyte_core::AtomicF32;
use manguebyte_gfx::{IRect, Pixmap};
use manguebyte_params::{FloatParam, FloatRange, Param, param_set};
use manguebyte_plugin::editor::testing::{HostCall, RecordingEditorHost};
use manguebyte_plugin::editor::{EditorContext, HostSize, OpenEditor, Resize};
use manguebyte_ui::{AtomicSource, IntoView, ParamHandle, Role, column, knob, label, meter, row};
use manguebyte_window::win32::testing::{self, Message, ReadBack, TopLevel};
use manguebyte_window::{Button, PhysicalPoint};

/// `DLGC_WANTALLKEYS` and `DLGC_WANTTAB`, in a window's answer to
/// `WM_GETDLGCODE`: it wants Tab, Enter and Escape from a dialog.
const WANTS_ALL_KEYS: isize = 0x0004 | 0x0002;

use crate::Editor;
use crate::open::{Backend, Opened, open_window};

struct Params {
    gain: FloatParam,
}
param_set!(Params { gain });

fn params() -> Arc<Params> {
    Arc::new(Params {
        gain: FloatParam::new("gain", "Gain", FloatRange::linear(0.0, 1.0), 0.5).with_precision(2),
    })
}

fn spec(params: &Arc<Params>, level: &Arc<AtomicF32>) -> Editor {
    let params = Arc::clone(params);
    let level = Arc::clone(level);
    Editor::new(HostSize::new(200, 140), move || {
        column((
            label("Test").title(),
            row((
                knob(ParamHandle::new(Arc::clone(&params), |p| &p.gain)),
                meter("Level", AtomicSource::new(Arc::clone(&level)))
                    .range(-60.0, 0.0)
                    .unit("dB")
                    .precision(1),
            ))
            .spacing(8.0),
        ))
        .padding(8.0)
    })
    .with_resize(Resize::Free {
        min: HostSize::new(150, 100),
        max: HostSize::new(400, 280),
    })
}

/// What GDI shows for premultiplied pixels: the colour over black, opaque.
fn opaque(pixels: &Pixmap) -> Pixmap {
    let mut data = pixels.data().to_vec();
    for pixel in data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    Pixmap::from_premultiplied(pixels.width(), pixels.height(), data).expect("opaque")
}

fn handle(opened: &Opened) -> usize {
    opened.window.raw_handle().expect("open")
}

fn device(x: f32, y: f32, scale: f32) -> PhysicalPoint {
    PhysicalPoint::new((x * scale).round() as i32, (y * scale).round() as i32)
}

/// Runs the message loop until the editor has presented `count` frames.
fn pump_frames(opened: &Opened, count: u64) -> bool {
    let target = opened.shared.presented.get() + count;
    testing::pump_until(Duration::from_secs(5), || {
        opened.shared.presented.get() >= target
    })
}

fn knob_center(opened: &Opened) -> (f32, f32) {
    let root = opened.shared.root.borrow();
    let entry = root
        .as_ref()
        .expect("open")
        .access_tree()
        .into_iter()
        .find(|entry| entry.node.role == Role::Slider && entry.node.name == "Gain")
        .expect("a knob");
    let center = entry.bounds.center();
    (center.x, center.y)
}

fn knob_text(opened: &Opened) -> Option<String> {
    let root = opened.shared.root.borrow();
    root.as_ref()?
        .access_tree()
        .into_iter()
        .find(|entry| entry.node.role == Role::Slider)
        .and_then(|entry| entry.node.value)
}

#[test]
fn an_editor_lives_inside_a_real_win32_parent() {
    let parent = TopLevel::new(480, 320).expect("a desktop");
    let readable = testing::require_read_back(&parent, IRect::new(420, 280, 40, 30));
    let params = params();
    let level = Arc::new(AtomicF32::new(-60.0));
    let spec = spec(&params, &level);

    // --- Opened at the host's scale 1.5: a child of the host's window, of
    // the spec's size at that scale.
    let host = Rc::new(RecordingEditorHost::new().with_params(params.clone()));
    let (context, link) = EditorContext::attach(host.clone());
    let mut opened = open_window(&spec, Backend::Native, parent.parent(), Some(1.5), context)
        .expect("a child window");
    // Opened hidden, with no frames, until the host shows it.
    assert!(!opened.window.is_visible());
    opened.set_visible(true);
    let child = handle(&opened);
    assert_eq!(testing::parent_of(child), parent.handle());
    assert_eq!(opened.size(), HostSize::new(300, 210));
    assert_eq!(opened.window.scale(), 1.5);

    // The system asks for its contents and the frame timer runs: what is on
    // the screen is the interface's back buffer.
    assert!(pump_frames(&opened, 1), "no frame was presented");
    testing::update(child);
    if readable {
        let shown = testing::read_back(&opened.window.cx(), ReadBack::Screen).expect("ours");
        let pixels = opened
            .shared
            .root
            .borrow()
            .as_ref()
            .expect("open")
            .pixels()
            .clone();
        assert_eq!((pixels.width(), pixels.height()), (300, 210));
        assert!(
            shown == opaque(&pixels),
            "the screen is not the back buffer"
        );
    }

    // --- A drag on the knob, as the system delivers it: 20 logical pixels
    // up is a tenth of the theme's drag range.
    let (x, y) = knob_center(&opened);
    let scale = 1.5;
    testing::send(
        child,
        Message::ButtonDown {
            button: Button::Primary,
            at: device(x, y, scale),
            shift: false,
            control: false,
        },
    );
    for step in 1..=4 {
        let moved = y - 5.0 * step as f32;
        testing::send(
            child,
            Message::MouseMove {
                at: device(x, moved, scale),
                shift: false,
                control: false,
            },
        );
    }
    testing::send(
        child,
        Message::ButtonUp {
            button: Button::Primary,
            at: device(x, y - 20.0, scale),
        },
    );
    let id = params.gain.param_id();
    let calls = host.take_calls();
    assert_eq!(
        calls.first(),
        Some(&HostCall::BeginGesture(id)),
        "{calls:?}"
    );
    assert_eq!(calls.last(), Some(&HostCall::EndGesture(id)), "{calls:?}");
    assert_eq!(calls.len(), 6, "{calls:?}");
    assert!(
        (params.gain.value() - 0.6).abs() < 1e-6,
        "{}",
        params.gain.value()
    );

    // --- A value typed into the knob, as the system delivers it: a double
    // click (two presses with the same message time), then key and
    // character messages, then Enter.
    for _ in 0..2 {
        testing::send(
            child,
            Message::ButtonDown {
                button: Button::Primary,
                at: device(x, y, scale),
                shift: false,
                control: false,
            },
        );
        testing::send(
            child,
            Message::ButtonUp {
                button: Button::Primary,
                at: device(x, y, scale),
            },
        );
    }
    assert!(pump_frames(&opened, 1));
    assert!(opened.window.wants_all_keys());
    let code = testing::send(child, Message::GetDlgCode).expect("ours");
    assert_eq!(code & WANTS_ALL_KEYS, WANTS_ALL_KEYS);
    let caret = testing::system_caret().expect("a caret while typing");
    assert_eq!(caret.0, child);
    host.take_calls();
    for (vk, unit) in [(0x30, '0'), (0xBE, '.'), (0x33, '3'), (0x0D, '\r')] {
        testing::send(
            child,
            Message::KeyDown {
                vk,
                repeat: false,
                system: false,
            },
        );
        testing::send(child, Message::Char(unit as u16));
        testing::send(child, Message::KeyUp { vk });
    }
    let calls = host.take_calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        matches!(calls[1], HostCall::Perform(param, v) if param == id && (v - 0.3).abs() < 1e-6),
        "{calls:?}"
    );
    assert!(!opened.window.wants_all_keys());
    assert_eq!(testing::system_caret(), None);
    // Nothing went on to the host's window while typing.
    let received = parent.take_received();
    assert!(received.is_empty(), "{received:?}");

    // --- A value set from elsewhere and a level the audio side publishes
    // reach the screen through the window's own timer.
    params.gain.set(0.25);
    level.store(-6.0, std::sync::atomic::Ordering::Relaxed);
    assert!(pump_frames(&opened, 1));
    assert_eq!(knob_text(&opened).as_deref(), Some("0.25"));
    if readable {
        testing::update(child);
        let shown = testing::read_back(&opened.window.cx(), ReadBack::Screen).expect("ours");
        let pixels = opened
            .shared
            .root
            .borrow()
            .as_ref()
            .expect("open")
            .pixels()
            .clone();
        assert!(
            shown == opaque(&pixels),
            "the screen is not the back buffer"
        );
    }

    // --- The host resizes the editor; the interface follows.
    assert!(opened.set_size(HostSize::new(450, 300)));
    assert_eq!(opened.size(), HostSize::new(450, 300));
    assert!(
        !opened.set_size(HostSize::new(9999, 9999)),
        "beyond the rules"
    );
    let layout = opened.shared.root.borrow().as_ref().map(|root| root.size());
    assert_eq!(layout, Some(manguebyte_gfx::Size::new(300.0, 200.0)));

    // --- Hidden, it does no work; shown again, it does.
    opened.set_visible(false);
    let frames = opened.shared.frames.get();
    testing::pump(Duration::from_millis(80));
    assert_eq!(
        opened.shared.frames.get(),
        frames,
        "frames ran while hidden"
    );
    opened.set_visible(true);
    assert!(testing::pump_until(Duration::from_secs(5), || {
        opened.shared.frames.get() > frames
    }));

    // --- Closing destroys the window.
    drop(opened);
    drop(link);
    assert!(!testing::is_window(child));
    assert_eq!(host.well_formed(), Ok(()));

    // --- The host destroys its window under an open editor: the editor
    // lets go of its interface, ending a drag in progress, and tells the
    // host it cannot go on.
    let doomed = TopLevel::new(320, 240).expect("a desktop");
    let host = Rc::new(RecordingEditorHost::new().with_params(params.clone()));
    let (context, link) = EditorContext::attach(host.clone());
    let mut opened = open_window(&spec, Backend::Native, doomed.parent(), Some(1.0), context)
        .expect("a child window");
    opened.set_visible(true);
    assert!(pump_frames(&opened, 1));
    let (x, y) = knob_center(&opened);
    testing::send(
        handle(&opened),
        Message::ButtonDown {
            button: Button::Primary,
            at: device(x, y, 1.0),
            shift: false,
            control: false,
        },
    );
    doomed.destroy();
    assert_eq!(
        host.take_calls(),
        [
            HostCall::BeginGesture(id),
            HostCall::EndGesture(id),
            HostCall::Closed
        ]
    );
    assert!(opened.shared.root.borrow().is_none());
    assert!(!opened.window.is_open());
    drop(opened);
    drop(link);

    // --- Opened and closed many times, it leaves no window, DC or bitmap
    // behind. The system caches some objects on first use, so the count
    // starts after the windows above.
    let cycle = |round: usize| {
        let host = Rc::new(RecordingEditorHost::new());
        let (context, link) = EditorContext::attach(host);
        let scale = [1.0, 1.25, 2.0][round % 3];
        let mut opened = open_window(
            &spec,
            Backend::Native,
            parent.parent(),
            Some(scale),
            context,
        )
        .expect("a child window");
        opened.set_visible(true);
        if round % 20 == 0 {
            assert!(pump_frames(&opened, 1));
        }
        drop(opened);
        drop(link);
    };
    for round in 0..20 {
        cycle(round);
    }
    testing::pump(Duration::from_millis(20));
    let before = testing::gui_resources();
    for round in 0..200 {
        cycle(round);
    }
    testing::pump(Duration::from_millis(20));
    let after = testing::gui_resources();
    eprintln!("GDI and USER objects: {before:?} before 200 editors, {after:?} after");
    assert!(
        after.gdi <= before.gdi && after.user <= before.user,
        "{before:?} before 200 editors, {after:?} after"
    );
    // The last window took the class with it.
    assert!(!testing::class_registered());
    drop(parent);
}

/// Copy and paste through the system's clipboard, with Control held as
/// the keyboard state says (`GetKeyState`), in a value typed into the
/// knob. It replaces what the person running the tests had copied, so it
/// is ignored unless asked for; the text that was there is put back.
#[test]
#[ignore = "replaces the system clipboard; run with --ignored"]
fn a_typed_value_is_copied_and_pasted_through_the_system_clipboard() {
    const VK_CONTROL: u16 = 0x11;
    let parent = TopLevel::new(480, 320).expect("a desktop");
    let params = params();
    let spec = spec(&params, &Arc::new(AtomicF32::new(-60.0)));
    let host = Rc::new(RecordingEditorHost::new().with_params(params.clone()));
    let (context, link) = EditorContext::attach(host.clone());
    let mut opened = open_window(&spec, Backend::Native, parent.parent(), Some(1.0), context)
        .expect("a child window");
    opened.set_visible(true);
    assert!(pump_frames(&opened, 1));
    let child = handle(&opened);
    let saved = opened.window.read_clipboard_text();
    let (x, y) = knob_center(&opened);
    for _ in 0..2 {
        testing::send(
            child,
            Message::ButtonDown {
                button: Button::Primary,
                at: device(x, y, 1.0),
                shift: false,
                control: false,
            },
        );
        testing::send(
            child,
            Message::ButtonUp {
                button: Button::Primary,
                at: device(x, y, 1.0),
            },
        );
    }
    let key = |vk: u16| {
        testing::send(
            child,
            Message::KeyDown {
                vk,
                repeat: false,
                system: false,
            },
        );
        testing::send(child, Message::KeyUp { vk });
    };
    {
        // Control+C copies the value, all selected.
        let _control = testing::HeldKeys::hold(&[VK_CONTROL]);
        key(u16::from(b'C'));
    }
    assert_eq!(opened.window.read_clipboard_text().as_deref(), Some("0.50"));
    opened
        .window
        .write_clipboard_text("0.75")
        .expect("the clipboard");
    {
        let _control = testing::HeldKeys::hold(&[VK_CONTROL]);
        key(u16::from(b'V'));
    }
    host.take_calls();
    key(0x0D);
    let id = params.gain.param_id();
    let calls = host.take_calls();
    assert!(
        matches!(calls.get(1), Some(HostCall::Perform(param, v)) if *param == id && (v - 0.75).abs() < 1e-6),
        "{calls:?}"
    );
    if let Some(saved) = saved {
        let _ = opened.window.write_clipboard_text(&saved);
    }
    drop(opened);
    drop(link);
}
