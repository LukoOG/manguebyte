//! One line of text being edited: the text, the caret and the selection,
//! what keys, typed characters, input methods and the pointer do to them,
//! and how they are drawn. The text field and the value entry of knobs and
//! sliders are both made of this.

use std::fmt;
use std::ops::Range;
use std::rc::Rc;

use manguebyte_gfx::{Point, Rect, Scene, TextLayout, TextStyle};

use crate::access::selection_color;
use crate::clipboard::Clipboard;
use crate::event::{Composition, Key, KeyEvent};
use crate::theme::Theme;

/// The most characters a text being edited holds unless told otherwise.
pub(crate) const DEFAULT_MAX_CHARS: usize = 256;

/// The most characters of an input method's composition that are kept.
const MAX_PREEDIT: usize = 64;

/// The width of the caret, in logical pixels.
const CARET_WIDTH: f32 = 1.0;

/// Which characters may go into the text.
pub(crate) type Filter = Rc<dyn Fn(char) -> bool>;

/// What an edit made of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// It did not use the event.
    Ignored,
    /// It used the event; the text, the caret or the selection may have
    /// changed and it should be drawn again.
    Used,
    /// Enter: the person wants the text taken.
    Commit,
    /// Escape: the person wants the edit thrown away.
    Cancel,
}

/// How a selection made with the pointer grows while it is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Select {
    /// By character, from where it was pressed.
    Chars,
    /// By word, from the word that was double-clicked.
    Words { start: usize, end: usize },
    /// Everything, after a triple click.
    All,
}

/// The text an input method is composing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Preedit {
    /// Whether a composition is going on.
    active: bool,
    text: String,
    /// The input method's caret, in characters of `text`.
    caret: usize,
}

/// A line of text being edited.
///
/// Indices are byte indices into the text, always on character boundaries.
/// Movement and deletion go by clusters: a character and the combining
/// marks, variation selectors, emoji modifiers and joined characters that
/// follow it, which approximates Unicode's extended grapheme clusters
/// (UAX #29) without its tables. Words are runs of letters and digits, runs
/// of other visible characters, and runs of white space, as Windows text
/// fields move.
pub(crate) struct TextEdit {
    text: String,
    caret: usize,
    anchor: usize,
    max_chars: usize,
    filter: Option<Filter>,
    preedit: Preedit,
    select: Option<Select>,
    /// How far the text is scrolled to the left, in logical pixels, to keep
    /// the caret in sight.
    scroll: f32,
    /// The layout of what was last shown, and where its origin was.
    layout: Option<TextLayout>,
    origin: Point,
}

/// Whether `c` belongs to the cluster of the character before it.
fn extends(c: char) -> bool {
    matches!(
        u32::from(c),
        // Combining diacritical marks, their extension and supplement, for
        // symbols, and half marks.
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
            // Variation selectors and their supplement.
            | 0xFE00..=0xFE0F
            | 0xE0100..=0xE01EF
            // The zero-width joiner, emoji skin-tone modifiers, and tags.
            | 0x200D
            | 0x1F3FB..=0x1F3FF
            | 0xE0020..=0xE007F
    )
}

fn regional_indicator(c: char) -> bool {
    matches!(u32::from(c), 0x1F1E6..=0x1F1FF)
}

/// The kind of character, for words: 0 for white space, 1 for letters and
/// digits, 2 for anything else.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' || extends(c) {
        1
    } else {
        2
    }
}

/// `c` as it may go into one line of text: white space that breaks or
/// tabs as a space, other control characters not at all.
fn single_line(c: char) -> Option<char> {
    match c {
        '\n' | '\r' | '\t' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}' => Some(' '),
        c if c.is_control() => None,
        c => Some(c),
    }
}

impl TextEdit {
    /// An empty edit holding at most `max_chars` characters.
    pub(crate) fn new(max_chars: usize) -> TextEdit {
        TextEdit {
            text: String::new(),
            caret: 0,
            anchor: 0,
            max_chars,
            filter: None,
            preedit: Preedit::default(),
            select: None,
            scroll: 0.0,
            layout: None,
            origin: Point::ZERO,
        }
    }

    /// Only characters `filter` accepts may be typed or pasted.
    pub(crate) fn set_filter(&mut self, filter: Option<Filter>) {
        self.filter = filter;
    }

    pub(crate) fn set_max_chars(&mut self, max_chars: usize) {
        self.max_chars = max_chars;
    }

    /// The text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The selection, as a range of bytes: empty when nothing is selected,
    /// at the caret.
    pub(crate) fn selection(&self) -> Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    /// Where the caret is, in bytes.
    #[cfg(test)]
    fn caret_index(&self) -> usize {
        self.caret
    }

    /// Whether an input method is composing.
    pub(crate) fn is_composing(&self) -> bool {
        self.preedit.active
    }

    /// Replaces the text with `text`, as far as it fits and is allowed on
    /// one line, with everything selected. Ends a composition.
    pub(crate) fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text
            .extend(text.chars().filter_map(single_line).take(self.max_chars));
        self.preedit = Preedit::default();
        self.select = None;
        self.select_all();
    }

    /// Selects everything, the caret at the end.
    pub(crate) fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Puts the caret at the end, selecting nothing.
    pub(crate) fn caret_to_end(&mut self) {
        self.caret = self.text.len();
        self.anchor = self.caret;
    }

    // Boundaries.

    /// The start of the cluster that ends at or contains `index`.
    fn prev_boundary(&self, index: usize) -> usize {
        // Back to a character that surely starts a cluster: one that does
        // not extend the one before it, is not joined to it, and is not a
        // regional indicator after another...
        let mut start = index;
        while let Some(c) = self.text[..start].chars().next_back() {
            start -= c.len_utf8();
            let before = self.text[..start].chars().next_back();
            let pairs = regional_indicator(c) && before.is_some_and(regional_indicator);
            if !extends(c) && before != Some('\u{200D}') && !pairs {
                break;
            }
        }
        // ...then forwards, cluster by cluster, to the last start before
        // `index`.
        let mut at = start;
        loop {
            let next = self.next_boundary(at);
            if next >= index || next == at {
                return at;
            }
            at = next;
        }
    }

    /// The end of the cluster that starts at `index`.
    fn next_boundary(&self, index: usize) -> usize {
        let after = &self.text[index..];
        let mut chars = after.chars();
        let Some(first) = chars.next() else {
            return index;
        };
        let mut end = index + first.len_utf8();
        let mut joined = first == '\u{200D}';
        if regional_indicator(first) {
            let run = self.text[..index]
                .chars()
                .rev()
                .take_while(|&c| regional_indicator(c))
                .count();
            if run % 2 == 0 {
                if let Some(second) = self.text[end..].chars().next() {
                    if regional_indicator(second) {
                        end += second.len_utf8();
                    }
                }
            }
        }
        // Marks, and a character after a zero-width joiner, go with it.
        for c in self.text[end..].chars() {
            if !(joined || extends(c)) {
                break;
            }
            joined = c == '\u{200D}';
            end += c.len_utf8();
        }
        end
    }

    /// The start of the word before `index`, past the white space between.
    fn prev_word(&self, index: usize) -> usize {
        let mut at = index;
        let class_before = |at: usize| self.text[..at].chars().next_back().map(class);
        while class_before(at) == Some(0) {
            at = self.prev_boundary(at);
        }
        if let Some(kind) = class_before(at) {
            while class_before(at) == Some(kind) {
                at = self.prev_boundary(at);
            }
        }
        at
    }

    /// The start of the word after the one at `index`, past the white
    /// space between.
    fn next_word(&self, index: usize) -> usize {
        let mut at = index;
        let class_at = |at: usize| self.text[at..].chars().next().map(class);
        if let Some(kind) = class_at(at).filter(|&kind| kind != 0) {
            while class_at(at) == Some(kind) {
                at = self.next_boundary(at);
            }
        }
        while class_at(at) == Some(0) {
            at = self.next_boundary(at);
        }
        at
    }

    /// The word, or run of white space or of other characters, around
    /// `index`.
    fn word_at(&self, index: usize) -> Range<usize> {
        let kind = self.text[index..]
            .chars()
            .next()
            .or_else(|| self.text[..index].chars().next_back())
            .map(class);
        let Some(kind) = kind else {
            return index..index;
        };
        let mut start = index;
        while self.text[..start].chars().next_back().map(class) == Some(kind) {
            start = self.prev_boundary(start);
        }
        let mut end = index;
        while self.text[end..].chars().next().map(class) == Some(kind) {
            end = self.next_boundary(end);
        }
        start..end
    }

    // Changes.

    fn move_to(&mut self, index: usize, extend: bool) {
        self.caret = index;
        if !extend {
            self.anchor = index;
        }
    }

    /// Removes the selection. Returns whether there was one.
    fn delete_selection(&mut self) -> bool {
        let range = self.selection();
        if range.is_empty() {
            return false;
        }
        // A selection the pointer is making is of the text as it was.
        self.select = None;
        self.text.replace_range(range.clone(), "");
        self.caret = range.start;
        self.anchor = range.start;
        true
    }

    /// Puts `input` in place of the selection, as far as it fits and the
    /// filter allows; white space that breaks lines becomes spaces.
    /// Returns whether the text changed.
    pub(crate) fn insert(&mut self, input: &str) -> bool {
        let deleted = self.delete_selection();
        let room = self.max_chars.saturating_sub(self.text.chars().count());
        let filter = self.filter.clone();
        let accepted: String = input
            .chars()
            .filter_map(single_line)
            .filter(|&c| filter.as_ref().is_none_or(|accept| accept(c)))
            .take(room)
            .collect();
        if accepted.is_empty() {
            return deleted;
        }
        self.select = None;
        self.text.insert_str(self.caret, &accepted);
        self.caret += accepted.len();
        self.anchor = self.caret;
        true
    }

    /// Removes the cluster or word before the caret, or the selection.
    fn delete_back(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        let from = if word {
            self.prev_word(self.caret)
        } else {
            self.prev_boundary(self.caret)
        };
        if from == self.caret {
            return false;
        }
        self.select = None;
        self.text.replace_range(from..self.caret, "");
        self.caret = from;
        self.anchor = from;
        true
    }

    /// Removes the cluster or word after the caret, or the selection.
    fn delete_forward(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        let to = if word {
            self.next_word(self.caret)
        } else {
            self.next_boundary(self.caret)
        };
        if to == self.caret {
            return false;
        }
        self.select = None;
        self.text.replace_range(self.caret..to, "");
        true
    }

    fn copy(&self, clipboard: &dyn Clipboard) {
        let range = self.selection();
        if !range.is_empty() {
            clipboard.write_text(&self.text[range]);
        }
    }

    fn paste(&mut self, clipboard: &dyn Clipboard) {
        if let Some(text) = clipboard.read_text() {
            self.insert(&text);
        }
    }

    /// Handles a key. Releases of the keys a press would use are used too,
    /// so that the host sees neither.
    pub(crate) fn key(&mut self, key: KeyEvent, clipboard: &dyn Clipboard) -> Outcome {
        let m = key.modifiers;
        // Control on Windows and Linux, Command on macOS.
        let command = (m.control && !m.alt) || m.meta;
        // Control, or Option on macOS.
        let word = m.control || m.alt;
        if !key.pressed {
            return match key.key {
                Key::Tab | Key::Function(_) | Key::Other => Outcome::Ignored,
                Key::Character(_) if command => Outcome::Ignored,
                _ => Outcome::Used,
            };
        }
        if self.preedit.active {
            // The input method has the keys while it composes; what it
            // lets through changes nothing here.
            return match key.key {
                Key::Tab | Key::Function(_) | Key::Other => Outcome::Ignored,
                _ => Outcome::Used,
            };
        }
        match key.key {
            Key::Character(c) if command => match c.to_ascii_lowercase() {
                'a' => self.select_all(),
                'c' => self.copy(clipboard),
                'x' => {
                    self.copy(clipboard);
                    self.delete_selection();
                }
                'v' => self.paste(clipboard),
                // Undo and redo are not offered, and are not the host's
                // to do while text is being edited.
                'z' | 'y' => {}
                _ => return Outcome::Ignored,
            },
            // What the key types comes as text.
            Key::Character(_) => {}
            Key::Left | Key::Right => {
                let range = self.selection();
                let to = match (key.key, word) {
                    (Key::Left, true) => self.prev_word(self.caret),
                    (Key::Right, true) => self.next_word(self.caret),
                    // An arrow without Shift leaves a selection at its edge.
                    (Key::Left, false) if !m.shift && !range.is_empty() => range.start,
                    (Key::Right, false) if !m.shift && !range.is_empty() => range.end,
                    (Key::Left, false) => self.prev_boundary(self.caret),
                    _ => self.next_boundary(self.caret),
                };
                self.move_to(to, m.shift);
            }
            Key::Home | Key::Up | Key::PageUp => self.move_to(0, m.shift),
            Key::End | Key::Down | Key::PageDown => self.move_to(self.text.len(), m.shift),
            Key::Backspace => {
                self.delete_back(word);
            }
            Key::Delete if m.shift && !word => {
                self.copy(clipboard);
                self.delete_selection();
            }
            Key::Delete => {
                self.delete_forward(word);
            }
            Key::Insert if m.shift => self.paste(clipboard),
            Key::Insert if m.control => self.copy(clipboard),
            Key::Insert => {}
            Key::Enter => return Outcome::Commit,
            Key::Escape => return Outcome::Cancel,
            _ => return Outcome::Ignored,
        }
        Outcome::Used
    }

    /// Takes a typed character. Returns whether the text changed.
    pub(crate) fn type_char(&mut self, c: char) -> bool {
        if self.preedit.active {
            // What a composition produces arrives before or after its end;
            // either way it is no longer being composed.
            self.preedit = Preedit::default();
        }
        let mut buffer = [0; 4];
        self.insert(c.encode_utf8(&mut buffer))
    }

    /// Follows an input method's composition.
    pub(crate) fn compose(&mut self, step: Composition) {
        match step {
            Composition::Start => {
                self.delete_selection();
                self.preedit = Preedit {
                    active: true,
                    ..Preedit::default()
                };
            }
            Composition::Clear => {
                self.preedit.text.clear();
                self.preedit.caret = 0;
            }
            Composition::Push(c) => {
                if !self.preedit.active {
                    self.delete_selection();
                    self.preedit.active = true;
                }
                if let Some(c) = single_line(c) {
                    if self.preedit.text.chars().count() < MAX_PREEDIT {
                        self.preedit.text.push(c);
                        self.preedit.caret = self.preedit.text.chars().count();
                    }
                }
            }
            Composition::Caret(at) => {
                let count = self.preedit.text.chars().count();
                self.preedit.caret = (at as usize).min(count);
            }
            Composition::End => self.preedit = Preedit::default(),
        }
    }

    // The pointer.

    /// The text shown: the text, with what is being composed at the caret.
    fn shown(&self) -> std::borrow::Cow<'_, str> {
        if self.preedit.text.is_empty() {
            std::borrow::Cow::Borrowed(&self.text)
        } else {
            let mut shown = String::with_capacity(self.text.len() + self.preedit.text.len());
            shown.push_str(&self.text[..self.caret]);
            shown.push_str(&self.preedit.text);
            shown.push_str(&self.text[self.caret..]);
            std::borrow::Cow::Owned(shown)
        }
    }

    /// The layout of what is shown in `style`, made again if it changed.
    fn layout(&mut self, style: &TextStyle) -> &TextLayout {
        let shown = self.shown();
        let stale = self
            .layout
            .as_ref()
            .is_none_or(|layout| layout.text() != shown || layout.style() != style);
        if stale {
            let layout = TextLayout::new(&shown, &style.with_max_lines(1), None);
            self.layout = Some(layout);
        }
        self.layout
            .as_ref()
            .unwrap_or_else(|| unreachable!("made just above"))
    }

    /// The index in the text nearest to `at`, laid out in `style` in
    /// `inner` as it was last drawn.
    fn index_at(&mut self, at: Point, style: &TextStyle) -> usize {
        let origin = self.origin;
        let index = self
            .layout(style)
            .index_at(Point::new(at.x - origin.x, at.y - origin.y));
        // Before anything is composed the shown text is the text.
        if self.text.is_char_boundary(index) {
            index
        } else {
            self.text.len()
        }
    }

    /// A press at `at`: places the caret, extends the selection with
    /// Shift, selects a word on a double click and everything on a triple
    /// one.
    pub(crate) fn press(&mut self, at: Point, clicks: u32, shift: bool, style: &TextStyle) {
        // A press ends what an input method was composing, as it does in
        // the system's own fields.
        self.preedit = Preedit::default();
        let index = self.index_at(at, style);
        match clicks {
            0 | 1 => {
                self.move_to(index, shift);
                self.select = Some(Select::Chars);
            }
            2 => {
                let word = self.word_at(index);
                self.anchor = word.start;
                self.caret = word.end;
                self.select = Some(Select::Words {
                    start: word.start,
                    end: word.end,
                });
            }
            _ => {
                self.select_all();
                self.select = Some(Select::All);
            }
        }
    }

    /// The pointer moved to `at` while pressed: the selection follows it.
    pub(crate) fn drag(&mut self, at: Point, style: &TextStyle) {
        let Some(select) = self.select else {
            return;
        };
        let index = self.index_at(at, style);
        match select {
            Select::Chars => self.caret = index,
            Select::Words { start, end } => {
                let word = self.word_at(index);
                if word.start < start {
                    self.anchor = end;
                    self.caret = word.start;
                } else {
                    self.anchor = start;
                    self.caret = word.end.max(end);
                }
            }
            Select::All => {}
        }
    }

    /// The pointer was let go.
    pub(crate) fn release(&mut self) {
        self.select = None;
    }

    // Drawing.

    /// Draws the text in `inner`, scrolled to keep the caret in sight, with
    /// what is being composed, underlined, and, if `focused`, the selection
    /// and the caret. Everything is clipped to `inner`.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        inner: Rect,
        style: &TextStyle,
        theme: &Theme,
        focused: bool,
    ) {
        let caret_index = self.caret + self.preedit_caret_bytes();
        let preedit = self.caret..self.caret + self.preedit.text.len();
        let selection = self.selection();
        let layout = self.layout(style).clone();
        let line_height = layout.line_height();
        let caret_x = layout.caret(caret_index).map_or(0.0, |caret| caret.x);
        let width = layout.size().width;

        // Keep the caret in sight, and no more empty room on the right
        // than the caret needs.
        let room = (inner.width - CARET_WIDTH).max(0.0);
        let mut scroll = self.scroll;
        if caret_x - scroll > room {
            scroll = caret_x - room;
        }
        if caret_x < scroll {
            scroll = caret_x;
        }
        let most = (width + CARET_WIDTH - inner.width).max(0.0);
        self.scroll = scroll.min(most).max(0.0);
        let origin = Point::new(
            inner.x - self.scroll,
            inner.y + (inner.height - line_height) * 0.5,
        );
        self.origin = origin;

        let x_of = |index: usize| layout.caret(index).map_or(0.0, |caret| caret.x) + origin.x;
        let palette = &theme.palette;
        let selected = selection_color(theme);
        scene.clipped(inner, |scene| {
            if focused && !selection.is_empty() && self.preedit.text.is_empty() {
                let (x0, x1) = (x_of(selection.start), x_of(selection.end));
                scene.fill(
                    Rect::new(x0, origin.y, (x1 - x0).max(0.0), line_height),
                    selected,
                );
            }
            scene.text(&layout, origin);
            if !preedit.is_empty() {
                let (x0, x1) = (x_of(preedit.start), x_of(preedit.end));
                let y = origin.y + layout.baseline() + 2.0;
                scene.fill(Rect::new(x0, y, (x1 - x0).max(0.0), 1.0), palette.text);
            }
            if focused {
                scene.fill(
                    Rect::new(origin.x + caret_x, origin.y, CARET_WIDTH, line_height),
                    palette.text,
                );
            }
        });
    }

    /// Where the input method's caret is in the shown text, past the
    /// caret, in bytes.
    fn preedit_caret_bytes(&self) -> usize {
        self.preedit
            .text
            .char_indices()
            .nth(self.preedit.caret)
            .map_or(self.preedit.text.len(), |(at, _)| at)
    }

    /// Where the caret was last drawn, in logical pixels.
    pub(crate) fn caret_rect(&self) -> Option<Rect> {
        let layout = self.layout.as_ref()?;
        let index = self.caret + self.preedit_caret_bytes();
        let caret = layout.caret(index)?;
        Some(Rect::new(
            self.origin.x + caret.x,
            self.origin.y,
            CARET_WIDTH,
            layout.line_height(),
        ))
    }
}

impl fmt::Debug for TextEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextEdit")
            .field("text", &self.text)
            .field("caret", &self.caret)
            .field("anchor", &self.anchor)
            .field("composing", &self.preedit.text)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MemoryClipboard;
    use crate::event::Modifiers;

    fn edit(text: &str) -> TextEdit {
        let mut edit = TextEdit::new(DEFAULT_MAX_CHARS);
        edit.set_text(text);
        edit.caret_to_end();
        edit
    }

    fn press(key: Key, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            key,
            modifiers,
            pressed: true,
        }
    }

    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    const CONTROL: Modifiers = Modifiers {
        control: true,
        ..Modifiers::NONE
    };
    const SHIFT_CONTROL: Modifiers = Modifiers {
        shift: true,
        control: true,
        ..Modifiers::NONE
    };

    #[test]
    fn clusters_keep_marks_with_their_character() {
        // e and a combining acute; a flag of two regional indicators; a
        // family joined by zero-width joiners; a thumb with a skin tone.
        let text = "e\u{301}x\u{1F1E7}\u{1F1F7}\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{1F44D}\u{1F3FD}";
        let e = edit(text);
        let mut stops = vec![0];
        while let Some(&last) = stops.last() {
            let next = e.next_boundary(last);
            if next == last {
                break;
            }
            stops.push(next);
        }
        let expected = [0, 3, 4, 12, 12 + 4 + 3 + 4 + 3 + 4, text.len()];
        assert_eq!(stops, expected);
        // And back again, the same stops.
        let mut back = vec![text.len()];
        while let Some(&last) = back.last() {
            if last == 0 {
                break;
            }
            back.push(e.prev_boundary(last));
        }
        back.reverse();
        assert_eq!(back, expected);
    }

    #[test]
    fn three_regional_indicators_pair_from_the_start() {
        let text = "\u{1F1E7}\u{1F1F7}\u{1F1E9}";
        let e = edit(text);
        assert_eq!(e.next_boundary(0), 8);
        assert_eq!(e.next_boundary(8), 12);
        assert_eq!(e.prev_boundary(12), 8);
        assert_eq!(e.prev_boundary(8), 0);
    }

    #[test]
    fn words_jump_as_windows_fields_do() {
        let e = edit("gain = -6.0 dB");
        assert_eq!(e.next_word(0), 5);
        assert_eq!(e.next_word(5), 7);
        assert_eq!(e.next_word(7), 8);
        assert_eq!(e.next_word(8), 9);
        assert_eq!(e.next_word(9), 10);
        assert_eq!(e.next_word(12), 14);
        assert_eq!(e.prev_word(14), 12);
        assert_eq!(e.prev_word(12), 10);
        assert_eq!(e.prev_word(5), 0);
        assert_eq!(e.prev_word(0), 0);
        assert_eq!(e.word_at(1), 0..4);
        assert_eq!(e.word_at(4), 4..5);
        assert_eq!(e.word_at(14), 12..14);
    }

    #[test]
    fn keys_move_select_and_delete() {
        let clipboard = MemoryClipboard::new();
        let mut e = edit("one two");
        assert_eq!(e.key(press(Key::Left, CONTROL), &clipboard), Outcome::Used);
        assert_eq!(e.caret_index(), 4);
        e.key(press(Key::End, SHIFT), &clipboard);
        assert_eq!(e.selection(), 4..7);
        e.key(press(Key::Character('x'), CONTROL), &clipboard);
        assert_eq!(
            (e.text(), clipboard.text().as_deref()),
            ("one ", Some("two"))
        );
        e.key(press(Key::Backspace, CONTROL), &clipboard);
        assert_eq!(e.text(), "");
        e.key(press(Key::Character('v'), CONTROL), &clipboard);
        e.key(press(Key::Character('v'), CONTROL), &clipboard);
        assert_eq!(e.text(), "twotwo");
        e.key(press(Key::Home, Modifiers::NONE), &clipboard);
        e.key(press(Key::Right, SHIFT_CONTROL), &clipboard);
        assert_eq!(e.selection(), 0..6);
        // An arrow without Shift leaves the selection at its edge.
        e.key(press(Key::Left, Modifiers::NONE), &clipboard);
        assert_eq!((e.caret_index(), e.selection()), (0, 0..0));
        e.key(press(Key::Delete, Modifiers::NONE), &clipboard);
        assert_eq!(e.text(), "wotwo");
        assert_eq!(
            e.key(press(Key::Enter, Modifiers::NONE), &clipboard),
            Outcome::Commit
        );
        assert_eq!(
            e.key(press(Key::Escape, Modifiers::NONE), &clipboard),
            Outcome::Cancel
        );
        assert_eq!(
            e.key(press(Key::Tab, Modifiers::NONE), &clipboard),
            Outcome::Ignored
        );
        // Control with a key the edit has no use for goes to the host.
        assert_eq!(
            e.key(press(Key::Character('s'), CONTROL), &clipboard),
            Outcome::Ignored
        );
    }

    #[test]
    fn insertion_obeys_the_filter_the_limit_and_one_line() {
        let mut e = TextEdit::new(5);
        e.set_filter(Some(Rc::new(|c: char| c.is_ascii_digit() || c == ' ')));
        assert!(e.insert("1a2\n3\u{7}45678"));
        assert_eq!(e.text(), "12 34");
        assert!(!e.insert("9"), "full");
        e.select_all();
        assert!(e.insert("x"), "the selection went, nothing came in");
        assert_eq!(e.text(), "");
        // AltGr on Windows is Control and Alt: what it types is text.
        let clipboard = MemoryClipboard::new();
        let alt_gr = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            e.key(press(Key::Character('q'), alt_gr), &clipboard),
            Outcome::Used
        );
        assert!(e.type_char('7'));
        assert_eq!(e.text(), "7");
    }

    /// Found by the menus' fuzz test: text typed while the pointer drags a
    /// word selection left the selection's words pointing past the end.
    #[test]
    fn an_edit_ends_the_selection_the_pointer_is_making() {
        let mut e = edit("one two");
        e.select = Some(Select::Words { start: 4, end: 7 });
        e.anchor = 4;
        e.type_char('x');
        assert_eq!(e.text(), "one x");
        assert_eq!(e.select, None, "a drag now moves nothing");
        e.select = Some(Select::Words { start: 0, end: 3 });
        e.key(
            press(Key::Backspace, Modifiers::NONE),
            &MemoryClipboard::new(),
        );
        assert_eq!(e.select, None);
    }

    #[test]
    fn a_composition_is_shown_but_not_typed_until_it_ends() {
        let mut e = edit("ab");
        e.anchor = 0;
        e.compose(Composition::Start);
        assert_eq!(e.text(), "", "the selection went");
        for c in "ni".chars() {
            e.compose(Composition::Push(c));
        }
        e.compose(Composition::Caret(1));
        assert_eq!(e.shown(), "ni");
        assert_eq!(e.preedit_caret_bytes(), 1);
        assert_eq!(e.text(), "");
        e.compose(Composition::Clear);
        e.compose(Composition::Push('你'));
        e.compose(Composition::End);
        assert_eq!(e.shown(), "");
        assert!(e.type_char('你'));
        assert_eq!(e.text(), "你");
        // A composition keeps a bounded number of characters.
        e.compose(Composition::Start);
        for _ in 0..1000 {
            e.compose(Composition::Push('a'));
        }
        assert_eq!(e.preedit.text.len(), MAX_PREEDIT);
        e.compose(Composition::Caret(u32::MAX));
        assert_eq!(e.preedit.caret, MAX_PREEDIT);
    }
}
