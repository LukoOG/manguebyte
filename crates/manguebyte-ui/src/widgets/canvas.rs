//! Drawing with a closure.

use manguebyte_gfx::{Rect, Scene, Size, Transform};

use crate::access::{AccessNode, Role};
use crate::source::{Source, Watch};
use crate::theme::Theme;
use crate::widget::{AccessCx, MeasureCx, PaintCx, UpdateCx, Widget};

/// What a [`Canvas`]'s closure is given.
pub struct CanvasCx<'a> {
    theme: &'a Theme,
    size: Size,
    scale: f32,
}

impl CanvasCx<'_> {
    /// The theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// The canvas's size, in logical pixels. The closure draws from
    /// `(0, 0)` to here.
    pub fn size(&self) -> Size {
        self.size
    }

    /// The canvas's own rectangle, from `(0, 0)`.
    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size.width, self.size.height)
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }
}

type Draw = Box<dyn Fn(&CanvasCx<'_>, &mut Scene)>;

/// A drawing made by a closure, with no interaction: a logo, a curve, a
/// decoration.
///
/// The closure draws in the canvas's own coordinates, `(0, 0)` at its
/// top-left corner, and whatever it draws is clipped to the canvas. It is
/// called when the canvas is first drawn, when it moves or changes size,
/// and, if [`redraw_on`](Canvas::redraw_on) gave it a source, whenever the
/// number the source gives changes, such as a version or a frame counter.
pub struct Canvas {
    name: String,
    natural: Size,
    draw: Draw,
    watch: Option<Watch<u64>>,
}

/// A canvas called `name`, `natural` in size unless laid out otherwise,
/// drawn by `draw`.
///
/// ```
/// use std::cell::Cell;
/// use std::rc::Rc;
/// use manguebyte_gfx::{Circle, Color, Size};
/// use manguebyte_ui::testing::Harness;
/// use manguebyte_ui::{IntoView, Role, canvas, row};
///
/// let generation = Rc::new(Cell::new(0_u64));
/// let seen = Rc::clone(&generation);
/// let dot = canvas("Dot", Size::new(20.0, 20.0), |cx, scene| {
///     let middle = cx.bounds().center();
///     scene.fill(Circle::new(middle, 8.0), Color::hex(0x3D8BFF));
/// })
/// .redraw_on(move || seen.get());
/// let mut ui = Harness::new(row((dot,)).padding(5.0), Size::new(30.0, 30.0), 1.0);
/// ui.frame();
///
/// // Drawn in its own coordinates, moved to where layout put it.
/// let found = ui.get(Role::Image, "Dot");
/// assert_eq!(found.bounds.x, 5.0);
/// assert!(ui.dump_of(&found).contains("circle"));
/// // A new number from its source draws it again; the same drawing
/// // damages nothing.
/// generation.set(1);
/// assert_eq!(ui.frame().damaged, 0);
/// ```
pub fn canvas(
    name: impl Into<String>,
    natural: Size,
    draw: impl Fn(&CanvasCx<'_>, &mut Scene) + 'static,
) -> Canvas {
    Canvas::new(name, natural, draw)
}

impl Canvas {
    /// A canvas called `name`, `natural` in size unless laid out
    /// otherwise, drawn by `draw`.
    pub fn new(
        name: impl Into<String>,
        natural: Size,
        draw: impl Fn(&CanvasCx<'_>, &mut Scene) + 'static,
    ) -> Canvas {
        Canvas {
            name: name.into(),
            natural,
            draw: Box::new(draw),
            watch: None,
        }
    }

    /// The same canvas, drawn again whenever `source` gives a different
    /// number.
    #[must_use]
    pub fn redraw_on(mut self, source: impl Source<u64>) -> Canvas {
        self.watch = Some(Watch::new(source));
        self
    }
}

impl Widget for Canvas {
    fn access(&self, _cx: &AccessCx<'_>) -> AccessNode {
        AccessNode::new(Role::Image, self.name.as_str())
    }

    fn measure(&mut self, _cx: &MeasureCx<'_>) -> Size {
        self.natural
    }

    fn update(&mut self, cx: &mut UpdateCx<'_>) {
        if self.watch.as_mut().is_some_and(Watch::poll) {
            cx.request_paint();
        }
    }

    fn paint(&mut self, cx: &PaintCx<'_>, scene: &mut Scene) {
        let rect = cx.rect();
        let canvas = CanvasCx {
            theme: cx.theme(),
            size: rect.size(),
            scale: cx.scale(),
        };
        let draw = &self.draw;
        scene.clipped(rect, |scene| {
            scene.transformed(Transform::translate(rect.x, rect.y), |scene| {
                draw(&canvas, scene);
            });
        });
    }
}
