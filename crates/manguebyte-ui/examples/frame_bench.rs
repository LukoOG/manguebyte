//! How long a frame of an editor with 16 knobs and 4 meters takes at 2x.
//!
//! Run it in release mode:
//!
//! ```text
//! cargo run -p manguebyte-ui --example frame_bench --release
//! cargo run -p manguebyte-ui --example frame_bench --release -- --quick
//! ```
//!
//! Each case runs a frame many times and prints the median and fastest
//! time, and the device pixels the frame drew again. A frame here is the
//! whole of `Root::frame`: polling every widget, layout when needed,
//! recording the scenes that changed and drawing the damage into the back
//! buffer. Presenting the buffer on screen is not included. Times depend
//! on the machine and on what else it is doing.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use manguebyte_core::AtomicF32;
use manguebyte_gfx::{IRect, Point, Size};
use manguebyte_params::{FloatParam, FloatRange, Param, ParamSet};
use manguebyte_plugin::editor::EditorContext;
use manguebyte_plugin::editor::testing::RecordingEditorHost;
use manguebyte_ui::{
    AtomicSource, Button, Event, IntoView, Modifiers, ParamHandle, Root, View, column, knob, meter,
    panel, row,
};

/// Sixteen parameters, as a plugin might have.
struct Bank {
    knobs: Vec<FloatParam>,
}

impl ParamSet for Bank {
    fn visit<'a>(&'a self, visitor: &mut dyn FnMut(&'a dyn Param)) {
        for knob in &self.knobs {
            visitor(knob);
        }
    }
}

const SIZE: Size = Size::new(400.0, 520.0);
const SCALE: f32 = 2.0;

fn bank() -> Arc<Bank> {
    Arc::new(Bank {
        knobs: (0..16)
            .map(|i| {
                FloatParam::new(
                    &format!("k{i}"),
                    &format!("Knob {}", i + 1),
                    FloatRange::linear(-24.0, 24.0),
                    0.0,
                )
                .with_unit("dB")
                .with_precision(1)
            })
            .collect(),
    })
}

fn view(bank: &Arc<Bank>, levels: &[Arc<AtomicF32>]) -> View {
    let rows: Vec<View> = (0..4)
        .map(|r| {
            let mut items: Vec<View> = (0..4)
                .map(|c| {
                    let index = r * 4 + c;
                    knob(ParamHandle::new(Arc::clone(bank), move |b: &Bank| {
                        &b.knobs[index]
                    }))
                    .into_view()
                })
                .collect();
            items.push(
                meter(
                    format!("Level {}", r + 1),
                    AtomicSource::new(Arc::clone(&levels[r])),
                )
                .range(-60.0, 0.0)
                .unit("dB")
                .precision(1)
                .into_view(),
            );
            row(items).spacing(8.0)
        })
        .collect();
    panel(column(rows).spacing(8.0).padding(12.0))
}

struct Editor {
    root: Root,
    bank: Arc<Bank>,
    levels: Vec<Arc<AtomicF32>>,
    clock: Duration,
    // Dropped after the root.
    _link: manguebyte_plugin::editor::HostLink,
}

fn open() -> Editor {
    let bank = bank();
    let levels: Vec<Arc<AtomicF32>> = (0..4).map(|_| Arc::new(AtomicF32::new(-60.0))).collect();
    let host =
        Rc::new(RecordingEditorHost::new().with_params(Arc::clone(&bank) as Arc<dyn ParamSet>));
    let (context, link) = EditorContext::attach(host);
    let root = Root::new(view(&bank, &levels), SIZE, SCALE, context);
    Editor {
        root,
        bank,
        levels,
        clock: Duration::ZERO,
        _link: link,
    }
}

impl Editor {
    /// Runs a frame 16 ms after the last and returns the damaged pixels.
    fn frame(&mut self) -> u64 {
        self.clock += Duration::from_millis(16);
        let frame = self.root.frame(self.clock);
        frame.damage.iter().map(IRect::area).sum()
    }
}

struct Row {
    name: &'static str,
    median: f64,
    best: f64,
    pixels: u64,
}

fn measure(name: &'static str, runs: usize, mut run: impl FnMut() -> u64) -> Row {
    let mut times = Vec::with_capacity(runs);
    let mut pixels = 0;
    // One run first, so caches are as they are in use.
    run();
    for _ in 0..runs {
        let start = Instant::now();
        pixels = run();
        times.push(start.elapsed().as_secs_f64() * 1e3);
    }
    times.sort_by(f64::total_cmp);
    Row {
        name,
        median: times[times.len() / 2],
        best: times[0],
        pixels,
    }
}

fn main() {
    let quick = std::env::args().any(|arg| arg == "--quick");
    let runs = if quick { 30 } else { 300 };
    let mut rows = Vec::new();

    rows.push(measure(
        "open: build, lay out, draw everything",
        runs / 3,
        || {
            let mut editor = open();
            editor.frame()
        },
    ));

    let mut editor = open();
    editor.frame();
    rows.push(measure("steady: nothing changed", runs, || editor.frame()));

    let mut flip = false;
    rows.push(measure("one knob moved by automation", runs, || {
        flip = !flip;
        editor.bank.knobs[5].set(if flip { 12.0 } else { -6.0 });
        editor.frame()
    }));

    let mut level = -60.0;
    rows.push(measure("four meters moving", runs, || {
        level = if level > -1.0 { -60.0 } else { level + 0.7 };
        for (i, meter) in editor.levels.iter().enumerate() {
            meter.store(level + i as f32, Ordering::Relaxed);
        }
        editor.frame()
    }));

    let mut value = -24.0;
    rows.push(measure("16 knobs and 4 meters moving", runs, || {
        value = if value > 23.0 { -24.0 } else { value + 0.5 };
        for (i, knob) in editor.bank.knobs.iter().enumerate() {
            knob.set(value + i as f32 * 0.1);
        }
        for meter in &editor.levels {
            meter.store(value - 30.0, Ordering::Relaxed);
        }
        editor.frame()
    }));

    // A drag on the first knob: a press, then a move and a frame each time.
    let first = editor
        .root
        .access_tree()
        .into_iter()
        .find(|entry| entry.node.name == "Knob 1")
        .expect("the first knob");
    let at = first.bounds.center();
    editor.root.event(Event::PointerDown {
        at,
        button: Button::Primary,
        clicks: 1,
        modifiers: Modifiers::NONE,
    });
    editor.frame();
    let mut y = 0.0;
    rows.push(measure("one knob dragged", runs, || {
        y = if y > 150.0 { 0.0 } else { y + 1.5 };
        editor.root.event(Event::PointerMove {
            at: Point::new(at.x, at.y - y),
            modifiers: Modifiers::NONE,
        });
        editor.frame()
    }));

    let buffer = editor.root.pixels();
    println!(
        "16 knobs and 4 meters, {} x {} logical at {SCALE}x: a buffer of {} x {} pixels",
        SIZE.width,
        SIZE.height,
        buffer.width(),
        buffer.height()
    );
    println!("{runs} frames per case (a third of that for opening)\n");
    println!("| case | median ms | best ms | pixels drawn |");
    println!("|---|---|---|---|");
    for row in rows {
        println!(
            "| {} | {:.4} | {:.4} | {} |",
            row.name, row.median, row.best, row.pixels
        );
    }
}
