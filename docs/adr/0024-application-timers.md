# ADR 0024: Timers of an application

Status: accepted, 2026-11-01

## Context

A window of `manguebyte-window` has timers (`Window::set_timer`), which
come to its handler as `Event::Timer`. An application (`App`) had none.
Work that is the program's and no window's had nowhere to run on the
application's thread: following the audio devices
(`manguebyte_gui::audio_settings::sync`, about once a second), saving
the settings now and then, asking a running standalone what it reports.
A program borrowed a window's frame for it, which stops while the window
is hidden, or ran it on a thread of its own, which the standalone does
not need and the interface's state then has to cross (TECH_DEBT
item 115).

What is needed:

- calls on the application's thread, since a standalone and the
  interface's state live there, about every so many milliseconds, until
  stopped;
- set, replaced and stopped from anywhere on that thread, a timer's own
  call included, without a borrow panic;
- ticks while the person holds a menu open or moves a window, as a
  window's timers have, and while a file dialog runs;
- nothing unwinding into the system, as for a window's handler;
- the same behaviour on the headless backend, where a test fires the
  ticks.

JUCE's public documentation describes the analogue: `Timer` is a class a
component or any object inherits, with `startTimer(ms)`, `stopTimer()`
and a `timerCallback()` on the message thread; a tick that comes while
the callback still runs is not delivered again, and late ones are not
made up.

## Decision

`App::set_timer(id, period_ms, tick)` starts or restarts the
application's timer `id` with `tick: FnMut(&App) + 'static`;
`App::kill_timer(id)` stops it; `App::timers()` lists them. Ids are
`TimerId`, as for windows, and setting an id again replaces its call
and period. The call is given the application, so it need not hold a
clone of it, which would keep the application alive through its own
timer.

On Win32 an application timer is a thread timer: `SetTimer` with no
window and a timer procedure. `DispatchMessageW` calls the procedure for
its `WM_TIMER`, so it ticks in `App::run` and in every other loop of the
thread, the system's modal loops for menus and for moving a window, and
a file dialog's, included. The system chooses the ids of thread timers;
a table of the thread maps each to the application, held weakly, and
the application keeps the system's id with each timer. Setting an id
again passes the system's id, which replaces that timer. The procedure
catches a panic and kills the timer whose call panicked.

The call is taken out of its entry while it runs and put back after,
unless the timer was set again or killed meanwhile (each setting has a
serial number). So a tick that comes while the call runs, from a loop
inside it, finds no call and is dropped; the call may set or kill any
timer, its own included; and a call that is replaced or killed is
dropped outside every borrow, since what it holds may reach the timers
as it drops. When the application goes, its timers are killed and their
calls dropped.

A headless application keeps its timers and runs nothing by itself:
`HeadlessApp::fire_timers` fires each running timer once, in the order
they were first set, as the system would when their periods pass, and
`HeadlessApp::timers` lists them.

## Alternatives not taken

- **A hidden window of the application for its timers.** `SetTimer` on
  a message-only window would deliver `WM_TIMER` to a window procedure,
  with ids of our own and no table. It costs a window class and a
  window for every application, and the procedure would be the windows'
  own, with their dispatch queue, which a timer does not need.
- **Timers as an event to a handler of the application.** Windows hear
  timers as events because they have a handler already; an application
  has none, and one handler for every timer would make each program
  write the dispatch on ids that a call per timer gives for free.
- **A handle that stops its timer when dropped.** It reads well, but it
  has to be kept somewhere, and a timer's call cannot hold its own
  handle. Ids match `Window::set_timer`, and a program that wants a
  handle can wrap one.
- **A thread for the program's periodic work.** What the work touches
  (a `Standalone`, the interface's `State`s) lives on the application's
  thread; a thread would need it to be `Send` or to post back, which is
  what a timer does.

## Consequences

- A program follows devices, saves state or polls a standalone from
  `App::set_timer`, on the thread it all lives on, also while its window
  is hidden. The example of `manguebyte_gui::audio_settings` shows it.
- Ticks come only while the thread takes messages: in a loop, never
  during a long call on it. The period is at least 10 ms on Win32
  (`USER_TIMER_MINIMUM`), and `WM_TIMER` comes after the thread's other
  messages, so ticks are late while the thread is busy, and late ones
  are not made up.
- Only Win32 and the headless backend have applications, so only they
  have application timers.
- Not breaking: new methods on `App` and `HeadlessApp`.
- Tested: on the headless backend, setting, replacing, killing and the
  order of firing, a call that kills a later timer and replaces itself,
  a tick dropped while its own call runs, a call whose drop reaches the
  timers, ticks between the steps of the loop that end it, the calls
  dropped with the application; on Win32, thread timers ticking in the
  application's loop and in a loop that is not, set again and killed,
  a panicking call caught with its timer killed, and no tick after the
  application goes. What is left: TECH_DEBT item 117.
