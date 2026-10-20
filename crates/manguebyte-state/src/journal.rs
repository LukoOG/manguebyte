//! The change journal: how anything learns what changed, without callbacks.

use std::collections::VecDeque;
use std::collections::vec_deque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NodeId;

/// One change to a [`Document`](crate::Document), as its journal records it.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The document's [`version`](crate::Document::version) once this
    /// change was made. Every change has its own, one more than the last.
    pub seq: u64,
    /// The node that changed: the one whose property changed, or the
    /// parent that gained or lost a child.
    pub node: NodeId,
    /// What changed.
    pub kind: ChangeKind,
    /// Why it changed.
    pub origin: Origin,
}

/// What a [`Change`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeKind {
    /// The property `name` was set or removed.
    Property {
        /// The property's name.
        name: Arc<str>,
    },
    /// `child` was inserted at `index` among the node's children, with
    /// everything under it.
    ChildAdded {
        /// The child, the root of what was inserted.
        child: NodeId,
        /// Its place among the node's children.
        index: usize,
    },
    /// `child`, which was at `index`, was removed with everything under it.
    ChildRemoved {
        /// The child that was removed.
        child: NodeId,
        /// The place it had among the node's children.
        index: usize,
    },
    /// The whole document was replaced, by
    /// [`Document::replace`](crate::Document::replace). Read it again from
    /// the root.
    Replaced,
}

/// Why a [`Change`] happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Origin {
    /// An edit, through [`Document::edit`](crate::Document::edit) and its
    /// siblings, including an edit that failed being taken back.
    Edit,
    /// [`Document::undo`](crate::Document::undo).
    Undo,
    /// [`Document::redo`](crate::Document::redo).
    Redo,
    /// The parameter mirror following the parameters, which a host
    /// automates: [`Document::sync_params`](crate::Document::sync_params).
    /// Never in the undo history.
    Params,
    /// A whole document taken in, by
    /// [`Document::replace`](crate::Document::replace).
    Load,
}

/// Where a reader of the journal has got to. Each reader keeps its own:
/// an editor panel, a snapshot publisher, a network sync.
///
/// [`Cursor::default`] is the very start of the document, and
/// [`Document::cursor`](crate::Document::cursor) is the present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Cursor {
    seen: u64,
}

impl Cursor {
    /// The last [`Change::seq`] this cursor has read.
    pub fn seen(&self) -> u64 {
        self.seen
    }
}

/// What [`Document::changes`](crate::Document::changes) found since a
/// [`Cursor`].
#[derive(Debug)]
pub enum Changes<'a> {
    /// Every change since the cursor, oldest first. Empty if nothing
    /// changed.
    Since(vec_deque::Iter<'a, Change>),
    /// The journal no longer holds every change since the cursor: it keeps
    /// only the newest [`journal_capacity`](crate::Document::journal_capacity).
    /// Read whatever you need from the document again.
    Missed,
}

/// The newest version of a document, readable from any thread without
/// locking the document.
///
/// A thread that shares a document behind a mutex polls this, and only
/// locks the document when the number has moved. Reading it is one atomic
/// load: real-time safe, though the audio thread has no use for it, since
/// it never sees the document itself.
#[derive(Debug, Clone)]
pub struct VersionWatch(pub(crate) Arc<AtomicU64>);

impl VersionWatch {
    /// The document's [`version`](crate::Document::version) at its latest
    /// change.
    pub fn version(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

/// The newest changes, in a ring of bounded size.
#[derive(Debug)]
pub(crate) struct Journal {
    entries: VecDeque<Change>,
    capacity: usize,
    seq: u64,
    watch: Arc<AtomicU64>,
}

impl Journal {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
            seq: 0,
            watch: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }

    pub(crate) fn watch(&self) -> VersionWatch {
        VersionWatch(Arc::clone(&self.watch))
    }

    pub(crate) fn record(&mut self, node: NodeId, kind: ChangeKind, origin: Origin) {
        self.seq += 1;
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(Change {
            seq: self.seq,
            node,
            kind,
            origin,
        });
        self.watch.store(self.seq, Ordering::Release);
    }

    pub(crate) fn cursor(&self) -> Cursor {
        Cursor { seen: self.seq }
    }

    pub(crate) fn since(&self, cursor: &mut Cursor) -> Changes<'_> {
        let seen = cursor.seen.min(self.seq);
        cursor.seen = self.seq;
        let first = self
            .entries
            .front()
            .map_or(self.seq + 1, |change| change.seq);
        if seen + 1 < first {
            return Changes::Missed;
        }
        // `seen + 1 >= first` and `seen <= seq`, so this is within the ring.
        let skip = usize::try_from(seen + 1 - first).unwrap_or(usize::MAX);
        Changes::Since(self.entries.range(skip.min(self.entries.len())..))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(journal: &mut Journal, name: &str) {
        journal.record(
            NodeId::ROOT,
            ChangeKind::Property {
                name: Arc::from(name),
            },
            Origin::Edit,
        );
    }

    fn seqs(changes: Changes<'_>) -> Option<Vec<u64>> {
        match changes {
            Changes::Since(iter) => Some(iter.map(|change| change.seq).collect()),
            Changes::Missed => None,
        }
    }

    #[test]
    fn readers_see_each_change_once_and_learn_when_they_fell_behind() {
        let mut journal = Journal::new(3);
        let watch = journal.watch();
        let mut early = Cursor::default();
        let mut late = Cursor::default();
        assert_eq!(seqs(journal.since(&mut early)), Some(vec![]));

        property(&mut journal, "a");
        property(&mut journal, "b");
        assert_eq!(watch.version(), 2);
        assert_eq!(seqs(journal.since(&mut early)), Some(vec![1, 2]));
        assert_eq!(seqs(journal.since(&mut early)), Some(vec![]));

        property(&mut journal, "c");
        property(&mut journal, "d");
        assert_eq!(seqs(journal.since(&mut early)), Some(vec![3, 4]));
        // `late` has read nothing, and change 1 has left the ring.
        assert_eq!(seqs(journal.since(&mut late)), None);
        assert_eq!(late.seen(), 4);
        assert_eq!(seqs(journal.since(&mut late)), Some(vec![]));

        let mut present = journal.cursor();
        property(&mut journal, "e");
        assert_eq!(seqs(journal.since(&mut present)), Some(vec![5]));

        journal.set_capacity(1);
        let mut one_behind = Cursor { seen: 3 };
        assert_eq!(seqs(journal.since(&mut one_behind)), None);
    }
}
