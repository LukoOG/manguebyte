//! Address patterns.
//!
//! A receiver has addresses such as `/mixer/channel/3/gain`. A sender may
//! write a pattern in place of an address to reach several of them with one
//! message. [`matches()`] decides whether a pattern reaches an address, by
//! the rules of the OSC 1.0 specification plus the `//` wildcard of OSC
//! 1.1:
//!
//! | In a pattern | Matches |
//! |---|---|
//! | `?` | any one character |
//! | `*` | any run of characters, including none |
//! | `[abc]` | one character from the list |
//! | `[a-z]` | one character in the range, ends included |
//! | `[!abc]` | one character that is not in the list |
//! | `{foo,bar}` | one of the strings in the list |
//! | `//` | any number of whole parts, including none |
//! | anything else | itself |
//!
//! None of `?`, `*` and the bracketed forms matches a `/`, so a pattern has
//! to have as many parts as the address, unless it uses `//`.
//!
//! ```
//! use manguebyte_osc::pattern::matches;
//!
//! assert!(matches("/mixer/channel/*/gain", "/mixer/channel/3/gain"));
//! assert!(matches("/voice/[1-4]/{gate,pitch}", "/voice/2/pitch"));
//! assert!(matches("//gain", "/mixer/channel/3/gain"));
//! assert!(!matches("/mixer/*", "/mixer/channel/3/gain"));
//! ```
//!
//! # Details the specification leaves open
//!
//! - Inside brackets, `-` is a range only between two characters, and `!`
//!   negates only as the first character; elsewhere both stand for
//!   themselves. A range written backwards, `[z-a]`, is the same range.
//!   `[]` matches nothing and `[!]` matches any one character.
//! - Inside braces everything up to the next `,` or `}` is taken as it is
//!   written: `{a*,b}` matches the two-character string `a*`, not a pattern.
//!   An empty alternative is allowed, so `/note{,s}` matches `/note` and
//!   `/notes`.
//! - `//` stands for "any number of whole parts", as the descendant step
//!   does in XPath, which is where OSC 1.1 takes it from: `/a//b` matches
//!   `/a/b` and `/a/x/y/b`.
//! - A pattern that is not well formed matches nothing:
//!   [`validate_pattern`] says what is wrong with it.
//! - Text is compared one Unicode character at a time. The specification
//!   only allows ASCII, where that is the same as one byte at a time.
//!
//! # Cost
//!
//! A pattern arrives from the network, so its sender decides how much work
//! matching it is. Two things keep that in hand.
//!
//! The usual way to match `*` is to try one length and back up on failure,
//! which takes time exponential in the number of wildcards for a pattern
//! built to hurt. [`matches()`] instead keeps the set of places in the
//! address that the pattern read so far can have reached, and moves the
//! whole set forward one pattern element at a time. Its time is bounded by
//! the length of the pattern times the length of the address, whatever the
//! pattern contains.
//!
//! That bound is still a product, and a datagram can carry sixty kilobytes
//! of patterns, to be tried against every address a receiver has. So
//! matching can also be given a budget: [`matches_within`] and
//! [`Dispatcher::dispatch_within`](crate::Dispatcher::dispatch_within)
//! count the work they do and give up with [`BudgetExceeded`] when it is
//! more than the caller allows. A receiver that takes patterns from the
//! network should use those, as the [bridge](crate::bridge) does.
//!
//! Work is counted in units that follow where the time goes. Trying one
//! element of a pattern against an address costs four units, and one more
//! for every place in the address that has to be looked at: the stretch
//! from the first to the last place that the pattern read so far can have
//! reached, which is a single place until a `*` or a `//` widens it. A
//! list between brackets or braces costs more the longer it is. An address
//! without wildcards costs eight units, and one more for every eight bytes
//! of its length. The count depends only on the pattern and the address,
//! never on the machine.
//!
//! A pattern as senders write them costs about five units for each of its
//! elements up to the first that does not fit the address, so a handful
//! against an address that begins differently, and under a hundred against
//! one it matches.

use std::fmt;

/// Addresses shorter than this are matched with working memory on the
/// stack. Longer ones borrow it from the heap for the length of the call.
const INLINE_POSITIONS: usize = 256;

/// Matching was given up because it needed more work than it was allowed.
///
/// Returned by [`matches_within`] and
/// [`Dispatcher::dispatch_within`](crate::Dispatcher::dispatch_within).
/// See the [module documentation](self) for what is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded;

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("matching the address pattern needed more work than was allowed")
    }
}

impl std::error::Error for BudgetExceeded {}

/// What it costs to read one element of a pattern that is `len` bytes long
/// and get ready to try it: a fixed amount, and more for a long list or a
/// long run of stars, which take time to read.
fn element_cost(len: usize) -> usize {
    4 + len / 8
}

/// What it costs to compare or look up a pattern that has no wildcards.
pub(crate) fn plain_cost(pattern: &str) -> usize {
    8 + pattern.len() / 8
}

/// Takes `units` of work out of what is left, or reports that there is not
/// that much. A budget that has run out stays at zero.
pub(crate) fn spend(work: &mut u64, units: usize) -> Result<(), BudgetExceeded> {
    match work.checked_sub(units as u64) {
        Some(left) => {
            *work = left;
            Ok(())
        }
        None => {
            *work = 0;
            Err(BudgetExceeded)
        }
    }
}

/// Why a string is not a well-formed address pattern.
///
/// Every `at` is a byte position in the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternError {
    /// The pattern does not begin with `/`.
    NoLeadingSlash,
    /// A part between two slashes is empty where `//` cannot be meant: at
    /// the end of the pattern, or in a run of three slashes.
    EmptyPart {
        /// Where the slash before the empty part is.
        at: usize,
    },
    /// A `[` has no `]` after it.
    UnclosedBracket {
        /// Where the `[` is.
        at: usize,
    },
    /// A `{` has no `}` after it.
    UnclosedBrace {
        /// Where the `{` is.
        at: usize,
    },
    /// A `]` or `}` has nothing to close.
    UnexpectedClose {
        /// The character.
        ch: char,
        /// Where it is.
        at: usize,
    },
    /// A `/` inside brackets or braces, or a `{` inside braces.
    InvalidInGroup {
        /// The character.
        ch: char,
        /// Where it is.
        at: usize,
    },
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLeadingSlash => f.write_str("an address pattern must begin with '/'"),
            Self::EmptyPart { at } => write!(f, "empty part after the '/' at byte {at}"),
            Self::UnclosedBracket { at } => write!(f, "'[' at byte {at} is never closed"),
            Self::UnclosedBrace { at } => write!(f, "'{{' at byte {at} is never closed"),
            Self::UnexpectedClose { ch, at } => {
                write!(f, "'{ch}' at byte {at} has nothing to close")
            }
            Self::InvalidInGroup { ch, at } => {
                write!(
                    f,
                    "'{ch}' at byte {at} is not allowed inside brackets or braces"
                )
            }
        }
    }
}

impl std::error::Error for PatternError {}

/// Why a string is not an address a receiver can register.
///
/// Every `at` is a byte position in the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AddressError {
    /// The address does not begin with `/`.
    NoLeadingSlash,
    /// A part between two slashes, or after the last one, is empty.
    EmptyPart {
        /// Where the slash before the empty part is.
        at: usize,
    },
    /// A character the specification reserves: a space, a control
    /// character, or one of `# * , ? [ ] { }`.
    InvalidCharacter {
        /// The character.
        ch: char,
        /// Where it is.
        at: usize,
    },
    /// The address is longer than the receiver accepts.
    TooLong {
        /// Its length in bytes.
        len: usize,
        /// The longest length accepted.
        max: usize,
    },
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLeadingSlash => f.write_str("an address must begin with '/'"),
            Self::EmptyPart { at } => write!(f, "empty part after the '/' at byte {at}"),
            Self::InvalidCharacter { ch, at } => {
                write!(f, "{ch:?} at byte {at} is not allowed in an address")
            }
            Self::TooLong { len, max } => {
                write!(f, "the address is {len} bytes long; the limit is {max}")
            }
        }
    }
}

impl std::error::Error for AddressError {}

/// One step of a pattern.
#[derive(Debug, Clone, Copy)]
enum Element<'p> {
    /// `/`: the boundary between two parts.
    Slash,
    /// `//`: a boundary, then any number of whole parts.
    Descend,
    /// `?`.
    AnyOne,
    /// One or more `*` in a row, which match the same as one.
    AnyRun,
    /// `[...]`, with the `!` already taken off.
    Class { negated: bool, items: &'p str },
    /// `{...}`: the text between the braces.
    Alternatives(&'p str),
    /// Any other character.
    Literal(char),
}

/// Cuts a pattern into elements, and stops at the first thing that is
/// wrong with it.
struct Elements<'p> {
    pattern: &'p str,
    at: usize,
    failed: bool,
}

impl<'p> Elements<'p> {
    fn new(pattern: &'p str) -> Self {
        Self {
            pattern,
            at: 0,
            failed: false,
        }
    }

    fn parse(&mut self) -> Result<Option<Element<'p>>, PatternError> {
        let at = self.at;
        let rest = self.pattern.get(at..).unwrap_or("");
        if at == 0 && !rest.starts_with('/') {
            return Err(PatternError::NoLeadingSlash);
        }
        let mut chars = rest.chars();
        let Some(first) = chars.next() else {
            return Ok(None);
        };
        let after = chars.as_str();
        let (element, len) = match first {
            '/' => match after.strip_prefix('/') {
                Some(beyond) => {
                    if beyond.is_empty() || beyond.starts_with('/') {
                        return Err(PatternError::EmptyPart { at: at + 1 });
                    }
                    (Element::Descend, 2)
                }
                None => {
                    if after.is_empty() && at != 0 {
                        return Err(PatternError::EmptyPart { at });
                    }
                    (Element::Slash, 1)
                }
            },
            '?' => (Element::AnyOne, 1),
            '*' => {
                let run = rest.len() - rest.trim_start_matches('*').len();
                (Element::AnyRun, run)
            }
            '[' => {
                let close = after
                    .find(']')
                    .ok_or(PatternError::UnclosedBracket { at })?;
                let body = after.get(..close).unwrap_or("");
                if let Some(slash) = body.find('/') {
                    return Err(PatternError::InvalidInGroup {
                        ch: '/',
                        at: at + 1 + slash,
                    });
                }
                let (negated, items) = match body.strip_prefix('!') {
                    Some(items) => (true, items),
                    None => (false, body),
                };
                (Element::Class { negated, items }, close + 2)
            }
            '{' => {
                let close = after.find('}').ok_or(PatternError::UnclosedBrace { at })?;
                let body = after.get(..close).unwrap_or("");
                if let Some((offset, ch)) =
                    body.char_indices().find(|(_, ch)| matches!(ch, '/' | '{'))
                {
                    return Err(PatternError::InvalidInGroup {
                        ch,
                        at: at + 1 + offset,
                    });
                }
                (Element::Alternatives(body), close + 2)
            }
            ']' | '}' => return Err(PatternError::UnexpectedClose { ch: first, at }),
            other => (Element::Literal(other), other.len_utf8()),
        };
        self.at = at + len;
        Ok(Some(element))
    }
}

impl<'p> Iterator for Elements<'p> {
    type Item = Result<Element<'p>, PatternError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.parse() {
            Ok(element) => element.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// Whether the items between a pair of brackets include `ch`.
fn class_includes(items: &str, ch: char) -> bool {
    let mut rest = items.chars();
    while let Some(low) = rest.next() {
        // A range needs a `-` and then a character to end it. A `-` with
        // nothing after it is an ordinary character.
        let mut ahead = rest.clone();
        if ahead.next() == Some('-') {
            if let Some(high) = ahead.next() {
                rest = ahead;
                let (low, high) = if low <= high {
                    (low, high)
                } else {
                    (high, low)
                };
                if (low..=high).contains(&ch) {
                    return true;
                }
                continue;
            }
        }
        if low == ch {
            return true;
        }
    }
    false
}

/// Whether `pattern` matches `address`, by the rules in the
/// [module documentation](self).
///
/// Returns `false` for a pattern that is not well formed. The address is
/// taken as it is; use [`validate_address`] to check one.
///
/// Takes time proportional to the length of the pattern times the length of
/// the address. Does not allocate for an address shorter than 256 bytes.
/// For a pattern that comes from the network, [`matches_within`] puts a
/// limit on the work.
pub fn matches(pattern: &str, address: &str) -> bool {
    // More than any pattern and address that fit in memory can use up.
    let mut work = u64::MAX;
    matches_within(pattern, address, &mut work).unwrap_or(false)
}

/// [`matches()`], doing at most `work` units of work.
///
/// What the call used is taken out of `work`, so one budget can be shared
/// by several calls. The [module documentation](self) says what a unit is.
/// Does not allocate for an address shorter than 256 bytes.
///
/// ```
/// use manguebyte_osc::pattern::{BudgetExceeded, matches_within};
///
/// let mut work = 1_000;
/// assert_eq!(matches_within("/voice/*/gain", "/voice/3/gain", &mut work), Ok(true));
/// assert!(work > 900);
///
/// // A pattern built to be expensive is given up on.
/// let hostile = format!("//*{}x", "{,}".repeat(5_000));
/// assert_eq!(
///     matches_within(&hostile, "/voice/3/gain", &mut work),
///     Err(BudgetExceeded)
/// );
/// ```
///
/// # Errors
///
/// [`BudgetExceeded`] if the answer was not found within `work` units.
/// `work` is zero afterwards.
pub fn matches_within(
    pattern: &str,
    address: &str,
    work: &mut u64,
) -> Result<bool, BudgetExceeded> {
    if !has_wildcards(pattern) {
        // Nothing special in it, so it can only match itself. The one way
        // such a pattern can be malformed is an empty part.
        spend(work, plain_cost(pattern))?;
        return Ok(pattern == address
            && pattern.starts_with('/')
            && (pattern.len() == 1 || !pattern.ends_with('/')));
    }
    Scratch::new().matches(pattern, address, work)
}

/// The matcher's working memory, enough for an address shorter than 256
/// bytes. One of these serves any number of matches, one after another.
pub(crate) struct Scratch {
    reached: [bool; INLINE_POSITIONS],
    next: [bool; INLINE_POSITIONS],
}

impl Scratch {
    pub(crate) fn new() -> Self {
        Self {
            reached: [false; INLINE_POSITIONS],
            next: [false; INLINE_POSITIONS],
        }
    }

    /// [`matches_within`] for a pattern that is known to have wildcards,
    /// using this memory if the address fits in it.
    pub(crate) fn matches(
        &mut self,
        pattern: &str,
        address: &str,
        work: &mut u64,
    ) -> Result<bool, BudgetExceeded> {
        let positions = address.len() + 1;
        match (
            self.reached.get_mut(..positions),
            self.next.get_mut(..positions),
        ) {
            (Some(reached), Some(next)) => advance(pattern, address, reached, next, work),
            _ => {
                let mut reached = vec![false; positions];
                let mut next = vec![false; positions];
                advance(pattern, address, &mut reached, &mut next, work)
            }
        }
    }
}

/// The lengths, in bytes, of the addresses a pattern can match. Working
/// this out takes one pass over the pattern, and lets a receiver skip the
/// addresses that are too short or too long without matching them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shape {
    shortest: usize,
    /// `None` if there is no limit, because of a `*` or a `//`.
    longest: Option<usize>,
}

impl Shape {
    /// Reads the whole pattern, so this also says whether it is well
    /// formed: `None` if it is not.
    pub(crate) fn of(pattern: &str, work: &mut u64) -> Result<Option<Self>, BudgetExceeded> {
        let mut shortest = 0usize;
        let mut longest = Some(0usize);
        let mut elements = Elements::new(pattern);
        let mut read = 0;
        while let Some(element) = elements.next() {
            let Ok(element) = element else {
                return Ok(None);
            };
            spend(work, element_cost(elements.at - read))?;
            read = elements.at;
            let (least, most) = match element {
                Element::Slash => (1, Some(1)),
                Element::Descend => (1, None),
                Element::AnyRun => (0, None),
                // One character, which is one to four bytes.
                Element::AnyOne | Element::Class { .. } => (1, Some(4)),
                Element::Literal(ch) => (ch.len_utf8(), Some(ch.len_utf8())),
                Element::Alternatives(list) => {
                    let lengths = || list.split(',').map(str::len);
                    (lengths().min().unwrap_or(0), lengths().max())
                }
            };
            shortest = shortest.saturating_add(least);
            longest = longest
                .zip(most)
                .map(|(sum, most)| sum.saturating_add(most));
        }
        Ok(Some(Self { shortest, longest }))
    }

    /// Whether the pattern could match an address of `len` bytes.
    pub(crate) fn admits(&self, len: usize) -> bool {
        len >= self.shortest && self.longest.is_none_or(|longest| len <= longest)
    }
}

/// The matcher. `reached[i]` says that the pattern read so far can have
/// consumed exactly the first `i` bytes of the address. Each element of the
/// pattern turns that set into the next one, and the pattern matches if,
/// after its last element, the whole address has been consumed.
///
/// Both slices have one entry per byte of the address, plus one. What they
/// hold on entry does not matter.
///
/// Only the stretch of the address that holds reached places is looked at
/// for each element, and that stretch is what the element costs. For a
/// pattern without `*` or `//` it is a single place.
fn advance<'s>(
    pattern: &str,
    address: &str,
    mut reached: &'s mut [bool],
    mut next: &'s mut [bool],
    work: &mut u64,
) -> Result<bool, BudgetExceeded> {
    let text = address.as_bytes();
    let end = text.len();
    reached.fill(false);
    next.fill(false);
    let Some(start) = reached.first_mut() else {
        return Ok(false);
    };
    *start = true;
    // Every place reached lies in `low..=high`. Outside that stretch
    // `reached` is all false, and `next` is all false everywhere.
    let (mut low, mut high) = (0usize, 0usize);
    let live = |set: &[bool], position: usize| set.get(position) == Some(&true);

    let mut elements = Elements::new(pattern);
    let mut read = 0;
    while let Some(element) = elements.next() {
        let Ok(element) = element else {
            return Ok(false);
        };
        spend(work, element_cost(elements.at - read))?;
        read = elements.at;
        let width = high - low + 1;
        // The stretch the places reached by this element lie in.
        let mut stretch: Option<(usize, usize)> = None;
        let mut reach = |next: &mut [bool], position: usize| {
            if let Some(slot) = next.get_mut(position) {
                *slot = true;
                stretch = Some(match stretch {
                    Some((first, last)) => (first.min(position), last.max(position)),
                    None => (position, position),
                });
            }
        };

        match element {
            Element::AnyRun => {
                // From each place reached, every place up to the end of the
                // part it is in.
                let mut open = false;
                let mut position = low;
                while position <= end {
                    if position <= high {
                        open |= live(reached, position);
                    } else if !open {
                        break;
                    }
                    if open {
                        reach(next, position);
                    }
                    if text.get(position) == Some(&b'/') {
                        open = false;
                    }
                    position += 1;
                }
                spend(work, position - low + 1)?;
            }
            Element::Descend => {
                // From each slash reached, the place after it and after
                // every later slash: the parts in between are skipped whole.
                let mut open = false;
                let mut position = low;
                while position < end && (open || position <= high) {
                    if text.get(position) == Some(&b'/') {
                        open |= position <= high && live(reached, position);
                        if open {
                            reach(next, position + 1);
                        }
                    }
                    position += 1;
                }
                spend(work, position - low + 1)?;
            }
            Element::Alternatives(list) => {
                // What is left of the address after the first place
                // reached. A longer alternative fits nowhere.
                let room = end - low;
                let mut empty_seen = false;
                for alternative in list.split(',') {
                    let len = alternative.len();
                    if len > room || (len == 0 && empty_seen) {
                        // Cannot match, or changes nothing: an empty
                        // alternative written twice is the same as once.
                        spend(work, 1)?;
                        continue;
                    }
                    empty_seen |= len == 0;
                    // Comparing takes longer the longer the alternative is.
                    spend(work, width.saturating_mul(1 + len / 16))?;
                    for position in low..=high {
                        let fits = live(reached, position)
                            && text
                                .get(position..)
                                .is_some_and(|ahead| ahead.starts_with(alternative.as_bytes()));
                        if fits {
                            reach(next, position + len);
                        }
                    }
                }
            }
            Element::Slash | Element::AnyOne | Element::Class { .. } | Element::Literal(_) => {
                spend(work, width)?;
                for position in low..=high {
                    if !live(reached, position) {
                        continue;
                    }
                    // A `*` reaches every byte of a part, and a byte in the
                    // middle of a character is not a place a character
                    // begins: nothing goes on from there.
                    let Some(found) = address.get(position..).and_then(|s| s.chars().next()) else {
                        continue;
                    };
                    let accepted = match element {
                        Element::Slash => found == '/',
                        Element::AnyOne => found != '/',
                        Element::Class { negated, items } => {
                            // Looking a character up takes as long as the
                            // list is.
                            spend(work, items.len())?;
                            found != '/' && class_includes(items, found) != negated
                        }
                        Element::Literal(wanted) => found == wanted,
                        _ => false,
                    };
                    if accepted {
                        reach(next, position + found.len_utf8());
                    }
                }
            }
        }

        if let Some(old) = reached.get_mut(low..=high) {
            old.fill(false);
        }
        let Some(stretch) = stretch else {
            // Nothing can match from here. A later syntax error would give
            // the same answer.
            return Ok(false);
        };
        (low, high) = stretch;
        core::mem::swap(&mut reached, &mut next);
    }
    Ok(live(reached, end))
}

/// Whether `pattern` contains anything that makes it more than a plain
/// address: a wildcard, brackets, braces or `//`.
///
/// A receiver can use this to look a plain address up directly and keep
/// [`matches()`] for the patterns that need it.
pub fn has_wildcards(pattern: &str) -> bool {
    pattern.contains(['?', '*', '[', ']', '{', '}']) || pattern.contains("//")
}

/// Checks that `pattern` is a well-formed address pattern.
///
/// [`matches()`] returns `false` for exactly the patterns this function
/// rejects, whatever the address.
///
/// ```
/// use manguebyte_osc::pattern::{PatternError, validate_pattern};
///
/// assert_eq!(validate_pattern("/voice/[1-4]/gate"), Ok(()));
/// assert_eq!(
///     validate_pattern("/voice/[1-4/gate"),
///     Err(PatternError::UnclosedBracket { at: 7 })
/// );
/// ```
pub fn validate_pattern(pattern: &str) -> Result<(), PatternError> {
    for element in Elements::new(pattern) {
        element?;
    }
    Ok(())
}

/// Checks that `address` is one a receiver can register: it begins with
/// `/`, has no empty part, and uses none of the characters the
/// specification reserves for patterns.
///
/// The specification allows printable ASCII only. Characters outside ASCII
/// are let through here, because senders in the field use them.
///
/// ```
/// use manguebyte_osc::pattern::{AddressError, validate_address};
///
/// assert_eq!(validate_address("/mixer/channel/3/gain"), Ok(()));
/// assert_eq!(
///     validate_address("/mixer/*/gain"),
///     Err(AddressError::InvalidCharacter { ch: '*', at: 7 })
/// );
/// ```
pub fn validate_address(address: &str) -> Result<(), AddressError> {
    if !address.starts_with('/') {
        return Err(AddressError::NoLeadingSlash);
    }
    let mut previous = None;
    for (at, ch) in address.char_indices() {
        match ch {
            '/' if previous == Some('/') => {
                return Err(AddressError::EmptyPart { at: at - 1 });
            }
            ' ' | '#' | '*' | ',' | '?' | '[' | ']' | '{' | '}' => {
                return Err(AddressError::InvalidCharacter { ch, at });
            }
            _ if ch.is_ascii_control() => {
                return Err(AddressError::InvalidCharacter { ch, at });
            }
            _ => {}
        }
        previous = Some(ch);
    }
    if address.len() > 1 && address.ends_with('/') {
        return Err(AddressError::EmptyPart {
            at: address.len() - 1,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_is_cut_into_the_elements_it_is_written_with() {
        let kinds: Vec<String> = Elements::new("/a//b?*[!x-z]{p,q}")
            .map(|element| format!("{:?}", element.unwrap()))
            .collect();
        assert_eq!(
            kinds,
            [
                "Slash",
                "Literal('a')",
                "Descend",
                "Literal('b')",
                "AnyOne",
                "AnyRun",
                "Class { negated: true, items: \"x-z\" }",
                "Alternatives(\"p,q\")",
            ]
        );
    }

    #[test]
    fn brackets_follow_the_rules_for_minus_and_exclamation() {
        assert!(class_includes("abc", 'b'));
        assert!(!class_includes("abc", 'd'));
        assert!(class_includes("a-c", 'b'));
        assert!(class_includes("c-a", 'b'));
        // A minus at either end stands for itself.
        assert!(class_includes("a-", '-'));
        assert!(class_includes("-a", '-'));
        assert!(!class_includes("a-", 'b'));
        // After a range is complete, the next minus has nothing to its
        // left to start another.
        assert!(class_includes("a-c-e", '-'));
        assert!(class_includes("a-c-e", 'e'));
        assert!(!class_includes("a-c-e", 'd'));
        assert!(!class_includes("", 'a'));
    }
}
