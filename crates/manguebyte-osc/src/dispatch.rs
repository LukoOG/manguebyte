//! Finding the handlers an incoming message is meant for.

use crate::pattern::{self, AddressError, BudgetExceeded, Scratch, Shape};

#[derive(Debug, Clone)]
struct Entry {
    address: Box<str>,
    handler: u32,
}

/// A table from addresses to handler numbers.
///
/// A receiver registers each address it answers to, with a number of its
/// own choosing. For every message that arrives, [`dispatch`](Self::dispatch)
/// reports the numbers of all addresses the message's pattern matches. The
/// numbers mean nothing to this crate: an index into a table of parameters
/// and a variant of an enum both work.
///
/// ```
/// use manguebyte_osc::Dispatcher;
///
/// let mut dispatcher = Dispatcher::new();
/// dispatcher.add("/voice/1/gain", 10)?;
/// dispatcher.add("/voice/2/gain", 20)?;
/// dispatcher.add("/voice/2/pan", 21)?;
///
/// let mut reached = Vec::new();
/// dispatcher.dispatch("/voice/*/gain", |handler| reached.push(handler));
/// assert_eq!(reached, [10, 20]);
/// # Ok::<(), manguebyte_osc::AddressError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct Dispatcher {
    /// In the order they were added.
    entries: Vec<Entry>,
    /// Indices into `entries`, ordered by address and then by age, so that
    /// a plain address is found by binary search.
    by_address: Vec<usize>,
}

impl Dispatcher {
    /// The longest address that can be registered, in bytes.
    ///
    /// The limit is what lets [`dispatch`](Self::dispatch) do its work
    /// without allocating.
    pub const MAX_ADDRESS_LEN: usize = 255;

    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `address` for `handler`.
    ///
    /// An address may be registered several times, with the same number or
    /// with different ones, and one number may stand for several addresses.
    ///
    /// # Errors
    ///
    /// An [`AddressError`] if `address` is not a plain, well-formed address
    /// (see [`validate_address`](pattern::validate_address)) or is longer
    /// than [`MAX_ADDRESS_LEN`](Self::MAX_ADDRESS_LEN).
    pub fn add(&mut self, address: &str, handler: u32) -> Result<(), AddressError> {
        pattern::validate_address(address)?;
        if address.len() > Self::MAX_ADDRESS_LEN {
            return Err(AddressError::TooLong {
                len: address.len(),
                max: Self::MAX_ADDRESS_LEN,
            });
        }
        let index = self.entries.len();
        let slot = self
            .by_address
            .partition_point(|&other| self.address_of(other) <= address);
        self.entries.push(Entry {
            address: address.into(),
            handler,
        });
        self.by_address.insert(slot, index);
        Ok(())
    }

    /// How many registrations the table holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The registrations, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> + '_ {
        self.entries
            .iter()
            .map(|entry| (&*entry.address, entry.handler))
    }

    /// Takes out every registration of `address` and returns how many there
    /// were.
    ///
    /// ```
    /// use manguebyte_osc::Dispatcher;
    ///
    /// let mut dispatcher = Dispatcher::new();
    /// dispatcher.add("/voice/1/gain", 10)?;
    /// dispatcher.add("/voice/2/gain", 20)?;
    /// assert_eq!(dispatcher.remove("/voice/1/gain"), 1);
    /// assert_eq!(dispatcher.dispatch("/voice/*/gain", |_| {}), 1);
    /// # Ok::<(), manguebyte_osc::AddressError>(())
    /// ```
    pub fn remove(&mut self, address: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| &*entry.address != address);
        let removed = before - self.entries.len();
        if removed > 0 {
            // The positions of the entries that stay have changed.
            let mut by_address: Vec<usize> = (0..self.entries.len()).collect();
            by_address.sort_by(|&a, &b| {
                self.address_of(a)
                    .cmp(self.address_of(b))
                    .then_with(|| a.cmp(&b))
            });
            self.by_address = by_address;
        }
        removed
    }

    /// Takes out every registration.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.by_address.clear();
    }

    /// Calls `handle` with the number of every registered address that
    /// `pattern` matches, in the order the addresses were added, and
    /// returns how many there were.
    ///
    /// A pattern that is not well formed matches nothing. A plain address
    /// is found by binary search. A pattern with wildcards is tried against
    /// every registered address, which takes time proportional to the
    /// length of the pattern times the total length of the addresses, and
    /// no more than that whatever the pattern contains. Does not allocate.
    ///
    /// That product is large for a long pattern and a large table, and
    /// whoever sends the pattern chooses its length. For a pattern that
    /// comes from the network use
    /// [`dispatch_within`](Self::dispatch_within), which gives up when the
    /// work passes a limit.
    pub fn dispatch(&self, pattern: &str, handle: impl FnMut(u32)) -> usize {
        // More than any pattern and table that fit in memory can use up.
        let mut work = u64::MAX;
        self.dispatch_within(pattern, &mut work, handle)
            .unwrap_or(0)
    }

    /// [`dispatch`](Self::dispatch), doing at most `work` units of matching
    /// work.
    ///
    /// What the call used is taken out of `work`, so one budget can cover
    /// all the messages of a packet. The [`pattern`](crate::pattern) module
    /// says what a unit is. Looking up a plain address costs what comparing
    /// it once does. A pattern with wildcards is read once, at four units
    /// or more for each of its elements; then it costs two units for each
    /// registered address, and what matching takes for the addresses whose
    /// length it could match. Does not allocate.
    ///
    /// ```
    /// use manguebyte_osc::Dispatcher;
    /// use manguebyte_osc::pattern::BudgetExceeded;
    ///
    /// let mut dispatcher = Dispatcher::new();
    /// for voice in 0..16 {
    ///     dispatcher.add(&format!("/voice/{voice}/gain"), voice)?;
    /// }
    ///
    /// let mut work = 10_000;
    /// assert_eq!(dispatcher.dispatch_within("/voice/1?/gain", &mut work, |_| {}), Ok(6));
    /// println!("that took {} units", 10_000 - work);
    ///
    /// // A pattern built to keep the matcher busy runs out of budget.
    /// let hostile = format!("//*{}x", "{,}".repeat(300));
    /// assert_eq!(
    ///     dispatcher.dispatch_within(&hostile, &mut work, |_| {}),
    ///     Err(BudgetExceeded)
    /// );
    /// # Ok::<(), manguebyte_osc::AddressError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`BudgetExceeded`] if `work` ran out, which leaves it at zero.
    /// `handle` may already have been called for some of the addresses, so
    /// a caller that must not act on half a pattern has to gather the
    /// numbers first and act when this has returned.
    pub fn dispatch_within(
        &self,
        pattern: &str,
        work: &mut u64,
        mut handle: impl FnMut(u32),
    ) -> Result<usize, BudgetExceeded> {
        let mut count = 0;
        if !pattern::has_wildcards(pattern) {
            pattern::spend(work, pattern::plain_cost(pattern))?;
            let first = self
                .by_address
                .partition_point(|&index| self.address_of(index) < pattern);
            for &index in self.by_address.get(first..).unwrap_or(&[]) {
                let Some(entry) = self.entries.get(index) else {
                    break;
                };
                if &*entry.address != pattern {
                    break;
                }
                handle(entry.handler);
                count += 1;
            }
            return Ok(count);
        }

        // One pass over the pattern says whether it is well formed and how
        // long an address it matches can be, which settles most addresses
        // without matching them.
        let Some(shape) = Shape::of(pattern, work)? else {
            return Ok(0);
        };
        let mut scratch = Scratch::new();
        for entry in &self.entries {
            pattern::spend(work, 2)?;
            if shape.admits(entry.address.len())
                && scratch.matches(pattern, &entry.address, work)?
            {
                handle(entry.handler);
                count += 1;
            }
        }
        Ok(count)
    }

    fn address_of(&self, index: usize) -> &str {
        self.entries.get(index).map_or("", |entry| &*entry.address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reached(dispatcher: &Dispatcher, pattern: &str) -> Vec<u32> {
        let mut handlers = Vec::new();
        let count = dispatcher.dispatch(pattern, |handler| handlers.push(handler));
        assert_eq!(count, handlers.len());
        handlers
    }

    #[test]
    fn a_plain_address_reaches_exactly_its_own_handlers_in_the_order_added() {
        let mut dispatcher = Dispatcher::new();
        for (address, handler) in [
            ("/b", 1),
            ("/a", 2),
            ("/c", 3),
            ("/a", 4),
            ("/a/b", 5),
            ("/b", 6),
            ("/a", 7),
        ] {
            dispatcher.add(address, handler).unwrap();
        }
        assert_eq!(dispatcher.len(), 7);
        assert_eq!(reached(&dispatcher, "/a"), [2, 4, 7]);
        assert_eq!(reached(&dispatcher, "/b"), [1, 6]);
        assert_eq!(reached(&dispatcher, "/c"), [3]);
        assert_eq!(reached(&dispatcher, "/a/b"), [5]);
        for nothing in ["/d", "/", "", "/a/", "a"] {
            assert!(reached(&dispatcher, nothing).is_empty(), "{nothing}");
        }
        // The same answers come out of the path that tries every address.
        assert_eq!(reached(&dispatcher, "/[a]"), [2, 4, 7]);
        assert_eq!(reached(&dispatcher, "/?"), [1, 2, 3, 4, 6, 7]);
        assert_eq!(reached(&dispatcher, "//b"), [1, 5, 6]);
    }

    #[test]
    fn only_plain_addresses_of_bounded_length_can_be_registered() {
        let mut dispatcher = Dispatcher::new();
        assert_eq!(
            dispatcher.add("/a/*", 0),
            Err(AddressError::InvalidCharacter { ch: '*', at: 3 })
        );
        assert_eq!(dispatcher.add("a", 0), Err(AddressError::NoLeadingSlash));
        assert_eq!(
            dispatcher.add("/a//b", 0),
            Err(AddressError::EmptyPart { at: 2 })
        );
        assert_eq!(
            dispatcher.add("/a/", 0),
            Err(AddressError::EmptyPart { at: 2 })
        );

        let longest = format!("/{}", "x".repeat(Dispatcher::MAX_ADDRESS_LEN - 1));
        assert_eq!(dispatcher.add(&longest, 9), Ok(()));
        assert_eq!(reached(&dispatcher, "/x*"), [9]);
        assert_eq!(
            dispatcher.add(&format!("{longest}x"), 0),
            Err(AddressError::TooLong { len: 256, max: 255 })
        );
        assert!(dispatcher.iter().eq([(longest.as_str(), 9)]));
    }
}
