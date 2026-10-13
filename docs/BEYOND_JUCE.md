# Where Manguebyte goes beyond JUCE

JUCE is the reference for what a complete audio framework contains, and
Manguebyte's first goal is to match it. This page is about the second goal:
the places where Manguebyte already does something JUCE does not, and how
each claim can be checked.

Three rules keep this page honest:

1. **Every claim points at a test.** If it cannot be verified by running
   something in this repository, it is not listed.
2. **Every statement about JUCE has a source**, and describes JUCE's `master`
   branch at version 9.0.4 (October 2026). JUCE keeps moving; the JUCE team
   has announced an "AudioProcessor v2" that targets several of these points.
3. **Limits are stated next to the claim**, not left for the reader to find.

JUCE is far ahead in breadth, and [the last section](#where-juce-is-ahead)
says where. Nothing here contains JUCE code: see
[ADR 0004](adr/0004-juce-as-reference.md).

## Summary

| | JUCE 9.0.4 | Manguebyte | Checked by |
|---|---|---|---|
| [Automation timing](#1-automation-on-the-exact-sample) | Per block in `AudioProcessor` | On the exact sample | [`harness.rs`](../crates/manguebyte-plugin/src/harness.rs), [`abi.rs`](../crates/manguebyte-clap/tests/abi.rs) |
| [Block size and sound](#2-the-sound-does-not-depend-on-the-hosts-block-size) | Left to each plugin | Bit-identical across block sizes, tested | [`offline.rs`](../crates/manguebyte/tests/offline.rs) |
| [Changes from a UI or a restored state](#3-every-parameter-change-reaches-the-plugin-as-an-event) | Seen by polling, or by listeners that may run on the audio thread | Delivered as events in the same stream as automation | [`harness.rs`](../crates/manguebyte-plugin/src/harness.rs) |
| [Real-time safety](#4-real-time-safety-is-tested-not-promised) | A convention | A heap call while processing fails CI | [`realtime.rs`](../crates/manguebyte-dsp/tests/realtime.rs), [`abi.rs`](../crates/manguebyte-clap/tests/abi.rs) |
| [Per-note control](#5-per-note-expression-and-modulation) | Through MPE channels | Per-note expression and modulation events | [`synth.rs`](../crates/manguebyte-dsp/src/synth.rs) |
| [CLAP](#6-native-clap) | Not available natively | Native, passes `clap-validator` | [CI](../.github/workflows/ci.yml) |
| [Testing a plugin](#7-a-plugin-can-be-tested-without-a-host) | Needs a host or a hand-built rig | In a unit test, with `Harness` | [`plugins/pluck`](../plugins/pluck/src/lib.rs) |
| [Thread safety of parameters and state](#8-parameters-and-state-cannot-race) | Synchronised by each plugin | Atomics; nothing to lock | [`set.rs`](../crates/manguebyte-params/src/set.rs) |
| [Licence](#9-licence) | AGPLv3 or commercial | MIT or Apache-2.0 | [`LICENSE-MIT`](../LICENSE-MIT) |
| [Slow-downs in silence](#10-a-fading-sound-cannot-slow-the-audio-thread-down) | A guard each plugin places in its own callback | Set by the framework around every plugin and every device callback | [`denormal.rs`](../crates/manguebyte-core/src/denormal.rs), both `abi.rs` |

## 1. Automation on the exact sample

**The problem.** A host records automation with sample positions. If a
plugin only learns the value once per block, a change that should happen on
frame 300 of a 512-frame block is applied on frame 0 or frame 512 instead.
Fast automation turns into steps, and the size of the steps depends on the
host's buffer setting.

**In JUCE.** `AudioProcessor::processBlock` receives audio and a MIDI buffer.
Parameter changes are not part of the call; a processor reads each
parameter's current value, which the host updates between blocks. In May 2026
a member of the JUCE team said sample-accurate automation would not be ready
for the first release of JUCE 9
([forum thread](https://forum.juce.com/t/sample-accurate-parameter-automation/68800)).

**In Manguebyte.** The process call carries an `EventBuffer` in which every
parameter change has the frame it falls on. `split_at_events` renders up to
each event, applies it, and continues, using `AudioBuffer::windowed` to
narrow the buffer in place. Nothing is copied or allocated.

**Check it.** `automation_lands_on_its_exact_frame_and_updates_the_parameter`
schedules changes on frames 100, 101 and 250 and asserts the output sample by
sample. `automation_lands_on_its_frame_when_processing_in_place` does the
same through the CLAP C interface, and a test of the same name in
[`manguebyte-vst3`](../crates/manguebyte-vst3/tests/abi.rs) does it through
VST 3, where a host sends a queue of points per parameter and every point
is delivered. (JUCE's VST3 wrapper applies the last point of each queue:
`processParameterChanges` in `juce_audio_plugin_client_VST3.cpp`.)

VST 3 adds a trap of its own. A host reports each change twice: to the
processor with a frame number, and to the controller on the main thread so
that an editor can follow. If the controller's copy is stored into the
parameter, the plugin jumps to a value that is slightly early or late until
the next point corrects it. While blocks are running, Manguebyte's wrapper
keeps the controller's copy for display only;
`what_the_controller_is_told_does_not_disturb_running_automation` checks it.

**Limit.** The framework delivers the timing; a processor chooses whether to
use it. Reading a parameter once per block remains valid and is cheaper. The
example plugin follows events for its voices and reads its reverb mix and
output level once per block, with smoothing.

## 2. The sound does not depend on the host's block size

**The problem.** Hosts hand plugins blocks of different lengths, and some
change the length on every call. If anything in a plugin is tied to block
boundaries, the same project sounds slightly different at 64 and at 1024
samples, and an offline bounce differs from what was heard while mixing.

**In JUCE.** Nothing in the framework prevents or tests this. It is up to
each plugin author to split blocks at events by hand and to notice when
something depends on the block length.

**In Manguebyte.** Because events carry their frame and the synth resets a
voice before every fresh note, the output is a function of the timeline and
not of how it was cut up. This is tested rather than assumed.

**Check it.** `the_song_is_the_same_whatever_the_block_size` renders a
polyphonic song with effects and automation six ways: 64, 256, 997 and 1024
frames, an irregular pattern of `1, 511, 7, 300, 64, 2, 129`, and blocks
larger than the instrument's own maximum. The results must be equal bit for
bit. The example plugin has the same test.

The first time this test ran, it failed. A voice that had finished in the
middle of a block kept advancing until the end of it, so the next note
started from a state that depended on the block length. The test exists to
find exactly that, and the fix is part of the `Voice` contract.

**Limit.** The guarantee covers what follows events. A value that a processor
reads once per block changes at block boundaries by definition.

## 3. Every parameter change reaches the plugin as an event

**The problem.** Host automation is not the only thing that changes a
parameter. A user turns a knob in the plugin's own window, or the host
restores a saved state. Those writes do not arrive through the host's event
list, so a plugin that tracks a value from events alone never hears about
them.

**In JUCE.** A processor finds out by polling the parameter, or through a
listener. Parameter listeners are called synchronously on whichever thread
made the change, which during host automation is the audio thread, and the
framework's attachment classes respond by posting to the operating system's
message queue, which takes a lock
([forum thread](https://forum.juce.com/t/apvts-updates-thread-realtime-safety/36928),
[bug report](https://forum.juce.com/t/bug-report-asyncupdater-for-real-time-use-in-parameterattachment/50645)).

**In Manguebyte.** There are no listeners. Before each block, `ParamWatcher`
compares every parameter with the value the plugin last saw and adds an event
on frame 0 for each one that differs, ahead of the host's own events. The
event stream is therefore complete: a plugin that follows it never needs to
poll, and no user code runs as a side effect of a write.

**Check it.** `changes_made_outside_the_host_reach_the_plugin_as_events` uses
a test plugin that never reads its parameter, only events. It passes for a
direct write, for a restored state, and for host automation on the same
frame, which takes precedence.

**Limit.** The plugin receives the *latest* value of each changed parameter
at the start of the next block. If a knob is moved through ten values between
two blocks, the plugin sees the tenth, not all ten. That is the intended
behaviour for a control, and it is why a plugin that wants no steps smooths
the value.

## 4. Real-time safety is tested, not promised

**The problem.** Code on the audio thread must not allocate or free memory:
the allocator can take a lock or ask the operating system for pages, and
either can outlast the few milliseconds a block has. One stray allocation is
an audible dropout that appears only under load.

**In JUCE.** Real-time safety is a rule in the documentation. The framework
does not check user code against it, and its own parameter attachments break
it, as described above.

**In Manguebyte.** The test suite replaces the global allocator with one
that counts. Any allocation, reallocation or free made while a processor is
processing fails the test, and so fails CI. The same guard wraps the CLAP
wrapper's `process` call, measured from the host's side of the C interface.

**Check it.** `processors_do_not_use_the_heap` and its neighbours cover every
processor, the FFT, a synth handling more notes than it has voices, and
parameter changes made while running. In `abi.rs`, `heap_calls` must be zero
for automation, notes, changes reported to the host and the block after a
state restore.

The guard has already earned its place. On macOS it reported one heap call
inside `process`: a test plugin was recording a value through a standard
mutex, which on that platform allocates the first time it is locked.

**Limit.** This is a test that runs, not a property the compiler proves. It
catches heap use on the paths the tests exercise, and it does not detect a
lock that never allocates. Checking this at compile time is a goal listed in
the roadmap; Rust has no stable way to express it yet. The framework's two
areas of `unsafe` code are additionally run under Miri, which found and led
to the fix of an invalid pointer in the CLAP wrapper.

## 5. Per-note expression and modulation

**The problem.** Bending, swelling or filtering one note of a chord needs a
way to address that note. So does a host's modulation system that wants to
move a parameter for one voice without overwriting the value the user set.

**In JUCE.** Per-note control is available through MPE, which gives each note
its own MIDI channel. The processor model has no note expression events and
no modulation distinct from changing a parameter's value, which is why the
third-party CLAP wrapper adds its own extension interfaces for them
([clap-juce-extensions](https://github.com/free-audio/clap-juce-extensions)).

**In Manguebyte.** Notes have an identity (`NoteRef`: channel, key and an
optional id), so two notes on the same key can overlap. `NoteExpression`
shapes one note. `ParamModulation` adds an offset on top of a parameter,
optionally for a single note, and never changes the stored value, so it
cannot fight automation or end up in a saved preset. `Synth` routes each to
the voice that is playing the note.

**Check it.** `expression_and_modulation_reach_one_note_and_parameters_reach_all`
and `notes_with_identifiers_overlap_on_one_key_and_end_separately` in
`synth.rs`; `an_instrument_takes_notes_expression_modulation_and_a_sidechain`
in `abi.rs` does it through CLAP.

**Limit.** MIDI 2.0 packets and MPE zone handling are not converted to these
events yet, and per-note *automation* from a CLAP host is not accepted yet.

## 6. Native CLAP

**In JUCE.** Building CLAP plugins is not supported natively. The plugin
client module covers VST, VST3, AU, AUv3, LV2 and AAX. CLAP was listed for
JUCE 9 in the 2024 roadmap and is not in the 9.0.x releases; the route today
is the unofficial `clap-juce-extensions`.

**In Manguebyte.** `export_clap!(MyPlugin)` is the whole integration. CLAP's
event model is the one this framework is built on, so nothing is approximated
on the way in.

**Check it.** CI builds the example plugin and runs the CLAP project's own
[`clap-validator`](https://github.com/free-audio/clap-validator) on it: 34 of
34 applicable tests pass. Ten are skipped because they concern features not
implemented yet or another platform.

**Limit.** No plugin GUI extension yet, no 64-bit audio, no presets. VST 3 is
the only other format so far, also without an editor: JUCE's AU, AAX and LV2
support has no counterpart here yet.

## 7. A plugin can be tested without a host

**In JUCE.** Exercising a plugin's audio path needs a host, or a test rig the
author builds around `AudioProcessor`.

**In Manguebyte.** `Harness` is that rig. It prepares the plugin, schedules
notes and automation on a timeline, cuts time into any pattern of block
sizes, keeps a transport moving and saves and restores state. The plugin's
sound becomes something an ordinary unit test can assert, on a CI machine
with no sound card.

**Check it.** The example plugin's tests check that it is silent until
played, that it plays the pitch it is asked for (by FFT), that automation and
per-note modulation change the sound, that state round-trips, and that the
output is independent of block size.

## 8. Parameters and state cannot race

**In JUCE.** `getStateInformation` and `setStateInformation` can be called
from any non-audio thread while `processBlock` is running, and the framework
does not serialise them; each plugin has to
([forum thread](https://forum.juce.com/t/are-getstateinformation-and-setstateinformation-always-called-on-the-audio-thread/53467)).
Between JUCE 5.0 and 5.2 the tree that state was saved from could lag behind
the real parameter values
([forum thread](https://forum.juce.com/t/audioprocessorvaluetreestate-thread-safety/21811)).

**In Manguebyte.** A parameter is an atomic, and every method takes `&self`.
Saving reads the atomics; loading writes them. There is one copy of each
value, so state cannot go stale, and the compiler rejects code that would
share anything else between threads without synchronisation. Saved state
holds real values keyed by identifier, so a range can be reshaped in a later
version without changing how an old session sounds.

**Check it.** `state_from_another_version_loads_as_far_as_it_can` and
`params_are_shareable_across_threads` in the parameters crate.

**Limit.** This covers parameters. Larger state, such as a loaded sample, has
a documented pattern (build off-thread, hand over through a queue) but no
ready-made type yet.

## 9. Licence

JUCE is offered under the AGPLv3 or a commercial licence. Manguebyte is under
MIT or Apache-2.0, at your option: no copyleft, no per-seat fee and no splash
screen, for open and closed products alike.

## 10. A fading sound cannot slow the audio thread down

**The problem.** A fading sound does not reach zero. Filter memory and reverb
tails shrink until their values are *subnormal*: still not zero, and tens of
times slower for a processor to compute with. The plugin then costs the most
in the silence after a note. Processors have a per-thread setting that rounds
such numbers to zero, and using it is standard practice.

**In JUCE.** The setting is available as
[`ScopedNoDenormals`](https://docs.juce.com/master/classScopedNoDenormals.html),
a guard that the author of each plugin places at the top of `processBlock`.
The plugin template includes it. Code that did not start from the template,
or a callback written later, is unprotected until someone notices.

**In Manguebyte.** The framework holds the guard itself: the CLAP and VST 3
wrappers and the standalone engine around `Plugin::process`, and the device
streams around every callback. There is nothing for a plugin author to
remember. The setting is put back on the way out, so the host's own code
runs as it expects.

**Check it.** `numbers_too_small_to_hear_are_zero_while_the_plugin_runs_and_only_then`,
in the tests of both wrappers, has a plugin report what the processor does
with a subnormal number from inside `process`, then checks that the host's
thread is back to its own setting afterwards, including after the plugin
panics. Measured on the example effect, 4,000 blocks of subnormal input took
442 ms without the guard and 31 ms with it.

**Limit.** `x86_64` and `aarch64` only; elsewhere the guard does nothing.
Rust does not define floating-point behaviour under a changed setting, and
[ADR 0006](adr/0006-subnormal-numbers-are-flushed.md) says what that means
in practice and why the trade is taken. A program that calls a processor in
its own loop, outside the wrappers, streams and test harness, creates the
guard itself.

## Where JUCE is ahead

JUCE has been in production for more than twenty years, and it shows. Today
it has, and Manguebyte does not:

- a complete GUI toolkit, with plugin editor windows in every format;
- Audio Unit, AAX and LV2 plugin formats;
- plugin hosting;
- many more DSP building blocks, and readers for compressed audio formats;
- iOS and Android;
- documentation, tutorials and a large community.

[JUCE_PARITY.md](JUCE_PARITY.md) tracks that gap module by module, and
[ROADMAP.md](../ROADMAP.md) says in what order it is being closed.
