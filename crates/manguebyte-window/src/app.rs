//! Application windows and the loop that runs them.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::rc::{Rc, Weak};

use manguebyte_gfx::{MAX_SIDE, Size};
use manguebyte_plugin::editor::checked_scale;

use crate::dispatch::WindowHandler;
use crate::event::{PhysicalPoint, PhysicalSize, TimerId};
use crate::headless::{Headless, HeadlessPlatform};
use crate::window::{Inner, Platform, Window, WindowError, WindowOptions};

/// How to make an application window: its title, its size in logical
/// pixels, whether the person may resize it and within what.
///
/// ```
/// use manguebyte_gfx::Size;
/// use manguebyte_window::AppWindowOptions;
///
/// let options = AppWindowOptions::new("Parabolic", Size::new(800.0, 500.0))
///     .with_min_size(Size::new(480.0, 320.0))
///     .with_file_drops();
/// assert!(options.resizable);
/// assert!(options.accepts_drops);
/// assert!(!options.fixed().resizable);
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AppWindowOptions {
    /// The title in the window's title bar and the taskbar.
    pub title: String,
    /// The size of the window's inside (its client area), in logical
    /// pixels: the window is made this size times the system's scale for
    /// the monitor it opens on.
    pub size: Size,
    /// Whether the person may resize the window by its frame and maximise
    /// it. [`Window::resize`] works either way.
    pub resizable: bool,
    /// The smallest inside, in logical pixels.
    pub min_size: Option<Size>,
    /// The largest inside, in logical pixels.
    pub max_size: Option<Size>,
    /// Whether the window is shown at once.
    pub visible: bool,
    /// Whether showing the window makes it the active window, as an
    /// application's first window should. Tests turn it off so as not to
    /// take the keyboard from the person at the desktop.
    pub activate: bool,
    /// Where the window's outer top-left corner goes, in device pixels of
    /// the desktop, or the system's choice when `None`.
    pub position: Option<PhysicalPoint>,
    /// Whether files and text dragged onto the window reach its handler
    /// (see [`WindowOptions::with_file_drops`]).
    pub accepts_drops: bool,
}

impl AppWindowOptions {
    /// A visible, resizable window titled `title`, `size` logical pixels
    /// inside, with no limits but the system's, placed by the system, that
    /// takes no drops.
    pub fn new(title: impl Into<String>, size: Size) -> AppWindowOptions {
        AppWindowOptions {
            title: title.into(),
            size,
            resizable: true,
            min_size: None,
            max_size: None,
            visible: true,
            activate: true,
            position: None,
            accepts_drops: false,
        }
    }

    /// The same, of a size the person cannot change.
    #[must_use]
    pub fn fixed(mut self) -> AppWindowOptions {
        self.resizable = false;
        self
    }

    /// The same, never smaller than `size` inside.
    #[must_use]
    pub fn with_min_size(mut self, size: Size) -> AppWindowOptions {
        self.min_size = Some(size);
        self
    }

    /// The same, never larger than `size` inside.
    #[must_use]
    pub fn with_max_size(mut self, size: Size) -> AppWindowOptions {
        self.max_size = Some(size);
        self
    }

    /// The same, hidden until [`set_visible`](Window::set_visible).
    #[must_use]
    pub fn hidden(mut self) -> AppWindowOptions {
        self.visible = false;
        self
    }

    /// The same, shown without becoming the active window.
    #[must_use]
    pub fn without_activation(mut self) -> AppWindowOptions {
        self.activate = false;
        self
    }

    /// The same, with its outer top-left corner at `position`.
    #[must_use]
    pub fn at(mut self, position: PhysicalPoint) -> AppWindowOptions {
        self.position = Some(position);
        self
    }

    /// The same, taking files and text dragged onto it (see
    /// [`WindowOptions::with_file_drops`]).
    #[must_use]
    pub fn with_file_drops(mut self) -> AppWindowOptions {
        self.accepts_drops = true;
        self
    }
}

/// A length in logical pixels, in device pixels at `scale`: whole pixels,
/// at least one and at most [`MAX_SIDE`].
fn device_side(logical: f32, scale: f64) -> u32 {
    let pixels = (f64::from(logical) * scale).round();
    // `as` saturates, which is what is wanted at the top.
    (pixels as u32).clamp(1, MAX_SIDE)
}

/// A size in logical pixels, in device pixels at `scale`.
pub(crate) fn device_size(size: Size, scale: f64) -> PhysicalSize {
    PhysicalSize::new(
        device_side(size.width, scale),
        device_side(size.height, scale),
    )
}

fn usable(size: Size) -> bool {
    size.width.is_finite() && size.height.is_finite() && size.width > 0.0 && size.height > 0.0
}

/// Checks the options an application window is made with.
fn check(options: &AppWindowOptions) -> Result<(), WindowError> {
    if !usable(options.size) {
        return Err(WindowError::InvalidOptions(
            "the size is not a positive number",
        ));
    }
    for limit in [options.min_size, options.max_size].into_iter().flatten() {
        if !usable(limit) {
            return Err(WindowError::InvalidOptions(
                "a size limit is not a positive number",
            ));
        }
    }
    if let (Some(min), Some(max)) = (options.min_size, options.max_size) {
        if min.width > max.width || min.height > max.height {
            return Err(WindowError::InvalidOptions(
                "the minimum size is larger than the maximum",
            ));
        }
    }
    Ok(())
}

/// What an application window has that a child window does not: a title
/// and limits to its size.
pub(crate) struct Frame {
    pub(crate) title: RefCell<String>,
    pub(crate) resizable: bool,
    pub(crate) min: Option<Size>,
    pub(crate) max: Option<Size>,
}

impl Frame {
    fn new(options: &AppWindowOptions) -> Frame {
        Frame {
            title: RefCell::new(
                options
                    .title
                    .split('\0')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            resizable: options.resizable,
            min: options.min_size,
            max: options.max_size,
        }
    }

    /// The smallest and largest inside in device pixels at `scale`.
    pub(crate) fn limits(&self, scale: f64) -> (PhysicalSize, PhysicalSize) {
        let min = self
            .min
            .map_or(PhysicalSize::new(1, 1), |min| device_size(min, scale));
        let max = self
            .max
            .map_or(PhysicalSize::new(MAX_SIDE, MAX_SIDE), |max| {
                device_size(max, scale)
            });
        let max = PhysicalSize::new(max.width.max(min.width), max.height.max(min.height));
        (min, max)
    }

    /// `size` within the limits at `scale`.
    pub(crate) fn clamp(&self, size: PhysicalSize, scale: f64) -> PhysicalSize {
        let (min, max) = self.limits(scale);
        PhysicalSize::new(
            size.width.clamp(min.width, max.width),
            size.height.clamp(min.height, max.height),
        )
    }
}

/// Why [`App::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RunEnd {
    /// No window the application opened is open any more.
    LastWindowClosed,
    /// [`App::quit`] was called.
    Quit,
    /// The thread's queue gave the system's quit message (`WM_QUIT` on
    /// Win32, which someone else posted), with its exit code. The loop
    /// does not post it again.
    QuitMessage(i32),
    /// A headless application ran every step a test posted, with windows
    /// still open: where a system loop would wait for the next message.
    Idle,
}

/// The backend under an application.
enum AppBackend {
    Headless {
        steps: RefCell<VecDeque<Box<dyn FnOnce()>>>,
        scale: Cell<f64>,
    },
    #[cfg(windows)]
    Win32(crate::win32::app::Win32App),
}

/// What an application timer calls.
type Tick = Box<dyn FnMut(&App)>;

/// A timer of the application: how often it ticks and what it calls.
struct AppTimer {
    id: TimerId,
    period_ms: u32,
    /// Which setting of the timer this is, so that a call that ends after
    /// its timer was set again or killed is not put back.
    serial: u64,
    /// Out while it runs, so that a tick a nested loop lets through
    /// meanwhile (a file dialog's, a move of a window) is dropped.
    tick: Option<Tick>,
    /// The system's id of the timer.
    #[cfg(windows)]
    system: usize,
}

/// What an application and its windows share.
pub(crate) struct AppInner {
    backend: AppBackend,
    windows: RefCell<Vec<Weak<Inner>>>,
    quit: Cell<bool>,
    running: Cell<bool>,
    timers: RefCell<Vec<AppTimer>>,
    serials: Cell<u64>,
}

impl AppInner {
    fn new(backend: AppBackend) -> AppInner {
        AppInner {
            backend,
            windows: RefCell::new(Vec::new()),
            quit: Cell::new(false),
            running: Cell::new(false),
            timers: RefCell::new(Vec::new()),
            serials: Cell::new(0),
        }
    }

    /// Runs the call of the first timer `find` picks, unless it is running
    /// already. Returns whether it ran.
    fn fire(inner: &Rc<AppInner>, find: impl Fn(&AppTimer) -> bool) -> bool {
        let taken = {
            let mut timers = inner.timers.borrow_mut();
            timers
                .iter_mut()
                .find(|timer| find(timer))
                .and_then(|timer| Some((timer.serial, timer.tick.take()?)))
        };
        let Some((serial, mut tick)) = taken else {
            return false;
        };
        tick(&App {
            inner: Rc::clone(inner),
        });
        let mut timers = inner.timers.borrow_mut();
        if let Some(timer) = timers.iter_mut().find(|timer| timer.serial == serial) {
            timer.tick = Some(tick);
        } else {
            // Set again or killed while it ran: the call goes, outside the
            // borrow, since what it holds may reach the timers as it drops.
            drop(timers);
            drop(tick);
        }
        true
    }

    /// Runs the call of the timer the system knows as `system`.
    #[cfg(windows)]
    pub(crate) fn fire_system(inner: &Rc<AppInner>, system: usize) -> bool {
        AppInner::fire(inner, |timer| timer.system == system)
    }

    /// Stops the timer the system knows as `system`, as after its call
    /// panicked.
    #[cfg(windows)]
    pub(crate) fn kill_system(inner: &Rc<AppInner>, system: usize) {
        let id = inner
            .timers
            .borrow()
            .iter()
            .find(|timer| timer.system == system)
            .map(|timer| timer.id);
        match id {
            Some(id) => {
                App {
                    inner: Rc::clone(inner),
                }
                .kill_timer(id);
            }
            None => crate::win32::app::kill_timer(system),
        }
    }

    /// The application's windows whose values still exist, in the order
    /// they were opened.
    fn windows(&self) -> Vec<Rc<Inner>> {
        let mut windows = self.windows.borrow_mut();
        windows.retain(|window| window.strong_count() > 0);
        windows.iter().filter_map(Weak::upgrade).collect()
    }

    fn open_count(&self) -> usize {
        // Taken out of the list first, so that a window whose last
        // reference goes here does not drop while the list is borrowed.
        let windows = self.windows();
        let open = windows.iter().filter(|window| window.is_open()).count();
        drop(windows);
        open
    }
}

impl Drop for AppInner {
    fn drop(&mut self) {
        let timers = std::mem::take(self.timers.get_mut());
        #[cfg(windows)]
        if matches!(self.backend, AppBackend::Win32(_)) {
            for timer in &timers {
                crate::win32::app::kill_timer(timer.system);
            }
        }
        drop(timers);
    }
}

/// Clears the running flag however a run ends.
struct Running<'a>(&'a Cell<bool>);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// An application: the windows it opens and the loop that runs them.
///
/// An application owns its thread, as a standalone program's main thread
/// is, which a plugin inside a host never does: a plugin's windows are
/// children of the host's, made with [`Window::open`], and run on the
/// host's loop. An `App` and its windows belong to the thread that made
/// them. Its windows run the same code as child windows for drawing,
/// timers, input, the clipboard and the scale; what they add is a frame
/// with a title, limits to their size, the close request
/// ([`Event::CloseRequested`](crate::Event::CloseRequested)) and a move to a
/// monitor of another scale.
///
/// [`run`](Self::run) runs the loop until the last window the application
/// opened is closed or [`quit`](Self::quit) is asked. On Win32 the
/// application initialises OLE for its thread when it is made (drops and
/// the file dialogs need it) and uninitialises it when the application and
/// all its windows are gone; it never posts `WM_QUIT`, so a loop outside it
/// is not ended by it.
///
/// A headless application ([`App::headless`]) makes headless windows and
/// runs a loop of steps a test posts ([`HeadlessApp::post`]) in place of
/// system messages, on every system.
///
/// ```
/// use std::cell::RefCell;
/// use std::rc::Rc;
/// use manguebyte_gfx::Size;
/// use manguebyte_window::{App, AppWindowOptions, Event, Handled, RunEnd, WindowCx};
///
/// let (app, test) = App::headless();
/// let log = Rc::new(RefCell::new(Vec::new()));
/// let seen = Rc::clone(&log);
/// let window = app
///     .open_window(AppWindowOptions::new("Notes", Size::new(300.0, 200.0)), move |cx: &WindowCx<'_>, event| {
///         seen.borrow_mut().push(event);
///         // The application agrees to every close request.
///         if event == Event::CloseRequested {
///             cx.close();
///         }
///         Handled::Yes
///     })
///     .expect("valid options");
/// assert_eq!(window.title().as_deref(), Some("Notes"));
///
/// // The person clicks the close button while the loop runs.
/// let controller = test.windows()[0].clone();
/// test.post(move || {
///     controller.request_close();
/// });
/// assert_eq!(app.run(), Ok(RunEnd::LastWindowClosed));
/// assert_eq!(*log.borrow(), [Event::CloseRequested, Event::Closed]);
/// ```
#[derive(Clone)]
pub struct App {
    inner: Rc<AppInner>,
}

impl App {
    /// An application on the system's windows, for this thread.
    ///
    /// # Errors
    ///
    /// [`WindowError::Unsupported`] where there is no backend for
    /// application windows: in this version anywhere but Windows.
    pub fn new() -> Result<App, WindowError> {
        #[cfg(windows)]
        {
            Ok(App {
                inner: Rc::new(AppInner::new(AppBackend::Win32(
                    crate::win32::app::Win32App::new(),
                ))),
            })
        }
        #[cfg(not(windows))]
        {
            let api = if cfg!(target_os = "macos") {
                crate::WindowApi::Cocoa
            } else {
                crate::WindowApi::X11
            };
            Err(WindowError::Unsupported(api))
        }
    }

    /// An application whose windows are headless, and the controller a
    /// test drives its loop with, on every system.
    pub fn headless() -> (App, HeadlessApp) {
        let inner = Rc::new(AppInner::new(AppBackend::Headless {
            steps: RefCell::new(VecDeque::new()),
            scale: Cell::new(1.0),
        }));
        let controller = HeadlessApp {
            inner: Rc::downgrade(&inner),
        };
        (App { inner }, controller)
    }

    /// Whether the application's windows are headless.
    pub fn is_headless(&self) -> bool {
        matches!(self.inner.backend, AppBackend::Headless { .. })
    }

    /// Opens a top-level window of the application, whose events go to
    /// `handler`. The window is as large inside as the options say, in
    /// logical pixels, at the system's scale for where it opens.
    ///
    /// # Errors
    ///
    /// [`WindowError::InvalidOptions`] for sizes that are not positive
    /// numbers or limits that contradict each other, or
    /// [`WindowError::Platform`] if the system refuses.
    pub fn open_window(
        &self,
        options: AppWindowOptions,
        handler: impl WindowHandler + 'static,
    ) -> Result<Window, WindowError> {
        check(&options)?;
        let frame = Frame::new(&options);
        let inner = match &self.inner.backend {
            AppBackend::Headless { scale, .. } => {
                let scale = scale.get();
                let size = frame.clamp(device_size(options.size, scale), scale);
                let window_options = WindowOptions {
                    size,
                    scale: None,
                    visible: options.visible,
                    focus_on_click: true,
                    accepts_drops: options.accepts_drops,
                };
                let inner = Rc::new(Inner::with_frame(
                    Platform::Headless(HeadlessPlatform::new(options.visible)),
                    &window_options,
                    Box::new(handler),
                    Some(frame),
                    Some(Rc::clone(&self.inner)),
                ));
                inner.init_system_scale(scale);
                inner.drops.set(options.accepts_drops);
                inner
            }
            #[cfg(windows)]
            AppBackend::Win32(app) => crate::win32::open_top_level(
                &options,
                frame,
                app.has_ole(),
                Rc::clone(&self.inner),
                Box::new(handler),
            )?,
        };
        self.inner.windows.borrow_mut().push(Rc::downgrade(&inner));
        Ok(Window::from_inner(inner))
    }

    /// Runs the loop until no window of the application is open, or
    /// [`quit`](Self::quit) is asked, and says which. Returns at once if
    /// no window is open. Windows of the application that are hidden count
    /// as open.
    ///
    /// On Win32 this is the thread's message loop (`GetMessageW`,
    /// `TranslateMessage`, `DispatchMessageW`): every window of the
    /// thread, the application's or not, gets its messages through it. A
    /// headless application runs the steps a test posted, in order, and
    /// returns [`RunEnd::Idle`] when there are no more.
    ///
    /// # Errors
    ///
    /// [`WindowError::Busy`] when called while the loop is already running
    /// (from inside a handler), or [`WindowError::Platform`] if the
    /// system's loop fails.
    pub fn run(&self) -> Result<RunEnd, WindowError> {
        let inner = &self.inner;
        if inner.running.replace(true) {
            return Err(WindowError::Busy);
        }
        let _running = Running(&inner.running);
        loop {
            if inner.quit.replace(false) {
                return Ok(RunEnd::Quit);
            }
            if inner.open_count() == 0 {
                return Ok(RunEnd::LastWindowClosed);
            }
            match &inner.backend {
                AppBackend::Headless { steps, .. } => {
                    let step = steps.borrow_mut().pop_front();
                    match step {
                        Some(step) => step(),
                        None => return Ok(RunEnd::Idle),
                    }
                }
                #[cfg(windows)]
                AppBackend::Win32(app) => {
                    if let Some(code) = app.next_message()? {
                        return Ok(RunEnd::QuitMessage(code));
                    }
                }
            }
        }
    }

    /// Asks [`run`](Self::run) to return once the event being handled now
    /// is done. Asked while the loop is not running, it ends the next run
    /// at once. Windows stay open.
    pub fn quit(&self) {
        self.inner.quit.set(true);
    }

    /// Whether [`run`](Self::run) is running now.
    pub fn is_running(&self) -> bool {
        self.inner.running.get()
    }

    /// How many windows of the application are open.
    pub fn open_windows(&self) -> usize {
        self.inner.open_count()
    }

    /// Starts or restarts the application's timer `id`, which calls `tick`
    /// about every `period_ms` milliseconds on the application's thread,
    /// between the events of its windows, until it is killed or the
    /// application goes: for work that is the program's and no window's,
    /// such as asking the audio devices what changed. Setting an id again
    /// replaces its call and period. Win32 fires no faster than every
    /// 10 ms.
    ///
    /// Ticks come while the thread takes messages: while [`run`](Self::run)
    /// runs, and inside the loops a call made from it runs, such as a file
    /// dialog's or the system's while the person moves a window. A tick
    /// that comes while the timer's own call still runs (from a loop inside
    /// it) is dropped, and late ticks are not made up. A call may set or
    /// kill its own timer or any other. On Win32 a call that panics is
    /// caught there, and its timer killed.
    ///
    /// The call is given the application, so it need not keep a clone of
    /// it: one would keep the application alive through its own timer.
    ///
    /// A headless application keeps its timers for the test to fire
    /// ([`HeadlessApp::fire_timers`]).
    ///
    /// ```
    /// use std::cell::Cell;
    /// use std::rc::Rc;
    /// use manguebyte_window::{App, TimerId};
    ///
    /// let (app, test) = App::headless();
    /// let polls = Rc::new(Cell::new(0));
    /// let counted = Rc::clone(&polls);
    /// app.set_timer(TimerId(1), 100, move |app: &App| {
    ///     counted.set(counted.get() + 1);
    ///     if counted.get() == 2 {
    ///         app.kill_timer(TimerId(1));
    ///     }
    /// })
    /// .expect("a period");
    /// assert_eq!(test.timers(), [(TimerId(1), 100)]);
    /// assert_eq!(test.fire_timers(), 1);
    /// assert_eq!(test.fire_timers(), 1);
    /// assert_eq!(test.fire_timers(), 0, "it killed itself");
    /// assert_eq!(polls.get(), 2);
    /// ```
    ///
    /// # Errors
    ///
    /// [`WindowError::InvalidPeriod`] for 0, or [`WindowError::Platform`]
    /// if the system refuses.
    pub fn set_timer(
        &self,
        id: TimerId,
        period_ms: u32,
        tick: impl FnMut(&App) + 'static,
    ) -> Result<(), WindowError> {
        if period_ms == 0 {
            return Err(WindowError::InvalidPeriod);
        }
        #[cfg(windows)]
        let system = match &self.inner.backend {
            AppBackend::Win32(_) => {
                let running = self
                    .inner
                    .timers
                    .borrow()
                    .iter()
                    .find(|timer| timer.id == id)
                    .map(|timer| timer.system);
                crate::win32::app::set_timer(running, period_ms, Rc::downgrade(&self.inner))?
            }
            AppBackend::Headless { .. } => 0,
        };
        let serial = self.inner.serials.get() + 1;
        self.inner.serials.set(serial);
        let timer = AppTimer {
            id,
            period_ms,
            serial,
            tick: Some(Box::new(tick)),
            #[cfg(windows)]
            system,
        };
        let replaced = {
            let mut timers = self.inner.timers.borrow_mut();
            match timers.iter_mut().find(|kept| kept.id == id) {
                Some(kept) => Some(std::mem::replace(kept, timer)),
                None => {
                    timers.push(timer);
                    None
                }
            }
        };
        // Outside the borrow, as in a kill.
        drop(replaced);
        Ok(())
    }

    /// Stops the application's timer `id`. Returns whether it was running.
    /// A call that runs now finishes, and no tick comes after.
    pub fn kill_timer(&self, id: TimerId) -> bool {
        let removed = {
            let mut timers = self.inner.timers.borrow_mut();
            let index = timers.iter().position(|timer| timer.id == id);
            index.map(|index| timers.remove(index))
        };
        let Some(timer) = removed else {
            return false;
        };
        #[cfg(windows)]
        if matches!(self.inner.backend, AppBackend::Win32(_)) {
            crate::win32::app::kill_timer(timer.system);
        }
        // What the call holds may reach the timers as it drops.
        drop(timer);
        true
    }

    /// The application's running timers and their periods in
    /// milliseconds, in the order they were first set.
    pub fn timers(&self) -> Vec<(TimerId, u32)> {
        self.inner
            .timers
            .borrow()
            .iter()
            .map(|timer| (timer.id, timer.period_ms))
            .collect()
    }
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("App")
            .field("headless", &self.is_headless())
            .field("open_windows", &self.open_windows())
            .field("running", &self.is_running())
            .finish()
    }
}

/// The test's side of a headless application, from [`App::headless`].
///
/// It plays the system's message queue: what it posts, the loop runs, one
/// step at a time, as it would deliver messages; and it hands out the
/// [`Headless`] controller of each window the application opened. It holds
/// the application weakly, so a step may keep a clone of it.
#[derive(Clone)]
pub struct HeadlessApp {
    inner: Weak<AppInner>,
}

impl HeadlessApp {
    /// Adds `step` to what the loop runs: after every step posted before
    /// it, while [`App::run`] runs. A step may post more. Returns false
    /// once the application is gone.
    pub fn post(&self, step: impl FnOnce() + 'static) -> bool {
        let Some(inner) = self.inner.upgrade() else {
            return false;
        };
        match &inner.backend {
            AppBackend::Headless { steps, .. } => {
                steps.borrow_mut().push_back(Box::new(step));
                true
            }
            #[cfg(windows)]
            AppBackend::Win32(_) => false,
        }
    }

    /// How many posted steps wait to run.
    pub fn pending_steps(&self) -> usize {
        self.inner
            .upgrade()
            .map_or(0, |inner| match &inner.backend {
                AppBackend::Headless { steps, .. } => steps.borrow().len(),
                #[cfg(windows)]
                AppBackend::Win32(_) => 0,
            })
    }

    /// The controllers of the application's windows whose values still
    /// exist, in the order they were opened.
    pub fn windows(&self) -> Vec<Headless> {
        let Some(inner) = self.inner.upgrade() else {
            return Vec::new();
        };
        let windows = inner.windows();
        windows
            .iter()
            .map(|window| Headless::new(Rc::downgrade(window)))
            .collect()
    }

    /// The application's running timers and their periods, as
    /// [`App::timers`] says.
    pub fn timers(&self) -> Vec<(TimerId, u32)> {
        self.inner
            .upgrade()
            .map_or_else(Vec::new, |inner| App { inner }.timers())
    }

    /// Fires each of the application's running timers once, in the order
    /// they were first set, as the system would when their periods pass,
    /// and returns how many calls ran. A timer a call sets meanwhile fires
    /// the next time, and one a call kills does not fire.
    pub fn fire_timers(&self) -> usize {
        let Some(inner) = self.inner.upgrade() else {
            return 0;
        };
        let serials: Vec<u64> = inner
            .timers
            .borrow()
            .iter()
            .map(|timer| timer.serial)
            .collect();
        serials
            .into_iter()
            .filter(|&serial| AppInner::fire(&inner, |timer| timer.serial == serial))
            .count()
    }

    /// Sets the system's scale for windows opened from now on, as if they
    /// opened on a monitor of that scale. A scale outside `0.25..=8.0` is
    /// ignored. Returns whether it was taken.
    pub fn set_system_scale(&self, scale: f64) -> bool {
        let Some(inner) = self.inner.upgrade() else {
            return false;
        };
        match (&inner.backend, checked_scale(scale)) {
            (AppBackend::Headless { scale: kept, .. }, Some(scale)) => {
                kept.set(scale);
                true
            }
            _ => false,
        }
    }
}

impl fmt::Debug for HeadlessApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadlessApp")
            .field("exists", &(self.inner.strong_count() > 0))
            .field("pending_steps", &self.pending_steps())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(min: Option<Size>, max: Option<Size>) -> Frame {
        let mut options = AppWindowOptions::new("t", Size::new(100.0, 100.0));
        options.min_size = min;
        options.max_size = max;
        Frame::new(&options)
    }

    #[test]
    fn limits_are_logical_sizes_at_the_scale() {
        let frame = frame(Some(Size::new(200.0, 100.0)), Some(Size::new(400.5, 300.0)));
        assert_eq!(
            frame.limits(1.5),
            (PhysicalSize::new(300, 150), PhysicalSize::new(601, 450))
        );
        assert_eq!(
            frame.clamp(PhysicalSize::new(10, 1000), 1.5),
            PhysicalSize::new(300, 450)
        );
        let free = super::tests::frame(None, None);
        assert_eq!(
            free.limits(2.0),
            (
                PhysicalSize::new(1, 1),
                PhysicalSize::new(MAX_SIDE, MAX_SIDE)
            )
        );
    }

    #[test]
    fn options_that_no_window_can_have_are_refused() {
        let bad = [
            AppWindowOptions::new("t", Size::new(0.0, 10.0)),
            AppWindowOptions::new("t", Size::new(f32::NAN, 10.0)),
            AppWindowOptions::new("t", Size::new(10.0, 10.0)).with_min_size(Size::new(-1.0, 5.0)),
            AppWindowOptions::new("t", Size::new(10.0, 10.0))
                .with_min_size(Size::new(50.0, 5.0))
                .with_max_size(Size::new(40.0, 50.0)),
        ];
        for options in bad {
            assert!(
                matches!(check(&options), Err(WindowError::InvalidOptions(_))),
                "{options:?}"
            );
        }
        assert_eq!(
            check(&AppWindowOptions::new("t", Size::new(1.0, 1.0))),
            Ok(())
        );
    }

    #[test]
    fn a_title_stops_at_a_nul() {
        let frame = Frame::new(&AppWindowOptions::new(
            "Parabolic\0hidden",
            Size::new(1.0, 1.0),
        ));
        assert_eq!(*frame.title.borrow(), "Parabolic");
    }
}
