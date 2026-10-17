//! What earlier scans found, kept by path and fingerprint.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::ScanError;
use super::paths::{Candidate, Fingerprint};
use super::record::{
    fields, format_from_word, format_word, line, plugin_fields, plugin_from_fields,
};
use crate::any::{Format, PluginEntry};

const HEADER: &str = "manguebyte-scan-cache\t1";
/// Largest cache file read.
const MAX_CACHE_BYTES: u64 = 64 << 20;
/// Most entries read from a cache file.
const MAX_ENTRIES: usize = 100_000;
/// Most plugins read for one entry.
const MAX_PLUGINS: usize = 4_096;

/// What scanning one file or bundle came to.
pub(crate) type Outcome = Result<Vec<PluginEntry>, ScanError>;

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    path: PathBuf,
    format: Format,
    fingerprint: Fingerprint,
    outcome: Outcome,
}

/// What earlier scans found: for each file or bundle, the fingerprint it
/// had and what it held, or why it could not be read.
///
/// A file is scanned again when its fingerprint changes, so a plugin that
/// is updated, replaced or repaired is read afresh, and one that has not
/// changed is not loaded again: one that crashed or hung stays reported,
/// without being run, until it changes or is [forgotten](Self::forget).
/// An error of the scanner itself, such as a scanner program that cannot
/// be started, is not kept.
///
/// The cache is text, one record per line, so it can be kept in a file
/// between runs with [`save`](Self::save) and [`load`](Self::load).
///
/// ```
/// use manguebyte_host::scan::ScanCache;
///
/// // A cache file that is missing or damaged is an empty cache.
/// let cache = ScanCache::load("no/such/cache.txt");
/// assert!(cache.is_empty());
/// assert_eq!(ScanCache::from_text("not a cache"), ScanCache::new());
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScanCache {
    entries: Vec<Entry>,
}

impl ScanCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many files and bundles the cache knows about.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache knows about nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets what the cache knows about `path`, so the next scan reads it
    /// again. Returns whether it knew anything.
    pub fn forget(&mut self, path: &Path) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.path != path);
        self.entries.len() != before
    }

    /// Forgets every file and bundle that is no longer there. Returns how
    /// many.
    pub fn forget_missing(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.path.exists());
        before - self.entries.len()
    }

    pub(crate) fn lookup(
        &self,
        candidate: &Candidate,
        fingerprint: &Fingerprint,
    ) -> Option<&Outcome> {
        self.entries
            .iter()
            .find(|entry| {
                entry.path == candidate.path
                    && entry.format == candidate.format
                    && entry.fingerprint == *fingerprint
            })
            .map(|entry| &entry.outcome)
    }

    pub(crate) fn store(
        &mut self,
        candidate: &Candidate,
        fingerprint: Fingerprint,
        outcome: Outcome,
    ) {
        self.entries
            .retain(|entry| !(entry.path == candidate.path && entry.format == candidate.format));
        if matches!(outcome, Err(ScanError::Unreadable(_))) {
            return;
        }
        self.entries.push(Entry {
            path: candidate.path.clone(),
            format: candidate.format,
            fingerprint,
            outcome,
        });
    }

    /// The cache as text.
    pub fn to_text(&self) -> String {
        let mut text = String::from(HEADER);
        text.push('\n');
        for entry in &self.entries {
            let path = entry.path.to_string_lossy();
            let print = entry.fingerprint;
            let (modified, size, files) = (
                print.modified.to_string(),
                print.size.to_string(),
                print.files.to_string(),
            );
            text.push_str(&line(&[
                "file",
                format_word(entry.format),
                &path,
                &modified,
                &size,
                &files,
            ]));
            text.push('\n');
            match &entry.outcome {
                Ok(plugins) => {
                    for plugin in plugins {
                        let mut record = vec!["plugin"];
                        record.extend(plugin_fields(plugin));
                        text.push_str(&line(&record));
                        text.push('\n');
                    }
                }
                Err(error) => {
                    let (kind, detail) = match error {
                        ScanError::Crashed { code, output } => {
                            let code = code.map(|code| code.to_string()).unwrap_or_default();
                            text.push_str(&line(&["failed", "crashed", &code, output]));
                            text.push('\n');
                            continue;
                        }
                        ScanError::TimedOut => ("timed-out", ""),
                        ScanError::Refused(why) => ("refused", why.as_str()),
                        ScanError::Unreadable(why) => ("unreadable", why.as_str()),
                    };
                    text.push_str(&line(&["failed", kind, detail]));
                    text.push('\n');
                }
            }
        }
        text
    }

    /// Reads a cache written by [`to_text`](Self::to_text).
    ///
    /// Text that is not a cache gives an empty one. A record that cannot be
    /// read drops the entry it belongs to, and the rest is kept: at worst a
    /// file is scanned again.
    pub fn from_text(text: &str) -> Self {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Self::new();
        }
        let mut cache = Self::new();
        let mut current: Option<Entry> = None;
        let mut broken = false;
        let finish = |cache: &mut Self, current: Option<Entry>, broken: bool| {
            if let Some(entry) = current.filter(|_| !broken) {
                if cache.entries.len() < MAX_ENTRIES {
                    cache.entries.push(entry);
                }
            }
        };
        for line in lines {
            let Some(record) = fields(line) else {
                broken = true;
                continue;
            };
            match record.first().map(String::as_str) {
                Some("file") => {
                    finish(&mut cache, current.take(), broken);
                    current = read_file_record(&record);
                    broken = current.is_none();
                }
                Some("plugin") => {
                    let Some(entry) = &mut current else {
                        continue;
                    };
                    let plugin = plugin_from_fields(entry.format, &record[1..]);
                    match (&mut entry.outcome, plugin) {
                        (Ok(plugins), Some(plugin)) if plugins.len() < MAX_PLUGINS => {
                            plugins.push(plugin);
                        }
                        _ => broken = true,
                    }
                }
                Some("failed") => {
                    let Some(entry) = &mut current else {
                        continue;
                    };
                    let detail = || record.get(2).cloned().unwrap_or_default();
                    let error = match record.get(1).map(String::as_str) {
                        Some("crashed") => Some(ScanError::Crashed {
                            code: record.get(2).and_then(|code| code.parse().ok()),
                            output: record.get(3).cloned().unwrap_or_default(),
                        }),
                        Some("timed-out") => Some(ScanError::TimedOut),
                        Some("refused") => Some(ScanError::Refused(detail())),
                        _ => None,
                    };
                    match (&entry.outcome, error) {
                        (Ok(plugins), Some(error)) if plugins.is_empty() => {
                            entry.outcome = Err(error);
                        }
                        _ => broken = true,
                    }
                }
                _ => broken = true,
            }
        }
        finish(&mut cache, current, broken);
        cache
    }

    /// Reads a cache file. A file that is missing, too large or not a
    /// cache gives an empty cache.
    pub fn load(path: impl AsRef<Path>) -> Self {
        let Ok(file) = std::fs::File::open(path) else {
            return Self::new();
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_CACHE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_CACHE_BYTES
        {
            return Self::new();
        }
        String::from_utf8(bytes).map_or_else(|_| Self::new(), |text| Self::from_text(&text))
    }

    /// Writes the cache to a file, by way of a file beside it that takes
    /// its place once whole, so a program that stops halfway never leaves
    /// half a cache behind.
    ///
    /// # Errors
    ///
    /// What the file system says.
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let path = path.as_ref();
        let mut partial = path.as_os_str().to_owned();
        partial.push(".partial");
        std::fs::write(&partial, self.to_text())?;
        std::fs::rename(&partial, path)
    }
}

fn read_file_record(record: &[String]) -> Option<Entry> {
    let [_, format, path, modified, size, files] = record else {
        return None;
    };
    Some(Entry {
        path: PathBuf::from(path),
        format: format_from_word(format)?,
        fingerprint: Fingerprint {
            modified: modified.parse().ok()?,
            size: size.parse().ok()?,
            files: files.parse().ok()?,
        },
        outcome: Ok(Vec::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::any::Kind;

    fn sample() -> ScanCache {
        let mut cache = ScanCache::new();
        let print = Fingerprint {
            modified: 1_700_000_000_123_456_789,
            size: 1_234,
            files: 1,
        };
        let entry = |id: &str| PluginEntry {
            format: Format::Clap,
            id: id.into(),
            name: "A\tname".into(),
            vendor: "V".into(),
            version: "1.0".into(),
            kind: Kind::Effect,
            categories: vec!["audio-effect".into(), "stereo".into()],
        };
        let at = |path: &str, format| Candidate {
            path: PathBuf::from(path),
            format,
        };
        cache.store(
            &at("a.clap", Format::Clap),
            print,
            Ok(vec![entry("one"), entry("two")]),
        );
        cache.store(&at("none.clap", Format::Clap), print, Ok(Vec::new()));
        cache.store(
            &at("crash.vst3", Format::Vst3),
            print,
            Err(ScanError::Crashed {
                code: Some(-3),
                output: "it\nbroke".into(),
            }),
        );
        cache.store(
            &at("hang.clap", Format::Clap),
            print,
            Err(ScanError::TimedOut),
        );
        cache.store(
            &at("no.lv2", Format::Lv2),
            print,
            Err(ScanError::Refused("bad turtle".into())),
        );
        // The scanner's own failure is not the file's, and is not kept.
        cache.store(
            &at("lost.clap", Format::Clap),
            print,
            Err(ScanError::Unreadable("no scanner".into())),
        );
        cache
    }

    #[test]
    fn a_cache_reads_back_as_it_was_written() {
        let cache = sample();
        assert_eq!(cache.len(), 5);
        assert_eq!(ScanCache::from_text(&cache.to_text()), cache);
        assert_eq!(
            ScanCache::from_text(&ScanCache::new().to_text()),
            ScanCache::new()
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "long under Miri; the code it reaches runs in the other tests"
    )]
    fn a_damaged_cache_loses_entries_and_nothing_else() {
        let text = sample().to_text();
        for length in 0..text.len() {
            if text.is_char_boundary(length) {
                let cache = ScanCache::from_text(&text[..length]);
                assert!(cache.len() <= 5);
            }
        }
        let bytes = text.as_bytes();
        for index in 0..bytes.len() {
            for &damage in b"\t\n\\x9" {
                let mut copy = bytes.to_vec();
                copy[index] = damage;
                if let Ok(text) = String::from_utf8(copy) {
                    let _ = ScanCache::from_text(&text);
                }
            }
        }
        // A plugin record without an id drops its entry only.
        let damaged = text.replacen("plugin\tone\t", "plugin\t\t", 1);
        assert_eq!(ScanCache::from_text(&damaged).len(), 4);
    }
}
