//! Interface state that outlives one open editor.
//!
//! A plugin's editor spec must be `Send + Sync + 'static`, and each time
//! the host opens the editor it builds a new interface. A [`State`] the
//! spec keeps is handed to every interface it builds, so what the person
//! chose survives closing and reopening the editor.

use std::sync::Arc;

use manguebyte_gfx::{Scene, Size};
use manguebyte_params::{FloatParam, FloatRange, param_set};
use manguebyte_plugin::editor::{
    EditorContext, EditorError, EditorSpec, HostSize, OpenEditor, ParentWindow,
};
use manguebyte_ui::testing::Harness;
use manguebyte_ui::{
    AccessCx, AccessNode, Action, Actions, Button, Event, EventCx, Handled, Label, MeasureCx,
    PaintCx, ParamHandle, Role, State, View, Widget, column, knob, row,
};

struct Params {
    gain: FloatParam,
}

param_set!(Params { gain });

/// A tab that shows page `index` when pressed.
struct Tab {
    index: u32,
    selected: State<u32>,
}

impl Widget for Tab {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Button, format!("Page {}", self.index + 1))
            .with_actions(Actions::PRESS)
    }

    fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
        Size::new(50.0, 20.0)
    }

    fn paint(&mut self, _cx: &PaintCx<'_>, _scene: &mut Scene) {}

    fn event(&mut self, _cx: &mut EventCx<'_>, event: &Event) -> Handled {
        match event {
            Event::PointerDown {
                button: Button::Primary,
                ..
            } => {
                self.selected.set(self.index);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    fn act(&mut self, _cx: &mut EventCx<'_>, action: Action) -> bool {
        if action != Action::Press {
            return false;
        }
        self.selected.set(self.index);
        true
    }
}

/// What a plugin hands its wrapper: the parameters, and the interface's
/// own state.
struct Spec {
    params: Arc<Params>,
    page: State<u32>,
}

impl Spec {
    /// The interface, built afresh each time the editor opens.
    fn view(&self) -> View {
        let page = self.page.clone();
        column((
            row((
                Tab {
                    index: 0,
                    selected: self.page.clone(),
                },
                Tab {
                    index: 1,
                    selected: self.page.clone(),
                },
            )),
            Label::dynamic(move || format!("Showing page {}", page.get() + 1)),
            knob(ParamHandle::new(Arc::clone(&self.params), |p| &p.gain)),
        ))
    }
}

impl EditorSpec for Spec {
    fn size(&self, scale: f64) -> HostSize {
        HostSize::new(200, 160).scale_by(scale)
    }

    fn open(
        &self,
        parent: ParentWindow,
        _scale: Option<f64>,
        _context: EditorContext,
    ) -> Result<Box<dyn OpenEditor>, EditorError> {
        // Putting `self.view()` in a window is the glue's work, not this
        // crate's; here the interface is opened in a harness instead.
        Err(EditorError::Unsupported(parent.api()))
    }
}

fn open(spec: &Spec) -> Harness {
    let mut ui = Harness::new(spec.view(), Size::new(200.0, 160.0), 1.0);
    ui.frame();
    ui
}

#[test]
fn a_page_chosen_before_closing_is_shown_again_after_reopening() {
    let spec = Arc::new(Spec {
        params: Arc::new(Params {
            gain: FloatParam::new("gain", "Gain", FloatRange::linear(-24.0, 24.0), 0.0),
        }),
        page: State::new(0),
    });
    // The wrapper keeps it as a shared spec, and the plugin instance that
    // made it may move between threads.
    let as_the_wrapper_keeps_it: Arc<dyn EditorSpec> = Arc::clone(&spec) as Arc<dyn EditorSpec>;
    let spec = std::thread::spawn(move || spec)
        .join()
        .expect("the spec crossed threads");

    let mut ui = open(&spec);
    assert!(ui.find(Role::Label, "Showing page 1").is_some());
    let second = ui.get(Role::Button, "Page 2");
    ui.click(second.center());
    ui.frame();
    assert!(ui.find(Role::Label, "Showing page 2").is_some());
    // Closed: the tree, its widgets and its labels are gone.
    assert!(
        ui.close().is_empty(),
        "choosing a page is no parameter edit"
    );

    // Reopened: a new tree, the same choice.
    let ui = open(&spec);
    assert!(ui.find(Role::Label, "Showing page 2").is_some());
    assert_eq!(spec.page.get(), 1);
    drop(ui);
    drop(as_the_wrapper_keeps_it);
    assert_eq!(Arc::strong_count(&spec), 1, "no interface kept the spec");
}
