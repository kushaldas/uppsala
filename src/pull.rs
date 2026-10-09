//! Pull-based XML event parser and the shared tokenizer behind it.
//!
//! The module has three layers:
//!
//! 1. `Tokenizer` (crate-private): the prolog/content/trailing state machine,
//!    start-tag scanning with namespace checks and QName resolution, text-run
//!    scanning, and the open-element stack. It is driven one syntactic unit at
//!    a time and reports each unit to a `TokenSink`.
//! 2. [`PullParser`] (public): drives the tokenizer with a sink that turns
//!    tokens into [`PullEvent`]s, buffered so one tag can yield several events
//!    (namespace bindings, self-closing elements).
//! 3. `DomSink` (crate-private): a sink that allocates arena nodes directly.
//!    [`Parser::parse`](crate::Parser::parse) uses it, so the DOM path never
//!    materializes a `PullEvent`. [`document_from_pull`](crate::pull::document_from_pull)
//!    remains the explicit
//!    event-stream-to-DOM path used by the differential tests.
//!
//! Both sinks share one tokenizer, so well-formedness checks, namespace rules,
//! entity-expansion budgets, and depth limits have a single implementation.
//! See ADR 0019 for why the DOM path does not consume events.

use std::borrow::Cow;
use std::collections::VecDeque;

use crate::dom::{
    Attribute, Document, Element, NodeId, NodeKind, ProcessingInstruction, QName, XmlDeclaration,
};
use crate::error::{XmlError, XmlResult};
use crate::namespace::NamespaceResolver;
use crate::parser::{
    borrow_from_cow, is_xml_char, parse_cdata, parse_comment, parse_doctype, parse_name, parse_pi,
    parse_quoted_value_with_entities, parse_reference_with_entities, parse_xml_declaration,
    split_qname, Cursor, EntityCache, EntityMap, DEFAULT_MAX_DEPTH, DEFAULT_MAX_ENTITY_EXPANSION,
};

/// A namespace declaration made by a start tag.
#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceDeclaration<'a> {
    /// The declared prefix, or `None` for the default namespace.
    pub prefix: Option<Cow<'a, str>>,
    /// The namespace URI.
    pub uri: Cow<'a, str>,
}

/// One event from the XML pull parser.
#[derive(Debug, Clone, PartialEq)]
pub enum PullEvent<'a> {
    /// XML declaration from the prolog.
    XmlDeclaration(XmlDeclaration<'a>),
    /// Raw `<!DOCTYPE ...>` declaration text.
    Doctype(Cow<'a, str>),
    /// Namespace binding that comes into scope on the next `StartElement`.
    StartNamespace {
        /// The declared prefix, or `None` for the default namespace.
        prefix: Option<Cow<'a, str>>,
        /// The namespace URI.
        uri: Cow<'a, str>,
    },
    /// Namespace binding leaving scope after an `EndElement`.
    EndNamespace,
    /// Element start tag.
    StartElement {
        /// Resolved element name.
        name: QName<'a>,
        /// Resolved non-namespace attributes.
        attributes: Vec<Attribute<'a>>,
        /// Namespace declarations carried on this element.
        namespace_declarations: Vec<(Cow<'a, str>, Cow<'a, str>)>,
        /// Byte offset of the opening `<`.
        byte_start: usize,
        /// Byte offset immediately after the start tag (`>` or `/>`).
        byte_end: usize,
        /// Element depth, with the document element at depth 0.
        depth: u32,
    },
    /// Element end tag, or the synthetic end corresponding to `/>`.
    EndElement {
        /// Resolved element name.
        name: QName<'a>,
        /// Byte offset of the opening `<` of the end tag. For `/>`, this is the
        /// opening `<` of the self-closing start tag.
        byte_start: usize,
        /// Byte offset immediately after the end tag.
        byte_end: usize,
        /// Element depth, with the document element at depth 0.
        depth: u32,
    },
    /// Character data after entity and line-ending normalization.
    Text {
        /// Text content.
        content: Cow<'a, str>,
        /// Source byte offset where this text run starts.
        byte_start: usize,
        /// Source byte offset where this text run ends.
        byte_end: usize,
    },
    /// CDATA section content.
    CData {
        /// CDATA content.
        content: Cow<'a, str>,
        /// Source byte offset of `<![CDATA[`.
        byte_start: usize,
        /// Source byte offset immediately after `]]>`.
        byte_end: usize,
    },
    /// Comment content.
    Comment {
        /// Comment text, without delimiters.
        content: Cow<'a, str>,
        /// Source byte offset of `<!--`.
        byte_start: usize,
        /// Source byte offset immediately after `-->`.
        byte_end: usize,
    },
    /// Processing instruction.
    ProcessingInstruction {
        /// Parsed PI.
        pi: ProcessingInstruction<'a>,
        /// Source byte offset of `<?`.
        byte_start: usize,
        /// Source byte offset immediately after `?>`.
        byte_end: usize,
    },
}

/// Receiver for the syntactic units produced by [`Tokenizer::step`].
///
/// One [`Tokenizer::step`] makes zero, one, or two sink calls:
///
/// - zero when the step only changes phase (skipping the BOM, entering the
///   prolog, leaving content for the trailing section) or when a text run
///   expands to nothing (an entity with empty replacement text);
/// - one for every ordinary unit (declaration, DOCTYPE, start tag, end tag,
///   text, CDATA, comment, PI);
/// - two for a self-closing tag: `start_element` immediately followed by
///   `end_element`.
///
/// Drivers must not infer from `step` returning `Ok(true)` that a token was
/// delivered. Sinks never fail: structural errors are the tokenizer's job,
/// and end-of-document checks belong to whoever drives the tokenizer to
/// completion.
pub(crate) trait TokenSink<'a> {
    fn xml_declaration(&mut self, decl: XmlDeclaration<'a>);
    fn doctype(&mut self, raw: Cow<'a, str>);
    fn start_element(
        &mut self,
        name: QName<'a>,
        attributes: Vec<Attribute<'a>>,
        namespace_declarations: Vec<(Cow<'a, str>, Cow<'a, str>)>,
        byte_start: usize,
        byte_end: usize,
        depth: u32,
    );
    fn end_element(&mut self, byte_start: usize, byte_end: usize, depth: u32);
    fn text(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize);
    fn cdata(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize);
    fn comment(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize);
    fn processing_instruction(
        &mut self,
        pi: ProcessingInstruction<'a>,
        byte_start: usize,
        byte_end: usize,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Start,
    Prolog,
    Content,
    Trailing,
    Done,
}

/// What the tokenizer must remember about an open element: the raw tag name
/// for end-tag matching and whether this element pushed a namespace scope.
/// Anything a sink needs (resolved name, node id, namespace count) lives on the
/// sink's own stack.
#[derive(Debug)]
struct OpenElement<'a> {
    raw_name: Cow<'a, str>,
    pushed_ns_scope: bool,
}

/// Shared XML tokenizer: one syntactic unit per [`Tokenizer::step`], reported
/// to a [`TokenSink`]. Fuses to the done state after any error.
pub(crate) struct Tokenizer<'a> {
    cursor: Cursor<'a>,
    max_depth: u32,
    forbid_dtd: bool,
    forbid_entities: bool,
    seen_doctype: bool,
    ns_resolver: Option<NamespaceResolver<'a>>,
    entities: EntityMap,
    entity_cache: EntityCache,
    entity_budget: usize,
    stack: Vec<OpenElement<'a>>,
    phase: Phase,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(input: &'a str, namespace_aware: bool) -> Self {
        Tokenizer {
            cursor: Cursor::new(input),
            max_depth: DEFAULT_MAX_DEPTH,
            forbid_dtd: false,
            forbid_entities: false,
            seen_doctype: false,
            ns_resolver: namespace_aware.then(NamespaceResolver::new),
            entities: EntityMap::default(),
            entity_cache: EntityCache::default(),
            entity_budget: DEFAULT_MAX_ENTITY_EXPANSION,
            stack: Vec::new(),
            phase: Phase::Start,
        }
    }

    pub(crate) fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    pub(crate) fn with_max_entity_expansion(mut self, max_bytes: usize) -> Self {
        self.entity_budget = max_bytes;
        self
    }

    pub(crate) fn with_forbid_dtd(mut self, forbid: bool) -> Self {
        self.forbid_dtd = forbid;
        self
    }

    pub(crate) fn with_forbid_entities(mut self, forbid: bool) -> Self {
        self.forbid_entities = forbid;
        self
    }

    /// Consume one syntactic unit and report it to `sink`.
    ///
    /// Returns `Ok(true)` while more input may follow and `Ok(false)` once the
    /// document is complete. `Ok(true)` does not mean a token was delivered:
    /// a step may make zero sink calls (phase changes, empty text runs) or
    /// two (self-closing tag); see [`TokenSink`]. After an error the
    /// tokenizer is fused: further calls return `Ok(false)`.
    #[inline]
    pub(crate) fn step<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<bool> {
        if self.phase == Phase::Done {
            return Ok(false);
        }
        match self.step_inner(sink) {
            Ok(more) => Ok(more),
            Err(err) => {
                self.phase = Phase::Done;
                self.stack.clear();
                Err(err)
            }
        }
    }

    fn step_inner<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<bool> {
        match self.phase {
            Phase::Start => {
                self.cursor.skip_bom();
                self.phase = Phase::Prolog;
                if self.cursor.starts_with("<?xml ")
                    || self.cursor.starts_with("<?xml\t")
                    || self.cursor.starts_with("<?xml\r")
                    || self.cursor.starts_with("<?xml\n")
                {
                    let decl = parse_xml_declaration(&mut self.cursor)?;
                    sink.xml_declaration(decl);
                }
                Ok(true)
            }
            Phase::Prolog => {
                self.cursor.skip_whitespace();
                if self.cursor.is_eof() {
                    return Err(XmlError::well_formedness(
                        "Document must have a root element",
                        0,
                        0,
                    ));
                }
                if self.cursor.starts_with("<!--") {
                    self.scan_comment(sink)?;
                    return Ok(true);
                }
                if self.cursor.starts_with("<?") {
                    self.scan_pi(sink)?;
                    return Ok(true);
                }
                if self.cursor.starts_with("<!DOCTYPE") {
                    if self.forbid_dtd {
                        return Err(XmlError::parse(
                            "DOCTYPE declarations are not allowed (forbid_dtd)",
                            self.cursor.line(),
                            self.cursor.column(),
                        ));
                    }
                    if self.seen_doctype {
                        return Err(XmlError::well_formedness(
                            "Only one DOCTYPE declaration is allowed",
                            self.cursor.line(),
                            self.cursor.column(),
                        ));
                    }
                    let start = self.cursor.pos;
                    parse_doctype(
                        &mut self.cursor,
                        &mut self.entities,
                        &mut self.entity_budget,
                        self.forbid_entities,
                        self.max_depth,
                    )?;
                    self.seen_doctype = true;
                    sink.doctype(Cow::Borrowed(&self.cursor.input[start..self.cursor.pos]));
                    return Ok(true);
                }
                if self.cursor.starts_with("<") {
                    self.scan_start_element(sink)?;
                    self.phase = Phase::Content;
                    return Ok(true);
                }
                Err(XmlError::well_formedness(
                    "Content found outside of root element",
                    self.cursor.line(),
                    self.cursor.column(),
                ))
            }
            Phase::Content => {
                if self.stack.is_empty() {
                    self.phase = Phase::Trailing;
                    return Ok(true);
                }
                let bytes = self.cursor.input.as_bytes();
                if bytes.get(self.cursor.pos) == Some(&b'<') {
                    if self.cursor.starts_with("</") {
                        self.scan_end_element(sink)?;
                    } else if self.cursor.starts_with("<![CDATA[") {
                        self.scan_cdata(sink)?;
                    } else if self.cursor.starts_with("<!--") {
                        self.scan_comment(sink)?;
                    } else if self.cursor.starts_with("<?") {
                        self.scan_pi(sink)?;
                    } else {
                        self.scan_start_element(sink)?;
                    }
                } else {
                    // A text run can produce no token (e.g. an entity that
                    // expands to nothing); that is not end-of-document.
                    self.scan_text(sink)?;
                }
                Ok(true)
            }
            Phase::Trailing => {
                self.cursor.skip_whitespace();
                if self.cursor.is_eof() {
                    self.phase = Phase::Done;
                    return Ok(false);
                }
                if self.cursor.starts_with("<!--") {
                    self.scan_comment(sink)?;
                    return Ok(true);
                }
                if self.cursor.starts_with("<?") {
                    self.scan_pi(sink)?;
                    return Ok(true);
                }
                if self.cursor.starts_with("<") {
                    return Err(XmlError::well_formedness(
                        "Only one root element is allowed",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                Err(XmlError::well_formedness(
                    "Content found outside of root element",
                    self.cursor.line(),
                    self.cursor.column(),
                ))
            }
            Phase::Done => Ok(false),
        }
    }

    fn scan_comment<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<()> {
        let start = self.cursor.pos;
        let content = parse_comment(&mut self.cursor)?;
        sink.comment(content, start, self.cursor.pos);
        Ok(())
    }

    fn scan_pi<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<()> {
        let start = self.cursor.pos;
        let pi = parse_pi(&mut self.cursor)?;
        sink.processing_instruction(pi, start, self.cursor.pos);
        Ok(())
    }

    fn scan_cdata<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<()> {
        let start = self.cursor.pos;
        let content = parse_cdata(&mut self.cursor)?;
        sink.cdata(content, start, self.cursor.pos);
        Ok(())
    }

    fn scan_text<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<()> {
        enum TextBuf {
            Empty,
            Borrowed { start: usize },
            Owned { start: usize, text: String },
        }

        impl TextBuf {
            fn switch_to_owned(&mut self, input: &str, end_pos: usize) {
                match self {
                    TextBuf::Empty => {
                        *self = TextBuf::Owned {
                            start: end_pos,
                            text: String::new(),
                        };
                    }
                    TextBuf::Borrowed { start } => {
                        let owned_start = *start;
                        let text = input[owned_start..end_pos].to_string();
                        *self = TextBuf::Owned {
                            start: owned_start,
                            text,
                        };
                    }
                    TextBuf::Owned { .. } => {}
                }
            }

            fn push_str(&mut self, input: &str, end_pos: usize, s: &str) {
                self.switch_to_owned(input, end_pos);
                if let TextBuf::Owned { text, .. } = self {
                    text.push_str(s);
                }
            }

            fn push_char(&mut self, input: &str, end_pos: usize, c: char) {
                self.switch_to_owned(input, end_pos);
                if let TextBuf::Owned { text, .. } = self {
                    text.push(c);
                }
            }

            fn into_parts<'a>(
                self,
                input: &'a str,
                end_pos: usize,
            ) -> Option<(Cow<'a, str>, usize)> {
                match self {
                    TextBuf::Empty => None,
                    TextBuf::Borrowed { start } if start < end_pos => {
                        Some((Cow::Borrowed(&input[start..end_pos]), start))
                    }
                    TextBuf::Owned { start, text } if !text.is_empty() => {
                        Some((Cow::Owned(text), start))
                    }
                    _ => None,
                }
            }
        }

        let mut text_buf = TextBuf::Empty;
        loop {
            if self.cursor.pos >= self.cursor.input.len() {
                return Err(XmlError::UnexpectedEof);
            }

            let bytes = self.cursor.input.as_bytes();
            let scan_start = self.cursor.pos;
            let (advance, has_non_ascii_or_control) =
                crate::simd::scan_content_delimiters(&bytes[scan_start..]);
            let i = scan_start + advance;

            if i > scan_start {
                if has_non_ascii_or_control {
                    let chunk = &self.cursor.input[scan_start..i];
                    for c in chunk.chars() {
                        if !is_xml_char(c) {
                            return Err(XmlError::well_formedness(
                                format!("Invalid XML character U+{:04X}", c as u32),
                                self.cursor.line(),
                                self.cursor.column(),
                            ));
                        }
                    }
                }
                match &mut text_buf {
                    TextBuf::Empty => text_buf = TextBuf::Borrowed { start: scan_start },
                    TextBuf::Borrowed { .. } => {}
                    TextBuf::Owned { text, .. } => {
                        text.push_str(&self.cursor.input[scan_start..i]);
                    }
                }
                self.cursor.pos = i;
            }

            if self.cursor.pos >= self.cursor.input.len() {
                return Err(XmlError::UnexpectedEof);
            }

            match bytes[self.cursor.pos] {
                b'<' => {
                    let end = self.cursor.pos;
                    if let Some((content, start)) = text_buf.into_parts(self.cursor.input, end) {
                        sink.text(content, start, end);
                    }
                    return Ok(());
                }
                b'&' => {
                    let before_pos = self.cursor.pos;
                    let resolved = parse_reference_with_entities(
                        &mut self.cursor,
                        &self.entities,
                        &mut self.entity_cache,
                        &mut self.entity_budget,
                    )?;
                    text_buf.push_str(self.cursor.input, before_pos, &resolved);
                }
                b'\r' => {
                    let before_pos = self.cursor.pos;
                    self.cursor.pos += 1;
                    if self.cursor.peek_byte() == Some(b'\n') {
                        self.cursor.pos += 1;
                    }
                    text_buf.push_char(self.cursor.input, before_pos, '\n');
                }
                b']' => {
                    if self.cursor.starts_with("]]>") {
                        return Err(XmlError::well_formedness(
                            "']]>' not allowed in element content",
                            self.cursor.line(),
                            self.cursor.column(),
                        ));
                    }
                    match &mut text_buf {
                        TextBuf::Empty => {
                            text_buf = TextBuf::Borrowed {
                                start: self.cursor.pos,
                            };
                        }
                        TextBuf::Borrowed { .. } => {}
                        TextBuf::Owned { text, .. } => text.push(']'),
                    }
                    self.cursor.advance_no_newlines(1);
                }
                _ => {
                    // `scan_content_delimiters` (src/simd.rs) is contracted to
                    // stop only at `<`, `&`, `\r`, or `]`, so the arms above are
                    // exhaustive for the current scanner. Don't encode that
                    // cross-module invariant as `unreachable!()`: if the
                    // scanner's delimiter set ever grows and this consumer is
                    // missed, an `unreachable!()` here would turn every affected
                    // document into a process-killing panic (a DoS on hostile
                    // input). Degrade gracefully instead: validate and append
                    // one whole character as ordinary content, then advance.
                    let Some(tail) = self.cursor.input.get(self.cursor.pos..) else {
                        return Err(XmlError::well_formedness(
                            "Parser stopped at a non-character boundary while scanning text",
                            self.cursor.line(),
                            self.cursor.column(),
                        ));
                    };
                    let Some(ch) = tail.chars().next() else {
                        return Err(XmlError::UnexpectedEof);
                    };
                    if !is_xml_char(ch) {
                        return Err(XmlError::well_formedness(
                            format!("Invalid XML character U+{:04X}", ch as u32),
                            self.cursor.line(),
                            self.cursor.column(),
                        ));
                    }
                    let before_pos = self.cursor.pos;
                    self.cursor.advance_no_newlines(ch.len_utf8());
                    text_buf.push_char(self.cursor.input, before_pos, ch);
                }
            }
        }
    }

    fn scan_start_element<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<()> {
        let depth = self.stack.len() as u32;
        if depth >= self.max_depth {
            return Err(XmlError::parse(
                format!(
                    "Element nesting exceeds maximum depth of {}",
                    self.max_depth
                ),
                self.cursor.line(),
                self.cursor.column(),
            ));
        }
        let start_pos = self.cursor.pos;

        self.cursor.expect("<")?;
        let tag_name = parse_name(&mut self.cursor)?;

        let mut raw_attrs: Vec<(Cow<'a, str>, Cow<'a, str>)> = Vec::new();
        let mut ns_decls: Vec<(Cow<'a, str>, Cow<'a, str>)> = Vec::new();

        loop {
            self.cursor.skip_whitespace();
            if self.cursor.is_eof() {
                return Err(XmlError::UnexpectedEof);
            }
            if matches!(self.cursor.peek_byte(), Some(b'>') | Some(b'/')) {
                break;
            }
            let attr_name = parse_name(&mut self.cursor)?;
            self.cursor.skip_whitespace();
            self.cursor.expect("=")?;
            self.cursor.skip_whitespace();
            let attr_value = parse_quoted_value_with_entities(
                &mut self.cursor,
                &self.entities,
                &mut self.entity_cache,
                &mut self.entity_budget,
            )?;

            if &*attr_name == "xmlns" {
                if ns_decls.iter().any(|(p, _)| p.is_empty()) {
                    return Err(XmlError::well_formedness(
                        format!("Duplicate attribute: {}", attr_name),
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                if &*attr_value == crate::namespace::XML_NAMESPACE
                    || &*attr_value == crate::namespace::XMLNS_NAMESPACE
                {
                    return Err(XmlError::namespace(
                        "Reserved namespace must not be declared as the default namespace",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                ns_decls.push((Cow::Borrowed(""), attr_value));
            } else if let Some(prefix) = attr_name.strip_prefix("xmlns:") {
                if prefix == "xmlns" {
                    return Err(XmlError::namespace(
                        "The prefix 'xmlns' must not be declared",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                if !crate::writer::is_valid_xml_ncname(prefix) {
                    return Err(XmlError::namespace(
                        format!("Invalid namespace declaration name: {}", attr_name),
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                if prefix == "xml" && &*attr_value != crate::namespace::XML_NAMESPACE {
                    return Err(XmlError::namespace(
                        "The prefix 'xml' must not be bound to any other namespace",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                if prefix != "xml" && &*attr_value == crate::namespace::XML_NAMESPACE {
                    return Err(XmlError::namespace(
                        "The XML namespace must not be bound to another prefix",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                if &*attr_value == crate::namespace::XMLNS_NAMESPACE {
                    return Err(XmlError::namespace(
                        "The xmlns namespace must not be declared",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                if ns_decls.iter().any(|(p, _)| &**p == prefix) {
                    return Err(XmlError::well_formedness(
                        format!("Duplicate attribute: {}", attr_name),
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                let prefix_cow: Cow<'a, str> = match &attr_name {
                    Cow::Borrowed(s) => Cow::Borrowed(&s[6..]),
                    Cow::Owned(s) => Cow::Owned(s[6..].to_string()),
                };
                ns_decls.push((prefix_cow, attr_value));
            } else {
                if raw_attrs.iter().any(|(n, _)| *n == *attr_name) {
                    return Err(XmlError::well_formedness(
                        format!("Duplicate attribute: {}", attr_name),
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
                raw_attrs.push((attr_name, attr_value));
            }

            if let Some(b) = self.cursor.peek_byte() {
                if b != b'>' && b != b'/' && b != b' ' && b != b'\t' && b != b'\n' && b != b'\r' {
                    return Err(XmlError::well_formedness(
                        "Expected whitespace between attributes",
                        self.cursor.line(),
                        self.cursor.column(),
                    ));
                }
            }
        }

        let pushed_ns_scope = self.ns_resolver.is_some() && !ns_decls.is_empty();
        if let Some(resolver) = self.ns_resolver.as_mut() {
            if pushed_ns_scope {
                resolver.push_scope();
                for (prefix, uri) in &ns_decls {
                    resolver.declare(prefix.clone(), uri.clone());
                }
            }
        }

        // Resolve names and consume the tag close. Every step here is fallible
        // (undeclared prefix, duplicate attribute, missing `>`), and we have
        // already pushed a namespace scope above — so any error must unwind
        // that scope to keep `push_scope`/`pop_scope` balanced. `step` fuses
        // the tokenizer on error today, which hides an unbalanced resolver,
        // but the balance must not depend on that: keep it correct so the
        // resolver stays reusable and the tokenizer could become resumable.
        let (qname, resolved_attrs, self_closing, byte_end) =
            match self.resolve_and_close_start_tag(&tag_name, raw_attrs) {
                Ok(parts) => parts,
                Err(err) => {
                    if pushed_ns_scope {
                        if let Some(resolver) = self.ns_resolver.as_mut() {
                            resolver.pop_scope();
                        }
                    }
                    return Err(err);
                }
            };

        sink.start_element(qname, resolved_attrs, ns_decls, start_pos, byte_end, depth);

        if self_closing {
            sink.end_element(start_pos, byte_end, depth);
            if pushed_ns_scope {
                if let Some(resolver) = self.ns_resolver.as_mut() {
                    resolver.pop_scope();
                }
            }
        } else {
            self.stack.push(OpenElement {
                raw_name: tag_name,
                pushed_ns_scope,
            });
        }
        Ok(())
    }

    /// Resolve the element and attribute QNames against the current namespace
    /// scope and consume the tag's `>` or `/>`. Split out from
    /// [`Self::scan_start_element`] so its single caller can unwind a
    /// just-pushed namespace scope on any error (see the call site). Returns
    /// the resolved name, resolved attributes, whether the tag self-closes, and
    /// the byte offset immediately after the close.
    #[allow(clippy::type_complexity)]
    fn resolve_and_close_start_tag(
        &mut self,
        tag_name: &Cow<'a, str>,
        raw_attrs: Vec<(Cow<'a, str>, Cow<'a, str>)>,
    ) -> XmlResult<(QName<'a>, Vec<Attribute<'a>>, bool, usize)> {
        let (prefix, local_name) = split_qname(tag_name);
        let qname = if let Some(resolver) = self.ns_resolver.as_ref() {
            let ns: Option<Cow<'a, str>> = if let Some(p) = prefix {
                let uri = resolver.resolve(p).ok_or_else(|| {
                    XmlError::namespace(
                        format!("Undeclared namespace prefix: {}", p),
                        self.cursor.line(),
                        self.cursor.column(),
                    )
                })?;
                Some(uri.clone())
            } else {
                resolver.resolve_default().cloned()
            };
            QName {
                namespace_uri: ns,
                prefix: prefix.map(|s| borrow_from_cow(tag_name, s)),
                local_name: borrow_from_cow(tag_name, local_name),
            }
        } else {
            QName::local(tag_name.clone())
        };

        let mut resolved_attrs = Vec::with_capacity(raw_attrs.len());
        for (attr_name, attr_value) in raw_attrs {
            let (a_prefix, a_local) = split_qname(&attr_name);
            let a_qname = if let Some(resolver) = self.ns_resolver.as_ref() {
                if let Some(p) = a_prefix {
                    let ns_uri = resolver.resolve(p).ok_or_else(|| {
                        XmlError::namespace(
                            format!("Undeclared namespace prefix: {}", p),
                            self.cursor.line(),
                            self.cursor.column(),
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
            if resolved_attrs.iter().any(|existing: &Attribute<'a>| {
                existing.name.local_name == a_qname.local_name
                    && existing.name.namespace_uri.as_deref() == a_qname.namespace_uri.as_deref()
            }) {
                return Err(XmlError::well_formedness(
                    format!("Duplicate attribute: {}", a_qname),
                    self.cursor.line(),
                    self.cursor.column(),
                ));
            }
            resolved_attrs.push(Attribute {
                name: a_qname,
                value: attr_value,
            });
        }

        let self_closing = self.cursor.peek_byte() == Some(b'/');
        if self_closing {
            self.cursor.expect("/>")?;
        } else {
            self.cursor.expect(">")?;
        }
        let byte_end = self.cursor.pos;
        Ok((qname, resolved_attrs, self_closing, byte_end))
    }

    fn scan_end_element<S: TokenSink<'a>>(&mut self, sink: &mut S) -> XmlResult<()> {
        let Some(open) = self.stack.pop() else {
            return Err(XmlError::well_formedness(
                "Unexpected end tag",
                self.cursor.line(),
                self.cursor.column(),
            ));
        };
        let start = self.cursor.pos;
        self.cursor.expect("</")?;
        let end_tag_name = parse_name(&mut self.cursor)?;
        self.cursor.skip_whitespace();
        self.cursor.expect(">")?;
        if *end_tag_name != *open.raw_name {
            return Err(XmlError::well_formedness(
                format!(
                    "Mismatched end tag: expected </{}>, found </{}>",
                    open.raw_name, end_tag_name
                ),
                self.cursor.line(),
                self.cursor.column(),
            ));
        }

        // After the pop, the stack length equals this element's depth.
        sink.end_element(start, self.cursor.pos, self.stack.len() as u32);
        if open.pushed_ns_scope {
            if let Some(resolver) = self.ns_resolver.as_mut() {
                resolver.pop_scope();
            }
        }
        Ok(())
    }
}

/// Sink that turns tokens into buffered [`PullEvent`]s.
struct EventSink<'a> {
    namespace_aware: bool,
    pending: VecDeque<PullEvent<'a>>,
    /// Resolved name and namespace-declaration count per open element, so
    /// `EndElement` can carry the name and the matching `EndNamespace` count.
    open: Vec<(QName<'a>, usize)>,
}

impl<'a> TokenSink<'a> for EventSink<'a> {
    fn xml_declaration(&mut self, decl: XmlDeclaration<'a>) {
        self.pending.push_back(PullEvent::XmlDeclaration(decl));
    }

    fn doctype(&mut self, raw: Cow<'a, str>) {
        self.pending.push_back(PullEvent::Doctype(raw));
    }

    fn start_element(
        &mut self,
        name: QName<'a>,
        attributes: Vec<Attribute<'a>>,
        namespace_declarations: Vec<(Cow<'a, str>, Cow<'a, str>)>,
        byte_start: usize,
        byte_end: usize,
        depth: u32,
    ) {
        if self.namespace_aware {
            for (prefix, uri) in &namespace_declarations {
                self.pending.push_back(PullEvent::StartNamespace {
                    prefix: if prefix.is_empty() {
                        None
                    } else {
                        Some(prefix.clone())
                    },
                    uri: uri.clone(),
                });
            }
        }
        self.open.push((name.clone(), namespace_declarations.len()));
        self.pending.push_back(PullEvent::StartElement {
            name,
            attributes,
            namespace_declarations,
            byte_start,
            byte_end,
            depth,
        });
    }

    fn end_element(&mut self, byte_start: usize, byte_end: usize, depth: u32) {
        // The tokenizer only reports an end for an element it reported a
        // start for, so the stack is never empty here; if it ever were, there
        // is no name to report and the event is dropped rather than invented.
        let Some((name, namespace_count)) = self.open.pop() else {
            return;
        };
        self.pending.push_back(PullEvent::EndElement {
            name,
            byte_start,
            byte_end,
            depth,
        });
        if self.namespace_aware {
            for _ in 0..namespace_count {
                self.pending.push_back(PullEvent::EndNamespace);
            }
        }
    }

    fn text(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize) {
        self.pending.push_back(PullEvent::Text {
            content,
            byte_start,
            byte_end,
        });
    }

    fn cdata(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize) {
        self.pending.push_back(PullEvent::CData {
            content,
            byte_start,
            byte_end,
        });
    }

    fn comment(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize) {
        self.pending.push_back(PullEvent::Comment {
            content,
            byte_start,
            byte_end,
        });
    }

    fn processing_instruction(
        &mut self,
        pi: ProcessingInstruction<'a>,
        byte_start: usize,
        byte_end: usize,
    ) {
        self.pending.push_back(PullEvent::ProcessingInstruction {
            pi,
            byte_start,
            byte_end,
        });
    }
}

/// XML pull parser over a complete decoded string.
pub struct PullParser<'a> {
    core: Tokenizer<'a>,
    sink: EventSink<'a>,
}

impl<'a> PullParser<'a> {
    /// Create a new pull parser with namespace awareness enabled and default
    /// safety limits.
    pub fn new(input: &'a str) -> Self {
        Self::with_namespace_aware(input, true)
    }

    /// Create a new pull parser with configurable namespace awareness.
    pub fn with_namespace_aware(input: &'a str, namespace_aware: bool) -> Self {
        PullParser {
            core: Tokenizer::new(input, namespace_aware),
            sink: EventSink {
                namespace_aware,
                pending: VecDeque::new(),
                open: Vec::new(),
            },
        }
    }

    /// Override the maximum element-nesting depth.
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.core = self.core.with_max_depth(max_depth);
        self
    }

    /// Override the maximum total bytes of entity expansion.
    pub fn with_max_entity_expansion(mut self, max_bytes: usize) -> Self {
        self.core = self.core.with_max_entity_expansion(max_bytes);
        self
    }

    /// Reject any `<!DOCTYPE` declaration at parse time.
    pub fn with_forbid_dtd(mut self, forbid: bool) -> Self {
        self.core = self.core.with_forbid_dtd(forbid);
        self
    }

    /// Reject `<!ENTITY>` declarations inside a DTD.
    pub fn with_forbid_entities(mut self, forbid: bool) -> Self {
        self.core = self.core.with_forbid_entities(forbid);
        self
    }

    /// Return the next event, or `Ok(None)` at end of document.
    ///
    /// Fused after any error: once an error is returned, later calls return
    /// `Ok(None)`.
    pub fn next_event(&mut self) -> XmlResult<Option<PullEvent<'a>>> {
        loop {
            if let Some(event) = self.sink.pending.pop_front() {
                return Ok(Some(event));
            }
            match self.core.step(&mut self.sink) {
                Ok(true) => continue,
                Ok(false) => return Ok(None),
                Err(err) => {
                    self.sink.pending.clear();
                    self.sink.open.clear();
                    return Err(err);
                }
            }
        }
    }
}

impl<'a> Iterator for PullParser<'a> {
    type Item = XmlResult<PullEvent<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_event() {
            Ok(Some(event)) => Some(Ok(event)),
            Ok(None) => None,
            Err(err) => Some(Err(err)),
        }
    }
}

/// Sink that allocates arena nodes directly: each token becomes exactly one
/// `NodeData` move into the arena, with no intermediate event value.
struct DomSink<'a> {
    doc: Document<'a>,
    stack: Vec<NodeId>,
}

impl<'a> DomSink<'a> {
    fn new(input: &'a str) -> Self {
        let mut doc = Document::new();
        doc.input = input;
        // Pre-allocate based on input size. Markup-heavy documents such as
        // plist files can be closer to 14 bytes per node once whitespace text
        // nodes are included, so larger inputs use a denser estimate to avoid
        // repeated arena growth during parsing.
        //
        // Use `try_reserve` (not `reserve`) so a hostile or simply huge input
        // cannot trigger an aborting upfront allocation via the global
        // allocation-error handler. The reservation is only a hint: if the
        // dense estimate fails on a memory-constrained host, fall back to the
        // sparser estimate, and if that also fails just proceed — parsing still
        // succeeds by regrowing the arena on demand.
        let dense = if input.len() >= 256 * 1024 {
            input.len() / 14
        } else {
            input.len() / 40
        };
        let sparse = input.len() / 40;
        if doc.nodes.try_reserve(dense).is_err() && dense != sparse {
            let _ = doc.nodes.try_reserve(sparse);
        }
        let root = doc.root();
        DomSink {
            doc,
            stack: vec![root],
        }
    }

    #[inline]
    fn parent(&self) -> NodeId {
        match self.stack.last() {
            Some(&id) => id,
            None => self.doc.root(),
        }
    }

    #[inline]
    fn append_leaf(&mut self, kind: NodeKind<'a>, byte_start: usize, byte_end: usize) {
        let parent = self.parent();
        let id = self.doc.alloc_node(kind, byte_start);
        self.doc.set_byte_end_pos(id, byte_end);
        self.doc.append_child_unchecked(parent, id);
    }

    fn finish(self, input: &'a str) -> XmlResult<Document<'a>> {
        let DomSink { mut doc, stack } = self;
        if stack.len() != 1 {
            return Err(XmlError::UnexpectedEof);
        }
        if doc.document_element().is_none() {
            return Err(XmlError::well_formedness(
                "Document must have a root element",
                0,
                0,
            ));
        }
        let root = doc.root();
        doc.set_byte_end_pos(root, input.len());
        // Building the arena went through mutators that clear the flag; the
        // finished tree is an exact image of `input`, so source ranges may
        // size serialization buffers until the first edit.
        doc.mark_source_hint_valid();
        Ok(doc)
    }
}

impl<'a> TokenSink<'a> for DomSink<'a> {
    #[inline]
    fn xml_declaration(&mut self, decl: XmlDeclaration<'a>) {
        self.doc.xml_declaration = Some(decl);
    }

    #[inline]
    fn doctype(&mut self, raw: Cow<'a, str>) {
        self.doc.doctype = Some(raw);
    }

    #[inline]
    fn start_element(
        &mut self,
        name: QName<'a>,
        attributes: Vec<Attribute<'a>>,
        namespace_declarations: Vec<(Cow<'a, str>, Cow<'a, str>)>,
        byte_start: usize,
        _byte_end: usize,
        _depth: u32,
    ) {
        let parent = self.parent();
        let id = self.doc.alloc_node(
            NodeKind::Element(Element {
                name,
                attributes,
                namespace_declarations,
            }),
            byte_start,
        );
        self.doc.append_child_unchecked(parent, id);
        self.stack.push(id);
    }

    #[inline]
    fn end_element(&mut self, _byte_start: usize, byte_end: usize, _depth: u32) {
        // Never pop the document root: the tokenizer only reports an end for
        // an element it reported a start for, so this guard is belt-and-braces.
        if self.stack.len() > 1 {
            if let Some(id) = self.stack.pop() {
                self.doc.set_byte_end_pos(id, byte_end);
            }
        }
    }

    #[inline]
    fn text(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize) {
        self.append_leaf(NodeKind::Text(content), byte_start, byte_end);
    }

    #[inline]
    fn cdata(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize) {
        self.append_leaf(NodeKind::CData(content), byte_start, byte_end);
    }

    #[inline]
    fn comment(&mut self, content: Cow<'a, str>, byte_start: usize, byte_end: usize) {
        self.append_leaf(NodeKind::Comment(content), byte_start, byte_end);
    }

    #[inline]
    fn processing_instruction(
        &mut self,
        pi: ProcessingInstruction<'a>,
        byte_start: usize,
        byte_end: usize,
    ) {
        self.append_leaf(NodeKind::ProcessingInstruction(pi), byte_start, byte_end);
    }
}

/// Build a DOM by driving `tokenizer` straight into the arena. This is the
/// path behind [`Parser::parse`](crate::Parser::parse): no `PullEvent` is
/// constructed, buffered, or destructured per node.
pub(crate) fn build_document<'a>(
    input: &'a str,
    mut tokenizer: Tokenizer<'a>,
) -> XmlResult<Document<'a>> {
    let mut sink = DomSink::new(input);
    while tokenizer.step(&mut sink)? {}
    sink.finish(input)
}

/// Build a DOM document from a stream of pull events.
///
/// This is the explicit event-stream-to-DOM path, kept for differential
/// testing against `Parser::parse`. `Parser::parse` does not go through
/// events: it drives the same tokenizer straight into the arena
/// (`build_document`, crate-private), so this function is measurably slower
/// and is not the way to parse a document you only want as a DOM.
///
/// `parser` must be unconsumed. Events already taken with `next_event` cannot
/// be recovered: the XML declaration, DOCTYPE, and any open ancestors they
/// carried would be missing from the result. Passing a parser that has
/// produced events returns an error rather than a truncated tree.
pub fn document_from_pull<'a>(
    input: &'a str,
    mut parser: PullParser<'a>,
) -> XmlResult<Document<'a>> {
    if parser.core.phase != Phase::Start {
        return Err(XmlError::parse(
            "document_from_pull requires an unconsumed PullParser: events were already taken from it",
            0,
            0,
        ));
    }
    let mut sink = DomSink::new(input);
    while let Some(event) = parser.next_event()? {
        match event {
            PullEvent::XmlDeclaration(decl) => sink.xml_declaration(decl),
            PullEvent::Doctype(dt) => sink.doctype(dt),
            PullEvent::StartNamespace { .. } | PullEvent::EndNamespace => {}
            PullEvent::StartElement {
                name,
                attributes,
                namespace_declarations,
                byte_start,
                byte_end,
                depth,
            } => sink.start_element(
                name,
                attributes,
                namespace_declarations,
                byte_start,
                byte_end,
                depth,
            ),
            PullEvent::EndElement {
                byte_start,
                byte_end,
                depth,
                ..
            } => sink.end_element(byte_start, byte_end, depth),
            PullEvent::Text {
                content,
                byte_start,
                byte_end,
            } => sink.text(content, byte_start, byte_end),
            PullEvent::CData {
                content,
                byte_start,
                byte_end,
            } => sink.cdata(content, byte_start, byte_end),
            PullEvent::Comment {
                content,
                byte_start,
                byte_end,
            } => sink.comment(content, byte_start, byte_end),
            PullEvent::ProcessingInstruction {
                pi,
                byte_start,
                byte_end,
            } => sink.processing_instruction(pi, byte_start, byte_end),
        }
    }
    sink.finish(input)
}

/// Parse a string into a DOM document using the pull parser defaults.
pub fn parse_document(input: &str) -> XmlResult<Document<'_>> {
    document_from_pull(input, PullParser::new(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_names(xml: &str) -> Vec<&'static str> {
        PullParser::new(xml)
            .map(|e| match e.unwrap() {
                PullEvent::XmlDeclaration(_) => "decl",
                PullEvent::Doctype(_) => "doctype",
                PullEvent::StartNamespace { .. } => "start-ns",
                PullEvent::EndNamespace => "end-ns",
                PullEvent::StartElement { .. } => "start",
                PullEvent::EndElement { .. } => "end",
                PullEvent::Text { .. } => "text",
                PullEvent::CData { .. } => "cdata",
                PullEvent::Comment { .. } => "comment",
                PullEvent::ProcessingInstruction { .. } => "pi",
            })
            .collect()
    }

    #[test]
    fn nested_event_order() {
        assert_eq!(
            event_names("<r>t<a/>u</r>"),
            vec!["start", "text", "start", "end", "text", "end"]
        );
    }

    #[test]
    fn attributes_and_namespaces() {
        let mut p = PullParser::new(r#"<a:r xmlns:a="urn:a" a:k="v"/>"#);
        assert!(matches!(
            p.next_event().unwrap(),
            Some(PullEvent::StartNamespace { .. })
        ));
        match p.next_event().unwrap().unwrap() {
            PullEvent::StartElement {
                name,
                attributes,
                namespace_declarations,
                ..
            } => {
                assert_eq!(name.namespace_uri.as_deref(), Some("urn:a"));
                assert_eq!(attributes.len(), 1);
                assert_eq!(attributes[0].name.namespace_uri.as_deref(), Some("urn:a"));
                assert_eq!(namespace_declarations.len(), 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn namespace_balance() {
        assert_eq!(
            event_names(r#"<r xmlns="urn"><a xmlns:p="urn:p"/></r>"#),
            vec!["start-ns", "start", "start-ns", "start", "end", "end-ns", "end", "end-ns"]
        );
    }

    #[test]
    fn end_element_carries_name_and_depth() {
        let events: Vec<_> = PullParser::new(r#"<r><a:b xmlns:a="urn:a"/><c></c></r>"#)
            .map(|e| e.unwrap())
            .collect();
        let ends: Vec<(String, Option<String>, u32)> = events
            .iter()
            .filter_map(|e| match e {
                PullEvent::EndElement { name, depth, .. } => Some((
                    name.local_name.to_string(),
                    name.namespace_uri.as_ref().map(|u| u.to_string()),
                    *depth,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            ends,
            vec![
                ("b".to_string(), Some("urn:a".to_string()), 1),
                ("c".to_string(), None, 1),
                ("r".to_string(), None, 0),
            ]
        );
    }

    #[test]
    fn text_cdata_comment_pi_events() {
        assert_eq!(
            event_names("<r>t<![CDATA[c]]><!--m--><?pi x?></r>"),
            vec!["start", "text", "cdata", "comment", "pi", "end"]
        );
    }

    #[test]
    fn empty_entity_expansion_is_not_end_of_document() {
        // Regression: an entity expanding to nothing produced a text run with
        // no event, which next_event misread as end-of-document (W3C
        // valid-sa-023/085/086, rmt-e2e-15a).
        assert_eq!(
            event_names(r#"<!DOCTYPE r [<!ENTITY e "">]><r>&e;</r>"#),
            vec!["doctype", "start", "end"]
        );
    }

    #[test]
    fn ns_scope_unwinds_on_resolution_error() {
        // Regression (F-2): a start tag that pushes a namespace scope
        // (`xmlns:a`) and then fails name resolution (undeclared prefix `b` on
        // an attribute) must pop that scope before returning the error, keeping
        // push_scope/pop_scope balanced. Before the fix the scope leaked
        // (depth 2 instead of the baseline 1).
        let mut p = PullParser::new(r#"<r xmlns:a="urn:a" b:k="v"/>"#);
        let mut errored = false;
        loop {
            match p.next_event() {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    errored = true;
                    break;
                }
            }
        }
        assert!(errored, "undeclared attribute prefix should error");
        assert_eq!(
            p.core.ns_resolver.as_ref().unwrap().scope_depth(),
            1,
            "namespace scope leaked after a resolution error"
        );
    }

    #[test]
    fn fused_after_error() {
        let mut p = PullParser::new("<r></x><!--ok-->");
        assert!(matches!(
            p.next_event(),
            Ok(Some(PullEvent::StartElement { .. }))
        ));
        assert!(p.next_event().is_err());
        assert!(matches!(p.next_event(), Ok(None)));
        assert!(matches!(p.next_event(), Ok(None)));
    }

    #[test]
    fn rejects_mismatched_end_tag() {
        let err = PullParser::new("<r></x>")
            .collect::<XmlResult<Vec<_>>>()
            .expect_err("mismatched tag should fail");
        assert!(err.to_string().contains("Mismatched end tag"));
    }

    #[test]
    fn rejects_duplicate_doctype() {
        let err = PullParser::new(r#"<!DOCTYPE a SYSTEM "a.dtd"><!DOCTYPE b SYSTEM "b.dtd"><r/>"#)
            .collect::<XmlResult<Vec<_>>>()
            .expect_err("duplicate DOCTYPE should fail");
        assert!(err.to_string().contains("Only one DOCTYPE"));
    }

    #[test]
    fn pull_dom_matches_normal_dom() {
        let xml = r#"<?xml version="1.0"?><!--p--><r a="1"><a>t</a><![CDATA[c]]></r>"#;
        let from_pull = parse_document(xml).unwrap();
        let normal = crate::Parser::new().parse(xml).unwrap();
        assert_eq!(from_pull.to_xml(), normal.to_xml());
        assert_eq!(
            from_pull.node_range(from_pull.document_element().unwrap()),
            normal.node_range(normal.document_element().unwrap())
        );
    }

    #[test]
    fn document_from_pull_rejects_consumed_parser() {
        let xml = "<?xml version=\"1.0\"?><r><a/></r>";
        let mut p = PullParser::new(xml);
        assert!(matches!(
            p.next_event(),
            Ok(Some(PullEvent::XmlDeclaration(_)))
        ));
        let err = document_from_pull(xml, p).expect_err("consumed parser must be rejected");
        assert!(err.to_string().contains("unconsumed"), "{err}");
        // A fresh parser over the same input still builds the full document.
        let from_pull = document_from_pull(xml, PullParser::new(xml)).unwrap();
        let direct = crate::Parser::new().parse(xml).unwrap();
        assert_eq!(from_pull.to_xml(), direct.to_xml());
        assert!(from_pull.xml_declaration.is_some());
    }

    #[test]
    fn direct_dom_sets_ranges_doctype_and_declaration() {
        let xml = "<?xml version=\"1.0\"?><!DOCTYPE r><r><a/>x<!--c--><?p d?></r>";
        let doc = crate::Parser::new().parse(xml).unwrap();
        assert_eq!(doc.xml_declaration.as_ref().unwrap().version, "1.0");
        assert_eq!(doc.doctype.as_deref(), Some("<!DOCTYPE r>"));
        let root = doc.document_element().unwrap();
        assert_eq!(doc.node_source(root), Some("<r><a/>x<!--c--><?p d?></r>"));
        let kids = doc.children(root);
        assert_eq!(doc.node_source(kids[0]), Some("<a/>"));
        assert_eq!(doc.node_source(kids[1]), Some("x"));
        assert_eq!(doc.node_source(kids[2]), Some("<!--c-->"));
        assert_eq!(doc.node_source(kids[3]), Some("<?p d?>"));
        assert_eq!(doc.node_range(doc.root()), Some(0..xml.len()));
    }
}
