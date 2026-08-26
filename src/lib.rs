//! # Uppsala
//!
//! A zero-dependency pure Rust XML library implementing the core XML stack from
//! parsing through schema validation.
//!
//! - **XML 1.0 (Fifth Edition)** parsing and well-formedness checking
//! - **Namespaces in XML 1.0 (Third Edition)** with prefix resolution and scoping
//! - **Arena-based DOM** with tree mutation (insert, remove, replace nodes)
//! - **XPath 1.0** evaluation (all axes, core functions, predicates)
//! - **XML Schema (XSD) 1.1** validation (structures + datatypes)
//! - **XSD regex engine** for pattern facets (custom NFA matcher)
//! - **Serialization** with round-trip fidelity, pretty-printing, and streaming output
//! - **[`XmlWriter`]** for imperative XML construction without a DOM
//! - **UTF-16 auto-detection** (LE/BE with or without BOM)
//!
//! # Quick Start
//!
//! ## Parsing and querying
//!
//! ```
//! use uppsala::{parse, NodeId, XPathEvaluator, XPathValue};
//!
//! let xml = r#"<library>
//!   <book category="fiction"><title>Dune</title></book>
//!   <book category="science"><title>Cosmos</title></book>
//! </library>"#;
//!
//! let mut doc = parse(xml).unwrap();
//!
//! // DOM traversal
//! let titles = doc.get_elements_by_tag_name("title");
//! assert_eq!(titles.len(), 2);
//!
//! // XPath queries
//! doc.prepare_xpath();
//! let eval = XPathEvaluator::new();
//! let root = doc.root();
//! if let Ok(XPathValue::NodeSet(nodes)) =
//!     eval.evaluate(&doc, root, "//book[@category='fiction']/title")
//! {
//!     assert_eq!(doc.text_content_deep(nodes[0]), "Dune");
//! }
//! ```
//!
//! ## XSD validation
//!
//! ```
//! use uppsala::{parse, XsdValidator};
//!
//! let schema_xml = r#"
//! <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!   <xs:element name="age" type="xs:positiveInteger"/>
//! </xs:schema>"#;
//!
//! let schema_doc = parse(schema_xml).unwrap();
//! let validator = XsdValidator::from_schema(&schema_doc).unwrap();
//!
//! let doc = parse("<age>25</age>").unwrap();
//! assert!(validator.validate(&doc).is_empty());
//!
//! let doc = parse("<age>-5</age>").unwrap();
//! assert!(!validator.validate(&doc).is_empty());
//! ```
//!
//! ## Building XML
//!
//! ```
//! use uppsala::XmlWriter;
//!
//! let mut w = XmlWriter::new();
//! w.write_declaration();
//! w.start_element("root", &[("xmlns", "urn:example")]);
//! w.text("hello");
//! w.end_element("root");
//!
//! assert!(w.into_string().contains("<root"));
//! ```
//!
//! # Resource limits
//!
//! To fail closed on hostile input, the parser enforces fixed resource caps
//! by default (see [`parser`] for the associated constants and the
//! `Parser::with_*` overrides):
//!
//! - **Element nesting depth** — capped at
//!   [`DEFAULT_MAX_DEPTH`](parser::DEFAULT_MAX_DEPTH) (128). Documents nested
//!   deeper are rejected with an error rather than risking a stack overflow.
//! - **Entity expansion byte budget** — capped at
//!   [`DEFAULT_MAX_ENTITY_EXPANSION`](parser::DEFAULT_MAX_ENTITY_EXPANSION)
//!   (1 MiB). Total bytes produced by entity expansion per parse cannot
//!   exceed this, defeating billion-laughs / quadratic-blowup amplification.
//! - **Entity expansion nesting depth** — capped at
//!   [`DEFAULT_MAX_ENTITY_DEPTH`](parser::DEFAULT_MAX_ENTITY_DEPTH) (256), so
//!   a deep linear entity chain fails closed instead of overflowing the stack
//!   even before the byte budget is reached.
//!
//! The depth and expansion-byte limits are configurable via builder methods
//! on [`Parser`] ([`Parser::with_max_depth`] and
//! [`Parser::with_max_entity_expansion`]) when a workload legitimately needs
//! a different bound.

use std::borrow::Cow;

/// Arena-based DOM representation of XML documents.
pub mod dom;
/// Error types: [`XmlError`], [`XmlResult`], and per-domain error structs.
pub mod error;

mod exslt;
/// Namespace prefix resolution with scope stack.
pub mod namespace;
/// XML 1.0 (Fifth Edition) recursive-descent parser.
pub mod parser;
/// XML pull parser event stream.
pub mod pull;
/// Accelerated byte scanning for parser hot loops.
mod simd;

/// Fuzz-only re-export of the internal SIMD scanners (scalar + SSE2) and the
/// serializer escaper, for the differential harnesses in `audit/fuzz`. Gated
/// behind `--features fuzzing`; absent from normal and release builds.
#[cfg(feature = "fuzzing")]
pub use crate::simd::fuzz_exports;
/// Imperative [`XmlWriter`] for streaming XML construction.
pub mod writer;
/// XPath 1.0 evaluation engine.
pub mod xpath;
/// XML Schema (XSD) validation.
pub mod xsd;
/// XSD regular expression engine for pattern facets.
pub mod xsd_regex;
/// XSLT 1.0 transformation engine.
pub mod xslt;

pub use dom::{
    Attribute, ChildrenIter, Document, Element, NodeId, NodeKind, ProcessingInstruction, QName,
    XmlDeclaration, XmlWriteOptions,
};
pub use error::{
    NamespaceError, ParseError, ValidationError, WellFormednessError, XPathError, XmlError,
    XmlResult,
};
pub use namespace::NamespaceResolver;
pub use parser::Parser;
pub use pull::{NamespaceDeclaration, PullEvent, PullParser};
pub use writer::XmlWriter;
pub use xpath::{XPathEvaluator, XPathValue};
pub use xsd::{XsdValidator, XSI_NAMESPACE, XS_NAMESPACE};
pub use xsd_regex::XsdRegex;
pub use xslt::{Stylesheet, DEFAULT_MAX_XSLT_DEPTH, XSLT_NAMESPACE};

/// Parse an XML string into a Document.
pub fn parse(input: &str) -> XmlResult<Document<'_>> {
    let parser = Parser::new();
    parser.parse(input)
}

/// Parse XML from raw bytes, auto-detecting encoding (UTF-8, UTF-16 LE, UTF-16 BE).
///
/// This handles the encoding detection rules from XML 1.0 Appendix F:
/// - UTF-16 LE BOM (`FF FE`): decode as UTF-16 little-endian
/// - UTF-16 BE BOM (`FE FF`): decode as UTF-16 big-endian
/// - UTF-8 BOM (`EF BB BF`): decode as UTF-8
/// - No BOM with `00 3C`: UTF-16 BE without BOM
/// - No BOM with `3C 00`: UTF-16 LE without BOM
/// - Otherwise: UTF-8
///
/// The returned document serializes as a Rust UTF-8 string, so an XML
/// declaration encoding parsed from the byte stream is normalized to `UTF-8`.
pub fn parse_bytes(input: &[u8]) -> XmlResult<Document<'static>> {
    let text = decode_xml_bytes(input)?;
    let mut doc = Parser::new().parse(&text)?;
    if let Some(decl) = doc.xml_declaration.as_mut() {
        if decl.encoding.is_some() {
            decl.encoding = Some(Cow::Borrowed("UTF-8"));
        }
    }
    Ok(doc.into_static())
}

/// Transform an XML document with an XSLT 1.0 stylesheet, returning the
/// serialized result.
///
/// This is the one-shot convenience entry point: it parses both the stylesheet
/// and the source, prepares the source for XPath evaluation, compiles the
/// stylesheet, and applies it. To transform many documents with one stylesheet,
/// compile once with [`Stylesheet::compile`] and reuse it.
///
/// # Example
///
/// ```
/// let xslt = r#"<xsl:stylesheet version="1.0"
///     xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
///   <xsl:output method="xml" omit-xml-declaration="yes"/>
///   <xsl:template match="/"><out><xsl:value-of select="/a"/></out></xsl:template>
/// </xsl:stylesheet>"#;
/// let out = uppsala::transform(xslt, "<a>hi</a>").unwrap();
/// assert_eq!(out, "<out>hi</out>");
/// ```
pub fn transform(xslt: &str, xml: &str) -> XmlResult<String> {
    let style_doc = Parser::new().parse(xslt)?;
    let stylesheet = Stylesheet::compile(&style_doc)?;
    let mut source = Parser::new().parse(xml)?;
    source.prepare_xpath();
    stylesheet.transform(&source)
}

/// Decode raw XML bytes to a String, auto-detecting encoding.
fn decode_xml_bytes(input: &[u8]) -> XmlResult<String> {
    if input.len() < 2 {
        // Too short for BOM detection, assume UTF-8
        return String::from_utf8(input.to_vec())
            .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-8: {}", e), 1, 1));
    }

    // Check for BOM
    if input[0] == 0xFF && input[1] == 0xFE {
        // UTF-16 LE BOM
        return decode_utf16_le(&input[2..]);
    }
    if input[0] == 0xFE && input[1] == 0xFF {
        // UTF-16 BE BOM
        return decode_utf16_be(&input[2..]);
    }
    if input.len() >= 3 && input[0] == 0xEF && input[1] == 0xBB && input[2] == 0xBF {
        // UTF-8 BOM — strip it and decode as UTF-8
        return String::from_utf8(input[3..].to_vec())
            .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-8: {}", e), 1, 1));
    }

    // No BOM — check for UTF-16 without BOM (XML spec Appendix F)
    if input[0] == 0x00 && input[1] == 0x3C {
        // Likely UTF-16 BE without BOM
        return decode_utf16_be(input);
    }
    if input[0] == 0x3C && input[1] == 0x00 {
        // Likely UTF-16 LE without BOM
        return decode_utf16_le(input);
    }

    // Default: UTF-8
    String::from_utf8(input.to_vec())
        .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-8: {}", e), 1, 1))
}

/// Decode UTF-16 little-endian bytes to a String.
fn decode_utf16_le(bytes: &[u8]) -> XmlResult<String> {
    // A UTF-16 stream must be an exact sequence of 16-bit code units.
    // Dropping an orphan trailing byte would silently accept truncated input.
    if bytes.len() % 2 != 0 {
        return Err(XmlError::well_formedness(
            "Invalid UTF-16 LE: odd number of bytes",
            1,
            1,
        ));
    }

    // Pair up bytes as u16 code units (little-endian)
    let code_units: Vec<u16> = bytes
        .chunks(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&code_units)
        .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-16 LE: {}", e), 1, 1))
}

/// Decode UTF-16 big-endian bytes to a String.
fn decode_utf16_be(bytes: &[u8]) -> XmlResult<String> {
    // A UTF-16 stream must be an exact sequence of 16-bit code units.
    // Dropping an orphan trailing byte would silently accept truncated input.
    if bytes.len() % 2 != 0 {
        return Err(XmlError::well_formedness(
            "Invalid UTF-16 BE: odd number of bytes",
            1,
            1,
        ));
    }

    // Pair up bytes as u16 code units (big-endian)
    let code_units: Vec<u16> = bytes
        .chunks(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&code_units)
        .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-16 BE: {}", e), 1, 1))
}
