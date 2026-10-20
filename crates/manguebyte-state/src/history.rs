//! Reversible operations, transactions and the undo and redo stacks.

use std::collections::{HashMap, VecDeque};
use std::mem;
use std::sync::Arc;

use crate::tree::Stash;
use crate::{NodeId, Value};

/// One reversible step. Applied forward it makes the change; backward it
/// takes it back.
#[derive(Debug)]
pub(crate) enum Op {
    /// A property set (`new` is `Some`) or removed (`new` is `None`).
    Set {
        node: NodeId,
        name: Arc<str>,
        old: Option<Value>,
        new: Option<Value>,
    },
    /// A subtree inserted (`insert`) or removed. Whichever way round, the
    /// nodes live in the document or in `stash`, never both, so applying
    /// the step moves them and copies nothing.
    Structure {
        parent: NodeId,
        index: usize,
        root: NodeId,
        insert: bool,
        stash: Stash,
        size: usize,
    },
    /// A node moved from one place to another.
    Move {
        node: NodeId,
        from: (NodeId, usize),
        to: (NodeId, usize),
    },
    /// A linked parameter's value changed.
    Param { id: Arc<str>, old: f64, new: f64 },
}

impl Op {
    /// Roughly how many bytes the step keeps alive.
    pub(crate) fn size(&self) -> usize {
        mem::size_of::<Self>()
            + match self {
                Self::Set { name, old, new, .. } => {
                    name.len()
                        + old.as_ref().map_or(0, Value::heap_size)
                        + new.as_ref().map_or(0, Value::heap_size)
                }
                Self::Structure { size, .. } => *size,
                Self::Move { .. } => 0,
                Self::Param { id, .. } => id.len(),
            }
    }

    /// Whether the step, merged, ended up changing nothing.
    fn is_no_op(&self) -> bool {
        match self {
            Self::Set { old, new, .. } => match (old, new) {
                (Some(old), Some(new)) => old.same(new),
                (None, None) => true,
                _ => false,
            },
            Self::Param { old, new, .. } => old.to_bits() == new.to_bits(),
            _ => false,
        }
    }
}

/// What a step is about, for merging later steps about the same thing.
#[derive(Debug, PartialEq, Eq, Hash)]
enum Key {
    Property(NodeId, Arc<str>),
    Param(Arc<str>),
}

/// A named group of steps, undone and redone as one.
#[derive(Debug)]
pub(crate) struct Transaction {
    pub(crate) name: Arc<str>,
    pub(crate) ops: Vec<Op>,
    pub(crate) bytes: usize,
    /// Where the step for each property or parameter is, while later steps
    /// may still merge into this transaction. Dropped once it is closed.
    index: Option<HashMap<Key, usize>>,
}

impl Transaction {
    pub(crate) fn new(name: Arc<str>) -> Self {
        Self {
            name: Arc::clone(&name),
            ops: Vec::new(),
            bytes: mem::size_of::<Self>() + name.len(),
            index: Some(HashMap::new()),
        }
    }

    /// Takes in `ops`, merging each change of a property or a parameter
    /// into an earlier change of the same one: the earlier step keeps its
    /// old value and takes the new one. Returns how many bytes it grew by,
    /// which may be negative.
    ///
    /// Merging is sound because a step on one property commutes with every
    /// step on anything else, and no step can come between two changes of
    /// a node's property that would make the node a different one: ids are
    /// never reused, so a removed node cannot get a property in the same
    /// transaction.
    pub(crate) fn absorb(&mut self, ops: Vec<Op>) -> isize {
        let before = self.bytes;
        let index = self.index.get_or_insert_with(HashMap::new);
        for op in ops {
            let key = match &op {
                Op::Set { node, name, .. } => Some(Key::Property(*node, Arc::clone(name))),
                Op::Param { id, .. } => Some(Key::Param(Arc::clone(id))),
                _ => None,
            };
            let earlier = key.as_ref().and_then(|key| index.get(key)).copied();
            match (earlier, op) {
                (Some(at), Op::Set { new, .. }) => {
                    if let Op::Set { new: merged, .. } = &mut self.ops[at] {
                        self.bytes -= merged.as_ref().map_or(0, Value::heap_size);
                        self.bytes += new.as_ref().map_or(0, Value::heap_size);
                        *merged = new;
                    }
                }
                (Some(at), Op::Param { new, .. }) => {
                    if let Op::Param { new: merged, .. } = &mut self.ops[at] {
                        *merged = new;
                    }
                }
                (_, op) => {
                    if let Some(key) = key {
                        index.insert(key, self.ops.len());
                    }
                    self.bytes += op.size();
                    self.ops.push(op);
                }
            }
        }
        self.bytes as isize - before as isize
    }

    /// Stops merging, and drops the steps that merged into nothing. Returns
    /// how many bytes it shrank by.
    pub(crate) fn close(&mut self) -> usize {
        self.index = None;
        let before = self.bytes;
        let mut freed = 0;
        self.ops.retain(|op| {
            let keep = !op.is_no_op();
            if !keep {
                freed += op.size();
            }
            keep
        });
        self.bytes -= freed;
        before - self.bytes
    }
}

/// How much history a document keeps.
///
/// When a new step would go past either limit, the oldest steps are
/// forgotten until it does not. The newest step is always kept, even when
/// it alone is over the byte limit, so the last edit can always be undone;
/// a step limit of zero keeps no history at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    /// How many steps, undo and redo together, are kept at most.
    pub max_steps: usize,
    /// Roughly how many bytes of history are kept at most. The count is an
    /// estimate that errs high: text and binary data shared between steps
    /// count once per step.
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    /// 500 steps and 32 MiB.
    fn default() -> Self {
        Self {
            max_steps: 500,
            max_bytes: 32 << 20,
        }
    }
}

/// The undo and redo stacks.
#[derive(Debug, Default)]
pub(crate) struct History {
    /// Oldest first; the next step to undo is the last.
    pub(crate) undo: VecDeque<Transaction>,
    /// The next step to redo is the last.
    pub(crate) redo: Vec<Transaction>,
    pub(crate) bytes: usize,
    pub(crate) limits: HistoryLimits,
}

impl History {
    pub(crate) fn push(&mut self, transaction: Transaction) {
        self.clear_redo();
        self.bytes += transaction.bytes;
        self.undo.push_back(transaction);
    }

    pub(crate) fn clear_redo(&mut self) {
        for transaction in self.redo.drain(..) {
            self.bytes -= transaction.bytes;
        }
    }

    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }

    pub(crate) fn grew(&mut self, by: isize) {
        self.bytes = self.bytes.saturating_add_signed(by);
    }

    pub(crate) fn len(&self) -> usize {
        self.undo.len() + self.redo.len()
    }

    /// Forgets the oldest steps until the history is within its limits.
    /// Returns whether the newest undo step was forgotten too.
    pub(crate) fn trim(&mut self) -> bool {
        let limits = self.limits;
        let over =
            |history: &Self| history.len() > limits.max_steps || history.bytes > limits.max_bytes;
        let keep = usize::from(limits.max_steps > 0);
        let had = self.undo.len();
        while over(self) && self.undo.len() > keep {
            let oldest = self.undo.pop_front().expect("not empty");
            self.bytes -= oldest.bytes;
        }
        // Redo steps go only once every undo step but the newest has, the
        // furthest from the present first.
        while over(self) && !self.redo.is_empty() {
            let furthest = self.redo.remove(0);
            self.bytes -= furthest.bytes;
        }
        had > 0 && self.undo.is_empty()
    }
}
