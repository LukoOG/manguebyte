//! The text form of a document, its loading report and schema migrations.

use std::fmt::{self, Write as _};

use crate::tree::MAX_DEPTH;
use crate::value::is_valid_name;
use crate::{Document, EditError, NodeId, NodeTree, ParamRef, Value};

pub(crate) const HEADER: &str = "manguebyte-state";
/// The version of the text and binary forms this build writes.
pub const FORMAT_VERSION: u32 = 1;

/// Why saved state could not be read at all. Smaller problems are skipped
/// and listed in the [`LoadReport`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadError {
    /// The text does not start with the `manguebyte-state` header, or the
    /// bytes with the binary form's magic.
    MissingHeader,
    /// The header is there but no root node follows.
    NoRoot,
    /// A node is opened and never closed, or closed and never opened. Holds
    /// the 1-based line, or the byte offset of the binary form.
    Unbalanced(usize),
    /// The tree is deeper than [`MAX_DEPTH`].
    TooDeep,
    /// The binary form ends in the middle of something.
    Truncated,
    /// A [`Migrations`] step left a tree no edit could have made.
    Migration(EditError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => f.write_str("not a document: header missing"),
            Self::NoRoot => f.write_str("the document has no root node"),
            Self::Unbalanced(at) => write!(f, "unbalanced node at {at}"),
            Self::TooDeep => write!(f, "the tree is deeper than {MAX_DEPTH} levels"),
            Self::Truncated => f.write_str("the document ends too early"),
            Self::Migration(error) => write!(f, "a migration left the document invalid: {error}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Something a load skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The 1-based line of the text form, or the byte offset of the binary
    /// form.
    pub at: usize,
    /// What was skipped and why.
    pub what: String,
}

/// What loading a document did.
///
/// Loading is tolerant, in the spirit of `manguebyte_params::ParamState`:
/// a property with a type this build does not know (written by a newer
/// one), a line it cannot read, or a block it does not recognise is
/// skipped and listed here, and the rest loads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadReport {
    /// The format version the state was written in. Higher than
    /// [`FORMAT_VERSION`] means a newer build wrote it: what this one does
    /// not understand is in `skipped`.
    pub format_version: u32,
    /// The application's schema version the state was written with.
    pub schema_found: u32,
    /// The schema version the document has now, after migrations.
    pub schema: u32,
    /// Whether the schema is newer than the [`Migrations`] the load was
    /// given know: an older application opening a newer one's document,
    /// which it loads as it is.
    pub newer_schema: bool,
    /// What was skipped.
    pub skipped: Vec<Skipped>,
    /// How many nodes carried an id that another node had already taken,
    /// and were given a fresh one. References to them may now point to the
    /// other node.
    pub renumbered: usize,
}

impl LoadReport {
    /// Whether everything was read as written: nothing skipped, nothing
    /// renumbered, and no newer format or schema than this build knows.
    pub fn is_exact(&self) -> bool {
        self.skipped.is_empty()
            && self.renumbered == 0
            && self.format_version <= FORMAT_VERSION
            && !self.newer_schema
    }

    pub(crate) fn skip(&mut self, at: usize, what: impl Into<String>) {
        self.skipped.push(Skipped {
            at,
            what: what.into(),
        });
    }
}

/// One migration step: rewrites a tree of schema `n` into schema `n + 1`.
type Step = Box<dyn Fn(&mut NodeTree) + Send + Sync>;

/// Upgrades documents saved by older versions of an application.
///
/// An application gives its document format a schema version
/// ([`Document::set_schema`]), saved with the document, and registers one
/// step per version change. Loading a document of an older schema runs the
/// steps from its version up to [`current`](Self::current) on the tree
/// before the document is built. A document of a newer schema than the
/// application knows is loaded as it is, and the report says so.
///
/// ```
/// use manguebyte_state::{Document, Migrations, Value};
///
/// // Version 1 called it "volume", in decibels; version 2 "gain", as a factor.
/// let migrations = Migrations::new(2).step(1, |root| {
///     root.walk_mut(&mut |node| {
///         if let Some(db) = node.remove("volume").and_then(|value| value.as_f64()) {
///             node.set("gain", 10f64.powf(db / 20.0));
///         }
///     });
/// });
///
/// let old = "manguebyte-state 1\nschema 1\nnode 0 instrument {\n  volume = float -20\n}\n";
/// let (doc, report) = Document::from_text_with(old, &migrations)?;
/// assert_eq!(doc.schema(), 2);
/// assert_eq!(report.schema_found, 1);
/// let gain = doc.get(doc.root(), "gain").and_then(Value::as_f64).unwrap();
/// assert!((gain - 0.1).abs() < 1e-12);
/// # Ok::<(), manguebyte_state::LoadError>(())
/// ```
pub struct Migrations {
    current: u32,
    steps: Vec<(u32, Step)>,
}

impl fmt::Debug for Migrations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Migrations")
            .field("current", &self.current)
            .field(
                "steps",
                &self.steps.iter().map(|(from, _)| *from).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Migrations {
    /// No steps yet, for an application whose schema is now `current`.
    pub fn new(current: u32) -> Self {
        Self {
            current,
            steps: Vec::new(),
        }
    }

    /// Adds the step that rewrites schema `from` into `from + 1`. A version
    /// with no step needs no change.
    #[must_use]
    pub fn step(mut self, from: u32, step: impl Fn(&mut NodeTree) + Send + Sync + 'static) -> Self {
        self.steps.push((from, Box::new(step)));
        self
    }

    /// The schema version documents are brought up to.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Runs the steps from `found` up, returning the schema reached.
    pub(crate) fn run(&self, tree: &mut NodeTree, found: u32) -> u32 {
        if found >= self.current {
            return found;
        }
        for version in found..self.current {
            for (_, step) in self.steps.iter().filter(|(from, _)| *from == version) {
                step(tree);
            }
        }
        self.current
    }
}

/// What a loader read before the document is built from it.
pub(crate) struct Parsed {
    pub(crate) root: NodeTree,
    pub(crate) next_id: u64,
    pub(crate) schema: u32,
    pub(crate) report: LoadReport,
}

impl Parsed {
    /// Runs the migrations and builds the document.
    pub(crate) fn finish(
        mut self,
        migrations: Option<&Migrations>,
    ) -> Result<(Document, LoadReport), LoadError> {
        self.report.schema_found = self.schema;
        let schema = match migrations {
            Some(migrations) => {
                self.report.newer_schema = self.schema > migrations.current();
                migrations.run(&mut self.root, self.schema)
            }
            None => self.schema,
        };
        self.report.schema = schema;
        // The loaders check names, floats and depth, so only a migration
        // can break them.
        let (doc, renumbered) =
            Document::build(self.root, self.next_id, schema).map_err(LoadError::Migration)?;
        self.report.renumbered = renumbered;
        Ok((doc, self.report))
    }
}

impl Document {
    /// The document as text: a versioned header and one line per node and
    /// per property, readable and fit for version control.
    ///
    /// ```text
    /// manguebyte-state 1
    /// schema 2
    /// next 4
    /// node 0 instrument {
    ///   name = text "Kit"
    ///   node 1 params {
    ///     cutoff = float 1000
    ///   }
    ///   node 3 zone {
    ///     key = int 60
    ///     sample = bytes 52494646
    ///     loop = bool true
    ///     follows = node 1
    ///     modulates = param "cutoff"
    ///   }
    /// }
    /// ```
    ///
    /// Properties are written sorted by name, so the same document is
    /// always the same text. Floats are written in the shortest form that
    /// reads back as exactly the same number.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{HEADER} {FORMAT_VERSION}");
        let _ = writeln!(out, "schema {}", self.schema());
        let _ = writeln!(out, "next {}", self.next_id());
        enum Step {
            Open(NodeId, usize),
            Close(usize),
        }
        let mut stack = vec![Step::Open(NodeId::ROOT, 0)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Open(id, depth) => {
                    let indent = "  ".repeat(depth);
                    let kind = self.kind(id).unwrap_or("node");
                    let _ = writeln!(out, "{indent}node {} {kind} {{", id.get());
                    for (name, value) in self.properties(id) {
                        let _ = write!(out, "{indent}  {name} = ");
                        write_value(&mut out, value);
                        out.push('\n');
                    }
                    stack.push(Step::Close(depth));
                    for child in self.children(id).iter().rev() {
                        stack.push(Step::Open(*child, depth + 1));
                    }
                }
                Step::Close(depth) => {
                    let _ = writeln!(out, "{}}}", "  ".repeat(depth));
                }
            }
        }
        out
    }

    /// Reads a document written by [`to_text`](Self::to_text), by this
    /// version or another, as far as it can; the report says what was
    /// skipped.
    ///
    /// # Errors
    ///
    /// A [`LoadError`] if the text is not a document at all.
    pub fn from_text(text: &str) -> Result<(Self, LoadReport), LoadError> {
        parse_text(text)?.finish(None)
    }

    /// [`from_text`](Self::from_text), bringing an older schema up to date
    /// with `migrations` first.
    ///
    /// # Errors
    ///
    /// As for [`from_text`](Self::from_text).
    pub fn from_text_with(
        text: &str,
        migrations: &Migrations,
    ) -> Result<(Self, LoadReport), LoadError> {
        parse_text(text)?.finish(Some(migrations))
    }
}

fn write_value(out: &mut String, value: &Value) {
    out.push_str(value.type_name());
    match value {
        Value::Bool(on) => {
            let _ = write!(out, " {on}");
        }
        Value::Int(number) => {
            let _ = write!(out, " {number}");
        }
        // `{}` on an `f64` prints the shortest text that reads back as
        // exactly the same number, `-0` included.
        Value::Float(number) => {
            let _ = write!(out, " {number}");
        }
        Value::Text(text) => {
            out.push(' ');
            write_quoted(out, text);
        }
        Value::Bytes(bytes) => {
            if !bytes.is_empty() {
                out.push(' ');
            }
            for byte in bytes.iter() {
                let _ = write!(out, "{byte:02x}");
            }
        }
        Value::Param(param) => {
            out.push(' ');
            write_quoted(out, param.id());
        }
        Value::Node(node) => {
            let _ = write!(out, " {}", node.get());
        }
    }
}

/// Writes `text` in double quotes, escaping what would break a line.
pub(crate) fn write_quoted(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Reads text written by [`write_quoted`], which must be the whole of
/// `quoted`.
pub(crate) fn read_quoted(quoted: &str) -> Option<String> {
    let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let mut text = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return None,
            '\\' => match chars.next()? {
                '"' => text.push('"'),
                '\\' => text.push('\\'),
                'n' => text.push('\n'),
                'r' => text.push('\r'),
                't' => text.push('\t'),
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let mut code = String::new();
                    loop {
                        match chars.next()? {
                            '}' => break,
                            digit => code.push(digit),
                        }
                    }
                    let code = u32::from_str_radix(&code, 16).ok()?;
                    text.push(char::from_u32(code)?);
                }
                _ => return None,
            },
            c => text.push(c),
        }
    }
    Some(text)
}

/// Reads one property's type and payload.
fn read_value(kind: &str, payload: &str) -> Result<Value, String> {
    let bad = || format!("unreadable {kind} {payload:?}");
    match kind {
        "bool" => match payload {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(bad()),
        },
        "int" => payload.parse().map(Value::Int).map_err(|_| bad()),
        "float" => match payload.parse::<f64>() {
            Ok(number) if number.is_finite() => Ok(Value::Float(number)),
            _ => Err(bad()),
        },
        "text" => read_quoted(payload).map(Value::from).ok_or_else(bad),
        "bytes" => {
            let digits = payload.as_bytes();
            if digits.len() % 2 != 0 {
                return Err(bad());
            }
            digits
                .chunks(2)
                .map(|pair| {
                    let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
                    u8::from_str_radix(pair, 16).map_err(|_| bad())
                })
                .collect::<Result<Vec<u8>, String>>()
                .map(Value::from)
        }
        "param" => read_quoted(payload)
            .and_then(|id| ParamRef::new(&id))
            .map(Value::Param)
            .ok_or_else(bad),
        "node" => payload
            .parse()
            .map(|id| Value::Node(NodeId(id)))
            .map_err(|_| bad()),
        other => Err(format!("unknown type {other:?}")),
    }
}

pub(crate) fn parse_text(text: &str) -> Result<Parsed, LoadError> {
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));

    let format_version = lines
        .next()
        .and_then(|(_, line)| line.strip_prefix(HEADER))
        .and_then(|rest| rest.trim().parse::<u32>().ok())
        .ok_or(LoadError::MissingHeader)?;
    let mut report = LoadReport {
        format_version,
        ..LoadReport::default()
    };
    let mut schema = 0;
    let mut next_id = 0;
    let mut stack: Vec<NodeTree> = Vec::new();
    let mut root = None;
    // Lines of a block this build does not recognise, still open.
    let mut skipping = 0usize;

    for (at, line) in lines {
        if skipping > 0 {
            if line == "}" {
                skipping -= 1;
            } else if line.ends_with('{') {
                skipping += 1;
            }
            continue;
        }
        if line == "}" {
            let node = stack.pop().ok_or(LoadError::Unbalanced(at))?;
            match stack.last_mut() {
                Some(parent) => parent.children.push(node),
                None => root = Some(node),
            }
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        if let ["node", id, kind, "{"] = words[..] {
            if root.is_some() {
                report.skip(at, "a second root node");
                skipping = 1;
                continue;
            }
            if !is_valid_name(kind) {
                report.skip(at, format!("a node of invalid kind {kind:?}"));
                skipping = 1;
                continue;
            }
            if stack.len() > MAX_DEPTH {
                return Err(LoadError::TooDeep);
            }
            let mut node = NodeTree::new(kind);
            node.id = match id.parse() {
                Ok(id) => Some(NodeId(id)),
                Err(_) => {
                    report.skip(at, format!("the node id {id:?}"));
                    None
                }
            };
            stack.push(node);
            continue;
        }
        if let Some(node) = stack.last_mut() {
            if words.get(1) == Some(&"=") {
                let name = words[0];
                let rest = line[name.len()..].trim_start()[1..].trim_start();
                let (kind, payload) = rest.split_once(' ').unwrap_or((rest, ""));
                if !is_valid_name(name) {
                    report.skip(at, format!("a property of invalid name {name:?}"));
                    continue;
                }
                match read_value(kind, payload.trim()) {
                    Ok(value) => node.set(name, value),
                    Err(why) => report.skip(at, format!("the property {name:?}: {why}")),
                }
                continue;
            }
        } else if root.is_none() {
            match words[..] {
                ["schema", version] => match version.parse() {
                    Ok(version) => {
                        schema = version;
                        continue;
                    }
                    Err(_) => {
                        report.skip(at, format!("the schema {version:?}"));
                        continue;
                    }
                },
                ["next", next] => match next.parse() {
                    Ok(next) => {
                        next_id = next;
                        continue;
                    }
                    Err(_) => {
                        report.skip(at, format!("the next id {next:?}"));
                        continue;
                    }
                },
                _ => {}
            }
        }
        if line.ends_with('{') {
            report.skip(at, format!("an unknown block {line:?}"));
            skipping = 1;
        } else {
            report.skip(at, format!("an unknown line {line:?}"));
        }
    }
    if !stack.is_empty() {
        return Err(LoadError::Unbalanced(text.lines().count()));
    }
    let root = root.ok_or(LoadError::NoRoot)?;
    Ok(Parsed {
        root,
        next_id,
        schema,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_text_round_trips() {
        for text in [
            "",
            "plain",
            "with \"quotes\" and \\slashes\\",
            "lines\nand\r\ntabs\t",
            "control \u{0} \u{7f} \u{1b}",
            "unicode: maracatu, ação, 日本",
        ] {
            let mut quoted = String::new();
            write_quoted(&mut quoted, text);
            assert!(!quoted.contains('\n'));
            assert_eq!(read_quoted(&quoted).as_deref(), Some(text), "{quoted}");
        }
        for bad in [
            "",
            "\"",
            "x\"\"",
            "\"a\"b\"",
            "\"\\q\"",
            "\"\\u{zz}\"",
            "\"\\u{d800}\"",
        ] {
            assert_eq!(read_quoted(bad), None, "{bad}");
        }
    }

    #[test]
    fn values_read_back_as_written() {
        let values = [
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(i64::MIN),
            Value::Float(-0.0),
            Value::Float(0.1),
            Value::Float(1e300),
            Value::Float(f64::MIN_POSITIVE),
            Value::from("a = b { }"),
            Value::from(vec![0u8, 0xff, 0x10]),
            Value::from(Vec::<u8>::new()),
            Value::Param(ParamRef::new("env.attack").unwrap()),
            Value::Node(NodeId(42)),
        ];
        for value in values {
            let mut out = String::new();
            write_value(&mut out, &value);
            let (kind, payload) = out.split_once(' ').unwrap_or((&out, ""));
            let read = read_value(kind, payload).unwrap();
            assert!(read.same(&value), "{out}");
        }
    }
}
