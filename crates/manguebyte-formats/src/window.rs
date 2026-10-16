//! A stretch of a file held in memory, for the decoders that read frames or
//! pages of varying size from any position.

use std::io::{self, Read, Seek, SeekFrom};

/// How many bytes are asked of the file at a time.
pub(crate) const READ_CHUNK: usize = 32 * 1024;

/// The stretch of the file that frames, or pages, are decoded from.
///
/// It holds the bytes not decoded yet and is refilled as they run out, so
/// its size follows the size of a frame and not the length of the file.
pub(crate) struct Window {
    bytes: Vec<u8>,
    /// Offset in the file of `bytes[0]`.
    offset: u64,
    /// Where in `bytes` the next frame or page starts.
    pub(crate) position: usize,
    /// Whether the file has nothing after the bytes held.
    pub(crate) exhausted: bool,
    /// Length of the file.
    end: u64,
    /// Whether the reader stands right after the bytes held, so that what
    /// is read next continues them. It does not after the window has been
    /// moved, and a read that fails may leave the reader anywhere. The
    /// reader is put back in step when bytes are next needed.
    in_step: bool,
    /// How many bytes are asked of the file at a time, at the least.
    chunk: usize,
}

impl Window {
    /// An empty window at `offset` in a file of `end` bytes.
    pub(crate) fn new(offset: u64, end: u64) -> Self {
        Self::with_chunk(offset, end, READ_CHUNK)
    }

    /// The same, asking the file for `chunk` bytes at a time at the least.
    pub(crate) fn with_chunk(offset: u64, end: u64, chunk: usize) -> Self {
        Self {
            bytes: Vec::new(),
            offset,
            position: 0,
            exhausted: false,
            end,
            in_step: false,
            chunk,
        }
    }

    /// Moves to `offset`. Nothing is read, and nothing is asked of the
    /// reader, until bytes are needed; if the bytes held reach as far as
    /// `offset`, they are kept and none may be needed at all.
    pub(crate) fn move_to(&mut self, offset: u64) {
        let within = offset
            .checked_sub(self.offset)
            .filter(|&within| within <= self.bytes.len() as u64);
        match within {
            Some(within) => self.position = within as usize,
            None => {
                self.bytes.clear();
                self.offset = offset;
                self.position = 0;
                self.exhausted = false;
                self.in_step = false;
            }
        }
    }

    /// Offset in the file of the next frame or page.
    pub(crate) fn file_position(&self) -> u64 {
        self.offset + self.position as u64
    }

    pub(crate) fn available(&self) -> &[u8] {
        &self.bytes[self.position..]
    }

    /// Makes at least `need` bytes available, unless the file ends first.
    pub(crate) fn fill<R: Read + Seek>(&mut self, reader: &mut R, need: usize) -> io::Result<()> {
        if self.exhausted || self.bytes.len() - self.position >= need {
            return Ok(());
        }
        // What has been decoded is dropped, so that the window holds about
        // one frame however long the file is.
        self.bytes.drain(..self.position);
        self.offset += self.position as u64;
        self.position = 0;

        let mut have = self.bytes.len();
        if !self.in_step {
            reader.seek(SeekFrom::Start(self.offset + have as u64))?;
            self.in_step = true;
        }

        // No more room is made than the file has bytes left: a damaged
        // header may speak of frames far larger than the whole file.
        let left = self.end.saturating_sub(self.offset + have as u64);
        let target = need
            .max(self.chunk)
            .min(have.saturating_add(usize::try_from(left).unwrap_or(usize::MAX)));
        self.bytes.reserve_exact(target - have);
        self.bytes.resize(target, 0);
        let mut result = Ok(());
        while have < need.min(target) {
            match reader.read(&mut self.bytes[have..]) {
                Ok(0) => break,
                Ok(count) => have += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    // Where a failed read leaves the reader is not defined.
                    self.in_step = false;
                    result = Err(error);
                    break;
                }
            }
        }
        self.bytes.truncate(have);
        // The end was reached, or a read came back empty before it because
        // the file is shorter than it was when it was opened.
        let reached_end = self.offset + have as u64 >= self.end;
        let came_up_short = result.is_ok() && have < need.min(target);
        self.exhausted = reached_end || came_up_short;
        result
    }
}
