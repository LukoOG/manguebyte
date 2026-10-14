//! Address pattern matching: a table of cases, a comparison against a
//! second matcher written straight from the specification, and patterns
//! built to make a naive matcher take for ever.
//!
//! The second matcher shares no code with the crate: it decides which
//! patterns are well formed with its own reading of the rules
//! (`reference_well_formed`), and matches by trying every possibility.
//! Both of those are checked against hand-written tables first, so that
//! agreeing with them means something.

use std::time::{Duration, Instant};

use manguebyte_osc::Dispatcher;
use manguebyte_osc::pattern::{PatternError, has_wildcards, matches, validate_pattern};

/// Pattern, address, and whether the first matches the second.
const CASES: &[(&str, &str, bool)] = &[
    // Plain addresses match themselves and nothing else.
    ("/a", "/a", true),
    ("/a", "/b", false),
    ("/a", "/A", false),
    ("/a", "/a/b", false),
    ("/a/b", "/a", false),
    ("/a/b", "/a/b", true),
    ("/", "/", true),
    ("/a", "/", false),
    ("/", "/a", false),
    ("/oscillator/4/frequency", "/oscillator/4/frequency", true),
    ("/oscillator/4/frequency", "/oscillator/4/frequenc", false),
    // `?`: exactly one character, never a slash.
    ("/?", "/a", true),
    ("/?", "/ab", false),
    ("/?", "/", false),
    ("/a?c", "/abc", true),
    ("/a?c", "/ac", false),
    ("/a?c", "/a/c", false),
    ("/??", "/ab", true),
    ("/??", "/a", false),
    ("/?/?", "/a/b", true),
    // `*`: any run, including none, never a slash.
    ("/*", "/abc", true),
    ("/*", "/", true),
    ("/*", "/a/b", false),
    ("/a*", "/a", true),
    ("/a*", "/abc", true),
    ("/a*", "/b", false),
    ("/*a", "/a", true),
    ("/*a", "/ba", true),
    ("/*a", "/ab", false),
    ("/a*c", "/ac", true),
    ("/a*c", "/abbbc", true),
    ("/a*c", "/abbbd", false),
    ("/a*c", "/ab/c", false),
    ("/a**c", "/abc", true),
    ("/*/*", "/a/b", true),
    ("/*/*", "/a", false),
    ("/*/*", "/a/b/c", false),
    ("/a*/b", "/axx/b", true),
    ("/a*/b", "/a/x/b", false),
    // The star has to give characters back for what follows it.
    ("/*a*b*", "/xxaxxbxx", true),
    ("/*a*b*", "/xxbxxaxx", false),
    ("/a*a", "/a", false),
    ("/a*a", "/aa", true),
    ("/a*a", "/aba", true),
    ("/*aab", "/aaab", true),
    ("/*ab*ab", "/abab", true),
    ("/*ab*ab", "/ababab", true),
    ("/*ab*ab", "/aba", false),
    ("/*?", "/a", true),
    ("/*?", "/", false),
    ("/?*?", "/ab", true),
    ("/?*?", "/a", false),
    // Brackets: a list, a range, a negation.
    ("/[abc]", "/b", true),
    ("/[abc]", "/d", false),
    ("/[abc]", "/ab", false),
    ("/[abc]", "/", false),
    ("/[a-c]x", "/bx", true),
    ("/[a-c]", "/a", true),
    ("/[a-c]", "/c", true),
    ("/[a-c]", "/d", false),
    ("/[c-a]", "/b", true),
    ("/[0-9][0-9]", "/42", true),
    ("/[0-9][0-9]", "/4", false),
    ("/[a-cx-z]", "/y", true),
    ("/[a-cx-z]", "/m", false),
    ("/[!abc]", "/d", true),
    ("/[!abc]", "/a", false),
    ("/[!a-c]", "/b", false),
    ("/[!a-c]", "/z", true),
    // A negated list still does not match a slash.
    ("/a[!b]c", "/a/c", false),
    ("/a[!b]c", "/axc", true),
    // `!` negates only in first place, `-` makes a range only between two
    // characters.
    ("/[a!]", "/!", true),
    ("/[a!]", "/a", true),
    ("/[a!]", "/b", false),
    ("/[a-]", "/-", true),
    ("/[a-]", "/a", true),
    ("/[a-]", "/b", false),
    ("/[-a]", "/-", true),
    ("/[-a]", "/a", true),
    ("/[!-]", "/-", false),
    ("/[!-]", "/x", true),
    ("/[!!]", "/!", false),
    ("/[!!]", "/a", true),
    ("/[a-c-e]", "/-", true),
    ("/[a-c-e]", "/e", true),
    ("/[a-c-e]", "/d", false),
    // An empty list matches nothing; the negation of it, any one character.
    ("/[]", "/a", false),
    ("/[]", "/", false),
    ("/[!]", "/a", true),
    ("/[!]", "/", false),
    ("/[!]", "/ab", false),
    // Inside brackets the other special characters are ordinary.
    ("/[*?]", "/*", true),
    ("/[*?]", "/?", true),
    ("/[*?]", "/a", false),
    ("/[{]", "/{", true),
    ("/[[]", "/[", true),
    ("/[,]", "/,", true),
    // Braces: one of the strings, taken literally.
    ("/{foo,bar}", "/foo", true),
    ("/{foo,bar}", "/bar", true),
    ("/{foo,bar}", "/baz", false),
    ("/{foo,bar}", "/foobar", false),
    ("/{foo,bar}", "/", false),
    ("/a{b,c}d", "/abd", true),
    ("/a{b,c}d", "/acd", true),
    ("/a{b,c}d", "/ad", false),
    ("/a{b,c}d", "/abcd", false),
    ("/{a}", "/a", true),
    ("/note{,s}", "/note", true),
    ("/note{,s}", "/notes", true),
    ("/note{,s}", "/notess", false),
    ("/{}", "/", true),
    ("/x{}y", "/xy", true),
    ("/x{}y", "/x,y", false),
    // Both the short and the long alternative have to be tried.
    ("/{a,ab}b", "/ab", true),
    ("/{a,ab}b", "/abb", true),
    ("/{a,ab}b", "/abbb", false),
    ("/{ab,a}b", "/ab", true),
    ("/{ab,a}b", "/abb", true),
    ("/{a,ab}{b,bc}c", "/abc", true),
    ("/{a,ab}{b,bc}c", "/abcc", true),
    ("/{a,ab}{b,bc}c", "/abbc", true),
    ("/{a,ab}{b,bc}c", "/abbcc", true),
    ("/{a,ab}{b,bc}c", "/ac", false),
    ("/{a,ab}*b", "/ab", true),
    ("/*{a,ab}", "/xab", true),
    // Inside braces the other special characters are ordinary.
    ("/{a*,b}", "/a*", true),
    ("/{a*,b}", "/ab", false),
    ("/{a?}", "/a?", true),
    ("/{a?}", "/ab", false),
    ("/{[a]}", "/[a]", true),
    ("/{[a]}", "/a", false),
    // `//`: any number of whole parts, including none.
    ("//b", "/b", true),
    ("//b", "/a/b", true),
    ("//b", "/a/c/b", true),
    ("//b", "/ab", false),
    ("//b", "/b/c", false),
    ("//b", "/", false),
    ("/a//b", "/a/b", true),
    ("/a//b", "/a/x/b", true),
    ("/a//b", "/a/x/y/b", true),
    ("/a//b", "/ab", false),
    ("/a//b", "/a/xb", false),
    ("/a//b", "/x/a/b", false),
    ("/a//b", "/a/b/x", false),
    ("//a//b", "/x/a/y/b", true),
    ("//a//b", "/a/b", true),
    ("//a//b", "/b/a", false),
    ("//*", "/a/b/c", true),
    ("//*", "/", true),
    ("//?", "/a/b", true),
    ("//?", "/a/bc", false),
    ("//[ab]", "/x/a", true),
    ("//{x,y}z", "/q/yz", true),
    ("//spherical", "/position/spherical", true),
    ("//spherical", "/velocity/spherical", true),
    ("//spherical", "/velocity/cartesian", false),
    ("/a*//b", "/ax/y/b", true),
    ("/a*//b", "/b/y/b", false),
    // One character, not one byte.
    ("/?", "/\u{e9}", true),
    ("/??", "/\u{e9}", false),
    ("/caf?", "/caf\u{e9}", true),
    ("/caf\u{e9}", "/caf\u{e9}", true),
    ("/caf[\u{e9}]", "/caf\u{e9}", true),
    ("/[\u{e0}-\u{fc}]", "/\u{e9}", true),
    ("/[!\u{e0}-\u{fc}]", "/\u{e9}", false),
    ("/{\u{e9},e}", "/\u{e9}", true),
    ("/*\u{e9}", "/caf\u{e9}", true),
    // As a sender would write them.
    ("/oscillator/*/frequency", "/oscillator/4/frequency", true),
    (
        "/oscillator/[1-4]/frequency",
        "/oscillator/4/frequency",
        true,
    ),
    (
        "/oscillator/[1-3]/frequency",
        "/oscillator/4/frequency",
        false,
    ),
    ("/{oscillator,filter}/4/*", "/filter/4/q", true),
    (
        "/{oscillator,filter}/?/freq*",
        "/oscillator/4/frequency",
        true,
    ),
    // A pattern that is not well formed matches nothing, not even itself.
    ("", "", false),
    ("a", "a", false),
    ("a/b", "a/b", false),
    ("/a/", "/a/", false),
    ("/a/", "/a", false),
    ("//", "/", false),
    ("//", "//", false),
    ("/a//", "/a/", false),
    ("///a", "/a", false),
    ("///a", "///a", false),
    ("/a///b", "/a/b", false),
    ("/a[", "/a[", false),
    ("/a[b", "/ab", false),
    ("/a]", "/a]", false),
    ("/a{b", "/ab", false),
    ("/a{b", "/a{b", false),
    ("/a}", "/a}", false),
    ("/{a{b}}", "/ab", false),
    ("/[a/b]", "/a", false),
    ("/{a/b}", "/a/b", false),
];

#[test]
fn the_table_of_cases_holds() {
    for &(pattern, address, expected) in CASES {
        assert_eq!(
            matches(pattern, address),
            expected,
            "{pattern:?} against {address:?}"
        );
    }
    println!("{} cases", CASES.len());
    assert!(CASES.len() >= 190);
}

#[test]
fn malformed_patterns_are_named_and_match_nothing() {
    let cases = [
        ("", PatternError::NoLeadingSlash),
        ("a", PatternError::NoLeadingSlash),
        ("*", PatternError::NoLeadingSlash),
        ("/a/", PatternError::EmptyPart { at: 2 }),
        ("//", PatternError::EmptyPart { at: 1 }),
        ("/a//", PatternError::EmptyPart { at: 3 }),
        ("///a", PatternError::EmptyPart { at: 1 }),
        ("/a///b", PatternError::EmptyPart { at: 3 }),
        ("/a[", PatternError::UnclosedBracket { at: 2 }),
        ("/a[bc", PatternError::UnclosedBracket { at: 2 }),
        ("/a{b,c", PatternError::UnclosedBrace { at: 2 }),
        ("/a]", PatternError::UnexpectedClose { ch: ']', at: 2 }),
        ("/a}b", PatternError::UnexpectedClose { ch: '}', at: 2 }),
        ("/[a]]", PatternError::UnexpectedClose { ch: ']', at: 4 }),
        ("/[a/b]", PatternError::InvalidInGroup { ch: '/', at: 3 }),
        ("/{a/b}", PatternError::InvalidInGroup { ch: '/', at: 3 }),
        ("/{a{b}}", PatternError::InvalidInGroup { ch: '{', at: 3 }),
    ];
    for (pattern, error) in cases {
        assert_eq!(validate_pattern(pattern), Err(error), "{pattern:?}");
        assert!(!error.to_string().is_empty());
        for address in ["/", "/a", "/ab", "/a/b", pattern] {
            assert!(
                !matches(pattern, address),
                "{pattern:?} against {address:?}"
            );
        }
    }
    for &(pattern, _, expected) in CASES {
        if expected {
            assert_eq!(validate_pattern(pattern), Ok(()), "{pattern:?}");
        }
    }
}

#[test]
fn a_pattern_with_nothing_special_is_not_a_wildcard() {
    for plain in ["/a", "/mixer/channel/3/gain", "/", "/a-b_c.d", ""] {
        assert!(!has_wildcards(plain), "{plain:?}");
    }
    for special in ["/a?", "/*", "/[a]", "/a]", "/{a,b}", "/a}", "//a", "/a//b"] {
        assert!(has_wildcards(special), "{special:?}");
    }
}

/// Whether a pattern is well formed, by the rules the `pattern` module
/// documents, written down without looking at how the crate checks them:
///
/// - it begins with `/`;
/// - every `[` has a `]` after it and every `{` a `}`, the first one after
///   it closing it; between `[` and `]` there is no `/`, and between `{`
///   and `}` neither a `/` nor another `{`;
/// - a `]` or `}` outside such a pair is an error;
/// - with each pair counted as one ordinary character, the slashes leave
///   no empty part where `//` cannot be meant: no run of three slashes,
///   and no slash at the end unless the whole pattern is `/`.
fn reference_well_formed(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    if chars.first() != Some(&'/') {
        return false;
    }
    let mut skeleton = String::new();
    let mut at = 0;
    while at < chars.len() {
        let (close, forbidden): (char, &[char]) = match chars[at] {
            '[' => (']', &['/']),
            '{' => ('}', &['/', '{']),
            ']' | '}' => return false,
            other => {
                skeleton.push(other);
                at += 1;
                continue;
            }
        };
        let Some(length) = chars[at + 1..].iter().position(|&ch| ch == close) else {
            return false;
        };
        let inside = &chars[at + 1..at + 1 + length];
        if inside.iter().any(|ch| forbidden.contains(ch)) {
            return false;
        }
        skeleton.push('x');
        at += length + 2;
    }
    !skeleton.contains("///") && (skeleton == "/" || !skeleton.ends_with('/'))
}

/// A second matcher, written as directly from the specification's text as
/// possible: read the pattern from the left and, wherever there is a
/// choice, try every possibility. It is exponential in the worst case and
/// only fit for short inputs, which is all it is given here.
fn reference(pattern: &str, address: &str) -> bool {
    if !reference_well_formed(pattern) {
        return false;
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let address: Vec<char> = address.chars().collect();
    reference_walk(&pattern, &address)
}

fn reference_walk(pattern: &[char], address: &[char]) -> bool {
    let Some(&first) = pattern.first() else {
        return address.is_empty();
    };
    match first {
        '/' if pattern.get(1) == Some(&'/') => {
            // A slash, then any number of whole parts: carry on after this
            // slash or after any later one.
            address.first() == Some(&'/')
                && (0..address.len())
                    .filter(|&at| address[at] == '/')
                    .any(|at| reference_walk(&pattern[2..], &address[at + 1..]))
        }
        '*' => {
            let part = address
                .iter()
                .position(|&ch| ch == '/')
                .unwrap_or(address.len());
            (0..=part).any(|taken| reference_walk(&pattern[1..], &address[taken..]))
        }
        '?' => {
            address.first().is_some_and(|&ch| ch != '/')
                && reference_walk(&pattern[1..], &address[1..])
        }
        '[' => {
            let close = pattern.iter().position(|&ch| ch == ']').unwrap();
            let Some(&ch) = address.first() else {
                return false;
            };
            ch != '/'
                && reference_class(&pattern[1..close], ch)
                && reference_walk(&pattern[close + 1..], &address[1..])
        }
        '{' => {
            let close = pattern.iter().position(|&ch| ch == '}').unwrap();
            pattern[1..close].split(|&ch| ch == ',').any(|alternative| {
                address.starts_with(alternative)
                    && reference_walk(&pattern[close + 1..], &address[alternative.len()..])
            })
        }
        literal => {
            address.first() == Some(&literal) && reference_walk(&pattern[1..], &address[1..])
        }
    }
}

fn reference_class(body: &[char], ch: char) -> bool {
    let (negated, body) = match body.first() {
        Some('!') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut included = false;
    let mut at = 0;
    while at < body.len() {
        if at + 2 < body.len() && body[at + 1] == '-' {
            let (low, high) = (body[at].min(body[at + 2]), body[at].max(body[at + 2]));
            included |= (low..=high).contains(&ch);
            at += 3;
        } else {
            included |= body[at] == ch;
            at += 1;
        }
    }
    included != negated
}

/// A small deterministic generator (xorshift64*), so that a failure can be
/// reproduced from the seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() >> 33) as usize % bound
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

#[test]
fn the_reference_matcher_agrees_on_the_table() {
    // If the second matcher disagreed with the table, comparing against it
    // below would prove nothing.
    for &(pattern, address, expected) in CASES {
        assert_eq!(
            reference(pattern, address),
            expected,
            "{pattern:?} against {address:?}"
        );
    }
}

/// Which patterns are well formed, decided by hand from the rules in the
/// `pattern` module's documentation, including the awkward places where a
/// character that is special outside a group is ordinary inside one.
const WELL_FORMED: &[(&str, bool)] = &[
    ("/", true),
    ("/a", true),
    ("/a/b", true),
    ("//a", true),
    ("/a//b", true),
    ("//a//b", true),
    ("/a b#,", true),
    ("/[]", true),
    ("/[!]", true),
    ("/{}", true),
    ("/{,}", true),
    ("/[}]", true),
    ("/[{]", true),
    ("/[[]", true),
    ("/{]}", true),
    ("/{[}", true),
    ("/{[a]}", true),
    ("/[a]/{b,c}/*", true),
    ("/[a]{b}", true),
    ("/a/{}", true),
    ("/\u{e9}/[\u{e0}-\u{fc}]", true),
    ("", false),
    ("a", false),
    ("[/]", false),
    ("{a}/b", false),
    ("/a/", false),
    ("//", false),
    ("///", false),
    ("///a", false),
    ("/a//", false),
    ("/a///b", false),
    ("/a////b", false),
    ("/{a}/", false),
    ("/[a]//", false),
    ("/[", false),
    ("/[a", false),
    ("/{", false),
    ("/{a,b", false),
    ("/]", false),
    ("/}", false),
    ("/[]]", false),
    ("/{a}}", false),
    ("/{[}]", false),
    ("/[{]}", false),
    ("/[a/b]", false),
    ("/[/]", false),
    ("/{/}", false),
    ("/{a{b}", false),
    ("/{{}", false),
    // A slash is not allowed in a group, even as its last character.
    ("/[a/]", false),
    ("/{a,b/}", false),
];

#[test]
fn well_formed_patterns_are_told_apart_by_both_readings_of_the_rules() {
    for &(pattern, expected) in WELL_FORMED {
        assert_eq!(reference_well_formed(pattern), expected, "{pattern:?}");
        assert_eq!(validate_pattern(pattern).is_ok(), expected, "{pattern:?}");
        if !expected {
            assert!(!matches(pattern, pattern), "{pattern:?}");
        }
    }
    // The rows of the matching table, too: every pattern that matches
    // something is well formed.
    for &(pattern, _, expected) in CASES {
        if expected {
            assert!(reference_well_formed(pattern), "{pattern:?}");
        }
    }
}

#[test]
fn random_well_formed_patterns_match_as_the_reference_says() {
    const PIECES: &[&str] = &[
        "a", "b", "a", "b", "ab", "?", "*", "[ab]", "[!a]", "[a-b]", "[!]", "{a,b}", "{,a}",
        "{a,ab}", "{ab,b}", "{}", "/", "/", "/", "//",
    ];
    const LETTERS: &[&str] = &["a", "b", "a", "b", "/", "c"];
    let mut random = Random(0x0005_C0FF_EE15_600D);
    let (mut compared, mut matched) = (0u32, 0u32);
    for _ in 0..40_000 {
        let mut pattern = String::from("/");
        for _ in 0..random.below(7) {
            pattern.push_str(random.pick(PIECES));
        }
        let well_formed = reference_well_formed(&pattern);
        assert_eq!(
            validate_pattern(&pattern).is_ok(),
            well_formed,
            "{pattern:?}"
        );
        if !well_formed {
            continue;
        }
        for _ in 0..8 {
            let mut address = String::from("/");
            for _ in 0..random.below(7) {
                address.push_str(random.pick(LETTERS));
            }
            let expected = reference(&pattern, &address);
            assert_eq!(
                matches(&pattern, &address),
                expected,
                "{pattern:?} against {address:?}"
            );
            compared += 1;
            matched += u32::from(expected);
        }
    }
    println!("{compared} comparisons, {matched} of them matches");
    // Enough of both answers for the agreement to mean something.
    assert!(compared > 100_000);
    assert!(matched > 10_000 && compared - matched > 10_000);
}

#[test]
fn random_text_of_any_kind_is_judged_as_the_reference_says() {
    // Mostly malformed patterns: this is about never panicking, about
    // `validate_pattern` drawing the line where the documented rules do,
    // and about `matches` rejecting exactly what is malformed.
    const ALPHABET: &[&str] = &[
        "a", "b", "/", "/", "*", "?", "[", "]", "{", "}", ",", "-", "!", "\u{e9}",
    ];
    let mut random = Random(0x0BAD_5EED_0BAD_5EED);
    let (mut compared, mut well_formed, mut matched) = (0u32, 0u32, 0u32);
    for _ in 0..150_000 {
        let mut pattern = String::new();
        if random.below(10) != 0 {
            pattern.push('/');
        }
        for _ in 0..random.below(9) {
            pattern.push_str(random.pick(ALPHABET));
        }
        let mut address = String::from("/");
        for _ in 0..random.below(6) {
            address.push_str(random.pick(&["a", "b", "/", "-", "!", ",", "\u{e9}"]));
        }
        let valid = validate_pattern(&pattern).is_ok();
        assert_eq!(valid, reference_well_formed(&pattern), "{pattern:?}");
        let expected = reference(&pattern, &address);
        assert_eq!(
            matches(&pattern, &address),
            expected,
            "{pattern:?} against {address:?}"
        );
        assert!(valid || !expected);
        // A pattern always fails against itself when it is malformed.
        assert!(valid || !matches(&pattern, &pattern));
        compared += 1;
        well_formed += u32::from(valid);
        matched += u32::from(expected);
    }
    println!("{compared} comparisons, {well_formed} well formed, {matched} matches");
    assert!(well_formed > 10_000 && compared - well_formed > 10_000);
    assert!(matched > 500);
}

#[test]
fn a_pattern_built_to_hurt_is_answered_in_bounded_time() {
    // Each of these makes a matcher that backs up on failure try a number
    // of combinations that doubles with every wildcard. With some hundreds
    // of wildcards that matcher would not finish in a lifetime; this one
    // needs about a second in an unoptimised build. The limit only has to
    // tell those two apart.
    let started = Instant::now();
    let letters = "a".repeat(200);
    let address = format!("/{letters}");

    let stars = format!("/{}b", "*a".repeat(2_000));
    assert!(!matches(&stars, &address));
    let stars_that_fit = format!("/{}*", "*a".repeat(200));
    assert!(matches(&stars_that_fit, &address));
    let one_too_many = format!("/{}*", "*a".repeat(201));
    assert!(!matches(&one_too_many, &address));

    let choices = format!("/{}b", "{,a}".repeat(1_000));
    assert!(!matches(&choices, &address));
    let choices_that_fit = format!("/{}", "{,a}".repeat(1_000));
    assert!(matches(&choices_that_fit, &address));
    let lengths = format!("/{}", "{a,aa}".repeat(150));
    assert!(matches(&lengths, &address));
    let too_short = format!("/{}", "{a,aa}".repeat(99));
    assert!(!matches(&too_short, &address));

    let descents = format!("{}/x", "//*".repeat(1_000));
    let deep = format!("{}/y", "/a".repeat(100));
    assert!(!matches(&descents, &deep));
    let descents_that_fit = format!("{}/y", "//*".repeat(50));
    assert!(matches(&descents_that_fit, &deep));

    let elapsed = started.elapsed();
    println!("pathological patterns took {elapsed:?}");
    assert!(elapsed < Duration::from_secs(60), "took {elapsed:?}");
}

#[test]
fn addresses_longer_than_the_working_memory_on_the_stack_still_match() {
    for len in [254, 255, 256, 257, 1_000, 5_000] {
        let address = format!("/{}", "ab".repeat(len / 2));
        assert!(matches("/*", &address), "length {len}");
        assert!(matches("/ab*b", &address));
        assert!(matches("/*{ab,ba}", &address));
        assert!(matches("//*[b]", &address));
        assert!(!matches("/*a", &address));
        assert!(!matches("/ab*/b", &address));
        assert!(matches(&address, &address));
        let question_marks = format!("/{}", "?".repeat(address.len() - 1));
        assert!(matches(&question_marks, &address));
        assert!(!matches(&format!("{question_marks}?"), &address));
    }
}

#[test]
fn the_dispatcher_reaches_what_the_matcher_matches() {
    let addresses = [
        "/a", "/b", "/a/a", "/a/b", "/b/a", "/ab", "/a/b/a", "/ba/b", "/a", "/c/ab",
    ];
    let mut dispatcher = Dispatcher::new();
    for (handler, address) in addresses.iter().enumerate() {
        dispatcher.add(address, handler as u32).unwrap();
    }

    const PIECES: &[&str] = &[
        "a", "b", "ab", "?", "*", "[ab]", "[!a]", "{a,b}", "{,a}", "/", "/", "//", "]",
    ];
    let mut random = Random(0x000D_15BA_7C4E_5EED);
    let mut reached_total = 0;
    for _ in 0..20_000 {
        let mut pattern = String::from("/");
        for _ in 0..random.below(6) {
            pattern.push_str(random.pick(PIECES));
        }
        let mut reached = Vec::new();
        let count = dispatcher.dispatch(&pattern, |handler| reached.push(handler));
        let expected: Vec<u32> = (0..addresses.len())
            .filter(|&index| matches(&pattern, addresses[index]))
            .map(|index| index as u32)
            .collect();
        assert_eq!(reached, expected, "{pattern:?}");
        assert_eq!(count, expected.len());
        reached_total += count;
    }
    assert!(reached_total > 5_000);
}

/// Registrations can be taken out again, as a receiver needs when a
/// parameter goes away. What is left is found by both of the dispatcher's
/// paths, the lookup of a plain address and the trial of a pattern against
/// every address, in the order it was added.
#[test]
fn what_is_removed_from_the_dispatcher_is_no_longer_reached() {
    const ADDRESSES: &[&str] = &["/a", "/b", "/a/a", "/a/b", "/b/a", "/ab", "/c"];
    const PATTERNS: &[&str] = &["/?", "/*", "//a", "/{a,c}", "/[!a]*", "/a/*"];
    let mut random = Random(0x0000_DE1E_7E00_0001);
    let mut removed_total = 0;
    for _ in 0..200 {
        let mut dispatcher = Dispatcher::new();
        let mut model: Vec<(&str, u32)> = Vec::new();
        for step in 0..40u32 {
            let address = random.pick(ADDRESSES);
            if random.below(3) == 0 {
                let before = model.len();
                model.retain(|&(other, _)| other != address);
                assert_eq!(dispatcher.remove(address), before - model.len());
                removed_total += before - model.len();
            } else {
                dispatcher.add(address, step).unwrap();
                model.push((address, step));
            }
            assert_eq!(dispatcher.len(), model.len());
            assert!(dispatcher.iter().eq(model.iter().copied()));
            for &pattern in ADDRESSES.iter().chain(PATTERNS) {
                let mut reached = Vec::new();
                let count = dispatcher.dispatch(pattern, |handler| reached.push(handler));
                let expected: Vec<u32> = model
                    .iter()
                    .filter(|&&(address, _)| matches(pattern, address))
                    .map(|&(_, handler)| handler)
                    .collect();
                assert_eq!(reached, expected, "{pattern:?}");
                assert_eq!(count, expected.len());
            }
        }
        // Something that was never there.
        assert_eq!(dispatcher.remove("/never"), 0);
        dispatcher.clear();
        assert!(dispatcher.is_empty());
        assert_eq!(dispatcher.dispatch("/*", |_| panic!("cleared")), 0);
        assert_eq!(dispatcher.dispatch("/a", |_| panic!("cleared")), 0);
    }
    assert!(removed_total > 1_000);
}
