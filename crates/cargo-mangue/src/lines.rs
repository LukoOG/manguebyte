//! Reading the output of another program one line at a time, in bounded
//! memory.
//!
//! The tool reads what cargo and the validators print. Nothing promises
//! that a program ends its lines, so a reader that grows its buffer until it
//! sees a line ending can be made to use any amount of memory. This one
//! keeps at most a fixed number of bytes of each line and drops the rest.

use std::io::{self, BufRead};

/// One line of text, without its line ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line<'a> {
    /// The bytes of the line, up to the reader's limit.
    pub bytes: &'a [u8],
    /// Whether the line was longer than the limit, so that `bytes` holds
    /// only its beginning.
    pub truncated: bool,
}

/// Splits a stream into lines, keeping at most `limit` bytes of each.
///
/// ```
/// use cargo_mangue::lines::Lines;
///
/// let mut lines = Lines::new(&b"one\r\ntwo two two\nthree"[..], 5);
/// let first = lines.next_line()?.unwrap();
/// assert_eq!((first.bytes, first.truncated), (&b"one"[..], false));
/// let second = lines.next_line()?.unwrap();
/// assert_eq!((second.bytes, second.truncated), (&b"two t"[..], true));
/// assert_eq!(lines.next_line()?.unwrap().bytes, b"three");
/// assert!(lines.next_line()?.is_none());
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct Lines<R> {
    reader: R,
    limit: usize,
    line: Vec<u8>,
}

impl<R: BufRead> Lines<R> {
    /// Reads lines from `reader`, keeping at most `limit` bytes of each.
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            limit,
            line: Vec::new(),
        }
    }

    /// The next line, or `None` at the end of the stream.
    ///
    /// A line ends at `\n`; a `\r` before it is dropped. The last line
    /// need not end with one. The `\r` is part of the line ending and not
    /// of the line, so it does not count towards the limit: a line of
    /// exactly `limit` bytes is whole whichever way it ends.
    ///
    /// # Errors
    ///
    /// Fails if the stream does.
    pub fn next_line(&mut self) -> io::Result<Option<Line<'_>>> {
        self.line.clear();
        // How many bytes of the line did not fit, and the last byte of the
        // line whether it fitted or not. Together they tell a line that is
        // too long from one whose carriage return is all that is left over.
        let mut beyond: usize = 0;
        let mut last = None;
        let mut read_any = false;
        loop {
            let available = match self.reader.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if available.is_empty() {
                if !read_any {
                    return Ok(None);
                }
                break;
            }
            read_any = true;
            let end = available.iter().position(|&byte| byte == b'\n');
            let piece = &available[..end.unwrap_or(available.len())];
            let kept = piece.len().min(self.limit - self.line.len());
            self.line.extend_from_slice(&piece[..kept]);
            beyond = beyond.saturating_add(piece.len() - kept);
            last = piece.last().copied().or(last);
            let used = end.map_or(available.len(), |end| end + 1);
            self.reader.consume(used);
            if end.is_some() {
                break;
            }
        }
        let ends_with_return = last == Some(b'\r');
        if beyond == 0 && ends_with_return {
            // Everything fitted, so the carriage return is the last byte
            // kept.
            self.line.pop();
        }
        // One byte left over that is the carriage return is no part of the
        // line: every byte of the line itself was kept.
        let truncated = beyond > 1 || (beyond == 1 && !ends_with_return);
        Ok(Some(Line {
            bytes: &self.line,
            truncated,
        }))
    }

    /// How many bytes the line buffer can hold without growing. It never
    /// exceeds the limit by more than the allocator's rounding.
    pub fn buffered_capacity(&self) -> usize {
        self.line.capacity()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read};

    use super::*;

    fn all(text: &[u8], limit: usize) -> Vec<(Vec<u8>, bool)> {
        let mut lines = Lines::new(text, limit);
        let mut found = Vec::new();
        while let Some(line) = lines.next_line().unwrap() {
            found.push((line.bytes.to_vec(), line.truncated));
        }
        found
    }

    #[test]
    fn splits_at_line_endings_of_both_kinds() {
        assert_eq!(
            all(b"a\nbb\r\n\nccc", 16),
            [
                (b"a".to_vec(), false),
                (b"bb".to_vec(), false),
                (b"".to_vec(), false),
                (b"ccc".to_vec(), false),
            ]
        );
        assert_eq!(all(b"", 16), []);
        assert_eq!(all(b"\n", 16), [(b"".to_vec(), false)]);
        assert_eq!(all(b"x\n", 16), [(b"x".to_vec(), false)]);
    }

    #[test]
    fn a_long_line_is_cut_and_the_next_one_is_whole() {
        assert_eq!(
            all(b"0123456789\nok\n", 4),
            [(b"0123".to_vec(), true), (b"ok".to_vec(), false)]
        );
        assert_eq!(all(b"abcd\n", 4), [(b"abcd".to_vec(), false)]);
        assert_eq!(all(b"abc", 0), [(b"".to_vec(), true)]);
    }

    /// Found in review: the carriage return of a line ending was counted
    /// against the limit before it was dropped, so `abcd\r\n` read with a
    /// limit of 4 came back as cut, and `Cargo::build` turns a cut line
    /// into an error.
    #[test]
    fn a_line_exactly_as_long_as_the_limit_is_whole_whichever_way_it_ends() {
        let whole = [(b"abcd".to_vec(), false), (b"ok".to_vec(), false)];
        assert_eq!(all(b"abcd\nok\n", 4), whole);
        assert_eq!(all(b"abcd\r\nok\r\n", 4), whole);
        // The same at the end of the stream, with and without a line feed.
        assert_eq!(all(b"abcd\r\n", 4), whole[..1]);
        assert_eq!(all(b"abcd\r", 4), whole[..1]);
        assert_eq!(all(b"abcd", 4), whole[..1]);
        // An empty line fits in no room at all.
        assert_eq!(
            all(b"\r\n\n", 0),
            [(b"".to_vec(), false), (b"".to_vec(), false)]
        );

        // One byte more is too long, whichever way that line ends. A
        // carriage return that is not the last byte is part of the line.
        let cut = [(b"abcd".to_vec(), true), (b"ok".to_vec(), false)];
        assert_eq!(all(b"abcde\nok\n", 4), cut);
        assert_eq!(all(b"abcde\r\nok\n", 4), cut);
        assert_eq!(all(b"abcd\r\r\nok\n", 4), cut);
        assert_eq!(all(b"abcd\rx\nok\n", 4), cut);
        assert_eq!(
            all(b"abc\re\r\nok\n", 4),
            [(b"abc\r".to_vec(), true), (b"ok".to_vec(), false)]
        );
    }

    #[test]
    fn a_line_ending_split_between_two_pieces_of_the_stream_is_still_one_ending() {
        // Every way of cutting the stream, with the limit below, at and
        // above the length of the lines.
        let text = b"abcd\r\nefgh\r\n\r\nijklm\r\nno";
        for limit in 0..=6 {
            let expected = all(text, limit);
            for capacity in 1..=text.len() + 1 {
                let reader = BufReader::with_capacity(capacity, &text[..]);
                let mut lines = Lines::new(reader, limit);
                let mut found = Vec::new();
                while let Some(line) = lines.next_line().unwrap() {
                    found.push((line.bytes.to_vec(), line.truncated));
                }
                assert_eq!(found, expected, "limit {limit}, {capacity}-byte buffer");
            }
        }
        assert_eq!(
            all(text, 4),
            [
                (b"abcd".to_vec(), false),
                (b"efgh".to_vec(), false),
                (b"".to_vec(), false),
                (b"ijkl".to_vec(), true),
                (b"no".to_vec(), false),
            ]
        );
    }

    /// A stream that never ends its line: `total` bytes of the letter x.
    struct Endless {
        left: usize,
    }

    impl Read for Endless {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(self.left);
            buffer[..count].fill(b'x');
            self.left -= count;
            Ok(count)
        }
    }

    #[test]
    fn memory_stays_bounded_however_long_a_line_is() {
        const LIMIT: usize = 4_096;
        let stream = BufReader::new(Endless { left: 64 << 20 });
        let mut lines = Lines::new(stream, LIMIT);
        let line = lines.next_line().unwrap().unwrap();
        assert_eq!(line.bytes.len(), LIMIT);
        assert!(line.truncated);
        assert!(lines.next_line().unwrap().is_none());
        // Sixty-four megabytes went by, and the buffer holds a few
        // kilobytes.
        assert!(lines.buffered_capacity() <= 2 * LIMIT);
    }

    #[test]
    fn works_whatever_the_size_of_the_pieces_the_stream_arrives_in() {
        let text = b"first line\r\nsecond\n\nfourth is longer than the limit\nlast";
        let expected = all(text, 12);
        for capacity in 1..=text.len() + 1 {
            let reader = BufReader::with_capacity(capacity, &text[..]);
            let mut lines = Lines::new(reader, 12);
            let mut found = Vec::new();
            while let Some(line) = lines.next_line().unwrap() {
                found.push((line.bytes.to_vec(), line.truncated));
            }
            assert_eq!(found, expected, "with a {capacity}-byte buffer");
        }
    }
}
