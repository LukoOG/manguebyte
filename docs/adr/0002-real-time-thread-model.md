# ADR 0002: Two bridges to the audio thread, and tests that enforce them

**Status:** Accepted
**Date:** 2026-10-13
**Deciders:** Maintainers

## Context

The audio thread has a deadline of a few milliseconds on every callback.
Anything with unbounded duration (a heap allocation, a lock, a system call)
will eventually miss it, and a missed deadline is an audible glitch.

In most frameworks this rule is a convention that users are told to follow.
The framework itself often breaks it: JUCE's parameter attachments call
`AsyncUpdater::triggerAsyncUpdate` when a value changes off the message
thread, which posts to the operating system's message queue. That takes a
lock on every platform and can allocate. JUCE's own documentation advises
against calling it from a real-time thread, and developers have reported
dropouts from it, particularly in faster-than-real-time bounces on Windows.

References:

- <https://forum.juce.com/t/apvts-updates-thread-realtime-safety/36928>
- <https://forum.juce.com/t/bug-report-asyncupdater-for-real-time-use-in-parameterattachment/50645>

## Decision

1. Exactly two mechanisms carry information to or from the audio thread:
   atomic values and the wait-free single-producer single-consumer queue in
   `manguebyte-core`. The framework offers no other, and uses no other
   internally.
2. `Processor::process` and `Processor::reset` must not allocate, free, lock,
   block, do I/O or panic. `Processor::prepare` may.
3. The rule is enforced by tests: a counting global allocator fails the build
   if any processor touches the heap while processing, and the queue's
   `unsafe` code runs under Miri in CI.

## Options considered

### Option A: document the rule and trust it

| Dimension | Assessment |
|---|---|
| Complexity | None |
| Reliability | Regressions are found by users, as glitches |

### Option B: a general-purpose message bus between threads

| Dimension | Assessment |
|---|---|
| Complexity | High |
| Reliability | Depends on every message type being allocation-free |

**Cons:** hides which thread a handler runs on, which is the original problem.

### Option C: two primitive bridges plus enforcement tests (chosen)

| Dimension | Assessment |
|---|---|
| Complexity | Low: two small types |
| Reliability | An allocation in `process` fails CI |

**Cons:** users write the polling themselves; there is no "subscribe".

## Trade-off analysis

Option C asks for a few more lines in user code (poll a value each block,
drain a queue) and gives back the ability to see, at every point, which
thread is running. That visibility is the property the alternatives lose.

Compile-time enforcement, where code that could allocate on the audio thread
does not compile, is the long-term goal listed under "Beyond parity" in the
roadmap. Rust has no stable effect system to express it today, so run-time
enforcement in tests is the strongest available tool.

## Consequences

- Every processor added to the framework is also added to
  `crates/manguebyte-dsp/tests/realtime.rs`.
- Device callbacks hand out buffers allocated when the stream was built.
- Freeing memory is also forbidden, so large objects handed to the audio
  thread are sent back through a queue to be dropped elsewhere.
- Lock-free does not mean wait-free: a multi-producer queue would need a
  retry loop. The framework stays with single-producer single-consumer, and a
  second producer gets a second queue.

## Action items

1. [x] `spsc` queue with slice operations
2. [x] Allocation guard test for `manguebyte-dsp`
3. [x] Miri job in CI for `manguebyte-core`
4. [ ] Extend the allocation guard to device block adapters and parameters
5. [ ] Debug-build panic when a processor allocates in a user's own code
