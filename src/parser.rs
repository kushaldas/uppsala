//! XML 1.0 (Fifth Edition) parser with well-formedness checking.
//!
//! This module implements a recursive-descent parser that tokenizes XML input
//! and builds a [`Document`] tree. It enforces the well-formedness constraints
//! defined in the XML 1.0 specification.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::dom::{
    Attribute, Document, Element, NodeId, NodeKind, ProcessingInstruction, QName, XmlDeclaration,
};
use crate::error::{XmlError, XmlResult};
use crate::namespace::NamespaceResolver;

/// A map of general entity names to their replacement text.
type EntityMap = HashMap<String, String>;

/// Cache of already-validated entity expansion results.
/// Key: entity name, Value: expanded text.
type EntityCache = HashMap<String, String>;

/// Default maximum element-nesting depth.
///
/// Sized well above legitimate XML (SOAP, XHTML, SVG, and XSLT documents in the
/// wild rarely exceed ~50 levels) while staying comfortably within a 2 MiB
/// thread stack (Rust's default worker-thread size) even under debug builds
/// where per-frame overhead is inflated. Exposed via [`Parser::with_max_depth`].
pub const DEFAULT_MAX_DEPTH: u32 = 128;

/// The XML 1.0 parser.
pub struct Parser {
    /// Whether to resolve namespaces during parsing.
    namespace_aware: bool,
    /// Maximum allowed element-nesting depth. Enforced in `parse_element`
    /// to prevent stack overflow on maliciously deep input.
    max_depth: u32,
}

impl Parser {
    /// Create a new parser with namespace awareness enabled and the default
    /// maximum nesting depth ([`DEFAULT_MAX_DEPTH`]).
    pub fn new() -> Self {
        Parser {
            namespace_aware: true,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// Create a new parser with configurable namespace awareness. Uses the
    /// default nesting-depth cap.
    pub fn with_namespace_aware(namespace_aware: bool) -> Self {
        Parser {
            namespace_aware,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// Override the maximum element-nesting depth. Returns `self` so it can
    /// chain with other builder methods.
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Parse an XML string into a [`Document`].
    pub fn parse<'a>(&self, input: &'a str) -> XmlResult<Document<'a>> {
        let mut cursor = Cursor::new(input);
        let mut doc = Document::new();
        doc.input = input;

        // Pre-allocate based on input size heuristic (avg ~40 bytes per element)
        doc.nodes.reserve(input.len() / 40);
        let mut ns_resolver = if self.namespace_aware {
            Some(NamespaceResolver::new())
        } else {
            None
        };
        let mut entities = EntityMap::new();
        let mut entity_cache = EntityCache::new();

        // Skip BOM if present
        cursor.skip_bom();

        // Parse optional XML declaration (must be at very start, after BOM)
        if cursor.starts_with("<?xml ")
            || cursor.starts_with("<?xml\t")
            || cursor.starts_with("<?xml\r")
            || cursor.starts_with("<?xml\n")
        {
            let decl = parse_xml_declaration(&mut cursor)?;
            doc.xml_declaration = Some(decl);
        }

        // Parse prolog content (comments, PIs, whitespace, DOCTYPE)
        let root_id = doc.root();
        parse_misc(&mut cursor, &mut doc, root_id, &mut entities)?;

        // Parse document element and trailing misc
        let mut found_root = false;
        while !cursor.is_eof() {
            cursor.skip_whitespace();
            if cursor.is_eof() {
                break;
            }
            if cursor.starts_with("<!--") {
                let start = cursor.pos;
                let comment = parse_comment(&mut cursor)?;
                let id = doc.alloc_node(NodeKind::Comment(comment), start);
                doc.set_byte_end_pos(id, cursor.pos);
                doc.append_child_unchecked(root_id, id);
            } else if cursor.starts_with("<?") {
                let start = cursor.pos;
                let pi = parse_pi(&mut cursor)?;
                let id = doc.alloc_node(NodeKind::ProcessingInstruction(pi), start);
                doc.set_byte_end_pos(id, cursor.pos);
                doc.append_child_unchecked(root_id, id);
            } else if cursor.starts_with("<") {
                if found_root {
                    return Err(XmlError::well_formedness(
                        "Only one root element is allowed",
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                parse_element(
                    &mut cursor,
                    &mut doc,
                    root_id,
                    &mut ns_resolver,
                    &entities,
                    &mut entity_cache,
                    0,
                    self.max_depth,
                )?;
                found_root = true;
            } else {
                // Non-whitespace text outside root element
                return Err(XmlError::well_formedness(
                    "Content found outside of root element",
                    cursor.line(),
                    cursor.column(),
                ));
            }
        }

        if !found_root {
            return Err(XmlError::well_formedness(
                "Document must have a root element",
                0,
                0,
            ));
        }

        // Set document node end position to end of input
        doc.set_byte_end_pos(root_id, input.len());

        Ok(doc)
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
struct Cursor<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a str) -> Self {
        Cursor { input, pos: 0 }
    }

    /// Compute line number (1-based) from current byte position.
    /// Only called in error paths and node allocation, not in the hot parse loop.
    #[inline(never)]
    fn line(&self) -> usize {
        self.input.as_bytes()[..self.pos]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1
    }

    /// Compute column number (1-based) from current byte position.
    #[inline(never)]
    fn column(&self) -> usize {
        let bytes = &self.input.as_bytes()[..self.pos];
        match bytes.iter().rposition(|&b| b == b'\n') {
            Some(nl_pos) => self.pos - nl_pos,
            None => self.pos + 1,
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    /// Peek at the current byte without creating a char iterator.
    /// Much faster than peek() for ASCII-dominated XML content.
    #[inline(always)]
    fn peek_byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.remaining().starts_with(prefix)
    }

    /// Advance by n bytes.
    #[inline(always)]
    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    /// Advance by n bytes (alias for advance, kept for compatibility).
    #[inline(always)]
    fn advance_no_newlines(&mut self, n: usize) {
        self.pos += n;
    }

    fn advance_char(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_bom(&mut self) {
        if self.remaining().starts_with('\u{FEFF}') {
            self.pos += '\u{FEFF}'.len_utf8();
        }
    }

    fn skip_whitespace(&mut self) {
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

    fn expect(&mut self, expected: &str) -> XmlResult<()> {
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
    fn read_until(&mut self, delimiter: &str) -> XmlResult<Cow<'a, str>> {
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
    fn read_until_owned(&mut self, delimiter: &str) -> XmlResult<String> {
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

// ─── Character classifications (XML 1.0 Fifth Edition) ──

fn is_xml_whitespace(c: char) -> bool {
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
fn is_xml_char(c: char) -> bool {
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

/// Check if a byte is a valid ASCII XML name character.
#[inline(always)]
fn is_ascii_name_char(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b':' | b'-' | b'.')
}

/// Parse an XML Name. Returns a borrowed slice (names never contain entities).
/// Uses fast ASCII byte scanning with fallback to Unicode for non-ASCII.
fn parse_name<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
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
        let c = cursor.input[start..].chars().next().unwrap();
        if !is_name_start_char(c) {
            return Err(XmlError::parse(
                "Expected XML name",
                cursor.line(),
                cursor.column(),
            ));
        }
        start + c.len_utf8()
    };

    // Scan remaining characters (ASCII fast path)
    while pos < bytes.len() {
        let b = bytes[pos];
        if b < 0x80 {
            if is_ascii_name_char(b) {
                pos += 1;
            } else {
                break;
            }
        } else {
            let c = cursor.input[pos..].chars().next().unwrap();
            if is_name_char(c) {
                pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    // Names are almost always ASCII, so no newlines — use advance_no_newlines
    cursor.advance_no_newlines(pos - start);
    Ok(Cow::Borrowed(&cursor.input[start..pos]))
}

/// Convert a substring slice of a Cow into a Cow.
/// If the source is Borrowed, the result is Borrowed; otherwise Owned.
#[inline]
fn borrow_from_cow<'a>(source: &Cow<'a, str>, slice: &str) -> Cow<'a, str> {
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
fn split_qname(name: &str) -> (Option<&str>, &str) {
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
fn parse_xml_declaration<'a>(cursor: &mut Cursor<'a>) -> XmlResult<XmlDeclaration<'a>> {
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

/// Validate an encoding name per XML 1.0 production [81]:
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

/// Parse a quoted attribute value (handles both `"` and `'`).
/// Uses lazy allocation: returns Borrowed if no entities/special chars found.
fn parse_quoted_value<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
    parse_quoted_value_with_entities(cursor, &HashMap::new(), &mut EntityCache::new())
}

/// Parse a quoted attribute value with entity resolution.
/// Returns Cow::Borrowed when no entity/char references are encountered,
/// Cow::Owned when allocation is needed.
fn parse_quoted_value_with_entities<'a>(
    cursor: &mut Cursor<'a>,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
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

    // Slow path: copy what we have so far, then use bulk scanning between specials
    let mut value = String::from(&cursor.input[start..fast_end]);
    cursor.advance(fast_end - cursor.pos);

    loop {
        // Bulk scan for next special character in the slow path
        let bytes = cursor.input.as_bytes();
        let scan_start = cursor.pos;
        let mut scan_pos = scan_start;
        while scan_pos < bytes.len() {
            let b = bytes[scan_pos];
            if b == qb || b == b'&' || b == b'<' {
                break;
            }
            scan_pos += 1;
        }
        if scan_pos > scan_start {
            // Validate and copy clean characters in bulk
            let chunk = &cursor.input[scan_start..scan_pos];
            for c in chunk.chars() {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X}", c as u32),
                        cursor.line(),
                        cursor.column(),
                    ));
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
                let resolved = parse_reference_with_entities(cursor, entities, entity_cache)?;
                value.push_str(&resolved);
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
    parse_reference_with_entities(cursor, &HashMap::new(), &mut EntityCache::new())
}

/// Parse a character or entity reference with custom entity resolution.
fn parse_reference_with_entities(
    cursor: &mut Cursor,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
) -> XmlResult<String> {
    cursor.expect("&")?;
    let after_amp = cursor.peek_byte();
    if after_amp == Some(b'#') {
        cursor.advance_no_newlines(1); // skip '#'
        let is_hex = cursor.peek_byte() == Some(b'x');
        if is_hex {
            cursor.advance_no_newlines(1); // skip 'x'
        }
        // Scan digits until ';' using byte scanning
        let start = cursor.pos;
        let bytes = cursor.input.as_bytes();
        let mut end = start;
        while end < bytes.len() && bytes[end] != b';' {
            end += 1;
        }
        if end >= bytes.len() {
            return Err(XmlError::UnexpectedEof);
        }
        let digits = &cursor.input[start..end];
        cursor.advance_no_newlines(end - start + 1); // +1 for ';'

        let code = if is_hex {
            u32::from_str_radix(digits, 16).map_err(|_| {
                XmlError::parse(
                    format!("Invalid hex character reference: {}", digits),
                    cursor.line(),
                    cursor.column(),
                )
            })?
        } else {
            digits.parse::<u32>().map_err(|_| {
                XmlError::parse(
                    format!("Invalid decimal character reference: {}", digits),
                    cursor.line(),
                    cursor.column(),
                )
            })?
        };
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
        Ok(c.to_string())
    } else {
        // Named entity reference
        let name = parse_name(cursor)?;
        cursor.expect(";")?;
        match &*name {
            "lt" => Ok("<".to_string()),
            "gt" => Ok(">".to_string()),
            "amp" => Ok("&".to_string()),
            "apos" => Ok("'".to_string()),
            "quot" => Ok("\"".to_string()),
            _ => {
                // Check cache first to avoid re-expansion and re-validation
                if let Some(cached) = entity_cache.get(&*name) {
                    return Ok(cached.clone());
                }
                if let Some(value) = entities.get(&*name) {
                    // Fully expand the entity value, resolving nested entity refs.
                    let expanded = expand_entity_value(
                        value,
                        entities,
                        &mut vec![name.to_string()],
                        cursor.line(),
                        cursor.column(),
                    )?;
                    // Validate well-formedness of the entity replacement text.
                    let validation_text = expand_entity_value_no_builtins(
                        value,
                        entities,
                        &mut vec![name.to_string()],
                        cursor.line(),
                        cursor.column(),
                    )?;
                    validate_entity_as_content(
                        &validation_text,
                        entities,
                        cursor.line(),
                        cursor.column(),
                    )?;
                    // Cache the result for subsequent references
                    entity_cache.insert(name.to_string(), expanded.clone());
                    Ok(expanded)
                } else {
                    Err(XmlError::well_formedness(
                        format!("Unknown entity reference: &{};", name),
                        cursor.line(),
                        cursor.column(),
                    ))
                }
            }
        }
    }
}

/// Recursively expand entity references in an entity value.
/// Detects circular references and validates the expanded text.
fn expand_entity_value(
    value: &str,
    entities: &EntityMap,
    seen: &mut Vec<String>,
    line: usize,
    col: usize,
) -> XmlResult<String> {
    let mut result = String::new();
    let mut pos = 0;
    let bytes = value.as_bytes();

    while pos < bytes.len() {
        // Skip over CDATA sections without processing entity references inside them
        if value[pos..].starts_with("<![CDATA[") {
            if let Some(end) = value[pos..].find("]]>") {
                let cdata_end = pos + end + 3;
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
                    result.push_str(&value[pos..pos + 2 + semi]);
                    pos = pos + 2 + semi;
                } else {
                    // Named entity reference
                    match ref_content {
                        "lt" => {
                            result.push('<');
                            pos = pos + 2 + semi;
                        }
                        "gt" => {
                            result.push('>');
                            pos = pos + 2 + semi;
                        }
                        "amp" => {
                            result.push('&');
                            pos = pos + 2 + semi;
                        }
                        "apos" => {
                            result.push('\'');
                            pos = pos + 2 + semi;
                        }
                        "quot" => {
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
                                let expanded =
                                    expand_entity_value(ref_value, entities, seen, line, col)?;
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
                // No semicolon found - malformed
                result.push('&');
                pos += 1;
            }
        } else {
            // Regular character - just advance
            let c = value[pos..].chars().next().unwrap();
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
    line: usize,
    col: usize,
) -> XmlResult<String> {
    let mut result = String::new();
    let mut pos = 0;
    let bytes = value.as_bytes();

    while pos < bytes.len() {
        // Skip over CDATA sections without processing entity references inside them
        if value[pos..].starts_with("<![CDATA[") {
            if let Some(end) = value[pos..].find("]]>") {
                let cdata_end = pos + end + 3;
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
                    result.push_str(&value[pos..pos + 2 + semi]);
                    pos = pos + 2 + semi;
                } else {
                    match ref_content {
                        "lt" | "gt" | "amp" | "apos" | "quot" => {
                            // Keep built-in entities as-is (don't resolve)
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
                                    ref_value, entities, seen, line, col,
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
                result.push('&');
                pos += 1;
            }
        } else {
            let c = value[pos..].chars().next().unwrap();
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
fn parse_comment<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
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
    Ok(content)
}

/// Parse a processing instruction (`<?target data?>`).
fn parse_pi<'a>(cursor: &mut Cursor<'a>) -> XmlResult<ProcessingInstruction<'a>> {
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
        data: Some(data),
    })
}

/// Parse prolog miscellaneous content (comments, PIs, whitespace, DOCTYPE).
fn parse_misc<'a>(
    cursor: &mut Cursor<'a>,
    doc: &mut Document<'a>,
    parent: NodeId,
    entities: &mut EntityMap,
) -> XmlResult<()> {
    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            break;
        }
        if cursor.starts_with("<!--") {
            let start = cursor.pos;
            let comment = parse_comment(cursor)?;
            let id = doc.alloc_node(NodeKind::Comment(comment), start);
            doc.set_byte_end_pos(id, cursor.pos);
            doc.append_child_unchecked(parent, id);
        } else if cursor.starts_with("<?") {
            let start = cursor.pos;
            let pi = parse_pi(cursor)?;
            let id = doc.alloc_node(NodeKind::ProcessingInstruction(pi), start);
            doc.set_byte_end_pos(id, cursor.pos);
            doc.append_child_unchecked(parent, id);
        } else if cursor.starts_with("<!DOCTYPE") {
            parse_doctype(cursor, doc, entities)?;
        } else {
            break;
        }
    }
    Ok(())
}

/// Parse a DOCTYPE declaration, including internal subset.
fn parse_doctype<'a>(
    cursor: &mut Cursor<'a>,
    doc: &mut Document<'a>,
    entities: &mut EntityMap,
) -> XmlResult<()> {
    let start_pos = cursor.pos;
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
        parse_internal_subset(cursor, entities)?;
        cursor.expect("]")?;
        cursor.skip_whitespace();
    }

    // Must end with >
    cursor.expect(">")?;
    // Capture the raw DOCTYPE text for round-trip serialization (borrowed from input)
    doc.doctype = Some(Cow::Borrowed(&cursor.input[start_pos..cursor.pos]));
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

/// Check if a character is a valid PubidChar (XML 1.0 production [13]).
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
fn parse_internal_subset(cursor: &mut Cursor, entities: &mut EntityMap) -> XmlResult<()> {
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
            parse_element_decl(cursor)?;
        } else if cursor.starts_with("<!ATTLIST") {
            parse_attlist_decl(cursor, entities)?;
        } else if cursor.starts_with("<!ENTITY") {
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
fn parse_element_decl(cursor: &mut Cursor) -> XmlResult<()> {
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
    parse_content_spec(cursor)?;

    cursor.skip_whitespace();
    cursor.expect(">")?;
    Ok(())
}

/// Parse a content specification for an ELEMENT declaration.
fn parse_content_spec(cursor: &mut Cursor) -> XmlResult<()> {
    if cursor.starts_with("EMPTY") {
        cursor.advance(5);
        Ok(())
    } else if cursor.starts_with("ANY") {
        cursor.advance(3);
        Ok(())
    } else if cursor.peek() == Some('(') {
        parse_content_model(cursor)
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
fn parse_content_model(cursor: &mut Cursor) -> XmlResult<()> {
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

    // children content model
    parse_cp(cursor)?;
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
        parse_cp(cursor)?;
    }
}

/// Parse a content particle.
fn parse_cp(cursor: &mut Cursor) -> XmlResult<()> {
    if cursor.peek() == Some('(') {
        parse_children_group(cursor)?;
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

/// Parse a children group.
fn parse_children_group(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("(")?;
    cursor.skip_whitespace();

    if cursor.starts_with("#PCDATA") {
        return Err(XmlError::well_formedness(
            "#PCDATA not allowed in nested content model group",
            cursor.line(),
            cursor.column(),
        ));
    }

    parse_cp(cursor)?;
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
        parse_cp(cursor)?;
    }
}

/// Parse an ATTLIST declaration.
fn parse_attlist_decl(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<()> {
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
        parse_att_def(cursor, entities)?;
    }
}

/// Parse a single attribute definition within an ATTLIST.
fn parse_att_def(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<()> {
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

    parse_default_decl(cursor, entities)?;

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
fn parse_default_decl(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<()> {
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
        parse_att_value_in_dtd(cursor, entities)?;
    } else if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
        parse_att_value_in_dtd(cursor, entities)?;
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
fn parse_att_value_in_dtd(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<String> {
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
                let resolved =
                    parse_reference_with_entities(cursor, entities, &mut EntityCache::new())?;
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

/// Parse an element and its content recursively.
///
/// `depth` is the current nesting depth (0 for the document element); `max_depth`
/// is the configured cap. When `depth >= max_depth` the parser returns an error
/// instead of recursing further — this prevents stack overflow on maliciously
/// deep input.
#[allow(clippy::too_many_arguments)]
fn parse_element<'a>(
    cursor: &mut Cursor<'a>,
    doc: &mut Document<'a>,
    parent: NodeId,
    ns_resolver: &mut Option<NamespaceResolver<'a>>,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
    depth: u32,
    max_depth: u32,
) -> XmlResult<NodeId> {
    if depth >= max_depth {
        return Err(XmlError::parse(
            format!("Element nesting exceeds maximum depth of {}", max_depth),
            cursor.line(),
            cursor.column(),
        ));
    }
    let start_pos = cursor.pos;

    cursor.expect("<")?;
    let tag_name = parse_name(cursor)?;

    // Parse attributes
    let mut raw_attrs: Vec<(Cow<'a, str>, Cow<'a, str>)> = Vec::with_capacity(8);
    let mut ns_decls: Vec<(Cow<'a, str>, Cow<'a, str>)> = Vec::new();

    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            return Err(XmlError::UnexpectedEof);
        }
        if matches!(cursor.peek_byte(), Some(b'>') | Some(b'/')) {
            break;
        }
        let attr_name = parse_name(cursor)?;
        cursor.skip_whitespace();
        cursor.expect("=")?;
        cursor.skip_whitespace();
        let attr_value = parse_quoted_value_with_entities(cursor, entities, entity_cache)?;

        // Separate namespace declarations from regular attributes.
        // xmlns attrs go only into ns_decls (not raw_attrs) to avoid cloning.
        if &*attr_name == "xmlns" {
            if ns_decls.iter().any(|(p, _)| p.is_empty()) {
                return Err(XmlError::well_formedness(
                    format!("Duplicate attribute: {}", attr_name),
                    cursor.line(),
                    cursor.column(),
                ));
            }
            ns_decls.push((Cow::Borrowed(""), attr_value));
        } else if let Some(prefix) = attr_name.strip_prefix("xmlns:") {
            if prefix == "xmlns" {
                return Err(XmlError::namespace(
                    "The prefix 'xmlns' must not be declared",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            if prefix == "xml" && &*attr_value != "http://www.w3.org/XML/1998/namespace" {
                return Err(XmlError::namespace(
                    "The prefix 'xml' must not be bound to any other namespace",
                    cursor.line(),
                    cursor.column(),
                ));
            }
            if ns_decls.iter().any(|(p, _)| &**p == prefix) {
                return Err(XmlError::well_formedness(
                    format!("Duplicate attribute: {}", attr_name),
                    cursor.line(),
                    cursor.column(),
                ));
            }
            let prefix_cow: Cow<'a, str> = match &attr_name {
                Cow::Borrowed(s) => Cow::Borrowed(&s[6..]),
                Cow::Owned(s) => Cow::Owned(s[6..].to_string()),
            };
            ns_decls.push((prefix_cow, attr_value));
        } else {
            // Regular attribute — check for duplicates among regular attrs only
            if raw_attrs.iter().any(|(n, _)| *n == *attr_name) {
                return Err(XmlError::well_formedness(
                    format!("Duplicate attribute: {}", attr_name),
                    cursor.line(),
                    cursor.column(),
                ));
            }
            raw_attrs.push((attr_name, attr_value));
        }

        // After an attribute value, must have whitespace, '>', or '/>'
        if let Some(b) = cursor.peek_byte() {
            if b != b'>' && b != b'/' && b != b' ' && b != b'\t' && b != b'\n' && b != b'\r' {
                return Err(XmlError::well_formedness(
                    "Expected whitespace between attributes",
                    cursor.line(),
                    cursor.column(),
                ));
            }
        }
    }

    // Push namespace scope
    if let Some(resolver) = ns_resolver.as_mut() {
        resolver.push_scope();
        for (prefix, uri) in &ns_decls {
            resolver.declare(prefix.clone(), uri.clone());
        }
    }

    // Resolve the element QName
    // tag_name from parse_name is always Cow::Borrowed, so we can sub-borrow safely
    let (prefix, local_name) = split_qname(&tag_name);
    let qname = if let Some(resolver) = ns_resolver.as_ref() {
        let ns: Option<Cow<'a, str>> = if let Some(p) = prefix {
            let uri = resolver.resolve(p).ok_or_else(|| {
                XmlError::namespace(
                    format!("Undeclared namespace prefix: {}", p),
                    cursor.line(),
                    cursor.column(),
                )
            })?;
            Some(uri.clone())
        } else {
            resolver.resolve_default().cloned()
        };
        // prefix and local_name are slices of tag_name which is Cow::Borrowed from input
        QName {
            namespace_uri: ns,
            prefix: prefix.map(|s| borrow_from_cow(&tag_name, s)),
            local_name: borrow_from_cow(&tag_name, local_name),
        }
    } else {
        QName::local(tag_name.clone())
    };

    // Resolve attribute QNames — consume raw_attrs (xmlns already separated out)
    let mut resolved_attrs = Vec::with_capacity(raw_attrs.len());
    for (attr_name, attr_value) in raw_attrs {
        let (a_prefix, a_local) = split_qname(&attr_name);
        let a_qname = if let Some(resolver) = ns_resolver.as_ref() {
            if let Some(p) = a_prefix {
                let ns_uri = resolver.resolve(p).ok_or_else(|| {
                    XmlError::namespace(
                        format!("Undeclared namespace prefix: {}", p),
                        cursor.line(),
                        cursor.column(),
                    )
                })?;
                QName {
                    namespace_uri: Some(ns_uri.clone()),
                    prefix: Some(borrow_from_cow(&attr_name, p)),
                    local_name: borrow_from_cow(&attr_name, a_local),
                }
            } else {
                QName::local(borrow_from_cow(&attr_name, a_local))
            }
        } else {
            QName::local(attr_name)
        };
        resolved_attrs.push(Attribute {
            name: a_qname,
            value: attr_value,
        });
    }

    // Create element node
    let elem = Element {
        name: qname,
        attributes: resolved_attrs,
        namespace_declarations: ns_decls,
    };
    let elem_id = doc.alloc_node(NodeKind::Element(elem), start_pos);
    doc.append_child_unchecked(parent, elem_id);

    // Self-closing?
    if cursor.peek_byte() == Some(b'/') {
        cursor.expect("/>")?;
        doc.set_byte_end_pos(elem_id, cursor.pos);
        if let Some(resolver) = ns_resolver.as_mut() {
            resolver.pop_scope();
        }
        return Ok(elem_id);
    }

    cursor.expect(">")?;

    // Parse element content
    parse_content(
        cursor,
        doc,
        elem_id,
        ns_resolver,
        entities,
        entity_cache,
        depth,
        max_depth,
    )?;

    // Parse end tag
    cursor.expect("</")?;
    let end_tag_name = parse_name(cursor)?;
    cursor.skip_whitespace();
    cursor.expect(">")?;
    doc.set_byte_end_pos(elem_id, cursor.pos);

    if *end_tag_name != *tag_name {
        return Err(XmlError::well_formedness(
            format!(
                "Mismatched end tag: expected </{}>, found </{}>",
                tag_name, end_tag_name
            ),
            cursor.line(),
            cursor.column(),
        ));
    }

    if let Some(resolver) = ns_resolver.as_mut() {
        resolver.pop_scope();
    }

    Ok(elem_id)
}

/// Parse element content (text, child elements, CDATA, comments, PIs).
/// Uses lazy allocation: text content is borrowed from input when possible.
///
/// `depth` is the depth of the *parent* element; any child element parsed here
/// will be at `depth + 1` and is checked against `max_depth`.
#[allow(clippy::too_many_arguments)]
fn parse_content<'a>(
    cursor: &mut Cursor<'a>,
    doc: &mut Document<'a>,
    parent: NodeId,
    ns_resolver: &mut Option<NamespaceResolver<'a>>,
    entities: &EntityMap,
    entity_cache: &mut EntityCache,
    depth: u32,
    max_depth: u32,
) -> XmlResult<()> {
    // Lazy text buffer: tracks start position for borrowing, switches to owned on entity/\r
    enum TextBuf {
        Empty,
        Borrowed { start: usize },
        Owned(String),
    }

    impl TextBuf {
        fn flush<'a>(
            self,
            input: &'a str,
            doc: &mut Document<'a>,
            parent: NodeId,
            byte_pos: usize,
            end_pos: usize,
        ) {
            match self {
                TextBuf::Empty => {}
                TextBuf::Borrowed { start } => {
                    if start < end_pos {
                        let text = Cow::Borrowed(&input[start..end_pos]);
                        let id = doc.alloc_node(NodeKind::Text(text), start);
                        doc.set_byte_end_pos(id, end_pos);
                        doc.append_child_unchecked(parent, id);
                    }
                }
                TextBuf::Owned(s) => {
                    if !s.is_empty() {
                        let id = doc.alloc_node(NodeKind::Text(Cow::Owned(s)), byte_pos);
                        doc.set_byte_end_pos(id, end_pos);
                        doc.append_child_unchecked(parent, id);
                    }
                }
            }
        }

        fn switch_to_owned(&mut self, input: &str, end_pos: usize) {
            match self {
                TextBuf::Empty => {
                    *self = TextBuf::Owned(String::new());
                }
                TextBuf::Borrowed { start } => {
                    let s = input[*start..end_pos].to_string();
                    *self = TextBuf::Owned(s);
                }
                TextBuf::Owned(_) => {} // already owned
            }
        }

        fn push_str(&mut self, input: &str, end_pos: usize, s: &str) {
            self.switch_to_owned(input, end_pos);
            if let TextBuf::Owned(ref mut buf) = self {
                buf.push_str(s);
            }
        }

        fn push_char(&mut self, input: &str, end_pos: usize, c: char) {
            self.switch_to_owned(input, end_pos);
            if let TextBuf::Owned(ref mut buf) = self {
                buf.push(c);
            }
        }
    }

    let text_start_pos = cursor.pos;
    let mut text_buf: TextBuf = TextBuf::Empty;

    loop {
        if cursor.pos >= cursor.input.len() {
            return Err(XmlError::UnexpectedEof);
        }

        // Batch scan: find the next interesting byte (<, &, \r, ])
        let bytes = cursor.input.as_bytes();
        let scan_start = cursor.pos;
        let (advance, has_non_ascii_or_control) =
            crate::simd::scan_content_delimiters(&bytes[scan_start..]);
        let i = scan_start + advance;

        // Accumulate clean bytes in bulk
        if i > scan_start {
            // Only validate XML chars if we saw non-ASCII or control bytes
            if has_non_ascii_or_control {
                let chunk = &cursor.input[scan_start..i];
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
            match &mut text_buf {
                TextBuf::Empty => {
                    text_buf = TextBuf::Borrowed { start: scan_start };
                }
                TextBuf::Borrowed { .. } => {
                    // Just extend the borrowed range
                }
                TextBuf::Owned(ref mut buf) => {
                    buf.push_str(&cursor.input[scan_start..i]);
                }
            }
            cursor.pos = i;
        }

        if cursor.pos >= cursor.input.len() {
            return Err(XmlError::UnexpectedEof);
        }

        // Dispatch on the found byte
        match bytes[cursor.pos] {
            b'<' => {
                // Peek at next byte to determine what kind of markup
                match bytes.get(cursor.pos + 1) {
                    Some(b'/') => {
                        // End tag - flush text and return
                        text_buf.flush(cursor.input, doc, parent, text_start_pos, cursor.pos);
                        return Ok(());
                    }
                    Some(b'!') => {
                        text_buf.flush(cursor.input, doc, parent, text_start_pos, cursor.pos);
                        text_buf = TextBuf::Empty;
                        if cursor.starts_with("<![CDATA[") {
                            let start = cursor.pos;
                            let cdata = parse_cdata(cursor)?;
                            let id = doc.alloc_node(NodeKind::CData(cdata), start);
                            doc.set_byte_end_pos(id, cursor.pos);
                            doc.append_child_unchecked(parent, id);
                        } else if cursor.starts_with("<!--") {
                            let start = cursor.pos;
                            let comment = parse_comment(cursor)?;
                            let id = doc.alloc_node(NodeKind::Comment(comment), start);
                            doc.set_byte_end_pos(id, cursor.pos);
                            doc.append_child_unchecked(parent, id);
                        } else {
                            return Err(XmlError::well_formedness(
                                "Invalid markup in element content",
                                cursor.line(),
                                cursor.column(),
                            ));
                        }
                    }
                    Some(b'?') => {
                        text_buf.flush(cursor.input, doc, parent, text_start_pos, cursor.pos);
                        text_buf = TextBuf::Empty;
                        let start = cursor.pos;
                        let pi = parse_pi(cursor)?;
                        let id = doc.alloc_node(NodeKind::ProcessingInstruction(pi), start);
                        doc.set_byte_end_pos(id, cursor.pos);
                        doc.append_child_unchecked(parent, id);
                    }
                    _ => {
                        // Child element
                        text_buf.flush(cursor.input, doc, parent, text_start_pos, cursor.pos);
                        text_buf = TextBuf::Empty;
                        parse_element(
                            cursor,
                            doc,
                            parent,
                            ns_resolver,
                            entities,
                            entity_cache,
                            depth + 1,
                            max_depth,
                        )?;
                    }
                }
            }
            b'&' => {
                let before_pos = cursor.pos;
                let resolved = parse_reference_with_entities(cursor, entities, entity_cache)?;
                text_buf.push_str(cursor.input, before_pos, &resolved);
            }
            b'\r' => {
                // Normalize \r\n and standalone \r to \n (XML 1.0 section 2.11)
                let before_pos = cursor.pos;
                cursor.pos += 1; // skip the \r
                if cursor.peek_byte() == Some(b'\n') {
                    cursor.pos += 1; // skip the \n
                }
                text_buf.push_char(cursor.input, before_pos, '\n');
            }
            b']' => {
                // Check for illegal ]]> in content
                if cursor.starts_with("]]>") {
                    return Err(XmlError::well_formedness(
                        "']]>' not allowed in element content",
                        cursor.line(),
                        cursor.column(),
                    ));
                }
                // Just a regular ] character
                match &mut text_buf {
                    TextBuf::Empty => {
                        text_buf = TextBuf::Borrowed { start: cursor.pos };
                        cursor.advance_no_newlines(1);
                    }
                    TextBuf::Borrowed { .. } => {
                        cursor.advance_no_newlines(1);
                    }
                    TextBuf::Owned(ref mut buf) => {
                        buf.push(']');
                        cursor.advance_no_newlines(1);
                    }
                }
            }
            _ => unreachable!(),
        }
    }
}

/// Parse a CDATA section. Returns borrowed slice (CDATA never has entity expansion).
fn parse_cdata<'a>(cursor: &mut Cursor<'a>) -> XmlResult<Cow<'a, str>> {
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
    Ok(content)
}

#[cfg(test)]
mod tests {
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
}
