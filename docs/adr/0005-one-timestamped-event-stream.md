# ADR 0005: One timestamped event stream into the process call

**Status:** Accepted
**Date:** 2026-10-13
**Deciders:** Maintainers

## Context

A processor is told about more than audio: notes start and stop, parameters
are automated, a host modulates values, a player bends one note of a chord.
All of these happen at a particular moment, and that moment is rarely the
start of a block.

JUCE's `AudioProcessor` receives a MIDI buffer with sample positions, but
parameter changes arrive separately as block-rate values, so automation is
quantised to the block. There is no notion of an event addressed to one note
other than through MPE channels, and no modulation that is distinct from
changing the value. The JUCE team has said these are the subject of an
"AudioProcessor v2", and the third-party CLAP wrapper has to add its own
extension interfaces to reach them.

CLAP's event model shows a better shape: a single ordered list in which
notes, note expressions, parameter values and parameter modulation all carry
a sample offset.

References:

- <https://forum.juce.com/t/sample-accurate-parameter-automation/68800>
- <https://github.com/free-audio/clap-juce-extensions>
- CLAP, `events.h`: <https://github.com/free-audio/clap>

## Decision

1. **One stream.** `EventBuffer` holds every kind of timed input, sorted by
   frame: note on, note off, note choke, note expression, parameter value,
   parameter modulation and raw MIDI. A note and the parameter change that
   belongs with it cannot arrive out of order, because there is only one
   order.
2. **A context beside the buffer.** `Processor::process_with` takes the audio
   buffer and a `ProcessContext` holding the events, the `Transport` and an
   optional sidechain. The plain `process(&mut AudioBuffer)` stays as the
   simple form, and is the default implementation's target, so an effect that
   only transforms audio is written exactly as before.
3. **Notes have identity.** A `NoteRef` is a channel, a key and an optional
   id, so two notes on the same key can overlap and end separately. Velocity
   is a float.
4. **Modulation is not automation.** `ParamModulation` is an offset on top
   of a parameter's value, optionally for a single note. The value itself is
   untouched, so modulation never fights automation and never ends up in a
   saved preset.
5. **Values in real units.** Parameter events carry the value in the
   parameter's own unit, the same convention as saved state (ADR 0003).
   Format wrappers convert to and from `0..=1` positions at the edge.
6. **Splitting without copying.** `AudioBuffer::windowed` narrows a buffer to
   a range of frames in place, and `split_at_events` uses it to render up to
   each event and then apply it. Sample accuracy costs no allocation and no
   copy.
7. **Parameters are named by a stable hash.** `ParamId` is a 32-bit FNV-1a
   hash of the text identifier, computable in a constant, so events can be
   matched without string comparison on the audio thread.

## Options considered

### Option A: MIDI buffer plus block-rate parameters

The JUCE model. Simple, and enough for a great deal of software.

**Cons:** automation steps at block boundaries, so the sound depends on the
host's block size; per-note control needs MPE; no place for modulation.

### Option B: separate queues per kind of event

A note queue, a parameter queue, a modulation queue.

**Cons:** the relative order of events on the same frame is lost, and every
processor has to merge the queues itself.

### Option C: one ordered stream (chosen)

**Pros:** order is unambiguous; one loop handles everything; maps directly
onto CLAP and can be produced from VST3, AU and MIDI by a wrapper.
**Cons:** the event type is a larger enum; consumers match on kinds they may
not care about.

## Trade-off analysis

The cost of Option C is a wider enum and a `match`. What it buys is that
timing stops being a property of the host's block size. That is testable, and
it is tested: `crates/manguebyte/tests/offline.rs` renders a song with six
different block patterns and requires the results to be identical bit for
bit. The first run of that test failed, exposing that a voice which finished
mid-block kept running to the end of the block, which is exactly the class of
defect the model is meant to make visible.

Keeping the audio in place, in one buffer with the output's channel count,
means a processor with fewer inputs than outputs finds its input in the first
channels. That is less explicit than separate input and output buffers, but
it keeps processor chains free of copies and matches what every plugin format
can provide. The sidechain, which really is a different signal, is separate.

## Consequences

- A voice must return to its initial state when choked, because the synth
  renders it to the end of the piece of the block it finished in. This is
  part of the `Voice` contract and is documented there.
- `ProcessContext` is non-exhaustive. Output events, for processors that
  generate notes, will be added as a field.
- Events past the end of a block are handled after its last frame rather
  than dropped.
- A full `EventBuffer` hands the event back to the caller, which decides
  what to do; nothing is lost silently.

## Action items

1. [x] `Event`, `EventBuffer`, `NoteRef`, `Expression`
2. [x] `Transport` and `ProcessContext`
3. [x] `AudioBuffer::windowed` and `split_at_events`
4. [x] `Synth` and `Voice`
5. [x] Block-size independence test
6. [ ] Output events
7. [x] MIDI 2.0 and MPE read into `Event`s: `dsp::note_input::NoteInput`
8. [ ] `Event`s written back as MIDI 2.0 packets
