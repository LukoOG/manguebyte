//! Immutable snapshots handed to the audio thread: built outside, handed
//! inside, given back to be freed outside.
//!
//! The audio thread never reads a [`Document`]: a
//! document allocates on every edit and is guarded by whoever owns it.
//! Instead, the thread that edits builds whatever the audio thread needs
//! from the document (a zone table, a mapping, a copy of a subtree as a
//! [`NodeTree`](crate::NodeTree)), and publishes it through a
//! [`SnapshotWriter`]. The audio thread's [`SnapshotReader`] picks up the
//! newest one between blocks and hands the one it let go of back through a
//! second queue, so that it is freed on the writer's thread.
//!
//! On the audio thread nothing is allocated, freed, locked or waited for:
//! [`SnapshotReader::update`] moves a pointer in and a pointer out of two
//! wait-free queues ([`manguebyte_core::spsc`]). This is the "build
//! outside, hand inside" pattern of `manguebyte-web`'s engine hand-over.
//!
//! ```
//! use manguebyte_state::Document;
//! use manguebyte_state::snapshot;
//!
//! /// What the audio thread needs: the keys of the zones, nothing more.
//! #[derive(Default)]
//! struct Zones { keys: Vec<i64> }
//!
//! let mut doc = Document::new("instrument");
//! let (mut writer, mut reader) = snapshot::channel(Zones::default(), 4);
//!
//! // The editing thread.
//! doc.edit("Add zone", |tx| {
//!     let zone = tx.add_child(tx.root(), "zone")?;
//!     tx.set(zone, "key", 60)
//! })?;
//! let published = writer.publish_if_changed(&doc, |doc| Zones {
//!     keys: doc
//!         .children_of_kind(doc.root(), "zone")
//!         .filter_map(|zone| doc.get(zone, "key")?.as_int())
//!         .collect(),
//! });
//! assert!(published);
//!
//! // The audio thread, at the start of a block.
//! assert_eq!(reader.read().keys, [60]);
//! # Ok::<(), manguebyte_state::EditError>(())
//! ```

use std::fmt;

use manguebyte_core::spsc;

use crate::Document;

/// Makes a pair to publish snapshots of type `T` through, starting with
/// `initial`. Up to `capacity` snapshots can be on their way at once
/// (rounded up to a power of two).
///
/// Allocates; neither half does on the reading side.
///
/// # Panics
///
/// Panics if `capacity` is zero.
pub fn channel<T: Send>(initial: T, capacity: usize) -> (SnapshotWriter<T>, SnapshotReader<T>) {
    let (to_audio, from_writer) = spsc::channel(capacity);
    // One more than can be on the way, so that the reader always has room
    // to give back what it lets go of (`update` checks anyway).
    let (to_writer, from_audio) = spsc::channel(capacity + 1);
    (
        SnapshotWriter {
            to_audio,
            from_audio,
            published_version: None,
        },
        SnapshotReader {
            current: Box::new(initial),
            from_writer,
            to_writer,
        },
    )
}

/// The publishing half, on the thread that edits.
pub struct SnapshotWriter<T> {
    to_audio: spsc::Producer<Box<T>>,
    from_audio: spsc::Consumer<Box<T>>,
    published_version: Option<u64>,
}

impl<T> fmt::Debug for SnapshotWriter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotWriter")
            .field("published_version", &self.published_version)
            .finish_non_exhaustive()
    }
}

impl<T: Send> SnapshotWriter<T> {
    /// Publishes `snapshot` for the reader to pick up at its next
    /// [`update`](SnapshotReader::update). Frees, here, the snapshots the
    /// reader has let go of. Allocates a box for `snapshot`.
    ///
    /// # Errors
    ///
    /// Hands `snapshot` back if `capacity` snapshots are already on their
    /// way, which means the reader has not updated for a while.
    pub fn publish(&mut self, snapshot: T) -> Result<(), T> {
        self.publish_boxed(Box::new(snapshot))
            .map_err(|boxed| *boxed)
    }

    /// Publishes a snapshot already in a box, such as one
    /// [`take_retired`](Self::take_retired) gave back to be filled again.
    ///
    /// # Errors
    ///
    /// As for [`publish`](Self::publish).
    pub fn publish_boxed(&mut self, snapshot: Box<T>) -> Result<(), Box<T>> {
        self.collect();
        self.to_audio.push(snapshot)
    }

    /// Builds a snapshot of `doc` with `build` and publishes it, if the
    /// document has changed since the last snapshot published this way.
    /// Returns whether it published.
    ///
    /// When the reader is behind and the queue is full, nothing is
    /// published and the next call tries again.
    pub fn publish_if_changed(
        &mut self,
        doc: &Document,
        build: impl FnOnce(&Document) -> T,
    ) -> bool {
        if self.published_version == Some(doc.version()) {
            return false;
        }
        if self.to_audio.free_len() == 0 {
            self.collect();
            if self.to_audio.free_len() == 0 {
                return false;
            }
        }
        let published = self.publish(build(doc)).is_ok();
        if published {
            self.published_version = Some(doc.version());
        }
        published
    }

    /// The document version of the last snapshot
    /// [`publish_if_changed`](Self::publish_if_changed) published.
    pub fn published_version(&self) -> Option<u64> {
        self.published_version
    }

    /// Frees the snapshots the reader has let go of. Returns how many.
    pub fn collect(&mut self) -> usize {
        let mut freed = 0;
        while self.from_audio.pop().is_some() {
            freed += 1;
        }
        freed
    }

    /// Takes back one snapshot the reader has let go of, to be filled
    /// again and published with [`publish_boxed`](Self::publish_boxed)
    /// instead of allocating a new one.
    pub fn take_retired(&mut self) -> Option<Box<T>> {
        self.from_audio.pop()
    }
}

/// The reading half, on the audio thread.
///
/// Drop it off the audio thread: it owns the snapshot in use.
pub struct SnapshotReader<T> {
    current: Box<T>,
    from_writer: spsc::Consumer<Box<T>>,
    to_writer: spsc::Producer<Box<T>>,
}

impl<T> fmt::Debug for SnapshotReader<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotReader").finish_non_exhaustive()
    }
}

impl<T: Send> SnapshotReader<T> {
    /// Takes the newest published snapshot, if any is waiting, and gives
    /// the ones it replaces back to the writer. Returns whether the
    /// snapshot changed.
    ///
    /// Real-time safe: no allocation, no freeing, no lock, no wait.
    pub fn update(&mut self) -> bool {
        let mut changed = false;
        // Take a snapshot only when the one it replaces can be given back:
        // it must never be freed here.
        while self.to_writer.free_len() > 0 {
            let Some(next) = self.from_writer.pop() else {
                break;
            };
            let old = std::mem::replace(&mut self.current, next);
            if self.to_writer.push(old).is_err() {
                unreachable!("there was room");
            }
            changed = true;
        }
        changed
    }

    /// The snapshot in use, without looking for a newer one.
    pub fn get(&self) -> &T {
        &self.current
    }

    /// [`update`](Self::update), then the snapshot in use. Real-time safe.
    pub fn read(&mut self) -> &T {
        self.update();
        &self.current
    }
}
