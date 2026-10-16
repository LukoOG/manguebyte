//! Parameters as controls see them: read once per frame, changed only
//! through gestures.
//!
//! A [`ParamHandle`] names one parameter of a plugin's parameter set and
//! reads it straight from its atomic. A [`ParamBinding`] is what a control
//! keeps: the handle, the value it showed last, the
//! [`Gesture`](manguebyte_plugin::editor::Gesture) it has open while a
//! person holds it, and the last value it sent.
//!
//! A host may store an edit into the parameter only later: the VST 3
//! wrapper does so when the host echoes the edit in its next `process`
//! call. So a control shows the value it is sending while a gesture is
//! open, and after an edit it keeps showing the value it sent, and starts
//! the next edit from it, for as long as the parameter still reads what it
//! read before the edit, and at most [`ParamBinding::ECHO_WAIT`]. As soon
//! as the parameter reads anything else (the echo, or host automation) the
//! control shows the parameter's own value again. Every edit starts from
//! the value read at that moment, not from the one read at the last frame.
//!
//! ```
//! use std::rc::Rc;
//! use std::sync::Arc;
//! use std::time::Duration;
//! use manguebyte_params::{FloatParam, FloatRange, Param, param_set};
//! use manguebyte_plugin::editor::EditorContext;
//! use manguebyte_plugin::editor::testing::{HostCall, RecordingEditorHost};
//! use manguebyte_ui::{ParamBinding, ParamHandle};
//!
//! struct Params { gain: FloatParam }
//! param_set!(Params { gain });
//!
//! let params = Arc::new(Params {
//!     gain: FloatParam::new("gain", "Gain", FloatRange::linear(-24.0, 24.0), 0.0)
//!         .with_unit("dB")
//!         .with_precision(1),
//! });
//! let handle = ParamHandle::new(Arc::clone(&params), |p| &p.gain);
//! assert_eq!(handle.name(), "Gain");
//! assert_eq!(handle.text(0.75), "12.0 dB");
//!
//! // A host that does not store values: it stands for one that echoes
//! // them late, or never.
//! let host = Rc::new(RecordingEditorHost::new());
//! let (context, _link) = EditorContext::attach(host.clone());
//! let mut binding = ParamBinding::new(handle);
//! assert!(binding.poll(Duration::ZERO), "the first read counts as a change");
//! binding.begin(&context);
//! binding.set(0.75);
//! assert_eq!(binding.shown(), 0.75, "the value being sent");
//! binding.end();
//! assert_eq!(binding.text(), "12.0 dB", "the value sent, not yet echoed");
//!
//! // A key press now moves on from the value sent.
//! let next = binding.current() + 0.01;
//! binding.set_once(&context, next);
//! assert_eq!(binding.shown(), 0.76);
//!
//! // No echo came: once the wait is over, the parameter's own value.
//! assert!(!binding.poll(Duration::from_millis(200)));
//! assert!(binding.poll(ParamBinding::ECHO_WAIT));
//! assert_eq!(binding.shown(), 0.5);
//! assert_eq!(binding.text(), "0.0 dB");
//! let id = params.gain.param_id();
//! assert_eq!(host.take_calls(), [
//!     HostCall::BeginGesture(id),
//!     HostCall::Perform(id, 0.75),
//!     HostCall::EndGesture(id),
//!     HostCall::BeginGesture(id),
//!     HostCall::Perform(id, 0.76),
//!     HostCall::EndGesture(id),
//! ]);
//! ```

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use manguebyte_core::ParamId;
use manguebyte_params::{Param, ParamSet};
use manguebyte_plugin::editor::{EditorContext, Gesture};

/// How a handle finds its parameter.
trait Lookup {
    fn param(&self) -> Option<&dyn Param>;
}

/// A typed field of a parameter set.
struct Field<P, F, T> {
    set: Arc<P>,
    field: F,
    param: PhantomData<fn() -> T>,
}

impl<P, T, F> Lookup for Field<P, F, T>
where
    P: Send + Sync + 'static,
    T: Param + 'static,
    F: Fn(&P) -> &T + 'static,
{
    fn param(&self) -> Option<&dyn Param> {
        Some((self.field)(&self.set))
    }
}

/// A parameter found by its id in a set.
struct ById {
    set: Arc<dyn ParamSet>,
    id: ParamId,
}

impl Lookup for ById {
    fn param(&self) -> Option<&dyn Param> {
        self.set.find_param(self.id)
    }
}

/// One parameter of a plugin, as an interface reads it.
///
/// Cheap to clone; clones name the same parameter. Reading the value is an
/// atomic load and a little arithmetic, and allocates nothing; turning a
/// value into text allocates, so controls do it only when the value they
/// show has changed.
#[derive(Clone)]
pub struct ParamHandle {
    lookup: Rc<dyn Lookup>,
    id: ParamId,
}

impl ParamHandle {
    /// The parameter `field` picks out of `params`, such as
    /// `|p| &p.cutoff`. The field is checked by the compiler: there are no
    /// string identifiers in interface code.
    pub fn new<P, T, F>(params: Arc<P>, field: F) -> Self
    where
        P: Send + Sync + 'static,
        T: Param + 'static,
        F: Fn(&P) -> &T + 'static,
    {
        let id = field(&params).param_id();
        ParamHandle {
            lookup: Rc::new(Field {
                set: params,
                field,
                param: PhantomData,
            }),
            id,
        }
    }

    /// The parameter of `params` whose id is `id`, or `None` if there is
    /// none. For interfaces built from a parameter list rather than typed
    /// fields; the parameter is found again on every read, by walking the
    /// set, which allocates nothing.
    pub fn by_id(params: Arc<dyn ParamSet>, id: ParamId) -> Option<Self> {
        params.find_param(id)?;
        Some(ParamHandle {
            lookup: Rc::new(ById { set: params, id }),
            id,
        })
    }

    /// The parameter.
    pub fn param(&self) -> Option<&dyn Param> {
        self.lookup.param()
    }

    /// The parameter's id, as gestures carry it.
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// The name shown to people, or an empty string if the parameter has
    /// gone.
    pub fn name(&self) -> &str {
        self.param().map_or("", |param| param.name())
    }

    /// The current position in `0..=1`. One atomic load.
    pub fn normalized(&self) -> f64 {
        self.param().map_or(0.0, |param| param.normalized())
    }

    /// The position in `0..=1` the parameter starts at.
    pub fn default_normalized(&self) -> f64 {
        self.param().map_or(0.0, |param| param.default_normalized())
    }

    /// How many steps separate the lowest and highest value, or `None` for
    /// a continuous parameter.
    pub fn step_count(&self) -> Option<u32> {
        self.param().and_then(|param| param.step_count())
    }

    /// The value at `normalized`, in the parameter's own unit.
    pub fn plain(&self, normalized: f64) -> f64 {
        self.param()
            .map_or(0.0, |param| param.normalized_to_plain(normalized))
    }

    /// The position of `plain`, a value in the parameter's own unit.
    pub fn to_normalized(&self, plain: f64) -> f64 {
        self.param()
            .map_or(0.0, |param| param.plain_to_normalized(plain))
    }

    /// The parameter's current value as text in its own words, followed
    /// by its unit: [`Param::display`]. Allocates.
    pub fn display(&self) -> String {
        self.param()
            .map_or_else(String::new, |param| param.display())
    }

    /// The parameter's current value, in its own unit.
    pub fn value(&self) -> f64 {
        self.param().map_or(0.0, |param| param.plain())
    }

    /// The position, in `0..=1`, of the value `text` reads as: text a
    /// person typed, with or without the unit, or text
    /// [`text`](Self::text) wrote, read by
    /// [`Param::parse_display`] and so by the parameter's own parser.
    /// `None` if it reads as no value, or the parameter has gone.
    /// Allocates.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use manguebyte_params::{FloatParam, FloatRange, param_set};
    /// # use manguebyte_ui::ParamHandle;
    /// # struct Params { gain: FloatParam }
    /// # param_set!(Params { gain });
    /// let params = Arc::new(Params {
    ///     gain: FloatParam::new("gain", "Gain", FloatRange::linear(-24.0, 24.0), 0.0).with_unit("dB"),
    /// });
    /// let gain = ParamHandle::new(params, |p| &p.gain);
    /// assert_eq!(gain.parse("12 dB"), Some(0.75));
    /// assert_eq!(gain.parse("-24"), Some(0.0));
    /// assert_eq!(gain.parse("loud"), None);
    /// ```
    pub fn parse(&self, text: &str) -> Option<f64> {
        let param = self.param()?;
        let plain = param.parse_display(text.trim())?;
        let normalized = param.plain_to_normalized(plain);
        normalized.is_finite().then_some(normalized)
    }

    /// The value at `normalized` as text in the parameter's own words,
    /// followed by its unit, as [`Param::display`] shows the current value.
    /// Allocates.
    pub fn text(&self, normalized: f64) -> String {
        let Some(param) = self.param() else {
            return String::new();
        };
        let text = param.plain_to_text(param.normalized_to_plain(normalized));
        match param.info().unit() {
            "" => text,
            unit => format!("{text} {unit}"),
        }
    }
}

impl fmt::Debug for ParamHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParamHandle")
            .field("id", &self.id)
            .field("name", &self.name())
            .field("normalized", &self.normalized())
            .finish()
    }
}

/// A value a control sent that the parameter may not show yet.
#[derive(Debug, Clone, Copy)]
struct Sent {
    /// The value sent, in `0..=1`.
    value: f64,
    /// The parameter's own value, as bits, just before it was sent.
    before: u64,
    /// When the wait for the host's echo began, on the interface's clock.
    at: Duration,
}

/// `value`, which is a number, clamped to `0..=1` with no negative zero, as
/// a [`Gesture`] clamps what it sends.
fn unit(value: f64) -> f64 {
    if value > 1.0 {
        1.0
    } else if value > 0.0 {
        value
    } else {
        0.0
    }
}

/// What a control keeps of the parameter it changes: the handle, the
/// value it last showed, its open gesture and the last value it sent.
///
/// [`poll`](Self::poll) once per frame says whether the value to show has
/// changed. The editing methods report a person's edits to the host
/// through the editor contract's
/// [`Gesture`](manguebyte_plugin::editor::Gesture), which keeps them well
/// formed: one begin, values, one end, and the end sent even if the
/// control is dropped in the middle of a drag.
///
/// The value shown is, in order of preference:
///
/// 1. while a gesture is open, the last value it sent, or the value it
///    began at;
/// 2. after an edit, the value sent, while the parameter still reads the
///    value it read just before that value was sent, and for less than
///    [`ECHO_WAIT`](Self::ECHO_WAIT) after the edit;
/// 3. otherwise the parameter's own value.
///
/// An edit that starts from the value shown (a drag, an arrow key, a
/// wheel notch, a switch turned over) takes it from
/// [`current`](Self::current), which reads it again at that moment.
pub struct ParamBinding {
    handle: ParamHandle,
    gesture: Option<Gesture>,
    /// The parameter's own value, as bits, just before the open gesture
    /// last sent a value.
    sent_before: u64,
    /// The last value sent, while it may still be waiting for its echo.
    waiting: Option<Sent>,
    /// The latest time a poll was given.
    now: Duration,
    /// The value shown; `NaN` before the first read.
    shown: f64,
    /// Whether `shown` is the parameter's own value.
    own: bool,
}

impl ParamBinding {
    /// How long a control keeps showing a value it sent, and moving on
    /// from it, while the parameter still reads what it did before: long
    /// enough for a host that stores an edit only when it next processes
    /// audio, short enough that a value the host never takes is soon
    /// replaced by the parameter's own.
    pub const ECHO_WAIT: Duration = Duration::from_millis(250);

    /// A binding to `handle`, with nothing shown yet.
    pub fn new(handle: ParamHandle) -> Self {
        ParamBinding {
            handle,
            gesture: None,
            sent_before: 0,
            waiting: None,
            now: Duration::ZERO,
            shown: f64::NAN,
            own: true,
        }
    }

    /// The parameter.
    pub fn handle(&self) -> &ParamHandle {
        &self.handle
    }

    fn param_bits(&self) -> u64 {
        self.handle.normalized().to_bits()
    }

    /// Whether `sent` is still the value to show: the parameter reads what
    /// it read before, and the wait is not over.
    fn holds(&self, sent: &Sent) -> bool {
        self.param_bits() == sent.before && self.now.saturating_sub(sent.at) < Self::ECHO_WAIT
    }

    /// The value to show now, and whether it is the parameter's own.
    fn read(&self) -> (f64, bool) {
        if let Some(gesture) = self.gesture.as_ref().filter(|gesture| gesture.is_live()) {
            if let Some(value) = gesture.sent().or_else(|| gesture.start()) {
                return (value, false);
            }
        }
        match &self.waiting {
            Some(sent) if self.holds(sent) => (sent.value, false),
            _ => (self.handle.normalized(), true),
        }
    }

    /// Reads the value to show, forgetting a value sent that no longer is,
    /// and returns whether it differs from the value shown last.
    fn refresh(&mut self) -> bool {
        if self.waiting.is_some_and(|sent| !self.holds(&sent)) {
            self.waiting = None;
        }
        let (value, own) = self.read();
        let changed = value.to_bits() != self.shown.to_bits();
        self.shown = value;
        self.own = own;
        changed
    }

    /// Reads the value to show at time `now`, on the clock that drives the
    /// interface, and returns whether it differs from the value shown
    /// last. The first poll counts as a change. If `now` goes backwards, no
    /// time is taken to have passed.
    pub fn poll(&mut self, now: Duration) -> bool {
        self.now = self.now.max(now);
        self.refresh()
    }

    /// Reads the value to show again, now, and returns it: what an edit
    /// that moves on from the value shown should start from. The parameter
    /// may have changed since the last [`poll`](Self::poll), through host
    /// automation for one.
    pub fn current(&mut self) -> f64 {
        self.refresh();
        self.shown
    }

    /// The value shown, in `0..=1`, as of the last [`poll`](Self::poll) or
    /// edit. Before the first, the value to show now.
    pub fn shown(&self) -> f64 {
        if self.shown.is_nan() {
            self.read().0
        } else {
            self.shown
        }
    }

    /// Whether the value shown is one the control sent, during a gesture
    /// or while it waits for the host's echo, rather than the parameter's
    /// own.
    pub fn shows_sent(&self) -> bool {
        if self.shown.is_nan() {
            !self.read().1
        } else {
            !self.own
        }
    }

    /// Whether a gesture is open.
    pub fn is_editing(&self) -> bool {
        self.gesture.is_some()
    }

    /// Begins a gesture, unless one is open, starting from the value to
    /// show at this moment ([`current`](Self::current)): what
    /// [`cancel`](Self::cancel) puts back.
    pub fn begin(&mut self, context: &EditorContext) {
        if self.gesture.is_none() {
            let start = self.current();
            self.sent_before = self.param_bits();
            self.gesture = Some(Gesture::begin(context, self.handle.id(), start));
        }
        self.refresh();
    }

    /// Sends `normalized` within the open gesture; nothing if none is open.
    /// The value is clamped to `0..=1` and a value that is not a number is
    /// ignored.
    pub fn set(&mut self, normalized: f64) {
        if let Some(gesture) = &mut self.gesture {
            let before = self.handle.normalized().to_bits();
            let last = gesture.sent().map(f64::to_bits);
            gesture.set(normalized);
            // A value equal to the last one sent is not sent again.
            if gesture.sent().map(f64::to_bits) != last {
                self.sent_before = before;
            }
        }
        self.refresh();
    }

    /// Sends `normalized` as a whole edit (begin, value, end), or within
    /// the open gesture if there is one. The value is clamped to `0..=1`
    /// and a value that is not a number is ignored.
    pub fn set_once(&mut self, context: &EditorContext, normalized: f64) {
        if self.gesture.is_some() {
            self.set(normalized);
            return;
        }
        if !normalized.is_nan() {
            let before = self.param_bits();
            Gesture::set_once(context, self.handle.id(), normalized);
            if context.is_attached() {
                self.waiting = Some(Sent {
                    value: unit(normalized),
                    before,
                    at: self.now,
                });
            }
        }
        self.refresh();
    }

    /// Ends the open gesture, if any. The last value it sent is shown
    /// until the host echoes it, or for [`ECHO_WAIT`](Self::ECHO_WAIT).
    pub fn end(&mut self) {
        if let Some(gesture) = self.gesture.take() {
            let sent = if gesture.is_live() {
                gesture.sent()
            } else {
                None
            };
            gesture.end();
            if let Some(value) = sent {
                self.waiting = Some(Sent {
                    value,
                    before: self.sent_before,
                    at: self.now,
                });
            }
        }
        self.refresh();
    }

    /// Ends the open gesture after putting back the value it began at,
    /// which is shown until the host echoes it, or for
    /// [`ECHO_WAIT`](Self::ECHO_WAIT).
    pub fn cancel(&mut self) {
        if let Some(gesture) = self.gesture.take() {
            // The start is sent back only if something was sent since.
            let restored = if gesture.is_live() && gesture.sent().is_some() {
                gesture.start()
            } else {
                None
            };
            let before = self.param_bits();
            gesture.cancel();
            if let Some(value) = restored {
                self.waiting = Some(Sent {
                    value,
                    before,
                    at: self.now,
                });
            }
        }
        self.refresh();
    }

    /// The value shown, as text in the parameter's own words: a value the
    /// control sent ([`shows_sent`](Self::shows_sent)) through
    /// [`ParamHandle::text`], otherwise the parameter's own
    /// ([`Param::display`]). Allocates.
    pub fn text(&self) -> String {
        if self.shows_sent() {
            self.handle.text(self.shown())
        } else {
            self.handle.display()
        }
    }

    /// The value shown, in the parameter's own unit: a value the control
    /// sent, or the parameter's own.
    pub fn plain(&self) -> f64 {
        if self.shows_sent() {
            self.handle.plain(self.shown())
        } else {
            self.handle.value()
        }
    }
}

impl fmt::Debug for ParamBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParamBinding")
            .field("handle", &self.handle)
            .field("editing", &self.is_editing())
            .field("shown", &self.shown)
            .field("waiting", &self.waiting.map(|sent| sent.value))
            .finish()
    }
}
