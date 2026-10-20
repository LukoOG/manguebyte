# ADR 0008: Application state is a document of reversible edits, read through journals and snapshots

**Status:** Accepted
**Date:** 2026-10-20
**Deciders:** Maintainers

## Context

An editor with more than knobs needs state that is not a parameter: the
zones of a sampler, a modulation matrix, the layout of a drum kit. It needs
Ctrl+Z from the first day, and that state has to travel with the plugin's
saved state. Parabolic, a no-code sampler and instrument builder planned on
this framework, is all of these at once.

JUCE answers with `juce_data_structures`, which this framework lacked
(`docs/JUCE_PARITY.md`). Its public documentation describes:

- `ValueTree`, a reference-counted tree of typed properties and children,
  with `ValueTree::Listener` callbacks made synchronously, on the thread
  that changed the tree, for every property and child change;
- `UndoManager`, which keeps `UndoableAction`s grouped into transactions
  started by `beginNewTransaction`. Edits to a `ValueTree` are undoable only
  when an `UndoManager` is passed to each call; actions coalesce through
  `UndoableAction::createCoalescedAction`; the history is limited by a count
  of "units" and a minimum number of transactions;
- `PropertiesFile`, a settings file in XML or binary in a folder chosen
  through `PropertiesFile::Options`.

Three things in that design run against this framework's rules:

- Listeners are callbacks on whichever thread made the change. ADR 0003
  rejected exactly this for parameters, and ADR 0002 forbids anything that
  can run user code on the audio thread.
- Undo is opt-in per call. An edit made without passing the undo manager is
  silently not undoable, and nothing tells the two apart.
- A `ValueTree` cannot be read from the audio thread without a lock or a
  copy; JUCE leaves that to each plugin.

ADR 0003 also rejected "a central value tree" holding parameter values,
because the values would exist twice and could disagree. Whatever is built
here must mirror parameters without becoming a second source of truth.

## Decision

1. **A crate of its own, `manguebyte-state`,** depending on `core` and
   `params` only. It is on the audio side of the layering of ADR 0001 and
   ADR 0007 (`scripts/check-layering.sh` lists it): a plugin can keep a
   document without any interface crate, and an interface crate can use it.
   It has no `unsafe` code.
2. **A tree with stable identity.** A `Document` holds nodes in an arena by
   `NodeId`, a `u64` handed out in increasing order and never reused, not
   even after a removal can no longer be undone; the next id is saved with
   the document. A node has a kind, properties by name and ordered children.
   A property holds a `Value`: a switch, a whole number, a finite float,
   text, bytes, a reference to a parameter by id, or a reference to another
   node. Properties are kept sorted by name, so the same tree is always the
   same text and undo restores order exactly. Text and bytes are shared
   (`Arc`), so history and snapshots never copy a sample.
3. **Every change is a transaction, and there is no other way to change
   it.** `Document::edit("Add zone", |tx| …)` runs a closure with a `Tx`;
   whatever the closure does is one undoable step. If it returns an error,
   what it did is taken back before `edit` returns. Each primitive edit is
   recorded as a reversible operation: a property set or removed, a subtree
   inserted or removed, a node moved, a parameter changed. A removed subtree
   is moved out of the arena into the operation and back again on undo, so
   undo and redo copy nothing.
4. **Gestures and series merge.** `begin_gesture` and `end_gesture` make
   every edit in between one step, named after the gesture; an edit that
   merges into a step whose operation on the same property (or the same
   parameter) already exists replaces only the new value, keeping the first
   old one. A drag of a thousand frames costs one operation per property it
   touched. Edits with no clear end, such as nudging with the arrow keys,
   use `edit_merging(name, key, …)`, which merges while the key is the same
   and nothing else has happened. A step that merges into nothing (a knob
   dragged back where it started) is dropped. Merging is not based on time:
   timing would make the history depend on the speed of the machine, and
   tests could not pin it down.
5. **One line of history.** A new step discards every step that could be
   redone. An edit that changes nothing makes no step and keeps them.
6. **Bounded history.** `HistoryLimits` caps the steps (undo and redo
   together) and an estimate of the bytes they keep alive; the oldest steps
   go first. The newest step is always kept, so the last edit can always be
   undone, and a cap of zero steps turns history off.
7. **No callbacks: versions and journals.** Every change, an undo or a redo
   included, bumps the document's version and goes into a bounded journal
   of `Change`s: which node, what (`Property`, `ChildAdded`, `ChildRemoved`,
   `Replaced`) and why (`Origin::Edit`, `Undo`, `Redo`, `Params`, `Load`).
   Each reader keeps its own `Cursor` and drains `changes(&mut cursor)`
   when it wants; a reader that fell further behind than the journal keeps
   is told `Changes::Missed` and reads what it needs again. A
   `VersionWatch` is the version in an atomic, so another thread polls it
   without locking the document and locks only when it moved. This is the
   polling model of ADR 0003 applied to a tree: nothing runs on the
   changing thread but the change, re-entrant edits from inside a
   notification cannot happen, and the order of changes is the journal's.
8. **Parameters are mirrored, not owned.** `link_params(&params, node)`
   makes one node the mirror of a parameter set: a float property per
   parameter, named by its id. The atomics stay the only truth; the audio
   thread never reads the mirror. A user's change goes through
   `Tx::set_param` (or `Tx::set` on a mirror property, which forwards to
   it): the atomic is written and the change is an undoable step. A host's
   automation reaches the mirror only through `sync_params`, called on the
   interface's timer, which records the change in the journal with
   `Origin::Params` and **never in the history**: automation is the host's
   to record, and Ctrl+Z must not take back what a lane did. The link is a
   `Weak`, so a parameter struct can hold its document without a cycle.
9. **Saved with the plugin's state.** `manguebyte-params` gains
   `ExtraState`, state a `ParamSet` keeps beside its values, listed after
   `extra` in `param_set!`. `manguebyte_plugin::save_state` writes it after
   the values as named `section`s (format version 2, written only when there
   is extra state, so every other plugin's state is unchanged).
   `SharedDocument`, a document behind a mutex, is such state. Saving it
   syncs the mirror first, because the editor that would have synced it may
   be closed while the host automates. Restoring replaces the document
   (`Document::replace_keeping_params`): the history is cleared, since a
   host's restore is the host's to undo, the parameter link is kept, and
   the parameter values restored from the same state win over the mirror,
   as the atomics are the truth. Opening a file in an application is the
   other way round (`Document::replace`): there the document's values are
   written to the parameters.
10. **Two forms on disk, both tolerant.** A text form in the style of
    `ParamState` (a `manguebyte-state 1` header, one line per node and per
    property, every property typed) and a compact binary form whose payloads
    carry their length. A reader skips what it cannot read, a type or a
    block it does not know, and reports it in a `LoadReport`. The format
    version only rises for additions that an older reader can skip;
    anything else would get a new header. An application's own changes to
    its document are versioned by a schema number saved with it, and
    `Migrations` bring older schemas up to date on load.
11. **A settings file.** `Settings` keeps keys and text values in the
    user's configuration folder (`%APPDATA%`, `~/Library/Application
    Support`, `$XDG_CONFIG_HOME` or `~/.config`), written beside and renamed
    over the old file.
12. **Snapshots for the audio thread.** `snapshot::channel` is the "build
    outside, hand inside" pattern of `manguebyte-web`'s engine hand-over on
    two wait-free queues: the editing thread builds whatever the audio
    thread needs from the document and publishes it in a box; the audio
    thread's `update` takes the newest and pushes the one it replaces back
    to be freed by the writer. It takes a snapshot only when it has room to
    give the old one back, so it never frees, allocates, locks or waits.

## Options considered

### Option A: JUCE's shape, a tree with listeners and an optional undo manager

**Pros:** familiar; a listener is a short way to keep a view in sync.
**Cons:** callbacks on the changing thread, the failure ADR 0003 lists;
re-entrant edits from inside a listener; undo that silently does not happen
when the manager is not passed; no answer for the audio thread.

### Option B: a command pattern with application-defined actions

Each edit is an object with `perform` and `undo`, written by the
application, as with `UndoableAction`.

**Pros:** any change can be undoable, including ones outside a tree.
**Cons:** every application writes and tests its own inverses, and one wrong
inverse corrupts the history from then on. Persistence, journals and
snapshots each need their own code per action. Here the document's few
primitive operations are written and tested once, and everything built on
them is undoable without more code.

### Option C: a persistent tree, with old roots as the history

Each edit makes a new version of the tree sharing what did not change; undo
is going back to an older root, and a snapshot for the audio thread is a
reference-counted root.

**Pros:** snapshots and undo are almost free; no inverse operations at all.
**Cons:** needs a persistent map, which is either a new dependency or a
substantial structure written here; every edit allocates a path of nodes;
the audio thread dropping the last reference to an old root would free on
the audio thread unless roots are returned through a queue anyway; merging
a gesture means rebuilding versions. Worth revisiting if snapshots of large
documents become the bottleneck.

### Option D: a mutable arena, reversible operations and a journal (chosen)

**Pros:** edits are cheap and in place; undo and redo move nodes rather than
copy them; one closure is one step, so nothing is accidentally outside the
history; nothing calls back; the audio thread reads plain values built for
it.
**Cons:** a snapshot is rebuilt by the application for each change it
publishes; readers must poll and handle `Missed`; the history's byte count
is an estimate.

## Trade-off analysis

The cost of not having listeners is that a view polls: on an interface
timer it drains its cursor, which for an unchanged document is one
comparison. In exchange, no code ever runs because something else changed,
so an edit cannot trigger another edit halfway through, and the thread that
reacts to a change is always the one that asked.

Mirroring parameters looks like the second copy ADR 0003 refused. It is not
one in the sense that mattered there: nothing reads the mirror to decide a
value. The audio thread reads the atomics, saved state is taken from the
atomics (`ParamSet::save`), and the mirror is rewritten from them by
`sync_params`. What the mirror buys is that an editor sees the parameters
as part of the same document, with the same journal, and that a user's
parameter change is a step of the same history as adding a zone.

Keeping automation out of the history is a choice. The alternative, an undo
step per automated value, would fill the history in seconds and make Ctrl+Z
fight the host. The consequence is that undoing a user's change puts back
the value from before that change even when automation moved the parameter
since.

Writing plugin state version 2 only when there is extra state keeps every
existing plugin's state byte for byte as it was. A build of the framework
from before this change refuses version 2 with `UnsupportedVersion(2)`
rather than misreading it.

## Consequences

- An editor changes the document only through `edit`, `edit_merging` and
  gestures, and learns about changes only through cursors or the version.
- A parameter gesture is two gestures: `Document::begin_gesture` for the
  history and `manguebyte_plugin::editor::Gesture` for the host. The
  document does not talk to the host.
- The audio thread is given snapshots, never the document. What goes into
  a snapshot is the application's choice; a `NodeTree` copy of a subtree is
  the simplest and reads without allocating.
- Limits are TECH_DEBT item 87.

## Action items

1. [x] `Document`, `Tx`, `NodeId`, `Value`, `NodeTree`
2. [x] Transactions, gestures, merging by key, limits
3. [x] Journal, cursors, `VersionWatch`
4. [x] Parameter mirror, `ExtraState`, plugin state version 2,
   `SharedDocument`
5. [x] Text and binary forms, `LoadReport`, `Migrations`
6. [x] `Settings`
7. [x] `snapshot` hand-over, tested under the counting allocator
8. [ ] An example plugin whose editor edits a document
9. [ ] `manguebyte` facade feature re-exporting the crate
