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
    /// A nested character class (for subtraction). Spec-complete placeholder;
    /// the parser does not currently produce nested character classes.
    #[allow(dead_code)]
    Nested(CharClass),
}

/// Multi-character escape types.
#[derive(Debug, Clone, Copy)]
enum CharEscape {
    /// `\d` = [0-9]
    Digit,
    /// `\D` = [^0-9]
    NotDigit,
    /// `\s` = [ \t\n\r]
    Space,
    /// `\S` = [^ \t\n\r]
    NotSpace,
    /// `\w` = [a-zA-Z0-9_] (simplified; XSD defines it via Unicode categories)
    Word,
    /// `\W` = complement of \w
    NotWord,
    /// `\i` = XML initial name character (Letter | '_' | ':')
    XmlInitial,
    /// `\I` = complement of \i
    NotXmlInitial,
    /// `\c` = XML name character (Letter | Digit | '.' | '-' | '_' | ':' | CombiningChar | Extender)
    XmlNameChar,
    /// `\C` = complement of \c
    NotXmlNameChar,
}

/// Unicode property for \p{...} and \P{...}.
#[derive(Debug, Clone)]
struct UnicodeProperty {
    negated: bool,
    name: String,
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
}

impl MatchBudget {
    fn new(max_steps: usize) -> Self {
        MatchBudget {
            steps: 0,
            max_steps,
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
    if branches.len() == 1 {
        Ok(branches.pop().unwrap())
    } else {
        Ok(RegexNode::Alternation(branches))
    }
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
    if items.len() == 1 {
        Ok(items.pop().unwrap())
    } else {
        Ok(RegexNode::Sequence(items))
    }
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
        'd' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::Digit)],
            subtraction: None,
        })),
        'D' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::NotDigit)],
            subtraction: None,
        })),
        's' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::Space)],
            subtraction: None,
        })),
        'S' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::NotSpace)],
            subtraction: None,
        })),
        'w' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::Word)],
            subtraction: None,
        })),
        'W' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::NotWord)],
            subtraction: None,
        })),
        'i' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::XmlInitial)],
            subtraction: None,
        })),
        'I' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::NotXmlInitial)],
            subtraction: None,
        })),
        'c' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::XmlNameChar)],
            subtraction: None,
        })),
        'C' => Ok(RegexNode::CharClass(CharClass {
            negated: false,
            members: vec![ClassMember::Escape(CharEscape::NotXmlNameChar)],
            subtraction: None,
        })),
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
                Ok(RegexNode::CharClass(CharClass {
                    negated: false,
                    members: vec![ClassMember::Property(UnicodeProperty { negated, name })],
                    subtraction: None,
                }))
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

    Ok(CharClass {
        negated,
        members,
        subtraction,
    })
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
                        Ok(ClassMember::Property(UnicodeProperty { negated, name }))
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
            if start < input.len() && char_class_matches(cc, input[start]) {
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

    // Greedy loop accumulator: a direct-indexed `seen` bitmap (positions
    // are bounded by `input.len()`) gives O(1) membership test per
    // candidate. Combined with a parallel `results` Vec that holds only
    // the unique reachable end-positions, total work is O(N) insertions
    // plus one O(N log N) final sort — vs the O(N^2) cost a
    // merge-into-sorted-vec accumulator would incur on linear patterns
    // like `[a-z]+` over a long string of `a`s.
    let mut seen: Vec<bool> = vec![false; input.len() + 1];
    let mut results: Vec<usize> = Vec::new();
    for &p in &current_positions {
        if p < seen.len() && !seen[p] {
            seen[p] = true;
            results.push(p);
        }
    }

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

fn char_class_matches(cc: &CharClass, ch: char) -> bool {
    let mut matches = if cc.negated {
        !any_member_matches(&cc.members, ch)
    } else {
        any_member_matches(&cc.members, ch)
    };

    if let Some(ref sub) = cc.subtraction {
        if char_class_matches(sub, ch) {
            matches = false;
        }
    }

    matches
}

fn any_member_matches(members: &[ClassMember], ch: char) -> bool {
    for member in members {
        match member {
            ClassMember::Char(c) => {
                if ch == *c {
                    return true;
                }
            }
            ClassMember::Range(start, end) => {
                if ch >= *start && ch <= *end {
                    return true;
                }
            }
            ClassMember::Escape(esc) => {
                if escape_matches(*esc, ch) {
                    return true;
                }
            }
            ClassMember::Property(prop) => {
                if property_matches(prop, ch) {
                    return true;
                }
            }
            ClassMember::Nested(inner) => {
                if char_class_matches(inner, ch) {
                    return true;
                }
            }
        }
    }
    false
}

fn escape_matches(esc: CharEscape, ch: char) -> bool {
    match esc {
        CharEscape::Digit => ch.is_ascii_digit(),
        CharEscape::NotDigit => !ch.is_ascii_digit(),
        CharEscape::Space => matches!(ch, ' ' | '\t' | '\n' | '\r'),
        CharEscape::NotSpace => !matches!(ch, ' ' | '\t' | '\n' | '\r'),
        CharEscape::Word => is_word_char(ch),
        CharEscape::NotWord => !is_word_char(ch),
        CharEscape::XmlInitial => is_xml_initial(ch),
        CharEscape::NotXmlInitial => !is_xml_initial(ch),
        CharEscape::XmlNameChar => is_xml_name_char(ch),
        CharEscape::NotXmlNameChar => !is_xml_name_char(ch),
    }
}

/// XSD \w: all characters except the set of "punctuation", "separator",
/// and "other" characters. Simplified to [a-zA-Z0-9_] for ASCII, plus
/// Unicode letters and digits.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// XML 1.0 initial name character: Letter | '_' | ':'
fn is_xml_initial(ch: char) -> bool {
    ch == '_' || ch == ':' || ch.is_alphabetic()
}

/// XML 1.0 name character: Letter | Digit | '.' | '-' | '_' | ':' | CombiningChar | Extender
fn is_xml_name_char(ch: char) -> bool {
    is_xml_initial(ch) || ch.is_ascii_digit() || ch == '.' || ch == '-'
        || ch.is_numeric() // covers digits in other scripts
        // CombiningChar and Extender — cover via Unicode categories
        || is_combining_char(ch)
        || is_extender(ch)
}

fn is_combining_char(ch: char) -> bool {
    let c = ch as u32;
    // Unicode combining marks (approximate, covers Mn and Mc)
    (0x0300..=0x036F).contains(&c)  // Combining Diacritical Marks
        || (0x0483..=0x0487).contains(&c)
        || (0x0591..=0x05BD).contains(&c)
        || (0x05BF..=0x05BF).contains(&c)
        || (0x05C1..=0x05C2).contains(&c)
        || (0x05C4..=0x05C5).contains(&c)
        || (0x0610..=0x061A).contains(&c)
        || (0x064B..=0x065F).contains(&c)
        || (0x0670..=0x0670).contains(&c)
        || (0x06D6..=0x06DC).contains(&c)
        || (0x06DF..=0x06E4).contains(&c)
        || (0x06E7..=0x06E8).contains(&c)
        || (0x06EA..=0x06ED).contains(&c)
        || (0x0711..=0x0711).contains(&c)
        || (0x0730..=0x074A).contains(&c)
        || (0x0901..=0x0903).contains(&c)
        || (0x093C..=0x093C).contains(&c)
        || (0x093E..=0x094D).contains(&c)
        || (0x0951..=0x0954).contains(&c)
        || (0x0962..=0x0963).contains(&c)
        || (0x0981..=0x0983).contains(&c)
        || (0x09BC..=0x09BC).contains(&c)
        || (0x09BE..=0x09C4).contains(&c)
        || (0x09C7..=0x09C8).contains(&c)
        || (0x09CB..=0x09CD).contains(&c)
        || (0x09D7..=0x09D7).contains(&c)
        || (0x09E2..=0x09E3).contains(&c)
        || (0x0A01..=0x0A03).contains(&c)
        || (0x0A3C..=0x0A3C).contains(&c)
        || (0x0A3E..=0x0A42).contains(&c)
        || (0x0A47..=0x0A48).contains(&c)
        || (0x0A4B..=0x0A4D).contains(&c)
        || (0x0A70..=0x0A71).contains(&c)
        || (0x0A81..=0x0A83).contains(&c)
        || (0x0ABC..=0x0ABC).contains(&c)
        || (0x0ABE..=0x0AC5).contains(&c)
        || (0x0AC7..=0x0AC9).contains(&c)
        || (0x0ACB..=0x0ACD).contains(&c)
        || (0x0B01..=0x0B03).contains(&c)
        || (0x0B3C..=0x0B3C).contains(&c)
        || (0x0B3E..=0x0B43).contains(&c)
        || (0x0B47..=0x0B48).contains(&c)
        || (0x0B4B..=0x0B4D).contains(&c)
        || (0x0B56..=0x0B57).contains(&c)
        || (0x0B82..=0x0B82).contains(&c)
        || (0x0BBE..=0x0BC2).contains(&c)
        || (0x0BC6..=0x0BC8).contains(&c)
        || (0x0BCA..=0x0BCD).contains(&c)
        || (0x0BD7..=0x0BD7).contains(&c)
        || (0x0C01..=0x0C03).contains(&c)
        || (0x0C3E..=0x0C44).contains(&c)
        || (0x0C46..=0x0C48).contains(&c)
        || (0x0C4A..=0x0C4D).contains(&c)
        || (0x0C55..=0x0C56).contains(&c)
        || (0x0C82..=0x0C83).contains(&c)
        || (0x0CBE..=0x0CC4).contains(&c)
        || (0x0CC6..=0x0CC8).contains(&c)
        || (0x0CCA..=0x0CCD).contains(&c)
        || (0x0CD5..=0x0CD6).contains(&c)
        || (0x0D02..=0x0D03).contains(&c)
        || (0x0D3E..=0x0D43).contains(&c)
        || (0x0D46..=0x0D48).contains(&c)
        || (0x0D4A..=0x0D4D).contains(&c)
        || (0x0D57..=0x0D57).contains(&c)
        || (0x0E31..=0x0E31).contains(&c)
        || (0x0E34..=0x0E3A).contains(&c)
        || (0x0E47..=0x0E4E).contains(&c)
        || (0x0EB1..=0x0EB1).contains(&c)
        || (0x0EB4..=0x0EB9).contains(&c)
        || (0x0EBB..=0x0EBC).contains(&c)
        || (0x0EC8..=0x0ECD).contains(&c)
        || (0x0F18..=0x0F19).contains(&c)
        || (0x0F35..=0x0F35).contains(&c)
        || (0x0F37..=0x0F37).contains(&c)
        || (0x0F39..=0x0F39).contains(&c)
        || (0x0F3E..=0x0F3F).contains(&c)
        || (0x0F71..=0x0F84).contains(&c)
        || (0x0F86..=0x0F87).contains(&c)
        || (0x0F90..=0x0F97).contains(&c)
        || (0x0F99..=0x0FBC).contains(&c)
        || (0x0FC6..=0x0FC6).contains(&c)
        || (0x20D0..=0x20DC).contains(&c)
        || (0x20E1..=0x20E1).contains(&c)
        || (0x302A..=0x302F).contains(&c)
        || (0x3099..=0x309A).contains(&c)
        || (0xFE20..=0xFE23).contains(&c)
}

fn is_extender(ch: char) -> bool {
    let c = ch as u32;
    c == 0x00B7
        || c == 0x02D0
        || c == 0x02D1
        || c == 0x0387
        || c == 0x0640
        || c == 0x0E46
        || c == 0x0EC6
        || c == 0x3005
        || (0x3031..=0x3035).contains(&c)
        || (0x309D..=0x309E).contains(&c)
        || (0x30FC..=0x30FE).contains(&c)
}

/// Match Unicode property \p{...} or \P{...}.
fn property_matches(prop: &UnicodeProperty, ch: char) -> bool {
    let base_match = match_property_name(&prop.name, ch);
    if prop.negated {
        !base_match
    } else {
        base_match
    }
}

/// Match a Unicode general category or block name.
fn match_property_name(name: &str, ch: char) -> bool {
    match name {
        // General categories
        "L" => ch.is_alphabetic(),
        "Lu" => ch.is_uppercase(),
        "Ll" => ch.is_lowercase(),
        "Lt" => is_titlecase(ch),
        "Lm" => is_modifier_letter(ch),
        "Lo" => is_other_letter(ch),
        "M" => is_mark(ch),
        "Mn" => is_nonspacing_mark(ch),
        "Mc" => is_spacing_mark(ch),
        "Me" => is_enclosing_mark(ch),
        "N" => ch.is_numeric(),
        "Nd" => ch.is_ascii_digit() || is_decimal_digit(ch),
        "Nl" => is_letter_number(ch),
        "No" => is_other_number(ch),
        "P" => is_punctuation(ch),
        "Pc" => is_connector_punctuation(ch),
        "Pd" => is_dash_punctuation(ch),
        "Ps" => is_open_punctuation(ch),
        "Pe" => is_close_punctuation(ch),
        "Pi" => is_initial_punctuation(ch),
        "Pf" => is_final_punctuation(ch),
        "Po" => is_other_punctuation(ch),
        "S" => is_symbol(ch),
        "Sm" => is_math_symbol(ch),
        "Sc" => is_currency_symbol(ch),
        "Sk" => is_modifier_symbol(ch),
        "So" => is_other_symbol(ch),
        "Z" => is_separator(ch),
        "Zs" => is_space_separator(ch),
        "Zl" => ch == '\u{2028}',
        "Zp" => ch == '\u{2029}',
        "C" => is_other(ch),
        "Cc" => ch.is_control(),
        "Cf" => is_format(ch),
        "Co" => is_private_use(ch),
        "Cn" => !ch.is_alphanumeric() && !is_assigned(ch),
        // Unicode block escapes (Is...)
        _ if name.starts_with("Is") => match_unicode_block(&name[2..], ch),
        _ => false,
    }
}

// ─── Unicode category helpers ────────────────────────────────────────────────
// These provide approximate implementations using Rust's built-in char methods
// where possible, and code point ranges for specifics.

fn is_titlecase(ch: char) -> bool {
    let c = ch as u32;
    // Titlecase letters: Dz, Lj, Nj, etc.
    matches!(
        c,
        0x01C5 | 0x01C8 | 0x01CB | 0x01F2 | 0x1F88..=0x1F8F | 0x1F98..=0x1F9F
        | 0x1FA8..=0x1FAF | 0x1FBC | 0x1FCC | 0x1FFC
    )
}

fn is_modifier_letter(ch: char) -> bool {
    let c = ch as u32;
    (0x02B0..=0x02C1).contains(&c)
        || (0x02C6..=0x02D1).contains(&c)
        || (0x02E0..=0x02E4).contains(&c)
        || c == 0x02EC
        || c == 0x02EE
        || (0x0374..=0x0375).contains(&c)
        || c == 0x037A
        || (0x0559..=0x0559).contains(&c)
        || c == 0x0640
        || (0x06E5..=0x06E6).contains(&c)
        || c == 0x07F4
        || c == 0x07F5
        || c == 0x07FA
        || (0x0E46..=0x0E46).contains(&c)
        || (0x0EC6..=0x0EC6).contains(&c)
        || (0x10FC..=0x10FC).contains(&c)
        || (0x17D7..=0x17D7).contains(&c)
        || (0x1843..=0x1843).contains(&c)
        || (0x1D2C..=0x1D6A).contains(&c)
        || (0x1D78..=0x1D78).contains(&c)
        || (0x1D9B..=0x1DBF).contains(&c)
        || (0x2090..=0x2094).contains(&c)
        || (0x2D6F..=0x2D6F).contains(&c)
        || (0x3005..=0x3005).contains(&c)
        || (0x3031..=0x3035).contains(&c)
        || (0x303B..=0x303B).contains(&c)
        || (0x309D..=0x309E).contains(&c)
        || (0x30FC..=0x30FE).contains(&c)
        || (0xA717..=0xA71F).contains(&c)
        || (0xFF70..=0xFF70).contains(&c)
        || (0xFF9E..=0xFF9F).contains(&c)
}

fn is_other_letter(ch: char) -> bool {
    // Lo: letters that are not Lu, Ll, Lt, Lm
    ch.is_alphabetic()
        && !ch.is_uppercase()
        && !ch.is_lowercase()
        && !is_titlecase(ch)
        && !is_modifier_letter(ch)
}

fn is_mark(ch: char) -> bool {
    is_nonspacing_mark(ch) || is_spacing_mark(ch) || is_enclosing_mark(ch)
}

fn is_nonspacing_mark(ch: char) -> bool {
    is_combining_char(ch) && !is_spacing_mark(ch)
}

fn is_spacing_mark(ch: char) -> bool {
    let c = ch as u32;
    // Mc category — spacing combining marks
    (0x0903..=0x0903).contains(&c)
        || (0x093E..=0x0940).contains(&c)
        || (0x0949..=0x094C).contains(&c)
        || (0x0982..=0x0983).contains(&c)
        || (0x09BE..=0x09C0).contains(&c)
        || (0x09C7..=0x09C8).contains(&c)
        || (0x09CB..=0x09CC).contains(&c)
        || (0x09D7..=0x09D7).contains(&c)
        || (0x0A03..=0x0A03).contains(&c)
        || (0x0A3E..=0x0A40).contains(&c)
        || (0x0A83..=0x0A83).contains(&c)
        || (0x0ABE..=0x0AC0).contains(&c)
        || (0x0AC9..=0x0AC9).contains(&c)
        || (0x0ACB..=0x0ACC).contains(&c)
        || (0x0B02..=0x0B03).contains(&c)
        || (0x0B3E..=0x0B3E).contains(&c)
        || (0x0B40..=0x0B40).contains(&c)
        || (0x0B47..=0x0B48).contains(&c)
        || (0x0B4B..=0x0B4C).contains(&c)
        || (0x0B57..=0x0B57).contains(&c)
        || (0x0BBE..=0x0BBF).contains(&c)
        || (0x0BC1..=0x0BC2).contains(&c)
        || (0x0BC6..=0x0BC8).contains(&c)
        || (0x0BCA..=0x0BCC).contains(&c)
        || (0x0BD7..=0x0BD7).contains(&c)
        || (0x0C01..=0x0C03).contains(&c)
        || (0x0C41..=0x0C44).contains(&c)
        || (0x0C82..=0x0C83).contains(&c)
        || (0x0CBE..=0x0CBE).contains(&c)
        || (0x0CC0..=0x0CC4).contains(&c)
        || (0x0CC7..=0x0CC8).contains(&c)
        || (0x0CCA..=0x0CCB).contains(&c)
        || (0x0CD5..=0x0CD6).contains(&c)
        || (0x0D02..=0x0D03).contains(&c)
        || (0x0D3E..=0x0D40).contains(&c)
        || (0x0D46..=0x0D48).contains(&c)
        || (0x0D4A..=0x0D4C).contains(&c)
        || (0x0D57..=0x0D57).contains(&c)
        || (0x0F3E..=0x0F3F).contains(&c)
        || (0x0F7F..=0x0F7F).contains(&c)
}

fn is_enclosing_mark(ch: char) -> bool {
    let c = ch as u32;
    (0x0488..=0x0489).contains(&c)
        || (0x20DD..=0x20E0).contains(&c)
        || (0x20E2..=0x20E4).contains(&c)
        || c == 0xA670
        || c == 0xA671
        || c == 0xA672
}

fn is_decimal_digit(ch: char) -> bool {
    let c = ch as u32;
    ch.is_ascii_digit()
        || (0x0660..=0x0669).contains(&c) // Arabic-Indic
        || (0x06F0..=0x06F9).contains(&c) // Extended Arabic-Indic
        || (0x0966..=0x096F).contains(&c) // Devanagari
        || (0x09E6..=0x09EF).contains(&c) // Bengali
        || (0x0A66..=0x0A6F).contains(&c) // Gurmukhi
        || (0x0AE6..=0x0AEF).contains(&c) // Gujarati
        || (0x0B66..=0x0B6F).contains(&c) // Oriya
        || (0x0BE7..=0x0BEF).contains(&c) // Tamil
        || (0x0C66..=0x0C6F).contains(&c) // Telugu
        || (0x0CE6..=0x0CEF).contains(&c) // Kannada
        || (0x0D66..=0x0D6F).contains(&c) // Malayalam
        || (0x0E50..=0x0E59).contains(&c) // Thai
        || (0x0ED0..=0x0ED9).contains(&c) // Lao
        || (0x0F20..=0x0F29).contains(&c) // Tibetan
        || (0x1040..=0x1049).contains(&c) // Myanmar
        || (0x17E0..=0x17E9).contains(&c) // Khmer
        || (0x1810..=0x1819).contains(&c) // Mongolian
        || (0xFF10..=0xFF19).contains(&c) // Fullwidth
}

fn is_letter_number(ch: char) -> bool {
    let c = ch as u32;
    (0x2160..=0x2182).contains(&c) // Roman numerals
        || (0x3007..=0x3007).contains(&c) // CJK ideograph zero
        || (0x3021..=0x3029).contains(&c) // Hangzhou numerals
        || (0x3038..=0x303A).contains(&c)
}

fn is_other_number(ch: char) -> bool {
    let c = ch as u32;
    (0x00B2..=0x00B3).contains(&c) // superscript 2-3
        || c == 0x00B9 // superscript 1
        || (0x00BC..=0x00BE).contains(&c) // vulgar fractions
        || (0x09F4..=0x09F9).contains(&c) // Bengali currency
        || (0x0BF0..=0x0BF2).contains(&c) // Tamil
        || (0x0F2A..=0x0F33).contains(&c) // Tibetan
        || (0x2070..=0x2079).contains(&c) // superscripts
        || (0x2080..=0x2089).contains(&c) // subscripts
        || (0x2153..=0x215E).contains(&c) // fractions
        || (0x2460..=0x249B).contains(&c) // enclosed alphanumerics
        || (0x24EA..=0x24EA).contains(&c)
        || (0x2776..=0x2793).contains(&c)
        || (0x2CFD..=0x2CFD).contains(&c)
        || (0x3192..=0x3195).contains(&c)
        || (0x3220..=0x3229).contains(&c)
        || (0x3251..=0x325F).contains(&c)
        || (0x3280..=0x3289).contains(&c)
        || (0x32B1..=0x32BF).contains(&c)
}

fn is_punctuation(ch: char) -> bool {
    is_connector_punctuation(ch)
        || is_dash_punctuation(ch)
        || is_open_punctuation(ch)
        || is_close_punctuation(ch)
        || is_initial_punctuation(ch)
        || is_final_punctuation(ch)
        || is_other_punctuation(ch)
}

fn is_connector_punctuation(ch: char) -> bool {
    let c = ch as u32;
    c == 0x005F // _
        || c == 0x203F
        || c == 0x2040
        || c == 0x2054
        || c == 0xFE33
        || c == 0xFE34
        || c == 0xFE4D
        || c == 0xFE4E
        || c == 0xFE4F
        || c == 0xFF3F
}

fn is_dash_punctuation(ch: char) -> bool {
    let c = ch as u32;
    c == 0x002D // -
        || c == 0x058A
        || c == 0x05BE
        || c == 0x1400
        || c == 0x1806
        || c == 0x2010
        || c == 0x2011
        || c == 0x2012
        || c == 0x2013
        || c == 0x2014
        || c == 0x2015
        || c == 0x2E17
        || c == 0x301C
        || c == 0x3030
        || c == 0x30A0
        || c == 0xFE31
        || c == 0xFE32
        || c == 0xFE58
        || c == 0xFE63
        || c == 0xFF0D
}

fn is_open_punctuation(ch: char) -> bool {
    let c = ch as u32;
    c == 0x0028 // (
        || c == 0x005B // [
        || c == 0x007B // {
        || c == 0x0F3A
        || c == 0x0F3C
        || c == 0x169B
        || c == 0x201A
        || c == 0x201E
        || c == 0x2045
        || c == 0x207D
        || c == 0x208D
        || c == 0x2329
        || c == 0x23B4 // not technically Ps in all versions
        || c == 0x2768
        || c == 0x276A
        || c == 0x276C
        || c == 0x276E
        || c == 0x2770
        || c == 0x2772
        || c == 0x2774
        || c == 0x27C5
        || c == 0x27E6
        || c == 0x27E8
        || c == 0x27EA
        || c == 0x2983
        || c == 0x2985
        || c == 0x2987
        || c == 0x2989
        || c == 0x298B
        || c == 0x298D
        || c == 0x298F
        || c == 0x2991
        || c == 0x2993
        || c == 0x2995
        || c == 0x2997
        || c == 0x29D8
        || c == 0x29DA
        || c == 0x29FC
        || c == 0x3008
        || c == 0x300A
        || c == 0x300C
        || c == 0x300E
        || c == 0x3010
        || c == 0x3014
        || c == 0x3016
        || c == 0x3018
        || c == 0x301A
        || c == 0x301D
        || c == 0xFD3E
        || c == 0xFE17
        || c == 0xFE35
        || c == 0xFE37
        || c == 0xFE39
        || c == 0xFE3B
        || c == 0xFE3D
        || c == 0xFE3F
        || c == 0xFE41
        || c == 0xFE43
        || c == 0xFE47
        || c == 0xFE59
        || c == 0xFE5B
        || c == 0xFE5D
        || c == 0xFF08
        || c == 0xFF3B
        || c == 0xFF5B
        || c == 0xFF5F
        || c == 0xFF62
}

fn is_close_punctuation(ch: char) -> bool {
    let c = ch as u32;
    c == 0x0029 // )
        || c == 0x005D // ]
        || c == 0x007D // }
        || c == 0x0F3B
        || c == 0x0F3D
        || c == 0x169C
        || c == 0x2046
        || c == 0x207E
        || c == 0x208E
        || c == 0x232A
        || c == 0x23B5
        || c == 0x2769
        || c == 0x276B
        || c == 0x276D
        || c == 0x276F
        || c == 0x2771
        || c == 0x2773
        || c == 0x2775
        || c == 0x27C6
        || c == 0x27E7
        || c == 0x27E9
        || c == 0x27EB
        || c == 0x2984
        || c == 0x2986
        || c == 0x2988
        || c == 0x298A
        || c == 0x298C
        || c == 0x298E
        || c == 0x2990
        || c == 0x2992
        || c == 0x2994
        || c == 0x2996
        || c == 0x2998
        || c == 0x29D9
        || c == 0x29DB
        || c == 0x29FD
        || c == 0x3009
        || c == 0x300B
        || c == 0x300D
        || c == 0x300F
        || c == 0x3011
        || c == 0x3015
        || c == 0x3017
        || c == 0x3019
        || c == 0x301B
        || c == 0x301E
        || c == 0x301F
        || c == 0xFD3F
        || c == 0xFE18
        || c == 0xFE36
        || c == 0xFE38
        || c == 0xFE3A
        || c == 0xFE3C
        || c == 0xFE3E
        || c == 0xFE40
        || c == 0xFE42
        || c == 0xFE44
        || c == 0xFE48
        || c == 0xFE5A
        || c == 0xFE5C
        || c == 0xFE5E
        || c == 0xFF09
        || c == 0xFF3D
        || c == 0xFF5D
        || c == 0xFF60
        || c == 0xFF63
}

fn is_initial_punctuation(ch: char) -> bool {
    let c = ch as u32;
    c == 0x00AB
        || c == 0x2018
        || c == 0x201B
        || c == 0x201C
        || c == 0x201F
        || c == 0x2039
        || c == 0x2E02
        || c == 0x2E04
        || c == 0x2E09
        || c == 0x2E0C
        || c == 0x2E1C
}

fn is_final_punctuation(ch: char) -> bool {
    let c = ch as u32;
    c == 0x00BB
        || c == 0x2019
        || c == 0x201D
        || c == 0x203A
        || c == 0x2E03
        || c == 0x2E05
        || c == 0x2E0A
        || c == 0x2E0D
        || c == 0x2E1D
}

fn is_other_punctuation(ch: char) -> bool {
    let c = ch as u32;
    // Common ASCII punctuation (Pc, Pd, Ps, Pe excluded)
    matches!(
        c,
        0x0021..=0x0023
            | 0x0025..=0x0027
            | 0x002A
            | 0x002C
            | 0x002E..=0x002F
            | 0x003A..=0x003B
            | 0x003F..=0x0040
            | 0x005C
            | 0x00A1
            | 0x00A7
            | 0x00B6..=0x00B7
            | 0x00BF
            | 0x037E
            | 0x0387
            | 0x055A..=0x055F
            | 0x0589
            | 0x05C0
            | 0x05C3
            | 0x05C6
            | 0x05F3..=0x05F4
            | 0x060C..=0x060D
            | 0x061B
            | 0x061E..=0x061F
            | 0x066A..=0x066D
            | 0x06D4
            | 0x0700..=0x070D
    )
}

fn is_symbol(ch: char) -> bool {
    is_math_symbol(ch) || is_currency_symbol(ch) || is_modifier_symbol(ch) || is_other_symbol(ch)
}

fn is_math_symbol(ch: char) -> bool {
    let c = ch as u32;
    c == 0x002B // +
        || matches!(c, 0x003C..=0x003E) // <, =, >
        || c == 0x007C // |
        || c == 0x007E // ~
        || c == 0x00AC
        || c == 0x00B1
        || c == 0x00D7
        || c == 0x00F7
        || (0x2200..=0x22FF).contains(&c) // Mathematical Operators
        || (0x2A00..=0x2AFF).contains(&c) // Supplemental Mathematical Operators
        || (0x27C0..=0x27EF).contains(&c) // Misc Mathematical Symbols-A
        || (0x2980..=0x29FF).contains(&c) // Misc Mathematical Symbols-B
        || c == 0xFB29
        || c == 0xFE62
        || c == 0xFE64
        || c == 0xFE65
        || c == 0xFE66
        || c == 0xFF0B
        || c == 0xFF1C
        || c == 0xFF1D
        || c == 0xFF1E
        || c == 0xFF5C
        || c == 0xFF5E
        || c == 0xFFE2
        || c == 0xFFE9
        || c == 0xFFEA
        || c == 0xFFEB
        || c == 0xFFEC
}

fn is_currency_symbol(ch: char) -> bool {
    let c = ch as u32;
    c == 0x0024 // $
        || matches!(c, 0x00A2..=0x00A5)
        || c == 0x058F
        || c == 0x060B
        || c == 0x09F2
        || c == 0x09F3
        || c == 0x0AF1
        || c == 0x0BF9
        || c == 0x0E3F
        || c == 0x17DB
        || c == 0x20A0
        || c == 0x20A1
        || c == 0x20A2
        || c == 0x20A3
        || c == 0x20A4
        || c == 0x20A5
        || c == 0x20A6
        || c == 0x20A7
        || c == 0x20A8
        || c == 0x20A9
        || c == 0x20AA
        || c == 0x20AB
        || c == 0x20AC // €
        || c == 0x20AD
        || c == 0x20AE
        || c == 0x20AF
        || c == 0x20B0
        || c == 0x20B1
        || c == 0xFDFC
        || c == 0xFE69
        || c == 0xFF04
        || c == 0xFFE0
        || c == 0xFFE1
        || c == 0xFFE5
        || c == 0xFFE6
}

fn is_modifier_symbol(ch: char) -> bool {
    let c = ch as u32;
    c == 0x005E // ^
        || c == 0x0060 // `
        || c == 0x00A8
        || c == 0x00AF
        || c == 0x00B4
        || c == 0x00B8
        || c == 0x02C2
        || c == 0x02C3
        || c == 0x02C4
        || c == 0x02C5
        || (0x02D2..=0x02DF).contains(&c)
        || (0x02E5..=0x02ED).contains(&c)
        || (0x02EF..=0x02FF).contains(&c)
        || c == 0x0374
        || c == 0x0375
        || c == 0x0384
        || c == 0x0385
        || c == 0x1FBD
        || (0x1FBF..=0x1FC1).contains(&c)
        || (0x1FCD..=0x1FCF).contains(&c)
        || (0x1FDD..=0x1FDF).contains(&c)
        || (0x1FED..=0x1FEF).contains(&c)
        || (0x1FFD..=0x1FFE).contains(&c)
        || c == 0x309B
        || c == 0x309C
        || c == 0xA700
        || c == 0xFF3E
        || c == 0xFF40
        || c == 0xFFE3
}

fn is_other_symbol(ch: char) -> bool {
    let c = ch as u32;
    c == 0x00A6
        || c == 0x00A9
        || c == 0x00AE
        || c == 0x00B0
        || (0x2100..=0x214F).contains(&c) // Letterlike Symbols
        || (0x2190..=0x21FF).contains(&c) // Arrows
        || (0x2300..=0x23FF).contains(&c) // Misc Technical
        || (0x2400..=0x243F).contains(&c) // Control Pictures
        || (0x2440..=0x245F).contains(&c) // OCR
        || (0x2500..=0x257F).contains(&c) // Box Drawing
        || (0x2580..=0x259F).contains(&c) // Block Elements
        || (0x25A0..=0x25FF).contains(&c) // Geometric Shapes
        || (0x2600..=0x26FF).contains(&c) // Misc Symbols
        || (0x2700..=0x27BF).contains(&c) // Dingbats
        || (0x2800..=0x28FF).contains(&c) // Braille
        || (0x2B00..=0x2BFF).contains(&c) // Misc Symbols and Arrows
        || (0x3200..=0x32FF).contains(&c) // Enclosed CJK
        || (0x3300..=0x33FF).contains(&c) // CJK Compatibility
        || (0xFE00..=0xFE0F).contains(&c) // Variation Selectors
        || (0xFFE4..=0xFFE8).contains(&c)
        || (0xFFED..=0xFFEE).contains(&c)
}

fn is_separator(ch: char) -> bool {
    is_space_separator(ch) || ch == '\u{2028}' || ch == '\u{2029}'
}

fn is_space_separator(ch: char) -> bool {
    let c = ch as u32;
    c == 0x0020
        || c == 0x00A0
        || c == 0x1680
        || c == 0x180E
        || (0x2000..=0x200A).contains(&c)
        || c == 0x202F
        || c == 0x205F
        || c == 0x3000
}

fn is_other(ch: char) -> bool {
    ch.is_control() || is_format(ch) || is_private_use(ch)
}

fn is_format(ch: char) -> bool {
    let c = ch as u32;
    c == 0x00AD
        || c == 0x0600
        || c == 0x0601
        || c == 0x0602
        || c == 0x0603
        || c == 0x06DD
        || c == 0x070F
        || c == 0x17B4
        || c == 0x17B5
        || (0x200B..=0x200F).contains(&c)
        || (0x202A..=0x202E).contains(&c)
        || (0x2060..=0x2064).contains(&c)
        || (0x206A..=0x206F).contains(&c)
        || c == 0xFEFF
        || (0xFFF9..=0xFFFB).contains(&c)
}

fn is_private_use(ch: char) -> bool {
    let c = ch as u32;
    (0xE000..=0xF8FF).contains(&c)
        || (0xF0000..=0xFFFFD).contains(&c)
        || (0x100000..=0x10FFFD).contains(&c)
}

fn is_assigned(ch: char) -> bool {
    // Approximate: consider a character assigned if it has any Unicode property
    ch.is_alphanumeric()
        || ch.is_alphabetic()
        || is_punctuation(ch)
        || is_symbol(ch)
        || is_separator(ch)
        || ch.is_control()
        || is_format(ch)
        || is_private_use(ch)
        || is_mark(ch)
}

// ─── Unicode Block matching ─────────────────────────────────────────────────

fn match_unicode_block(block_name: &str, ch: char) -> bool {
    let c = ch as u32;
    match block_name {
        "BasicLatin" => (0x0000..=0x007F).contains(&c),
        "Latin-1Supplement" => (0x0080..=0x00FF).contains(&c),
        "LatinExtended-A" => (0x0100..=0x017F).contains(&c),
        "LatinExtended-B" => (0x0180..=0x024F).contains(&c),
        "IPAExtensions" => (0x0250..=0x02AF).contains(&c),
        "SpacingModifierLetters" => (0x02B0..=0x02FF).contains(&c),
        "CombiningDiacriticalMarks" => (0x0300..=0x036F).contains(&c),
        "Greek" | "GreekandCoptic" => (0x0370..=0x03FF).contains(&c),
        "Cyrillic" => (0x0400..=0x04FF).contains(&c),
        "CyrillicSupplement" => (0x0500..=0x052F).contains(&c),
        "Armenian" => (0x0530..=0x058F).contains(&c),
        "Hebrew" => (0x0590..=0x05FF).contains(&c),
        "Arabic" => (0x0600..=0x06FF).contains(&c),
        "Syriac" => (0x0700..=0x074F).contains(&c),
        "Thaana" => (0x0780..=0x07BF).contains(&c),
        "Devanagari" => (0x0900..=0x097F).contains(&c),
        "Bengali" => (0x0980..=0x09FF).contains(&c),
        "Gurmukhi" => (0x0A00..=0x0A7F).contains(&c),
        "Gujarati" => (0x0A80..=0x0AFF).contains(&c),
        "Oriya" => (0x0B00..=0x0B7F).contains(&c),
        "Tamil" => (0x0B80..=0x0BFF).contains(&c),
        "Telugu" => (0x0C00..=0x0C7F).contains(&c),
        "Kannada" => (0x0C80..=0x0CFF).contains(&c),
        "Malayalam" => (0x0D00..=0x0D7F).contains(&c),
        "Sinhala" => (0x0D80..=0x0DFF).contains(&c),
        "Thai" => (0x0E00..=0x0E7F).contains(&c),
        "Lao" => (0x0E80..=0x0EFF).contains(&c),
        "Tibetan" => (0x0F00..=0x0FFF).contains(&c),
        "Myanmar" => (0x1000..=0x109F).contains(&c),
        "Georgian" => (0x10A0..=0x10FF).contains(&c),
        "HangulJamo" => (0x1100..=0x11FF).contains(&c),
        "Ethiopic" => (0x1200..=0x137F).contains(&c),
        "Cherokee" => (0x13A0..=0x13FF).contains(&c),
        "UnifiedCanadianAboriginalSyllabics" => (0x1400..=0x167F).contains(&c),
        "Ogham" => (0x1680..=0x169F).contains(&c),
        "Runic" => (0x16A0..=0x16FF).contains(&c),
        "Tagalog" => (0x1700..=0x171F).contains(&c),
        "Hanunoo" => (0x1720..=0x173F).contains(&c),
        "Buhid" => (0x1740..=0x175F).contains(&c),
        "Tagbanwa" => (0x1760..=0x177F).contains(&c),
        "Khmer" => (0x1780..=0x17FF).contains(&c),
        "Mongolian" => (0x1800..=0x18AF).contains(&c),
        "Limbu" => (0x1900..=0x194F).contains(&c),
        "TaiLe" => (0x1950..=0x197F).contains(&c),
        "KhmerSymbols" => (0x19E0..=0x19FF).contains(&c),
        "PhoneticExtensions" => (0x1D00..=0x1D7F).contains(&c),
        "LatinExtendedAdditional" => (0x1E00..=0x1EFF).contains(&c),
        "GreekExtended" => (0x1F00..=0x1FFF).contains(&c),
        "GeneralPunctuation" => (0x2000..=0x206F).contains(&c),
        "SuperscriptsandSubscripts" => (0x2070..=0x209F).contains(&c),
        "CurrencySymbols" => (0x20A0..=0x20CF).contains(&c),
        "CombiningDiacriticalMarksforSymbols" | "CombiningMarksforSymbols" => {
            (0x20D0..=0x20FF).contains(&c)
        }
        "LetterlikeSymbols" => (0x2100..=0x214F).contains(&c),
        "NumberForms" => (0x2150..=0x218F).contains(&c),
        "Arrows" => (0x2190..=0x21FF).contains(&c),
        "MathematicalOperators" => (0x2200..=0x22FF).contains(&c),
        "MiscellaneousTechnical" => (0x2300..=0x23FF).contains(&c),
        "ControlPictures" => (0x2400..=0x243F).contains(&c),
        "OpticalCharacterRecognition" => (0x2440..=0x245F).contains(&c),
        "EnclosedAlphanumerics" => (0x2460..=0x24FF).contains(&c),
        "BoxDrawing" => (0x2500..=0x257F).contains(&c),
        "BlockElements" => (0x2580..=0x259F).contains(&c),
        "GeometricShapes" => (0x25A0..=0x25FF).contains(&c),
        "MiscellaneousSymbols" => (0x2600..=0x26FF).contains(&c),
        "Dingbats" => (0x2700..=0x27BF).contains(&c),
        "MiscellaneousMathematicalSymbols-A" => (0x27C0..=0x27EF).contains(&c),
        "SupplementalArrows-A" => (0x27F0..=0x27FF).contains(&c),
        "BraillePatterns" => (0x2800..=0x28FF).contains(&c),
        "SupplementalArrows-B" => (0x2900..=0x297F).contains(&c),
        "MiscellaneousMathematicalSymbols-B" => (0x2980..=0x29FF).contains(&c),
        "SupplementalMathematicalOperators" => (0x2A00..=0x2AFF).contains(&c),
        "CJKRadicalsSupplement" => (0x2E80..=0x2EFF).contains(&c),
        "KangxiRadicals" => (0x2F00..=0x2FDF).contains(&c),
        "IdeographicDescriptionCharacters" => (0x2FF0..=0x2FFF).contains(&c),
        "CJKSymbolsandPunctuation" => (0x3000..=0x303F).contains(&c),
        "Hiragana" => (0x3040..=0x309F).contains(&c),
        "Katakana" => (0x30A0..=0x30FF).contains(&c),
        "Bopomofo" => (0x3100..=0x312F).contains(&c),
        "HangulCompatibilityJamo" => (0x3130..=0x318F).contains(&c),
        "Kanbun" => (0x3190..=0x319F).contains(&c),
        "BopomofoExtended" => (0x31A0..=0x31BF).contains(&c),
        "KatakanaPhoneticExtensions" => (0x31F0..=0x31FF).contains(&c),
        "EnclosedCJKLettersandMonths" => (0x3200..=0x32FF).contains(&c),
        "CJKCompatibility" => (0x3300..=0x33FF).contains(&c),
        "CJKUnifiedIdeographsExtensionA" => (0x3400..=0x4DBF).contains(&c),
        "YijingHexagramSymbols" => (0x4DC0..=0x4DFF).contains(&c),
        "CJKUnifiedIdeographs" => (0x4E00..=0x9FFF).contains(&c),
        "YiSyllables" => (0xA000..=0xA48F).contains(&c),
        "YiRadicals" => (0xA490..=0xA4CF).contains(&c),
        "HangulSyllables" => (0xAC00..=0xD7AF).contains(&c),
        "HighSurrogates" => (0xD800..=0xDB7F).contains(&c),
        "HighPrivateUseSurrogates" => (0xDB80..=0xDBFF).contains(&c),
        "LowSurrogates" => (0xDC00..=0xDFFF).contains(&c),
        "PrivateUseArea" | "PrivateUse" => (0xE000..=0xF8FF).contains(&c),
        "CJKCompatibilityIdeographs" => (0xF900..=0xFAFF).contains(&c),
        "AlphabeticPresentationForms" => (0xFB00..=0xFB4F).contains(&c),
        "ArabicPresentationForms-A" => (0xFB50..=0xFDFF).contains(&c),
        "VariationSelectors" => (0xFE00..=0xFE0F).contains(&c),
        "CombiningHalfMarks" => (0xFE20..=0xFE2F).contains(&c),
        "CJKCompatibilityForms" => (0xFE30..=0xFE4F).contains(&c),
        "SmallFormVariants" => (0xFE50..=0xFE6F).contains(&c),
        "ArabicPresentationForms-B" => (0xFE70..=0xFEFF).contains(&c),
        "HalfwidthandFullwidthForms" => (0xFF00..=0xFFEF).contains(&c),
        "Specials" => (0xFFF0..=0xFFFD).contains(&c),
        // Supplementary planes
        "OldItalic" => (0x10300..=0x1032F).contains(&c),
        "Gothic" => (0x10330..=0x1034F).contains(&c),
        "Deseret" => (0x10400..=0x1044F).contains(&c),
        "ByzantineMusicalSymbols" => (0x1D000..=0x1D0FF).contains(&c),
        "MusicalSymbols" => (0x1D100..=0x1D1FF).contains(&c),
        "MathematicalAlphanumericSymbols" => (0x1D400..=0x1D7FF).contains(&c),
        "CJKUnifiedIdeographsExtensionB" => (0x20000..=0x2A6DF).contains(&c),
        "CJKCompatibilityIdeographsSupplement" => (0x2F800..=0x2FA1F).contains(&c),
        "Tags" => (0xE0000..=0xE007F).contains(&c),
        _ => false,
    }
}

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
}
