//! XML 1.0 (Fifth Edition) parser with well-formedness checking.
//!
//! This module implements a recursive-descent parser that tokenizes XML input
//! and builds a [`Document`] tree. It enforces the well-formedness constraints
//! defined in the XML 1.0 specification.

use std::collections::HashMap;

use crate::dom::{
    Attribute, Document, Element, NodeId, NodeKind, ProcessingInstruction, QName, XmlDeclaration,
};
use crate::error::{XmlError, XmlResult};
use crate::namespace::NamespaceResolver;

/// A map of general entity names to their replacement text.
type EntityMap = HashMap<String, String>;

/// The XML 1.0 parser.
pub struct Parser {
    /// Whether to resolve namespaces during parsing.
    namespace_aware: bool,
}

impl Parser {
    /// Create a new parser with namespace awareness enabled.
    pub fn new() -> Self {
        Parser {
            namespace_aware: true,
        }
    }

    /// Create a new parser with configurable namespace awareness.
    pub fn with_namespace_aware(namespace_aware: bool) -> Self {
        Parser { namespace_aware }
    }

    /// Parse an XML string into a [`Document`].
    pub fn parse(&self, input: &str) -> XmlResult<Document> {
        let mut cursor = Cursor::new(input);
        let mut doc = Document::new();
        let mut ns_resolver = if self.namespace_aware {
            Some(NamespaceResolver::new())
        } else {
            None
        };
        let mut entities = EntityMap::new();

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
                let comment = parse_comment(&mut cursor)?;
                let id = doc.alloc_node(NodeKind::Comment(comment), cursor.line, cursor.column);
                doc.append_child(root_id, id);
            } else if cursor.starts_with("<?") {
                let pi = parse_pi(&mut cursor)?;
                let id = doc.alloc_node(
                    NodeKind::ProcessingInstruction(pi),
                    cursor.line,
                    cursor.column,
                );
                doc.append_child(root_id, id);
            } else if cursor.starts_with("<") {
                if found_root {
                    return Err(XmlError::well_formedness(
                        "Only one root element is allowed",
                        cursor.line,
                        cursor.column,
                    ));
                }
                parse_element(&mut cursor, &mut doc, root_id, &mut ns_resolver, &entities)?;
                found_root = true;
            } else {
                // Non-whitespace text outside root element
                return Err(XmlError::well_formedness(
                    "Content found outside of root element",
                    cursor.line,
                    cursor.column,
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

        Ok(doc)
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Cursor ─────────────────────────────────────────────

/// A cursor over the input string that tracks position (line/column).
struct Cursor<'a> {
    input: &'a str,
    pos: usize,
    line: usize,
    column: usize,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a str) -> Self {
        Cursor {
            input,
            pos: 0,
            line: 1,
            column: 1,
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

    fn starts_with(&self, prefix: &str) -> bool {
        self.remaining().starts_with(prefix)
    }

    fn advance(&mut self, n: usize) {
        let bytes = &self.input.as_bytes()[self.pos..self.pos + n];
        for &b in bytes {
            if b == b'\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
        self.pos += n;
    }

    fn advance_char(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.advance(c.len_utf8());
        Some(c)
    }

    fn skip_bom(&mut self) {
        if self.remaining().starts_with('\u{FEFF}') {
            self.advance('\u{FEFF}'.len_utf8());
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if is_xml_whitespace(c) {
                self.advance_char();
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, expected: &str) -> XmlResult<()> {
        if self.starts_with(expected) {
            self.advance(expected.len());
            Ok(())
        } else {
            Err(XmlError::parse(
                format!("Expected '{}'", expected),
                self.line,
                self.column,
            ))
        }
    }

    /// Read until the given delimiter is found. Returns the text before the delimiter.
    /// The delimiter is consumed.
    fn read_until(&mut self, delimiter: &str) -> XmlResult<String> {
        if let Some(idx) = self.remaining().find(delimiter) {
            let text = self.remaining()[..idx].to_string();
            self.advance(idx + delimiter.len());
            Ok(text)
        } else {
            Err(XmlError::parse(
                format!("Expected '{}'", delimiter),
                self.line,
                self.column,
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

/// Parse an XML Name.
fn parse_name(cursor: &mut Cursor) -> XmlResult<String> {
    let mut name = String::new();
    match cursor.peek() {
        Some(c) if is_name_start_char(c) => {
            cursor.advance_char();
            name.push(c);
        }
        _ => {
            return Err(XmlError::parse(
                "Expected XML name",
                cursor.line,
                cursor.column,
            ));
        }
    }
    while let Some(c) = cursor.peek() {
        if is_name_char(c) {
            cursor.advance_char();
            name.push(c);
        } else {
            break;
        }
    }
    Ok(name)
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
fn parse_xml_declaration(cursor: &mut Cursor) -> XmlResult<XmlDeclaration> {
    cursor.expect("<?xml")?;

    // Must have whitespace after "<?xml"
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::parse(
            "Expected whitespace after '<?xml'",
            cursor.line,
            cursor.column,
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
    if version != "1.0" && version != "1.1" {
        return Err(XmlError::well_formedness(
            format!("Invalid XML version: '{}'", version),
            cursor.line,
            cursor.column,
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
                cursor.line,
                cursor.column,
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
                cursor.line,
                cursor.column,
            ));
        }
        encoding = Some(enc);

        let has_ws_after_encoding = cursor.peek().map(is_xml_whitespace).unwrap_or(false);
        cursor.skip_whitespace();
        if cursor.starts_with("standalone") {
            if !has_ws_after_encoding {
                return Err(XmlError::parse(
                    "Expected whitespace before 'standalone'",
                    cursor.line,
                    cursor.column,
                ));
            }
            let val = parse_standalone(cursor)?;
            standalone = Some(val);
        }
    } else if cursor.starts_with("standalone") {
        if !has_ws_after_version {
            return Err(XmlError::parse(
                "Expected whitespace before 'standalone'",
                cursor.line,
                cursor.column,
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
    if val != "yes" && val != "no" {
        return Err(XmlError::well_formedness(
            format!(
                "Invalid standalone value: '{}' (must be 'yes' or 'no')",
                val
            ),
            cursor.line,
            cursor.column,
        ));
    }
    Ok(val == "yes")
}

/// Parse a quoted attribute value (handles both `"` and `'`).
fn parse_quoted_value(cursor: &mut Cursor) -> XmlResult<String> {
    parse_quoted_value_with_entities(cursor, &HashMap::new())
}

/// Parse a quoted attribute value with entity resolution.
fn parse_quoted_value_with_entities(
    cursor: &mut Cursor,
    entities: &EntityMap,
) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote character",
                cursor.line,
                cursor.column,
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
                let resolved = parse_reference_with_entities(cursor, entities)?;
                value.push_str(&resolved);
            }
            Some('<') => {
                return Err(XmlError::well_formedness(
                    "'<' not allowed in attribute values",
                    cursor.line,
                    cursor.column,
                ));
            }
            Some(c) => {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X}", c as u32),
                        cursor.line,
                        cursor.column,
                    ));
                }
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Parse a character or entity reference (`&amp;`, `&#x41;`, etc.).
fn parse_reference(cursor: &mut Cursor) -> XmlResult<String> {
    parse_reference_with_entities(cursor, &HashMap::new())
}

/// Parse a character or entity reference with custom entity resolution.
fn parse_reference_with_entities(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<String> {
    cursor.expect("&")?;
    if cursor.starts_with("#x") {
        // Hexadecimal character reference (must be lowercase 'x' per XML 1.0 [66])
        cursor.advance(2);
        let mut hex = String::new();
        while let Some(c) = cursor.peek() {
            if c == ';' {
                cursor.advance_char();
                break;
            }
            cursor.advance_char();
            hex.push(c);
        }
        let code = u32::from_str_radix(&hex, 16).map_err(|_| {
            XmlError::parse(
                format!("Invalid hex character reference: {}", hex),
                cursor.line,
                cursor.column,
            )
        })?;
        let c = char::from_u32(code).ok_or_else(|| {
            XmlError::parse(
                format!("Invalid character reference: U+{:04X}", code),
                cursor.line,
                cursor.column,
            )
        })?;
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!(
                    "Character reference U+{:04X} is not a valid XML character",
                    code
                ),
                cursor.line,
                cursor.column,
            ));
        }
        Ok(c.to_string())
    } else if cursor.starts_with("#") {
        // Decimal character reference
        cursor.advance(1);
        let mut dec = String::new();
        while let Some(c) = cursor.peek() {
            if c == ';' {
                cursor.advance_char();
                break;
            }
            cursor.advance_char();
            dec.push(c);
        }
        let code: u32 = dec.parse().map_err(|_| {
            XmlError::parse(
                format!("Invalid decimal character reference: {}", dec),
                cursor.line,
                cursor.column,
            )
        })?;
        let c = char::from_u32(code).ok_or_else(|| {
            XmlError::parse(
                format!("Invalid character reference: U+{:04X}", code),
                cursor.line,
                cursor.column,
            )
        })?;
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!(
                    "Character reference U+{:04X} is not a valid XML character",
                    code
                ),
                cursor.line,
                cursor.column,
            ));
        }
        Ok(c.to_string())
    } else {
        // Named entity reference
        let name = parse_name(cursor)?;
        cursor.expect(";")?;
        match name.as_str() {
            "lt" => Ok("<".to_string()),
            "gt" => Ok(">".to_string()),
            "amp" => Ok("&".to_string()),
            "apos" => Ok("'".to_string()),
            "quot" => Ok("\"".to_string()),
            _ => {
                if let Some(value) = entities.get(&name) {
                    // Fully expand the entity value, resolving nested entity refs.
                    let expanded = expand_entity_value(
                        value,
                        entities,
                        &mut vec![name.clone()],
                        cursor.line,
                        cursor.column,
                    )?;
                    // Validate well-formedness of the entity replacement text.
                    // We use expand_entity_value_no_builtins which:
                    // - Resolves user-defined entity references (e.g. &e2; → v)
                    // - Keeps built-in entity references as-is (&lt; stays &lt;)
                    // This means:
                    // - Bare '&' from &#38; stays as bare '&' → caught as malformed
                    // - '&lt;' stays as '&lt;' → fine, it's a valid entity reference
                    // - '<' from &#60; stays as '<' → caught as markup, validated
                    let validation_text = expand_entity_value_no_builtins(
                        value,
                        entities,
                        &mut vec![name.clone()],
                        cursor.line,
                        cursor.column,
                    )?;
                    validate_entity_as_content(
                        &validation_text,
                        entities,
                        cursor.line,
                        cursor.column,
                    )?;
                    Ok(expanded)
                } else {
                    Err(XmlError::well_formedness(
                        format!("Unknown entity reference: &{};", name),
                        cursor.line,
                        cursor.column,
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
/// This is used to check if the expanded text contains literal '<' from entity values
/// (which is markup) vs '<' from &lt; (which is text).
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
/// The entity replacement text must parse as valid content (elements, CDATA, PIs, etc.).
fn validate_entity_as_content(
    text: &str,
    _entities: &EntityMap,
    line: usize,
    col: usize,
) -> XmlResult<()> {
    // Wrap in a temporary element and try to parse the whole thing
    let wrapped = format!("<__entity_wrapper__>{}</__entity_wrapper__>", text);
    // We use a basic namespace-unaware parse just to check well-formedness
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
fn parse_comment(cursor: &mut Cursor) -> XmlResult<String> {
    cursor.expect("<!--")?;
    let content = cursor.read_until("-->")?;
    // Well-formedness: comments must not contain "--"
    if content.contains("--") {
        return Err(XmlError::well_formedness(
            "Comments must not contain '--'",
            cursor.line,
            cursor.column,
        ));
    }
    // Well-formedness: comment must not end with '-' (i.e. "--->" is invalid)
    if content.ends_with('-') {
        return Err(XmlError::well_formedness(
            "Comments must not end with '-'",
            cursor.line,
            cursor.column,
        ));
    }
    // Validate all characters are valid XML chars
    for c in content.chars() {
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!("Invalid XML character U+{:04X} in comment", c as u32),
                cursor.line,
                cursor.column,
            ));
        }
    }
    Ok(content)
}

/// Parse a processing instruction (`<?target data?>`).
fn parse_pi(cursor: &mut Cursor) -> XmlResult<ProcessingInstruction> {
    cursor.expect("<?")?;
    let target = parse_name(cursor)?;
    // Well-formedness: target must not be "xml" (case-insensitive)
    if target.eq_ignore_ascii_case("xml") {
        return Err(XmlError::well_formedness(
            "Processing instruction target must not be 'xml'",
            cursor.line,
            cursor.column,
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
            cursor.line,
            cursor.column,
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
                cursor.line,
                cursor.column,
            ));
        }
    }
    Ok(ProcessingInstruction {
        target,
        data: Some(data),
    })
}

/// Parse prolog miscellaneous content (comments, PIs, whitespace, DOCTYPE).
fn parse_misc(
    cursor: &mut Cursor,
    doc: &mut Document,
    parent: NodeId,
    entities: &mut EntityMap,
) -> XmlResult<()> {
    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            break;
        }
        if cursor.starts_with("<!--") {
            let comment = parse_comment(cursor)?;
            let id = doc.alloc_node(NodeKind::Comment(comment), cursor.line, cursor.column);
            doc.append_child(parent, id);
        } else if cursor.starts_with("<?") {
            let pi = parse_pi(cursor)?;
            let id = doc.alloc_node(
                NodeKind::ProcessingInstruction(pi),
                cursor.line,
                cursor.column,
            );
            doc.append_child(parent, id);
        } else if cursor.starts_with("<!DOCTYPE") {
            parse_doctype(cursor, entities)?;
        } else {
            break;
        }
    }
    Ok(())
}

/// Parse a DOCTYPE declaration, including internal subset.
/// Collects general entity declarations for use in document content.
fn parse_doctype(cursor: &mut Cursor, entities: &mut EntityMap) -> XmlResult<()> {
    cursor.expect("<!DOCTYPE")?;

    // Must have whitespace after <!DOCTYPE
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!DOCTYPE'",
            cursor.line,
            cursor.column,
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
                cursor.line,
                cursor.column,
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
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_pubid_literal(cursor)?;
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace between public and system literal",
                cursor.line,
                cursor.column,
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
                cursor.line,
                cursor.column,
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
                cursor.line,
                cursor.column,
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
                        cursor.line,
                        cursor.column,
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
                cursor.line,
                cursor.column,
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
                cursor.line,
                cursor.column,
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
            cursor.line,
            cursor.column,
        ));
    }
    if cursor.starts_with("?>") {
        cursor.expect("?>")?;
        return Ok(());
    }
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::parse(
            "Expected whitespace after PI target",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();
    cursor.read_until("?>")?;
    Ok(())
}

/// Parse a parameter entity reference (`%name;`).
fn parse_pe_reference(cursor: &mut Cursor) -> XmlResult<String> {
    cursor.expect("%")?;
    let name = parse_name(cursor)?;
    cursor.expect(";")?;
    // We don't resolve PE references, but we validate the syntax
    Ok(name)
}

/// Reject a PE reference inside a markup declaration in the internal subset.
/// Per XML 1.0 §2.8 WFC: PEs in Internal Subset:
/// "In the internal DTD subset, parameter-entity references MUST NOT
///  occur within markup declarations"
fn reject_pe_in_markup_decl(cursor: &Cursor) -> XmlResult<()> {
    Err(XmlError::well_formedness(
        "Parameter entity reference not allowed within markup declaration in internal subset",
        cursor.line,
        cursor.column,
    ))
}

/// Parse an ELEMENT declaration (`<!ELEMENT name contentspec>`).
fn parse_element_decl(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("<!ELEMENT")?;

    // Must have whitespace after <!ELEMENT
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!ELEMENT'",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // Element name
    parse_name(cursor)?;

    // Must have whitespace before contentspec
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after element name in ELEMENT declaration",
            cursor.line,
            cursor.column,
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
        // PE reference inside element declaration — reject per WFC
        reject_pe_in_markup_decl(cursor)?;
        Ok(())
    } else {
        Err(XmlError::well_formedness(
            "Expected content specification (EMPTY, ANY, or content model)",
            cursor.line,
            cursor.column,
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
        // Mixed: (#PCDATA) or (#PCDATA | name1 | name2)*
        if cursor.peek() == Some(')') {
            cursor.advance_char();
            // (#PCDATA) may have optional '*' but nothing else
            if cursor.peek() == Some('*') {
                cursor.advance_char();
            }
            return Ok(());
        }
        // Must be (#PCDATA | name1 | name2 ...)*
        loop {
            cursor.skip_whitespace();
            if cursor.peek() == Some(')') {
                cursor.advance_char();
                // Mixed content with alternatives MUST end with )*
                if cursor.peek() != Some('*') {
                    return Err(XmlError::well_formedness(
                        "Mixed content model with alternatives must end with ')*'",
                        cursor.line,
                        cursor.column,
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
                // Must be a Name, and must NOT be wrapped in parens
                if cursor.peek() == Some('(') {
                    return Err(XmlError::well_formedness(
                        "Parenthesized group not allowed in Mixed content model",
                        cursor.line,
                        cursor.column,
                    ));
                }
                // #PCDATA alternatives cannot have occurrence indicators
                parse_name(cursor)?;
                cursor.skip_whitespace();
                if cursor.peek() == Some('*')
                    || cursor.peek() == Some('+')
                    || cursor.peek() == Some('?')
                {
                    return Err(XmlError::well_formedness(
                        "Occurrence indicator not allowed on elements in Mixed content model",
                        cursor.line,
                        cursor.column,
                    ));
                }
            }
        }
    }

    // children content model: cp ((',' cp)* | ('|' cp)*)
    parse_cp(cursor)?;
    cursor.skip_whitespace();

    // Empty group () is not allowed
    if cursor.peek() == Some(')') {
        cursor.advance_char();
        // Optional occurrence indicator
        if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
            cursor.advance_char();
        }
        return Ok(());
    }

    // Determine separator: ',' for seq, '|' for choice
    let sep = match cursor.peek() {
        Some(',') => ',',
        Some('|') => '|',
        _ => {
            return Err(XmlError::well_formedness(
                "Expected ',' or '|' or ')' in content model",
                cursor.line,
                cursor.column,
            ));
        }
    };

    loop {
        cursor.skip_whitespace();
        if cursor.peek() == Some(')') {
            cursor.advance_char();
            // Optional occurrence indicator (must be directly attached)
            if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
                cursor.advance_char();
            }
            return Ok(());
        }
        if cursor.peek() == Some(sep) {
            cursor.advance_char();
        } else if cursor.peek() == Some(',') || cursor.peek() == Some('|') {
            // Mixing separators is not allowed
            return Err(XmlError::well_formedness(
                "Cannot mix ',' and '|' in content model group",
                cursor.line,
                cursor.column,
            ));
        } else {
            return Err(XmlError::well_formedness(
                format!("Expected '{}' or ')' in content model", sep),
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_cp(cursor)?;
    }
}

/// Parse a content particle (Name or nested group, with optional occurrence indicator).
fn parse_cp(cursor: &mut Cursor) -> XmlResult<()> {
    if cursor.peek() == Some('(') {
        // Nested group — cannot contain #PCDATA (Mixed content is only at top level)
        parse_children_group(cursor)?;
    } else if cursor.starts_with("%") {
        reject_pe_in_markup_decl(cursor)?;
    } else {
        parse_name(cursor)?;
        // Optional occurrence indicator directly after name (no space)
        if matches!(cursor.peek(), Some('*') | Some('+') | Some('?')) {
            cursor.advance_char();
        }
    }
    Ok(())
}

/// Parse a children group (content model without Mixed content).
/// This is used for nested groups inside content particles.
fn parse_children_group(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("(")?;
    cursor.skip_whitespace();

    // #PCDATA not allowed in nested groups
    if cursor.starts_with("#PCDATA") {
        return Err(XmlError::well_formedness(
            "#PCDATA not allowed in nested content model group",
            cursor.line,
            cursor.column,
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
                cursor.line,
                cursor.column,
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
                cursor.line,
                cursor.column,
            ));
        } else {
            return Err(XmlError::well_formedness(
                format!("Expected '{}' or ')' in content model", sep),
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_cp(cursor)?;
    }
}

/// Parse an ATTLIST declaration (`<!ATTLIST name attdef* >`).
fn parse_attlist_decl(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<()> {
    cursor.expect("<!ATTLIST")?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!ATTLIST'",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // Element name
    parse_name(cursor)?;

    // Parse attribute definitions until >
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
    // Attribute name
    parse_name(cursor)?;

    // Must have whitespace
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after attribute name",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // Attribute type
    parse_att_type(cursor)?;

    // Must have whitespace
    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after attribute type",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // Default declaration
    parse_default_decl(cursor, entities)?;

    Ok(())
}

/// Parse an attribute type (CDATA, ID, IDREF, etc., or enumeration).
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
        // Must have whitespace then '(' enumeration ')'
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'NOTATION'",
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_enumeration(cursor)?;
    } else if cursor.peek() == Some('(') {
        // Enumeration
        parse_enumeration(cursor)?;
    } else if cursor.starts_with("%") {
        reject_pe_in_markup_decl(cursor)?;
    } else {
        return Err(XmlError::well_formedness(
            "Expected attribute type (CDATA, ID, IDREF, etc.)",
            cursor.line,
            cursor.column,
        ));
    }
    Ok(())
}

/// Parse an enumeration (`(value1 | value2 | ...)`).
fn parse_enumeration(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("(")?;
    cursor.skip_whitespace();

    // Parse first value (Nmtoken for enumeration, Name for NOTATION)
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

/// Parse an Nmtoken (one or more NameChars).
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
            cursor.line,
            cursor.column,
        ));
    }
    Ok(token)
}

/// Parse a default declaration (#REQUIRED, #IMPLIED, #FIXED, or a default value).
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
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_att_value_in_dtd(cursor, entities)?;
    } else if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
        parse_att_value_in_dtd(cursor, entities)?;
    } else {
        return Err(XmlError::well_formedness(
            "Expected default declaration (#REQUIRED, #IMPLIED, #FIXED, or default value)",
            cursor.line,
            cursor.column,
        ));
    }
    Ok(())
}

/// Parse an attribute value inside a DTD declaration.
/// Entity references in DTD attribute defaults must refer to defined entities.
fn parse_att_value_in_dtd(cursor: &mut Cursor, entities: &EntityMap) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote character for attribute value",
                cursor.line,
                cursor.column,
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
                let resolved = parse_reference_with_entities(cursor, entities)?;
                value.push_str(&resolved);
            }
            Some('<') => {
                return Err(XmlError::well_formedness(
                    "'<' not allowed in attribute value",
                    cursor.line,
                    cursor.column,
                ));
            }
            Some(c) => {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X} in DTD", c as u32),
                        cursor.line,
                        cursor.column,
                    ));
                }
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Parse an ENTITY declaration (`<!ENTITY name "value">` etc.).
fn parse_entity_decl(cursor: &mut Cursor, entities: &mut EntityMap) -> XmlResult<()> {
    cursor.expect("<!ENTITY")?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!ENTITY'",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // Check for parameter entity (% name)
    let is_pe = cursor.peek() == Some('%');
    if is_pe {
        cursor.advance_char();
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after '%' in parameter entity declaration",
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
    }

    // Entity name
    let name = parse_name(cursor)?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after entity name",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // EntityDef: EntityValue | (ExternalID NDataDecl?)
    if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
        // EntityValue - a quoted string that may contain PE references and char references
        let value = parse_entity_value(cursor)?;
        cursor.skip_whitespace();

        // PE should not have NDATA
        if !is_pe {
            // Store the general entity
            entities.entry(name).or_insert(value);
        }
    } else if cursor.starts_with("SYSTEM") || cursor.starts_with("PUBLIC") {
        // External entity
        if cursor.starts_with("SYSTEM") {
            cursor.advance(6);
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace after 'SYSTEM'",
                    cursor.line,
                    cursor.column,
                ));
            }
            cursor.skip_whitespace();
            parse_system_literal(cursor)?;
        } else {
            cursor.advance(6); // PUBLIC
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace after 'PUBLIC'",
                    cursor.line,
                    cursor.column,
                ));
            }
            cursor.skip_whitespace();
            parse_pubid_literal(cursor)?;
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace between public and system literal",
                    cursor.line,
                    cursor.column,
                ));
            }
            cursor.skip_whitespace();
            parse_system_literal(cursor)?;
        }
        let has_ws_before_ndata = cursor.peek().map(is_xml_whitespace).unwrap_or(false);
        cursor.skip_whitespace();

        // Optional NDATA for general entities (not PE)
        if !is_pe && cursor.starts_with("NDATA") {
            if !has_ws_before_ndata {
                return Err(XmlError::well_formedness(
                    "Expected whitespace before 'NDATA'",
                    cursor.line,
                    cursor.column,
                ));
            }
            cursor.advance(5);
            if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace after 'NDATA'",
                    cursor.line,
                    cursor.column,
                ));
            }
            cursor.skip_whitespace();
            parse_name(cursor)?;
            cursor.skip_whitespace();
        } else if is_pe && cursor.starts_with("NDATA") {
            return Err(XmlError::well_formedness(
                "NDATA not allowed on parameter entity declarations",
                cursor.line,
                cursor.column,
            ));
        }
    } else if cursor.starts_with("%") {
        // PE reference inside entity declaration — reject per WFC
        reject_pe_in_markup_decl(cursor)?;
        cursor.skip_whitespace();
    } else {
        return Err(XmlError::well_formedness(
            "Expected entity value or external ID in ENTITY declaration",
            cursor.line,
            cursor.column,
        ));
    }

    cursor.skip_whitespace();
    cursor.expect(">")?;
    Ok(())
}

/// Parse an EntityValue (a quoted string that may contain PE references, char references,
/// and bypassed entity references).
fn parse_entity_value(cursor: &mut Cursor) -> XmlResult<String> {
    let quote = match cursor.peek() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => {
            return Err(XmlError::parse(
                "Expected quote for entity value",
                cursor.line,
                cursor.column,
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
                    // Character reference - resolve it
                    let resolved = parse_reference(cursor)?;
                    value.push_str(&resolved);
                } else {
                    // Entity reference - bypass it (keep as-is for later resolution)
                    // But still validate it's well-formed
                    cursor.advance(1); // skip &
                    let name = parse_name(cursor)?;
                    cursor.expect(";")?;
                    value.push('&');
                    value.push_str(&name);
                    value.push(';');
                }
            }
            Some('%') => {
                // PE reference in entity value inside internal subset violates
                // the "PEs in Internal Subset" WFC (XML 1.0 §2.8):
                // "In the internal DTD subset, parameter-entity references
                //  MUST NOT occur within markup declarations"
                return Err(XmlError::well_formedness(
                    "Parameter entity reference not allowed within markup declaration in internal subset",
                    cursor.line,
                    cursor.column,
                ));
            }
            Some(c) => {
                if !is_xml_char(c) {
                    return Err(XmlError::well_formedness(
                        format!("Invalid XML character U+{:04X} in entity value", c as u32),
                        cursor.line,
                        cursor.column,
                    ));
                }
                cursor.advance_char();
                value.push(c);
            }
        }
    }
    Ok(value)
}

/// Parse a NOTATION declaration (`<!NOTATION name ExternalID>`).
fn parse_notation_decl(cursor: &mut Cursor) -> XmlResult<()> {
    cursor.expect("<!NOTATION")?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after '<!NOTATION'",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // Notation name
    parse_name(cursor)?;

    if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
        return Err(XmlError::well_formedness(
            "Expected whitespace after notation name",
            cursor.line,
            cursor.column,
        ));
    }
    cursor.skip_whitespace();

    // ExternalID or PublicID
    if cursor.starts_with("SYSTEM") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'SYSTEM'",
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_system_literal(cursor)?;
    } else if cursor.starts_with("PUBLIC") {
        cursor.advance(6);
        if !cursor.peek().map(is_xml_whitespace).unwrap_or(false) {
            return Err(XmlError::well_formedness(
                "Expected whitespace after 'PUBLIC'",
                cursor.line,
                cursor.column,
            ));
        }
        cursor.skip_whitespace();
        parse_pubid_literal(cursor)?;
        cursor.skip_whitespace();
        // Optional system literal for NOTATION PUBLIC
        if cursor.peek() == Some('"') || cursor.peek() == Some('\'') {
            parse_system_literal(cursor)?;
        }
    } else {
        return Err(XmlError::well_formedness(
            "Expected 'SYSTEM' or 'PUBLIC' in NOTATION declaration",
            cursor.line,
            cursor.column,
        ));
    }

    cursor.skip_whitespace();
    cursor.expect(">")?;
    Ok(())
}

/// Parse an element and its content recursively.
fn parse_element(
    cursor: &mut Cursor,
    doc: &mut Document,
    parent: NodeId,
    ns_resolver: &mut Option<NamespaceResolver>,
    entities: &EntityMap,
) -> XmlResult<NodeId> {
    let start_line = cursor.line;
    let start_col = cursor.column;

    cursor.expect("<")?;
    let tag_name = parse_name(cursor)?;

    // Parse attributes
    let mut raw_attrs: Vec<(String, String)> = Vec::new();
    let mut ns_decls: HashMap<String, String> = HashMap::new();

    loop {
        cursor.skip_whitespace();
        if cursor.is_eof() {
            return Err(XmlError::UnexpectedEof);
        }
        if cursor.starts_with("/>") || cursor.starts_with(">") {
            break;
        }
        let attr_name = parse_name(cursor)?;
        cursor.skip_whitespace();
        cursor.expect("=")?;
        cursor.skip_whitespace();
        let attr_value = parse_quoted_value_with_entities(cursor, entities)?;

        // Check for duplicate attributes
        if raw_attrs.iter().any(|(n, _)| n == &attr_name) {
            return Err(XmlError::well_formedness(
                format!("Duplicate attribute: {}", attr_name),
                cursor.line,
                cursor.column,
            ));
        }

        // Separate namespace declarations from regular attributes
        if attr_name == "xmlns" {
            ns_decls.insert(String::new(), attr_value.clone());
        } else if let Some(prefix) = attr_name.strip_prefix("xmlns:") {
            if prefix == "xmlns" {
                return Err(XmlError::namespace(
                    "The prefix 'xmlns' must not be declared",
                    cursor.line,
                    cursor.column,
                ));
            }
            if prefix == "xml" && attr_value != "http://www.w3.org/XML/1998/namespace" {
                return Err(XmlError::namespace(
                    "The prefix 'xml' must not be bound to any other namespace",
                    cursor.line,
                    cursor.column,
                ));
            }
            ns_decls.insert(prefix.to_string(), attr_value.clone());
        }

        raw_attrs.push((attr_name, attr_value));

        // After an attribute value, must have whitespace, '>', or '/>'
        // Missing whitespace between attributes is a well-formedness error
        if let Some(c) = cursor.peek() {
            if c != '>' && c != '/' && !is_xml_whitespace(c) {
                return Err(XmlError::well_formedness(
                    "Expected whitespace between attributes",
                    cursor.line,
                    cursor.column,
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
    let (prefix, local_name) = split_qname(&tag_name);
    let qname = if let Some(resolver) = ns_resolver.as_ref() {
        let ns_uri = if let Some(p) = prefix {
            resolver.resolve(p).map(|s| s.to_string()).ok_or_else(|| {
                XmlError::namespace(
                    format!("Undeclared namespace prefix: {}", p),
                    start_line,
                    start_col,
                )
            })?
        } else {
            // Default namespace
            resolver.resolve_default().unwrap_or("").to_string()
        };
        let ns = if ns_uri.is_empty() {
            None
        } else {
            Some(ns_uri)
        };
        QName {
            namespace_uri: ns,
            prefix: prefix.map(|s| s.to_string()),
            local_name: local_name.to_string(),
        }
    } else {
        QName::local(tag_name.clone())
    };

    // Resolve attribute QNames
    let mut resolved_attrs = Vec::new();
    for (attr_name, attr_value) in &raw_attrs {
        // Skip xmlns declarations from the attribute list
        if attr_name == "xmlns" || attr_name.starts_with("xmlns:") {
            continue;
        }
        let (a_prefix, a_local) = split_qname(attr_name);
        let a_qname = if let Some(resolver) = ns_resolver.as_ref() {
            if let Some(p) = a_prefix {
                let ns_uri = resolver.resolve(p).map(|s| s.to_string()).ok_or_else(|| {
                    XmlError::namespace(
                        format!("Undeclared namespace prefix: {}", p),
                        cursor.line,
                        cursor.column,
                    )
                })?;
                QName {
                    namespace_uri: Some(ns_uri),
                    prefix: Some(p.to_string()),
                    local_name: a_local.to_string(),
                }
            } else {
                // Unprefixed attributes are NOT in any namespace (per Namespaces spec)
                QName::local(a_local.to_string())
            }
        } else {
            QName::local(attr_name.clone())
        };
        resolved_attrs.push(Attribute {
            name: a_qname,
            value: attr_value.clone(),
        });
    }

    // Create element node
    let elem = Element {
        name: qname,
        attributes: resolved_attrs,
        namespace_declarations: ns_decls,
    };
    let elem_id = doc.alloc_node(NodeKind::Element(elem), start_line, start_col);
    doc.build_attribute_nodes(elem_id);
    doc.append_child(parent, elem_id);

    // Self-closing?
    if cursor.starts_with("/>") {
        cursor.expect("/>")?;
        if let Some(resolver) = ns_resolver.as_mut() {
            resolver.pop_scope();
        }
        return Ok(elem_id);
    }

    cursor.expect(">")?;

    // Parse element content
    parse_content(cursor, doc, elem_id, ns_resolver, entities)?;

    // Parse end tag
    cursor.expect("</")?;
    let end_tag_name = parse_name(cursor)?;
    cursor.skip_whitespace();
    cursor.expect(">")?;

    if end_tag_name != tag_name {
        return Err(XmlError::well_formedness(
            format!(
                "Mismatched end tag: expected </{}>, found </{}>",
                tag_name, end_tag_name
            ),
            cursor.line,
            cursor.column,
        ));
    }

    if let Some(resolver) = ns_resolver.as_mut() {
        resolver.pop_scope();
    }

    Ok(elem_id)
}

/// Parse element content (text, child elements, CDATA, comments, PIs).
fn parse_content(
    cursor: &mut Cursor,
    doc: &mut Document,
    parent: NodeId,
    ns_resolver: &mut Option<NamespaceResolver>,
    entities: &EntityMap,
) -> XmlResult<()> {
    let mut text_buf = String::new();
    let text_start_line = cursor.line;
    let text_start_col = cursor.column;

    loop {
        if cursor.is_eof() {
            return Err(XmlError::UnexpectedEof);
        }

        if cursor.starts_with("</") {
            // End tag - flush text and return
            if !text_buf.is_empty() {
                let id = doc.alloc_node(
                    NodeKind::Text(text_buf.clone()),
                    text_start_line,
                    text_start_col,
                );
                doc.append_child(parent, id);
                text_buf.clear();
            }
            return Ok(());
        }

        if cursor.starts_with("<![CDATA[") {
            // Flush text
            if !text_buf.is_empty() {
                let id = doc.alloc_node(
                    NodeKind::Text(text_buf.clone()),
                    text_start_line,
                    text_start_col,
                );
                doc.append_child(parent, id);
                text_buf.clear();
            }
            let cdata = parse_cdata(cursor)?;
            let id = doc.alloc_node(NodeKind::CData(cdata), cursor.line, cursor.column);
            doc.append_child(parent, id);
        } else if cursor.starts_with("<!--") {
            if !text_buf.is_empty() {
                let id = doc.alloc_node(
                    NodeKind::Text(text_buf.clone()),
                    text_start_line,
                    text_start_col,
                );
                doc.append_child(parent, id);
                text_buf.clear();
            }
            let comment = parse_comment(cursor)?;
            let id = doc.alloc_node(NodeKind::Comment(comment), cursor.line, cursor.column);
            doc.append_child(parent, id);
        } else if cursor.starts_with("<?") {
            if !text_buf.is_empty() {
                let id = doc.alloc_node(
                    NodeKind::Text(text_buf.clone()),
                    text_start_line,
                    text_start_col,
                );
                doc.append_child(parent, id);
                text_buf.clear();
            }
            let pi = parse_pi(cursor)?;
            let id = doc.alloc_node(
                NodeKind::ProcessingInstruction(pi),
                cursor.line,
                cursor.column,
            );
            doc.append_child(parent, id);
        } else if cursor.starts_with("<") {
            if !text_buf.is_empty() {
                let id = doc.alloc_node(
                    NodeKind::Text(text_buf.clone()),
                    text_start_line,
                    text_start_col,
                );
                doc.append_child(parent, id);
                text_buf.clear();
            }
            parse_element(cursor, doc, parent, ns_resolver, entities)?;
        } else if cursor.starts_with("&") {
            let resolved = parse_reference_with_entities(cursor, entities)?;
            text_buf.push_str(&resolved);
        } else if cursor.starts_with("]]>") {
            return Err(XmlError::well_formedness(
                "']]>' not allowed in element content",
                cursor.line,
                cursor.column,
            ));
        } else {
            // Regular text character
            let c = cursor.advance_char().unwrap();
            if !is_xml_char(c) {
                return Err(XmlError::well_formedness(
                    format!("Invalid XML character U+{:04X}", c as u32),
                    cursor.line,
                    cursor.column,
                ));
            }
            // Normalize \r\n and standalone \r to \n (XML 1.0 section 2.11)
            if c == '\r' {
                if cursor.peek() == Some('\n') {
                    cursor.advance_char();
                }
                text_buf.push('\n');
            } else {
                text_buf.push(c);
            }
        }
    }
}

/// Parse a CDATA section.
fn parse_cdata(cursor: &mut Cursor) -> XmlResult<String> {
    cursor.expect("<![CDATA[")?;
    let content = cursor.read_until("]]>")?;
    // Validate all characters are valid XML chars
    for c in content.chars() {
        if !is_xml_char(c) {
            return Err(XmlError::well_formedness(
                format!("Invalid XML character U+{:04X} in CDATA section", c as u32),
                cursor.line,
                cursor.column,
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
        assert_eq!(elem.name.local_name, "root");
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
        assert_eq!(decl.version, "1.0");
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
}
