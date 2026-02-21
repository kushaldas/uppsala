//! XSD validation logic for element and content model validation.
//!
//! This module contains the core validation methods on `XsdValidator`:
//! - `validate()` — entry point: validates an entire document against the schema
//! - `validate_element()` — validates a single element against its declaration
//! - `validate_complex_content()` — validates attributes and content model of complex types
//! - `validate_sequence()` / `validate_choice()` / `validate_all()` — content model validators
//! - `validate_simple_content()` — validates text content against a simple type
//! - `validate_attribute_value()` — validates an attribute value against its declared type
//! - xsi:type resolution and type substitution blocking checks
//! - Substitution group matching for element declarations

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::ValidationError;
use crate::namespace::build_resolver_for_node;

use super::builtins::{
    apply_whitespace_normalization, validate_builtin_value, validate_facet, validate_list_facet,
    whitespace_for_type,
};
use super::parser::parse_builtin_type;
use super::types::*;
use super::wildcard::wildcard_allows_namespace;
use super::{XSI_NAMESPACE, XS_NAMESPACE};

/// Result of resolving an `xsi:type` attribute on an element.
///
/// When an instance element carries `xsi:type`, the validator resolves it to one of:
/// - A built-in XSD type (e.g. `xs:int`)
/// - A named schema type (simple or complex)
/// - Not found (error case)
enum XsiTypeResult {
    /// A built-in XSD type like xs:string, xs:int, etc.
    BuiltIn(BuiltInType),
    /// A named type definition from the schema.
    Named(TypeDef),
    /// The xsi:type QName could not be resolved.
    NotFound(String),
}

impl XsdValidator {
    /// Validate a document against this schema.
    ///
    /// Finds the root element, looks up its global element declaration, and
    /// delegates to `validate_element()`. Returns a (possibly empty) list of
    /// validation errors.
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
            elem.name.namespace_uri.as_deref().map(|s| s.to_string()),
            elem.name.local_name.to_string(),
        );
        let key_no_ns = (None, elem.name.local_name.to_string());

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

    /// Check if an element has any child elements (not just text nodes).
    fn element_has_child_elements(&self, doc: &Document, node: NodeId) -> bool {
        for child in doc.children(node) {
            if let Some(NodeKind::Element(_)) = doc.node_kind(child) {
                return true;
            }
        }
        false
    }

    /// Get a display name for a built-in type (e.g. "xs:string").
    fn builtin_type_name(&self, bt: &BuiltInType) -> &'static str {
        match bt {
            BuiltInType::String => "xs:string",
            BuiltInType::Boolean => "xs:boolean",
            BuiltInType::Decimal => "xs:decimal",
            BuiltInType::Float => "xs:float",
            BuiltInType::Double => "xs:double",
            BuiltInType::Integer => "xs:integer",
            BuiltInType::Long => "xs:long",
            BuiltInType::Int => "xs:int",
            BuiltInType::Short => "xs:short",
            BuiltInType::Byte => "xs:byte",
            BuiltInType::NonNegativeInteger => "xs:nonNegativeInteger",
            BuiltInType::PositiveInteger => "xs:positiveInteger",
            BuiltInType::NonPositiveInteger => "xs:nonPositiveInteger",
            BuiltInType::NegativeInteger => "xs:negativeInteger",
            BuiltInType::UnsignedLong => "xs:unsignedLong",
            BuiltInType::UnsignedInt => "xs:unsignedInt",
            BuiltInType::UnsignedShort => "xs:unsignedShort",
            BuiltInType::UnsignedByte => "xs:unsignedByte",
            BuiltInType::DateTime => "xs:dateTime",
            BuiltInType::Date => "xs:date",
            BuiltInType::Time => "xs:time",
            BuiltInType::Duration => "xs:duration",
            BuiltInType::GYear => "xs:gYear",
            BuiltInType::GYearMonth => "xs:gYearMonth",
            BuiltInType::GMonth => "xs:gMonth",
            BuiltInType::GMonthDay => "xs:gMonthDay",
            BuiltInType::GDay => "xs:gDay",
            BuiltInType::HexBinary => "xs:hexBinary",
            BuiltInType::Base64Binary => "xs:base64Binary",
            BuiltInType::AnyURI => "xs:anyURI",
            BuiltInType::QName => "xs:QName",
            BuiltInType::NormalizedString => "xs:normalizedString",
            BuiltInType::Token => "xs:token",
            BuiltInType::Language => "xs:language",
            BuiltInType::Name => "xs:Name",
            BuiltInType::NCName => "xs:NCName",
            BuiltInType::ID => "xs:ID",
            BuiltInType::IDREF => "xs:IDREF",
            BuiltInType::IDREFS => "xs:IDREFS",
            BuiltInType::NMTOKEN => "xs:NMTOKEN",
            BuiltInType::NMTOKENS => "xs:NMTOKENS",
            BuiltInType::NOTATION => "xs:NOTATION",
            BuiltInType::ENTITY => "xs:ENTITY",
            BuiltInType::ENTITIES => "xs:ENTITIES",
            BuiltInType::AnyType => "xs:anyType",
            BuiltInType::AnySimpleType => "xs:anySimpleType",
        }
    }

    /// Resolve an `xsi:type` attribute on an element.
    ///
    /// Looks for `xsi:type` in the element's attributes, parses the QName value,
    /// resolves the prefix to a namespace URI, and looks up the type in the schema
    /// or built-in type registry.
    ///
    /// Returns `None` if no `xsi:type` is present.
    fn resolve_xsi_type(&self, doc: &Document, node: NodeId) -> Option<XsiTypeResult> {
        let elem = doc.element(node)?;

        // Look for xsi:type attribute
        let xsi_type_value = elem.get_attribute_ns(XSI_NAMESPACE, "type").or_else(|| {
            // Also try by prefix match for elements where namespace resolution
            // hasn't been applied to attributes
            elem.attributes
                .iter()
                .find(|a| a.name.local_name == "type" && a.name.prefix.as_deref() == Some("xsi"))
                .map(|a| &*a.value)
        })?;

        // Parse the QName value (may be prefixed like "xs:int")
        let (prefix, local_name) = if let Some(colon_pos) = xsi_type_value.find(':') {
            (
                Some(&xsi_type_value[..colon_pos]),
                &xsi_type_value[colon_pos + 1..],
            )
        } else {
            (None, xsi_type_value)
        };

        // Resolve prefix to namespace URI
        let type_ns = if let Some(pfx) = prefix {
            // Look up prefix in namespace declarations
            let resolver = build_resolver_for_node(doc, node);
            resolver.resolve(pfx).map(|s| s.to_string())
        } else {
            // No prefix — use default namespace if present
            // Per XSD spec, an unprefixed QName in xsi:type uses the default namespace
            let resolver = build_resolver_for_node(doc, node);
            resolver.resolve_default().map(|s| s.to_string())
        };

        // Check if it's a built-in XSD type
        if type_ns.as_deref() == Some(XS_NAMESPACE) {
            if let Some(bt) = parse_builtin_type(local_name) {
                return Some(XsiTypeResult::BuiltIn(bt));
            }
        }

        // Try looking up in schema types
        let key = (type_ns.clone(), local_name.to_string());
        if let Some(td) = self.types.get(&key) {
            return Some(XsiTypeResult::Named(td.clone()));
        }
        // Also try without namespace
        let key_no_ns = (None, local_name.to_string());
        if let Some(td) = self.types.get(&key_no_ns) {
            return Some(XsiTypeResult::Named(td.clone()));
        }

        Some(XsiTypeResult::NotFound(xsi_type_value.to_string()))
    }

    /// Check if xsi:type substitution is blocked by element or type block constraints.
    ///
    /// Per XSD §3.4.4.2 "Type Derivation OK (Complex)", blocking is checked against:
    /// 1. The element declaration's block (`decl_block_ext`/`decl_block_rst`) — blocks any
    ///    derivation step in the entire chain that uses the blocked method.
    /// 2. The declared type's block (`decl_type_block_ext`/`decl_type_block_rst`) — same rule,
    ///    applied to the type that the element declaration refers to.
    ///
    /// Intermediate types' block constraints do NOT affect xsi:type substitution checking.
    ///
    /// Returns `Some(error_message)` if blocked, `None` if allowed.
    fn check_type_substitution_blocked(
        &self,
        xsi_type: &TypeDef,
        decl_block_ext: bool,
        decl_block_rst: bool,
        decl_type_block_ext: bool,
        decl_type_block_rst: bool,
    ) -> Option<String> {
        // Walk up the derivation chain of the xsi:type type.
        // Track which derivation methods appear in the chain.
        let mut has_extension_in_chain = false;
        let mut has_restriction_in_chain = false;
        let mut current = xsi_type;

        loop {
            let ct = match current {
                TypeDef::Complex(ct) => ct,
                TypeDef::Simple(_) => break,
            };

            let is_extension = match ct.derived_by_extension {
                Some(true) => true,
                Some(false) => false,
                None => break, // Not derived, we're at a root type
            };

            if is_extension {
                has_extension_in_chain = true;
            } else {
                has_restriction_in_chain = true;
            }

            // Check element-level block against accumulated chain
            if has_extension_in_chain && decl_block_ext {
                return Some(
                    "Type substitution blocked: derivation chain includes extension, which is blocked by element declaration".to_string(),
                );
            }
            if has_restriction_in_chain && decl_block_rst {
                return Some(
                    "Type substitution blocked: derivation chain includes restriction, which is blocked by element declaration".to_string(),
                );
            }

            // Check the declared type's block against accumulated chain
            if has_extension_in_chain && decl_type_block_ext {
                return Some(
                    "Type substitution blocked: derivation chain includes extension, which is blocked by the declared type".to_string(),
                );
            }
            if has_restriction_in_chain && decl_type_block_rst {
                return Some(
                    "Type substitution blocked: derivation chain includes restriction, which is blocked by the declared type".to_string(),
                );
            }

            // Move up to base type
            if let Some(ref base_key) = ct.base_type {
                if let Some(base_td) = self.types.get(base_key) {
                    current = base_td;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        None
    }

    /// Check if `xsi_type` is the declared type or derived from it.
    ///
    /// The declared element type is given as a `TypeRef`. Returns `true` if the
    /// xsi:type is valid for substitution (ignoring block constraints).
    fn is_type_derived_from_decl(&self, xsi_type: &TypeDef, decl_type_ref: &TypeRef) -> bool {
        // Get the declared type's key (namespace, local_name)
        let decl_key = match decl_type_ref {
            TypeRef::Named(ns, name) => (ns.clone(), name.clone()),
            TypeRef::BuiltIn(bt) => {
                // xsi:type is always a named type here, AnyType allows everything
                if *bt == BuiltInType::AnyType {
                    return true;
                }
                // For other built-in types, the xsi:type must match exactly
                // (named schema types can't derive from built-in complex types normally)
                return false;
            }
            TypeRef::Inline(_) => {
                // Inline (anonymous) type — xsi:type substitution is not meaningful
                // since you can't name the declared type. Allow it.
                return true;
            }
        };

        // Check if xsi:type IS the declared type
        let xsi_key = match xsi_type {
            TypeDef::Complex(ct) => {
                if let Some(ref name) = ct.name {
                    // Try to match: check if namespace+name matches decl_key
                    // We need the type's namespace. Walk through the types map to find it.
                    if let Some(found_key) = self.find_type_key_by_typedef(xsi_type) {
                        if found_key == decl_key {
                            return true;
                        }
                        found_key
                    } else {
                        // Anonymous type — can't be the same as a named declared type
                        (None, name.clone())
                    }
                } else {
                    return false; // anonymous type, can't match
                }
            }
            TypeDef::Simple(st) => {
                if let Some(ref name) = st.name {
                    if let Some(found_key) = self.find_type_key_by_typedef(xsi_type) {
                        if found_key == decl_key {
                            return true;
                        }
                        found_key
                    } else {
                        (None, name.clone())
                    }
                } else {
                    return false;
                }
            }
        };

        // Walk the derivation chain of xsi:type to see if it eventually derives from decl_key
        self.is_derived_from(&xsi_key, &decl_key)
    }

    /// Find the key in `self.types` that corresponds to a given `TypeDef`.
    ///
    /// Searches the types map by matching the type's name field against the map's
    /// local name component. Returns the full `(Option<namespace>, name)` key.
    fn find_type_key_by_typedef(&self, td: &TypeDef) -> Option<(Option<String>, String)> {
        let name = match td {
            TypeDef::Complex(ct) => ct.name.as_ref()?,
            TypeDef::Simple(st) => st.name.as_ref()?,
        };
        // Look for it in the types map
        for (key, _val) in &self.types {
            if &key.1 == name {
                return Some(key.clone());
            }
        }
        None
    }

    /// Check if a type identified by `type_key` is derived (directly or transitively)
    /// from a type identified by `ancestor_key`.
    ///
    /// Walks up the derivation chain (via `base_type` links on complex types) up to
    /// 50 levels deep to prevent infinite loops.
    fn is_derived_from(
        &self,
        type_key: &(Option<String>, String),
        ancestor_key: &(Option<String>, String),
    ) -> bool {
        let mut current_key = type_key.clone();
        // Walk up to 50 levels to avoid infinite loops
        for _ in 0..50 {
            if let Some(td) = self.types.get(&current_key) {
                match td {
                    TypeDef::Complex(ct) => {
                        if let Some(ref base_key) = ct.base_type {
                            if base_key == ancestor_key {
                                return true;
                            }
                            current_key = base_key.clone();
                        } else {
                            return false; // no base type
                        }
                    }
                    TypeDef::Simple(_st) => {
                        // Simple types derive from their base built-in type
                        // For now, we can't easily walk the chain for simple types
                        // since base is a BuiltInType, not a key
                        return false;
                    }
                }
            } else {
                return false;
            }
        }
        false
    }

    /// Compute the effective attribute wildcard for a complex type.
    ///
    /// For types derived by extension, this is the union of the base type's
    /// effective wildcard and the derived type's own wildcard.
    /// For restriction types or types not derived, this is just the type's own wildcard.
    fn compute_effective_wildcard(&self, ct: &ComplexTypeDef) -> Option<AttributeWildcard> {
        if ct.derived_by_extension == Some(true) {
            // Get the base type's effective wildcard (recursively)
            let base_wildcard = if let Some(ref base_key) = ct.base_type {
                if let Some(TypeDef::Complex(base_ct)) = self.types.get(base_key) {
                    self.compute_effective_wildcard(base_ct)
                } else {
                    None
                }
            } else {
                None
            };

            // Union of base wildcard and derived wildcard
            match (&base_wildcard, &ct.attribute_wildcard) {
                (Some(base_wc), Some(derived_wc)) => Some(base_wc.union(derived_wc)),
                (Some(base_wc), None) => Some(base_wc.clone()),
                (None, Some(derived_wc)) => Some(derived_wc.clone()),
                (None, None) => None,
            }
        } else {
            // Restriction or not derived: use the type's own wildcard
            ct.attribute_wildcard.clone()
        }
    }

    /// Compute effective attributes for a complex type, including inherited attributes
    /// from the base type chain.
    ///
    /// - **Extension**: base attributes + derived attributes (derived can add new ones)
    /// - **Restriction**: derived attributes override base; attributes not mentioned in
    ///   the restriction are inherited; prohibited attributes are removed
    /// - **Not derived**: just the type's own attributes
    fn compute_effective_attributes(&self, ct: &ComplexTypeDef) -> Vec<AttributeDecl> {
        // Get base type's effective attributes
        let base_attrs = if let Some(ref base_key) = ct.base_type {
            if let Some(TypeDef::Complex(base_ct)) = self.types.get(base_key) {
                self.compute_effective_attributes(base_ct)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        if base_attrs.is_empty() {
            return ct.attributes.clone();
        }

        match ct.derived_by_extension {
            Some(true) => {
                // Extension: base attributes + derived attributes
                let mut result = base_attrs;
                for attr in &ct.attributes {
                    if !result.iter().any(|a| a.name == attr.name) {
                        result.push(attr.clone());
                    }
                }
                result
            }
            Some(false) => {
                // Restriction: start with base attributes, then apply overrides
                // Attributes explicitly declared in restriction replace base attrs.
                // Attributes with use="prohibited" remove the attribute.
                let mut result = Vec::new();
                for base_attr in &base_attrs {
                    // Check if the restriction overrides or prohibits this attribute
                    let override_attr = ct.attributes.iter().find(|a| a.name == base_attr.name);
                    if let Some(oa) = override_attr {
                        // Use the overridden version (but check if prohibited)
                        if !oa.prohibited {
                            result.push(oa.clone());
                        }
                        // If prohibited, skip it (don't add to result)
                    } else {
                        // Not mentioned in restriction: inherit from base
                        result.push(base_attr.clone());
                    }
                }
                // Also add any new attributes from restriction that aren't in base
                // (unusual but technically possible)
                for attr in &ct.attributes {
                    if !attr.prohibited && !result.iter().any(|a| a.name == attr.name) {
                        result.push(attr.clone());
                    }
                }
                result
            }
            None => {
                // Not derived
                ct.attributes.clone()
            }
        }
    }

    /// Compute the effective content model for a complex type, merging base type
    /// particles for extension types.
    ///
    /// For a type derived by extension from another complex type with a sequence
    /// content model, the effective content is the base type's particles followed
    /// by the extension's particles. This recursively walks the extension chain.
    ///
    /// Returns `None` if the type is not an extension or cannot be merged.
    fn compute_effective_particles(&self, ct: &ComplexTypeDef) -> Option<Vec<Particle>> {
        if ct.derived_by_extension != Some(true) {
            return None;
        }
        let (base_ns, base_name) = ct.base_type.as_ref()?;
        let key = (base_ns.clone(), base_name.clone());
        let base_type = self.types.get(&key)?;
        if let TypeDef::Complex(base_ct) = base_type {
            // Recursively get the base type's effective particles
            let base_particles = if let Some(recursive) = self.compute_effective_particles(base_ct)
            {
                recursive
            } else {
                // No further merging needed, just get the base type's own particles
                match &base_ct.content {
                    ContentModel::Sequence(particles, _, _) => particles.clone(),
                    ContentModel::Empty => Vec::new(),
                    _ => return None, // Can't merge non-sequence base content
                }
            };

            // Get the extension's own particles
            let ext_particles = match &ct.content {
                ContentModel::Sequence(particles, _, _) => particles.clone(),
                ContentModel::Empty => Vec::new(),
                _ => return None,
            };

            // Merge: base particles followed by extension particles
            let mut merged = base_particles;
            merged.extend(ext_particles);
            Some(merged)
        } else {
            None
        }
    }

    /// Validate a single element against its element declaration.
    ///
    /// This is the main per-element validation entry point. It handles:
    /// 1. Element reference resolution (is_ref → look up global declaration)
    /// 2. Abstract element rejection
    /// 3. `xsi:nil` processing (nillable elements with nil=true must be empty)
    /// 4. `xsi:type` override resolution and type substitution blocking
    /// 5. Type resolution and dispatch to complex/simple content validation
    /// 6. Identity constraint evaluation (key/unique/keyref)
    fn validate_element(
        &self,
        doc: &Document,
        node: NodeId,
        decl: &ElementDecl,
        errors: &mut Vec<ValidationError>,
    ) {
        // If this is an element reference, resolve the actual declaration from the
        // global elements map to get the real type_ref, nillable, block constraints, etc.
        if decl.is_ref {
            let key = (decl.namespace.clone(), decl.name.clone());
            if let Some(global_decl) = self.elements.get(&key) {
                let resolved = global_decl.clone();
                self.validate_element(doc, node, &resolved, errors);
                return;
            }
            // Fall through with the ref decl's AnyType if global not found
        }

        // Reject abstract elements: they cannot appear directly in instances
        if decl.is_abstract {
            errors.push(ValidationError {
                message: format!(
                    "Element '{}' is abstract and cannot appear in an instance document",
                    decl.name
                ),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
            return;
        }

        // Check for xsi:nil="true"
        if let Some(elem) = doc.element(node) {
            let xsi_nil_value = elem.get_attribute_ns(XSI_NAMESPACE, "nil").or_else(|| {
                elem.attributes
                    .iter()
                    .find(|a| a.name.local_name == "nil" && a.name.prefix.as_deref() == Some("xsi"))
                    .map(|a| &*a.value)
            });
            if xsi_nil_value == Some("true") || xsi_nil_value == Some("1") {
                if !decl.nillable {
                    errors.push(ValidationError {
                        message: "xsi:nil='true' on non-nillable element".to_string(),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                    return;
                }
                // Nillable element with xsi:nil="true": must be empty
                // (no child elements and no non-whitespace text content)
                let has_children = self.element_has_child_elements(doc, node);
                let text = doc.text_content_deep(node);
                let has_text = !text.trim().is_empty();
                if has_children || has_text {
                    errors.push(ValidationError {
                        message: "Element with xsi:nil='true' must have no content".to_string(),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                // Skip all further content validation — nilled element is valid if empty
                return;
            }
        }

        // Check for xsi:type override
        if let Some(xsi_type_ref) = self.resolve_xsi_type(doc, node) {
            match xsi_type_ref {
                XsiTypeResult::BuiltIn(bt) => {
                    // NOTATION cannot be used as the {type definition} of an element
                    if bt == BuiltInType::NOTATION {
                        errors.push(ValidationError {
                            message: "xs:NOTATION cannot be used as the type of an element"
                                .to_string(),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    if bt == BuiltInType::AnyType {
                        self.validate_children_against_global_decls(doc, node, errors);
                    } else {
                        // Simple built-in type: element must not have child elements
                        if self.element_has_child_elements(doc, node) {
                            errors.push(ValidationError {
                                message: format!(
                                    "Element with xsi:type '{}' must not have child elements",
                                    self.builtin_type_name(&bt)
                                ),
                                line: Some(doc.node_line(node)),
                                column: Some(doc.node_column(node)),
                            });
                            return;
                        }
                        let text = doc.text_content_deep(node);
                        validate_builtin_value(&text, &bt, doc, node, errors);
                    }
                    return;
                }
                XsiTypeResult::Named(td) => {
                    // Check that xsi:type is the declared type or derived from it
                    if !self.is_type_derived_from_decl(&td, &decl.type_ref) {
                        let type_name = match &td {
                            TypeDef::Complex(ct) => ct.name.as_deref().unwrap_or("anonymous"),
                            TypeDef::Simple(st) => st.name.as_deref().unwrap_or("anonymous"),
                        };
                        errors.push(ValidationError {
                            message: format!(
                                "xsi:type '{}' is not derived from the declared element type",
                                type_name,
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    // Check block constraints
                    // Get the declared type's block constraints
                    let (decl_type_block_ext, decl_type_block_rst) = match &decl.type_ref {
                        TypeRef::Named(ns, name) => {
                            let key = (ns.clone(), name.clone());
                            if let Some(TypeDef::Complex(ct)) = self.types.get(&key) {
                                (ct.block_extension, ct.block_restriction)
                            } else {
                                (false, false)
                            }
                        }
                        _ => (false, false),
                    };
                    if let Some(block_msg) = self.check_type_substitution_blocked(
                        &td,
                        decl.block_extension,
                        decl.block_restriction,
                        decl_type_block_ext,
                        decl_type_block_rst,
                    ) {
                        errors.push(ValidationError {
                            message: block_msg,
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    match td {
                        TypeDef::Complex(ct) => {
                            self.validate_complex_content(doc, node, &ct, errors);
                        }
                        TypeDef::Simple(st) => {
                            // Simple type: element must not have child elements
                            if self.element_has_child_elements(doc, node) {
                                errors.push(ValidationError {
                                    message: format!(
                                        "Element with simple xsi:type must not have child elements"
                                    ),
                                    line: Some(doc.node_line(node)),
                                    column: Some(doc.node_column(node)),
                                });
                                return;
                            }
                            self.validate_simple_content(doc, node, &st, errors);
                        }
                    }
                    return;
                }
                XsiTypeResult::NotFound(type_name) => {
                    errors.push(ValidationError {
                        message: format!("xsi:type '{}' not found", type_name),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                    return;
                }
            }
        }

        let type_def = self.resolve_type(&decl.type_ref);

        match type_def {
            Some(TypeDef::Complex(ct)) => {
                self.validate_complex_content(doc, node, ct, errors);
            }
            Some(TypeDef::Simple(st)) => {
                // Simple types cannot have child elements
                if self.element_has_child_elements(doc, node) {
                    let elem_name = doc
                        .element(node)
                        .map(|e| &*e.name.local_name)
                        .unwrap_or("?");
                    errors.push(ValidationError {
                        message: format!(
                            "Element '{}' has simple type but contains child elements",
                            elem_name
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
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
                            // Built-in simple types cannot have child elements
                            if self.element_has_child_elements(doc, node) {
                                let elem_name = doc
                                    .element(node)
                                    .map(|e| &*e.name.local_name)
                                    .unwrap_or("?");
                                errors.push(ValidationError {
                                    message: format!(
                                        "Element '{}' has simple type '{:?}' but contains child elements",
                                        elem_name, bt
                                    ),
                                    line: Some(doc.node_line(node)),
                                    column: Some(doc.node_column(node)),
                                });
                            }
                            let text = doc.text_content_deep(node);
                            validate_builtin_value(&text, bt, doc, node, errors);
                        }
                    }
                }
                // Otherwise, no validation possible (unknown type)
            }
        }

        // Evaluate identity constraints declared on this element
        if !decl.identity_constraints.is_empty() {
            self.evaluate_identity_constraints(doc, node, &decl.identity_constraints, errors);
        }
    }

    /// Resolve a type reference to a `TypeDef`.
    ///
    /// - `TypeRef::Named` → look up in `self.types` by (namespace, name) key
    /// - `TypeRef::Inline` → return the inline type definition directly
    /// - `TypeRef::BuiltIn` → return `None` (built-in types are handled separately)
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
    ///
    /// For elements typed as `xs:anyType`, any content is allowed, but child
    /// elements that have matching global declarations are still validated
    /// against those declarations.
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
                    child_elem
                        .name
                        .namespace_uri
                        .as_deref()
                        .map(|s| s.to_string()),
                    child_elem.name.local_name.to_string(),
                );
                let key_no_ns = (None, child_elem.name.local_name.to_string());

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

    /// Validate the complex content of an element against a complex type definition.
    ///
    /// This handles:
    /// - Required/optional attribute checking
    /// - Attribute value validation against declared types
    /// - Attribute wildcard processing (processContents=skip/lax/strict)
    /// - Rejecting undeclared attributes when no wildcard is present
    /// - Element-only text content rejection (non-mixed types)
    /// - Content model validation (sequence/choice/all/empty/simpleContent/any)
    /// - Extension type particle merging
    fn validate_complex_content(
        &self,
        doc: &Document,
        node: NodeId,
        ct: &ComplexTypeDef,
        errors: &mut Vec<ValidationError>,
    ) {
        // Compute effective attributes (including inherited from base types)
        let effective_attrs = self.compute_effective_attributes(ct);

        // Validate attributes
        if let Some(elem) = doc.element(node) {
            for attr_decl in &effective_attrs {
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
            for attr_decl in &effective_attrs {
                eprintln!(
                    "DEBUG: effective_attr name={} type_ref={:?}",
                    attr_decl.name, attr_decl.type_ref
                );
                if let Some(attr) = elem
                    .attributes
                    .iter()
                    .find(|a| a.name.local_name == attr_decl.name)
                {
                    let value = &attr.value;
                    eprintln!(
                        "DEBUG: validating attr {}={} against {:?}",
                        attr_decl.name, value, attr_decl.type_ref
                    );
                    self.validate_attribute_value(value, &attr_decl.type_ref, doc, node, errors);
                }
            }

            // Compute the effective wildcard: for types derived by extension,
            // merge the base type's wildcard with the derived type's wildcard (union).
            let effective_wildcard = self.compute_effective_wildcard(ct);

            // Validate unmatched attributes against wildcard or reject if no wildcard
            if let Some(ref wildcard) = effective_wildcard {
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
                    let already_declared = effective_attrs
                        .iter()
                        .any(|ad| ad.name == attr.name.local_name);
                    if already_declared {
                        continue;
                    }

                    let attr_ns_str = attr.name.namespace_uri.as_deref().map(|s| s.to_string());

                    // Check namespace constraint
                    if !wildcard.allows_namespace(attr_ns_str.as_deref()) {
                        errors.push(ValidationError {
                            message: format!(
                                "Attribute '{}' in namespace '{}' is not allowed by wildcard constraint",
                                attr.name.local_name,
                                attr_ns_str.as_deref().unwrap_or("(no namespace)")
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        continue;
                    }

                    // processContents validation
                    match wildcard.process_contents {
                        ProcessContents::Skip => {
                            // No validation needed
                        }
                        ProcessContents::Lax | ProcessContents::Strict => {
                            // Look up in global attribute declarations
                            let key = (attr_ns_str.clone(), attr.name.local_name.to_string());
                            let global_decl = self.global_attributes.get(&key).or_else(|| {
                                let key2 = (
                                    self.target_namespace.clone(),
                                    attr.name.local_name.to_string(),
                                );
                                self.global_attributes.get(&key2)
                            });
                            match global_decl {
                                Some(decl) => {
                                    // Validate attribute value against its declared type
                                    self.validate_attribute_value(
                                        &attr.value,
                                        &decl.type_ref,
                                        doc,
                                        node,
                                        errors,
                                    );
                                }
                                None => {
                                    // For strict: must find a declaration
                                    if wildcard.process_contents == ProcessContents::Strict {
                                        errors.push(ValidationError {
                                            message: format!(
                                                "Attribute '{}' in namespace '{}' has no global declaration (strict processContents)",
                                                attr.name.local_name,
                                                attr_ns_str.as_deref().unwrap_or("(no namespace)")
                                            ),
                                            line: Some(doc.node_line(node)),
                                            column: Some(doc.node_column(node)),
                                        });
                                    }
                                    // For lax: no declaration is OK
                                }
                            }
                        }
                    }
                }
            } else {
                // No wildcard: reject any undeclared attributes
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
                    // Check if declared
                    let already_declared = effective_attrs
                        .iter()
                        .any(|ad| ad.name == attr.name.local_name);
                    if !already_declared {
                        errors.push(ValidationError {
                            message: format!(
                                "Attribute '{}' is not allowed (no wildcard permits additional attributes)",
                                attr.name.local_name,
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
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

        // For non-mixed element-only content models, reject non-whitespace text
        if !ct.mixed {
            let is_element_only = matches!(
                ct.content,
                ContentModel::Sequence(..) | ContentModel::Choice(..) | ContentModel::All(..)
            );
            if is_element_only {
                for child in doc.children(node) {
                    if let Some(text) = doc.text_content(child) {
                        if !text.trim().is_empty() {
                            errors.push(ValidationError {
                                message:
                                    "Non-whitespace text content is not allowed in element-only content"
                                        .to_string(),
                                line: Some(doc.node_line(child)),
                                column: Some(doc.node_column(child)),
                            });
                            break; // report once
                        }
                    }
                }
            }
        }

        // For extension types, merge base type's particles with extension's particles
        if let Some(merged_particles) = self.compute_effective_particles(ct) {
            let consumed = self.validate_sequence(
                doc,
                &child_elements,
                &merged_particles,
                1,
                &MaxOccurs::Bounded(1),
                node,
                errors,
            );
            // Report remaining children as unexpected
            for &remaining in &child_elements[consumed..] {
                if let Some(elem) = doc.element(remaining) {
                    errors.push(ValidationError {
                        message: format!(
                            "Unexpected element '{}' in sequence",
                            elem.name.local_name
                        ),
                        line: Some(doc.node_line(remaining)),
                        column: Some(doc.node_column(remaining)),
                    });
                }
            }
            return;
        }

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
            ContentModel::Sequence(particles, min_occurs, max_occurs) => {
                let consumed = self.validate_sequence(
                    doc,
                    &child_elements,
                    particles,
                    *min_occurs,
                    max_occurs,
                    node,
                    errors,
                );
                // Report remaining children as unexpected
                for &remaining in &child_elements[consumed..] {
                    if let Some(elem) = doc.element(remaining) {
                        errors.push(ValidationError {
                            message: format!(
                                "Unexpected element '{}' in sequence",
                                elem.name.local_name
                            ),
                            line: Some(doc.node_line(remaining)),
                            column: Some(doc.node_column(remaining)),
                        });
                    }
                }
            }
            ContentModel::Choice(particles, min_occurs, max_occurs) => {
                let consumed = self.validate_choice(
                    doc,
                    &child_elements,
                    particles,
                    *min_occurs,
                    max_occurs,
                    node,
                    errors,
                );
                // Report remaining children as unexpected
                for &remaining in &child_elements[consumed..] {
                    if let Some(elem) = doc.element(remaining) {
                        errors.push(ValidationError {
                            message: format!(
                                "Unexpected element '{}' after choice",
                                elem.name.local_name
                            ),
                            line: Some(doc.node_line(remaining)),
                            column: Some(doc.node_column(remaining)),
                        });
                    }
                }
            }
            ContentModel::All(particles) => {
                self.validate_all(doc, &child_elements, particles, node, errors);
            }
            ContentModel::SimpleContent(type_ref) => {
                match type_ref.as_ref() {
                    TypeRef::BuiltIn(bt) => {
                        let text = doc.text_content_deep(node);
                        validate_builtin_value(&text, bt, doc, node, errors);
                    }
                    TypeRef::Named(ns, local_name) => {
                        let key = (ns.clone(), local_name.clone());
                        if let Some(type_def) = self.types.get(&key) {
                            match type_def {
                                TypeDef::Simple(st) => {
                                    self.validate_simple_content(doc, node, st, errors);
                                }
                                TypeDef::Complex(_) => {
                                    // Complex base type for simpleContent — text validated against
                                    // the complex type's own simpleContent base (recursively)
                                }
                            }
                        }
                    }
                    TypeRef::Inline(inner_type_def) => {
                        if let TypeDef::Simple(st) = inner_type_def.as_ref() {
                            self.validate_simple_content(doc, node, st, errors);
                        }
                    }
                }
            }
            ContentModel::Any => {
                // Any content is valid
            }
        }
    }

    /// Find a global element declaration by local name and namespace.
    fn find_global_element(&self, name: &str, ns: Option<&str>) -> Option<ElementDecl> {
        let key = (ns.map(|s| s.to_string()), name.to_string());
        self.elements.get(&key).cloned()
    }

    /// Check if an instance element matches a declared element or is a member of
    /// its substitution group.
    ///
    /// Returns `Some(global_decl)` if the element is a substitution group member
    /// that should be validated against its own declaration.
    /// Returns `None` if it's a direct match (caller handles validation) or no match.
    fn element_matches_with_substitution(
        &self,
        elem_name: &str,
        elem_ns: Option<&str>,
        decl: &ElementDecl,
    ) -> Option<ElementDecl> {
        // Check if the instance element is a member of the substitution group
        // headed by decl.
        let head_key = (decl.namespace.clone(), decl.name.clone());
        if let Some(members) = self.substitution_groups.get(&head_key) {
            let elem_key = (elem_ns.map(|s| s.to_string()), elem_name.to_string());
            if members.contains(&elem_key) {
                // Found: the instance element substitutes for the declared element.
                // Return the member's own global declaration for type validation.
                return self.find_global_element(elem_name, elem_ns);
            }
        }
        None
    }

    /// Validate a sequence content model.
    ///
    /// Iterates through the sequence's particles in order, matching child elements.
    /// The sequence can repeat between `compositor_min` and `compositor_max` times.
    /// Each particle within the sequence has its own min/max occurs constraints.
    ///
    /// Returns the number of child elements consumed.
    fn validate_sequence(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        compositor_min: u64,
        compositor_max: &MaxOccurs,
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) -> usize {
        let max_reps = match compositor_max {
            MaxOccurs::Bounded(n) => *n,
            MaxOccurs::Unbounded => u64::MAX,
        };

        let mut child_idx = 0;
        let mut seq_reps = 0u64;

        // Outer loop: repeat the entire sequence up to max_reps times
        'outer: while seq_reps < max_reps {
            let start_idx = child_idx;

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
                                    (Some(_), None) => false,
                                    (None, Some(_)) => false,
                                };
                                if name_matches && ns_matches {
                                    self.validate_element(doc, child, decl, errors);
                                    count += 1;
                                    child_idx += 1;
                                } else if let Some(subst_decl) = self
                                    .element_matches_with_substitution(
                                        &elem.name.local_name,
                                        elem.name.namespace_uri.as_deref(),
                                        decl,
                                    )
                                {
                                    // Element is a substitution group member;
                                    // validate against its own declaration.
                                    self.validate_element(doc, child, &subst_decl, errors);
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
                            if seq_reps >= compositor_min {
                                // We've already met the minimum repetitions,
                                // so this is just the sequence not starting another rep.
                                // Roll back to start_idx for this failed rep.
                                child_idx = start_idx;
                                break 'outer;
                            }
                            errors.push(ValidationError {
                                message: format!(
                                    "Expected at least {} occurrence(s) of element '{}', found {}",
                                    particle.min_occurs, decl.name, count
                                ),
                                line: Some(doc.node_line(parent)),
                                column: Some(doc.node_column(parent)),
                            });
                            break 'outer;
                        }
                    }
                    ParticleKind::Sequence(sub_particles) => {
                        let sub_min = particle.min_occurs;
                        let sub_max = &particle.max_occurs;
                        let before = errors.len();
                        let consumed = self.validate_sequence(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        child_idx += consumed;
                        if errors.len() > before {
                            break 'outer;
                        }
                    }
                    ParticleKind::Choice(sub_particles) => {
                        let sub_min = particle.min_occurs;
                        let sub_max = &particle.max_occurs;
                        let consumed = self.validate_choice(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        child_idx += consumed;
                    }
                    ParticleKind::Any {
                        namespace_constraint,
                        process_contents,
                    } => {
                        // xs:any element wildcard: consume matching children
                        while child_idx < children.len() && count < max {
                            let child = children[child_idx];
                            if let Some(elem) = doc.element(child) {
                                if wildcard_allows_namespace(
                                    namespace_constraint,
                                    elem.name.namespace_uri.as_deref(),
                                ) {
                                    // Namespace matches; apply processContents
                                    match process_contents {
                                        ProcessContents::Skip => {
                                            // Accept unconditionally
                                        }
                                        ProcessContents::Lax => {
                                            // Try to find global element declaration; validate if found
                                            if let Some(global_decl) = self.find_global_element(
                                                &elem.name.local_name,
                                                elem.name.namespace_uri.as_deref(),
                                            ) {
                                                self.validate_element(
                                                    doc,
                                                    child,
                                                    &global_decl,
                                                    errors,
                                                );
                                            }
                                        }
                                        ProcessContents::Strict => {
                                            // Must find global element declaration
                                            if let Some(global_decl) = self.find_global_element(
                                                &elem.name.local_name,
                                                elem.name.namespace_uri.as_deref(),
                                            ) {
                                                self.validate_element(
                                                    doc,
                                                    child,
                                                    &global_decl,
                                                    errors,
                                                );
                                            } else {
                                                errors.push(ValidationError {
                                                    message: format!(
                                                        "No global element declaration found for '{}' (strict wildcard)",
                                                        elem.name.local_name
                                                    ),
                                                    line: Some(doc.node_line(child)),
                                                    column: Some(doc.node_column(child)),
                                                });
                                            }
                                        }
                                    }
                                    count += 1;
                                    child_idx += 1;
                                } else {
                                    break; // namespace doesn't match
                                }
                            } else {
                                break;
                            }
                        }
                        if count < particle.min_occurs {
                            if seq_reps >= compositor_min {
                                child_idx = start_idx;
                                break 'outer;
                            }
                            errors.push(ValidationError {
                                message: format!(
                                    "Expected at least {} element(s) matching wildcard, found {}",
                                    particle.min_occurs, count
                                ),
                                line: Some(doc.node_line(parent)),
                                column: Some(doc.node_column(parent)),
                            });
                            break 'outer;
                        }
                    }
                }
            }

            seq_reps += 1;

            // If no progress was made, stop looping
            if child_idx == start_idx {
                break;
            }

            // If all children consumed, stop
            if child_idx >= children.len() {
                break;
            }
        }

        if seq_reps < compositor_min {
            errors.push(ValidationError {
                message: format!(
                    "Sequence must occur at least {} time(s), found {}",
                    compositor_min, seq_reps
                ),
                line: Some(doc.node_line(parent)),
                column: Some(doc.node_column(parent)),
            });
        }

        child_idx
    }

    /// Validate a choice content model.
    ///
    /// Tries to match the current child element against one of the choice alternatives.
    /// The choice can repeat between `compositor_min` and `compositor_max` times.
    /// Each repetition picks one matching alternative and consumes its elements.
    ///
    /// Returns the number of child elements consumed.
    fn validate_choice(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        compositor_min: u64,
        compositor_max: &MaxOccurs,
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) -> usize {
        let max_reps = match compositor_max {
            MaxOccurs::Bounded(n) => *n,
            MaxOccurs::Unbounded => u64::MAX,
        };

        if children.is_empty() {
            if compositor_min > 0 {
                // Check if any particle allows 0 occurrences
                let any_optional = particles.iter().any(|p| p.min_occurs == 0);
                if !any_optional && !particles.is_empty() {
                    errors.push(ValidationError {
                        message: "Expected one of the choice alternatives".to_string(),
                        line: Some(doc.node_line(parent)),
                        column: Some(doc.node_column(parent)),
                    });
                }
            }
            return 0;
        }

        let mut child_idx = 0;
        let mut choice_reps = 0u64;

        // Outer loop: repeat the choice up to max_reps times.
        // Each iteration picks one alternative and consumes matching children for it.
        while choice_reps < max_reps && child_idx < children.len() {
            let current_child = children[child_idx];
            let elem = match doc.element(current_child) {
                Some(e) => e,
                None => break,
            };

            // Try to match the current child against one of the choice alternatives
            let mut matched_any = false;

            for p in particles {
                match &p.kind {
                    ParticleKind::Element(decl) => {
                        let name_matches = decl.name == elem.name.local_name;
                        let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                            (Some(a), Some(b)) => a == b,
                            (None, None) => true,
                            (Some(_), None) => false,
                            (None, Some(_)) => false,
                        };
                        // Check for direct match or substitution group match
                        let subst_decl = if !(name_matches && ns_matches) {
                            self.element_matches_with_substitution(
                                &elem.name.local_name,
                                elem.name.namespace_uri.as_deref(),
                                decl,
                            )
                        } else {
                            None
                        };
                        if (name_matches && ns_matches) || subst_decl.is_some() {
                            // Consume as many consecutive elements as allowed by max_occurs
                            // (matching either the declared element or substitution group members)
                            let max = match p.max_occurs {
                                MaxOccurs::Bounded(n) => n as usize,
                                MaxOccurs::Unbounded => usize::MAX,
                            };
                            let mut count = 0usize;
                            while child_idx < children.len() && count < max {
                                let child = children[child_idx];
                                if let Some(child_elem) = doc.element(child) {
                                    let cn_matches = child_elem.name.local_name == decl.name;
                                    let cns_matches =
                                        match (&child_elem.name.namespace_uri, &decl.namespace) {
                                            (Some(a), Some(b)) => a == b,
                                            (None, None) => true,
                                            _ => false,
                                        };
                                    if cn_matches && cns_matches {
                                        self.validate_element(doc, child, decl, errors);
                                        child_idx += 1;
                                        count += 1;
                                    } else if let Some(child_subst) = self
                                        .element_matches_with_substitution(
                                            &child_elem.name.local_name,
                                            child_elem.name.namespace_uri.as_deref(),
                                            decl,
                                        )
                                    {
                                        self.validate_element(doc, child, &child_subst, errors);
                                        child_idx += 1;
                                        count += 1;
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            if count < p.min_occurs as usize {
                                errors.push(ValidationError {
                                    message: format!(
                                        "Expected at least {} occurrence(s) of element '{}' in choice, found {}",
                                        p.min_occurs, decl.name, count
                                    ),
                                    line: Some(doc.node_line(current_child)),
                                    column: Some(doc.node_column(current_child)),
                                });
                            }
                            matched_any = true;
                            break;
                        }
                    }
                    ParticleKind::Sequence(sub_particles) => {
                        let sub_min = p.min_occurs;
                        let sub_max = &p.max_occurs;
                        let before_errors = errors.len();
                        let consumed = self.validate_sequence(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        if consumed > 0 {
                            child_idx += consumed;
                            matched_any = true;
                            // If the nested sequence produced errors, we still consumed
                            // elements — but we should stop the choice loop
                            if errors.len() > before_errors {
                                choice_reps += 1;
                                break;
                            }
                            break;
                        } else if errors.len() > before_errors {
                            // Sequence matched nothing but produced errors — try next alternative
                            // Roll back errors from this failed attempt
                            errors.truncate(before_errors);
                        }
                        // If consumed == 0 and no errors, this alternative didn't match; try next
                    }
                    ParticleKind::Choice(sub_particles) => {
                        let sub_min = p.min_occurs;
                        let sub_max = &p.max_occurs;
                        let before_errors = errors.len();
                        let consumed = self.validate_choice(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        if consumed > 0 {
                            child_idx += consumed;
                            matched_any = true;
                            break;
                        } else if errors.len() > before_errors {
                            // Sub-choice matched nothing but produced errors — try next alternative
                            errors.truncate(before_errors);
                        }
                    }
                    ParticleKind::Any {
                        namespace_constraint,
                        process_contents,
                    } => {
                        if wildcard_allows_namespace(
                            namespace_constraint,
                            elem.name.namespace_uri.as_deref(),
                        ) {
                            // Consume as many wildcard-matching elements as allowed
                            let max = match p.max_occurs {
                                MaxOccurs::Bounded(n) => n as usize,
                                MaxOccurs::Unbounded => usize::MAX,
                            };
                            let mut count = 0usize;
                            while child_idx < children.len() && count < max {
                                let child = children[child_idx];
                                if let Some(child_elem) = doc.element(child) {
                                    if wildcard_allows_namespace(
                                        namespace_constraint,
                                        child_elem.name.namespace_uri.as_deref(),
                                    ) {
                                        match process_contents {
                                            ProcessContents::Skip => {}
                                            ProcessContents::Lax => {
                                                if let Some(global_decl) = self.find_global_element(
                                                    &child_elem.name.local_name,
                                                    child_elem.name.namespace_uri.as_deref(),
                                                ) {
                                                    self.validate_element(
                                                        doc,
                                                        child,
                                                        &global_decl,
                                                        errors,
                                                    );
                                                }
                                            }
                                            ProcessContents::Strict => {
                                                if let Some(global_decl) = self.find_global_element(
                                                    &child_elem.name.local_name,
                                                    child_elem.name.namespace_uri.as_deref(),
                                                ) {
                                                    self.validate_element(
                                                        doc,
                                                        child,
                                                        &global_decl,
                                                        errors,
                                                    );
                                                } else {
                                                    errors.push(ValidationError {
                                                        message: format!(
                                                            "No global element declaration found for '{}' (strict wildcard)",
                                                            child_elem.name.local_name
                                                        ),
                                                        line: Some(doc.node_line(child)),
                                                        column: Some(doc.node_column(child)),
                                                    });
                                                }
                                            }
                                        }
                                        child_idx += 1;
                                        count += 1;
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            matched_any = count > 0;
                            if matched_any {
                                break;
                            }
                        }
                    }
                }
            }

            if !matched_any {
                // Current child doesn't match any alternative
                if choice_reps < compositor_min {
                    errors.push(ValidationError {
                        message: format!(
                            "Element '{}' does not match any choice alternative",
                            elem.name.local_name
                        ),
                        line: Some(doc.node_line(current_child)),
                        column: Some(doc.node_column(current_child)),
                    });
                }
                break;
            }

            choice_reps += 1;
        }

        if choice_reps < compositor_min {
            if child_idx == 0 && errors.is_empty() {
                // No children matched and no error was emitted yet
                let any_optional = particles.iter().any(|p| p.min_occurs == 0);
                if !any_optional && !particles.is_empty() {
                    errors.push(ValidationError {
                        message: "Expected one of the choice alternatives".to_string(),
                        line: Some(doc.node_line(parent)),
                        column: Some(doc.node_column(parent)),
                    });
                }
            }
        }

        child_idx
    }

    /// Validate an `xs:all` content model.
    ///
    /// In an all group, each particle can appear at most once, and order doesn't matter.
    /// Required particles (min_occurs > 0) must all be present.
    /// Supports both element particles and wildcard particles.
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
                    match &particle.kind {
                        ParticleKind::Element(decl) => {
                            let name_matches = decl.name == elem.name.local_name;
                            let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                                (Some(a), Some(b)) => a == b,
                                (None, None) => true,
                                (Some(_), None) => false,
                                (None, Some(_)) => false,
                            };
                            let subst_decl = if !(name_matches && ns_matches) {
                                self.element_matches_with_substitution(
                                    &elem.name.local_name,
                                    elem.name.namespace_uri.as_deref(),
                                    decl,
                                )
                            } else {
                                None
                            };
                            if (name_matches && ns_matches) || subst_decl.is_some() {
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
                                    if let Some(ref sd) = subst_decl {
                                        self.validate_element(doc, child, sd, errors);
                                    } else {
                                        self.validate_element(doc, child, decl, errors);
                                    }
                                }
                                found = true;
                                break;
                            }
                        }
                        ParticleKind::Any {
                            namespace_constraint,
                            process_contents,
                        } => {
                            if wildcard_allows_namespace(
                                namespace_constraint,
                                elem.name.namespace_uri.as_deref(),
                            ) {
                                matched[i] = true;
                                match process_contents {
                                    ProcessContents::Skip => {}
                                    ProcessContents::Lax => {
                                        if let Some(global_decl) = self.find_global_element(
                                            &elem.name.local_name,
                                            elem.name.namespace_uri.as_deref(),
                                        ) {
                                            self.validate_element(doc, child, &global_decl, errors);
                                        }
                                    }
                                    ProcessContents::Strict => {
                                        if let Some(global_decl) = self.find_global_element(
                                            &elem.name.local_name,
                                            elem.name.namespace_uri.as_deref(),
                                        ) {
                                            self.validate_element(doc, child, &global_decl, errors);
                                        } else {
                                            errors.push(ValidationError {
                                                message: format!(
                                                    "No global element declaration found for '{}' (strict wildcard)",
                                                    elem.name.local_name
                                                ),
                                                line: Some(doc.node_line(child)),
                                                column: Some(doc.node_column(child)),
                                            });
                                        }
                                    }
                                }
                                found = true;
                                break;
                            }
                        }
                        _ => {}
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

    /// Validate simple (text) content of an element against a simple type definition.
    ///
    /// Handles both list types (whitespace-separated items validated individually)
    /// and atomic types. Applies XSD whiteSpace normalization before validation.
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
                        validate_facet(
                            item,
                            facet,
                            item_bt,
                            doc,
                            node,
                            errors,
                            self.enforce_qname_length_facets,
                        );
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
                validate_facet(
                    &text,
                    facet,
                    &st.base,
                    doc,
                    node,
                    errors,
                    self.enforce_qname_length_facets,
                );
            }
        }
    }

    /// Validate an attribute value against its declared type reference.
    ///
    /// Handles all three forms of type references:
    /// - `BuiltIn` → validate directly against the built-in type
    /// - `Inline` → resolve to simple type and validate with facets
    /// - `Named` → look up in schema types map and validate
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
                                        validate_facet(
                                            item,
                                            facet,
                                            item_bt,
                                            doc,
                                            node,
                                            errors,
                                            self.enforce_qname_length_facets,
                                        );
                                    }
                                }
                            }
                            for facet in &st.facets {
                                validate_list_facet(&items, facet, value, doc, node, errors);
                            }
                        } else {
                            validate_builtin_value(value, &st.base, doc, node, errors);
                            for facet in &st.facets {
                                validate_facet(
                                    value,
                                    facet,
                                    &st.base,
                                    doc,
                                    node,
                                    errors,
                                    self.enforce_qname_length_facets,
                                );
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
                eprintln!(
                    "DEBUG: validate_attribute_value Named key={:?} found={}",
                    key,
                    self.types.contains_key(&key)
                );
                if let Some(TypeDef::Simple(st)) = self.types.get(&key) {
                    eprintln!(
                        "DEBUG: SimpleTypeDef base={:?} facets={:?}",
                        st.base, st.facets
                    );
                    if st.is_list {
                        let items: Vec<&str> = value.split_whitespace().collect();
                        if let Some(ref item_bt) = st.item_type {
                            for item in &items {
                                validate_builtin_value(item, item_bt, doc, node, errors);
                                for facet in &st.item_facets {
                                    validate_facet(
                                        item,
                                        facet,
                                        item_bt,
                                        doc,
                                        node,
                                        errors,
                                        self.enforce_qname_length_facets,
                                    );
                                }
                            }
                        }
                        for facet in &st.facets {
                            validate_list_facet(&items, facet, value, doc, node, errors);
                        }
                    } else {
                        validate_builtin_value(value, &st.base, doc, node, errors);
                        for facet in &st.facets {
                            validate_facet(
                                value,
                                facet,
                                &st.base,
                                doc,
                                node,
                                errors,
                                self.enforce_qname_length_facets,
                            );
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
