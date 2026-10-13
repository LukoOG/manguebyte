# ADR 0003: Parameters are atomics in a struct, with no listeners

**Status:** Accepted
**Date:** 2026-10-13
**Deciders:** Maintainers

## Context

Parameters are read by the audio thread, written by a user interface, written
by the host during automation, and serialised by the host from a thread of
its choosing. JUCE's `AudioProcessorValueTreeState` is the best-known attempt
at this in C++, and its history shows the hard parts:

- Parameter listeners are called synchronously on the thread that changes
  the value. During host automation that is the audio thread, so
  `parameterChanged` handlers written with a user interface in mind run
  there, behind a critical section.
- Between JUCE 5.0 and 5.2 the value tree that state was saved from was
  refreshed by a timer, up to about half a second behind the real values.
- `getStateInformation` and `setStateInformation` may arrive on any non-audio
  thread, concurrently with processing, and synchronising them is left to
  each plugin.
- The audio thread obtains a parameter through a string lookup that returns
  a pointer to an atomic.

References:

- <https://forum.juce.com/t/apvts-updates-thread-realtime-safety/36928>
- <https://forum.juce.com/t/audioprocessorvaluetreestate-thread-safety/21811>
- <https://forum.juce.com/t/are-getstateinformation-and-setstateinformation-always-called-on-the-audio-thread/53467>

## Decision

1. A parameter owns an atomic holding its real value. Every method takes
   `&self`. A set of parameters is a plain struct shared in an `Arc`.
2. There are no listeners and no change callbacks. The audio thread polls
   through `ParamSmoother::follow` or `Changed::update`; an interface polls
   on its own timer.
3. The audio thread reads typed fields (`params.cutoff.value()`). Lookup by
   string id exists only on the `ParamSet` trait, for hosts and interfaces.
4. Saved state is produced from the live values when asked for, never cached,
   and stores real values keyed by id rather than `0..=1` positions.
5. Loading is tolerant: unknown ids are skipped, missing ones keep their
   value, out-of-range values are clamped, and a report says what happened.
6. A value that is not a number is ignored on write.

## Options considered

### Option A: observer pattern with thread-hopping

Listeners registered per parameter, with the framework marshalling calls to
the right thread.

**Pros:** familiar; no polling.
**Cons:** marshalling from the audio thread needs a queue per listener or a
post to the system message queue; the thread a handler runs on is decided far
from where it is written. This is the design whose failure modes are listed
above.

### Option B: a central value tree

One dynamically typed tree holding all state, observed by everything.

**Pros:** undo, serialisation and binding come almost free.
**Cons:** not readable from the audio thread without copying or locking;
values exist twice (tree and atomic) and can disagree.

### Option C: atomics in a struct, polled (chosen)

**Pros:** impossible to race; nothing to lock; state cannot go stale because
there is only one copy; field access is checked by the compiler.
**Cons:** an interface must poll (cheap: one atomic load per parameter per
frame); a parameter list cannot change while running.

## Trade-off analysis

Polling costs a handful of atomic loads per block or per frame, which is
negligible next to any DSP. In exchange the whole class of "which thread is
this running on" defects disappears, and so does the state-drift class,
because there is no second copy of the values.

Storing real values in saved state means a later version can reshape a range
without changing how an old session sounds. Positions in `0..=1` are still
what hosts automate, which is a property of plugin formats and not of our
storage.

## Consequences

- Sample-accurate automation is not expressed through `Param` at all. It
  arrives as timestamped events in the process call, which the plugin API
  will define; `Param` holds the latest value for anyone who polls.
- Change gestures (begin and end of a user edit) belong to the same event
  path, with a guard type so that an "end" cannot be forgotten.
- Choice parameters save their index. Options may only be appended.
- A fixed parameter list rules out adding parameters at run time, which
  plugin formats handle poorly in any case.

## Action items

1. [x] `FloatParam`, `IntParam`, `BoolParam`, `ChoiceParam`, `ParamSet`
2. [x] `ParamState` text format with a version header
3. [ ] Parameter events and gesture guards in `manguebyte-plugin`
4. [ ] `EnumParam` generic over a Rust enum
5. [ ] A derive macro as an optional alternative to `param_set!`
