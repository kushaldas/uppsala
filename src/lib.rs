//! # Uppsala
//!
//! A pure Rust XML parser and DOM library implementing:
//!
//! - **XML 1.0 (Fifth Edition)** parsing and well-formedness checking
//! - **Namespaces in XML 1.0 (Third Edition)** with prefix resolution and scoping
//! - **XML Information Set (Infoset)** DOM representation
//! - **DOM tree mutation** (insert, remove, replace nodes)
//! - **XPath 1.0** document navigation
//! - **XML Schema (XSD) 1.1** validation (structures + datatypes)

pub mod dom;
pub mod error;
pub mod namespace;
pub mod parser;
pub mod writer;
pub mod xpath;
pub mod xsd;
pub mod xsd_regex;

pub use dom::{Attribute, Document, Element, NodeId, NodeKind, QName, XmlWriteOptions};
pub use error::{XmlError, XmlResult};
pub use namespace::NamespaceResolver;
pub use parser::Parser;
pub use writer::XmlWriter;
pub use xpath::XPathEvaluator;
pub use xsd::XsdValidator;

/// Parse an XML string into a Document.
pub fn parse(input: &str) -> XmlResult<Document> {
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
pub fn parse_bytes(input: &[u8]) -> XmlResult<Document> {
    let text = decode_xml_bytes(input)?;
    parse(&text)
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
    // Pair up bytes as u16 code units (little-endian)
    let code_units: Vec<u16> = bytes
        .chunks(2)
        .filter(|chunk| chunk.len() == 2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&code_units)
        .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-16 LE: {}", e), 1, 1))
}

/// Decode UTF-16 big-endian bytes to a String.
fn decode_utf16_be(bytes: &[u8]) -> XmlResult<String> {
    // Pair up bytes as u16 code units (big-endian)
    let code_units: Vec<u16> = bytes
        .chunks(2)
        .filter(|chunk| chunk.len() == 2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&code_units)
        .map_err(|e| XmlError::well_formedness(format!("Invalid UTF-16 BE: {}", e), 1, 1))
}
