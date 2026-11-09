//! XSD Regular Expression engine.
//!
//! Implements the XML Schema regular expression dialect as specified in
//! XSD 1.1 Part 2, Appendix F. Key differences from PCRE/Perl regex:
//!
//! - Patterns are always anchored (match the entire string).
//! - No anchors (`^`, `$`), backreferences, or lookahead/lookbehind.
//! - XSD-specific character class escapes: `\i`, `\c`, `\I`, `\C`.
//! - Unicode category escapes: `\p{Lu}`, `\p{IsBasicLatin}`, `\P{...}`.
//! - Character class subtraction: `[a-z-[aeiou]]`.
//! - Multi-character escapes: `\d`, `\D`, `\s`, `\S`, `\w`, `\W`.
//!
//! General categories (`\p{..}`, `\d`, `\w` and their complements) come from
//! the generated tables in `xsd_regex_tables`, which name their Unicode
//! version. `\i` and `\c` are the XML 1.0 (Second Edition) `Letter` and
//! `NameChar` productions that XSD 1.0 references, and the `\p{Is..}` block
//! names are XSD 1.0's fixed list (Part 2, Appendix F.1) plus the names earlier
//! releases accepted, so neither follows later Unicode versions.

use crate::xsd_regex_tables::{
    GeneralCategory, ASCII_CATEGORIES, CATEGORY_PAGES, CATEGORY_RUNS, XML_NAME_PAGES,
    XML_NAME_RANGES, XML_NAME_START_PAGES, XML_NAME_START_RANGES,
};

/// A compiled XSD regular expression.
#[derive(Debug, Clone)]
pub struct XsdRegex {
    node: RegexNode,
}

/// AST node for the regex.
#[derive(Debug, Clone)]
enum RegexNode {
    /// Match a single literal character.
    Literal(char),
    /// Match any character (`.`).
    Dot,
    /// Match a character class (positive or negative, with ranges/escapes).
    CharClass(CharClass),
    /// A sequence of nodes to match in order.
    Sequence(Vec<RegexNode>),
    /// Alternation: match one of the branches.
    Alternation(Vec<RegexNode>),
    /// Repetition: match the inner node between min and max times.
    Repetition {
        inner: Box<RegexNode>,
        min: usize,
        max: Option<usize>, // None = unbounded
    },
}

/// A character class, possibly with subtraction.
#[derive(Debug, Clone)]
struct CharClass {
    negated: bool,
    members: Vec<ClassMember>,
    subtraction: Option<Box<CharClass>>,
}

/// A member of a character class.
#[derive(Debug, Clone)]
enum ClassMember {
    /// A single character.
    Char(char),
    /// A character range (inclusive).
    Range(char, char),
    /// A multi-character escape (\d, \s, \w, \i, \c, etc.)
    Escape(CharEscape),
    /// A Unicode property (\p{...} or \P{...}).
    Property(UnicodeProperty),
    /// The union of the class's members that are defined by general
    /// categories (`\d`, `\D`, `\w`, `\W` and every `\p`/`\P` other than a
    /// block), as a set: bit `c as u32` stands for category `c`. Built by
    /// [`char_class`], so a class looks a character's category up once,
    /// whatever the number of such members.
    Categories(u32),
    /// A nested character class (for subtraction). Spec-complete placeholder;
    /// the parser does not currently produce nested character classes.
    #[allow(dead_code)]
    Nested(CharClass),
}

/// Multi-character escape types.
#[derive(Debug, Clone, Copy)]
enum CharEscape {
    /// `\d` = `\p{Nd}`
    Digit,
    /// `\D` = `[^\d]`
    NotDigit,
    /// `\s` = [ \t\n\r]
    Space,
    /// `\S` = [^ \t\n\r]
    NotSpace,
    /// `\w` = every character except the categories P, Z and C
    Word,
    /// `\W` = complement of \w
    NotWord,
    /// `\i` = XML 1.0 (Second Edition) initial name character (Letter | '_' | ':')
    XmlInitial,
    /// `\I` = complement of \i
    NotXmlInitial,
    /// `\c` = XML 1.0 (Second Edition) `NameChar`
    XmlNameChar,
    /// `\C` = complement of \c
    NotXmlNameChar,
}

/// Unicode property for \p{...} and \P{...}.
#[derive(Debug, Clone)]
struct UnicodeProperty {
    negated: bool,
    class: PropertyClass,
}

/// The set a `\p{..}` names, resolved when the pattern is compiled.
#[derive(Debug, Clone, Copy)]
enum PropertyClass {
    /// One general category, such as `Lu`.
    Category(GeneralCategory),
    /// All the categories of one letter, such as `L`.
    Group(char),
    /// An `Is` block escape: its code point ranges.
    Block(&'static [(u32, u32)]),
}

/// Default maximum nesting depth of `(...)` groups plus character-class
/// subtractions `[a-[b]]` in an XSD pattern. Real-world patterns rarely
/// exceed 4-5 levels of nesting; 64 is generous headroom while
/// preventing a pathologically-deep pattern from stack-overflowing the
/// recursive-descent parser. Override via
/// [`XsdRegex::compile_with_max_depth`].
pub const DEFAULT_MAX_REGEX_GROUP_DEPTH: u32 = 64;

/// Default maximum number of `match_node` invocations per call to
/// [`XsdRegex::is_match`]. The matcher is a backtracking-with-dedup
/// engine that can reach O(n^3) or O(n^4) cost on nested-repetition
/// patterns (classic polynomial ReDoS, e.g. `(a*)*b` against a long
/// string of `a`s). 1 million steps is enough for every legitimate
/// pattern we've seen in the W3C test suites (and plenty more) while
/// cutting a 1 000-byte polynomial-ReDoS input off in well under a
/// second. Override via [`XsdRegex::is_match_with_max_steps`].
///
/// Budget exhaustion is reported as a failed match (fail-closed). An
/// input the matcher cannot evaluate within the budget is treated as
/// "does not match" — the security-correct outcome for a schema
/// validator: the value gets rejected rather than causing a DoS.
pub const DEFAULT_MAX_REGEX_STEPS: usize = 1_000_000;

impl XsdRegex {
    /// Compile an XSD pattern string into a regex using the default
    /// group-nesting cap ([`DEFAULT_MAX_REGEX_GROUP_DEPTH`]).
    pub fn compile(pattern: &str) -> Result<Self, String> {
        Self::compile_with_max_depth(pattern, DEFAULT_MAX_REGEX_GROUP_DEPTH)
    }

    /// Compile an XSD pattern with an explicit group-nesting cap. Useful
    /// when the caller has their own budget (e.g. a stricter sandbox)
    /// or legitimately needs to accept patterns deeper than the default
    /// permits.
    pub fn compile_with_max_depth(pattern: &str, max_depth: u32) -> Result<Self, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut pos = 0;
        let node = parse_alternation(&chars, &mut pos, 0, max_depth)?;
        if pos < chars.len() {
            return Err(format!(
                "Unexpected character '{}' at position {}",
                chars[pos], pos
            ));
        }
        Ok(XsdRegex { node })
    }

    /// Test if the given string matches this pattern.
    ///
    /// XSD patterns are always anchored: the entire string must match.
    /// The per-match step budget scales with input length so legitimate
    /// large inputs against linear patterns (like `[a-z]+` over a
    /// several-MB text value) still match, while polynomial-blow-up
    /// patterns against the same-sized input still fail-closed quickly.
    /// Scaling formula: `max(DEFAULT_MAX_REGEX_STEPS, input_chars * 100)`.
    /// 100 steps per character is plenty for any O(n) pattern (which
    /// takes ~1 step per char) while keeping a tight enough cap that
    /// `O(n^2)` / `O(n^3)` adversarial patterns saturate in bounded time.
    pub fn is_match(&self, text: &str) -> bool {
        // Single walk over `text`: collect into `Vec<char>` once and
        // derive the budget from `chars.len()`. The naive `chars().count()
        // + chars().collect()` shape was a measurable double-scan on the
        // multi-MB inputs the budget scaling targets.
        let chars: Vec<char> = text.chars().collect();
        let scaled = chars.len().saturating_mul(100);
        let budget = scaled.max(DEFAULT_MAX_REGEX_STEPS);
        self.is_match_chars(&chars, budget)
    }

    /// Test if the given string matches this pattern with an explicit
    /// step budget. Useful when the caller has a stricter CPU budget
    /// than the default or needs to accept patterns that legitimately
    /// require more steps.
    pub fn is_match_with_max_steps(&self, text: &str, max_steps: usize) -> bool {
        let chars: Vec<char> = text.chars().collect();
        self.is_match_chars(&chars, max_steps)
    }

    /// Internal core: match against a pre-collected `&[char]` slice with
    /// an explicit step budget. Lets [`Self::is_match`] and
    /// [`Self::is_match_with_max_steps`] share the matcher invocation
    /// without each one re-scanning the input.
    fn is_match_chars(&self, chars: &[char], max_steps: usize) -> bool {
        let mut budget = MatchBudget::new(max_steps);
        match_node(&self.node, chars, 0, &mut budget)
            .into_iter()
            .any(|end| end == chars.len())
    }
}

/// Per-match step counter. The matcher ticks this on every entry to
/// `match_node`; once the budget is exhausted every subsequent tick
/// returns `false`, causing the matcher to report "no reachable
/// positions" and fail the match. This is how F-05 (polynomial ReDoS)
/// is contained without converting the engine to a Thompson-style NFA.
struct MatchBudget {
    steps: usize,
    max_steps: usize,
    /// The general-category run of the last non-ASCII character looked up in
    /// this match, reused while the next characters stay inside it.
    run: CategoryRun,
}

impl MatchBudget {
    fn new(max_steps: usize) -> Self {
        MatchBudget {
            steps: 0,
            max_steps,
            run: CategoryRun::EMPTY,
        }
    }

    /// Charge one step. Returns `true` while the budget has room and
    /// `false` once exhausted; once exhausted, the matcher treats every
    /// subsequent call as "no reachable positions".
    #[inline]
    fn tick(&mut self) -> bool {
        if self.steps >= self.max_steps {
            return false;
        }
        self.steps += 1;
        true
    }
}

// ─── Parser ──────────────────────────────────────────────────────────────────

/// Parse alternation: branch ('|' branch)*
///
/// `depth` is the current `(...)` / `-[...]` nesting depth; `max_depth`
/// is the configured cap (from [`XsdRegex::compile_with_max_depth`]).
/// Together they let a pathological pattern fail with a clean error
/// rather than stack-overflowing the process.
fn parse_alternation(
    chars: &[char],
    pos: &mut usize,
    depth: u32,
    max_depth: u32,
) -> Result<RegexNode, String> {
    let mut branches = vec![parse_sequence(chars, pos, depth, max_depth)?];
    while *pos < chars.len() && chars[*pos] == '|' {
        *pos += 1;
        branches.push(parse_sequence(chars, pos, depth, max_depth)?);
    }
    // A single branch collapses to that branch (no Alternation wrapper).
    // `remove(0)` is panic-free here: the length is exactly 1.
    Ok(match branches.len() {
        1 => branches.remove(0),
        _ => RegexNode::Alternation(branches),
    })
}

/// Parse a sequence of quantified atoms.
fn parse_sequence(
    chars: &[char],
    pos: &mut usize,
    depth: u32,
    max_depth: u32,
) -> Result<RegexNode, String> {
    let mut items = Vec::new();
    while *pos < chars.len() && chars[*pos] != '|' && chars[*pos] != ')' {
        items.push(parse_quantified(chars, pos, depth, max_depth)?);
    }
    // A single item collapses to that item (no Sequence wrapper); zero items
    // stays an empty Sequence. `remove(0)` is panic-free here: the length is 1.
    Ok(match items.len() {
        1 => items.remove(0),
        _ => RegexNode::Sequence(items),
    })
}

/// Parse an atom followed by an optional quantifier.
fn parse_quantified(
    chars: &[char],
    pos: &mut usize,
    depth: u32,
    max_depth: u32,
) -> Result<RegexNode, String> {
    let atom = parse_atom(chars, pos, depth, max_depth)?;
    if *pos < chars.len() {
        match chars[*pos] {
            '*' => {
                *pos += 1;
                Ok(RegexNode::Repetition {
                    inner: Box::new(atom),
                    min: 0,
                    max: None,
                })
            }
            '+' => {
                *pos += 1;
                Ok(RegexNode::Repetition {
                    inner: Box::new(atom),
                    min: 1,
                    max: None,
                })
            }
            '?' => {
                *pos += 1;
                Ok(RegexNode::Repetition {
                    inner: Box::new(atom),
                    min: 0,
                    max: Some(1),
                })
            }
            '{' => parse_brace_quantifier(chars, pos, atom),
            _ => Ok(atom),
        }
    } else {
        Ok(atom)
    }
}

/// Parse {n}, {n,}, {n,m}
fn parse_brace_quantifier(
    chars: &[char],
    pos: &mut usize,
    atom: RegexNode,
) -> Result<RegexNode, String> {
    *pos += 1; // skip '{'
    let min = parse_number(chars, pos)?;
    if *pos < chars.len() && chars[*pos] == '}' {
        *pos += 1;
        Ok(RegexNode::Repetition {
            inner: Box::new(atom),
            min,
            max: Some(min),
        })
    } else if *pos < chars.len() && chars[*pos] == ',' {
        *pos += 1;
        if *pos < chars.len() && chars[*pos] == '}' {
            *pos += 1;
            Ok(RegexNode::Repetition {
                inner: Box::new(atom),
                min,
                max: None,
            })
        } else {
            let max = parse_number(chars, pos)?;
            if *pos < chars.len() && chars[*pos] == '}' {
                *pos += 1;
                // Reject `{n,m}` with `m < n` at compile time. Without
                // this, `match_repetition` computes `m - n` directly and
                // would panic in debug / wrap in release. Patterns are
                // attacker-controlled via schemas, so fail-closed at
                // compile rather than during matching.
                if max < min {
                    return Err(format!("Quantifier {{{},{}}} has max < min", min, max));
                }
                Ok(RegexNode::Repetition {
                    inner: Box::new(atom),
                    min,
                    max: Some(max),
                })
            } else {
                Err("Expected '}' after quantifier".into())
            }
        }
    } else {
        Err("Expected ',' or '}' in quantifier".into())
    }
}

fn parse_number(chars: &[char], pos: &mut usize) -> Result<usize, String> {
    let start = *pos;
    while *pos < chars.len() && chars[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start {
        return Err("Expected number in quantifier".into());
    }
    let s: String = chars[start..*pos].iter().collect();
    s.parse::<usize>()
        .map_err(|_| format!("Invalid number: {}", s))
}

/// Parse a single atom: literal, '.', escape, group, or character class.
fn parse_atom(
    chars: &[char],
    pos: &mut usize,
    depth: u32,
    max_depth: u32,
) -> Result<RegexNode, String> {
    if *pos >= chars.len() {
        return Err("Unexpected end of pattern".into());
    }
    match chars[*pos] {
        '(' => {
            if depth >= max_depth {
                return Err(format!(
                    "Pattern group nesting exceeds maximum depth of {}",
                    max_depth
                ));
            }
            *pos += 1;
            let inner = parse_alternation(chars, pos, depth + 1, max_depth)?;
            if *pos < chars.len() && chars[*pos] == ')' {
                *pos += 1;
                Ok(inner)
            } else {
                Err("Expected ')'".into())
            }
        }
        '[' => {
            let cc = parse_char_class(chars, pos, depth, max_depth)?;
            Ok(RegexNode::CharClass(cc))
        }
        '.' => {
            *pos += 1;
            Ok(RegexNode::Dot)
        }
        '\\' => parse_escape(chars, pos),
        _ => {
            let c = chars[*pos];
            *pos += 1;
            Ok(RegexNode::Literal(c))
        }
    }
}

/// Parse an escape sequence: \d, \s, \p{...}, \n, \t, etc.
fn parse_escape(chars: &[char], pos: &mut usize) -> Result<RegexNode, String> {
    *pos += 1; // skip '\'
    if *pos >= chars.len() {
        return Err("Unexpected end of pattern after '\\'".into());
    }
    let c = chars[*pos];
    *pos += 1;
    match c {
        'd' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::Digit)],
            None,
        ))),
        'D' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::NotDigit)],
            None,
        ))),
        's' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::Space)],
            None,
        ))),
        'S' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::NotSpace)],
            None,
        ))),
        'w' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::Word)],
            None,
        ))),
        'W' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::NotWord)],
            None,
        ))),
        'i' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::XmlInitial)],
            None,
        ))),
        'I' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::NotXmlInitial)],
            None,
        ))),
        'c' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::XmlNameChar)],
            None,
        ))),
        'C' => Ok(RegexNode::CharClass(char_class(
            false,
            vec![ClassMember::Escape(CharEscape::NotXmlNameChar)],
            None,
        ))),
        'p' | 'P' => {
            let negated = c == 'P';
            if *pos < chars.len() && chars[*pos] == '{' {
                *pos += 1;
                let start = *pos;
                while *pos < chars.len() && chars[*pos] != '}' {
                    *pos += 1;
                }
                if *pos >= chars.len() {
                    return Err("Expected '}' after property name".into());
                }
                let name: String = chars[start..*pos].iter().collect();
                *pos += 1; // skip '}'
                let class = resolve_property(&name)
                    .ok_or_else(|| format!("Unknown Unicode property '{}'", name))?;
                Ok(RegexNode::CharClass(char_class(
                    false,
                    vec![ClassMember::Property(UnicodeProperty { negated, class })],
                    None,
                )))
            } else {
                Err("Expected '{' after \\p or \\P".into())
            }
        }
        'n' => Ok(RegexNode::Literal('\n')),
        'r' => Ok(RegexNode::Literal('\r')),
        't' => Ok(RegexNode::Literal('\t')),
        // All other escaped characters are literal
        _ => Ok(RegexNode::Literal(c)),
    }
}

/// Parse a character class: [...]
fn parse_char_class(
    chars: &[char],
    pos: &mut usize,
    depth: u32,
    max_depth: u32,
) -> Result<CharClass, String> {
    *pos += 1; // skip '['
    let negated = if *pos < chars.len() && chars[*pos] == '^' {
        *pos += 1;
        true
    } else {
        false
    };

    let mut members = Vec::new();
    parse_class_members(chars, pos, &mut members)?;

    // Check for subtraction: -[...]
    let subtraction = if *pos < chars.len() && chars[*pos] == '-' {
        // Look ahead: if next is '[', it's subtraction
        if *pos + 1 < chars.len() && chars[*pos + 1] == '[' {
            if depth >= max_depth {
                return Err(format!(
                    "Character-class subtraction nesting exceeds maximum depth of {}",
                    max_depth
                ));
            }
            *pos += 1; // skip '-'
            let sub = parse_char_class(chars, pos, depth + 1, max_depth)?;
            Some(Box::new(sub))
        } else {
            // Trailing dash — treat as literal
            members.push(ClassMember::Char('-'));
            *pos += 1;
            None
        }
    } else {
        None
    };

    if *pos < chars.len() && chars[*pos] == ']' {
        *pos += 1;
    } else {
        return Err("Expected ']' to close character class".into());
    }

    Ok(char_class(negated, members, subtraction))
}

/// Parse members inside a character class until ']' or subtraction '-['.
fn parse_class_members(
    chars: &[char],
    pos: &mut usize,
    members: &mut Vec<ClassMember>,
) -> Result<(), String> {
    while *pos < chars.len() && chars[*pos] != ']' {
        // Check for subtraction: -[
        if chars[*pos] == '-' && *pos + 1 < chars.len() && chars[*pos + 1] == '[' {
            break;
        }

        let member = parse_class_atom(chars, pos)?;

        // Check for range: a-b (but not if next is subtraction like a-[)
        if *pos + 1 < chars.len()
            && chars[*pos] == '-'
            && chars[*pos + 1] != '['
            && chars[*pos + 1] != ']'
        {
            // It's a range
            *pos += 1; // skip '-'
            let end_member = parse_class_atom(chars, pos)?;
            match (&member, &end_member) {
                (ClassMember::Char(start), ClassMember::Char(end)) => {
                    members.push(ClassMember::Range(*start, *end));
                }
                _ => {
                    // Not a valid range, treat as individual members with literal '-'
                    members.push(member);
                    members.push(ClassMember::Char('-'));
                    members.push(end_member);
                }
            }
        } else {
            members.push(member);
        }
    }
    Ok(())
}

/// Parse a single atom inside a character class.
fn parse_class_atom(chars: &[char], pos: &mut usize) -> Result<ClassMember, String> {
    if *pos >= chars.len() {
        return Err("Unexpected end of character class".into());
    }
    match chars[*pos] {
        '\\' => {
            *pos += 1;
            if *pos >= chars.len() {
                return Err("Unexpected end after '\\' in character class".into());
            }
            let c = chars[*pos];
            *pos += 1;
            match c {
                'd' => Ok(ClassMember::Escape(CharEscape::Digit)),
                'D' => Ok(ClassMember::Escape(CharEscape::NotDigit)),
                's' => Ok(ClassMember::Escape(CharEscape::Space)),
                'S' => Ok(ClassMember::Escape(CharEscape::NotSpace)),
                'w' => Ok(ClassMember::Escape(CharEscape::Word)),
                'W' => Ok(ClassMember::Escape(CharEscape::NotWord)),
                'i' => Ok(ClassMember::Escape(CharEscape::XmlInitial)),
                'I' => Ok(ClassMember::Escape(CharEscape::NotXmlInitial)),
                'c' => Ok(ClassMember::Escape(CharEscape::XmlNameChar)),
                'C' => Ok(ClassMember::Escape(CharEscape::NotXmlNameChar)),
                'p' | 'P' => {
                    let negated = c == 'P';
                    if *pos < chars.len() && chars[*pos] == '{' {
                        *pos += 1;
                        let start = *pos;
                        while *pos < chars.len() && chars[*pos] != '}' {
                            *pos += 1;
                        }
                        if *pos >= chars.len() {
                            return Err("Expected '}' after property name".into());
                        }
                        let name: String = chars[start..*pos].iter().collect();
                        *pos += 1;
                        let class = resolve_property(&name)
                            .ok_or_else(|| format!("Unknown Unicode property '{}'", name))?;
                        Ok(ClassMember::Property(UnicodeProperty { negated, class }))
                    } else {
                        Err("Expected '{' after \\p or \\P in character class".into())
                    }
                }
                'n' => Ok(ClassMember::Char('\n')),
                'r' => Ok(ClassMember::Char('\r')),
                't' => Ok(ClassMember::Char('\t')),
                _ => Ok(ClassMember::Char(c)),
            }
        }
        c => {
            *pos += 1;
            Ok(ClassMember::Char(c))
        }
    }
}

// ─── Matcher ─────────────────────────────────────────────────────────────────

/// Match a regex node against the input. Returns all possible end
/// positions. `budget` is charged one step per call; if the budget is
/// exhausted the function returns an empty vec (same shape as "no
/// match") and every subsequent call also short-circuits, so the
/// matcher as a whole reports "no match" rather than hanging.
fn match_node(
    node: &RegexNode,
    input: &[char],
    start: usize,
    budget: &mut MatchBudget,
) -> Vec<usize> {
    if !budget.tick() {
        return Vec::new();
    }
    match node {
        RegexNode::Literal(expected) => {
            if start < input.len() && input[start] == *expected {
                vec![start + 1]
            } else {
                vec![]
            }
        }
        RegexNode::Dot => {
            // XSD '.' matches any character except \n and \r
            if start < input.len() && input[start] != '\n' && input[start] != '\r' {
                vec![start + 1]
            } else {
                vec![]
            }
        }
        RegexNode::CharClass(cc) => {
            if start < input.len() && char_class_matches(cc, input[start], &mut budget.run) {
                vec![start + 1]
            } else {
                vec![]
            }
        }
        RegexNode::Sequence(nodes) => match_sequence(nodes, input, start, budget),
        RegexNode::Alternation(branches) => {
            let mut results = Vec::new();
            for branch in branches {
                results.extend(match_node(branch, input, start, budget));
            }
            results
        }
        RegexNode::Repetition { inner, min, max } => {
            match_repetition(inner, *min, *max, input, start, budget)
        }
    }
}

/// Match a sequence of nodes in order.
fn match_sequence(
    nodes: &[RegexNode],
    input: &[char],
    start: usize,
    budget: &mut MatchBudget,
) -> Vec<usize> {
    if nodes.is_empty() {
        return vec![start];
    }

    let mut current_positions = vec![start];

    for node in nodes {
        let mut next_positions = Vec::new();
        for &pos in &current_positions {
            next_positions.extend(match_node(node, input, pos, budget));
        }
        // Deduplicate to avoid exponential blowup
        next_positions.sort_unstable();
        next_positions.dedup();
        if next_positions.is_empty() {
            return vec![];
        }
        current_positions = next_positions;
    }

    current_positions
}

/// Match a repetition (greedy).
fn match_repetition(
    inner: &RegexNode,
    min: usize,
    max: Option<usize>,
    input: &[char],
    start: usize,
    budget: &mut MatchBudget,
) -> Vec<usize> {
    let mut current_positions = vec![start];

    // Match the first `min` occurrences (required). Per-iteration
    // sort+dedup is cheap here because `min` is bounded by the pattern
    // (not by input length), and inner branching is typically tiny.
    for _ in 0..min {
        let mut next = Vec::new();
        for &pos in &current_positions {
            next.extend(match_node(inner, input, pos, budget));
        }
        next.sort_unstable();
        next.dedup();
        if next.is_empty() {
            return vec![];
        }
        current_positions = next;
    }

    // `results` accumulates the unique reachable end-positions. The initial
    // positions are typically already sorted+deduped (from the required-min
    // loop, or a single start position), so a cheap sort+dedup suffices.
    let mut results: Vec<usize> = current_positions.clone();
    results.sort_unstable();
    results.dedup();

    // Defence-in-depth: `parse_brace_quantifier` rejects `{n,m}` with
    // `m < n` at compile time, so reaching the panic-on-underflow path
    // would require a future regression. Use `checked_sub` and treat
    // the impossible case as "no further iterations" (fail-closed).
    let remaining = match max {
        Some(m) => match m.checked_sub(min) {
            Some(r) => r,
            None => return results,
        },
        None => input.len().saturating_add(1), // More than enough
    };

    // The `seen` membership bitmap is sized to `input.len()` — an O(N)
    // allocation. It is materialized LAZILY, only on the first greedy
    // iteration that actually advances. A repetition that cannot match even
    // once (the common `a*b*`-over-`aaaa…` shape, where the inner atom fails
    // immediately) therefore never pays the O(N) allocation. Without this, an
    // outer repetition that invokes `match_repetition` O(N) times would incur
    // an O(N) allocation each time — O(N^2) total work that the per-`match_node`
    // step budget cannot bound, since a single tick covers an O(N) memset.
    let mut seen: Vec<bool> = Vec::new();

    for _ in 0..remaining {
        let mut next = Vec::new();
        for &pos in &current_positions {
            next.extend(match_node(inner, input, pos, budget));
        }
        next.sort_unstable();
        next.dedup();
        if next.is_empty() {
            break;
        }

        if seen.is_empty() {
            // First productive iteration: build the bitmap and seed it with the
            // end-positions already in `results`.
            seen = vec![false; input.len() + 1];
            for &p in &results {
                if p < seen.len() {
                    seen[p] = true;
                }
            }
        }

        let mut added = false;
        for &p in &next {
            // Bounds check is defensive: inner matchers should never
            // return a position > input.len(), but a future regression
            // shouldn't be able to panic the matcher.
            if p < seen.len() && !seen[p] {
                seen[p] = true;
                results.push(p);
                added = true;
            }
        }
        if !added {
            // No new end-positions reachable — saturated.
            break;
        }
        current_positions = next;
    }

    // `results` is built in insertion order, which interleaves positions
    // from different `current_positions` branches. Sort once at the end
    // so downstream alternation / sequence de-duplication sees a tidy
    // result. O(N log N) in the worst case, dwarfed by the saved O(N^2).
    results.sort_unstable();
    results
}

// ─── Character class matching ────────────────────────────────────────────────

/// Every general category, as a set (see [`ClassMember::Categories`]).
const ALL_CATEGORIES: u32 = (1 << 30) - 1;

/// The set holding `cat` alone.
fn category_bit(cat: GeneralCategory) -> u32 {
    1 << cat as u32
}

/// The set of the categories of one group letter (`L`, `M`, `N`, `P`, `S`,
/// `Z` or `C`).
fn group_bits(group: char) -> u32 {
    use GeneralCategory::*;
    let members: &[GeneralCategory] = match group {
        'L' => &[Lu, Ll, Lt, Lm, Lo],
        'M' => &[Mn, Mc, Me],
        'N' => &[Nd, Nl, No],
        'P' => &[Pc, Pd, Ps, Pe, Pi, Pf, Po],
        'S' => &[Sm, Sc, Sk, So],
        'Z' => &[Zs, Zl, Zp],
        'C' => &[Cc, Cf, Cs, Co, Cn],
        _ => &[],
    };
    members.iter().fold(0, |set, &cat| set | category_bit(cat))
}

/// The categories `member` stands for, or `None` when it is not defined by
/// general categories (a character, a range, `\s`, `\i`, `\c`, a block).
fn member_categories(member: &ClassMember) -> Option<u32> {
    let not_word = group_bits('P') | group_bits('Z') | group_bits('C');
    match member {
        ClassMember::Escape(CharEscape::Digit) => Some(category_bit(GeneralCategory::Nd)),
        ClassMember::Escape(CharEscape::NotDigit) => {
            Some(ALL_CATEGORIES & !category_bit(GeneralCategory::Nd))
        }
        ClassMember::Escape(CharEscape::Word) => Some(ALL_CATEGORIES & !not_word),
        ClassMember::Escape(CharEscape::NotWord) => Some(not_word),
        ClassMember::Property(prop) => {
            let set = match prop.class {
                PropertyClass::Category(cat) => category_bit(cat),
                PropertyClass::Group(group) => group_bits(group),
                PropertyClass::Block(_) => return None,
            };
            Some(if prop.negated {
                ALL_CATEGORIES & !set
            } else {
                set
            })
        }
        _ => None,
    }
}

/// Build a character class. When two or more members are defined by general
/// categories, they are folded into one [`ClassMember::Categories`] set at the
/// end, so matching looks a character's category up once per class, not once
/// per member. A single such member keeps its own path (`\d` keeps its ASCII
/// fast path).
fn char_class(
    negated: bool,
    members: Vec<ClassMember>,
    subtraction: Option<Box<CharClass>>,
) -> CharClass {
    let foldable = members
        .iter()
        .filter(|m| member_categories(m).is_some())
        .count();
    if foldable < 2 {
        return CharClass {
            negated,
            members,
            subtraction,
        };
    }
    let mut categories = 0;
    let mut kept = Vec::with_capacity(members.len());
    for member in members {
        match member_categories(&member) {
            Some(set) => categories |= set,
            None => kept.push(member),
        }
    }
    if categories != 0 {
        kept.push(ClassMember::Categories(categories));
    }
    CharClass {
        negated,
        members: kept,
        subtraction,
    }
}

/// A run of code points that share one general category: `first..=last`.
#[derive(Clone, Copy)]
struct CategoryRun {
    first: u32,
    last: u32,
    category: GeneralCategory,
}

impl CategoryRun {
    /// A run that holds no code point.
    const EMPTY: CategoryRun = CategoryRun {
        first: 1,
        last: 0,
        category: GeneralCategory::Cn,
    };

    fn contains(&self, c: u32) -> bool {
        self.first <= c && c <= self.last
    }
}

/// A character being tested against a character class. Its general category
/// is looked up at most once, however many members of the class (and of its
/// subtractions) ask for it, and not at all while the character stays in the
/// run of the previous lookup.
struct ClassChar<'a> {
    ch: char,
    category: Option<GeneralCategory>,
    run: &'a mut CategoryRun,
}

impl<'a> ClassChar<'a> {
    fn new(ch: char, run: &'a mut CategoryRun) -> Self {
        ClassChar {
            ch,
            category: None,
            run,
        }
    }

    fn category(&mut self) -> GeneralCategory {
        if let Some(cat) = self.category {
            return cat;
        }
        let c = self.ch as u32;
        let cat = if let Some(&cat) = ASCII_CATEGORIES.get(c as usize) {
            cat
        } else {
            if !self.run.contains(c) {
                *self.run = category_run(c);
            }
            self.run.category
        };
        self.category = Some(cat);
        cat
    }

    /// XSD `\d`: `\p{Nd}`, the decimal digits of every script.
    fn is_decimal_digit(&mut self) -> bool {
        if self.ch.is_ascii() {
            return self.ch.is_ascii_digit();
        }
        self.category() == GeneralCategory::Nd
    }

    /// XSD `\w`: `[#x0000-#x10FFFF]-[\p{P}\p{Z}\p{C}]`, every character except
    /// punctuation, separators and "other" characters (controls, format,
    /// private use and unassigned).
    fn is_word_char(&mut self) -> bool {
        !matches!(category_group(self.category()), 'P' | 'Z' | 'C')
    }
}

fn char_class_matches(cc: &CharClass, ch: char, run: &mut CategoryRun) -> bool {
    class_matches(cc, &mut ClassChar::new(ch, run))
}

fn class_matches(cc: &CharClass, c: &mut ClassChar) -> bool {
    let mut matches = if cc.negated {
        !any_member_matches(&cc.members, c)
    } else {
        any_member_matches(&cc.members, c)
    };

    if let Some(ref sub) = cc.subtraction {
        if class_matches(sub, c) {
            matches = false;
        }
    }

    matches
}

fn any_member_matches(members: &[ClassMember], c: &mut ClassChar) -> bool {
    let ch = c.ch;
    for member in members {
        match member {
            ClassMember::Char(m) => {
                if ch == *m {
                    return true;
                }
            }
            ClassMember::Range(start, end) => {
                if ch >= *start && ch <= *end {
                    return true;
                }
            }
            ClassMember::Escape(esc) => {
                if escape_matches(*esc, c) {
                    return true;
                }
            }
            ClassMember::Property(prop) => {
                if property_matches(prop, c) {
                    return true;
                }
            }
            ClassMember::Categories(set) => {
                if set & category_bit(c.category()) != 0 {
                    return true;
                }
            }
            ClassMember::Nested(inner) => {
                if class_matches(inner, c) {
                    return true;
                }
            }
        }
    }
    false
}

fn escape_matches(esc: CharEscape, c: &mut ClassChar) -> bool {
    let ch = c.ch;
    match esc {
        CharEscape::Digit => c.is_decimal_digit(),
        CharEscape::NotDigit => !c.is_decimal_digit(),
        CharEscape::Space => matches!(ch, ' ' | '\t' | '\n' | '\r'),
        CharEscape::NotSpace => !matches!(ch, ' ' | '\t' | '\n' | '\r'),
        CharEscape::Word => c.is_word_char(),
        CharEscape::NotWord => !c.is_word_char(),
        CharEscape::XmlInitial => is_xml_initial(ch),
        CharEscape::NotXmlInitial => !is_xml_initial(ch),
        CharEscape::XmlNameChar => is_xml_name_char(ch),
        CharEscape::NotXmlNameChar => !is_xml_name_char(ch),
    }
}

/// The run of one general category that holds `c`, a non-ASCII code point: a
/// binary search of the runs of `c`'s 256-code-point page, which the page
/// index narrows to a few runs (often one). No allocation, no recursion, and
/// every index is clamped into the table.
fn category_run(c: u32) -> CategoryRun {
    let len = CATEGORY_RUNS.len();
    let page = (c >> 8) as usize;
    let lo = CATEGORY_PAGES
        .get(page)
        .map_or(0, |&i| usize::from(i))
        .min(len - 1);
    let hi = CATEGORY_PAGES
        .get(page + 1)
        .map_or(len - 1, |&i| usize::from(i))
        .clamp(lo, len - 1);
    // The run at `lo` holds the page's first code point (or U+0080), which is
    // at most `c`, so at least one run of the slice starts at or before `c`.
    let i = CATEGORY_RUNS[lo..=hi].partition_point(|&(start, _)| start <= c);
    let j = lo + i.max(1) - 1;
    let (first, category) = CATEGORY_RUNS[j];
    let last = CATEGORY_RUNS
        .get(j + 1)
        .map_or(0x10FFFF, |&(next, _)| next - 1);
    CategoryRun {
        first,
        last,
        category,
    }
}

/// The general category of `ch`, without a run cache.
#[cfg(test)]
fn general_category(ch: char) -> GeneralCategory {
    let mut run = CategoryRun::EMPTY;
    ClassChar::new(ch, &mut run).category()
}

/// The one-letter group of a general category (`L`, `M`, `N`, `P`, `S`, `Z`
/// or `C`).
fn category_group(cat: GeneralCategory) -> char {
    use GeneralCategory::*;
    match cat {
        Lu | Ll | Lt | Lm | Lo => 'L',
        Mn | Mc | Me => 'M',
        Nd | Nl | No => 'N',
        Pc | Pd | Ps | Pe | Pi | Pf | Po => 'P',
        Sm | Sc | Sk | So => 'S',
        Zs | Zl | Zp => 'Z',
        Cc | Cf | Cs | Co | Cn => 'C',
    }
}

/// Whether `c` lies in one of `ranges`, which are sorted, disjoint and
/// inclusive: a binary search.
fn in_ranges(ranges: &[(u32, u32)], c: u32) -> bool {
    let i = ranges.partition_point(|&(first, _)| first <= c);
    ranges
        .get(i.wrapping_sub(1))
        .is_some_and(|&(_, last)| c <= last)
}

/// Whether `c` lies in one of `ranges` (all in the BMP), using `pages`, their
/// page index, to narrow the binary search to the ranges that meet `c`'s
/// 256-code-point page. A code point beyond the BMP is in none of them.
fn in_paged_ranges(ranges: &[(u32, u32)], pages: &[u16; 257], c: u32) -> bool {
    let page = (c >> 8) as usize;
    let (Some(&lo), Some(&hi)) = (pages.get(page), pages.get(page + 1)) else {
        return false;
    };
    let hi = (usize::from(hi) + 1).min(ranges.len());
    ranges
        .get(usize::from(lo)..hi)
        .is_some_and(|candidates| in_ranges(candidates, c))
}

/// XSD `\i`: XML 1.0 (Second Edition) `Letter | '_' | ':'`.
fn is_xml_initial(ch: char) -> bool {
    if ch.is_ascii() {
        return ch.is_ascii_alphabetic() || ch == '_' || ch == ':';
    }
    in_paged_ranges(&XML_NAME_START_RANGES, &XML_NAME_START_PAGES, ch as u32)
}

/// XSD `\c`: XML 1.0 (Second Edition) `NameChar`.
fn is_xml_name_char(ch: char) -> bool {
    if ch.is_ascii() {
        return ch.is_ascii_alphanumeric() || matches!(ch, '_' | ':' | '.' | '-');
    }
    in_paged_ranges(&XML_NAME_RANGES, &XML_NAME_PAGES, ch as u32)
}

/// Match Unicode property \p{...} or \P{...}.
fn property_matches(prop: &UnicodeProperty, c: &mut ClassChar) -> bool {
    let base_match = match prop.class {
        PropertyClass::Category(cat) => c.category() == cat,
        PropertyClass::Group(group) => category_group(c.category()) == group,
        PropertyClass::Block(ranges) => in_ranges(ranges, c.ch as u32),
    };
    if prop.negated {
        !base_match
    } else {
        base_match
    }
}

/// Resolve the name inside `\p{..}` or `\P{..}`. Returns `None` when `name` is
/// not a recognized property, so the pattern is refused at compile time:
/// otherwise `\p{unknown}` would match nothing and `\P{unknown}` *every*
/// character, silently widening a pattern (a validation bypass).
///
/// The recognized set is **closed**: the general categories of XSD 1.0 Part 2
/// (Appendix F.1) `IsCategory`, which exclude `Cs`, and the `IsBlock` names of
/// [`XSD_BLOCKS`].
fn resolve_property(name: &str) -> Option<PropertyClass> {
    use GeneralCategory::*;
    if let Some(block) = name.strip_prefix("Is") {
        return XSD_BLOCKS
            .iter()
            .find(|(block_name, _)| *block_name == block)
            .map(|&(_, ranges)| PropertyClass::Block(ranges));
    }
    let category = match name {
        "L" | "M" | "N" | "P" | "S" | "Z" | "C" => {
            return name.chars().next().map(PropertyClass::Group);
        }
        "Lu" => Lu,
        "Ll" => Ll,
        "Lt" => Lt,
        "Lm" => Lm,
        "Lo" => Lo,
        "Mn" => Mn,
        "Mc" => Mc,
        "Me" => Me,
        "Nd" => Nd,
        "Nl" => Nl,
        "No" => No,
        "Pc" => Pc,
        "Pd" => Pd,
        "Ps" => Ps,
        "Pe" => Pe,
        "Pi" => Pi,
        "Pf" => Pf,
        "Po" => Po,
        "Sm" => Sm,
        "Sc" => Sc,
        "Sk" => Sk,
        "So" => So,
        "Zs" => Zs,
        "Zl" => Zl,
        "Zp" => Zp,
        "Cc" => Cc,
        "Cf" => Cf,
        "Co" => Co,
        "Cn" => Cn,
        _ => return None,
    };
    Some(PropertyClass::Category(category))
}

// ─── Unicode Block matching ─────────────────────────────────────────────────

/// The `IsBlock` names of XSD 1.0 and their code points: Unicode block names
/// with the white space removed.
///
/// The first part is the table of XSD 1.0 Part 2 (Second Edition), Appendix
/// F.1, verbatim: the BMP blocks of Unicode 3.1, with `Specials` in two
/// ranges. The rest are the other blocks of Unicode 3.1, the version of the
/// Unicode Database that edition references and whose blocks a minimally
/// conforming processor must support: the three surrogate blocks, which the
/// table leaves out and no character can match, and the supplementary blocks.
/// `PrivateUse` keeps the table's single BMP range, although Unicode 3.1 also
/// gives that name to planes 15 and 16. Last come 20 names outside that list
/// which earlier releases accepted, with the ranges they used; they are kept so
/// that existing schemas still build. No other name is recognized, and no later
/// range replaces the XSD 1.0 ones.
static XSD_BLOCKS: [(&str, &[(u32, u32)]); 116] = [
    ("BasicLatin", &[(0x0000, 0x007F)]),
    ("Latin-1Supplement", &[(0x0080, 0x00FF)]),
    ("LatinExtended-A", &[(0x0100, 0x017F)]),
    ("LatinExtended-B", &[(0x0180, 0x024F)]),
    ("IPAExtensions", &[(0x0250, 0x02AF)]),
    ("SpacingModifierLetters", &[(0x02B0, 0x02FF)]),
    ("CombiningDiacriticalMarks", &[(0x0300, 0x036F)]),
    ("Greek", &[(0x0370, 0x03FF)]),
    ("Cyrillic", &[(0x0400, 0x04FF)]),
    ("Armenian", &[(0x0530, 0x058F)]),
    ("Hebrew", &[(0x0590, 0x05FF)]),
    ("Arabic", &[(0x0600, 0x06FF)]),
    ("Syriac", &[(0x0700, 0x074F)]),
    ("Thaana", &[(0x0780, 0x07BF)]),
    ("Devanagari", &[(0x0900, 0x097F)]),
    ("Bengali", &[(0x0980, 0x09FF)]),
    ("Gurmukhi", &[(0x0A00, 0x0A7F)]),
    ("Gujarati", &[(0x0A80, 0x0AFF)]),
    ("Oriya", &[(0x0B00, 0x0B7F)]),
    ("Tamil", &[(0x0B80, 0x0BFF)]),
    ("Telugu", &[(0x0C00, 0x0C7F)]),
    ("Kannada", &[(0x0C80, 0x0CFF)]),
    ("Malayalam", &[(0x0D00, 0x0D7F)]),
    ("Sinhala", &[(0x0D80, 0x0DFF)]),
    ("Thai", &[(0x0E00, 0x0E7F)]),
    ("Lao", &[(0x0E80, 0x0EFF)]),
    ("Tibetan", &[(0x0F00, 0x0FFF)]),
    ("Myanmar", &[(0x1000, 0x109F)]),
    ("Georgian", &[(0x10A0, 0x10FF)]),
    ("HangulJamo", &[(0x1100, 0x11FF)]),
    ("Ethiopic", &[(0x1200, 0x137F)]),
    ("Cherokee", &[(0x13A0, 0x13FF)]),
    ("UnifiedCanadianAboriginalSyllabics", &[(0x1400, 0x167F)]),
    ("Ogham", &[(0x1680, 0x169F)]),
    ("Runic", &[(0x16A0, 0x16FF)]),
    ("Khmer", &[(0x1780, 0x17FF)]),
    ("Mongolian", &[(0x1800, 0x18AF)]),
    ("LatinExtendedAdditional", &[(0x1E00, 0x1EFF)]),
    ("GreekExtended", &[(0x1F00, 0x1FFF)]),
    ("GeneralPunctuation", &[(0x2000, 0x206F)]),
    ("SuperscriptsandSubscripts", &[(0x2070, 0x209F)]),
    ("CurrencySymbols", &[(0x20A0, 0x20CF)]),
    ("CombiningMarksforSymbols", &[(0x20D0, 0x20FF)]),
    ("LetterlikeSymbols", &[(0x2100, 0x214F)]),
    ("NumberForms", &[(0x2150, 0x218F)]),
    ("Arrows", &[(0x2190, 0x21FF)]),
    ("MathematicalOperators", &[(0x2200, 0x22FF)]),
    ("MiscellaneousTechnical", &[(0x2300, 0x23FF)]),
    ("ControlPictures", &[(0x2400, 0x243F)]),
    ("OpticalCharacterRecognition", &[(0x2440, 0x245F)]),
    ("EnclosedAlphanumerics", &[(0x2460, 0x24FF)]),
    ("BoxDrawing", &[(0x2500, 0x257F)]),
    ("BlockElements", &[(0x2580, 0x259F)]),
    ("GeometricShapes", &[(0x25A0, 0x25FF)]),
    ("MiscellaneousSymbols", &[(0x2600, 0x26FF)]),
    ("Dingbats", &[(0x2700, 0x27BF)]),
    ("BraillePatterns", &[(0x2800, 0x28FF)]),
    ("CJKRadicalsSupplement", &[(0x2E80, 0x2EFF)]),
    ("KangxiRadicals", &[(0x2F00, 0x2FDF)]),
    ("IdeographicDescriptionCharacters", &[(0x2FF0, 0x2FFF)]),
    ("CJKSymbolsandPunctuation", &[(0x3000, 0x303F)]),
    ("Hiragana", &[(0x3040, 0x309F)]),
    ("Katakana", &[(0x30A0, 0x30FF)]),
    ("Bopomofo", &[(0x3100, 0x312F)]),
    ("HangulCompatibilityJamo", &[(0x3130, 0x318F)]),
    ("Kanbun", &[(0x3190, 0x319F)]),
    ("BopomofoExtended", &[(0x31A0, 0x31BF)]),
    ("EnclosedCJKLettersandMonths", &[(0x3200, 0x32FF)]),
    ("CJKCompatibility", &[(0x3300, 0x33FF)]),
    ("CJKUnifiedIdeographsExtensionA", &[(0x3400, 0x4DB5)]),
    ("CJKUnifiedIdeographs", &[(0x4E00, 0x9FFF)]),
    ("YiSyllables", &[(0xA000, 0xA48F)]),
    ("YiRadicals", &[(0xA490, 0xA4CF)]),
    ("HangulSyllables", &[(0xAC00, 0xD7A3)]),
    ("PrivateUse", &[(0xE000, 0xF8FF)]),
    ("CJKCompatibilityIdeographs", &[(0xF900, 0xFAFF)]),
    ("AlphabeticPresentationForms", &[(0xFB00, 0xFB4F)]),
    ("ArabicPresentationForms-A", &[(0xFB50, 0xFDFF)]),
    ("CombiningHalfMarks", &[(0xFE20, 0xFE2F)]),
    ("CJKCompatibilityForms", &[(0xFE30, 0xFE4F)]),
    ("SmallFormVariants", &[(0xFE50, 0xFE6F)]),
    ("ArabicPresentationForms-B", &[(0xFE70, 0xFEFE)]),
    ("Specials", &[(0xFEFF, 0xFEFF), (0xFFF0, 0xFFFD)]),
    ("HalfwidthandFullwidthForms", &[(0xFF00, 0xFFEF)]),
    // The other Unicode 3.1 blocks.
    ("HighSurrogates", &[(0xD800, 0xDB7F)]),
    ("HighPrivateUseSurrogates", &[(0xDB80, 0xDBFF)]),
    ("LowSurrogates", &[(0xDC00, 0xDFFF)]),
    ("OldItalic", &[(0x10300, 0x1032F)]),
    ("Gothic", &[(0x10330, 0x1034F)]),
    ("Deseret", &[(0x10400, 0x1044F)]),
    ("ByzantineMusicalSymbols", &[(0x1D000, 0x1D0FF)]),
    ("MusicalSymbols", &[(0x1D100, 0x1D1FF)]),
    ("MathematicalAlphanumericSymbols", &[(0x1D400, 0x1D7FF)]),
    ("CJKUnifiedIdeographsExtensionB", &[(0x20000, 0x2A6D6)]),
    (
        "CJKCompatibilityIdeographsSupplement",
        &[(0x2F800, 0x2FA1F)],
    ),
    ("Tags", &[(0xE0000, 0xE007F)]),
    // Names outside XSD 1.0's list that earlier releases of this crate accepted,
    // with the ranges they used, kept so that existing schemas still build.
    ("GreekandCoptic", &[(0x0370, 0x03FF)]),
    ("CyrillicSupplement", &[(0x0500, 0x052F)]),
    ("Tagalog", &[(0x1700, 0x171F)]),
    ("Hanunoo", &[(0x1720, 0x173F)]),
    ("Buhid", &[(0x1740, 0x175F)]),
    ("Tagbanwa", &[(0x1760, 0x177F)]),
    ("Limbu", &[(0x1900, 0x194F)]),
    ("TaiLe", &[(0x1950, 0x197F)]),
    ("KhmerSymbols", &[(0x19E0, 0x19FF)]),
    ("PhoneticExtensions", &[(0x1D00, 0x1D7F)]),
    ("CombiningDiacriticalMarksforSymbols", &[(0x20D0, 0x20FF)]),
    ("MiscellaneousMathematicalSymbols-A", &[(0x27C0, 0x27EF)]),
    ("SupplementalArrows-A", &[(0x27F0, 0x27FF)]),
    ("SupplementalArrows-B", &[(0x2900, 0x297F)]),
    ("MiscellaneousMathematicalSymbols-B", &[(0x2980, 0x29FF)]),
    ("SupplementalMathematicalOperators", &[(0x2A00, 0x2AFF)]),
    ("KatakanaPhoneticExtensions", &[(0x31F0, 0x31FF)]),
    ("YijingHexagramSymbols", &[(0x4DC0, 0x4DFF)]),
    ("PrivateUseArea", &[(0xE000, 0xF8FF)]),
    ("VariationSelectors", &[(0xFE00, 0xFE0F)]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_literal() {
        let re = XsdRegex::compile("abc").unwrap();
        assert!(re.is_match("abc"));
        assert!(!re.is_match("ab"));
        assert!(!re.is_match("abcd"));
    }

    #[test]
    fn test_dot() {
        let re = XsdRegex::compile("a.c").unwrap();
        assert!(re.is_match("abc"));
        assert!(re.is_match("axc"));
        assert!(!re.is_match("ac"));
    }

    #[test]
    fn test_char_class() {
        let re = XsdRegex::compile("[abc]").unwrap();
        assert!(re.is_match("a"));
        assert!(re.is_match("b"));
        assert!(!re.is_match("d"));
    }

    #[test]
    fn test_char_range() {
        let re = XsdRegex::compile("[0-9]").unwrap();
        assert!(re.is_match("5"));
        assert!(!re.is_match("a"));
    }

    #[test]
    fn test_negated_class() {
        let re = XsdRegex::compile("[^a-z]").unwrap();
        assert!(re.is_match("5"));
        assert!(!re.is_match("a"));
    }

    #[test]
    fn test_quantifier_star() {
        let re = XsdRegex::compile("a*").unwrap();
        assert!(re.is_match(""));
        assert!(re.is_match("aaa"));
        assert!(!re.is_match("b"));
    }

    #[test]
    fn test_quantifier_plus() {
        let re = XsdRegex::compile("a+").unwrap();
        assert!(!re.is_match(""));
        assert!(re.is_match("a"));
        assert!(re.is_match("aaa"));
    }

    #[test]
    fn test_quantifier_question() {
        let re = XsdRegex::compile("ab?c").unwrap();
        assert!(re.is_match("ac"));
        assert!(re.is_match("abc"));
        assert!(!re.is_match("abbc"));
    }

    #[test]
    fn test_quantifier_exact() {
        let re = XsdRegex::compile("[0-9]{3}").unwrap();
        assert!(re.is_match("123"));
        assert!(!re.is_match("12"));
        assert!(!re.is_match("1234"));
    }

    #[test]
    fn test_quantifier_range() {
        let re = XsdRegex::compile("[0-9]{2,4}").unwrap();
        assert!(!re.is_match("1"));
        assert!(re.is_match("12"));
        assert!(re.is_match("123"));
        assert!(re.is_match("1234"));
        assert!(!re.is_match("12345"));
    }

    #[test]
    fn test_alternation() {
        let re = XsdRegex::compile("cat|dog").unwrap();
        assert!(re.is_match("cat"));
        assert!(re.is_match("dog"));
        assert!(!re.is_match("catdog"));
    }

    #[test]
    fn test_group() {
        let re = XsdRegex::compile("(ab)+").unwrap();
        assert!(re.is_match("ab"));
        assert!(re.is_match("abab"));
        assert!(!re.is_match("a"));
    }

    #[test]
    fn test_digit_escape() {
        let re = XsdRegex::compile("\\d{3}").unwrap();
        assert!(re.is_match("123"));
        assert!(!re.is_match("abc"));
    }

    #[test]
    fn test_space_escape() {
        let re = XsdRegex::compile("a\\sb").unwrap();
        assert!(re.is_match("a b"));
        assert!(re.is_match("a\tb"));
        assert!(!re.is_match("ab"));
    }

    #[test]
    fn test_xml_initial() {
        let re = XsdRegex::compile("\\i\\c*").unwrap();
        assert!(re.is_match("foo"));
        assert!(re.is_match("_bar"));
        assert!(!re.is_match("1bar"));
    }

    #[test]
    fn test_hex_pattern() {
        let re = XsdRegex::compile("[0-9A-Fa-f]{2}").unwrap();
        assert!(re.is_match("FF"));
        assert!(re.is_match("0a"));
        assert!(!re.is_match("GG"));
        assert!(!re.is_match("F"));
    }

    #[test]
    fn test_nmtokens_pattern() {
        let re = XsdRegex::compile("[A-C]{0,2}").unwrap();
        assert!(re.is_match(""));
        assert!(re.is_match("A"));
        assert!(re.is_match("AB"));
        assert!(!re.is_match("ABC"));
    }

    #[test]
    fn test_decimal_pattern() {
        let re = XsdRegex::compile("\\d{1}").unwrap();
        assert!(re.is_match("3"));
        assert!(!re.is_match("33"));
    }

    #[test]
    fn test_escaped_literal() {
        let re = XsdRegex::compile("\\-\\d{3}").unwrap();
        assert!(re.is_match("-123"));
        assert!(!re.is_match("123"));
    }

    #[test]
    fn test_complex_nist_pattern() {
        // Pattern from NIST anyURI tests
        let re = XsdRegex::compile("\\c{3,6}://(\\c{1,7}\\.){1,2}\\c{3}").unwrap();
        assert!(re.is_match("gopher://Sty.reques.org"));
    }

    #[test]
    fn test_char_class_subtraction() {
        let re = XsdRegex::compile("[a-z-[aeiou]]").unwrap();
        assert!(re.is_match("b"));
        assert!(re.is_match("c"));
        assert!(!re.is_match("a"));
        assert!(!re.is_match("e"));
    }

    #[test]
    fn test_newline_escape() {
        let re = XsdRegex::compile("a\\nb").unwrap();
        assert!(re.is_match("a\nb"));
        assert!(!re.is_match("ab"));
    }

    /// F-04: deeply nested `(...)` groups must be rejected cleanly
    /// instead of stack-overflowing the recursive-descent parser.
    #[test]
    fn test_group_depth_cap_rejects_deep_nesting() {
        let mut pat = String::new();
        for _ in 0..500 {
            pat.push('(');
        }
        pat.push('a');
        for _ in 0..500 {
            pat.push(')');
        }
        let err = XsdRegex::compile(&pat).expect_err("deep nesting must be rejected");
        assert!(
            err.contains("maximum depth"),
            "expected depth-cap error, got: {}",
            err
        );
    }

    /// F-04: same guard for character-class subtraction nesting.
    #[test]
    fn test_class_subtraction_depth_cap() {
        let mut pat = String::new();
        for _ in 0..500 {
            pat.push_str("[a-");
        }
        pat.push_str("[a-z]");
        for _ in 0..500 {
            pat.push(']');
        }
        let err =
            XsdRegex::compile(&pat).expect_err("deep class-subtraction nesting must be rejected");
        assert!(
            err.contains("maximum depth"),
            "expected depth-cap error, got: {}",
            err
        );
    }

    /// Legitimate nesting well under the cap still compiles.
    #[test]
    fn test_moderate_group_nesting_still_compiles() {
        // 10 levels of nesting is common in real schemas.
        let mut pat = String::new();
        for _ in 0..10 {
            pat.push('(');
        }
        pat.push('a');
        for _ in 0..10 {
            pat.push(')');
        }
        let re = XsdRegex::compile(&pat).expect("10-deep nesting must compile");
        assert!(re.is_match("a"));
    }

    /// F-1 (review follow-up): custom cap via `compile_with_max_depth`
    /// must fire at the configured value.
    #[test]
    fn test_compile_with_custom_max_depth() {
        // 10-deep pattern: cap of 5 rejects, cap of 20 accepts.
        let mut pat = String::new();
        for _ in 0..10 {
            pat.push('(');
        }
        pat.push('a');
        for _ in 0..10 {
            pat.push(')');
        }
        assert!(
            XsdRegex::compile_with_max_depth(&pat, 5).is_err(),
            "cap of 5 must reject 10-deep pattern"
        );
        let re = XsdRegex::compile_with_max_depth(&pat, 20)
            .expect("cap of 20 must admit 10-deep pattern");
        assert!(re.is_match("a"));
    }

    /// F-05: polynomial ReDoS. The classic catastrophic-backtracking
    /// shape `(a*)*b` against a long string of `a`s (no trailing `b`)
    /// makes the backtracking matcher explore every way to partition
    /// the `a`s, which is O(n^3) or worse. Asserted deterministically
    /// against the step budget rather than wall-clock — a tight cap on
    /// `is_match_with_max_steps` must produce a fail-closed result no
    /// matter how slow / parallel-loaded the host is.
    #[test]
    fn test_polynomial_redos_fails_closed_with_step_budget() {
        let re = XsdRegex::compile("(a*)*b").expect("compile");
        let input: String = "a".repeat(500);
        // Genuine no-match (no trailing 'b'): correct under any budget.
        assert!(
            !re.is_match(&input),
            "input does not end with 'b', must not match"
        );
        // A tight step budget must fail closed even for the
        // catastrophic-backtracking shape — any other outcome means
        // the budget didn't fire.
        assert!(
            !re.is_match_with_max_steps(&input, 1),
            "tight step budget must fail closed for pathological backtracking"
        );
    }

    /// Legitimate simple patterns still match well within budget.
    #[test]
    fn test_normal_match_unaffected_by_budget() {
        let re = XsdRegex::compile("[a-z]+[0-9]+").expect("compile");
        assert!(re.is_match("abc123"));
        assert!(!re.is_match("abc"));
        assert!(!re.is_match("123"));
    }

    /// F-1 (review follow-up): custom step budget via
    /// `is_match_with_max_steps` must fire at the configured value.
    #[test]
    fn test_is_match_with_custom_budget() {
        let re = XsdRegex::compile("(a*)*b").expect("compile");
        let input: String = "a".repeat(200);
        // A tight budget should fail to find the match (fail-closed).
        assert!(!re.is_match_with_max_steps(&input, 1_000));
        // A very generous budget still fails (genuine no-match), but
        // without hitting the cap.
        assert!(!re.is_match_with_max_steps(&input, 10_000_000));
    }

    /// F-1: legitimate linear pattern against a large input must
    /// still match under the default (scaled) budget. Pre-F-1 the
    /// constant 1M-step cap caused false-rejects once input exceeded
    /// ~1 million chars.
    #[test]
    fn test_large_legitimate_input_matches() {
        let re = XsdRegex::compile("[a-z]+").expect("compile");
        let input: String = "a".repeat(2_000_000);
        assert!(
            re.is_match(&input),
            "2-million-char legitimate input must match under the \
             input-scaled default budget"
        );
    }

    /// `{n,m}` quantifier with `m < n` must be rejected at compile time
    /// rather than panicking inside `match_repetition` on the `m - n`
    /// underflow.
    #[test]
    fn test_brace_quantifier_rejects_max_below_min() {
        let err = XsdRegex::compile("a{5,3}").expect_err("max<min must be rejected");
        assert!(
            err.contains("max < min"),
            "expected max<min error, got: {}",
            err
        );
        // Equal min/max stays accepted.
        assert!(XsdRegex::compile("a{3,3}").is_ok());
        // Normal range stays accepted.
        assert!(XsdRegex::compile("a{2,5}").is_ok());
    }

    /// F-2: exercise `match_repetition` on a substantial linear-pattern
    /// input. The bitmap accumulator keeps this O(N log N); a regression
    /// to the old O(N^2 log N) shape would balloon CPU but the timing
    /// thresholds were flaky on slow / loaded CI runners. Assert
    /// correctness only — `test_large_legitimate_input_matches` already
    /// covers the 2-million-char path under the input-scaled budget,
    /// which is the meaningful regression net.
    #[test]
    fn test_match_repetition_large_linear_input_matches() {
        let re = XsdRegex::compile("[a-z]+").expect("compile");
        let input: String = "a".repeat(100_000);
        assert!(
            re.is_match(&input),
            "100K-char linear-pattern input must match"
        );
    }

    // ─── Generated tables ───────────────────────────────────────────────────

    /// The tables are well formed: runs start at U+0080, ascend strictly and
    /// change category at every entry; name ranges are sorted and disjoint.
    #[test]
    fn test_generated_tables_are_well_formed() {
        assert_eq!(CATEGORY_RUNS[0].0, 0x80);
        for pair in CATEGORY_RUNS.windows(2) {
            assert!(pair[0].0 < pair[1].0 && pair[0].1 != pair[1].1, "{pair:?}");
        }
        assert!(CATEGORY_RUNS.last().unwrap().0 <= 0x10FFFF);
        for (page, pair) in CATEGORY_PAGES.windows(2).enumerate() {
            let (first, next) = (usize::from(pair[0]), usize::from(pair[1]));
            assert!(first <= next, "page {page:#X}");
            assert!(
                CATEGORY_RUNS[first].0 <= ((page as u32) << 8).max(0x80),
                "page {page:#X}"
            );
        }
        assert_eq!(
            usize::from(*CATEGORY_PAGES.last().unwrap()),
            CATEGORY_RUNS.len() - 1
        );
        for table in [&XML_NAME_START_RANGES[..], &XML_NAME_RANGES[..]] {
            for pair in table.windows(2) {
                assert!(
                    pair[0].0 <= pair[0].1 && pair[0].1 + 1 < pair[1].0,
                    "{pair:?}"
                );
            }
        }
    }

    /// The lookup returns the table's category for every code point, both
    /// fresh and through a run cache carried from one code point to the next
    /// (in order, and stepping backwards across every run boundary).
    #[test]
    fn test_general_category_agrees_with_the_table_everywhere() {
        for (cp, &cat) in ASCII_CATEGORIES.iter().enumerate() {
            assert_eq!(general_category(char::from(cp as u8)), cat);
        }
        let mut run = CategoryRun::EMPTY;
        for (i, &(start, cat)) in CATEGORY_RUNS.iter().enumerate() {
            let end = CATEGORY_RUNS.get(i + 1).map_or(0x10FFFF, |next| next.0 - 1);
            for ch in (start..=end).filter_map(char::from_u32) {
                assert_eq!(general_category(ch), cat, "U+{:04X}", ch as u32);
                let cached = ClassChar::new(ch, &mut run).category();
                assert_eq!(cached, cat, "cached U+{:04X}", ch as u32);
            }
            if let Some(before) = char::from_u32(start - 1) {
                let previous = ClassChar::new(before, &mut run).category();
                assert_eq!(previous, general_category(before), "U+{:04X}", start - 1);
            }
        }
    }

    /// Every run of decimal digits: its first and last code points match
    /// `\d` and `\p{Nd}`, and the code points just outside it do not (runs
    /// are maximal, so a neighbour is never `Nd`).
    #[test]
    fn test_every_nd_run_matches_d_at_both_ends() {
        let compile = |p: &str| XsdRegex::compile(p).unwrap();
        let (d, not_d, nd) = (compile(r"\d"), compile(r"\D"), compile(r"\p{Nd}"));
        let mut runs = vec![(0x30, 0x39)];
        for (i, &(start, cat)) in CATEGORY_RUNS.iter().enumerate() {
            if cat == GeneralCategory::Nd {
                runs.push((start, CATEGORY_RUNS[i + 1].0 - 1));
            }
        }
        assert!(runs.len() > 60, "{} runs", runs.len());
        for (first, last) in runs {
            for cp in [first, last] {
                let s = char::from_u32(cp).unwrap().to_string();
                assert!(d.is_match(&s) && nd.is_match(&s), "U+{cp:04X}");
                assert!(!not_d.is_match(&s), "U+{cp:04X}");
            }
            for cp in [first - 1, last + 1] {
                let s = char::from_u32(cp).unwrap().to_string();
                assert!(!d.is_match(&s) && !nd.is_match(&s), "U+{cp:04X}");
                assert!(not_d.is_match(&s), "U+{cp:04X}");
            }
        }
    }

    /// `\w` at both ends of every run: a word character exactly when the
    /// run's category is outside P, Z and C.
    #[test]
    fn test_w_at_both_ends_of_every_run() {
        let w = XsdRegex::compile(r"\w").unwrap();
        let not_w = XsdRegex::compile(r"\W").unwrap();
        for (i, &(start, cat)) in CATEGORY_RUNS.iter().enumerate() {
            let end = CATEGORY_RUNS.get(i + 1).map_or(0x10FFFF, |next| next.0 - 1);
            let word = !matches!(category_group(cat), 'P' | 'Z' | 'C');
            for ch in [start, end].into_iter().filter_map(char::from_u32) {
                let s = ch.to_string();
                assert_eq!(w.is_match(&s), word, "U+{:04X} {cat:?}", ch as u32);
                assert_eq!(not_w.is_match(&s), !word, "U+{:04X} {cat:?}", ch as u32);
            }
        }
    }

    /// A class whose category members are folded into one set matches exactly
    /// the code points the same members match one by one.
    #[test]
    fn test_folded_category_members_match_like_the_members() {
        let bodies = [
            r"\p{Lu}\p{Ll}\p{Nd}\i",
            r"\w\p{IsCyrillic}\-\p{Sm}",
            r"\W\d",
            r"\P{L}\p{Nd}",
            r"\p{N}\P{Lu}a-z",
            r"\D\s\p{Lu}",
            r"\p{Zs}\p{Cc}\p{Co}\p{Cn}\p{Pc}",
        ];
        for body in bodies {
            let chars: Vec<char> = body.chars().collect();
            let mut members = Vec::new();
            parse_class_members(&chars, &mut 0, &mut members).unwrap();
            for negated in [false, true] {
                let plain = CharClass {
                    negated,
                    members: members.clone(),
                    subtraction: None,
                };
                let folded = char_class(negated, members.clone(), None);
                assert!(
                    folded
                        .members
                        .iter()
                        .any(|m| matches!(m, ClassMember::Categories(_))),
                    "{body}"
                );
                let mut run = CategoryRun::EMPTY;
                for ch in (0u32..=0x10FFFF).filter_map(char::from_u32) {
                    let mut fresh = CategoryRun::EMPTY;
                    assert_eq!(
                        class_matches(&folded, &mut ClassChar::new(ch, &mut run)),
                        class_matches(&plain, &mut ClassChar::new(ch, &mut fresh)),
                        "{body} negated={negated} U+{:04X}",
                        ch as u32
                    );
                }
            }
        }
    }

    /// The page-indexed name lookups give what a search of the whole table
    /// gives, for every code point.
    #[test]
    fn test_paged_name_lookup_agrees_with_the_full_tables() {
        for cp in 0u32..=0x10FFFF {
            assert_eq!(
                in_paged_ranges(&XML_NAME_START_RANGES, &XML_NAME_START_PAGES, cp),
                in_ranges(&XML_NAME_START_RANGES, cp),
                "U+{cp:04X}"
            );
            assert_eq!(
                in_paged_ranges(&XML_NAME_RANGES, &XML_NAME_PAGES, cp),
                in_ranges(&XML_NAME_RANGES, cp),
                "U+{cp:04X}"
            );
        }
    }

    /// The ASCII fast paths give what the tables give.
    #[test]
    fn test_ascii_fast_paths_agree_with_the_tables() {
        for cp in 0u32..0x80 {
            let ch = char::from_u32(cp).unwrap();
            assert_eq!(
                is_xml_initial(ch),
                in_ranges(&XML_NAME_START_RANGES, cp),
                "{ch:?}"
            );
            assert_eq!(
                is_xml_name_char(ch),
                in_ranges(&XML_NAME_RANGES, cp),
                "{ch:?}"
            );
            let nd = ASCII_CATEGORIES[cp as usize] == GeneralCategory::Nd;
            let mut run = CategoryRun::EMPTY;
            assert_eq!(
                ClassChar::new(ch, &mut run).is_decimal_digit(),
                nd,
                "{ch:?}"
            );
        }
    }
}
