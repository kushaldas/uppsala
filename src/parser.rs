//! XML 1.0 (Fifth Edition) parser with well-formedness checking.
//!
//! This module implements a recursive-descent parser that tokenizes XML input
//! and builds a [`Document`] tree. It enforces the well-formedness constraints
//! defined in the XML 1.0 specification.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::dom::{Document, ProcessingInstruction, XmlDeclaration};
use crate::error::{XmlError, XmlResult};

/// A map of general entity names to their replacement text.
pub(crate) type EntityMap = HashMap<String, String>;

/// Cache of already-validated entity expansion results.
/// Key: entity name, Value: expanded text.
pub(crate) type EntityCache = HashMap<String, String>;

/// Default maximum element-nesting depth.
///
/// Sized well above legitimate XML (SOAP, XHTML, SVG, and XSLT documents in the
/// wild rarely exceed ~50 levels) while staying comfortably within a 2 MiB
/// thread stack (Rust's default worker-thread size) even under debug builds
/// where per-frame overhead is inflated. Exposed via [`Parser::with_max_depth`].
pub const DEFAULT_MAX_DEPTH: u32 = 128;

/// Default total budget for entity expansion (bytes written to expansion
/// buffers over the lifetime of a single `parse()` call).
///
/// 1 MiB is more than enough for every legitimate internal-subset entity
/// usage but far below the ~10⁹-byte output of a classical billion-laughs
/// attack. Callers who legitimately need more can raise it via
/// [`Parser::with_max_entity_expansion`].
pub const DEFAULT_MAX_ENTITY_EXPANSION: usize = 1 << 20;

/// Maximum nesting depth for entity replacement-text expansion.
///
/// Entity expansion recurses once per nested reference. The byte budget
/// ([`DEFAULT_MAX_ENTITY_EXPANSION`]) bounds total output but not recursion
/// depth: a long *linear* chain of distinct entities (`e0 -> e1 -> … -> eN`)
/// whose leaf is tiny expands to ~1 byte yet descends N frames, overflowing the
/// stack (an uncatchable abort). This cap fails such chains closed with a normal
/// error. The limit is far above any realistic document's entity nesting.
pub const DEFAULT_MAX_ENTITY_DEPTH: usize = 256;

/// The XML 1.0 parser.
pub struct Parser {
    /// Whether to resolve namespaces during parsing.
    namespace_aware: bool,
    /// Maximum allowed element-nesting depth. Enforced in `parse_element`
    /// to prevent stack overflow on maliciously deep input.
    max_depth: u32,
    /// Maximum total bytes of entity expansion per `parse()` call. Enforced
    /// in `expand_entity_value` and on `entity_cache` hits to block both
    /// billion-laughs (exponential nesting) and quadratic-blowup (single
    /// large entity referenced many times) attacks.
    max_entity_expansion: usize,
    /// When `true`, any `<!DOCTYPE` declaration is rejected at parse time
    /// instead of being parsed. Off by default. See [`Parser::with_forbid_dtd`].
    forbid_dtd: bool,
    /// When `true`, `<!ENTITY>` declarations (general and parameter) inside a
    /// DTD are rejected while the rest of the DTD is still parsed. Off by
    /// default. See [`Parser::with_forbid_entities`].
    forbid_entities: bool,
}

impl Parser {
    /// Create a new parser with namespace awareness enabled and the default
    /// safety limits ([`DEFAULT_MAX_DEPTH`], [`DEFAULT_MAX_ENTITY_EXPANSION`]).
    pub fn new() -> Self {
        Parser {
            namespace_aware: true,
            max_depth: DEFAULT_MAX_DEPTH,
            max_entity_expansion: DEFAULT_MAX_ENTITY_EXPANSION,
            forbid_dtd: false,
            forbid_entities: false,
        }
    }

    /// Create a new parser with configurable namespace awareness. Uses the
    /// default nesting-depth and entity-expansion caps.
    pub fn with_namespace_aware(namespace_aware: bool) -> Self {
        Parser {
            namespace_aware,
            max_depth: DEFAULT_MAX_DEPTH,
            max_entity_expansion: DEFAULT_MAX_ENTITY_EXPANSION,
            forbid_dtd: false,
            forbid_entities: false,
        }
    }

    /// Override the maximum element-nesting depth. Returns `self` so it can
    /// chain with other builder methods.
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Override the maximum total bytes of entity expansion per `parse()`
    /// call. Chains with other builder methods.
    pub fn with_max_entity_expansion(mut self, max_bytes: usize) -> Self {
        self.max_entity_expansion = max_bytes;
        self
    }

    /// Reject any `<!DOCTYPE` declaration at parse time instead of parsing the
    /// DTD internal subset. Off by default. Useful for security-sensitive
    /// consumers (SAML, XML-DSig) where DTDs are never legitimate. Chains with
    /// other builder methods.
    pub fn with_forbid_dtd(mut self, forbid: bool) -> Self {
        self.forbid_dtd = forbid;
        self
    }

    /// Reject `<!ENTITY>` declarations (general and parameter) inside a DTD,
    /// while still allowing the rest of the internal subset (`<!ELEMENT>`,
    /// `<!ATTLIST>`, `<!NOTATION>`). Off by default; implied when
    /// [`with_forbid_dtd`](Self::with_forbid_dtd) rejects the whole DOCTYPE.
    /// Chains with other builder methods.
    pub fn with_forbid_entities(mut self, forbid: bool) -> Self {
        self.forbid_entities = forbid;
        self
    }

    /// Parse an XML string into a [`Document`].
    ///
    /// Drives the shared tokenizer straight into the arena (see
    /// `pull::build_document`); no pull event is materialized per node.
    pub fn parse<'a>(&self, input: &'a str) -> XmlResult<Document<'a>> {
        let tokenizer = crate::pull::Tokenizer::new(input, self.namespace_aware)
            .with_max_depth(self.max_depth)
            .with_max_entity_expansion(self.max_entity_expansion)
            .with_forbid_dtd(self.forbid_dtd)
            .with_forbid_entities(self.forbid_entities);
        crate::pull::build_document(input, tokenizer)
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Cursor ─────────────────────────────────────────────

/// A cursor over the input string that tracks byte position.
/// Line/column are computed lazily from the byte position when needed
/// (error reporting, node creation) to avoid scanning every byte for newlines.
pub(crate) struct Cursor<'a> {
    pub(crate) input: &'a str,
    pub(crate) pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(input: &'a str) -> Self {
        Cursor { input, pos: 0 }
    }

    /// Compute line number (1-based) from current byte position.
    /// Only called in error paths and node allocation, not in the hot parse loop.
    #[inline(never)]
    pub(crate) fn line(&self) -> usize {
        self.input.as_bytes()[..self.pos]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1
    }

    /// Compute column number (1-based) from current byte position.
    #[inline(never)]
    pub(crate) fn column(&self) -> usize {
        let bytes = &self.input.as_bytes()[..self.pos];
        match bytes.iter().rposition(|&b| b == b'\n') {
            Some(nl_pos) => self.pos - nl_pos,
            None => self.pos + 1,
        }
    }

    pub(crate) fn is_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    pub(crate) fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    pub(crate) fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    /// Peek at the current byte without creating a char iterator.
    /// Much faster than peek() for ASCII-dominated XML content.
    #[inline(always)]
    pub(crate) fn peek_byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    pub(crate) fn starts_with(&self, prefix: &str) -> bool {
        self.remaining().starts_with(prefix)
    }

    /// Advance by n bytes.
    #[inline(always)]
    pub(crate) fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    /// Advance by n bytes (alias for advance, kept for compatibility).
    #[inline(always)]
    pub(crate) fn advance_no_newlines(&mut self, n: usize) {
        self.pos += n;
    }

    pub(crate) fn advance_char(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    pub(crate) fn skip_bom(&mut self) {
        if self.remaining().starts_with('\u{FEFF}') {
            self.pos += '\u{FEFF}'.len_utf8();
        }
    }

    pub(crate) fn skip_whitespace(&mut self) {
        let bytes = &self.input.as_bytes()[self.pos..];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b' ' | b'\t' | b'\n' | b'\r' => i += 1,
                _ => break,
            }
        }
        self.pos += i;
    }

    pub(crate) fn expect(&mut self, expected: &str) -> XmlResult<()> {
        if self.starts_with(expected) {
            // Most expected strings are short ASCII with no newlines (e.g. "<", ">", "/>", "=")
            self.advance_no_newlines(expected.len());
            Ok(())
        } else {
            Err(XmlError::parse(
                format!("Expected '{}'", expected),
                self.line(),
                self.column(),
            ))
        }
    }

    /// Read until the given delimiter is found. Returns the text before the delimiter
    /// as a borrowed slice. The delimiter is consumed.
    pub(crate) fn read_until(&mut self, delimiter: &str) -> XmlResult<Cow<'a, str>> {
        if let Some(idx) = self.remaining().find(delimiter) {
            let text = &self.input[self.pos..self.pos + idx];
            self.advance(idx + delimiter.len());
            Ok(Cow::Borrowed(text))
        } else {
            Err(XmlError::parse(
                format!("Expected '{}'", delimiter),
                self.line(),
                self.column(),
            ))
        }
    }

    /// Read until the given delimiter is found. Returns owned String.
    /// Used for DTD parsing where we don't need zero-copy.
    pub(crate) fn read_until_owned(&mut self, delimiter: &str) -> XmlResult<String> {
        if let Some(idx) = self.remaining().find(delimiter) {
            let text = self.remaining()[..idx].to_string();
            self.advance(idx + delimiter.len());
            Ok(text)
        } else {
            Err(XmlError::parse(
                format!("Expected '{}'", delimiter),
                self.line(),
                self.column(),
            ))
        }
    }
}

/// Translate `\r\n` and any `\r` not followed by `\n` to a single `\n`
/// (XML 1.0 §2.11). Borrows when the text has no `\r`.
///
/// Character data gets the same translation in the text scanner; this is
/// for the constructs read as one raw slice (comments, PI data, CDATA).
pub(crate) fn normalize_line_ends(text: Cow<'_, str>) -> Cow<'_, str> {
    if !text.contains('\r') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            out.push('\n');
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

// ─── Character classifications (XML 1.0 Fifth Edition) ──

pub(crate) fn is_xml_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Check if a character is valid as the start of an XML Name.
fn is_name_start_char(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z' |
        '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' |
        '\u{F8}'..='\u{2FF}' | '\u{370}'..='\u{37D}' |
        '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' |
        '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' |
        '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' |
        '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}'
    )
}

/// Check if a character is valid as a subsequent character in an XML Name.
fn is_name_char(c: char) -> bool {
    is_name_start_char(c)
        || matches!(c,
            '-' | '.' | '0'..='9' | '\u{B7}' |
            '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'
        )
}

/// Check if a character is valid in XML 1.0 content.
pub(crate) fn is_xml_char(c: char) -> bool {
    matches!(c,
        '\u{9}' | '\u{A}' | '\u{D}' |
        '\u{20}'..='\u{D7FF}' |
        '\u{E000}'..='\u{FFFD}' |
        '\u{10000}'..='\u{10FFFF}'
    )
}

// ─── Parsing functions ──────────────────────────────────

/// Check if a byte is a valid ASCII XML name start character.
#[inline(always)]
fn is_ascii_name_start(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'_' | b':')
}

/// Parse an XML Name. Returns a borrowed slice (names never contain entities).
/// Uses fast ASCII byte scanning with fallback to Unicode for non-ASCII.
pub(crate) fn parse_name<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
    let start = cursor.pos;
    let bytes = cursor.input.as_bytes();

    // Validate first character
    let &first = bytes
        .get(start)
        .ok_or_else(|| XmlError::parse("Expected XML name", cursor.line(), cursor.column()))?;

    let mut pos = if first < 0x80 {
        if !is_ascii_name_start(first) {
            return Err(XmlError::parse(
                "Expected XML name",
                cursor.line(),
                cursor.column(),
            ));
        }
        start + 1
    } else {
        let c = cursor.input[start..]
            .chars()
            .next()
            .ok_or_else(|| XmlError::parse("Expected XML name", cursor.line(), cursor.column()))?;
        if !is_name_start_char(c) {
            return Err(XmlError::parse(
                "Expected XML name",
                cursor.line(),
                cursor.column(),
            ));
        }
        start + c.len_utf8()
    };

    // Scan ASCII name continuation bytes in bulk, falling back only when a
    // non-ASCII byte may start a Unicode NameChar.
    loop {
        pos += crate::simd::scan_name_continuation(&bytes[pos..]);
        if pos >= bytes.len() || bytes[pos] < 0x80 {
            break;
        }

        let c = cursor.input[pos..]
            .chars()
            .next()
            .ok_or_else(|| XmlError::parse("Expected XML name", cursor.line(), cursor.column()))?;
        if is_name_char(c) {
            pos += c.len_utf8();
        } else {
            break;
        }
    }

    // Names are almost always ASCII, so no newlines — use advance_no_newlines
    cursor.advance_no_newlines(pos - start);
    Ok(Cow::Borrowed(&cursor.input[start..pos]))
}

/// Convert a substring slice of a Cow into a Cow.
/// If the source is Borrowed, the result is Borrowed; otherwise Owned.
#[inline]
pub(crate) fn borrow_from_cow<'a>(source: &Cow<'a, str>, slice: &str) -> Cow<'a, str> {
    match source {
        Cow::Borrowed(s) => {
            // slice is a sub-slice of s, so we can compute the offset
            let start = slice.as_ptr() as usize - s.as_ptr() as usize;
            Cow::Borrowed(&s[start..start + slice.len()])
        }
        Cow::Owned(_) => Cow::Owned(slice.to_string()),
    }
}

/// Split a name into prefix and local parts.
pub(crate) fn split_qname(name: &str) -> (Option<&str>, &str) {
    if let Some(colon_pos) = name.find(':') {
        let prefix = &name[..colon_pos];
        let local = &name[colon_pos + 1..];
        // A colon at the start or end is just a local name with colon
        if prefix.is_empty() || local.is_empty() {
            (None, name)
        } else {
            (Some(prefix), local)
        }
    } else {
        (None, name)
    }
}

/// Parse an XML declaration (`<?xml ... ?>`).
pub(crate) fn parse_xml_declaration<'a>(cursor: &mut Cursor<'a>) -> XmlResult<XmlDeclaration<'a>> {
    cursor.expect("<?xml")?;

    // Must have whitespace after "<?xml"
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::parse(
            "Expected whitespace after '<?xml'",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    // version is required
    cursor.expect("version")?;
    cursor.skip_whitespace();
    cursor.expect("=")?;
    cursor.skip_whitespace();
    let version = parse_quoted_value(cursor)?;

    // Validate version: must be "1.0" or "1.1" (XML 1.0 §2.8 production [26])
    if &*version != "1.0" && &*version != "1.1" {
        return Err(XmlError::well_formedness(
            format!("Invalid XML version: '{}'", version),
            cursor.line(),
            cursor.column(),
        ));
    }

    let mut encoding = None;
    let mut standalone = None;

    // Check for whitespace before next attribute
    let has_ws_after_version = cursor.peek().map(is_xml_whitespace).unwrap_or(false);
    cursor.skip_whitespace();
    if cursor.starts_with("encoding") {
        if !has_ws_after_version {
            return Err(XmlError::parse(
                "Expected whitespace before 'encoding'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.expect("encoding")?;
        cursor.skip_whitespace();
        cursor.expect("=")?;
        cursor.skip_whitespace();
        let enc = parse_quoted_value(cursor)?;
        // Validate encoding name: [A-Za-z] ([A-Za-z0-9._] | '-')* (production [81])
        if !is_valid_encoding_name(&enc) {
            return Err(XmlError::well_formedness(
                format!("Invalid encoding name: '{}'", enc),
                cursor.line(),
                cursor.column(),
            ));
        }
        encoding = Some(enc);

        let has_ws_after_encoding = cursor.peek().map(is_xml_whitespace).unwrap_or(false);
        cursor.skip_whitespace();
        if cursor.starts_with("standalone") {
            if !has_ws_after_encoding {
                return Err(XmlError::parse(
                    "Expected whitespace before 'standalone'",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            let val = parse_standalone(cursor)?;
            standalone = Some(val);
        }
    } else if cursor.starts_with("standalone") {
        if !has_ws_after_version {
            return Err(XmlError::parse(
                "Expected whitespace before 'standalone'",
                cursor.line(),
                cursor.column(),
            ));
        }
        let val = parse_standalone(cursor)?;
        standalone = Some(val);
    }

    cursor.skip_whitespace();
    cursor.expect("?>")?;

    Ok(XmlDeclaration {
        version,
        encoding,
        standalone,
    })
}

/// Validate an encoding name per XML 1.0 production `[81]`:
/// EncName ::= [A-Za-z] ([A-Za-z0-9._] | '-')*
fn is_valid_encoding_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '-' {
            return false;
        }
    }
    true
}

/// Parse the standalone pseudo-attribute.
fn parse_standalone(cursor: &mut Cursor) -> XmlResult<bool> {
    cursor.expect("standalone")?;
    cursor.skip_whitespace();
    cursor.expect("=")?;
    cursor.skip_whitespace();
    let val = parse_quoted_value(cursor)?;
    if &*val != "yes" && &*val != "no" {
        return Err(XmlError::well_formedness(
            format!(
                "Invalid standalone value: '{}' (must be 'yes' or 'no')",
                val
            ),
            cursor.line(),
            cursor.column(),
        ));
    }
    Ok(&*val == "yes")
}

/// Charge `n` bytes against an entity-expansion budget. Returns a parse
/// error (with the current cursor location) when the budget would go
/// negative — this is the core of the billion-laughs / quadratic-blowup
/// defence.
#[inline]
fn charge_entity_budget(budget: &mut usize, n: usize, line: usize, col: usize) -> XmlResult<()> {
    match budget.checked_sub(n) {
        Some(remaining) => {
            *budget = remaining;
            Ok(())
        }
        None => Err(XmlError::parse(
            format!(
                "Entity expansion exceeds configured limit ({} bytes remaining)",
                *budget
            ),
            line,
            col,
        )),
    }
}

#[inline]
fn charge_entity_budget_at_cursor(
    budget: &mut usize,
    n: usize,
    cursor: &Cursor<'_>,
) -> XmlResult<()> {
    if let Some(remaining) = budget.checked_sub(n) {
        *budget = remaining;
        Ok(())
    } else {
        Err(XmlError::parse(
            format!(
                "Entity expansion exceeds configured limit ({} bytes remaining)",
                *budget
            ),
            cursor.line(),
            cursor.column(),
        ))
    }
}

/// Parse a quoted attribute value (handles both `"` and `'`).
/// Uses lazy allocation: returns Borrowed if no entities/special chars found.
fn parse_quoted_value<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
    // Standalone call site (no surrounding DTD entities): use a fresh budget
    // with the default cap so well-formed values without references parse
    // exactly as before.
    let mut budget = DEFAULT_MAX_ENTITY_EXPANSION;
    parse_quoted_value_with_entities(
        cursor,
        &EntityMap::default(),
        &mut EntityCache::default(),
        &mut budget,
    )
}

/// Parse a quoted attribute value with entity resolution.
/// Returns Cow::Borrowed when no entity/char references are encountered,
/// Cow::Owned when allocation is needed.
pub(crate) fn parse_quoted_value_with_entities<'a>(
    cursor: &mut Cursor<'a>,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
    budget: &mut usize,
) -> XmlResult<Cow<'a, str>> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote character",
                cursor.line(),
                cursor.column(),
            ));
        }
    };
    cursor.advance_char();

    let start = cursor.pos;
    // Fast path: scan ahead for the closing quote without encountering '&' or '<'
    let bytes = cursor.input.as_bytes();
    let qb = quote as u8;
    let (advance, has_non_ascii_or_control) =
        crate::simd::scan_attr_delimiters(&bytes[cursor.pos..], qb);
    let fast_end = cursor.pos + advance;
    if fast_end >= bytes.len() {
        return Err(XmlError::UnexpectedEof);
    }
    // The scan stopped at the first occurrence of quote, '&', or '<'.
    // If it's the closing quote, return the borrowed slice directly.
    if bytes[fast_end] == qb {
        let text = &cursor.input[start..fast_end];
        if has_non_ascii_or_control {
            for c in text.chars() {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X}", c as u32),
                        cursor.line(),
                        cursor.column(),
                    ));
                }
            }
        }
        cursor.pos = fast_end + 1; // +1 for closing quote
        return Ok(Cow::Borrowed(text));
    }

    // Slow path: copy what we have so far, then keep using bulk scans between
    // references. Attribute-heavy documents spend most of their time here.
    let mut value = String::with_capacity((fast_end - start).saturating_add(32));
    value.push_str(&cursor.input[start..fast_end]);
    cursor.advance(fast_end - cursor.pos);

    loop {
        let bytes = cursor.input.as_bytes();
        let scan_start = cursor.pos;
        let (advance, has_non_ascii_or_control) =
            crate::simd::scan_attr_delimiters(&bytes[scan_start..], qb);
        let scan_pos = scan_start + advance;
        if scan_pos >= bytes.len() {
            return Err(XmlError::UnexpectedEof);
        }
        if scan_pos > scan_start {
            let chunk = &cursor.input[scan_start..scan_pos];
            if has_non_ascii_or_control {
                for c in chunk.chars() {
                    if !is_xml_char(c) {
                        return Err(XmlError::well_formedness(
                            format!("Invalid XML character U+{:04X}", c as u32),
                            cursor.line(),
                            cursor.column(),
                        ));
                    }
                }
            }
            value.push_str(chunk);
            cursor.advance(scan_pos - scan_start);
        }

        match cursor.peek_byte() {
            None => return Err(XmlError::UnexpectedEof),
            Some(b) if b == qb => {
                cursor.advance_no_newlines(1);
                break;
            }
            Some(b'&') => {
                parse_reference_into(cursor, entities, entity_cache, budget, &mut value)?;
            }
            Some(b'<') => {
                return Err(XmlError::well_formedness(
                    "'<' not allowed in attribute values",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            Some(_) => unreachable!(),
        }
    }
    Ok(Cow::Owned(value))
}

/// Parse a character or entity reference (`&amp;`, `&#x41;`, etc.).
fn parse_reference(cursor: &mut Cursor) -> XmlResult<String> {
    let mut budget = DEFAULT_MAX_ENTITY_EXPANSION;
    parse_reference_with_entities(
        cursor,
        &EntityMap::default(),
        &mut EntityCache::default(),
        &mut budget,
    )
}

/// Parse a character or entity reference with custom entity resolution.
pub(crate) fn parse_reference_with_entities(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
    budget: &mut usize,
) -> XmlResult<String> {
    let mut result = String::new();
    parse_reference_into(cursor, entities, entity_cache, budget, &mut result)?;
    Ok(result)
}

/// Parse a character or entity reference and append its replacement text into
/// an existing buffer. This avoids per-reference `String` allocations on the
/// hot predefined-entity and numeric-character-reference paths.
fn parse_reference_into(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
    budget: &mut usize,
    out: &mut String,
) -> XmlResult<()> {
    cursor.expect("&")?;
    let after_amp = cursor.peek_byte();
    if after_amp == Some(b'#') {
        cursor.advance_no_newlines(1); // skip '#'
        let is_hex = cursor.peek_byte() == Some(b'x');
        if is_hex {
            cursor.advance_no_newlines(1); // skip 'x'
        }
        // Scan digits until ';' using the configured byte-search backend.
        let start = cursor.pos;
        let bytes = cursor.input.as_bytes();
        let end =
            start + crate::simd::find_byte(&bytes[start..], b';').ok_or(XmlError::UnexpectedEof)?;
        let digits = &cursor.input[start..end];
        if digits.is_empty() {
            return Err(XmlError::parse(
                if is_hex {
                    "Invalid hex character reference: &#x;"
                } else {
                    "Invalid decimal character reference: &#;"
                },
                cursor.line(),
                cursor.column(),
            ));
        }
        let mut code = 0_u32;
        for b in digits.bytes() {
            let digit = if is_hex {
                match b {
                    b'0'..=b'9' => (b - b'0') as u32,
                    b'a'..=b'f' => (b - b'a' + 10) as u32,
                    b'A'..=b'F' => (b - b'A' + 10) as u32,
                    _ => {
                        return Err(XmlError::parse(
                            format!("Invalid hex character reference: {}", digits),
                            cursor.line(),
                            cursor.column(),
                        ));
                    }
                }
            } else {
                match b {
                    b'0'..=b'9' => (b - b'0') as u32,
                    _ => {
                        return Err(XmlError::parse(
                            format!("Invalid decimal character reference: {}", digits),
                            cursor.line(),
                            cursor.column(),
                        ));
                    }
                }
            };
            code = code
                .checked_mul(if is_hex { 16 } else { 10 })
                .and_then(|n| n.checked_add(digit))
                .ok_or_else(|| {
                    XmlError::parse(
                        if is_hex {
                            format!("Invalid hex character reference: {}", digits)
                        } else {
                            format!("Invalid decimal character reference: {}", digits)
                        },
                        cursor.line(),
                        cursor.column(),
                    )
                })?;
        }
        cursor.advance_no_newlines(end - start + 1); // +1 for ';'

        let c = char::from_u32(code).ok_or_else(|| {
            XmlError::parse(
                format!("Invalid character reference: U+{:04X}", code),
                cursor.line(),
                cursor.column(),
            )
        })?;
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!(
                    "Character reference U+{:04X} is not a valid XML character",
                    code
                ),
                cursor.line(),
                cursor.column(),
            ));
        }
        out.push(c);
        Ok(())
    } else {
        match after_amp {
            Some(b'l') if cursor.remaining().starts_with("lt;") => {
                cursor.advance_no_newlines(3);
                out.push('<');
                return Ok(());
            }
            Some(b'g') if cursor.remaining().starts_with("gt;") => {
                cursor.advance_no_newlines(3);
                out.push('>');
                return Ok(());
            }
            Some(b'a') if cursor.remaining().starts_with("amp;") => {
                cursor.advance_no_newlines(4);
                out.push('&');
                return Ok(());
            }
            Some(b'a') if cursor.remaining().starts_with("apos;") => {
                cursor.advance_no_newlines(5);
                out.push('\'');
                return Ok(());
            }
            Some(b'q') if cursor.remaining().starts_with("quot;") => {
                cursor.advance_no_newlines(5);
                out.push('"');
                return Ok(());
            }
            _ => {}
        }

        let name = parse_name(cursor)?;
        cursor.expect(";")?;
        // Check cache first to avoid re-expansion and re-validation.
        // Charge the cached length against the budget — this is the
        // F-02 (quadratic blow-up) defence: one large entity
        // referenced N times becomes N * len bytes.
        if let Some(cached) = entity_cache.get(&*name) {
            charge_entity_budget_at_cursor(budget, cached.len(), cursor)?;
            out.push_str(cached);
            return Ok(());
        }
        if let Some(value) = entities.get(&*name) {
            // Fully expand the entity value, resolving nested entity refs.
            let expanded = expand_entity_value(
                value,
                entities,
                &mut vec![name.to_string()],
                budget,
                cursor.line(),
                cursor.column(),
            )?;
            // Validate well-formedness of the entity replacement text.
            let validation_text = expand_entity_value_no_builtins(
                value,
                entities,
                &mut vec![name.to_string()],
                budget,
                cursor.line(),
                cursor.column(),
            )?;
            validate_entity_as_content(&validation_text, entities, cursor.line(), cursor.column())?;
            // Cache the result for subsequent references
            entity_cache.insert(name.to_string(), expanded.clone());
            out.push_str(&expanded);
            Ok(())
        } else {
            Err(XmlError::well_formedness(
                format!("Unknown entity reference: &{};", name),
                cursor.line(),
                cursor.column(),
            ))
        }
    }
}

/// Recursively expand entity references in an entity value.
/// Detects circular references and validates the expanded text.
///
/// `budget` is a byte-budget charged on every append to the result buffer.
/// When exhausted, expansion aborts — this is the F-01 (billion-laughs)
/// defence.
fn expand_entity_value(
    value: &str,
    entities: &EntityMap,
    seen: &mut Vec<String>,
    budget: &mut usize,
    line: usize,
    col: usize,
) -> XmlResult<String> {
    // `seen.len()` is the current expansion depth (one entry pushed per nested
    // reference). Cap it so a deep linear entity chain fails closed instead of
    // overflowing the stack.
    if seen.len() > DEFAULT_MAX_ENTITY_DEPTH {
        return Err(XmlError::well_formedness(
            format!(
                "Entity expansion nested deeper than {}",
                DEFAULT_MAX_ENTITY_DEPTH
            ),
            line,
            col,
        ));
    }
    let mut result = String::new();
    let mut pos = 0;
    let bytes = value.as_bytes();

    while pos < bytes.len() {
        // Skip over CDATA sections without processing entity references inside them
        if value[pos..].starts_with("<![CDATA[") {
            if let Some(end) = value[pos..].find("]]>") {
                let cdata_end = pos + end + 3;
                charge_entity_budget(budget, cdata_end - pos, line, col)?;
                result.push_str(&value[pos..cdata_end]);
                pos = cdata_end;
                continue;
            }
        }
        if bytes[pos] == b'&' {
            // Find the semicolon
            if let Some(semi) = value[pos + 1..].find(';') {
                let ref_content = &value[pos + 1..pos + 1 + semi];
                if ref_content.starts_with('#') {
                    // Character reference - pass through (already resolved in entity value)
                    charge_entity_budget(budget, semi + 2, line, col)?;
                    result.push_str(&value[pos..pos + 2 + semi]);
                    pos = pos + 2 + semi;
                } else {
                    // Named entity reference
                    match ref_content {
                        "lt" => {
                            charge_entity_budget(budget, 1, line, col)?;
                            result.push('<');
                            pos = pos + 2 + semi;
                        }
                        "gt" => {
                            charge_entity_budget(budget, 1, line, col)?;
                            result.push('>');
                            pos = pos + 2 + semi;
                        }
                        "amp" => {
                            charge_entity_budget(budget, 1, line, col)?;
                            result.push('&');
                            pos = pos + 2 + semi;
                        }
                        "apos" => {
                            charge_entity_budget(budget, 1, line, col)?;
                            result.push('\'');
                            pos = pos + 2 + semi;
                        }
                        "quot" => {
                            charge_entity_budget(budget, 1, line, col)?;
                            result.push('"');
                            pos = pos + 2 + semi;
                        }
                        _ => {
                            let ref_name = ref_content.to_string();
                            // Check for circular reference
                            if seen.contains(&ref_name) {
                                return Err(XmlError::well_formedness(
                                    format!("Circular entity reference: &{};", ref_name),
                                    line,
                                    col,
                                ));
                            }
                            if let Some(ref_value) = entities.get(&ref_name) {
                                seen.push(ref_name);
                                let expanded = expand_entity_value(
                                    ref_value, entities, seen, budget, line, col,
                                )?;
                                seen.pop();
                                // `expanded` was already budget-charged
                                // inside the recursive call. Append without
                                // double-charging.
                                result.push_str(&expanded);
                            } else {
                                return Err(XmlError::well_formedness(
                                    format!("Unknown entity reference: &{};", ref_name),
                                    line,
                                    col,
                                ));
                            }
                            pos = pos + 2 + semi;
                        }
                    }
                }
            } else {
                // No semicolon found - malformed
                charge_entity_budget(budget, 1, line, col)?;
                result.push('&');
                pos += 1;
            }
        } else {
            // Regular character - just advance
            let c = value[pos..].chars().next().ok_or(XmlError::UnexpectedEof)?;
            charge_entity_budget(budget, c.len_utf8(), line, col)?;
            result.push(c);
            pos += c.len_utf8();
        }
    }
    Ok(result)
}

/// Like expand_entity_value but does NOT resolve built-in entities (&lt; &gt; &amp; &apos; &quot;).
fn expand_entity_value_no_builtins(
    value: &str,
    entities: &EntityMap,
    seen: &mut Vec<String>,
    budget: &mut usize,
    line: usize,
    col: usize,
) -> XmlResult<String> {
    // See `expand_entity_value`: bound recursion depth to fail deep linear
    // entity chains closed rather than overflowing the stack.
    if seen.len() > DEFAULT_MAX_ENTITY_DEPTH {
        return Err(XmlError::well_formedness(
            format!(
                "Entity expansion nested deeper than {}",
                DEFAULT_MAX_ENTITY_DEPTH
            ),
            line,
            col,
        ));
    }
    let mut result = String::new();
    let mut pos = 0;
    let bytes = value.as_bytes();

    while pos < bytes.len() {
        // Skip over CDATA sections without processing entity references inside them
        if value[pos..].starts_with("<![CDATA[") {
            if let Some(end) = value[pos..].find("]]>") {
                let cdata_end = pos + end + 3;
                charge_entity_budget(budget, cdata_end - pos, line, col)?;
                result.push_str(&value[pos..cdata_end]);
                pos = cdata_end;
                continue;
            }
        }
        if bytes[pos] == b'&' {
            if let Some(semi) = value[pos + 1..].find(';') {
                let ref_content = &value[pos + 1..pos + 1 + semi];
                if ref_content.starts_with('#') {
                    // Character reference - pass through as-is
                    charge_entity_budget(budget, semi + 2, line, col)?;
                    result.push_str(&value[pos..pos + 2 + semi]);
                    pos = pos + 2 + semi;
                } else {
                    match ref_content {
                        "lt" | "gt" | "amp" | "apos" | "quot" => {
                            // Keep built-in entities as-is (don't resolve)
                            charge_entity_budget(budget, semi + 2, line, col)?;
                            result.push_str(&value[pos..pos + 2 + semi]);
                            pos = pos + 2 + semi;
                        }
                        _ => {
                            let ref_name = ref_content.to_string();
                            if seen.contains(&ref_name) {
                                return Err(XmlError::well_formedness(
                                    format!("Circular entity reference: &{};", ref_name),
                                    line,
                                    col,
                                ));
                            }
                            if let Some(ref_value) = entities.get(&ref_name) {
                                seen.push(ref_name);
                                let expanded = expand_entity_value_no_builtins(
                                    ref_value, entities, seen, budget, line, col,
                                )?;
                                seen.pop();
                                result.push_str(&expanded);
                            } else {
                                return Err(XmlError::well_formedness(
                                    format!("Unknown entity reference: &{};", ref_name),
                                    line,
                                    col,
                                ));
                            }
                            pos = pos + 2 + semi;
                        }
                    }
                }
            } else {
                charge_entity_budget(budget, 1, line, col)?;
                result.push('&');
                pos += 1;
            }
        } else {
            let c = value[pos..].chars().next().ok_or(XmlError::UnexpectedEof)?;
            charge_entity_budget(budget, c.len_utf8(), line, col)?;
            result.push(c);
            pos += c.len_utf8();
        }
    }
    Ok(result)
}

/// Validate that an expanded entity value is well-formed when included as content.
fn validate_entity_as_content(
    text: &str,
    _entities: &EntityMap,
    line: usize,
    col: usize,
) -> XmlResult<()> {
    // Wrap in a temporary element and try to parse the whole thing
    let wrapped = format!("<__entity_wrapper__>{}</__entity_wrapper__>", text);
    let test_parser = Parser::with_namespace_aware(false);
    match test_parser.parse(&wrapped) {
        Ok(_) => Ok(()),
        Err(_) => Err(XmlError::well_formedness(
            "Entity replacement text is not well-formed content",
            line,
            col,
        )),
    }
}

/// Parse a comment (`<!-- ... -->`).
pub(crate) fn parse_comment<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
    cursor.expect("<!--")?;
    let content = cursor.read_until("-->")?;
    // Well-formedness: comments must not contain "--"
    if content.contains("--") {
        return Err(XmlError::well_formedness(
            "Comments must not contain '--'",
            cursor.line(),
            cursor.column(),
        ));
    }
    // Well-formedness: comment must not end with '-' (i.e. "--->" is invalid)
    if content.ends_with('-') {
        return Err(XmlError::well_formedness(
            "Comments must not end with '-'",
            cursor.line(),
            cursor.column(),
        ));
    }
    // Validate all characters are valid XML chars
    for c in content.chars() {
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!("Invalid XML character U+{:04X} in comment", c as u32),
                cursor.line(),
                cursor.column(),
            ));
        }
    }
    Ok(normalize_line_ends(content))
}

/// Parse a processing instruction (`<?target data?>`).
pub(crate) fn parse_pi<'a>(cursor: &mut Cursor<'a>) -> XmlResult<ProcessingInstruction<'a>> {
    cursor.expect("<?")?;
    let target = parse_name(cursor)?;
    // Well-formedness: target must not be "xml" (case-insensitive)
    if target.eq_ignore_ascii_case("xml") {
        return Err(XmlError::well_formedness(
            "Processing instruction target must not be 'xml'",
            cursor.line(),
            cursor.column(),
        ));
    }
    if cursor.starts_with("?>") {
        cursor.expect("?>")?;
        return Ok(ProcessingInstruction { target, data: None });
    }
    // Must have whitespace between target and data
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::parse(
            "Expected whitespace after PI target",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();
    let data = cursor.read_until("?>")?;
    // Validate all characters in PI data are valid XML chars
    for c in data.chars() {
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!(
                    "Invalid XML character U+{:04X} in processing instruction",
                    c as u32
                ),
                cursor.line(),
                cursor.column(),
            ));
        }
    }
    Ok(ProcessingInstruction {
        target,
        data: Some(normalize_line_ends(data)),
    })
}

/// Parse a DOCTYPE declaration, including internal subset.
///
/// `entity_budget` is the shared document-level expansion budget. It is
/// threaded into the internal-subset parser so ATTLIST default values
/// charge against the same cap as document content.
pub(crate) fn parse_doctype<'a>(
    cursor: &mut Cursor<'a>,
    entities: &mut EntityMap,
    entity_budget: &mut usize,
    forbid_entities: bool,
    max_depth: u32,
) -> XmlResult<()> {
    cursor.expect("<!DOCTYPE")?;

    // Must have whitespace after <!DOCTYPE
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!DOCTYPE'",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    // Parse root element name
    parse_name(cursor)?;
    cursor.skip_whitespace();

    // Optional ExternalID: SYSTEM or PUBLIC
    if cursor.starts_with("SYSTEM") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'SYSTEM'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_system_literal(cursor)?;
        cursor.skip_whitespace();
    } else if cursor.starts_with("PUBLIC") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'PUBLIC'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_pubid_literal(cursor)?;
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace between public and system literal",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_system_literal(cursor)?;
        cursor.skip_whitespace();
    }

    // Optional internal subset
    if cursor.peek() == Some('[') {
        cursor.advance_char();
        parse_internal_subset(cursor, entities, entity_budget, forbid_entities, max_depth)?;
        cursor.expect("]")?;
        cursor.skip_whitespace();
    }

    // Must end with >. The caller captures the raw DOCTYPE text from the
    // cursor range for round-trip serialization.
    cursor.expect(">")?;
    Ok(())
}

/// Parse a SystemLiteral (a quoted string).
fn parse_system_literal(cursor: &mut Cursor) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote for system literal",
                cursor.line(),
                cursor.column(),
            ));
        }
    };
    cursor.advance_char();
    let mut value = String::new();
    loop {
        match cursor.peek() {
            None => return Err(XmlError::UnexpectedEof),
            Some(c) if c == quote => {
                cursor.advance_char();
                break;
            }
            Some(c) => {
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Parse a PubidLiteral. Characters must be PubidChar.
fn parse_pubid_literal(cursor: &mut Cursor) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote for public ID literal",
                cursor.line(),
                cursor.column(),
            ));
        }
    };
    cursor.advance_char();
    let mut value = String::new();
    loop {
        match cursor.peek() {
            None => return Err(XmlError::UnexpectedEof),
            Some(c) if c == quote => {
                cursor.advance_char();
                break;
            }
            Some(c) => {
                if !is_pubid_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid character in public ID: U+{:04X}", c as u32),
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Check if a character is a valid PubidChar (XML 1.0 production `[13]`).
fn is_pubid_char(c: char) -> bool {
    matches!(c,
        ' ' | '\r' | '\n' |
        'a'..='z' | 'A'..='Z' | '0'..='9' |
        '-' | '\'' | '(' | ')' | '+' | ',' | '.' | '/' |
        ':' | '=' | '?' | ';' | '!' | '*' | '#' | '@' |
        '$' | '_' | '%'
    )
}

/// Parse the internal subset of a DOCTYPE declaration.
fn parse_internal_subset(
    cursor: &mut Cursor,
    entities: &mut EntityMap,
    entity_budget: &mut usize,
    forbid_entities: bool,
    max_depth: u32,
) -> XmlResult<()> {
    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            return Err(XmlError::UnexpectedEof);
        }
        if cursor.peek() == Some(']') {
            return Ok(());
        }

        if cursor.starts_with("<!--") {
            parse_comment(cursor)?;
        } else if cursor.starts_with("<?") {
            parse_pi_in_dtd(cursor)?;
        } else if cursor.starts_with("<!ELEMENT") {
            parse_element_decl(cursor, max_depth)?;
        } else if cursor.starts_with("<!ATTLIST") {
            parse_attlist_decl(cursor, entities, entity_budget)?;
        } else if cursor.starts_with("<!ENTITY") {
            if forbid_entities {
                return Err(XmlError::parse(
                    "Entity declarations are not allowed (forbid_entities)",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            parse_entity_decl(cursor, entities)?;
        } else if cursor.starts_with("<!NOTATION") {
            parse_notation_decl(cursor)?;
        } else if cursor.starts_with("<![") {
            // Conditional sections not allowed in internal subset without PE
            return Err(XmlError::well_formedness(
                "Conditional sections not allowed in internal subset",
                cursor.line(),
                cursor.column(),
            ));
        } else if cursor.starts_with("%") {
            // Parameter entity reference in internal subset
            parse_pe_reference(cursor)?;
        } else {
            return Err(XmlError::well_formedness(
                format!(
                    "Unexpected character '{}' in internal subset",
                    cursor.peek().unwrap_or('\0')
                ),
                cursor.line(),
                cursor.column(),
            ));
        }
    }
}

/// Parse a processing instruction inside the DTD.
fn parse_pi_in_dtd(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("<?")?;
    let target = parse_name(cursor)?;
    if target.eq_ignore_ascii_case("xml") {
        return Err(XmlError::well_formedness(
            "Processing instruction target must not be 'xml'",
            cursor.line(),
            cursor.column(),
        ));
    }
    if cursor.starts_with("?>") {
        cursor.expect("?>")?;
        return Ok(());
    }
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::parse(
            "Expected whitespace after PI target",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();
    cursor.read_until_owned("?>")?;
    Ok(())
}

/// Parse a parameter entity reference (`%name;`).
fn parse_pe_reference(cursor: &mut Cursor) -> XmlResult<String> {
    cursor.expect("%")?;
    let name = parse_name(cursor)?;
    cursor.expect(";")?;
    // We don't resolve PE references, but we validate the syntax
    Ok(name.into_owned())
}

/// Reject a PE reference inside a markup declaration in the internal subset.
fn reject_pe_in_markup_decl(cursor: &Cursor) -> XmlResult<()> {
    Err(XmlError::well_formedness(
        "Parameter entity reference not allowed within markup declaration in internal subset",
        cursor.line(),
        cursor.column(),
    ))
}

/// Parse an ELEMENT declaration (`<!ELEMENT name contentspec>`).
fn parse_element_decl(cursor: &mut Cursor, max_depth: u32) -> XmlResult<()> {
    cursor.expect("<!ELEMENT")?;

    // Must have whitespace after <!ELEMENT
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!ELEMENT'",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    // Element name
    parse_name(cursor)?;

    // Must have whitespace before contentspec
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after element name in ELEMENT declaration",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    // Parse content spec: EMPTY | ANY | Mixed | children
    parse_content_spec(cursor, max_depth)?;

    cursor.skip_whitespace();
    cursor.expect(">")?;
    Ok(())
}

/// Parse a content specification for an ELEMENT declaration.
fn parse_content_spec(cursor: &mut Cursor, max_depth: u32) -> XmlResult<()> {
    if cursor.starts_with("EMPTY") {
        cursor.advance(5);
        Ok(())
    } else if cursor.starts_with("ANY") {
        cursor.advance(3);
        Ok(())
    } else if cursor.peek() == Some('(') {
        parse_content_model(cursor, max_depth)
    } else if cursor.starts_with("%") {
        reject_pe_in_markup_decl(cursor)?;
        Ok(())
    } else {
        Err(XmlError::well_formedness(
            "Expected content specification (EMPTY, ANY, or content model)",
            cursor.line(),
            cursor.column(),
        ))
    }
}

/// Parse a content model (children or Mixed content).
fn parse_content_model(cursor: &mut Cursor, max_depth: u32) -> XmlResult<()> {
    cursor.expect("(")?;
    cursor.skip_whitespace();

    // Check if it's a Mixed content model starting with #PCDATA
    if cursor.starts_with("#PCDATA") {
        cursor.advance(7);
        cursor.skip_whitespace();
        if cursor.peek() == Some(')') {
            cursor.advance_char();
            if cursor.peek() == Some('*') {
                cursor.advance_char();
            }
            return Ok(());
        }
        loop {
            cursor.skip_whitespace();
            if cursor.peek() == Some(')') {
                cursor.advance_char();
                if cursor.peek() != Some('*') {
                    return Err(XmlError::well_formedness(
                        "Mixed content model with alternatives must end with ')*'",
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                cursor.advance_char();
                return Ok(());
            }
            cursor.expect("|")?;
            cursor.skip_whitespace();
            if cursor.starts_with("%") {
                reject_pe_in_markup_decl(cursor)?;
            } else {
                if cursor.peek() == Some('(') {
                    return Err(XmlError::well_formedness(
                        "Parenthesized group not allowed in Mixed content model",
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                parse_name(cursor)?;
                cursor.skip_whitespace();
                if cursor.peek() == Some('*')
                    || cursor.peek() == Some('+')
                    || cursor.peek() == Some('?')
                {
                    return Err(XmlError::well_formedness(
                        "Occurrence indicator not allowed on elements in Mixed content model",
                        cursor.line(),
                        cursor.column(),
                    ));
                }
            }
        }
    }

    // children content model; the outer group consumed above is level 0, so
    // any group opened by a particle inside it is at level 1
    parse_cp(cursor, 1, max_depth)?;
    cursor.skip_whitespace();

    if cursor.peek() == Some(')') {
        cursor.advance_char();
        if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
            cursor.advance_char();
        }
        return Ok(());
    }

    let sep = match cursor.peek() {
        Some(',') => ',',
        Some('|') => '|',
        _ => {
            return Err(XmlError::well_formedness(
                "Expected ',' or '|' or ')' in content model",
                cursor.line(),
                cursor.column(),
            ));
        }
    };

    loop {
        cursor.skip_whitespace();
        if cursor.peek() == Some(')') {
            cursor.advance_char();
            if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
                cursor.advance_char();
            }
            return Ok(());
        }
        if cursor.peek() == Some(sep) {
            cursor.advance_char();
        } else if cursor.peek() == Some(',') || cursor.peek() == Some('|') {
            return Err(XmlError::well_formedness(
                "Cannot mix ',' and '|' in content model group",
                cursor.line(),
                cursor.column(),
            ));
        } else {
            return Err(XmlError::well_formedness(
                format!("Expected '{}' or ')' in content model", sep),
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_cp(cursor, 1, max_depth)?;
    }
}

/// Parse a content particle.
///
/// `depth` is the nesting level that a parenthesized group opened by this
/// particle would occupy (the outermost content-model group is level 0).
fn parse_cp(cursor: &mut Cursor, depth: u32, max_depth: u32) -> XmlResult<()> {
    if cursor.peek() == Some('(') {
        parse_children_group(cursor, depth, max_depth)?;
    } else if cursor.starts_with("%") {
        reject_pe_in_markup_decl(cursor)?;
    } else {
        parse_name(cursor)?;
        if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
            cursor.advance_char();
        }
    }
    Ok(())
}

/// Parse a children group at nesting level `depth`.
fn parse_children_group(cursor: &mut Cursor, depth: u32, max_depth: u32) -> XmlResult<()> {
    // The outermost group (level 0, consumed by parse_content_model) is the
    // analogue of the document element at depth 0; this guard matches
    // parse_element's (error at depth >= max_depth) so Parser::with_max_depth
    // maps consistently across element and DTD content-model nesting.
    if depth >= max_depth {
        return Err(XmlError::parse(
            format!(
                "DTD content model depth limit exceeded (max_depth={})",
                max_depth
            ),
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.expect("(")?;
    cursor.skip_whitespace();

    if cursor.starts_with("#PCDATA") {
        return Err(XmlError::well_formedness(
            "#PCDATA not allowed in nested content model group",
            cursor.line(),
            cursor.column(),
        ));
    }

    parse_cp(cursor, depth + 1, max_depth)?;
    cursor.skip_whitespace();

    if cursor.peek() == Some(')') {
        cursor.advance_char();
        if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
            cursor.advance_char();
        }
        return Ok(());
    }

    let sep = match cursor.peek() {
        Some(',') => ',',
        Some('|') => '|',
        _ => {
            return Err(XmlError::well_formedness(
                "Expected ',' or '|' or ')' in content model",
                cursor.line(),
                cursor.column(),
            ));
        }
    };

    loop {
        cursor.skip_whitespace();
        if cursor.peek() == Some(')') {
            cursor.advance_char();
            if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
                cursor.advance_char();
            }
            return Ok(());
        }
        if cursor.peek() == Some(sep) {
            cursor.advance_char();
        } else if cursor.peek() == Some(',') || cursor.peek() == Some('|') {
            return Err(XmlError::well_formedness(
                "Cannot mix ',' and '|' in content model group",
                cursor.line(),
                cursor.column(),
            ));
        } else {
            return Err(XmlError::well_formedness(
                format!("Expected '{}' or ')' in content model", sep),
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_cp(cursor, depth + 1, max_depth)?;
    }
}

/// Parse an ATTLIST declaration.
fn parse_attlist_decl(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_budget: &mut usize,
) -> XmlResult<()> {
    cursor.expect("<!ATTLIST")?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!ATTLIST'",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    parse_name(cursor)?;

    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            return Err(XmlError::UnexpectedEof);
        }
        if cursor.peek() == Some('>') {
            cursor.advance_char();
            return Ok(());
        }
        if cursor.starts_with("%") {
            reject_pe_in_markup_decl(cursor)?;
            continue;
        }
        parse_att_def(cursor, entities, entity_budget)?;
    }
}

/// Parse a single attribute definition within an ATTLIST.
fn parse_att_def(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_budget: &mut usize,
) -> XmlResult<()> {
    parse_name(cursor)?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after attribute name",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    parse_att_type(cursor)?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after attribute type",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    parse_default_decl(cursor, entities, entity_budget)?;

    Ok(())
}

/// Parse an attribute type.
fn parse_att_type(cursor: &mut Cursor) -> XmlResult<()> {
    if cursor.starts_with("CDATA") {
        cursor.advance(5);
    } else if cursor.starts_with("IDREFS") {
        cursor.advance(6);
    } else if cursor.starts_with("IDREF") {
        cursor.advance(5);
    } else if cursor.starts_with("ID") {
        cursor.advance(2);
    } else if cursor.starts_with("ENTITIES") {
        cursor.advance(8);
    } else if cursor.starts_with("ENTITY") {
        cursor.advance(6);
    } else if cursor.starts_with("NMTOKENS") {
        cursor.advance(8);
    } else if cursor.starts_with("NMTOKEN") {
        cursor.advance(7);
    } else if cursor.starts_with("NOTATION") {
        cursor.advance(8);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'NOTATION'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_enumeration(cursor)?;
    } else if cursor.peek() == Some('(') {
        parse_enumeration(cursor)?;
    } else if cursor.starts_with("%") {
        reject_pe_in_markup_decl(cursor)?;
    } else {
        return Err(XmlError::well_formedness(
            "Expected attribute type (CDATA, ID, IDREF, etc.)",
            cursor.line(),
            cursor.column(),
        ));
    }
    Ok(())
}

/// Parse an enumeration.
fn parse_enumeration(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("(")?;
    cursor.skip_whitespace();

    parse_nmtoken(cursor)?;

    loop {
        cursor.skip_whitespace();
        if cursor.peek() == Some(')') {
            cursor.advance_char();
            return Ok(());
        }
        cursor.expect("|")?;
        cursor.skip_whitespace();
        parse_nmtoken(cursor)?;
    }
}

/// Parse an Nmtoken.
fn parse_nmtoken(cursor: &mut Cursor) -> XmlResult<String> {
    let mut token = String::new();
    while let Some(c) = cursor.peek() {
        if is_name_char(c) {
            cursor.advance_char();
            token.push(c);
        } else {
            break;
        }
    }
    if token.is_empty() {
        return Err(XmlError::parse(
            "Expected Nmtoken",
            cursor.line(),
            cursor.column(),
        ));
    }
    Ok(token)
}

/// Parse a default declaration.
fn parse_default_decl(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_budget: &mut usize,
) -> XmlResult<()> {
    if cursor.starts_with("#REQUIRED") {
        cursor.advance(9);
    } else if cursor.starts_with("#IMPLIED") {
        cursor.advance(8);
    } else if cursor.starts_with("#FIXED") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after '#FIXED'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_att_value_in_dtd(cursor, entities, entity_budget)?;
    } else if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
        parse_att_value_in_dtd(cursor, entities, entity_budget)?;
    } else {
        return Err(XmlError::well_formedness(
            "Expected default declaration (#REQUIRED, #IMPLIED, #FIXED, or default value)",
            cursor.line(),
            cursor.column(),
        ));
    }
    Ok(())
}

/// Parse an attribute value inside a DTD declaration.
///
/// `entity_budget` is the shared document-level expansion budget. Every
/// entity reference inside the default value charges against it, so a
/// runaway internal-subset DTD cannot drive peak memory beyond the
/// configured cap by chaining many ATTLIST defaults together.
fn parse_att_value_in_dtd(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_budget: &mut usize,
) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote character for attribute value",
                cursor.line(),
                cursor.column(),
            ));
        }
    };
    cursor.advance_char();
    let mut value = String::new();
    loop {
        match cursor.peek() {
            None => return Err(XmlError::UnexpectedEof),
            Some(c) if c == quote => {
                cursor.advance_char();
                break;
            }
            Some('&') => {
                // Entity references in ATTLIST default values share the
                // document-level `entity_budget` so a runaway internal subset
                // (e.g. many ATTLIST defaults each referencing a near-budget
                // entity) cannot bypass the cap by allocating fresh budgets
                // per declaration.
                let resolved = parse_reference_with_entities(
                    cursor,
                    entities,
                    &mut EntityCache::default(),
                    entity_budget,
                )?;
                value.push_str(&resolved);
            }
            Some('<') => {
                return Err(XmlError::well_formedness(
                    "'<' not allowed in attribute value",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            Some(c) => {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X} in DTD", c as u32),
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Parse an ENTITY declaration.
fn parse_entity_decl(cursor: &mut Cursor, entities: &mut EntityMap) -> XmlResult<()> {
    cursor.expect("<!ENTITY")?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!ENTITY'",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    let is_pe = cursor.peek() == Some('%');
    if is_pe {
        cursor.advance_char();
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after '%' in parameter entity declaration",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
    }

    let name = parse_name(cursor)?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after entity name",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
        let value = parse_entity_value(cursor)?;
        cursor.skip_whitespace();

        if !is_pe {
            entities.entry(name.into_owned()).or_insert(value);
        }
    } else if cursor.starts_with("SYSTEM") || cursor.starts_with("PUBLIC") {
        if cursor.starts_with("SYSTEM") {
            cursor.advance(6);
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace after 'SYSTEM'",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            cursor.skip_whitespace();
            parse_system_literal(cursor)?;
        } else {
            cursor.advance(6);
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace after 'PUBLIC'",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            cursor.skip_whitespace();
            parse_pubid_literal(cursor)?;
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace between public and system literal",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            cursor.skip_whitespace();
            parse_system_literal(cursor)?;
        }
        let has_ws_before_ndata = cursor.peek().map(is_xml_whitespace).unwrap_or(false);
        cursor.skip_whitespace();

        if !is_pe && cursor.starts_with("NDATA") {
            if !has_ws_before_ndata {
                return Err(XmlError::well_formedness(
                    "Expected whitespace before 'NDATA'",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            cursor.advance(5);
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace after 'NDATA'",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            cursor.skip_whitespace();
            parse_name(cursor)?;
            cursor.skip_whitespace();
        } else if is_pe && cursor.starts_with("NDATA") {
            return Err(XmlError::well_formedness(
                "NDATA not allowed on parameter entity declarations",
                cursor.line(),
                cursor.column(),
            ));
        }
    } else if cursor.starts_with("%") {
        reject_pe_in_markup_decl(cursor)?;
        cursor.skip_whitespace();
    } else {
        return Err(XmlError::well_formedness(
            "Expected entity value or external ID in ENTITY declaration",
            cursor.line(),
            cursor.column(),
        ));
    }

    cursor.skip_whitespace();
    cursor.expect(">")?;
    Ok(())
}

/// Parse an EntityValue.
fn parse_entity_value(cursor: &mut Cursor) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote for entity value",
                cursor.line(),
                cursor.column(),
            ));
        }
    };
    cursor.advance_char();
    let mut value = String::new();
    loop {
        match cursor.peek() {
            None => return Err(XmlError::UnexpectedEof),
            Some(c) if c == quote => {
                cursor.advance_char();
                break;
            }
            Some('&') => {
                if cursor.starts_with("&#x") || cursor.starts_with("&#") {
                    let resolved = parse_reference(cursor)?;
                    value.push_str(&resolved);
                } else {
                    cursor.advance(1);
                    let name = parse_name(cursor)?;
                    cursor.expect(";")?;
                    value.push('&');
                    value.push_str(&name);
                    value.push(';');
                }
            }
            Some('%') => {
                return Err(XmlError::well_formedness(
                    "Parameter entity reference not allowed within markup declaration in internal subset",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            Some('\r') => {
                // Line ends in the literal are translated like any other
                // input (§2.11); a `\r` from `&#13;` took the arm above and
                // is kept.
                cursor.advance(1);
                if cursor.peek_byte() == Some(b'\n') {
                    cursor.advance(1);
                }
                value.push('\n');
            }
            Some(c) => {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X} in entity value", c as u32),
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Parse a NOTATION declaration.
fn parse_notation_decl(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("<!NOTATION")?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!NOTATION'",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    parse_name(cursor)?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after notation name",
            cursor.line(),
            cursor.column(),
        ));
    }
    cursor.skip_whitespace();

    if cursor.starts_with("SYSTEM") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'SYSTEM'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_system_literal(cursor)?;
    } else if cursor.starts_with("PUBLIC") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'PUBLIC'",
                cursor.line(),
                cursor.column(),
            ));
        }
        cursor.skip_whitespace();
        parse_pubid_literal(cursor)?;
        cursor.skip_whitespace();
        if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
            parse_system_literal(cursor)?;
        }
    } else {
        return Err(XmlError::well_formedness(
            "Expected 'SYSTEM' or 'PUBLIC' in NOTATION declaration",
            cursor.line(),
            cursor.column(),
        ));
    }

    cursor.skip_whitespace();
    cursor.expect(">")?;
    Ok(())
}

/// Parse a CDATA section. Returns a borrowed slice (CDATA never has entity
/// expansion) unless a line end has to be translated.
pub(crate) fn parse_cdata<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
    cursor.expect("<![CDATA[")?;
    let content = cursor.read_until("]]>")?;
    // Validate all characters are valid XML chars
    for c in content.chars() {
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!("Invalid XML character U+{:04X} in CDATA section", c as u32),
                cursor.line(),
                cursor.column(),
            ));
        }
    }
    Ok(normalize_line_ends(content))
}

#[cfg(test)]
mod tests {
    use crate::dom::NodeKind;

    use super::*;

    #[test]
    fn test_parse_simple_element() {
        let doc = Parser::new().parse("<root/>").unwrap();
        let root = doc.document_element().unwrap();
        let elem = doc.element(root).unwrap();
        assert_eq!(&*elem.name.local_name, "root");
    }

    #[test]
    fn test_parse_text_content() {
        let doc = Parser::new().parse("<root>hello world</root>").unwrap();
        let root = doc.document_element().unwrap();
        let text = doc.text_content_deep(root);
        assert_eq!(text, "hello world");
    }

    #[test]
    fn test_parse_attributes() {
        let doc = Parser::new()
            .parse(r#"<root attr="value" foo='bar'/>"#)
            .unwrap();
        let root = doc.document_element().unwrap();
        let elem = doc.element(root).unwrap();
        assert_eq!(elem.get_attribute("attr"), Some("value"));
        assert_eq!(elem.get_attribute("foo"), Some("bar"));
    }

    #[test]
    fn test_parse_entity_references() {
        let doc = Parser::new()
            .parse("<root>&lt;&gt;&amp;&apos;&quot;</root>")
            .unwrap();
        let root = doc.document_element().unwrap();
        assert_eq!(doc.text_content_deep(root), "<>&'\"");
    }

    #[test]
    fn test_parse_character_references() {
        let doc = Parser::new().parse("<root>&#65;&#x42;</root>").unwrap();
        let root = doc.document_element().unwrap();
        assert_eq!(doc.text_content_deep(root), "AB");
    }

    #[test]
    fn test_mismatched_end_tag() {
        let result = Parser::new().parse("<root></other>");
        assert!(result.is_err());
    }

    #[test]
    fn test_duplicate_attribute() {
        let result = Parser::new().parse(r#"<root a="1" a="2"/>"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_xml_declaration() {
        let doc = Parser::new()
            .parse(r#"<?xml version="1.0" encoding="UTF-8"?><root/>"#)
            .unwrap();
        let decl = doc.xml_declaration.as_ref().unwrap();
        assert_eq!(&*decl.version, "1.0");
        assert_eq!(decl.encoding.as_deref(), Some("UTF-8"));
    }

    #[test]
    fn test_parse_cdata() {
        let doc = Parser::new()
            .parse("<root><![CDATA[<not>&xml;]]></root>")
            .unwrap();
        let root = doc.document_element().unwrap();
        assert_eq!(doc.text_content_deep(root), "<not>&xml;");
    }

    #[test]
    fn test_parse_comment() {
        let doc = Parser::new()
            .parse("<root><!-- a comment --></root>")
            .unwrap();
        let root = doc.document_element().unwrap();
        let children = doc.children(root);
        assert_eq!(children.len(), 1);
        assert!(matches!(
            doc.node_kind(children[0]),
            Some(NodeKind::Comment(_))
        ));
    }

    #[test]
    fn test_no_root_element() {
        let result = Parser::new().parse("");
        assert!(result.is_err());
    }

    #[test]
    fn test_two_root_elements() {
        let result = Parser::new().parse("<a/><b/>");
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_copy_text() {
        // Verify that simple text content borrows from input
        let input = "<root>hello</root>";
        let doc = Parser::new().parse(input).unwrap();
        let root = doc.document_element().unwrap();
        let children = doc.children(root);
        if let Some(NodeKind::Text(t)) = doc.node_kind(children[0]) {
            assert!(matches!(t, Cow::Borrowed(_)), "Expected borrowed text");
        }
    }

    #[test]
    fn test_zero_copy_name() {
        // Verify that element names borrow from input
        let input = "<root/>";
        let doc = Parser::new().parse(input).unwrap();
        let root = doc.document_element().unwrap();
        let elem = doc.element(root).unwrap();
        // With namespace resolution the name gets owned, but without:
        let doc2 = Parser::with_namespace_aware(false).parse(input).unwrap();
        let root2 = doc2.document_element().unwrap();
        let elem2 = doc2.element(root2).unwrap();
        assert!(
            matches!(elem2.name.local_name, Cow::Borrowed(_)),
            "Expected borrowed name"
        );
        let _ = elem; // suppress unused warning
    }

    fn nested_xml(depth: usize) -> String {
        let mut s = String::with_capacity(depth * 8);
        for _ in 0..depth {
            s.push_str("<a>");
        }
        s.push('x');
        for _ in 0..depth {
            s.push_str("</a>");
        }
        s
    }

    #[test]
    fn test_depth_cap_rejects_deep_input() {
        // 5 000-deep nesting would stack-overflow an unguarded recursive
        // parser. The default cap stops it with a clean error.
        let xml = nested_xml(5_000);
        let err = Parser::new()
            .parse(&xml)
            .expect_err("deep input must be rejected");
        assert!(
            format!("{}", err).contains("maximum depth"),
            "expected depth-cap error, got: {}",
            err
        );
    }

    #[test]
    fn test_depth_within_cap_parses() {
        // Well under DEFAULT_MAX_DEPTH — must parse without complaint.
        let xml = nested_xml(100);
        let doc = Parser::new().parse(&xml).expect("within cap must parse");
        assert!(doc.document_element().is_some());
    }

    #[test]
    fn test_custom_max_depth() {
        let xml = nested_xml(10);
        assert!(
            Parser::new().with_max_depth(5).parse(&xml).is_err(),
            "cap of 5 must reject 10-deep input"
        );
        assert!(
            Parser::new().with_max_depth(20).parse(&xml).is_ok(),
            "cap of 20 must admit 10-deep input"
        );
    }

    /// Canonical billion-laughs. Five nesting levels already yield
    /// 3 · 10⁵ bytes of expansion (>256 KiB), comfortably beyond a
    /// 1 MiB budget once the nested expansion runs to completion.
    /// Six levels → 3 MiB → budget exhausted mid-expansion.
    #[test]
    fn test_entity_budget_rejects_billion_laughs() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
  <!ENTITY lol "lol">
  <!ENTITY lol1 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
  <!ENTITY lol2 "&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;">
  <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
  <!ENTITY lol4 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">
  <!ENTITY lol5 "&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;">
  <!ENTITY lol6 "&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;">
]>
<lolz>&lol6;</lolz>"#;
        let err = Parser::new()
            .parse(xml)
            .expect_err("billion-laughs must be rejected by the default budget");
        let msg = format!("{}", err);
        assert!(
            msg.contains("Entity expansion"),
            "expected entity-budget error, got: {}",
            msg
        );
    }

    /// F-02 — one moderate entity referenced many times. The matcher
    /// cache hits each clone cached.len() bytes against the budget, so
    /// N × len quickly blows through even if any single expansion stays
    /// small.
    #[test]
    fn test_entity_budget_rejects_quadratic_blowup() {
        // `a` is 10 000 chars, `b` is 10 × a = 100 000 chars, and the
        // document body references `b` 50 times — 5 MiB total → budget
        // exceeded.
        let big_a = "A".repeat(10_000);
        let xml = format!(
            r#"<?xml version="1.0"?>
<!DOCTYPE doc [
  <!ENTITY a "{}">
  <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
]>
<doc>{}</doc>"#,
            big_a,
            "&b;".repeat(50),
        );
        let err = Parser::new()
            .parse(&xml)
            .expect_err("quadratic blow-up must be rejected");
        assert!(
            format!("{}", err).contains("Entity expansion"),
            "expected entity-budget error, got: {}",
            err
        );
    }

    /// Legitimate entity use must still parse under the default cap.
    #[test]
    fn test_entity_budget_allows_legitimate_use() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE doc [
  <!ENTITY name "Alice">
  <!ENTITY greeting "Hello, &name;!">
]>
<doc>&greeting; &greeting; &greeting;</doc>"#;
        let doc = Parser::new().parse(xml).expect("legitimate entities OK");
        let root = doc.document_element().unwrap();
        let text = doc.text_content_deep(root);
        assert_eq!(text, "Hello, Alice! Hello, Alice! Hello, Alice!");
    }

    /// M-1 regression — ATTLIST defaults must charge against the shared
    /// document budget, not a fresh per-`&` 1 MiB cap. Pre-fix, an internal
    /// subset DTD could chain N ATTLIST defaults each referencing a
    /// near-budget entity to consume `N × budget` peak memory while never
    /// tripping the document-level cap.
    #[test]
    fn test_entity_budget_covers_attlist_defaults() {
        // `a` is 500 KiB, referenced 3× in a single ATTLIST default → 1.5 MiB
        // expansion total. Pre-fix this parsed; post-fix the shared 1 MiB
        // budget rejects.
        let big_a = "A".repeat(500_000);
        let xml = format!(
            r#"<?xml version="1.0"?>
<!DOCTYPE r [
<!ENTITY a "{}">
<!ATTLIST r foo CDATA "&a;&a;&a;">
]>
<r/>"#,
            big_a
        );
        let err = Parser::new()
            .parse(&xml)
            .expect_err("ATTLIST default must charge against the shared budget");
        assert!(
            format!("{}", err).contains("Entity expansion"),
            "expected entity-budget error, got: {}",
            err
        );
    }

    /// Custom budget wins: a tiny cap must reject what the default accepts.
    #[test]
    fn test_custom_max_entity_expansion() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE doc [<!ENTITY s "XXXXXXXXXXXXXXXX">]>
<doc>&s;&s;&s;&s;</doc>"#;
        // Budget of 32 bytes — not enough for 4×16-byte expansions.
        assert!(
            Parser::new()
                .with_max_entity_expansion(32)
                .parse(xml)
                .is_err(),
            "tight budget must fire"
        );
        // Loose budget — parses.
        assert!(
            Parser::new()
                .with_max_entity_expansion(1 << 16)
                .parse(xml)
                .is_ok(),
            "loose budget must admit the same input"
        );
    }
}
