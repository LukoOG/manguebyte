//! Application state: what an editor edits, undoes, saves and hands to the
//! audio thread.
//!
//! This is the part of JUCE's `juce_data_structures` (value trees, the undo
//! manager, properties files) that a plugin or an application with a real
//! editor needs, rebuilt around this framework's rule that nothing calls
//! back (ADR 0003) and that the audio thread is never made to wait or
//! allocate (ADR 0002). ADR 0008 explains the design.
//!
//! - [`Document`] is a tree of typed nodes with properties ([`Value`]:
//!   numbers, text, switches, binary data, and references to parameters and
//!   to other nodes) and ordered children. Every node has a [`NodeId`] that
//!   never changes and is never reused, so references survive any edit.
//! - Every change is made in a named transaction through [`Tx`] and can be
//!   undone and redone; gestures and series of edits merge into one step;
//!   [`HistoryLimits`] bound the history by steps and bytes.
//! - Nothing is notified. Each reader drains the document's change journal
//!   at its own pace with a [`Cursor`], or polls a [`VersionWatch`] from
//!   another thread.
//! - [`Document::link_params`] mirrors a plugin's parameters: changes made
//!   by the user are undoable steps, changes made by host automation are
//!   not. A [`SharedDocument`] in a parameter struct is saved with the
//!   plugin's state.
//! - [`Document::to_text`] writes a versioned, readable text form, and
//!   [`Document::to_bytes`] a compact binary one. Both load tolerantly, and
//!   [`Migrations`] bring older schemas of an application up to date.
//! - [`Settings`] is a key and value file in the user's configuration
//!   folder.
//! - [`snapshot`] hands immutable snapshots built from a document to the
//!   audio thread, which never allocates, frees or locks to read them.
//!
//! # Threads
//!
//! A document belongs to one thread at a time, usually the editor's, or sits
//! behind a mutex in a [`SharedDocument`] for the thread a host saves state
//! on. It allocates on every edit and must never be touched by the audio
//! thread. What the audio thread needs is built from it off that thread
//! and published as a snapshot.

#![forbid(unsafe_code)]

pub mod binary;
mod document;
mod history;
mod journal;
pub mod settings;
mod shared;
pub mod snapshot;
mod text;
mod tree;
mod value;

pub use document::{Document, EditError, Tx};
pub use history::HistoryLimits;
pub use journal::{Change, ChangeKind, Changes, Cursor, Origin, VersionWatch};
pub use settings::{ParseSettingsError, Settings};
pub use shared::SharedDocument;
pub use text::{FORMAT_VERSION, LoadError, LoadReport, Migrations, Skipped};
pub use tree::{MAX_DEPTH, NodeTree};
pub use value::{NodeId, ParamRef, Value, is_valid_name};

/// Compiles the code blocks of the README as tests, so they cannot go stale.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
