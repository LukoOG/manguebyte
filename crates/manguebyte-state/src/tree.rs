//! The tree a document holds, and the owned form of a piece of it.

use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

use crate::value::VALUE_SIZE;
use crate::{NodeId, Value};

/// How deep a tree may grow: the root is at depth 0, its children at 1.
///
/// Edits that would go deeper are refused, and so is saved state that does,
/// so that nothing walking a tree can run out of stack.
pub const MAX_DEPTH: usize = 128;

/// One node as the document stores it.
#[derive(Debug, Clone)]
pub(crate) struct NodeData {
    pub(crate) kind: Arc<str>,
    pub(crate) parent: Option<NodeId>,
    /// Sorted by name, so that the order never depends on the order of
    /// edits and undo restores it exactly.
    pub(crate) props: Vec<(Arc<str>, Value)>,
    pub(crate) children: Vec<NodeId>,
}

impl NodeData {
    pub(crate) fn new(kind: Arc<str>, parent: Option<NodeId>) -> Self {
        Self {
            kind,
            parent,
            props: Vec::new(),
            children: Vec::new(),
        }
    }

    fn find(&self, name: &str) -> Result<usize, usize> {
        self.props
            .binary_search_by(|(candidate, _)| (**candidate).cmp(name))
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Value> {
        self.find(name).ok().map(|index| &self.props[index].1)
    }

    /// Roughly how many bytes the node keeps alive, children not included.
    pub(crate) fn size(&self) -> usize {
        mem::size_of::<Self>()
            + self.kind.len()
            + self
                .props
                .iter()
                .map(|(name, value)| VALUE_SIZE + 16 + name.len() + value.heap_size())
                .sum::<usize>()
            + self.children.len() * mem::size_of::<NodeId>()
    }
}

/// Nodes taken out of a tree, the root first, ready to go back in.
pub(crate) type Stash = Vec<(NodeId, NodeData)>;

/// The nodes of a document by id, with the root always present.
#[derive(Debug, Clone)]
pub(crate) struct Tree {
    pub(crate) nodes: HashMap<NodeId, NodeData>,
    pub(crate) next_id: u64,
}

impl Tree {
    pub(crate) fn new(kind: Arc<str>) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(NodeId::ROOT, NodeData::new(kind, None));
        Self { nodes, next_id: 1 }
    }

    pub(crate) fn node(&self, id: NodeId) -> Option<&NodeData> {
        self.nodes.get(&id)
    }

    pub(crate) fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// How far below the root `id` is.
    pub(crate) fn depth(&self, id: NodeId) -> usize {
        let mut depth = 0;
        let mut at = id;
        while let Some(parent) = self.node(at).and_then(|node| node.parent) {
            depth += 1;
            at = parent;
        }
        depth
    }

    /// How many levels there are below `id`: zero for a leaf.
    pub(crate) fn height(&self, id: NodeId) -> usize {
        let mut height = 0;
        let mut level = vec![id];
        loop {
            let next: Vec<NodeId> = level
                .iter()
                .filter_map(|id| self.node(*id))
                .flat_map(|node| node.children.iter().copied())
                .collect();
            if next.is_empty() {
                return height;
            }
            height += 1;
            level = next;
        }
    }

    /// Whether `ancestor` is `id` or above it.
    pub(crate) fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut at = Some(id);
        while let Some(node) = at {
            if node == ancestor {
                return true;
            }
            at = self.node(node).and_then(|data| data.parent);
        }
        false
    }

    /// `id` and every node under it, parents before children.
    pub(crate) fn subtree_ids(&self, id: NodeId) -> Vec<NodeId> {
        let mut ids = Vec::new();
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            ids.push(at);
            if let Some(node) = self.node(at) {
                stack.extend(node.children.iter().rev().copied());
            }
        }
        ids
    }

    /// Roughly how many bytes `id` and everything under it keep alive.
    pub(crate) fn subtree_size(&self, id: NodeId) -> usize {
        self.subtree_ids(id)
            .into_iter()
            .filter_map(|id| self.node(id))
            .map(NodeData::size)
            .sum()
    }

    /// Sets or removes a property, returning what it held. The node must
    /// exist.
    pub(crate) fn set_prop(
        &mut self,
        id: NodeId,
        name: &Arc<str>,
        value: Option<Value>,
    ) -> Option<Value> {
        let node = self.nodes.get_mut(&id).expect("set_prop on a missing node");
        match (node.find(name), value) {
            (Ok(index), Some(value)) => Some(mem::replace(&mut node.props[index].1, value)),
            (Ok(index), None) => Some(node.props.remove(index).1),
            (Err(index), Some(value)) => {
                node.props.insert(index, (Arc::clone(name), value));
                None
            }
            (Err(_), None) => None,
        }
    }

    /// Takes `root` and everything under it out of the tree. Returns its
    /// parent, its place there and the nodes. `root` must not be the root.
    pub(crate) fn detach(&mut self, root: NodeId) -> (NodeId, usize, Stash) {
        let parent = self.nodes[&root].parent.expect("detach the root");
        let siblings = &mut self.nodes.get_mut(&parent).expect("parent").children;
        let index = siblings
            .iter()
            .position(|child| *child == root)
            .expect("child of its parent");
        siblings.remove(index);
        let stash = self
            .subtree_ids(root)
            .into_iter()
            .map(|id| (id, self.nodes.remove(&id).expect("node of the subtree")))
            .collect();
        (parent, index, stash)
    }

    /// Puts nodes taken out by [`detach`](Self::detach) back, the first as
    /// child `index` of `parent`.
    pub(crate) fn attach(&mut self, parent: NodeId, index: usize, stash: Stash) {
        let root = stash
            .first()
            .map(|(id, _)| *id)
            .expect("a stash has a root");
        for (id, mut node) in stash {
            if id == root {
                node.parent = Some(parent);
            }
            self.nodes.insert(id, node);
        }
        self.nodes
            .get_mut(&parent)
            .expect("parent")
            .children
            .insert(index, root);
    }

    /// Moves `node` to be child `index` of `to`, counted once `node` has
    /// left its old place. Returns that old place.
    pub(crate) fn relocate(&mut self, node: NodeId, to: NodeId, index: usize) -> (NodeId, usize) {
        let from = self.nodes[&node].parent.expect("move the root");
        let siblings = &mut self.nodes.get_mut(&from).expect("parent").children;
        let from_index = siblings
            .iter()
            .position(|child| *child == node)
            .expect("child of its parent");
        siblings.remove(from_index);
        self.nodes
            .get_mut(&to)
            .expect("new parent")
            .children
            .insert(index, node);
        self.nodes.get_mut(&node).expect("node").parent = Some(to);
        (from, from_index)
    }

    /// An owned copy of `id` and everything under it.
    pub(crate) fn to_node_tree(&self, id: NodeId) -> Option<NodeTree> {
        let node = self.node(id)?;
        Some(NodeTree {
            id: Some(id),
            kind: node.kind.to_string(),
            props: node
                .props
                .iter()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect(),
            children: node
                .children
                .iter()
                .filter_map(|child| self.to_node_tree(*child))
                .collect(),
        })
    }
}

/// A piece of a document as plain owned data: a node, its properties and
/// everything under it.
///
/// It is what [`Document::subtree`](crate::Document::subtree) copies out,
/// what [`Tx::insert_tree`](crate::Tx::insert_tree) pastes in, what a
/// [`Migrations`](crate::Migrations) step rewrites, and what a document is
/// built from with [`Document::from_tree`](crate::Document::from_tree). It
/// is also a fine immutable snapshot to hand the audio thread: reading it
/// never allocates.
///
/// ```
/// use manguebyte_state::{NodeTree, Value};
///
/// let zone = NodeTree::new("zone")
///     .with("key", 60)
///     .with("sample", "kick.wav")
///     .with_child(NodeTree::new("envelope").with("attack", 0.01));
/// assert_eq!(zone.get("key"), Some(&Value::Int(60)));
/// assert_eq!(zone.children_of_kind("envelope").count(), 1);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct NodeTree {
    /// The node's id, if it has one. `None` asks for a fresh id wherever the
    /// tree is put into a document.
    pub id: Option<NodeId>,
    /// The node's kind, such as `zone`.
    pub kind: String,
    /// The properties, sorted by name when they come from a document.
    pub props: Vec<(String, Value)>,
    /// The children, in order.
    pub children: Vec<NodeTree>,
}

impl NodeTree {
    /// A node of `kind` with no id, properties or children.
    pub fn new(kind: &str) -> Self {
        Self {
            id: None,
            kind: kind.to_owned(),
            props: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Sets a property and returns the node, for building trees.
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.set(name, value);
        self
    }

    /// Appends a child and returns the node, for building trees.
    #[must_use]
    pub fn with_child(mut self, child: NodeTree) -> Self {
        self.children.push(child);
        self
    }

    /// The property `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.props
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value)
    }

    /// Sets the property `name`, replacing any value it had.
    pub fn set(&mut self, name: &str, value: impl Into<Value>) {
        let value = value.into();
        match self
            .props
            .iter_mut()
            .find(|(candidate, _)| candidate == name)
        {
            Some(entry) => entry.1 = value,
            None => self.props.push((name.to_owned(), value)),
        }
    }

    /// Removes the property `name`, returning what it held.
    pub fn remove(&mut self, name: &str) -> Option<Value> {
        let index = self
            .props
            .iter()
            .position(|(candidate, _)| candidate == name)?;
        Some(self.props.remove(index).1)
    }

    /// The children of the given kind, in order.
    pub fn children_of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a NodeTree> {
        self.children.iter().filter(move |child| child.kind == kind)
    }

    /// Calls `visit` on this node and every node under it, parents first.
    pub fn walk_mut(&mut self, visit: &mut dyn FnMut(&mut NodeTree)) {
        visit(self);
        for child in &mut self.children {
            child.walk_mut(visit);
        }
    }

    /// How many levels there are below this node: zero for a leaf.
    pub fn height(&self) -> usize {
        self.children
            .iter()
            .map(|child| child.height() + 1)
            .max()
            .unwrap_or(0)
    }

    /// How many nodes the tree holds, this one included.
    pub fn len(&self) -> usize {
        1 + self.children.iter().map(NodeTree::len).sum::<usize>()
    }

    /// Always `false`: a tree holds at least its own node.
    pub fn is_empty(&self) -> bool {
        false
    }
}
