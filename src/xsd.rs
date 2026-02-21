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

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{ValidationError, XmlError, XmlResult};
use crate::namespace::build_resolver_for_node;
use crate::xsd_regex::XsdRegex;

const XS_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";

/// Compare two values for ordering. First tries numeric decimal comparison;
/// if either value is not a pure decimal, falls back to lexicographic comparison.
/// This handles date/time types like gMonthDay (--MM-DD), date, dateTime, etc.
fn compare_values(a: &str, b: &str) -> Ordering {
    compare_decimal_strings(a, b).unwrap_or_else(|| a.cmp(b))
}
/// Check if a string is a valid decimal number (optional sign, digits, optional dot+digits).
fn is_decimal_string(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let s = s
        .strip_prefix('-')
        .or_else(|| s.strip_prefix('+'))
        .unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    let mut has_digit = false;
    let mut has_dot = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            has_digit = true;
        } else if c == '.' && !has_dot {
            has_dot = true;
        } else {
            return false;
        }
    }
    has_digit
}

fn compare_decimal_strings(a: &str, b: &str) -> Option<Ordering> {
    let a = a.trim();
    let b = b.trim();

    // Validate both inputs are actual decimal numbers
    if !is_decimal_string(a) || !is_decimal_string(b) {
        return None;
    }

    let (a_neg, a_abs) = if let Some(rest) = a.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = a.strip_prefix('+') {
        (false, rest)
    } else {
        (false, a)
    };

    let (b_neg, b_abs) = if let Some(rest) = b.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = b.strip_prefix('+') {
        (false, rest)
    } else {
        (false, b)
    };

    // Split into integer and fractional parts
    let (a_int, a_frac) = split_decimal(a_abs);
    let (b_int, b_frac) = split_decimal(b_abs);

    // Check if values are zero
    let a_is_zero = is_zero(a_int, a_frac);
    let b_is_zero = is_zero(b_int, b_frac);

    if a_is_zero && b_is_zero {
        return Some(Ordering::Equal);
    }

    // Handle sign differences
    if a_neg && !a_is_zero && (!b_neg || b_is_zero) {
        return Some(Ordering::Less);
    }
    if (!a_neg || a_is_zero) && b_neg && !b_is_zero {
        return Some(Ordering::Greater);
    }

    // Both same sign — compare absolute values
    let abs_cmp = compare_abs(a_int, a_frac, b_int, b_frac)?;

    if a_neg && b_neg {
        // Both negative: reverse comparison
        Some(abs_cmp.reverse())
    } else {
        Some(abs_cmp)
    }
}

fn split_decimal(s: &str) -> (&str, &str) {
    if let Some(dot) = s.find('.') {
        (&s[..dot], &s[dot + 1..])
    } else {
        (s, "")
    }
}

fn is_zero(int_part: &str, frac_part: &str) -> bool {
    int_part.chars().all(|c| c == '0') && frac_part.chars().all(|c| c == '0')
}

fn compare_abs(a_int: &str, a_frac: &str, b_int: &str, b_frac: &str) -> Option<Ordering> {
    // Strip leading zeros from integer parts
    let a_int = a_int.trim_start_matches('0');
    let b_int = b_int.trim_start_matches('0');

    // Compare integer parts first by length, then lexicographically
    match a_int.len().cmp(&b_int.len()) {
        Ordering::Less => return Some(Ordering::Less),
        Ordering::Greater => return Some(Ordering::Greater),
        Ordering::Equal => match a_int.cmp(b_int) {
            Ordering::Less => return Some(Ordering::Less),
            Ordering::Greater => return Some(Ordering::Greater),
            Ordering::Equal => {}
        },
    }

    // Integer parts are equal — compare fractional parts
    // Pad with trailing zeros to same length
    let max_frac = a_frac.len().max(b_frac.len());
    for i in 0..max_frac {
        let a_digit = a_frac.as_bytes().get(i).copied().unwrap_or(b'0');
        let b_digit = b_frac.as_bytes().get(i).copied().unwrap_or(b'0');
        match a_digit.cmp(&b_digit) {
            Ordering::Less => return Some(Ordering::Less),
            Ordering::Greater => return Some(Ordering::Greater),
            Ordering::Equal => {}
        }
    }

    Some(Ordering::Equal)
}

/// An XSD validator that holds a compiled schema and validates documents against it.
pub struct XsdValidator {
    /// Top-level element declarations: (namespace_uri, local_name) -> ElementDecl
    elements: HashMap<(Option<String>, String), ElementDecl>,
    /// Named type definitions: (namespace_uri, local_name) -> TypeDef
    types: HashMap<(Option<String>, String), TypeDef>,
    /// Global attribute declarations: (namespace_uri, local_name) -> AttributeDecl
    global_attributes: HashMap<(Option<String>, String), AttributeDecl>,
    /// Target namespace of the schema.
    target_namespace: Option<String>,
}

/// An element declaration.
#[derive(Debug, Clone)]
struct ElementDecl {
    name: String,
    namespace: Option<String>,
    type_ref: TypeRef,
    min_occurs: u64,
    max_occurs: MaxOccurs,
    nillable: bool,
}

/// Reference to a type - either a named type or an anonymous inline type.
#[derive(Debug, Clone)]
enum TypeRef {
    Named(Option<String>, String), // (namespace, local_name)
    Inline(Box<TypeDef>),
    BuiltIn(BuiltInType),
}

/// A type definition (complex or simple).
#[derive(Debug, Clone)]
enum TypeDef {
    Complex(ComplexTypeDef),
    Simple(SimpleTypeDef),
}

/// A complex type definition.
#[derive(Debug, Clone)]
struct ComplexTypeDef {
    name: Option<String>,
    content: ContentModel,
    attributes: Vec<AttributeDecl>,
    mixed: bool,
    has_any_attribute: bool,
}

/// Content model for a complex type.
#[derive(Debug, Clone)]
enum ContentModel {
    Empty,
    Sequence(Vec<Particle>),
    Choice(Vec<Particle>),
    All(Vec<Particle>),
    SimpleContent(Box<TypeRef>),
    /// Any content (xs:anyType)
    Any,
}

/// A particle in a content model (element ref, group, etc.).
#[derive(Debug, Clone)]
struct Particle {
    kind: ParticleKind,
    min_occurs: u64,
    max_occurs: MaxOccurs,
}

#[derive(Debug, Clone)]
enum ParticleKind {
    Element(ElementDecl),
    Sequence(Vec<Particle>),
    Choice(Vec<Particle>),
}

#[derive(Debug, Clone)]
enum MaxOccurs {
    Bounded(u64),
    Unbounded,
}

/// An attribute declaration.
#[derive(Debug, Clone)]
struct AttributeDecl {
    name: String,
    type_ref: TypeRef,
    required: bool,
    default: Option<String>,
}

/// A simple type definition.
#[derive(Debug, Clone)]
struct SimpleTypeDef {
    name: Option<String>,
    base: BuiltInType,
    facets: Vec<Facet>,
    /// Whether this type is a list type (items separated by whitespace).
    is_list: bool,
    /// For list types, the built-in type of each item.
    item_type: Option<BuiltInType>,
    /// For list types, facets inherited from the item type (when item type is a user-defined simple type).
    item_facets: Vec<Facet>,
    /// Non-builtin base type local name, for resolving list inheritance.
    _base_type_local: Option<String>,
    /// Non-builtin item type local name, for resolving in post-processing.
    _item_type_local: Option<String>,
}

/// Built-in XSD datatypes.
#[derive(Debug, Clone, PartialEq)]
enum BuiltInType {
    String,
    Boolean,
    Decimal,
    Float,
    Double,
    Integer,
    Long,
    Int,
    Short,
    Byte,
    NonNegativeInteger,
    PositiveInteger,
    NonPositiveInteger,
    NegativeInteger,
    UnsignedLong,
    UnsignedInt,
    UnsignedShort,
    UnsignedByte,
    DateTime,
    Date,
    Time,
    Duration,
    GYear,
    GYearMonth,
    GMonth,
    GMonthDay,
    GDay,
    HexBinary,
    Base64Binary,
    AnyURI,
    QName,
    NormalizedString,
    Token,
    Language,
    Name,
    NCName,
    ID,
    IDREF,
    IDREFS,
    NMTOKEN,
    NMTOKENS,
    NOTATION,
    ENTITY,
    ENTITIES,
    AnyType,
    AnySimpleType,
}

/// Facets for restricting simple types.
#[derive(Debug, Clone)]
enum Facet {
    MinLength(usize),
    MaxLength(usize),
    Length(usize),
    Pattern(String),
    Enumeration(Vec<String>),
    MinInclusive(String),
    MaxInclusive(String),
    MinExclusive(String),
    MaxExclusive(String),
    TotalDigits(usize),
    FractionDigits(usize),
    WhiteSpace(WhiteSpaceHandling),
}

#[derive(Debug, Clone)]
enum WhiteSpaceHandling {
    Preserve,
    Replace,
    Collapse,
}

impl XsdValidator {
    /// Build a validator from a parsed XSD schema document.
    pub fn from_schema(schema_doc: &Document) -> XmlResult<Self> {
        let mut validator = XsdValidator {
            elements: HashMap::new(),
            types: HashMap::new(),
            global_attributes: HashMap::new(),
            target_namespace: None,
        };

        let schema_elem = schema_doc
            .document_element()
            .ok_or_else(|| XmlError::validation("Schema document has no root element"))?;

        // Get target namespace and elementFormDefault
        let mut element_form_qualified = false;
        if let Some(elem) = schema_doc.element(schema_elem) {
            validator.target_namespace =
                elem.get_attribute("targetNamespace").map(|s| s.to_string());
            element_form_qualified = elem.get_attribute("elementFormDefault") == Some("qualified");
        }

        // Determine the effective namespace for local element declarations:
        // If elementFormDefault="qualified", local elements inherit the target namespace.
        let local_elem_ns = if element_form_qualified {
            validator.target_namespace.clone()
        } else {
            None
        };

        // Process top-level children
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let local = &elem.name.local_name;
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");

                if !is_xs {
                    continue;
                }

                match local.as_str() {
                    "element" => {
                        let decl = parse_element_decl(
                            schema_doc,
                            child,
                            &validator.target_namespace,
                            &local_elem_ns,
                        )?;
                        let key = (validator.target_namespace.clone(), decl.name.clone());
                        validator.elements.insert(key, decl);
                    }
                    "complexType" => {
                        let type_def = parse_complex_type(schema_doc, child, &local_elem_ns)?;
                        if let TypeDef::Complex(ref ct) = type_def {
                            if let Some(name) = &ct.name {
                                let key = (validator.target_namespace.clone(), name.clone());
                                validator.types.insert(key, type_def);
                            }
                        }
                    }
                    "simpleType" => {
                        let type_def = parse_simple_type(schema_doc, child)?;
                        if let TypeDef::Simple(ref st) = type_def {
                            if let Some(name) = &st.name {
                                let key = (validator.target_namespace.clone(), name.clone());
                                validator.types.insert(key, type_def);
                            }
                        }
                    }
                    "attribute" => {
                        // Parse global attribute declarations
                        if let Some(attr_elem) = schema_doc.element(child) {
                            if let Some(name) = attr_elem.get_attribute("name") {
                                let type_ref = if let Some(type_attr) =
                                    attr_elem.get_attribute("type")
                                {
                                    resolve_type_name(type_attr, &validator.target_namespace)
                                } else {
                                    // Check for inline simpleType child
                                    let mut inline_type = None;
                                    for gc in schema_doc.children(child) {
                                        if let Some(NodeKind::Element(ge)) =
                                            schema_doc.node_kind(gc)
                                        {
                                            if ge.name.local_name == "simpleType" {
                                                if let Ok(td) = parse_simple_type(schema_doc, gc) {
                                                    inline_type =
                                                        Some(TypeRef::Inline(Box::new(td)));
                                                }
                                            }
                                        }
                                    }
                                    inline_type.unwrap_or(TypeRef::BuiltIn(BuiltInType::String))
                                };
                                let required = attr_elem.get_attribute("use") == Some("required");
                                let default =
                                    attr_elem.get_attribute("default").map(|s| s.to_string());
                                let decl = AttributeDecl {
                                    name: name.to_string(),
                                    type_ref,
                                    required,
                                    default,
                                };
                                let key = (validator.target_namespace.clone(), name.to_string());
                                validator.global_attributes.insert(key, decl);
                            }
                        }
                    }
                    _ => {
                        // Ignore other top-level declarations for now
                    }
                }
            }
        }

        // Resolution pass: propagate list type info from base types to derived types
        // Types that restrict a list type inherit is_list and item_type
        let type_keys: Vec<_> = validator.types.keys().cloned().collect();
        for key in &type_keys {
            let base_local = {
                if let Some(TypeDef::Simple(st)) = validator.types.get(key) {
                    st._base_type_local.clone()
                } else {
                    None
                }
            };
            if let Some(base_name) = base_local {
                // Look up the base type in the same namespace
                let base_key = (key.0.clone(), base_name);
                let (is_list, item_type) = {
                    if let Some(TypeDef::Simple(base_st)) = validator.types.get(&base_key) {
                        (base_st.is_list, base_st.item_type.clone())
                    } else {
                        (false, None)
                    }
                };
                if is_list {
                    if let Some(TypeDef::Simple(st)) = validator.types.get_mut(key) {
                        st.is_list = true;
                        if st.item_type.is_none() {
                            st.item_type = item_type;
                        }
                    }
                }
            }
        }

        // Resolution pass 2: resolve item type facets for list types whose item type
        // is a user-defined simple type (not a built-in).
        let type_keys2: Vec<_> = validator.types.keys().cloned().collect();
        for key in &type_keys2 {
            let item_local = {
                if let Some(TypeDef::Simple(st)) = validator.types.get(key) {
                    st._item_type_local.clone()
                } else {
                    None
                }
            };
            if let Some(item_name) = item_local {
                // Look up the item type in the same namespace
                let item_key = (key.0.clone(), item_name);
                let resolved = {
                    if let Some(TypeDef::Simple(item_st)) = validator.types.get(&item_key) {
                        Some((item_st.base.clone(), item_st.facets.clone()))
                    } else {
                        None
                    }
                };
                if let Some((item_base, item_facets)) = resolved {
                    if let Some(TypeDef::Simple(st)) = validator.types.get_mut(key) {
                        st.item_type = Some(item_base);
                        st.item_facets = item_facets;
                    }
                }
            }
        }

        Ok(validator)
    }

    /// Validate a document against this schema. Returns a list of validation errors.
    pub fn validate(&self, doc: &Document) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        let doc_elem = match doc.document_element() {
            Some(e) => e,
            None => {
                errors.push(ValidationError {
                    message: "Document has no root element".to_string(),
                    line: None,
                    column: None,
                });
                return errors;
            }
        };

        let elem = match doc.element(doc_elem) {
            Some(e) => e,
            None => return errors,
        };

        // Find matching top-level element declaration
        let key_with_ns = (
            elem.name.namespace_uri.clone(),
            elem.name.local_name.clone(),
        );
        let key_no_ns = (None, elem.name.local_name.clone());

        let decl = self
            .elements
            .get(&key_with_ns)
            .or_else(|| self.elements.get(&key_no_ns));

        match decl {
            Some(decl) => {
                self.validate_element(doc, doc_elem, decl, &mut errors);
            }
            None => {
                errors.push(ValidationError {
                    message: format!(
                        "No element declaration found for '{}'",
                        elem.name.local_name
                    ),
                    line: Some(doc.node_line(doc_elem)),
                    column: Some(doc.node_column(doc_elem)),
                });
            }
        }

        errors
    }

    fn validate_element(
        &self,
        doc: &Document,
        node: NodeId,
        decl: &ElementDecl,
        errors: &mut Vec<ValidationError>,
    ) {
        let type_def = self.resolve_type(&decl.type_ref);

        match type_def {
            Some(TypeDef::Complex(ct)) => {
                self.validate_complex_content(doc, node, ct, errors);
            }
            Some(TypeDef::Simple(st)) => {
                self.validate_simple_content(doc, node, st, errors);
            }
            None => {
                // If type can't be resolved, check if it's a built-in
                if let TypeRef::BuiltIn(bt) = &decl.type_ref {
                    match bt {
                        BuiltInType::AnyType => {
                            // AnyType allows any content, but we should still
                            // validate child elements against their own declarations.
                            self.validate_children_against_global_decls(doc, node, errors);
                        }
                        _ => {
                            let text = doc.text_content_deep(node);
                            validate_builtin_value(&text, bt, doc, node, errors);
                        }
                    }
                }
                // Otherwise, no validation possible (unknown type)
            }
        }
    }

    fn resolve_type<'a>(&'a self, type_ref: &'a TypeRef) -> Option<&'a TypeDef> {
        match type_ref {
            TypeRef::Named(ns, name) => {
                let key = (ns.clone(), name.clone());
                self.types.get(&key)
            }
            TypeRef::Inline(td) => Some(td.as_ref()),
            TypeRef::BuiltIn(_) => None,
        }
    }

    /// Recursively validate child elements of an AnyType element against
    /// their global element declarations.
    fn validate_children_against_global_decls(
        &self,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        for child in doc.children(node) {
            if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
                // Look up child element in global declarations
                let key_with_ns = (
                    child_elem.name.namespace_uri.clone(),
                    child_elem.name.local_name.clone(),
                );
                let key_no_ns = (None, child_elem.name.local_name.clone());

                let child_decl = self
                    .elements
                    .get(&key_with_ns)
                    .or_else(|| self.elements.get(&key_no_ns));

                if let Some(decl) = child_decl {
                    self.validate_element(doc, child, decl, errors);
                } else {
                    // No declaration found — for AnyType, that's OK.
                    // Still recurse to validate deeper children.
                    self.validate_children_against_global_decls(doc, child, errors);
                }
            }
        }
    }

    fn validate_complex_content(
        &self,
        doc: &Document,
        node: NodeId,
        ct: &ComplexTypeDef,
        errors: &mut Vec<ValidationError>,
    ) {
        // Validate attributes
        if let Some(elem) = doc.element(node) {
            for attr_decl in &ct.attributes {
                if attr_decl.required {
                    let found = elem
                        .attributes
                        .iter()
                        .any(|a| a.name.local_name == attr_decl.name);
                    if !found {
                        errors.push(ValidationError {
                            message: format!("Required attribute '{}' is missing", attr_decl.name),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }

            // Validate attribute values against their declared types
            for attr_decl in &ct.attributes {
                if let Some(attr) = elem
                    .attributes
                    .iter()
                    .find(|a| a.name.local_name == attr_decl.name)
                {
                    let value = &attr.value;
                    self.validate_attribute_value(value, &attr_decl.type_ref, doc, node, errors);
                }
            }

            // If the complex type has anyAttribute, validate unmatched attributes
            // against global attribute declarations
            if ct.has_any_attribute {
                for attr in &elem.attributes {
                    // Skip namespace declarations
                    if attr.name.local_name == "xmlns"
                        || attr.name.prefix.as_deref() == Some("xmlns")
                    {
                        continue;
                    }
                    // Skip xsi:* attributes
                    if attr.name.prefix.as_deref() == Some("xsi")
                        || attr.name.namespace_uri.as_deref()
                            == Some("http://www.w3.org/2001/XMLSchema-instance")
                    {
                        continue;
                    }
                    // Skip if already matched by an explicit attribute declaration
                    let already_declared = ct
                        .attributes
                        .iter()
                        .any(|ad| ad.name == attr.name.local_name);
                    if already_declared {
                        continue;
                    }
                    // Look up in global attribute declarations
                    // Try with the schema's target namespace first, then without namespace
                    let attr_ns = attr.name.namespace_uri.as_ref().cloned();
                    let key = (attr_ns.clone(), attr.name.local_name.clone());
                    let global_decl = self.global_attributes.get(&key).or_else(|| {
                        // Also try with target namespace
                        let key2 = (self.target_namespace.clone(), attr.name.local_name.clone());
                        self.global_attributes.get(&key2)
                    });
                    if let Some(decl) = global_decl {
                        self.validate_attribute_value(
                            &attr.value,
                            &decl.type_ref,
                            doc,
                            node,
                            errors,
                        );
                    }
                }
            }
        }

        // Validate content model
        let child_elements: Vec<NodeId> = doc
            .children(node)
            .into_iter()
            .filter(|&c| matches!(doc.node_kind(c), Some(NodeKind::Element(_))))
            .collect();

        match &ct.content {
            ContentModel::Empty => {
                if !child_elements.is_empty() {
                    errors.push(ValidationError {
                        message: "Element should have empty content".to_string(),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                // Check no text content (unless mixed)
                if !ct.mixed {
                    let text = doc.text_content_deep(node);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        errors.push(ValidationError {
                            message: "Element should have empty content but contains text"
                                .to_string(),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
            ContentModel::Sequence(particles) => {
                self.validate_sequence(doc, &child_elements, particles, node, errors);
            }
            ContentModel::Choice(particles) => {
                self.validate_choice(doc, &child_elements, particles, node, errors);
            }
            ContentModel::All(particles) => {
                self.validate_all(doc, &child_elements, particles, node, errors);
            }
            ContentModel::SimpleContent(type_ref) => {
                if let TypeRef::BuiltIn(bt) = type_ref.as_ref() {
                    let text = doc.text_content_deep(node);
                    validate_builtin_value(&text, bt, doc, node, errors);
                }
            }
            ContentModel::Any => {
                // Any content is valid
            }
        }
    }

    fn validate_sequence(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        let mut child_idx = 0;

        for particle in particles {
            let mut count = 0u64;
            let max = match particle.max_occurs {
                MaxOccurs::Bounded(n) => n,
                MaxOccurs::Unbounded => u64::MAX,
            };

            match &particle.kind {
                ParticleKind::Element(decl) => {
                    while child_idx < children.len() && count < max {
                        let child = children[child_idx];
                        if let Some(elem) = doc.element(child) {
                            let name_matches = elem.name.local_name == decl.name;
                            let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                                (Some(a), Some(b)) => a == b,
                                (None, None) => true,
                                // If decl has no namespace, match any element with that local name
                                (_, None) => true,
                                // If decl has namespace but element doesn't, no match
                                (None, Some(_)) => false,
                            };
                            if name_matches && ns_matches {
                                self.validate_element(doc, child, decl, errors);
                                count += 1;
                                child_idx += 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    if count < particle.min_occurs {
                        errors.push(ValidationError {
                            message: format!(
                                "Expected at least {} occurrence(s) of element '{}', found {}",
                                particle.min_occurs, decl.name, count
                            ),
                            line: Some(doc.node_line(parent)),
                            column: Some(doc.node_column(parent)),
                        });
                    }
                }
                ParticleKind::Sequence(sub_particles) => {
                    // Nested sequence
                    self.validate_sequence(
                        doc,
                        &children[child_idx..],
                        sub_particles,
                        parent,
                        errors,
                    );
                    // Advance past consumed children (approximate)
                    child_idx = children.len();
                }
                ParticleKind::Choice(sub_particles) => {
                    self.validate_choice(
                        doc,
                        &children[child_idx..],
                        sub_particles,
                        parent,
                        errors,
                    );
                    child_idx = children.len();
                }
            }
        }

        // Remaining children are unexpected
        for &remaining in &children[child_idx..] {
            if let Some(elem) = doc.element(remaining) {
                errors.push(ValidationError {
                    message: format!("Unexpected element '{}' in sequence", elem.name.local_name),
                    line: Some(doc.node_line(remaining)),
                    column: Some(doc.node_column(remaining)),
                });
            }
        }
    }

    fn validate_choice(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        if children.is_empty() {
            // Check if any particle allows 0 occurrences
            let all_optional = particles.iter().any(|p| p.min_occurs == 0);
            if !all_optional && !particles.is_empty() {
                errors.push(ValidationError {
                    message: "Expected one of the choice alternatives".to_string(),
                    line: Some(doc.node_line(parent)),
                    column: Some(doc.node_column(parent)),
                });
            }
            return;
        }

        // Try to match the first child against one of the choice alternatives
        let first_child = children[0];
        if let Some(elem) = doc.element(first_child) {
            let matched = particles.iter().any(|p| match &p.kind {
                ParticleKind::Element(decl) => {
                    decl.name == elem.name.local_name
                        && match (&elem.name.namespace_uri, &decl.namespace) {
                            (Some(a), Some(b)) => a == b,
                            (None, None) => true,
                            (_, None) => true,
                            (None, Some(_)) => false,
                        }
                }
                _ => false,
            });
            if !matched {
                errors.push(ValidationError {
                    message: format!(
                        "Element '{}' does not match any choice alternative",
                        elem.name.local_name
                    ),
                    line: Some(doc.node_line(first_child)),
                    column: Some(doc.node_column(first_child)),
                });
            } else {
                // Validate the matched element
                for p in particles {
                    if let ParticleKind::Element(decl) = &p.kind {
                        let name_matches = decl.name == elem.name.local_name;
                        let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                            (Some(a), Some(b)) => a == b,
                            (None, None) => true,
                            (_, None) => true,
                            (None, Some(_)) => false,
                        };
                        if name_matches && ns_matches {
                            self.validate_element(doc, first_child, decl, errors);
                            break;
                        }
                    }
                }
            }
        }
    }

    fn validate_all(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        let mut matched = vec![false; particles.len()];

        for &child in children {
            if let Some(elem) = doc.element(child) {
                let mut found = false;
                for (i, particle) in particles.iter().enumerate() {
                    if let ParticleKind::Element(decl) = &particle.kind {
                        let name_matches = decl.name == elem.name.local_name;
                        let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                            (Some(a), Some(b)) => a == b,
                            (None, None) => true,
                            (_, None) => true,
                            (None, Some(_)) => false,
                        };
                        if name_matches && ns_matches {
                            if matched[i] {
                                errors.push(ValidationError {
                                    message: format!(
                                        "Duplicate element '{}' in all group",
                                        elem.name.local_name
                                    ),
                                    line: Some(doc.node_line(child)),
                                    column: Some(doc.node_column(child)),
                                });
                            } else {
                                matched[i] = true;
                                self.validate_element(doc, child, decl, errors);
                            }
                            found = true;
                            break;
                        }
                    }
                }
                if !found {
                    errors.push(ValidationError {
                        message: format!(
                            "Unexpected element '{}' in all group",
                            elem.name.local_name
                        ),
                        line: Some(doc.node_line(child)),
                        column: Some(doc.node_column(child)),
                    });
                }
            }
        }

        // Check required elements
        for (i, particle) in particles.iter().enumerate() {
            if particle.min_occurs > 0 && !matched[i] {
                if let ParticleKind::Element(decl) = &particle.kind {
                    errors.push(ValidationError {
                        message: format!(
                            "Required element '{}' is missing in all group",
                            decl.name
                        ),
                        line: Some(doc.node_line(parent)),
                        column: Some(doc.node_column(parent)),
                    });
                }
            }
        }
    }

    fn validate_simple_content(
        &self,
        doc: &Document,
        node: NodeId,
        st: &SimpleTypeDef,
        errors: &mut Vec<ValidationError>,
    ) {
        let raw_text = doc.text_content_deep(node);
        // Apply XSD whiteSpace normalization before any validation.
        let ws_mode = whitespace_for_type(&st.base);
        let text = apply_whitespace_normalization(&raw_text, &ws_mode);

        if st.is_list {
            // List type: value is whitespace-separated items
            let items: Vec<&str> = text.split_whitespace().collect();

            // Validate each item against the item type
            if let Some(ref item_bt) = st.item_type {
                for item in &items {
                    validate_builtin_value(item, item_bt, doc, node, errors);
                    // Also validate item-level facets (from user-defined item types)
                    for facet in &st.item_facets {
                        validate_facet(item, facet, item_bt, doc, node, errors);
                    }
                }
            }

            // Validate list-level facets (length counts items, not chars)
            for facet in &st.facets {
                validate_list_facet(&items, facet, &text, doc, node, errors);
            }
        } else {
            validate_builtin_value(&text, &st.base, doc, node, errors);

            // Validate facets
            for facet in &st.facets {
                validate_facet(&text, facet, &st.base, doc, node, errors);
            }
        }
    }

    /// Validate an attribute value against its declared type reference.
    fn validate_attribute_value(
        &self,
        value: &str,
        type_ref: &TypeRef,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        match type_ref {
            TypeRef::BuiltIn(bt) => {
                validate_builtin_value(value, bt, doc, node, errors);
            }
            TypeRef::Inline(td) => {
                match td.as_ref() {
                    TypeDef::Simple(st) => {
                        if st.is_list {
                            let items: Vec<&str> = value.split_whitespace().collect();
                            if let Some(ref item_bt) = st.item_type {
                                for item in &items {
                                    validate_builtin_value(item, item_bt, doc, node, errors);
                                    for facet in &st.item_facets {
                                        validate_facet(item, facet, item_bt, doc, node, errors);
                                    }
                                }
                            }
                            for facet in &st.facets {
                                validate_list_facet(&items, facet, value, doc, node, errors);
                            }
                        } else {
                            validate_builtin_value(value, &st.base, doc, node, errors);
                            for facet in &st.facets {
                                validate_facet(value, facet, &st.base, doc, node, errors);
                            }
                        }
                    }
                    TypeDef::Complex(_) => {
                        // Attributes shouldn't have complex types
                    }
                }
            }
            TypeRef::Named(ns, name) => {
                // Try to resolve the named type
                let key = (ns.clone(), name.clone());
                if let Some(TypeDef::Simple(st)) = self.types.get(&key) {
                    if st.is_list {
                        let items: Vec<&str> = value.split_whitespace().collect();
                        if let Some(ref item_bt) = st.item_type {
                            for item in &items {
                                validate_builtin_value(item, item_bt, doc, node, errors);
                                for facet in &st.item_facets {
                                    validate_facet(item, facet, item_bt, doc, node, errors);
                                }
                            }
                        }
                        for facet in &st.facets {
                            validate_list_facet(&items, facet, value, doc, node, errors);
                        }
                    } else {
                        validate_builtin_value(value, &st.base, doc, node, errors);
                        for facet in &st.facets {
                            validate_facet(value, facet, &st.base, doc, node, errors);
                        }
                    }
                } else if ns.as_deref() == Some(XS_NAMESPACE) {
                    // It's a built-in XSD type
                    if let Some(bt) = parse_builtin_type(name) {
                        validate_builtin_value(value, &bt, doc, node, errors);
                    }
                }
            }
        }
    }
}

// ─── Schema parsing helpers ─────────────────────────────

fn parse_element_decl(
    doc: &Document,
    node: NodeId,
    target_ns: &Option<String>,
    local_elem_ns: &Option<String>,
) -> XmlResult<ElementDecl> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for element declaration"))?;

    let name = elem
        .get_attribute("name")
        .ok_or_else(|| XmlError::validation("Element declaration missing 'name' attribute"))?
        .to_string();

    let min_occurs = elem
        .get_attribute("minOccurs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let max_occurs = match elem.get_attribute("maxOccurs") {
        Some("unbounded") => MaxOccurs::Unbounded,
        Some(s) => MaxOccurs::Bounded(s.parse().unwrap_or(1)),
        None => MaxOccurs::Bounded(1),
    };

    let nillable = elem.get_attribute("nillable") == Some("true");

    let type_ref = if let Some(type_name) = elem.get_attribute("type") {
        resolve_type_name(type_name, target_ns)
    } else {
        // Check for inline type definition
        let mut inline_type = TypeRef::BuiltIn(BuiltInType::AnyType);
        for child in doc.children(node) {
            if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
                let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || child_elem.name.prefix.as_deref() == Some("xs")
                    || child_elem.name.prefix.as_deref() == Some("xsd");
                if is_xs && child_elem.name.local_name == "complexType" {
                    inline_type =
                        TypeRef::Inline(Box::new(parse_complex_type(doc, child, local_elem_ns)?));
                } else if is_xs && child_elem.name.local_name == "simpleType" {
                    inline_type = TypeRef::Inline(Box::new(parse_simple_type(doc, child)?));
                }
            }
        }
        inline_type
    };

    Ok(ElementDecl {
        name,
        namespace: target_ns.clone(),
        type_ref,
        min_occurs,
        max_occurs,
        nillable,
    })
}

fn resolve_type_name(type_name: &str, target_ns: &Option<String>) -> TypeRef {
    // Check for xs: prefix
    let (prefix, local) = if let Some(colon) = type_name.find(':') {
        (&type_name[..colon], &type_name[colon + 1..])
    } else {
        ("", type_name)
    };

    let is_builtin = prefix == "xs" || prefix == "xsd";

    if is_builtin {
        if let Some(bt) = parse_builtin_type(local) {
            return TypeRef::BuiltIn(bt);
        }
    }

    // Named type reference
    if is_builtin {
        TypeRef::Named(Some(XS_NAMESPACE.to_string()), local.to_string())
    } else if prefix.is_empty() {
        TypeRef::Named(target_ns.clone(), local.to_string())
    } else {
        // Non-builtin prefixed type — assume it's in the target namespace
        // (In a full implementation we'd resolve the prefix via namespace declarations)
        TypeRef::Named(target_ns.clone(), local.to_string())
    }
}

fn parse_builtin_type(name: &str) -> Option<BuiltInType> {
    match name {
        "string" => Some(BuiltInType::String),
        "boolean" => Some(BuiltInType::Boolean),
        "decimal" => Some(BuiltInType::Decimal),
        "float" => Some(BuiltInType::Float),
        "double" => Some(BuiltInType::Double),
        "integer" => Some(BuiltInType::Integer),
        "long" => Some(BuiltInType::Long),
        "int" => Some(BuiltInType::Int),
        "short" => Some(BuiltInType::Short),
        "byte" => Some(BuiltInType::Byte),
        "nonNegativeInteger" => Some(BuiltInType::NonNegativeInteger),
        "positiveInteger" => Some(BuiltInType::PositiveInteger),
        "nonPositiveInteger" => Some(BuiltInType::NonPositiveInteger),
        "negativeInteger" => Some(BuiltInType::NegativeInteger),
        "unsignedLong" => Some(BuiltInType::UnsignedLong),
        "unsignedInt" => Some(BuiltInType::UnsignedInt),
        "unsignedShort" => Some(BuiltInType::UnsignedShort),
        "unsignedByte" => Some(BuiltInType::UnsignedByte),
        "dateTime" => Some(BuiltInType::DateTime),
        "date" => Some(BuiltInType::Date),
        "time" => Some(BuiltInType::Time),
        "duration" => Some(BuiltInType::Duration),
        "gYear" => Some(BuiltInType::GYear),
        "gYearMonth" => Some(BuiltInType::GYearMonth),
        "gMonth" => Some(BuiltInType::GMonth),
        "gMonthDay" => Some(BuiltInType::GMonthDay),
        "gDay" => Some(BuiltInType::GDay),
        "hexBinary" => Some(BuiltInType::HexBinary),
        "base64Binary" => Some(BuiltInType::Base64Binary),
        "anyURI" => Some(BuiltInType::AnyURI),
        "QName" => Some(BuiltInType::QName),
        "normalizedString" => Some(BuiltInType::NormalizedString),
        "token" => Some(BuiltInType::Token),
        "language" => Some(BuiltInType::Language),
        "Name" => Some(BuiltInType::Name),
        "NCName" => Some(BuiltInType::NCName),
        "ID" => Some(BuiltInType::ID),
        "IDREF" => Some(BuiltInType::IDREF),
        "IDREFS" => Some(BuiltInType::IDREFS),
        "NMTOKEN" => Some(BuiltInType::NMTOKEN),
        "NMTOKENS" => Some(BuiltInType::NMTOKENS),
        "NOTATION" => Some(BuiltInType::NOTATION),
        "ENTITY" => Some(BuiltInType::ENTITY),
        "ENTITIES" => Some(BuiltInType::ENTITIES),
        "anyType" => Some(BuiltInType::AnyType),
        "anySimpleType" => Some(BuiltInType::AnySimpleType),
        _ => None,
    }
}

fn parse_complex_type(
    doc: &Document,
    node: NodeId,
    local_elem_ns: &Option<String>,
) -> XmlResult<TypeDef> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for complexType"))?;

    let name = elem.get_attribute("name").map(|s| s.to_string());
    let mixed = elem.get_attribute("mixed") == Some("true");

    let mut content = ContentModel::Empty;
    let mut attributes = Vec::new();
    let mut has_any_attribute = false;

    for child in doc.children(node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");

            if !is_xs {
                continue;
            }

            match child_elem.name.local_name.as_str() {
                "sequence" => {
                    content = ContentModel::Sequence(parse_particles(doc, child, local_elem_ns)?);
                }
                "choice" => {
                    content = ContentModel::Choice(parse_particles(doc, child, local_elem_ns)?);
                }
                "all" => {
                    content = ContentModel::All(parse_particles(doc, child, local_elem_ns)?);
                }
                "attribute" => {
                    attributes.push(parse_attribute_decl(doc, child)?);
                }
                "anyAttribute" => {
                    has_any_attribute = true;
                }
                "simpleContent" | "complexContent" => {
                    // Handle extension/restriction
                    for grandchild in doc.children(child) {
                        if let Some(NodeKind::Element(gc_elem)) = doc.node_kind(grandchild) {
                            match gc_elem.name.local_name.as_str() {
                                "extension" | "restriction" => {
                                    if let Some(base) = gc_elem.get_attribute("base") {
                                        content = ContentModel::SimpleContent(Box::new(
                                            resolve_type_name(base, local_elem_ns),
                                        ));
                                    }
                                    // Parse attributes within extension/restriction
                                    for gc_child in doc.children(grandchild) {
                                        if let Some(NodeKind::Element(gc_child_elem)) =
                                            doc.node_kind(gc_child)
                                        {
                                            if gc_child_elem.name.local_name == "attribute" {
                                                attributes
                                                    .push(parse_attribute_decl(doc, gc_child)?);
                                            } else if gc_child_elem.name.local_name
                                                == "anyAttribute"
                                            {
                                                has_any_attribute = true;
                                            } else if gc_child_elem.name.local_name == "sequence" {
                                                content = ContentModel::Sequence(parse_particles(
                                                    doc,
                                                    gc_child,
                                                    local_elem_ns,
                                                )?);
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(TypeDef::Complex(ComplexTypeDef {
        name,
        content,
        attributes,
        mixed,
        has_any_attribute,
    }))
}

fn parse_particles(
    doc: &Document,
    node: NodeId,
    local_elem_ns: &Option<String>,
) -> XmlResult<Vec<Particle>> {
    let mut particles = Vec::new();

    for child in doc.children(node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");

            if !is_xs {
                continue;
            }

            let min_occurs = child_elem
                .get_attribute("minOccurs")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);

            let max_occurs = match child_elem.get_attribute("maxOccurs") {
                Some("unbounded") => MaxOccurs::Unbounded,
                Some(s) => MaxOccurs::Bounded(s.parse().unwrap_or(1)),
                None => MaxOccurs::Bounded(1),
            };

            match child_elem.name.local_name.as_str() {
                "element" => {
                    let decl = parse_element_decl(doc, child, local_elem_ns, local_elem_ns)?;
                    particles.push(Particle {
                        kind: ParticleKind::Element(decl),
                        min_occurs,
                        max_occurs,
                    });
                }
                "sequence" => {
                    let sub = parse_particles(doc, child, local_elem_ns)?;
                    particles.push(Particle {
                        kind: ParticleKind::Sequence(sub),
                        min_occurs,
                        max_occurs,
                    });
                }
                "choice" => {
                    let sub = parse_particles(doc, child, local_elem_ns)?;
                    particles.push(Particle {
                        kind: ParticleKind::Choice(sub),
                        min_occurs,
                        max_occurs,
                    });
                }
                _ => {}
            }
        }
    }

    Ok(particles)
}

fn parse_attribute_decl(doc: &Document, node: NodeId) -> XmlResult<AttributeDecl> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for attribute declaration"))?;

    let name = elem
        .get_attribute("name")
        .ok_or_else(|| XmlError::validation("Attribute declaration missing 'name'"))?
        .to_string();

    let type_ref = if let Some(type_name) = elem.get_attribute("type") {
        resolve_type_name(type_name, &None)
    } else {
        // Check for inline simpleType child
        let mut found_inline = None;
        for child in doc.children(node) {
            if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
                let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || child_elem.name.prefix.as_deref() == Some("xs")
                    || child_elem.name.prefix.as_deref() == Some("xsd");
                if is_xs && child_elem.name.local_name == "simpleType" {
                    found_inline = Some(TypeRef::Inline(Box::new(parse_simple_type(doc, child)?)));
                    break;
                }
            }
        }
        found_inline.unwrap_or(TypeRef::BuiltIn(BuiltInType::String))
    };

    let required = elem.get_attribute("use") == Some("required");
    let default = elem.get_attribute("default").map(|s| s.to_string());

    Ok(AttributeDecl {
        name,
        type_ref,
        required,
        default,
    })
}

fn parse_simple_type(doc: &Document, node: NodeId) -> XmlResult<TypeDef> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for simpleType"))?;

    let name = elem.get_attribute("name").map(|s| s.to_string());
    let mut base = BuiltInType::String;
    let mut facets = Vec::new();
    let mut is_list = false;
    let mut item_type = None;
    let mut item_type_local: Option<String> = None;
    // Store the non-builtin base type local name for later resolution
    let mut base_type_local: Option<String> = None;

    for child in doc.children(node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            if child_elem.name.local_name == "list" {
                // This is a list type
                is_list = true;
                if let Some(item_type_name) = child_elem.get_attribute("itemType") {
                    let (_prefix, local) = if let Some(colon) = item_type_name.find(':') {
                        (&item_type_name[..colon], &item_type_name[colon + 1..])
                    } else {
                        ("", item_type_name)
                    };
                    item_type = parse_builtin_type(local);
                    if item_type.is_none() {
                        // User-defined item type — store name for later resolution
                        item_type = Some(BuiltInType::String);
                        item_type_local = Some(local.to_string());
                    }
                }
            } else if child_elem.name.local_name == "restriction" {
                if let Some(base_name) = child_elem.get_attribute("base") {
                    let (prefix, local) = if let Some(colon) = base_name.find(':') {
                        (&base_name[..colon], &base_name[colon + 1..])
                    } else {
                        ("", base_name)
                    };
                    if prefix == "xs" || prefix == "xsd" || prefix.is_empty() {
                        // Check for built-in list types (NMTOKENS, IDREFS, ENTITIES)
                        if matches!(local, "NMTOKENS" | "IDREFS" | "ENTITIES") {
                            is_list = true;
                            item_type = match local {
                                "NMTOKENS" => Some(BuiltInType::NMTOKEN),
                                "IDREFS" => Some(BuiltInType::IDREF),
                                "ENTITIES" => Some(BuiltInType::ENTITY),
                                _ => None,
                            };
                        }
                        base = parse_builtin_type(local).unwrap_or(BuiltInType::String);
                    } else {
                        // Non-builtin base type — store for later resolution
                        base_type_local = Some(local.to_string());
                    }
                }

                // Parse facets
                for facet_child in doc.children(child) {
                    if let Some(NodeKind::Element(facet_elem)) = doc.node_kind(facet_child) {
                        let value = facet_elem.get_attribute("value").unwrap_or("").to_string();

                        match facet_elem.name.local_name.as_str() {
                            "minLength" => {
                                if let Ok(n) = value.parse() {
                                    facets.push(Facet::MinLength(n));
                                }
                            }
                            "maxLength" => {
                                if let Ok(n) = value.parse() {
                                    facets.push(Facet::MaxLength(n));
                                }
                            }
                            "length" => {
                                if let Ok(n) = value.parse() {
                                    facets.push(Facet::Length(n));
                                }
                            }
                            "pattern" => {
                                facets.push(Facet::Pattern(value));
                            }
                            "enumeration" => {
                                // Collect all enumerations
                                if let Some(Facet::Enumeration(ref mut vals)) = facets
                                    .iter_mut()
                                    .find(|f| matches!(f, Facet::Enumeration(_)))
                                {
                                    vals.push(value);
                                } else {
                                    facets.push(Facet::Enumeration(vec![value]));
                                }
                            }
                            "minInclusive" => facets.push(Facet::MinInclusive(value)),
                            "maxInclusive" => facets.push(Facet::MaxInclusive(value)),
                            "minExclusive" => facets.push(Facet::MinExclusive(value)),
                            "maxExclusive" => facets.push(Facet::MaxExclusive(value)),
                            "totalDigits" => {
                                if let Ok(n) = value.parse() {
                                    facets.push(Facet::TotalDigits(n));
                                }
                            }
                            "fractionDigits" => {
                                if let Ok(n) = value.parse() {
                                    facets.push(Facet::FractionDigits(n));
                                }
                            }
                            "whiteSpace" => {
                                facets.push(Facet::WhiteSpace(match value.as_str() {
                                    "preserve" => WhiteSpaceHandling::Preserve,
                                    "replace" => WhiteSpaceHandling::Replace,
                                    "collapse" => WhiteSpaceHandling::Collapse,
                                    _ => WhiteSpaceHandling::Preserve,
                                }));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(TypeDef::Simple(SimpleTypeDef {
        name,
        base,
        facets,
        is_list,
        item_type,
        item_facets: Vec::new(),
        _base_type_local: base_type_local,
        _item_type_local: item_type_local,
    }))
}

// ─── Validation helpers ─────────────────────────────────

/// Check if a string is a valid NCName (non-colonized name).
fn is_valid_ncname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Determine the whiteSpace normalization mode for a built-in type.
/// Per XSD Part 2: string→preserve, normalizedString→replace,
/// token and all types derived from token→collapse.
fn whitespace_for_type(bt: &BuiltInType) -> WhiteSpaceHandling {
    match bt {
        BuiltInType::String | BuiltInType::AnyType | BuiltInType::AnySimpleType => {
            WhiteSpaceHandling::Preserve
        }
        BuiltInType::NormalizedString => WhiteSpaceHandling::Replace,
        // Token and everything derived from it use collapse
        _ => WhiteSpaceHandling::Collapse,
    }
}

/// Apply XSD whiteSpace normalization to a string value.
/// - Preserve: return as-is
/// - Replace: replace CR, LF, TAB with space
/// - Collapse: replace CR/LF/TAB with space, collapse runs of spaces, strip leading/trailing
fn apply_whitespace_normalization(text: &str, mode: &WhiteSpaceHandling) -> String {
    match mode {
        WhiteSpaceHandling::Preserve => text.to_string(),
        WhiteSpaceHandling::Replace => text
            .chars()
            .map(|c| {
                if c == '\r' || c == '\n' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .collect(),
        WhiteSpaceHandling::Collapse => {
            let replaced: String = text
                .chars()
                .map(|c| {
                    if c == '\r' || c == '\n' || c == '\t' {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            let mut result = String::with_capacity(replaced.len());
            let mut prev_space = true; // true to strip leading spaces
            for c in replaced.chars() {
                if c == ' ' {
                    if !prev_space {
                        result.push(' ');
                    }
                    prev_space = true;
                } else {
                    result.push(c);
                    prev_space = false;
                }
            }
            // Strip trailing space
            if result.ends_with(' ') {
                result.pop();
            }
            result
        }
    }
}

fn validate_builtin_value(
    text: &str,
    bt: &BuiltInType,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    // Apply XSD whiteSpace normalization before any validation.
    // Per XSD Part 2, whiteSpace is a pre-processing step applied to the
    // ·lexical representation· before all other facet checks and type validation.
    let ws_mode = whitespace_for_type(bt);
    let normalized = apply_whitespace_normalization(text, &ws_mode);
    let text = &normalized;

    match bt {
        BuiltInType::String | BuiltInType::AnyType | BuiltInType::AnySimpleType => {
            // Any string is valid
        }
        BuiltInType::NormalizedString => {
            // After replace normalization, CR/LF/TAB should already be gone.
            // This check is for safety.
            if text.contains('\r') || text.contains('\n') || text.contains('\t') {
                errors.push(ValidationError {
                    message: "normalizedString must not contain CR, LF, or TAB".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Token => {
            // After collapse normalization, text is already collapsed.
            // Nothing further to check for plain xs:token.
        }
        BuiltInType::Boolean => {
            let v = text.trim();
            if !matches!(v, "true" | "false" | "1" | "0") {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid boolean", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Decimal => {
            let v = text.trim();
            if v.parse::<f64>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid decimal", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Float | BuiltInType::Double => {
            let v = text.trim();
            if v != "INF" && v != "-INF" && v != "NaN" && v.parse::<f64>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid float/double", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Integer => {
            let v = text.trim();
            if v.parse::<i128>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid integer", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Long => {
            let v = text.trim();
            if v.parse::<i64>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid long", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Int => {
            let v = text.trim();
            if v.parse::<i32>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid int", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Short => {
            let v = text.trim();
            if v.parse::<i16>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid short", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Byte => {
            let v = text.trim();
            if v.parse::<i8>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid byte", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NonNegativeInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n >= 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid nonNegativeInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::PositiveInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n > 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid positiveInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::NonPositiveInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n <= 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid nonPositiveInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::NegativeInteger => {
            let v = text.trim();
            match v.parse::<i128>() {
                Ok(n) if n < 0 => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("'{}' is not a valid negativeInteger", text),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        BuiltInType::UnsignedLong => {
            let v = text.trim();
            if v.parse::<u64>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedLong", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::UnsignedInt => {
            let v = text.trim();
            if v.parse::<u32>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedInt", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::UnsignedShort => {
            let v = text.trim();
            if v.parse::<u16>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedShort", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::UnsignedByte => {
            let v = text.trim();
            if v.parse::<u8>().is_err() {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid unsignedByte", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::DateTime => {
            // Basic pattern: YYYY-MM-DDThh:mm:ss
            let v = text.trim();
            if !is_valid_datetime(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid dateTime", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Date => {
            let v = text.trim();
            if !is_valid_date(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid date", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Time => {
            let v = text.trim();
            if !is_valid_time(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid time", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::HexBinary => {
            let v = text.trim();
            if v.len() % 2 != 0 || !v.chars().all(|c| c.is_ascii_hexdigit()) {
                errors.push(ValidationError {
                    message: format!("'{}' is not valid hexBinary", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Base64Binary => {
            let v: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            let is_valid = if v.is_empty() {
                true // empty string is valid base64Binary (0 octets)
            } else if v.len() % 4 != 0 {
                false // base64 must be a multiple of 4 characters
            } else {
                // Check that padding is only at the end, at most 2 '='
                let pad_count = v.chars().rev().take_while(|&c| c == '=').count();
                if pad_count > 2 {
                    false
                } else {
                    let data_part = &v[..v.len() - pad_count];
                    let pad_part = &v[v.len() - pad_count..];
                    data_part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
                        && pad_part.chars().all(|c| c == '=')
                }
            };
            if !is_valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not valid base64Binary", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::AnyURI => {
            // Basic URI validation: must not contain spaces
            let v = text.trim();
            if v.contains(' ') {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid anyURI", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NCName | BuiltInType::ID | BuiltInType::IDREF => {
            let v = text.trim();
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NCName/ID/IDREF", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Language => {
            // RFC 4646 language tag pattern
            let v = text.trim();
            if v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid language tag", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NMTOKEN => {
            let v = text.trim();
            if v.is_empty()
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
            {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NMTOKEN", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NMTOKENS => {
            // NMTOKENS is a whitespace-separated list of NMTOKENs
            let v = text.trim();
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "NMTOKENS must contain at least one token".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in v.split_whitespace() {
                    if token.is_empty()
                        || !token.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
                        })
                    {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid NMTOKEN in NMTOKENS", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::IDREFS => {
            // IDREFS is a whitespace-separated list of IDREFs (NCNames)
            let v = text.trim();
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "IDREFS must contain at least one IDREF".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in v.split_whitespace() {
                    if !is_valid_ncname(token) {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid IDREF in IDREFS", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::NOTATION => {
            // NOTATION values are NCNames
            let v = text.trim();
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NOTATION value", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::ENTITY => {
            // ENTITY values are NCNames
            let v = text.trim();
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid ENTITY value", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::ENTITIES => {
            // ENTITIES is a whitespace-separated list of ENTITY names (NCNames)
            let v = text.trim();
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "ENTITIES must contain at least one ENTITY".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in v.split_whitespace() {
                    if !is_valid_ncname(token) {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid ENTITY in ENTITIES", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::Duration => {
            let v = text.trim();
            if !is_valid_duration(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid duration", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GYear => {
            let v = text.trim();
            if !is_valid_gyear(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gYear", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GYearMonth => {
            let v = text.trim();
            if !is_valid_gyearmonth(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gYearMonth", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GMonth => {
            let v = text.trim();
            if !is_valid_gmonth(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gMonth", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GMonthDay => {
            let v = text.trim();
            if !is_valid_gmonthday(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gMonthDay", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GDay => {
            let v = text.trim();
            if !is_valid_gday(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gDay", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        _ => {
            // QName - would need namespace context for full validation
        }
    }
}

/// Validate a facet for a list type. Length facets count items, not characters.
fn validate_list_facet(
    items: &[&str],
    facet: &Facet,
    text: &str,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    let item_count = items.len();
    match facet {
        Facet::MinLength(min) => {
            if item_count < *min {
                errors.push(ValidationError {
                    message: format!("List has {} items, less than minLength {}", item_count, min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxLength(max) => {
            if item_count > *max {
                errors.push(ValidationError {
                    message: format!("List has {} items, exceeds maxLength {}", item_count, max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Length(len) => {
            if item_count != *len {
                errors.push(ValidationError {
                    message: format!("List has {} items, expected length {}", item_count, len),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Enumeration(values) => {
            // For list enumerations, the entire space-collapsed value must match
            let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !values.contains(&collapsed) {
                errors.push(ValidationError {
                    message: format!(
                        "'{}' is not one of the allowed values: {:?}",
                        collapsed, values
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Pattern(pattern) => {
            // Pattern facets on lists apply to the whole collapsed space-separated value
            if let Ok(re) = XsdRegex::compile(pattern) {
                if !re.is_match(text) {
                    errors.push(ValidationError {
                        message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::WhiteSpace(_) => {}
        _ => {
            // Other facets (min/max inclusive/exclusive, digits) don't apply to lists
        }
    }
}

/// Compute the "length" of a value for Length/MinLength/MaxLength facets,
/// taking into account type-specific semantics per XSD 1.1 spec:
/// - hexBinary: number of octets (string length / 2)
/// - base64Binary: number of decoded octets
/// - QName/NOTATION: number of URI-qualified characters (URI + local-name length)
/// - All others: number of characters
fn type_aware_length(text: &str, base_type: &BuiltInType, doc: &Document, node: NodeId) -> usize {
    match base_type {
        BuiltInType::HexBinary => {
            // Each pair of hex characters = 1 octet
            let trimmed = text.trim();
            trimmed.len() / 2
        }
        BuiltInType::Base64Binary => {
            // Count decoded octets from base64
            let stripped: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if stripped.is_empty() {
                return 0;
            }
            let padding = stripped.chars().rev().take_while(|&c| c == '=').count();
            let non_padding = stripped.len() - padding;
            // Each 4 base64 chars = 3 bytes, minus padding bytes
            (non_padding * 3) / 4
        }
        BuiltInType::QName => {
            // XSD spec: QName length = len(namespace URI) + len(local name).
            // We resolve the QName prefix against the instance document's namespace context.
            let trimmed = text.trim();
            let (prefix, local_name) = if let Some(colon_pos) = trimmed.find(':') {
                (&trimmed[..colon_pos], &trimmed[colon_pos + 1..])
            } else {
                ("", trimmed)
            };

            if prefix.is_empty() {
                // Unprefixed QName: in no namespace, length = local name length.
                local_name.len()
            } else {
                // Prefixed QName: resolve the prefix to a namespace URI
                let resolver = build_resolver_for_node(doc, node);
                if let Some(ns_uri) = resolver.resolve(prefix) {
                    ns_uri.len() + local_name.len()
                } else {
                    // Prefix not bound — fall back to local name length
                    local_name.len()
                }
            }
        }
        _ => text.len(),
    }
}

fn validate_facet(
    text: &str,
    facet: &Facet,
    base_type: &BuiltInType,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    match facet {
        Facet::MinLength(min) => {
            let len = type_aware_length(text, base_type, doc, node);
            if len < *min {
                errors.push(ValidationError {
                    message: format!("Value length {} is less than minLength {}", len, min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxLength(max) => {
            let len = type_aware_length(text, base_type, doc, node);
            if len > *max {
                errors.push(ValidationError {
                    message: format!("Value length {} exceeds maxLength {}", len, max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Length(expected) => {
            let len = type_aware_length(text, base_type, doc, node);
            if len != *expected {
                errors.push(ValidationError {
                    message: format!("Value length {} does not match length {}", len, expected),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Enumeration(values) => {
            let text_normalized = normalize_datetime_tz(text.trim());
            let match_found = values.iter().any(|v| {
                let v_normalized = normalize_datetime_tz(v.trim());
                v_normalized == text_normalized
            });
            if !match_found {
                errors.push(ValidationError {
                    message: format!("'{}' is not one of the allowed values: {:?}", text, values),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MinInclusive(min) => {
            if compare_values(text.trim(), min) == Ordering::Less {
                errors.push(ValidationError {
                    message: format!("Value '{}' is less than minInclusive {}", text.trim(), min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxInclusive(max) => {
            if compare_values(text.trim(), max) == Ordering::Greater {
                errors.push(ValidationError {
                    message: format!("Value '{}' exceeds maxInclusive {}", text.trim(), max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MinExclusive(min) => {
            let cmp = compare_values(text.trim(), min);
            if cmp == Ordering::Less || cmp == Ordering::Equal {
                errors.push(ValidationError {
                    message: format!(
                        "Value '{}' is not greater than minExclusive {}",
                        text.trim(),
                        min
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxExclusive(max) => {
            let cmp = compare_values(text.trim(), max);
            if cmp == Ordering::Greater || cmp == Ordering::Equal {
                errors.push(ValidationError {
                    message: format!(
                        "Value '{}' is not less than maxExclusive {}",
                        text.trim(),
                        max
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::TotalDigits(max_digits) => {
            let digits: String = text.trim().chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() > *max_digits {
                errors.push(ValidationError {
                    message: format!(
                        "Total digits {} exceeds totalDigits {}",
                        digits.len(),
                        max_digits
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::FractionDigits(max_frac) => {
            if let Some(dot_pos) = text.find('.') {
                let frac = &text[dot_pos + 1..];
                let frac_len = frac.trim_end_matches('0').len();
                if frac_len > *max_frac {
                    errors.push(ValidationError {
                        message: format!(
                            "Fraction digits {} exceeds fractionDigits {}",
                            frac_len, max_frac
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Pattern(pattern) => {
            if let Ok(re) = XsdRegex::compile(pattern) {
                if !re.is_match(text) {
                    errors.push(ValidationError {
                        message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
            // If the pattern fails to compile, we silently accept
            // (graceful degradation for unsupported regex features)
        }
        Facet::WhiteSpace(_) => {
            // White space normalization is applied during parsing
        }
    }
}

// ─── Date/time validation helpers ───────────────────────

/// Validate XSD duration format: PnYnMnDTnHnMnS
/// Rules:
/// - Must start with optional '-' then 'P'
/// - At least one date or time component must follow 'P'
/// - If 'T' is present, at least one time component must follow it
/// - Numbers must be non-negative integers (except seconds which may have fractional part)
fn is_valid_duration(s: &str) -> bool {
    let s = if s.starts_with('-') { &s[1..] } else { s };
    if !s.starts_with('P') || s.len() < 2 {
        return false;
    }
    let rest = &s[1..];

    // Split on 'T' to get date part and optional time part
    let (date_part, time_part) = if let Some(t_pos) = rest.find('T') {
        (&rest[..t_pos], Some(&rest[t_pos + 1..]))
    } else {
        (rest, None)
    };

    let mut has_any_component = false;

    // Parse date part: nY, nM, nD (in order)
    let mut remaining = date_part;
    for designator in ['Y', 'M', 'D'] {
        if let Some(pos) = remaining.find(designator) {
            let num = &remaining[..pos];
            if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            has_any_component = true;
            remaining = &remaining[pos + 1..];
        }
    }
    // There should be nothing left in the date part
    if !remaining.is_empty() {
        return false;
    }

    // Parse time part: nH, nM, nS (or n.nS)
    if let Some(tp) = time_part {
        if tp.is_empty() {
            return false; // T without any time components is invalid
        }
        let mut remaining = tp;
        let mut has_time_component = false;
        for designator in ['H', 'M', 'S'] {
            if let Some(pos) = remaining.find(designator) {
                let num = &remaining[..pos];
                if num.is_empty() {
                    return false;
                }
                // Seconds may have fractional part
                if designator == 'S' {
                    let parts: Vec<&str> = num.split('.').collect();
                    if parts.len() > 2 {
                        return false;
                    }
                    if !parts[0].chars().all(|c| c.is_ascii_digit()) || parts[0].is_empty() {
                        return false;
                    }
                    if parts.len() == 2
                        && (!parts[1].chars().all(|c| c.is_ascii_digit()) || parts[1].is_empty())
                    {
                        return false;
                    }
                } else if !num.chars().all(|c| c.is_ascii_digit()) {
                    return false;
                }
                has_time_component = true;
                remaining = &remaining[pos + 1..];
            }
        }
        if !remaining.is_empty() || !has_time_component {
            return false;
        }
        has_any_component = true;
    }

    has_any_component
}

/// Validate gYear format: [-]CCYY[Z|(+|-)hh:mm]
fn is_valid_gyear(s: &str) -> bool {
    let s = strip_timezone(s);
    let s = if s.starts_with('-') { &s[1..] } else { s };
    s.len() >= 4 && s.chars().all(|c| c.is_ascii_digit())
}

/// Validate gYearMonth format: [-]CCYY-MM[Z|(+|-)hh:mm]
fn is_valid_gyearmonth(s: &str) -> bool {
    let s = strip_timezone(s);
    let (s, _neg) = if s.starts_with('-') {
        (&s[1..], true)
    } else {
        (s, false)
    };
    // Find last '-' which separates year from month
    if let Some(dash_pos) = s.rfind('-') {
        if dash_pos < 4 {
            return false;
        }
        let year = &s[..dash_pos];
        let month = &s[dash_pos + 1..];
        if year.len() < 4 || !year.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if month.len() != 2 || !month.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if let Ok(m) = month.parse::<u32>() {
            (1..=12).contains(&m)
        } else {
            false
        }
    } else {
        false
    }
}

/// Validate gMonth format: --MM[Z|(+|-)hh:mm]
/// Note: XSD 1.0 also allowed --MM-- (with trailing --), so we accept both.
fn is_valid_gmonth(s: &str) -> bool {
    let s = strip_timezone(s);
    if !s.starts_with("--") || s.len() < 4 {
        return false;
    }
    let month_str = &s[2..4];
    if !month_str.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Accept --MM or --MM-- (XSD 1.0 legacy)
    let rest = &s[4..];
    if !rest.is_empty() && rest != "--" {
        return false;
    }
    if let Ok(m) = month_str.parse::<u32>() {
        (1..=12).contains(&m)
    } else {
        false
    }
}

/// Maximum days in a month (gMonthDay does not specify a year, so Feb allows 29)
fn max_days_for_month(month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 29,
        _ => 0,
    }
}

/// Validate gMonthDay format: --MM-DD[Z|(+|-)hh:mm]
fn is_valid_gmonthday(s: &str) -> bool {
    let s = strip_timezone(s);
    if !s.starts_with("--") || s.len() < 7 {
        return false;
    }
    let month_str = &s[2..4];
    if s.as_bytes()[4] != b'-' {
        return false;
    }
    let day_str = &s[5..7];
    if !month_str.chars().all(|c| c.is_ascii_digit())
        || !day_str.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    // Must be exactly 7 chars (after timezone stripping)
    if s.len() != 7 {
        return false;
    }
    let month = match month_str.parse::<u32>() {
        Ok(m) if (1..=12).contains(&m) => m,
        _ => return false,
    };
    let day = match day_str.parse::<u32>() {
        Ok(d) if d >= 1 => d,
        _ => return false,
    };
    day <= max_days_for_month(month)
}

/// Validate gDay format: ---DD[Z|(+|-)hh:mm]
fn is_valid_gday(s: &str) -> bool {
    let s = strip_timezone(s);
    if !s.starts_with("---") || s.len() < 5 {
        return false;
    }
    let day_str = &s[3..5];
    if day_str.len() != 2 || !day_str.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Must be exactly 5 chars after timezone stripping
    if s.len() != 5 {
        return false;
    }
    if let Ok(d) = day_str.parse::<u32>() {
        (1..=31).contains(&d)
    } else {
        false
    }
}

/// Normalize timezone representations in date/time strings so that
/// `Z`, `+00:00`, and `-00:00` are treated as equivalent for enumeration
/// comparison.  Also normalizes trailing fractional-zero seconds (e.g.
/// `.000` → removed) so that `2001-01-01T00:00:00.000Z` equals
/// `2001-01-01T00:00:00Z`.
fn normalize_datetime_tz(s: &str) -> String {
    let mut val = String::from(s);
    // Normalize timezone: replace +00:00 or -00:00 with Z
    if val.ends_with("+00:00") || val.ends_with("-00:00") {
        let end = val.len() - 6;
        val.truncate(end);
        val.push('Z');
    }
    // Normalize trailing fractional zeros in seconds: e.g. .000 before Z or tz
    // Find the seconds fractional part and strip trailing zeros
    // Pattern: ...ss.000Z or ...ss.000+hh:mm or ...ss.000
    // We look for the fractional seconds part
    if let Some(dot_pos) = val.rfind('.') {
        // Determine where the fractional part ends (before Z or timezone or end)
        let after_dot = &val[dot_pos + 1..];
        let frac_end = after_dot
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_dot.len());
        let frac = &after_dot[..frac_end];
        let trimmed_frac = frac.trim_end_matches('0');
        if trimmed_frac.is_empty() {
            // Remove the dot and fractional part entirely
            let suffix = &after_dot[frac_end..];
            let mut new = val[..dot_pos].to_string();
            new.push_str(suffix);
            val = new;
        } else if trimmed_frac.len() < frac.len() {
            let suffix = &after_dot[frac_end..];
            let mut new = val[..dot_pos + 1].to_string();
            new.push_str(trimmed_frac);
            new.push_str(suffix);
            val = new;
        }
    }
    val
}

fn is_valid_datetime(s: &str) -> bool {
    // YYYY-MM-DDThh:mm:ss[.sss][Z|(+|-)hh:mm]
    if let Some(t_pos) = s.find('T') {
        let date_part = &s[..t_pos];
        let time_part = &s[t_pos + 1..];
        is_valid_date(date_part) && is_valid_time(time_part)
    } else {
        false
    }
}

fn is_valid_date(s: &str) -> bool {
    // YYYY-MM-DD[Z|(+|-)hh:mm]
    let s = strip_timezone(s);
    let parts: Vec<&str> = s.split('-').collect();
    // Handle negative years
    if s.starts_with('-') {
        if parts.len() < 4 {
            return false;
        }
        // parts[0] is empty, parts[1] is year, parts[2] month, parts[3] day
        return parts[1].len() >= 4
            && parts[1].chars().all(|c| c.is_ascii_digit())
            && parts[2].len() == 2
            && parts[3].len() == 2;
    }
    if parts.len() != 3 {
        return false;
    }
    parts[0].len() >= 4
        && parts[0].chars().all(|c| c.is_ascii_digit())
        && parts[1].len() == 2
        && parts[1].chars().all(|c| c.is_ascii_digit())
        && parts[2].len() == 2
        && parts[2].chars().all(|c| c.is_ascii_digit())
}

fn is_valid_time(s: &str) -> bool {
    // hh:mm:ss[.sss][Z|(+|-)hh:mm]
    let s = strip_time_timezone(s);
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 3 {
        return false;
    }
    // Allow seconds with fractional part
    let seconds_parts: Vec<&str> = parts[2].split('.').collect();
    parts[0].len() == 2
        && parts[1].len() == 2
        && seconds_parts[0].len() == 2
        && parts[0].chars().all(|c| c.is_ascii_digit())
        && parts[1].chars().all(|c| c.is_ascii_digit())
        && seconds_parts[0].chars().all(|c| c.is_ascii_digit())
}

/// Strip timezone from a time-only string (hh:mm:ss[.sss][Z|(+|-)hh:mm]).
fn strip_time_timezone(s: &str) -> &str {
    if s.ends_with('Z') {
        return &s[..s.len() - 1];
    }
    // Look for timezone offset: +hh:mm or -hh:mm at the end
    // A timezone offset has the form [+-]dd:dd at the end (6 chars)
    if s.len() >= 6 {
        let tz_start = s.len() - 6;
        let c = s.as_bytes()[tz_start];
        if (c == b'+' || c == b'-') && s.as_bytes()[tz_start + 3] == b':' {
            return &s[..tz_start];
        }
    }
    s
}

fn strip_timezone(s: &str) -> &str {
    if s.ends_with('Z') {
        &s[..s.len() - 1]
    } else if let Some(pos) = s.rfind('+') {
        if pos > 0 {
            &s[..pos]
        } else {
            s
        }
    } else if let Some(pos) = s.rfind('-') {
        // Be careful not to strip the date separator
        // Timezone offset is at the end: ...±hh:mm
        if pos > 8 {
            &s[..pos]
        } else {
            s
        }
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn test_validate_string_element() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>
        "#;
        let doc_xml = "<root>hello</root>";

        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_integer_valid() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="count" type="xs:integer"/>
        </xs:schema>
        "#;
        let doc_xml = "<count>42</count>";

        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_integer_invalid() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="count" type="xs:integer"/>
        </xs:schema>
        "#;
        let doc_xml = "<count>not-a-number</count>";

        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_validate_boolean() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="flag" type="xs:boolean"/>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();

        for val in &["true", "false", "1", "0"] {
            let doc = parse(&format!("<flag>{}</flag>", val)).unwrap();
            let validator = XsdValidator::from_schema(&schema).unwrap();
            assert!(validator.validate(&doc).is_empty(), "Failed for {}", val);
        }

        let doc = parse("<flag>yes</flag>").unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        assert!(!validator.validate(&doc).is_empty());
    }

    #[test]
    fn test_validate_complex_type_sequence() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="person">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="name" type="xs:string"/>
                        <xs:element name="age" type="xs:integer"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>
        "#;

        let doc_xml = "<person><name>Alice</name><age>30</age></person>";
        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_required_attribute() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="item">
                <xs:complexType>
                    <xs:sequence/>
                    <xs:attribute name="id" type="xs:string" use="required"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();

        // Missing required attribute
        let doc = parse("<item/>").unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(!errors.is_empty());

        // With required attribute
        let doc = parse(r#"<item id="123"/>"#).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_min_max_inclusive() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="score">
                <xs:simpleType>
                    <xs:restriction base="xs:integer">
                        <xs:minInclusive value="0"/>
                        <xs:maxInclusive value="100"/>
                    </xs:restriction>
                </xs:simpleType>
            </xs:element>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();

        let doc = parse("<score>50</score>").unwrap();
        assert!(validator.validate(&doc).is_empty());

        let doc = parse("<score>150</score>").unwrap();
        assert!(!validator.validate(&doc).is_empty());

        let doc = parse("<score>-1</score>").unwrap();
        assert!(!validator.validate(&doc).is_empty());
    }

    #[test]
    fn test_validate_enumeration() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="color">
                <xs:simpleType>
                    <xs:restriction base="xs:string">
                        <xs:enumeration value="red"/>
                        <xs:enumeration value="green"/>
                        <xs:enumeration value="blue"/>
                    </xs:restriction>
                </xs:simpleType>
            </xs:element>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();

        let doc = parse("<color>red</color>").unwrap();
        assert!(validator.validate(&doc).is_empty());

        let doc = parse("<color>yellow</color>").unwrap();
        assert!(!validator.validate(&doc).is_empty());
    }
}
