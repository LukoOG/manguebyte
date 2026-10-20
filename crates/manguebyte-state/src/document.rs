//! The document: a tree of nodes, edited in transactions that can be undone.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem;
use std::sync::{Arc, Weak};

use manguebyte_params::ParamSet;

use crate::history::{History, HistoryLimits, Op, Transaction};
use crate::journal::{ChangeKind, Changes, Cursor, Journal, Origin, VersionWatch};
use crate::tree::{MAX_DEPTH, NodeData, Stash, Tree};
use crate::value::is_valid_name;
use crate::{NodeId, NodeTree, Value};

/// How many changes the journal keeps unless told otherwise.
const DEFAULT_JOURNAL: usize = 4096;

/// Why an edit was refused. A refused edit inside
/// [`Document::edit`] makes the whole transaction be taken back.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditError {
    /// There is no node with this id: it never existed, or it was removed.
    NoSuchNode(NodeId),
    /// A kind or a property name is not printable ASCII without spaces and
    /// `=` (see [`is_valid_name`](crate::is_valid_name)).
    InvalidName(String),
    /// A float that is NaN or infinite.
    NotFinite,
    /// A child index past the end of the children.
    IndexOutOfRange {
        /// The index asked for.
        index: usize,
        /// How many places there are.
        len: usize,
    },
    /// The root cannot be removed or moved.
    Root,
    /// A node cannot be moved under itself.
    IntoItself,
    /// The edit would make the tree deeper than [`MAX_DEPTH`].
    TooDeep,
    /// The document has no parameters linked, or they no longer exist.
    NoParamLink,
    /// The linked parameters have none with this id.
    NoSuchParam(String),
    /// The edit would break the parameter mirror: removing the mirror node
    /// or a parameter's property, or giving a parameter something that is
    /// not a number.
    Mirror,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchNode(id) => write!(f, "there is no node {id}"),
            Self::InvalidName(name) => write!(f, "{name:?} is not a valid name"),
            Self::NotFinite => f.write_str("a float must be finite"),
            Self::IndexOutOfRange { index, len } => {
                write!(f, "index {index} is out of range for {len} places")
            }
            Self::Root => f.write_str("the root cannot be removed or moved"),
            Self::IntoItself => f.write_str("a node cannot be moved under itself"),
            Self::TooDeep => write!(f, "the tree would be deeper than {MAX_DEPTH} levels"),
            Self::NoParamLink => f.write_str("no parameters are linked"),
            Self::NoSuchParam(id) => write!(f, "there is no parameter \"{id}\""),
            Self::Mirror => f.write_str("the edit would break the parameter mirror"),
        }
    }
}

impl std::error::Error for EditError {}

/// How an edit joins the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A transaction of its own.
    Plain,
    /// Part of the open gesture.
    Gesture,
    /// Merged with the edits just before it that had the same key.
    Key(u64),
}

/// The parameters a document mirrors.
struct ParamLink {
    params: Weak<dyn ParamSet>,
    node: NodeId,
}

/// A tree of typed nodes with properties, edited in named transactions
/// that can be undone and redone.
///
/// # The tree
///
/// Every node has a kind (`zone`), properties by name holding a [`Value`],
/// and ordered children. Each node has a [`NodeId`] that never changes and
/// is never given to another node, so references between nodes
/// ([`Value::Node`]) survive any edit, undo or redo, and saving.
///
/// # Edits
///
/// Every change goes through [`edit`](Self::edit), which runs a closure
/// with a [`Tx`]: everything the closure does is one step of history,
/// undone and redone as one by [`undo`](Self::undo) and
/// [`redo`](Self::redo). If the closure fails, what it did is taken back.
///
/// - A continuous gesture, such as dragging a knob, is one step from
///   [`begin_gesture`](Self::begin_gesture) to
///   [`end_gesture`](Self::end_gesture): every edit in between joins it, and
///   repeated changes of one property keep only the first old value and the
///   last new one, so a drag of a thousand frames costs what one change
///   costs.
/// - Edits with no clear end, such as nudging with the arrow keys, merge
///   through [`edit_merging`](Self::edit_merging) while they share a key and
///   nothing else happens in between.
/// - **The redo branch.** A new step discards every step that could be
///   redone; there is one line of history, never a tree. An edit that
///   changes nothing makes no step and keeps the redo steps.
/// - [`HistoryLimits`] bound the history by steps and by bytes; the oldest
///   steps go first.
///
/// # Finding out what changed
///
/// There are no listeners and no callbacks (ADR 0003, ADR 0008). Every
/// change bumps the [`version`](Self::version) and goes into a bounded
/// journal, which any number of readers drain at their own pace, each with
/// its own [`Cursor`]: see [`changes`](Self::changes). Another thread can
/// watch the version through a [`VersionWatch`] without locking anything.
/// The audio thread never sees the document: it is given immutable
/// snapshots built from it ([`snapshot`](crate::snapshot)).
///
/// # Parameters
///
/// A document can mirror a plugin's parameters on one node, with
/// [`link_params`](Self::link_params). [`Tx::set_param`] changes a
/// parameter as an undoable step; [`sync_params`](Self::sync_params) brings
/// the mirror up to date with whatever a host's automation did, without
/// touching the history.
///
/// ```
/// use manguebyte_state::{Document, Value};
///
/// let mut doc = Document::new("instrument");
/// let zone = doc.edit("Add zone", |tx| {
///     let zone = tx.add_child(tx.root(), "zone")?;
///     tx.set(zone, "key", 60)?;
///     Ok(zone)
/// })?;
///
/// doc.begin_gesture("Change zone gain");
/// for frame in 0..100 {
///     doc.edit("drag", |tx| tx.set(zone, "gain", f64::from(frame) / 100.0))?;
/// }
/// doc.end_gesture();
///
/// assert_eq!(doc.undo_name(), Some("Change zone gain"));
/// assert!(doc.undo());
/// assert_eq!(doc.get(zone, "gain"), None);
/// assert!(doc.undo());
/// assert!(!doc.contains(zone));
/// assert!(doc.redo());
/// assert_eq!(doc.get(zone, "key"), Some(&Value::Int(60)), "the same node, the same id");
/// # Ok::<(), manguebyte_state::EditError>(())
/// ```
pub struct Document {
    tree: Tree,
    schema: u32,
    history: History,
    journal: Journal,
    link: Option<ParamLink>,
    gesture: Option<Arc<str>>,
    merge: Option<Mode>,
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("nodes", &self.tree.nodes.len())
            .field("schema", &self.schema)
            .field("version", &self.version())
            .field("undo", &self.history.undo.len())
            .field("redo", &self.history.redo.len())
            .field("param_mirror", &self.param_mirror())
            .finish_non_exhaustive()
    }
}

impl Document {
    /// An empty document whose root is of kind `root_kind`, at schema 0.
    ///
    /// # Panics
    ///
    /// Panics if `root_kind` is not a valid name (see
    /// [`is_valid_name`](crate::is_valid_name)).
    pub fn new(root_kind: &str) -> Self {
        assert!(is_valid_name(root_kind), "invalid kind {root_kind:?}");
        Self::with_tree(Tree::new(Arc::from(root_kind)), 0)
    }

    fn with_tree(tree: Tree, schema: u32) -> Self {
        Self {
            tree,
            schema,
            history: History::default(),
            journal: Journal::new(DEFAULT_JOURNAL),
            link: None,
            gesture: None,
            merge: None,
        }
    }

    /// A document holding `tree`, which becomes its root.
    ///
    /// Nodes keep the ids they carry, except that the root is always
    /// [`NodeId::ROOT`], and a node with no id, or with an id an earlier
    /// node already took, gets a fresh one. References between nodes are
    /// kept as they are.
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidName`], [`EditError::NotFinite`] or
    /// [`EditError::TooDeep`] if the tree could not have been made by edits.
    pub fn from_tree(tree: NodeTree) -> Result<Self, EditError> {
        Ok(Self::build(tree, 0, 0)?.0)
    }

    /// Builds a document from a tree, returning how many nodes had to take
    /// a fresh id instead of the one they carried.
    pub(crate) fn build(
        root: NodeTree,
        next_id: u64,
        schema: u32,
    ) -> Result<(Self, usize), EditError> {
        check_tree(&root, 0)?;
        // Every id the tree mentions, so that fresh ones come after all.
        let mut highest = 0;
        let mut stack = vec![&root];
        while let Some(node) = stack.pop() {
            if let Some(id) = node.id {
                highest = highest.max(id.0);
            }
            stack.extend(node.children.iter());
        }
        let mut tree = Tree {
            nodes: HashMap::new(),
            next_id: next_id.max(highest.saturating_add(1)).max(1),
        };
        let mut used = HashSet::from([NodeId::ROOT]);
        let mut renumbered = usize::from(root.id.is_some_and(|id| id != NodeId::ROOT));
        let mut stack = vec![(root, NodeId::ROOT, None)];
        while let Some((node, id, parent)) = stack.pop() {
            let mut data = NodeData::new(Arc::from(node.kind.as_str()), parent);
            for (name, value) in node.props {
                let name: Arc<str> = Arc::from(name);
                match data
                    .props
                    .binary_search_by(|(candidate, _)| candidate.cmp(&name))
                {
                    Ok(index) => data.props[index].1 = value,
                    Err(index) => data.props.insert(index, (name, value)),
                }
            }
            // Siblings claim their ids in order, so the first of two that
            // carry the same id keeps it.
            let mut children = Vec::with_capacity(node.children.len());
            for child in node.children {
                let child_id = match child.id {
                    Some(wanted) if used.insert(wanted) => wanted,
                    carried => {
                        renumbered += usize::from(carried.is_some());
                        let fresh = tree.alloc_id();
                        used.insert(fresh);
                        fresh
                    }
                };
                data.children.push(child_id);
                children.push((child, child_id, Some(id)));
            }
            stack.extend(children.into_iter().rev());
            tree.nodes.insert(id, data);
        }
        Ok((Self::with_tree(tree, schema), renumbered))
    }

    // --- Reading ---

    /// The root, always [`NodeId::ROOT`].
    pub fn root(&self) -> NodeId {
        NodeId::ROOT
    }

    /// Whether the node exists now.
    pub fn contains(&self, node: NodeId) -> bool {
        self.tree.nodes.contains_key(&node)
    }

    /// How many nodes the document holds, the root included.
    pub fn len(&self) -> usize {
        self.tree.nodes.len()
    }

    /// Always `false`: a document holds at least its root.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The node's kind.
    pub fn kind(&self, node: NodeId) -> Option<&str> {
        self.tree.node(node).map(|data| &*data.kind)
    }

    /// The node's parent: `None` for the root and for nodes that do not
    /// exist.
    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.tree.node(node).and_then(|data| data.parent)
    }

    /// The node's children in order; empty if it has none or does not
    /// exist.
    pub fn children(&self, node: NodeId) -> &[NodeId] {
        self.tree
            .node(node)
            .map_or(&[], |data| data.children.as_slice())
    }

    /// The node's children of the given kind, in order.
    pub fn children_of_kind<'a>(
        &'a self,
        node: NodeId,
        kind: &'a str,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.children(node)
            .iter()
            .copied()
            .filter(move |child| self.kind(*child) == Some(kind))
    }

    /// The node's place among its parent's children.
    pub fn index_in_parent(&self, node: NodeId) -> Option<usize> {
        let parent = self.parent(node)?;
        self.children(parent)
            .iter()
            .position(|child| *child == node)
    }

    /// The property `name` of `node`.
    pub fn get(&self, node: NodeId, name: &str) -> Option<&Value> {
        self.tree.node(node)?.get(name)
    }

    /// The node's properties, sorted by name.
    pub fn properties(&self, node: NodeId) -> impl Iterator<Item = (&str, &Value)> {
        self.tree
            .node(node)
            .into_iter()
            .flat_map(|data| data.props.iter().map(|(name, value)| (&**name, value)))
    }

    /// An owned copy of `node` and everything under it, ids included.
    pub fn subtree(&self, node: NodeId) -> Option<NodeTree> {
        self.tree.to_node_tree(node)
    }

    /// The version of the application's own document format, saved with
    /// the document and used by [`Migrations`](crate::Migrations). Zero
    /// unless set.
    pub fn schema(&self) -> u32 {
        self.schema
    }

    /// Sets the [`schema`](Self::schema) version. Not an edit: it is not in
    /// the history and makes no change in the journal.
    pub fn set_schema(&mut self, schema: u32) {
        self.schema = schema;
    }

    /// The id the next new node will get.
    pub(crate) fn next_id(&self) -> u64 {
        self.tree.next_id
    }

    // --- Editing ---

    /// Runs `edit` as one step of history named `name`, such as "Add
    /// zone". Returns what `edit` returns.
    ///
    /// While a gesture is open the edit joins it instead, and `name` is not
    /// used. An edit that changes nothing makes no step.
    ///
    /// # Errors
    ///
    /// Whatever `edit` returns. Everything it changed before failing is
    /// taken back first, so a failed edit leaves the document as it was
    /// (its version and journal still record the change and its reversal).
    pub fn edit<R>(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut Tx<'_>) -> Result<R, EditError>,
    ) -> Result<R, EditError> {
        self.run(name, Mode::Plain, edit)
    }

    /// Runs `edit` as [`edit`](Self::edit) does, but merges it into the
    /// step before if that step was made by this method with the same `key`
    /// and nothing has happened in between: no other edit, undo, redo or
    /// gesture. For edits that come in a series with no clear end, such as
    /// nudging a zone with the arrow keys or turning a mouse wheel.
    ///
    /// # Errors
    ///
    /// As for [`edit`](Self::edit).
    pub fn edit_merging<R>(
        &mut self,
        name: &str,
        key: impl Hash,
        edit: impl FnOnce(&mut Tx<'_>) -> Result<R, EditError>,
    ) -> Result<R, EditError> {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        self.run(name, Mode::Key(hasher.finish()), edit)
    }

    fn run<R>(
        &mut self,
        name: &str,
        mode: Mode,
        edit: impl FnOnce(&mut Tx<'_>) -> Result<R, EditError>,
    ) -> Result<R, EditError> {
        let mut tx = Tx {
            doc: self,
            ops: Vec::new(),
        };
        let result = edit(&mut tx);
        let Tx { ops, .. } = tx;
        match result {
            Ok(value) => {
                self.record(name, ops, mode);
                Ok(value)
            }
            Err(error) => {
                for mut op in ops.into_iter().rev() {
                    self.apply(&mut op, false, Origin::Edit);
                }
                Err(error)
            }
        }
    }

    fn record(&mut self, name: &str, ops: Vec<Op>, mode: Mode) {
        if ops.is_empty() {
            return;
        }
        let mode = if self.gesture.is_some() {
            Mode::Gesture
        } else {
            mode
        };
        if mode != Mode::Plain && self.merge == Some(mode) {
            if let Some(top) = self.history.undo.back_mut() {
                let grew = top.absorb(ops);
                self.history.grew(grew);
                self.trim();
                return;
            }
        }
        self.close_merge();
        let name = match (&self.gesture, mode) {
            (Some(gesture), Mode::Gesture) => Arc::clone(gesture),
            _ => Arc::from(name),
        };
        let mut transaction = Transaction::new(name);
        transaction.absorb(ops);
        if mode == Mode::Plain {
            transaction.close();
            if transaction.ops.is_empty() {
                return;
            }
        }
        self.history.push(transaction);
        self.merge = (mode != Mode::Plain).then_some(mode);
        self.trim();
    }

    /// Stops the newest step from taking in more edits, and drops it if
    /// what it took in adds up to nothing.
    fn close_merge(&mut self) {
        if self.merge.take().is_none() {
            return;
        }
        let Some(top) = self.history.undo.back_mut() else {
            return;
        };
        let freed = top.close();
        let empty = top.ops.is_empty();
        self.history.grew(-(freed as isize));
        if empty {
            let empty = self.history.undo.pop_back().expect("top");
            self.history.grew(-(empty.bytes as isize));
        }
    }

    fn trim(&mut self) {
        if self.history.trim() {
            self.merge = None;
        }
    }

    /// Opens a gesture named `name`, such as "Change filter cutoff": every
    /// edit until [`end_gesture`](Self::end_gesture) joins one step of
    /// history. Opening a gesture while another is open ends that one.
    ///
    /// For a plugin parameter, also tell the host, with
    /// `manguebyte_plugin::editor::Gesture`: this gesture is about the
    /// history, that one about automation.
    pub fn begin_gesture(&mut self, name: &str) {
        self.close_merge();
        self.gesture = Some(Arc::from(name));
    }

    /// Ends the open gesture, if there is one. A gesture whose edits add up
    /// to nothing, a knob dragged back where it started, leaves no step.
    pub fn end_gesture(&mut self) {
        self.gesture = None;
        if self.merge == Some(Mode::Gesture) {
            self.close_merge();
        }
    }

    /// Whether a gesture is open.
    pub fn is_gesture_open(&self) -> bool {
        self.gesture.is_some()
    }

    /// Undoes the newest step. Returns `false` if there is none. Ends an
    /// open gesture first.
    pub fn undo(&mut self) -> bool {
        self.end_gesture();
        self.close_merge();
        let Some(mut transaction) = self.history.undo.pop_back() else {
            return false;
        };
        for op in transaction.ops.iter_mut().rev() {
            self.apply(op, false, Origin::Undo);
        }
        self.history.redo.push(transaction);
        true
    }

    /// Redoes the step undone last. Returns `false` if there is none.
    pub fn redo(&mut self) -> bool {
        self.end_gesture();
        self.close_merge();
        let Some(mut transaction) = self.history.redo.pop() else {
            return false;
        };
        for op in &mut transaction.ops {
            self.apply(op, true, Origin::Redo);
        }
        self.history.undo.push_back(transaction);
        true
    }

    /// Whether there is a step to undo.
    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    /// Whether there is a step to redo.
    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// The name of the step [`undo`](Self::undo) would undo, for an "Undo
    /// …" menu item.
    pub fn undo_name(&self) -> Option<&str> {
        self.history.undo.back().map(|step| &*step.name)
    }

    /// The name of the step [`redo`](Self::redo) would redo.
    pub fn redo_name(&self) -> Option<&str> {
        self.history.redo.last().map(|step| &*step.name)
    }

    /// The names of every step that can be undone, the next one first.
    pub fn undo_names(&self) -> impl Iterator<Item = &str> {
        self.history.undo.iter().rev().map(|step| &*step.name)
    }

    /// The names of every step that can be redone, the next one first.
    pub fn redo_names(&self) -> impl Iterator<Item = &str> {
        self.history.redo.iter().rev().map(|step| &*step.name)
    }

    /// Roughly how many bytes the history keeps alive.
    pub fn history_bytes(&self) -> usize {
        self.history.bytes
    }

    /// The limits on the history.
    pub fn history_limits(&self) -> HistoryLimits {
        self.history.limits
    }

    /// Sets the limits on the history, forgetting steps at once if it is
    /// over them.
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.history.limits = limits;
        self.trim();
    }

    /// Forgets every step, to undo and to redo. Ends an open gesture.
    pub fn clear_history(&mut self) {
        self.gesture = None;
        self.merge = None;
        self.history.clear();
    }

    /// Replaces the whole document with `other`, as when the user opens a
    /// file. Not a step of history: the history is cleared, an open gesture
    /// ended, and the journal records one [`ChangeKind::Replaced`].
    ///
    /// Linked parameters stay linked if `other` has a node with the
    /// mirror's id, and then **the document wins**: each parameter takes
    /// the value its mirror property holds. Otherwise the link is dropped.
    /// What `other` was linked to is not kept.
    pub fn replace(&mut self, other: Document) {
        if self.take(other) {
            self.restore_params_from(Origin::Load);
        }
    }

    /// Replaces the whole document with `other`, as
    /// [`replace`](Self::replace) does, except that **the parameters win**:
    /// the mirror is rewritten from them. For a host restoring a plugin's
    /// state, where the parameter values were restored from the same state
    /// and are the truth; [`SharedDocument`](crate::SharedDocument) does
    /// this.
    pub fn replace_keeping_params(&mut self, other: Document) {
        if self.take(other) {
            self.sync_params_from(Origin::Load);
        }
    }

    /// Takes `other`'s tree and schema. Returns whether the parameter link
    /// survived.
    fn take(&mut self, other: Document) -> bool {
        self.clear_history();
        self.tree = other.tree;
        self.schema = other.schema;
        self.journal
            .record(NodeId::ROOT, ChangeKind::Replaced, Origin::Load);
        match &self.link {
            Some(link) if self.tree.nodes.contains_key(&link.node) => true,
            Some(_) => {
                self.link = None;
                false
            }
            None => false,
        }
    }

    // --- Changes ---

    /// The number of changes ever made: it goes up by one with each, and
    /// never down. Undoing a change is a change.
    pub fn version(&self) -> u64 {
        self.journal.seq()
    }

    /// A [`VersionWatch`] on this document, for another thread to poll.
    pub fn watch(&self) -> VersionWatch {
        self.journal.watch()
    }

    /// A cursor at the present: [`changes`](Self::changes) from it gives
    /// only what changes after this call.
    pub fn cursor(&self) -> Cursor {
        self.journal.cursor()
    }

    /// The changes since `cursor`, oldest first, and moves the cursor to
    /// the present. Allocates nothing.
    ///
    /// ```
    /// use manguebyte_state::{ChangeKind, Changes, Document, Origin};
    ///
    /// let mut doc = Document::new("song");
    /// let mut panel = doc.cursor(); // An editor panel's own cursor.
    /// doc.edit("Rename", |tx| tx.set(tx.root(), "title", "Maracatu"))?;
    /// doc.undo();
    ///
    /// let Changes::Since(changes) = doc.changes(&mut panel) else {
    ///     unreachable!("the journal holds the last 4096 changes");
    /// };
    /// let origins: Vec<Origin> = changes.map(|change| change.origin).collect();
    /// assert_eq!(origins, [Origin::Edit, Origin::Undo]);
    /// # Ok::<(), manguebyte_state::EditError>(())
    /// ```
    pub fn changes(&self, cursor: &mut Cursor) -> Changes<'_> {
        self.journal.since(cursor)
    }

    /// How many changes the journal keeps. 4096 unless set.
    pub fn journal_capacity(&self) -> usize {
        self.journal.capacity()
    }

    /// Sets how many changes the journal keeps, at least one. A reader
    /// that falls further behind gets [`Changes::Missed`].
    pub fn set_journal_capacity(&mut self, capacity: usize) {
        self.journal.set_capacity(capacity);
    }

    // --- Parameters ---

    /// Mirrors `params` on `node`: a float property per parameter, named
    /// by its id and holding its real value. Brings the mirror up to date
    /// at once, outside the history, and returns how many properties that
    /// changed.
    ///
    /// The document keeps only a weak reference, so `params` may hold the
    /// document (as a plugin's parameters do through
    /// [`SharedDocument`](crate::SharedDocument)) without making a cycle.
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`] if `node` does not exist.
    pub fn link_params<P: ParamSet + 'static>(
        &mut self,
        params: &Arc<P>,
        node: NodeId,
    ) -> Result<usize, EditError> {
        if !self.contains(node) {
            return Err(EditError::NoSuchNode(node));
        }
        let params: Weak<P> = Arc::downgrade(params);
        let params: Weak<dyn ParamSet> = params;
        self.link = Some(ParamLink { params, node });
        Ok(self.sync_params())
    }

    /// Stops mirroring parameters. The mirror's properties stay as they
    /// are.
    pub fn unlink_params(&mut self) {
        self.link = None;
    }

    /// The node that mirrors the linked parameters.
    pub fn param_mirror(&self) -> Option<NodeId> {
        self.link.as_ref().map(|link| link.node)
    }

    fn linked(&self) -> Option<(Arc<dyn ParamSet>, NodeId)> {
        let link = self.link.as_ref()?;
        Some((link.params.upgrade()?, link.node))
    }

    /// Brings the parameter mirror up to date with the parameters, which a
    /// host may have automated since. Returns how many properties changed.
    ///
    /// Not in the history: automation is the host's to record, and an undo
    /// should not take back what an automation lane did. Each property it
    /// changes is in the journal with [`Origin::Params`]. Call it on the
    /// interface's timer.
    pub fn sync_params(&mut self) -> usize {
        self.sync_params_from(Origin::Params)
    }

    fn sync_params_from(&mut self, origin: Origin) -> usize {
        let Some((params, _)) = self.linked() else {
            return 0;
        };
        let mut values = Vec::new();
        params.visit(&mut |param| values.push((param.id(), param.plain())));
        values
            .into_iter()
            .filter(|(id, value)| self.mirror(id, *value, origin))
            .count()
    }

    /// Writes each number the mirror holds into its parameter, then brings
    /// the mirror up to date with what the parameters made of them, such
    /// as a value clamped to a narrower range. Returns how many parameters
    /// were written. Not in the history.
    ///
    /// For a document read from a file whose parameter values should win
    /// over the live ones; [`replace`](Self::replace) does this itself.
    pub fn restore_params(&mut self) -> usize {
        self.restore_params_from(Origin::Load)
    }

    fn restore_params_from(&mut self, origin: Origin) -> usize {
        let Some((params, node)) = self.linked() else {
            return 0;
        };
        let mut written = 0;
        params.visit(&mut |param| {
            if let Some(value) = self.get(node, param.id()).and_then(Value::as_f64) {
                param.set_plain(value);
                written += 1;
            }
        });
        self.sync_params_from(origin);
        written
    }

    /// Sets the mirror property of the parameter `id` to `value`, recording
    /// the change in the journal if there was one.
    fn mirror(&mut self, id: &str, value: f64, origin: Origin) -> bool {
        let Some(node) = self.link.as_ref().map(|link| link.node) else {
            return false;
        };
        let Some(data) = self.tree.node(node) else {
            return false;
        };
        let new = Value::Float(value);
        let name = match data.props.iter().find(|(candidate, _)| &**candidate == id) {
            Some((_, old)) if old.same(&new) => return false,
            Some((name, _)) => Arc::clone(name),
            None => Arc::from(id),
        };
        self.tree.set_prop(node, &name, Some(new));
        self.journal
            .record(node, ChangeKind::Property { name }, origin);
        true
    }

    /// Writes a linked parameter and its mirror.
    fn write_param(&mut self, id: &str, value: f64, origin: Origin) {
        let stored = match self.linked() {
            Some((params, _)) => match params.find(id) {
                Some(param) => {
                    param.set_plain(value);
                    param.plain()
                }
                None => value,
            },
            None => value,
        };
        self.mirror(id, stored, origin);
    }

    /// Applies one step forward or backward, recording what it changes in
    /// the journal.
    fn apply(&mut self, op: &mut Op, forward: bool, origin: Origin) {
        match op {
            Op::Set {
                node,
                name,
                old,
                new,
            } => {
                let value = if forward { new.clone() } else { old.clone() };
                self.tree.set_prop(*node, name, value);
                self.journal.record(
                    *node,
                    ChangeKind::Property {
                        name: Arc::clone(name),
                    },
                    origin,
                );
            }
            Op::Structure {
                parent,
                index,
                root,
                insert,
                stash,
                ..
            } => {
                if forward == *insert {
                    self.tree.attach(*parent, *index, mem::take(stash));
                    self.journal.record(
                        *parent,
                        ChangeKind::ChildAdded {
                            child: *root,
                            index: *index,
                        },
                        origin,
                    );
                } else {
                    let (_, _, taken) = self.tree.detach(*root);
                    *stash = taken;
                    self.journal.record(
                        *parent,
                        ChangeKind::ChildRemoved {
                            child: *root,
                            index: *index,
                        },
                        origin,
                    );
                }
            }
            Op::Move { node, from, to } => {
                let (source, target) = if forward { (*from, *to) } else { (*to, *from) };
                self.tree.relocate(*node, target.0, target.1);
                self.journal.record(
                    source.0,
                    ChangeKind::ChildRemoved {
                        child: *node,
                        index: source.1,
                    },
                    origin,
                );
                self.journal.record(
                    target.0,
                    ChangeKind::ChildAdded {
                        child: *node,
                        index: target.1,
                    },
                    origin,
                );
            }
            Op::Param { id, old, new } => {
                let value = if forward { *new } else { *old };
                let id = Arc::clone(id);
                self.write_param(&id, value, origin);
            }
        }
    }
}

/// Checks that a tree could have been made by edits: valid names, finite
/// floats, and no deeper than [`MAX_DEPTH`] once placed at `depth`.
fn check_tree(tree: &NodeTree, depth: usize) -> Result<(), EditError> {
    let mut stack = vec![(tree, depth)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(EditError::TooDeep);
        }
        if !is_valid_name(&node.kind) {
            return Err(EditError::InvalidName(node.kind.clone()));
        }
        for (name, value) in &node.props {
            if !is_valid_name(name) {
                return Err(EditError::InvalidName(name.clone()));
            }
            if !value.is_storable() {
                return Err(EditError::NotFinite);
            }
        }
        stack.extend(node.children.iter().map(|child| (child, depth + 1)));
    }
    Ok(())
}

/// The edits of one transaction, handed to the closure of
/// [`Document::edit`].
///
/// Every method checks its arguments first and changes nothing when it
/// fails. Reading goes through [`doc`](Self::doc), which sees every change
/// made so far in the transaction.
pub struct Tx<'a> {
    doc: &'a mut Document,
    ops: Vec<Op>,
}

impl fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tx")
            .field("steps", &self.ops.len())
            .finish_non_exhaustive()
    }
}

impl Tx<'_> {
    /// The document as it is now, this transaction's changes included.
    pub fn doc(&self) -> &Document {
        self.doc
    }

    /// The root, always [`NodeId::ROOT`].
    pub fn root(&self) -> NodeId {
        NodeId::ROOT
    }

    fn check_node(&self, node: NodeId) -> Result<(), EditError> {
        if self.doc.contains(node) {
            Ok(())
        } else {
            Err(EditError::NoSuchNode(node))
        }
    }

    fn check_name(name: &str) -> Result<(), EditError> {
        if is_valid_name(name) {
            Ok(())
        } else {
            Err(EditError::InvalidName(name.to_owned()))
        }
    }

    /// Whether `name` on `node` is a linked parameter's mirror property.
    fn mirrored(&self, node: NodeId, name: &str) -> bool {
        self.doc
            .linked()
            .is_some_and(|(params, mirror)| mirror == node && params.find(name).is_some())
    }

    /// Sets the property `name` of `node`.
    ///
    /// On the parameter mirror, a property named after a linked parameter
    /// is that parameter: the value, a number or a switch, goes through
    /// [`set_param`](Self::set_param).
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`], [`EditError::InvalidName`],
    /// [`EditError::NotFinite`], and on the mirror [`EditError::Mirror`]
    /// for a value that is not a number or a switch.
    pub fn set(
        &mut self,
        node: NodeId,
        name: &str,
        value: impl Into<Value>,
    ) -> Result<(), EditError> {
        let value = value.into();
        Self::check_name(name)?;
        self.check_node(node)?;
        if !value.is_storable() {
            return Err(EditError::NotFinite);
        }
        if self.mirrored(node, name) {
            let plain = match value {
                Value::Bool(on) => f64::from(u8::from(on)),
                other => other.as_f64().ok_or(EditError::Mirror)?,
            };
            return self.set_param(name, plain).map(drop);
        }
        let data = self.doc.tree.node(node).expect("checked");
        let (name, old) = match data
            .props
            .iter()
            .find(|(candidate, _)| &**candidate == name)
        {
            Some((_, old)) if old.same(&value) => return Ok(()),
            Some((name, old)) => (Arc::clone(name), Some(old.clone())),
            None => (Arc::from(name), None),
        };
        self.doc.tree.set_prop(node, &name, Some(value.clone()));
        self.doc.journal.record(
            node,
            ChangeKind::Property {
                name: Arc::clone(&name),
            },
            Origin::Edit,
        );
        self.ops.push(Op::Set {
            node,
            name,
            old,
            new: Some(value),
        });
        Ok(())
    }

    /// Removes the property `name` of `node`, returning what it held.
    /// Removing a property that is not there changes nothing.
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`], and [`EditError::Mirror`] for a linked
    /// parameter's mirror property.
    pub fn remove_property(
        &mut self,
        node: NodeId,
        name: &str,
    ) -> Result<Option<Value>, EditError> {
        self.check_node(node)?;
        if self.mirrored(node, name) {
            return Err(EditError::Mirror);
        }
        let data = self.doc.tree.node(node).expect("checked");
        let Some(name) = data
            .props
            .iter()
            .find(|(candidate, _)| &**candidate == name)
            .map(|(name, _)| Arc::clone(name))
        else {
            return Ok(None);
        };
        let old = self.doc.tree.set_prop(node, &name, None);
        self.doc.journal.record(
            node,
            ChangeKind::Property {
                name: Arc::clone(&name),
            },
            Origin::Edit,
        );
        self.ops.push(Op::Set {
            node,
            name,
            old: old.clone(),
            new: None,
        });
        Ok(old)
    }

    /// Appends a new node of `kind` to `parent`'s children and returns its
    /// id.
    ///
    /// # Errors
    ///
    /// As for [`insert_child`](Self::insert_child).
    pub fn add_child(&mut self, parent: NodeId, kind: &str) -> Result<NodeId, EditError> {
        let index = self.doc.children(parent).len();
        self.insert_child(parent, index, kind)
    }

    /// Inserts a new node of `kind` as child `index` of `parent` and returns
    /// its id, which no node has had before.
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`], [`EditError::InvalidName`],
    /// [`EditError::IndexOutOfRange`] or [`EditError::TooDeep`].
    pub fn insert_child(
        &mut self,
        parent: NodeId,
        index: usize,
        kind: &str,
    ) -> Result<NodeId, EditError> {
        self.insert_tree(parent, index, &NodeTree::new(kind))
    }

    /// Inserts a copy of `tree` as child `index` of `parent`, with fresh
    /// ids for every node, and returns the id of its root. A
    /// [`Value::Node`] inside `tree` that refers to a node of `tree` by the
    /// id it carries is made to refer to that node's copy, so a pasted
    /// group keeps its own references.
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`], [`EditError::InvalidName`],
    /// [`EditError::NotFinite`], [`EditError::IndexOutOfRange`] or
    /// [`EditError::TooDeep`].
    pub fn insert_tree(
        &mut self,
        parent: NodeId,
        index: usize,
        tree: &NodeTree,
    ) -> Result<NodeId, EditError> {
        self.check_node(parent)?;
        let len = self.doc.children(parent).len();
        if index > len {
            return Err(EditError::IndexOutOfRange { index, len });
        }
        check_tree(tree, self.doc.tree.depth(parent) + 1)?;

        // Fresh ids, parents first, and where each carried id went.
        let mut order: Vec<(&NodeTree, NodeId, Option<NodeId>)> = Vec::new();
        let mut renamed = HashMap::new();
        let root = self.doc.tree.alloc_id();
        let mut stack = vec![(tree, root, None)];
        while let Some((node, id, node_parent)) = stack.pop() {
            if let Some(carried) = node.id {
                renamed.entry(carried).or_insert(id);
            }
            order.push((node, id, node_parent));
            let ids: Vec<NodeId> = node
                .children
                .iter()
                .map(|_| self.doc.tree.alloc_id())
                .collect();
            for (child, child_id) in node.children.iter().zip(ids).rev() {
                stack.push((child, child_id, Some(id)));
            }
        }
        let mut children: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for (_, id, node_parent) in &order {
            if let Some(node_parent) = node_parent {
                children.entry(*node_parent).or_default().push(*id);
            }
        }
        let stash: Stash = order
            .into_iter()
            .map(|(node, id, node_parent)| {
                let mut data = NodeData::new(Arc::from(node.kind.as_str()), node_parent);
                for (name, value) in &node.props {
                    let value = match value {
                        Value::Node(target) => Value::Node(*renamed.get(target).unwrap_or(target)),
                        other => other.clone(),
                    };
                    let name: Arc<str> = Arc::from(name.as_str());
                    match data
                        .props
                        .binary_search_by(|(candidate, _)| candidate.cmp(&name))
                    {
                        Ok(at) => data.props[at].1 = value,
                        Err(at) => data.props.insert(at, (name, value)),
                    }
                }
                data.children = children.remove(&id).unwrap_or_default();
                (id, data)
            })
            .collect();
        let size = stash.iter().map(|(_, data)| data.size()).sum();
        self.doc.tree.attach(parent, index, stash);
        self.doc.journal.record(
            parent,
            ChangeKind::ChildAdded { child: root, index },
            Origin::Edit,
        );
        self.ops.push(Op::Structure {
            parent,
            index,
            root,
            insert: true,
            stash: Vec::new(),
            size,
        });
        Ok(root)
    }

    /// Removes `node` and everything under it. Its id is not given to any
    /// other node, so an undo brings back the very same node.
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`], [`EditError::Root`], and
    /// [`EditError::Mirror`] if the parameter mirror is `node` or under it.
    pub fn remove_node(&mut self, node: NodeId) -> Result<(), EditError> {
        if node == NodeId::ROOT {
            return Err(EditError::Root);
        }
        self.check_node(node)?;
        if let Some(mirror) = self.doc.param_mirror() {
            if self.doc.tree.is_within(mirror, node) {
                return Err(EditError::Mirror);
            }
        }
        let size = self.doc.tree.subtree_size(node);
        let (parent, index, stash) = self.doc.tree.detach(node);
        self.doc.journal.record(
            parent,
            ChangeKind::ChildRemoved { child: node, index },
            Origin::Edit,
        );
        self.ops.push(Op::Structure {
            parent,
            index,
            root: node,
            insert: false,
            stash,
            size,
        });
        Ok(())
    }

    /// Moves `node`, with everything under it, to be child `index` of
    /// `parent`, counting `parent`'s children as they are once `node` has
    /// left its place.
    ///
    /// # Errors
    ///
    /// [`EditError::NoSuchNode`], [`EditError::Root`],
    /// [`EditError::IntoItself`], [`EditError::IndexOutOfRange`] or
    /// [`EditError::TooDeep`].
    pub fn move_node(
        &mut self,
        node: NodeId,
        parent: NodeId,
        index: usize,
    ) -> Result<(), EditError> {
        if node == NodeId::ROOT {
            return Err(EditError::Root);
        }
        self.check_node(node)?;
        self.check_node(parent)?;
        let tree = &self.doc.tree;
        if tree.is_within(parent, node) {
            return Err(EditError::IntoItself);
        }
        let from_parent = tree
            .node(node)
            .and_then(|data| data.parent)
            .expect("not the root");
        let from_index = self.doc.index_in_parent(node).expect("has a parent");
        let len =
            tree.node(parent).expect("checked").children.len() - usize::from(from_parent == parent);
        if index > len {
            return Err(EditError::IndexOutOfRange { index, len });
        }
        if tree.depth(parent) + 1 + tree.height(node) > MAX_DEPTH {
            return Err(EditError::TooDeep);
        }
        if (from_parent, from_index) == (parent, index) {
            return Ok(());
        }
        let mut op = Op::Move {
            node,
            from: (from_parent, from_index),
            to: (parent, index),
        };
        self.doc.apply(&mut op, true, Origin::Edit);
        self.ops.push(op);
        Ok(())
    }

    /// Sets the linked parameter `id` to `plain`, in the parameter's own
    /// unit, as an undoable step, and updates its mirror. Returns the value
    /// the parameter took, which may be clamped or snapped.
    ///
    /// # Errors
    ///
    /// [`EditError::NoParamLink`], [`EditError::NoSuchParam`] or
    /// [`EditError::NotFinite`].
    pub fn set_param(&mut self, id: &str, plain: f64) -> Result<f64, EditError> {
        let (params, _) = self.doc.linked().ok_or(EditError::NoParamLink)?;
        let param = params
            .find(id)
            .ok_or_else(|| EditError::NoSuchParam(id.to_owned()))?;
        if !plain.is_finite() {
            return Err(EditError::NotFinite);
        }
        let old = param.plain();
        param.set_plain(plain);
        let new = param.plain();
        self.doc.mirror(id, new, Origin::Edit);
        if old.to_bits() != new.to_bits() {
            self.ops.push(Op::Param {
                id: Arc::from(id),
                old,
                new,
            });
        }
        Ok(new)
    }
}
