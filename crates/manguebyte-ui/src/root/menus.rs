//! Open menus: kept by the root over the tree, laid out above everything
//! else, and given every piece of input while one is open (see
//! [`crate::menu`]).
//!
//! An open menu is a set of ordinary widgets appended to the arena after
//! the interface's own, one panel and its items per level, so they are
//! drawn last, over everything, and found by assistive technology and
//! tests like any other widget. Closing a level truncates the arena back
//! to where it began and repaints what it covered.

use std::cell::Cell;
use std::rc::Rc;

use manguebyte_gfx::{Point, Rect};

use super::Root;
use crate::access::Action;
use crate::event::{Event, Handled, Key, KeyEvent, MenuEvent};
use crate::layout;
use crate::menu::{Menu, below_or_above, beside, menu_view};
use crate::tree::NodeId;

/// How far the pointer moves, in logical pixels, before a release over an
/// item chooses it: a press that opened a menu and was let go where it was
/// chooses nothing.
const DRAG: f32 = 4.0;

/// Where a menu hangs from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Anchor {
    /// Below or above this widget, at least as wide.
    Widget(NodeId),
    /// At this point, as a context menu.
    Point(Point),
}

/// A widget's request to open a menu.
#[derive(Debug, Clone)]
pub(crate) struct MenuRequest {
    pub(crate) menu: Menu,
    pub(crate) anchor: Anchor,
}

/// Where one level hangs from.
#[derive(Debug, Clone, Copy)]
enum Hang {
    /// The menu itself.
    Top(Anchor),
    /// A submenu, beside an item (a node index) of a panel (another).
    Beside { item: usize, panel: usize },
}

/// One open menu: the outermost, or a submenu.
struct Level {
    menu: Menu,
    /// The node of its panel; its items follow it, one node each.
    panel: usize,
    /// For each item with a submenu, whether that is open.
    expanded: Vec<Option<Rc<Cell<bool>>>>,
    hang: Hang,
    /// The item whose submenu is open.
    open: Option<usize>,
}

impl Level {
    fn item_node(&self, item: usize) -> usize {
        self.panel + 1 + item
    }

    fn items(&self) -> usize {
        self.menu.items().len()
    }
}

/// The menus open over a root.
pub(crate) struct Menus {
    /// The widget that opened them, which hears how they ended.
    opener: NodeId,
    /// The widget that had the focus, which gets it back.
    focus_before: Option<NodeId>,
    levels: Vec<Level>,
    /// Where the pointer was when they opened.
    origin: Option<Point>,
    /// A release over an item chooses it: the pointer went down inside a
    /// menu, or has moved since they opened.
    armed: bool,
}

impl Menus {
    /// Where the menus' nodes start: the end of the interface's own.
    pub(crate) fn base(&self) -> usize {
        self.levels.first().map_or(usize::MAX, |level| level.panel)
    }
}

impl Root {
    /// Opens `request` for `opener`, closing any menu open already.
    pub(crate) fn open_menu(&mut self, opener: NodeId, request: MenuRequest) {
        if self.menus.is_some() {
            self.close_menus(MenuEvent::Closed);
        }
        // The menu has the pointer from now on.
        if let Some(holder) = self.capture {
            self.deliver(holder, |widget, cx| widget.event(cx, &Event::PointerCancel));
            self.capture = None;
        }
        let focus_before = self.focus;
        self.menus = Some(Menus {
            opener,
            focus_before,
            levels: Vec::new(),
            origin: self.pointer,
            armed: false,
        });
        self.push_level(request.menu, Hang::Top(request.anchor));
        // The checked item has the focus, or else the first.
        let first = self.menus.as_ref().and_then(|menus| {
            let menu = &menus.levels.first()?.menu;
            let items = menu.items();
            items
                .iter()
                .position(|item| item.is_enabled() && item.is_checked() == Some(true))
                .or_else(|| items.iter().position(|item| item.is_enabled()))
        });
        if let Some(item) = first {
            self.highlight(0, item);
        }
    }

    /// Adds a level showing `menu`, hung from `hang`.
    fn push_level(&mut self, menu: Menu, hang: Hang) {
        let (view, expanded) = menu_view(&menu);
        let panel = self.tree.add(view, None).index();
        if let Some(menus) = &mut self.menus {
            menus.levels.push(Level {
                menu,
                panel,
                expanded,
                hang,
                open: None,
            });
        }
        self.needs_layout = true;
    }

    /// Closes the levels after `level`, repainting what they covered.
    fn close_after(&mut self, level: usize) {
        let Some(menus) = &mut self.menus else {
            return;
        };
        if level + 1 >= menus.levels.len() {
            return;
        }
        let cut = menus.levels[level + 1].panel;
        menus.levels.truncate(level + 1);
        let parent = &mut menus.levels[level];
        if let Some(item) = parent.open.take() {
            if let Some(Some(open)) = parent.expanded.get(item) {
                open.set(false);
            }
            let node = parent.item_node(item);
            if let Some(node) = self.tree.nodes.get_mut(node) {
                node.paint = true;
            }
        }
        self.cut_tree(cut);
    }

    /// Removes the nodes from `cut` on, keeping what they covered as
    /// damage for the next frame, and lets go of what pointed at them.
    fn cut_tree(&mut self, cut: usize) {
        for node in self.tree.nodes.get(cut..).unwrap_or_default() {
            self.removed.extend(node.painted);
        }
        self.tree.truncate(cut);
        if self.focus.is_some_and(|id| id.index() >= cut) {
            // Gone with its node: nothing hears of it.
            self.focus = None;
        }
        if self.capture.is_some_and(|id| id.index() >= cut) {
            self.capture = None;
        }
    }

    /// Closes every menu and tells the widget that opened them `result`.
    pub(crate) fn close_menus(&mut self, result: MenuEvent) {
        let Some(menus) = self.menus.take() else {
            return;
        };
        self.cut_tree(menus.base());
        let back = menus
            .focus_before
            .filter(|id| id.index() < self.tree.nodes.len());
        self.set_focus(back);
        if menus.opener.index() < self.tree.nodes.len() {
            let event = Event::Menu(result);
            self.deliver(menus.opener, |widget, cx| widget.event(cx, &event));
        }
    }

    /// Lays out every open level, after the interface itself.
    pub(crate) fn place_menus(&mut self) {
        let Some(menus) = &self.menus else {
            return;
        };
        let area = self.size;
        let hangs: Vec<(usize, usize, Hang)> = menus
            .levels
            .iter()
            .map(|level| (level.panel, level.items(), level.hang))
            .collect();
        for (panel, items, hang) in hangs {
            let natural = layout::natural_size(&self.tree.layout, panel);
            let rect_of = |index: usize| {
                self.tree
                    .nodes
                    .get(index)
                    .map_or(Rect::default(), |node| node.rect)
            };
            let rect = match hang {
                Hang::Top(Anchor::Widget(id)) => below_or_above(natural, rect_of(id.index()), area),
                Hang::Top(Anchor::Point(at)) => {
                    below_or_above(natural, Rect::new(at.x, at.y, 0.0, 0.0), area)
                }
                Hang::Beside {
                    item,
                    panel: parent,
                } => beside(natural, rect_of(item), rect_of(parent), area),
            };
            let placed = layout::solve(&self.tree.layout, panel, rect, self.scale);
            for index in panel..=panel + items {
                if let Some(placement) = placed.get(index) {
                    self.apply_placement(index, *placement);
                }
            }
        }
    }

    /// The level, and the item if any, at `at`: the innermost level whose
    /// panel holds the point.
    fn menu_at(&self, at: Point) -> Option<(usize, Option<usize>)> {
        let menus = self.menus.as_ref()?;
        for (index, level) in menus.levels.iter().enumerate().rev() {
            let inside = |node: usize| {
                self.tree
                    .nodes
                    .get(node)
                    .is_some_and(|n| n.rect.contains(at))
            };
            if !inside(level.panel) {
                continue;
            }
            let item = (0..level.items()).find(|&item| inside(level.item_node(item)));
            return Some((index, item));
        }
        None
    }

    /// The level and item of the node `id`, if it is an item of an open
    /// menu.
    pub(crate) fn menu_item_of(&self, id: NodeId) -> Option<(usize, usize)> {
        let menus = self.menus.as_ref()?;
        let index = id.index();
        menus.levels.iter().enumerate().find_map(|(at, level)| {
            (index > level.panel && index <= level.panel + level.items())
                .then(|| (at, index - level.panel - 1))
        })
    }

    fn item_enabled(&self, level: usize, item: usize) -> bool {
        self.menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level))
            .and_then(|level| level.menu.items().get(item))
            .is_some_and(|item| item.is_enabled())
    }

    /// Gives the item the focus, which highlights it.
    fn highlight(&mut self, level: usize, item: usize) {
        if !self.item_enabled(level, item) {
            return;
        }
        let node = self
            .menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level))
            .map(|level| level.item_node(item));
        if let Some(node) = node {
            self.set_focus(Some(NodeId::from_index(node)));
        }
    }

    /// Opens the submenu of `item` in `level`, closing any other open from
    /// there, and gives its first item the focus if `focus_first`.
    fn open_submenu(&mut self, level: usize, item: usize, focus_first: bool) {
        let Some(menus) = &self.menus else {
            return;
        };
        let Some(this) = menus.levels.get(level) else {
            return;
        };
        if this.open == Some(item) {
            if focus_first {
                self.focus_first(level + 1);
            }
            return;
        }
        let Some(submenu) = this.menu.items().get(item).and_then(|i| i.menu()).cloned() else {
            return;
        };
        if !self.item_enabled(level, item) {
            return;
        }
        let hang = Hang::Beside {
            item: this.item_node(item),
            panel: this.panel,
        };
        self.close_after(level);
        if let Some(this) = self.menus.as_mut().and_then(|m| m.levels.get_mut(level)) {
            this.open = Some(item);
            if let Some(Some(open)) = this.expanded.get(item) {
                open.set(true);
            }
            let node = this.item_node(item);
            if let Some(node) = self.tree.nodes.get_mut(node) {
                node.paint = true;
            }
        }
        self.push_level(submenu, hang);
        if focus_first {
            self.focus_first(level + 1);
        }
    }

    fn focus_first(&mut self, level: usize) {
        let first = self
            .menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level))
            .and_then(|level| level.menu.items().iter().position(|item| item.is_enabled()));
        if let Some(item) = first {
            self.highlight(level, item);
        }
    }

    /// Chooses `item` of `level`, or opens its submenu.
    fn activate(&mut self, level: usize, item: usize, from_keys: bool) -> bool {
        if !self.item_enabled(level, item) {
            return false;
        }
        let id = self
            .menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level))
            .and_then(|level| level.menu.items().get(item))
            .and_then(|item| item.id());
        match id {
            Some(id) => self.close_menus(MenuEvent::Chosen(id)),
            None => self.open_submenu(level, item, from_keys),
        }
        true
    }

    /// The level the keys work in, and its item with the focus.
    fn key_level(&self) -> (usize, Option<usize>) {
        if let Some((level, item)) = self.focus.and_then(|id| self.menu_item_of(id)) {
            return (level, Some(item));
        }
        let deepest = self
            .menus
            .as_ref()
            .map_or(0, |menus| menus.levels.len().saturating_sub(1));
        (deepest, None)
    }

    /// Moves the focus in `level` to the next item that can be chosen
    /// after `from` (`forward`) or before it, round at the ends; from the
    /// start or the end if `from` is `None`.
    fn step_focus(&mut self, level: usize, from: Option<usize>, forward: bool) {
        let count = self
            .menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level))
            .map_or(0, Level::items);
        if count == 0 {
            return;
        }
        // Moving on in a menu closes the submenu open from it.
        self.close_after(level);
        let start = match (from, forward) {
            (Some(at), _) => at,
            (None, true) => count - 1,
            (None, false) => 0,
        };
        for step in 1..=count {
            let item = if forward {
                (start + step) % count
            } else {
                (start + count * 2 - step) % count
            };
            if self.item_enabled(level, item) {
                self.highlight(level, item);
                return;
            }
        }
    }

    /// Closes `level`, a submenu, and gives the focus back to its item.
    fn close_level(&mut self, level: usize) {
        if level == 0 {
            self.close_menus(MenuEvent::Closed);
            return;
        }
        let parent = self
            .menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level - 1))
            .and_then(|parent| parent.open);
        self.close_after(level - 1);
        if let Some(item) = parent {
            self.highlight(level - 1, item);
        }
    }

    fn menu_key(&mut self, key: KeyEvent) {
        if !key.pressed {
            return;
        }
        let (level, item) = self.key_level();
        match key.key {
            Key::Down => self.step_focus(level, item, true),
            Key::Up => self.step_focus(level, item, false),
            Key::Home | Key::PageUp => self.step_focus(level, None, true),
            Key::End | Key::PageDown => self.step_focus(level, None, false),
            Key::Enter | Key::Character(' ') => {
                if let Some(item) = item {
                    self.activate(level, item, true);
                }
            }
            Key::Right => {
                if let Some(item) = item {
                    self.open_submenu(level, item, true);
                }
            }
            Key::Left if level > 0 => self.close_level(level),
            Key::Escape => self.close_level(level),
            Key::Tab => self.close_menus(MenuEvent::Closed),
            _ => {}
        }
    }

    /// The next item of the level with the focus whose name starts with
    /// `c`, after the one with the focus.
    fn type_ahead(&mut self, c: char) {
        let (level, item) = self.key_level();
        let Some(menu) = self
            .menus
            .as_ref()
            .and_then(|menus| menus.levels.get(level))
            .map(|level| &level.menu)
        else {
            return;
        };
        let count = menu.items().len();
        let wanted: Vec<char> = c.to_lowercase().collect();
        let starts = |label: &str| {
            let first: Vec<char> = label
                .chars()
                .next()
                .map(|first| first.to_lowercase().collect())
                .unwrap_or_default();
            !first.is_empty() && first == wanted
        };
        let start = item.unwrap_or(count.saturating_sub(1));
        let found = (1..=count)
            .map(|step| (start + step) % count.max(1))
            .find(|&at| menu.items()[at].is_enabled() && starts(menu.items()[at].label()));
        if let Some(found) = found {
            self.highlight(level, found);
        }
    }

    /// What an event does while a menu is open: the menus take it all.
    pub(crate) fn menu_event(&mut self, event: Event) -> Handled {
        if let Some(at) = event.position() {
            let origin = self.menus.as_ref().and_then(|menus| menus.origin);
            let moved = origin
                .is_none_or(|from| (at.x - from.x).abs() > DRAG || (at.y - from.y).abs() > DRAG);
            if moved && !matches!(event, Event::Wheel { .. }) {
                if let Some(menus) = &mut self.menus {
                    menus.armed = true;
                }
            }
        }
        match event {
            Event::PointerDown { at, .. } => match self.menu_at(at) {
                Some((level, item)) => {
                    if let Some(menus) = &mut self.menus {
                        menus.armed = true;
                    }
                    if let Some(item) = item {
                        self.highlight(level, item);
                        self.open_submenu(level, item, false);
                    }
                }
                // A press outside every menu closes them, and does nothing
                // else.
                None => self.close_menus(MenuEvent::Closed),
            },
            Event::PointerMove { at, .. } => {
                if let Some((level, Some(item))) = self.menu_at(at) {
                    if self.item_enabled(level, item) {
                        self.highlight(level, item);
                        let open = self
                            .menus
                            .as_ref()
                            .and_then(|menus| menus.levels.get(level))
                            .and_then(|level| level.open);
                        if open != Some(item) {
                            self.close_after(level);
                            self.open_submenu(level, item, false);
                        }
                    }
                }
            }
            Event::PointerUp { at, .. } => {
                let armed = self.menus.as_ref().is_some_and(|menus| menus.armed);
                if let (true, Some((level, Some(item)))) = (armed, self.menu_at(at)) {
                    let choice = self
                        .menus
                        .as_ref()
                        .and_then(|menus| menus.levels.get(level))
                        .and_then(|level| level.menu.items().get(item))
                        .is_some_and(|item| item.id().is_some());
                    if choice {
                        self.activate(level, item, false);
                    }
                }
            }
            Event::Key(key) => {
                // The release of a key whose press the menu used is used
                // too, after the menu has closed if the press closed it.
                if key.pressed && !self.keys_used.contains(&key.key) {
                    if self.keys_used.len() >= super::MAX_KEYS_HELD {
                        self.keys_used.remove(0);
                    }
                    self.keys_used.push(key.key);
                } else if !key.pressed {
                    self.keys_used.retain(|&used| used != key.key);
                }
                self.menu_key(key);
            }
            Event::Text(c) => self.type_ahead(c),
            Event::Focus(false) => {
                self.close_menus(MenuEvent::Closed);
                return self.event(event);
            }
            Event::PointerCancel
            | Event::Wheel { .. }
            | Event::Composition(_)
            | Event::Focus(true)
            | Event::Menu(_) => {}
        }
        Handled::Yes
    }

    /// Carries out `action` on `id` while a menu is open. Returns `None`
    /// when `id` is not in a menu; the menus are closed first then.
    pub(crate) fn menu_act(&mut self, id: NodeId, action: Action) -> Option<bool> {
        let Some((level, item)) = self.menu_item_of(id) else {
            self.close_menus(MenuEvent::Closed);
            return None;
        };
        Some(match action {
            Action::Press => self.activate(level, item, true),
            Action::Focus => {
                self.highlight(level, item);
                self.focus == Some(id)
            }
            _ => false,
        })
    }
}
