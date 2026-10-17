//! A Turtle reader: as much of Turtle as LV2 bundles use.
//!
//! Written from the W3C Recommendation *RDF 1.1 Turtle* (2014): prefixes
//! and bases in both spellings (`@prefix` and `PREFIX`), IRIs with relative
//! references resolved by RFC 3986 section 5.2, prefixed names, blank
//! nodes by label and in brackets, collections, and string, number and
//! boolean literals with their escapes, languages and datatypes.
//!
//! A bundle comes from wherever the user found it, so the reader treats
//! its text as hostile:
//!
//! - nothing in it panics, and every error says on which line;
//! - nesting is held to [`MAX_DEPTH`], so the recursion of the grammar has
//!   a fixed bound on its stack;
//! - a document is at most [`MAX_DOCUMENT_BYTES`] long, a graph holds at
//!   most [`MAX_TRIPLES`] triples, one IRI is at most [`MAX_IRI_BYTES`]
//!   long, and every distinct string the graph keeps is counted against
//!   [`MAX_STRING_BYTES`]. A term that appears many times is stored once
//!   and shared, so the memory a graph takes is bounded by those numbers
//!   and not by how often a document repeats itself.
//!
//! What is left out is what an LV2 bundle has no use for: a document's
//! character set is UTF-8 and nothing else, and a graph is a list of
//! triples with an index by subject, not a store that can be queried.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

/// Longest document read, in bytes.
pub(crate) const MAX_DOCUMENT_BYTES: usize = 4 << 20;
/// Most triples one graph holds, over every document read into it.
pub(crate) const MAX_TRIPLES: usize = 200_000;
/// Deepest nesting of brackets and collections.
pub(crate) const MAX_DEPTH: usize = 64;
/// Longest IRI, after prefixes and the base are applied.
pub(crate) const MAX_IRI_BYTES: usize = 2_048;
/// Most bytes of distinct strings one graph keeps.
pub(crate) const MAX_STRING_BYTES: usize = 32 << 20;

pub(crate) const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub(crate) const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// One node of a graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Term {
    /// A resource named by an absolute IRI.
    Iri(Rc<str>),
    /// A blank node, numbered within its graph.
    Blank(u32),
    /// A value.
    Literal(Literal),
}

impl Term {
    /// The IRI, if this is one.
    pub(crate) fn iri(&self) -> Option<&str> {
        match self {
            Self::Iri(iri) => Some(iri),
            _ => None,
        }
    }

    /// The literal, if this is one.
    pub(crate) fn literal(&self) -> Option<&Literal> {
        match self {
            Self::Literal(literal) => Some(literal),
            _ => None,
        }
    }
}

/// A literal: its text, and the datatype or language that says what the
/// text means.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Literal {
    /// The text, escapes resolved.
    pub(crate) value: Rc<str>,
    /// The datatype's IRI, if one was given or the literal is a number or
    /// a boolean.
    pub(crate) datatype: Option<Rc<str>>,
    /// The language tag, in lower case, if one was given.
    pub(crate) language: Option<Rc<str>>,
}

impl Literal {
    /// The literal as a finite number, if it reads as one.
    pub(crate) fn number(&self) -> Option<f64> {
        let value = self.value.trim();
        // Rust reads words such as `inf` and `NaN`, which are not numbers
        // in Turtle or in XML Schema's lexical forms that LV2 uses.
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E'))
        {
            return None;
        }
        value
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
    }

    /// The literal as `true` or `false`, if it is one of them, or a number
    /// that is not zero.
    #[cfg(test)]
    pub(crate) fn boolean(&self) -> Option<bool> {
        match &*self.value {
            "true" => Some(true),
            "false" => Some(false),
            _ => self.number().map(|number| number != 0.0),
        }
    }
}

/// One statement: a subject, a predicate and an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Triple {
    pub(crate) subject: Term,
    pub(crate) predicate: Rc<str>,
    pub(crate) object: Term,
}

/// Why a document could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurtleError {
    /// The line the problem is on, counted from 1.
    pub(crate) line: usize,
    /// What the problem is.
    pub(crate) what: &'static str,
}

impl fmt::Display for TurtleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.what)
    }
}

/// The triples of one or more documents, with an index by subject.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    triples: Vec<Triple>,
    by_subject: HashMap<Term, Vec<u32>>,
    /// Every distinct string the graph keeps, each stored once.
    strings: HashSet<Rc<str>>,
    string_bytes: usize,
    next_blank: u32,
}

impl Graph {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// How many triples the graph holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.triples.len()
    }

    /// Every triple, in the order it was read.
    #[cfg(test)]
    pub(crate) fn triples(&self) -> &[Triple] {
        &self.triples
    }

    /// Reads one document into the graph. `base` is the document's own
    /// IRI, which relative references are resolved against.
    ///
    /// A document with an error adds the triples before the error, and the
    /// error says where it is.
    pub(crate) fn parse(&mut self, text: &str, base: &str) -> Result<(), TurtleError> {
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(TurtleError {
                line: 1,
                what: "the document is too long",
            });
        }
        let mut parser = Parser {
            text,
            pos: 0,
            line: 1,
            base: strip_fragment(base).to_owned(),
            prefixes: HashMap::new(),
            blanks: HashMap::new(),
            graph: self,
            depth: 0,
        };
        // A byte-order mark is allowed at the start, and means nothing.
        if parser.text.starts_with('\u{FEFF}') {
            parser.pos = '\u{FEFF}'.len_utf8();
        }
        parser.document()
    }

    /// The objects of every triple with this subject and predicate, in
    /// the order they were read.
    pub(crate) fn objects<'a, 'p>(
        &'a self,
        subject: &Term,
        predicate: &'p str,
    ) -> impl Iterator<Item = &'a Term> + use<'a, 'p> {
        self.by_subject
            .get(subject)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(move |&index| {
                let triple = self.triples.get(index as usize)?;
                (&*triple.predicate == predicate).then_some(&triple.object)
            })
    }

    /// The first object of this subject and predicate.
    pub(crate) fn object<'a>(&'a self, subject: &Term, predicate: &str) -> Option<&'a Term> {
        self.objects(subject, predicate).next()
    }

    /// The subjects of every triple with this predicate and object, each
    /// once, in the order they were first read.
    pub(crate) fn subjects(&self, predicate: &str, object: &Term) -> Vec<Term> {
        let mut found: Vec<Term> = Vec::new();
        for triple in &self.triples {
            if &*triple.predicate == predicate
                && &triple.object == object
                && !found.contains(&triple.subject)
            {
                found.push(triple.subject.clone());
            }
        }
        found
    }

    /// The term for an IRI, as the graph would store it, for comparing.
    pub(crate) fn iri(text: &str) -> Term {
        Term::Iri(Rc::from(text))
    }

    /// Stores `text` once, and returns the shared copy.
    fn intern(&mut self, text: &str) -> Option<Rc<str>> {
        if let Some(known) = self.strings.get(text) {
            return Some(Rc::clone(known));
        }
        if self.string_bytes + text.len() > MAX_STRING_BYTES {
            return None;
        }
        self.string_bytes += text.len();
        let shared: Rc<str> = Rc::from(text);
        self.strings.insert(Rc::clone(&shared));
        Some(shared)
    }

    fn fresh_blank(&mut self) -> Option<u32> {
        let blank = self.next_blank;
        self.next_blank = self.next_blank.checked_add(1)?;
        Some(blank)
    }

    fn add(&mut self, triple: Triple) -> bool {
        if self.triples.len() >= MAX_TRIPLES {
            return false;
        }
        let index = self.triples.len() as u32;
        self.by_subject
            .entry(triple.subject.clone())
            .or_default()
            .push(index);
        self.triples.push(triple);
        true
    }
}

// --- The parser ---------------------------------------------------------------

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    line: usize,
    /// The base IRI, without a fragment.
    base: String,
    prefixes: HashMap<String, Rc<str>>,
    /// Blank node labels of this document, which are its own.
    blanks: HashMap<String, u32>,
    graph: &'a mut Graph,
    depth: usize,
}

type Parsed<T> = Result<T, TurtleError>;

impl Parser<'_> {
    fn fail<T>(&self, what: &'static str) -> Parsed<T> {
        Err(TurtleError {
            line: self.line,
            what,
        })
    }

    fn rest(&self) -> &str {
        self.text.get(self.pos..).unwrap_or_default()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, skip: usize) -> Option<char> {
        self.rest().chars().nth(skip)
    }

    fn bump(&mut self) -> Option<char> {
        let next = self.peek()?;
        self.pos += next.len_utf8();
        if next == '\n' {
            self.line += 1;
        }
        Some(next)
    }

    fn eat(&mut self, wanted: char) -> bool {
        if self.peek() == Some(wanted) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, wanted: char, what: &'static str) -> Parsed<()> {
        self.skip_space();
        if self.eat(wanted) {
            Ok(())
        } else {
            self.fail(what)
        }
    }

    /// Skips white space and comments.
    fn skip_space(&mut self) {
        while let Some(next) = self.peek() {
            match next {
                ' ' | '\t' | '\r' | '\n' => {
                    self.bump();
                }
                '#' => {
                    while let Some(next) = self.bump() {
                        if next == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    /// Whether the text goes on with `word`, in any case, followed by
    /// something that cannot continue a name.
    fn at_keyword(&self, word: &str) -> bool {
        let rest = self.rest();
        let Some(head) = rest.get(..word.len()) else {
            return false;
        };
        head.eq_ignore_ascii_case(word)
            && !rest[word.len()..]
                .chars()
                .next()
                .is_some_and(|next| is_name_char(next) || next == ':')
    }

    fn enter(&mut self) -> Parsed<()> {
        if self.depth >= MAX_DEPTH {
            return self.fail("brackets or collections nested too deeply");
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn emit(&mut self, subject: &Term, predicate: &Rc<str>, object: Term) -> Parsed<()> {
        let triple = Triple {
            subject: subject.clone(),
            predicate: Rc::clone(predicate),
            object,
        };
        if self.graph.add(triple) {
            Ok(())
        } else {
            self.fail("too many triples")
        }
    }

    fn intern(&mut self, text: &str) -> Parsed<Rc<str>> {
        match self.graph.intern(text) {
            Some(shared) => Ok(shared),
            None => self.fail("too much text"),
        }
    }

    fn intern_iri(&mut self, iri: &str) -> Parsed<Term> {
        if iri.len() > MAX_IRI_BYTES {
            return self.fail("an IRI is too long");
        }
        Ok(Term::Iri(self.intern(iri)?))
    }

    fn fresh_blank(&mut self) -> Parsed<Term> {
        match self.graph.fresh_blank() {
            Some(blank) => Ok(Term::Blank(blank)),
            None => self.fail("too many blank nodes"),
        }
    }

    // --- Statements ----------------------------------------------------------

    fn document(&mut self) -> Parsed<()> {
        loop {
            self.skip_space();
            if self.peek().is_none() {
                return Ok(());
            }
            self.statement()?;
        }
    }

    fn statement(&mut self) -> Parsed<()> {
        if self.peek() == Some('@') {
            self.bump();
            if self.at_keyword("prefix") {
                self.pos += "prefix".len();
                self.prefix_directive()?;
            } else if self.at_keyword("base") {
                self.pos += "base".len();
                self.base_directive()?;
            } else {
                return self.fail("an unknown directive");
            }
            return self.expect('.', "a directive must end with '.'");
        }
        if self.at_keyword("PREFIX") {
            self.pos += "PREFIX".len();
            return self.prefix_directive();
        }
        if self.at_keyword("BASE") {
            self.pos += "BASE".len();
            return self.base_directive();
        }
        self.triples()?;
        self.expect('.', "a statement must end with '.'")
    }

    fn prefix_directive(&mut self) -> Parsed<()> {
        self.skip_space();
        let start = self.pos;
        while self.peek().is_some_and(is_name_char) {
            self.bump();
        }
        let name = self.text[start..self.pos].to_owned();
        if name.ends_with('.') || name.starts_with(['.', '-']) {
            return self.fail("a prefix name is malformed");
        }
        if !self.eat(':') {
            return self.fail("a prefix name must end with ':'");
        }
        self.skip_space();
        let Term::Iri(namespace) = self.iri_ref()? else {
            return self.fail("a prefix must stand for an IRI");
        };
        self.prefixes.insert(name, namespace);
        Ok(())
    }

    fn base_directive(&mut self) -> Parsed<()> {
        self.skip_space();
        let Term::Iri(base) = self.iri_ref()? else {
            return self.fail("a base must be an IRI");
        };
        self.base = strip_fragment(&base).to_owned();
        Ok(())
    }

    fn triples(&mut self) -> Parsed<()> {
        self.skip_space();
        if self.peek() == Some('[') {
            let subject = self.blank_property_list()?;
            self.skip_space();
            // `[ ... ] .` says something about a blank node and nothing
            // more; otherwise more is said about it.
            if self.peek() != Some('.') {
                self.predicate_object_list(&subject)?;
            }
            return Ok(());
        }
        let subject = match self.peek() {
            Some('<') => self.iri_ref()?,
            Some('_') => self.blank_label()?,
            Some('(') => self.collection()?,
            Some(_) => self.prefixed_name()?,
            None => return self.fail("a statement is missing"),
        };
        self.predicate_object_list(&subject)
    }

    fn predicate_object_list(&mut self, subject: &Term) -> Parsed<()> {
        loop {
            self.skip_space();
            let predicate = self.verb()?;
            loop {
                self.skip_space();
                let object = self.object()?;
                self.emit(subject, &predicate, object)?;
                self.skip_space();
                if !self.eat(',') {
                    break;
                }
            }
            self.skip_space();
            if !self.eat(';') {
                return Ok(());
            }
            // Any number of ';' may follow, and the list may end after one.
            loop {
                self.skip_space();
                if !self.eat(';') {
                    break;
                }
            }
            if matches!(self.peek(), Some('.' | ']') | None) {
                return Ok(());
            }
        }
    }

    fn verb(&mut self) -> Parsed<Rc<str>> {
        if self.peek() == Some('a')
            && !self
                .peek_at(1)
                .is_some_and(|next| is_name_char(next) || next == ':')
        {
            self.bump();
            return self.intern(RDF_TYPE);
        }
        let term = match self.peek() {
            Some('<') => self.iri_ref()?,
            _ => self.prefixed_name()?,
        };
        match term {
            Term::Iri(iri) => Ok(iri),
            _ => self.fail("a predicate must be an IRI"),
        }
    }

    fn object(&mut self) -> Parsed<Term> {
        match self.peek() {
            Some('<') => self.iri_ref(),
            Some('_') => self.blank_label(),
            Some('[') => self.blank_property_list(),
            Some('(') => self.collection(),
            Some('"' | '\'') => self.string_literal(),
            Some(next) if next.is_ascii_digit() || matches!(next, '+' | '-' | '.') => self.number(),
            Some(_) if self.at_keyword("true") || self.at_keyword("false") => self.boolean(),
            Some(_) => self.prefixed_name(),
            None => self.fail("an object is missing"),
        }
    }

    fn blank_property_list(&mut self) -> Parsed<Term> {
        self.enter()?;
        self.bump();
        let node = self.fresh_blank()?;
        self.skip_space();
        if !self.eat(']') {
            self.predicate_object_list(&node)?;
            self.expect(']', "a '[' is not closed")?;
        }
        self.leave();
        Ok(node)
    }

    fn collection(&mut self) -> Parsed<Term> {
        self.enter()?;
        self.bump();
        let first = self.intern(RDF_FIRST)?;
        let rest = self.intern(RDF_REST)?;
        let nil = self.intern_iri(RDF_NIL)?;
        let mut head = None;
        let mut last: Option<Term> = None;
        loop {
            self.skip_space();
            if self.eat(')') {
                break;
            }
            if self.peek().is_none() {
                return self.fail("a '(' is not closed");
            }
            let item = self.object()?;
            let node = self.fresh_blank()?;
            self.emit(&node, &first, item)?;
            match &last {
                Some(previous) => self.emit(previous, &rest, node.clone())?,
                None => head = Some(node.clone()),
            }
            last = Some(node);
        }
        if let Some(last) = &last {
            self.emit(last, &rest, nil.clone())?;
        }
        self.leave();
        Ok(head.unwrap_or(nil))
    }

    // --- Terms ---------------------------------------------------------------

    fn iri_ref(&mut self) -> Parsed<Term> {
        if !self.eat('<') {
            return self.fail("an IRI must start with '<'");
        }
        let mut reference = String::new();
        loop {
            match self.bump() {
                None => return self.fail("an IRI is not closed"),
                Some('>') => break,
                Some('\\') => {
                    let escaped = self.unicode_escape()?;
                    reference.push(escaped);
                }
                Some(c) if c <= ' ' || matches!(c, '<' | '"' | '{' | '}' | '|' | '^' | '`') => {
                    return self.fail("an IRI holds a character it may not");
                }
                Some(c) => reference.push(c),
            }
            if reference.len() > MAX_IRI_BYTES {
                return self.fail("an IRI is too long");
            }
        }
        let resolved = resolve(&self.base, &reference);
        self.intern_iri(&resolved)
    }

    fn prefixed_name(&mut self) -> Parsed<Term> {
        let start = self.pos;
        while self.peek().is_some_and(is_name_char) {
            self.bump();
        }
        let prefix = &self.text[start..self.pos];
        if !self.eat(':') {
            return self.fail("expected an IRI, a prefixed name or a literal");
        }
        let Some(namespace) = self.prefixes.get(prefix).cloned() else {
            return self.fail("a prefix that was never declared");
        };
        let mut local = String::new();
        loop {
            match self.peek() {
                Some(c) if is_name_char(c) || c == ':' => {
                    // A '.' may be inside a name but not at its end, where
                    // it ends the statement.
                    if c == '.'
                        && !self.peek_at(1).is_some_and(|next| {
                            is_name_char(next) || matches!(next, ':' | '%' | '\\')
                        })
                    {
                        break;
                    }
                    self.bump();
                    local.push(c);
                }
                Some('%') => {
                    self.bump();
                    let (Some(high), Some(low)) = (self.bump(), self.bump()) else {
                        return self.fail("a '%' escape is cut short");
                    };
                    if !high.is_ascii_hexdigit() || !low.is_ascii_hexdigit() {
                        return self.fail("a '%' escape is not hexadecimal");
                    }
                    // Kept as written: the escape is part of the IRI.
                    local.push('%');
                    local.push(high);
                    local.push(low);
                }
                Some('\\') => {
                    self.bump();
                    match self.bump() {
                        Some(
                            c @ ('_' | '~' | '.' | '-' | '!' | '$' | '&' | '\'' | '(' | ')' | '*'
                            | '+' | ',' | ';' | '=' | '/' | '?' | '#' | '@' | '%'),
                        ) => local.push(c),
                        _ => return self.fail("an escape in a name that is not allowed"),
                    }
                }
                _ => break,
            }
            if local.len() > MAX_IRI_BYTES {
                return self.fail("an IRI is too long");
            }
        }
        if namespace.len() + local.len() > MAX_IRI_BYTES {
            return self.fail("an IRI is too long");
        }
        let mut iri = String::with_capacity(namespace.len() + local.len());
        iri.push_str(&namespace);
        iri.push_str(&local);
        self.intern_iri(&iri)
    }

    fn blank_label(&mut self) -> Parsed<Term> {
        if !(self.eat('_') && self.eat(':')) {
            return self.fail("a blank node label must start with '_:'");
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '.' && !self.peek_at(1).is_some_and(is_name_char) {
                break;
            }
            if !is_name_char(c) {
                break;
            }
            self.bump();
        }
        let label = &self.text[start..self.pos];
        if label.is_empty() {
            return self.fail("a blank node label is empty");
        }
        if let Some(&known) = self.blanks.get(label) {
            return Ok(Term::Blank(known));
        }
        if label.len() > MAX_IRI_BYTES {
            return self.fail("a blank node label is too long");
        }
        let Term::Blank(node) = self.fresh_blank()? else {
            return self.fail("too many blank nodes");
        };
        self.blanks.insert(label.to_owned(), node);
        Ok(Term::Blank(node))
    }

    fn boolean(&mut self) -> Parsed<Term> {
        let value = if self.at_keyword("true") {
            self.pos += 4;
            "true"
        } else {
            self.pos += 5;
            "false"
        };
        self.typed_literal(value, "boolean")
    }

    fn typed_literal(&mut self, value: &str, xsd_type: &str) -> Parsed<Term> {
        let value = self.intern(value)?;
        let datatype = self.intern(&format!("{XSD}{xsd_type}"))?;
        Ok(Term::Literal(Literal {
            value,
            datatype: Some(datatype),
            language: None,
        }))
    }

    fn number(&mut self) -> Parsed<Term> {
        let start = self.pos;
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        let digits = |parser: &mut Self| {
            let mut count = 0;
            while parser.peek().is_some_and(|c| c.is_ascii_digit()) {
                parser.bump();
                count += 1;
            }
            count
        };
        let whole = digits(self);
        let mut kind = "integer";
        let mut fraction = 0;
        // A '.' belongs to the number only if digits or an exponent follow;
        // otherwise it ends the statement.
        let exponent_follows = |parser: &Self, skip: usize| {
            matches!(parser.peek_at(skip), Some('e' | 'E'))
                && (parser.peek_at(skip + 1).is_some_and(|c| c.is_ascii_digit())
                    || (matches!(parser.peek_at(skip + 1), Some('+' | '-'))
                        && parser.peek_at(skip + 2).is_some_and(|c| c.is_ascii_digit())))
        };
        if self.peek() == Some('.')
            && (self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
                || (whole > 0 && exponent_follows(self, 1)))
        {
            self.bump();
            fraction = digits(self);
            kind = "decimal";
        }
        if whole == 0 && fraction == 0 {
            return self.fail("a number has no digits");
        }
        if exponent_follows(self, 0) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            digits(self);
            kind = "double";
        }
        let text = self.text[start..self.pos].to_owned();
        self.typed_literal(&text, kind)
    }

    fn string_literal(&mut self) -> Parsed<Term> {
        let Some(quote) = self.bump() else {
            return self.fail("a string is missing");
        };
        let long = self.peek() == Some(quote) && self.peek_at(1) == Some(quote);
        if long {
            self.bump();
            self.bump();
        }
        let mut value = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.fail("a string is not closed");
            };
            if matches!(c, '\n' | '\r') && !long {
                return self.fail("a short string spans lines");
            }
            self.bump();
            if c == quote {
                if !long {
                    break;
                }
                if self.peek() == Some(quote) && self.peek_at(1) == Some(quote) {
                    self.bump();
                    self.bump();
                    break;
                }
                value.push(c);
                continue;
            }
            match c {
                '\\' => {
                    let escaped = match self.peek() {
                        Some('t') => '\t',
                        Some('b') => '\u{8}',
                        Some('n') => '\n',
                        Some('r') => '\r',
                        Some('f') => '\u{c}',
                        Some('"') => '"',
                        Some('\'') => '\'',
                        Some('\\') => '\\',
                        Some('u' | 'U') => {
                            let escaped = self.unicode_escape()?;
                            value.push(escaped);
                            continue;
                        }
                        _ => return self.fail("an escape in a string that is not allowed"),
                    };
                    self.bump();
                    value.push(escaped);
                }
                c => value.push(c),
            }
        }
        let value = self.intern(&value)?;
        let mut literal = Literal {
            value,
            datatype: None,
            language: None,
        };
        if self.eat('@') {
            let start = self.pos;
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                self.bump();
            }
            let tag = self.text[start..self.pos].to_ascii_lowercase();
            if tag.is_empty() || tag.len() > 64 || tag.starts_with('-') {
                return self.fail("a language tag is malformed");
            }
            literal.language = Some(self.intern(&tag)?);
        } else if self.peek() == Some('^') && self.peek_at(1) == Some('^') {
            self.bump();
            self.bump();
            let datatype = match self.peek() {
                Some('<') => self.iri_ref()?,
                _ => self.prefixed_name()?,
            };
            let Term::Iri(datatype) = datatype else {
                return self.fail("a datatype must be an IRI");
            };
            literal.datatype = Some(datatype);
        }
        Ok(Term::Literal(literal))
    }

    /// Reads the rest of a `\u` or `\U` escape, the backslash already
    /// read.
    fn unicode_escape(&mut self) -> Parsed<char> {
        let digits = match self.bump() {
            Some('u') => 4,
            Some('U') => 8,
            _ => return self.fail("an escape that is not allowed"),
        };
        let mut code = 0u32;
        for _ in 0..digits {
            let Some(digit) = self.bump().and_then(|c| c.to_digit(16)) else {
                return self.fail("a Unicode escape is not hexadecimal");
            };
            code = code * 16 + digit;
        }
        match char::from_u32(code) {
            Some(c) => Ok(c),
            None => self.fail("a Unicode escape is not a character"),
        }
    }
}

/// Whether `c` may be part of a prefix, a local name or a blank node
/// label (Turtle's `PN_CHARS` and the '.' they may hold inside).
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '-' | '.')
        || matches!(c, '\u{B7}' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{37D}')
        || matches!(c, '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{203F}'..='\u{2040}')
        || matches!(c, '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}')
        || matches!(c, '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

// --- IRIs -----------------------------------------------------------------------

/// `iri` without its fragment.
fn strip_fragment(iri: &str) -> &str {
    iri.split('#').next().unwrap_or(iri)
}

/// The parts of a URI reference, as RFC 3986 appendix B splits it.
struct Parts<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

fn split(reference: &str) -> Parts<'_> {
    let (rest, fragment) = match reference.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (reference, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, Some(query)),
        None => (rest, None),
    };
    // A scheme is letters, digits, '+', '-' and '.', starting with a letter,
    // before the first ':' and before any '/'.
    let scheme_end = rest.find(':').filter(|&end| {
        end > 0
            && rest[..end].starts_with(|c: char| c.is_ascii_alphabetic())
            && rest[..end]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    let (scheme, rest) = match scheme_end {
        Some(end) => (Some(&rest[..end]), &rest[end + 1..]),
        None => (None, rest),
    };
    let (authority, path) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find('/').unwrap_or(after.len());
            (Some(&after[..end]), &after[end..])
        }
        None => (None, rest),
    };
    Parts {
        scheme,
        authority,
        path,
        query,
        fragment,
    }
}

/// RFC 3986 section 5.2.4: removes `.` and `..` segments from a path.
fn remove_dot_segments(path: &str) -> String {
    let mut output: Vec<&str> = Vec::new();
    let absolute = path.starts_with('/');
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    let mut ends_in_folder = false;
    for (index, segment) in segments.iter().enumerate() {
        if index == 0 && absolute {
            continue;
        }
        match *segment {
            "." => ends_in_folder = index == last,
            ".." => {
                output.pop();
                ends_in_folder = index == last;
            }
            other => {
                output.push(other);
                ends_in_folder = false;
            }
        }
    }
    let mut joined = String::with_capacity(path.len());
    if absolute {
        joined.push('/');
    }
    joined.push_str(&output.join("/"));
    if ends_in_folder && !joined.ends_with('/') {
        joined.push('/');
    }
    joined
}

/// RFC 3986 section 5.2.2: the target of `reference` read against `base`.
pub(crate) fn resolve(base: &str, reference: &str) -> String {
    let reference_parts = split(reference);
    let base_parts = split(base);
    let mut target = String::with_capacity(base.len() + reference.len());
    let push_authority = |target: &mut String, authority: Option<&str>| {
        if let Some(authority) = authority {
            target.push_str("//");
            target.push_str(authority);
        }
    };
    let (scheme, authority, path, query);
    if let Some(own) = reference_parts.scheme {
        scheme = Some(own);
        authority = reference_parts.authority;
        path = remove_dot_segments(reference_parts.path);
        query = reference_parts.query;
    } else if reference_parts.authority.is_some() {
        scheme = base_parts.scheme;
        authority = reference_parts.authority;
        path = remove_dot_segments(reference_parts.path);
        query = reference_parts.query;
    } else {
        scheme = base_parts.scheme;
        authority = base_parts.authority;
        if reference_parts.path.is_empty() {
            path = base_parts.path.to_owned();
            query = reference_parts.query.or(base_parts.query);
        } else {
            if reference_parts.path.starts_with('/') {
                path = remove_dot_segments(reference_parts.path);
            } else {
                // Section 5.2.3: merged with the base path.
                let merged = if base_parts.authority.is_some() && base_parts.path.is_empty() {
                    format!("/{}", reference_parts.path)
                } else {
                    let folder = base_parts
                        .path
                        .rfind('/')
                        .map_or("", |at| &base_parts.path[..=at]);
                    format!("{folder}{}", reference_parts.path)
                };
                path = remove_dot_segments(&merged);
            }
            query = reference_parts.query;
        }
    }
    if let Some(scheme) = scheme {
        target.push_str(scheme);
        target.push(':');
    }
    push_authority(&mut target, authority);
    target.push_str(&path);
    if let Some(query) = query {
        target.push('?');
        target.push_str(query);
    }
    if let Some(fragment) = reference_parts.fragment {
        target.push('#');
        target.push_str(fragment);
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "file:///bundles/Gain.lv2/manifest.ttl";

    fn read(text: &str) -> Result<Graph, TurtleError> {
        let mut graph = Graph::new();
        graph.parse(text, BASE)?;
        Ok(graph)
    }

    fn iri(text: &str) -> Term {
        Graph::iri(text)
    }

    fn literal(graph: &Graph, subject: &Term, predicate: &str) -> Option<String> {
        graph
            .object(subject, predicate)
            .and_then(Term::literal)
            .map(|literal| literal.value.to_string())
    }

    #[test]
    fn reads_an_lv2_description() {
        let text = r#"
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
            PREFIX doap: <http://usefulinc.com/ns/doap#>

            <urn:example:gain>
                a lv2:Plugin , lv2:AmplifierPlugin ;
                lv2:binary <gain.so> ;
                rdfs:seeAlso <gain.ttl> ;
                doap:name "Gain" , "Verstärker"@DE ;
                lv2:port [
                    a lv2:ControlPort , lv2:InputPort ;
                    lv2:index 0 ;
                    lv2:symbol "gain" ;
                    lv2:default -6.5 ;
                    lv2:minimum -9e1 ;
                    lv2:maximum +12 ;
                    lv2:scalePoint [ rdfs:label "Unity" ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#value> 0.0 ]
                ] , [
                    a lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in" ; # a comment
                ] ;;
                .
        "#;
        let graph = read(text).unwrap();
        let plugin = iri("urn:example:gain");
        let types: Vec<&str> = graph
            .objects(&plugin, RDF_TYPE)
            .filter_map(Term::iri)
            .collect();
        assert_eq!(
            types,
            [
                "http://lv2plug.in/ns/lv2core#Plugin",
                "http://lv2plug.in/ns/lv2core#AmplifierPlugin"
            ]
        );
        assert_eq!(
            graph
                .object(&plugin, "http://lv2plug.in/ns/lv2core#binary")
                .and_then(Term::iri),
            Some("file:///bundles/Gain.lv2/gain.so")
        );
        let names: Vec<(String, Option<String>)> = graph
            .objects(&plugin, "http://usefulinc.com/ns/doap#name")
            .filter_map(Term::literal)
            .map(|l| {
                (
                    l.value.to_string(),
                    l.language.as_deref().map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("Gain".to_owned(), None),
                ("Verstärker".to_owned(), Some("de".to_owned()))
            ]
        );
        let ports: Vec<&Term> = graph
            .objects(&plugin, "http://lv2plug.in/ns/lv2core#port")
            .collect();
        assert_eq!(ports.len(), 2);
        let control = ports[0];
        let number = |predicate: &str| {
            graph
                .object(control, predicate)
                .and_then(Term::literal)
                .and_then(Literal::number)
        };
        assert_eq!(number("http://lv2plug.in/ns/lv2core#default"), Some(-6.5));
        assert_eq!(number("http://lv2plug.in/ns/lv2core#minimum"), Some(-90.0));
        assert_eq!(number("http://lv2plug.in/ns/lv2core#maximum"), Some(12.0));
        assert_eq!(
            literal(&graph, control, "http://lv2plug.in/ns/lv2core#symbol").as_deref(),
            Some("gain")
        );
        let point = graph
            .object(control, "http://lv2plug.in/ns/lv2core#scalePoint")
            .unwrap();
        assert_eq!(
            literal(&graph, point, "http://www.w3.org/2000/01/rdf-schema#label").as_deref(),
            Some("Unity")
        );
        assert_eq!(
            graph
                .subjects(RDF_TYPE, &iri("http://lv2plug.in/ns/lv2core#AudioPort"))
                .len(),
            1
        );
    }

    #[test]
    fn numbers_and_the_dot_that_ends_a_statement() {
        let graph = read(
            "@prefix : <urn:x#> .\n\
             :a :i 5 . :a :d 5.25 . :a :e 1.5E3 . :a :f .5 . :a :g 7.e2 . :a :h -0 .\n\
             :a :b true , false . :a :n :name.with.dots . :a :m :end.",
        )
        .unwrap();
        let a = iri("urn:x#a");
        let get = |p: &str| graph.object(&a, &format!("urn:x#{p}")).cloned().unwrap();
        let number = |p: &str| get(p).literal().and_then(Literal::number);
        assert_eq!(number("i"), Some(5.0));
        assert_eq!(number("d"), Some(5.25));
        assert_eq!(number("e"), Some(1500.0));
        assert_eq!(number("f"), Some(0.5));
        assert_eq!(number("g"), Some(700.0));
        assert_eq!(number("h"), Some(0.0));
        assert_eq!(
            get("i").literal().unwrap().datatype.as_deref(),
            Some("http://www.w3.org/2001/XMLSchema#integer")
        );
        assert_eq!(
            get("e").literal().unwrap().datatype.as_deref(),
            Some("http://www.w3.org/2001/XMLSchema#double")
        );
        assert_eq!(get("b").literal().unwrap().boolean(), Some(true));
        assert_eq!(get("n").iri(), Some("urn:x#name.with.dots"));
        assert_eq!(get("m").iri(), Some("urn:x#end"));
    }

    #[test]
    fn strings_in_every_form() {
        let graph = read(concat!(
            "<urn:s> <urn:p> 'single' , \"double \\\"q\\\" \\t\\u00e9\\U0001F600\" ,\n",
            "  \"\"\"long\nwith \"quotes\" \"\"\" , '''a''b''' , \"typed\"^^<urn:t> .",
        ))
        .unwrap();
        let values: Vec<String> = graph
            .objects(&iri("urn:s"), "urn:p")
            .filter_map(Term::literal)
            .map(|l| l.value.to_string())
            .collect();
        assert_eq!(
            values,
            [
                "single",
                "double \"q\" \té\u{1F600}",
                "long\nwith \"quotes\" ",
                "a''b",
                "typed"
            ]
        );
        let typed = graph.objects(&iri("urn:s"), "urn:p").nth(4).unwrap();
        assert_eq!(typed.literal().unwrap().datatype.as_deref(), Some("urn:t"));
    }

    #[test]
    fn collections_and_blank_labels() {
        let graph = read(
            "<urn:s> <urn:list> ( 1 _:b ( ) ) .\n_:b <urn:p> <urn:o> .\n<urn:s> <urn:empty> () .",
        )
        .unwrap();
        let s = iri("urn:s");
        let head = graph.object(&s, "urn:list").unwrap().clone();
        let first = |node: &Term| graph.object(node, RDF_FIRST).cloned().unwrap();
        let rest = |node: &Term| graph.object(node, RDF_REST).cloned().unwrap();
        assert_eq!(first(&head).literal().and_then(Literal::number), Some(1.0));
        let second = rest(&head);
        let labelled = first(&second);
        assert_eq!(graph.object(&labelled, "urn:p"), Some(&iri("urn:o")));
        let third = rest(&second);
        assert_eq!(first(&third), iri(RDF_NIL));
        assert_eq!(rest(&third), iri(RDF_NIL));
        assert_eq!(graph.object(&s, "urn:empty"), Some(&iri(RDF_NIL)));
    }

    #[test]
    fn blank_labels_belong_to_their_document() {
        let mut graph = Graph::new();
        graph.parse("_:x <urn:p> 1 .", BASE).unwrap();
        graph.parse("_:x <urn:p> 2 .", BASE).unwrap();
        let subjects: HashSet<&Term> = graph.triples().iter().map(|t| &t.subject).collect();
        assert_eq!(subjects.len(), 2);
    }

    #[test]
    fn relative_references_follow_rfc_3986() {
        // The examples of RFC 3986 section 5.4.
        let base = "http://a/b/c/d;p?q";
        for (reference, target) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            (";x", "http://a/b/c/;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
        ] {
            assert_eq!(resolve(base, reference), target, "{reference:?}");
        }
        assert_eq!(
            resolve("file:///C:/Plug%20ins/A.lv2/manifest.ttl", "a.dll"),
            "file:///C:/Plug%20ins/A.lv2/a.dll"
        );
        // `@base` changes what later references are read against.
        let graph = read("@base <http://x/y/> . <z> <p> <../w> .").unwrap();
        assert_eq!(
            graph.object(&iri("http://x/y/z"), "http://x/y/p"),
            Some(&iri("http://x/w"))
        );
    }

    #[test]
    fn errors_say_where_they_are() {
        for (text, line) in [
            ("<urn:s> <urn:p> <urn:o>", 1),
            ("\n\n<urn:s> <urn:p> .", 3),
            ("<urn:s> nope:p <urn:o> .", 1),
            ("<urn:s> <urn:p> \"open", 1),
            ("<urn:s> <urn:p> 'a\nb' .", 1),
            ("@prefix x <urn:x> .", 1),
            ("@unknown <urn:x> .", 1),
            ("<urn:s> <urn:p> [ <urn:q> 1 .", 1),
            ("<urn:s> <urn:p> ( 1 2 .", 1),
            ("<urn:s> <urn:p> \"x\"@ .", 1),
            ("<urn:s> <urn:p> <bad iri> .", 1),
            ("<urn:s> <urn:p> \"\\q\" .", 1),
            ("<urn:s> <urn:p> + .", 1),
            ("<urn:s> \"literal\" <urn:o> .", 1),
        ] {
            let error = read(text).expect_err(text);
            assert_eq!(error.line, line, "{text:?}: {error}");
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "long under Miri; the code it reaches runs in the other tests"
    )]
    fn nesting_is_bounded() {
        let deep = |depth: usize, open: &str, inner: &str, close: &str| {
            format!(
                "<urn:s> <urn:p> {}{inner}{} .",
                open.repeat(depth),
                close.repeat(depth)
            )
        };
        let brackets = |depth| deep(depth, "[ <urn:q> ", "1", "]");
        let lists = |depth| deep(depth, "( ", "", ")");
        assert!(read(&brackets(MAX_DEPTH)).is_ok());
        assert_eq!(
            read(&brackets(MAX_DEPTH + 1)).unwrap_err().what,
            "brackets or collections nested too deeply"
        );
        assert!(read(&lists(MAX_DEPTH)).is_ok());
        assert!(read(&lists(MAX_DEPTH + 1)).is_err());
        // Far deeper than the stack could take if the depth were not
        // checked first, and still within the length of a document.
        assert!(read(&lists(300_000)).is_err());
        assert!(read(&brackets(300_000)).is_err());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "long under Miri; the code it reaches runs in the other tests"
    )]
    fn repetition_does_not_multiply_memory() {
        // One long namespace used many times: each use is the same string.
        let namespace = format!("urn:{}#", "n".repeat(MAX_IRI_BYTES - 10));
        let mut text = format!("@prefix p: <{namespace}> .\n<urn:s> <urn:p> ");
        for _ in 0..20_000 {
            text.push_str("p:x , ");
        }
        text.push_str("p:x .");
        let graph = read(&text).unwrap();
        assert_eq!(graph.len(), 20_001);
        assert!(graph.string_bytes < 4 * MAX_IRI_BYTES);

        // An IRI past the limit, whole or put together from a prefix.
        let long = format!("<urn:s> <urn:p> <urn:{}> .", "x".repeat(MAX_IRI_BYTES));
        assert!(read(&long).is_err());
        let joined = format!(
            "@prefix p: <{namespace}> . <urn:s> <urn:p> p:{} .",
            "y".repeat(64)
        );
        assert!(read(&joined).is_err());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "long under Miri; the code it reaches runs in the other tests"
    )]
    fn the_number_of_triples_is_bounded() {
        let mut text = String::from("<urn:s> <urn:p> 1");
        for _ in 0..MAX_TRIPLES {
            text.push_str(",1");
        }
        text.push_str(" .");
        let error = read(&text).unwrap_err();
        assert_eq!(error.what, "too many triples");
        let too_long = " ".repeat(MAX_DOCUMENT_BYTES + 1);
        assert!(read(&too_long).is_err());
    }

    /// A description close to what the framework's LV2 wrapper writes,
    /// with something of every kind the reader handles.
    const SAMPLE: &str = r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:manguebyte:org.example.gain>
    a lv2:Plugin , lv2:InstrumentPlugin ;
    lv2:binary <gain%20plugin.so> ;
    rdfs:comment """Turns "it" up""" ;
    lv2:minorVersion 2006 ;
    lv2:port [
        a lv2:ControlPort , lv2:InputPort ;
        lv2:index 0 ;
        lv2:symbol "gain" ;
        lv2:name "Gain \u00e9" ;
        lv2:default 1.0e0 ;
        lv2:scalePoint [ rdfs:label 'x' ; rdf:value 1 ] ;
        lv2:list ( 1 2.5 _:b ) ;
    ] .
_:b a lv2:Port .
"#;

    #[test]
    #[cfg_attr(
        miri,
        ignore = "long under Miri; the code it reaches runs in the other tests"
    )]
    fn every_truncation_and_every_damaged_byte_is_handled() {
        let sample = SAMPLE
            .replace(
                "rdf:value",
                "<http://www.w3.org/1999/02/22-rdf-syntax-ns#value>",
            )
            .replace("_:b a lv2:Port", "_:b a <urn:port>");
        read(&sample).expect("the sample is valid");
        let bytes = sample.as_bytes();
        for length in 0..=bytes.len() {
            if let Ok(text) = std::str::from_utf8(&bytes[..length]) {
                // A result either way; never a panic.
                let _ = read(text);
            }
        }
        for index in 0..bytes.len() {
            for damage in [
                b'"', b'<', b'>', b'[', b']', b'(', b')', b'.', b';', b'\\', b'#', 0,
            ] {
                let mut copy = bytes.to_vec();
                copy[index] = damage;
                if let Ok(text) = std::str::from_utf8(&copy) {
                    let _ = read(text);
                }
            }
        }
    }
}
