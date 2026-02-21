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
pub mod xpath;
pub mod xsd;
pub mod xsd_regex;

pub use dom::{Attribute, Document, Element, NodeId, NodeKind, QName};
pub use error::{XmlError, XmlResult};
pub use namespace::NamespaceResolver;
pub use parser::Parser;
pub use xpath::XPathEvaluator;
pub use xsd::XsdValidator;

/// Parse an XML string into a Document.
pub fn parse(input: &str) -> XmlResult<Document> {
    let parser = Parser::new();
    parser.parse(input)
}
