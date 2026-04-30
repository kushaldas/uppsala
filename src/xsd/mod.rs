//! XML Schema (XSD) 1.1 validation.
//!
//! This module implements validation of XML documents against XSD schemas.
//! It covers:
//!
//! - **Part 1 (Structures)**: Complex types, simple types, elements, attributes,
//!   sequences, choices, all groups, minOccurs/maxOccurs, mixed content.
//! - **Part 2 (Datatypes)**: Built-in primitive types (string, boolean, decimal,
//!   float, double, integer, date, dateTime, etc.) and facet-based restrictions
//!   (minLength, maxLength, pattern, enumeration, minInclusive, maxInclusive, etc.).
//!
//! # Usage
//!
//! ```
//! use uppsala::{parse, xsd::XsdValidator};
//!
//! let schema_xml = r#"
//! <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!   <xs:element name="root" type="xs:string"/>
//! </xs:schema>
//! "#;
//!
//! let doc_xml = "<root>hello</root>";
//!
//! let schema = parse(schema_xml).unwrap();
//! let doc = parse(doc_xml).unwrap();
//! let validator = XsdValidator::from_schema(&schema).unwrap();
//! let errors = validator.validate(&doc);
//! assert!(errors.is_empty());
//! ```

// ── Internal debug logging ───────────────────────────────────────────────────
//
// Trace logging used by the validator submodules during diagnosis. Enabled
// only when the `debug-logging` Cargo feature is on so library output
// stays clean by default.

macro_rules! debug_log {
    ($($arg:tt)*) => {{
        #[cfg(feature = "debug-logging")]
        { eprintln!("DEBUG: {}", format_args!($($arg)*)); }
    }};
}

// Submodule declarations — each file contains a logical slice of the XSD validator.

/// Arbitrary-precision decimal string comparison utilities.
mod decimal;

/// Date, time, and duration validation helpers for XSD built-in temporal types.
mod datetime;

/// Core type definitions: structs, enums, and data structures used throughout the
/// XSD validator (XsdValidator, ElementDecl, TypeDef, ComplexTypeDef, etc.).
pub(crate) mod types;

/// Wildcard namespace constraint helpers: intersection, union, and match checking.
mod wildcard;

/// Built-in type validation, facet enforcement, and whitespace normalization.
mod builtins;

/// Post-processing passes that resolve item-type facets for list types.
mod facet_resolution;

/// Identity constraint evaluation (xs:key, xs:unique, xs:keyref) with restricted
/// XPath selector/field processing.
mod identity;

/// Schema builder: `XsdValidator::from_schema()` and resolution passes.
mod builder;

/// Schema composition: xs:include, xs:import, xs:redefine with external file
/// loading, chameleon namespace fixup, and declaration merging.
mod composition;

/// Schema element/type/attribute/group parsing from DOM nodes.
mod parser;

/// Instance document validation: element, attribute, content model, and simple
/// content validation logic.
mod validation;

/// Unit tests for the XSD validator.
#[cfg(test)]
mod tests;

// ── Constants ────────────────────────────────────────────────────────────────

/// The XML Schema namespace URI (`xs:` / `xsd:`).
pub const XS_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";

/// The XML Schema Instance namespace URI (`xsi:`).
pub const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

// ── Re-exports ───────────────────────────────────────────────────────────────

/// The primary public type: construct via `XsdValidator::from_schema()` and call
/// `validator.validate(&doc)` to obtain a list of validation errors.
pub use types::XsdValidator;
