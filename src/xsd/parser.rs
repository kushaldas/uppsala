//! XSD schema parsing — converts a parsed DOM tree of an XSD document into
//! the internal type/element/attribute/group declarations used by the validator.
//!
//! ## Key functions
//!
//! | Function | Purpose |
//! |----------|---------|
//! | `parse_element_decl` | Top-level or local `<xs:element>` → `ElementDecl` |
//! | `parse_identity_constraints` | `<xs:key>`, `<xs:unique>`, `<xs:keyref>` on an element |
//! | `parse_substitution_group` | Parses `substitutionGroup` QName attribute |
//! | `resolve_type_name` | QName → `TypeRef` (built-in or named) |
//! | `parse_builtin_type` | Local name → `BuiltInType` variant |
//! | `parse_complex_type` | `<xs:complexType>` → `TypeDef::Complex` |
//! | `parse_any_attribute` | `<xs:anyAttribute>` → `AttributeWildcard` |
//! | `parse_attribute_group_def` | `<xs:attributeGroup>` definition |
//! | `parse_model_group_def` | `<xs:group>` definition (sequence/choice/all) |
//! | `strip_prefix` | Removes namespace prefix from a QName string |
//! | `parse_particles` | Children of a compositor → `Vec<Particle>` |
//! | `parse_attribute_decl` | `<xs:attribute>` → `AttributeDecl` |
//! | `parse_simple_type` | `<xs:simpleType>` → `TypeDef::Simple` (with facets, list detection) |

use std::collections::HashMap;

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{XmlError, XmlResult};
use crate::namespace::build_resolver_for_node;

use super::types::{
    AttributeDecl, AttributeGroupDef, AttributeWildcard, BuiltInType, ComplexTypeDef, ContentModel,
    ElementDecl, Facet, IdentityConstraint, IdentityConstraintKind, MaxOccurs, ModelGroupDef,
    NamespaceConstraint, Particle, ParticleKind, ProcessContents, SimpleTypeDef, TypeDef, TypeRef,
    WhiteSpaceHandling,
};
use super::XS_NAMESPACE;

/// Parse a top-level or local `<xs:element>` declaration into an `ElementDecl`.
///
/// Handles:
/// - `name`, `type`, `form`, `minOccurs`, `maxOccurs`, `nillable`, `block`, `abstract`
/// - Inline `<xs:complexType>` or `<xs:simpleType>` children
/// - `substitutionGroup` attribute resolution
/// - Identity constraint children (`xs:key`, `xs:unique`, `xs:keyref`)
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_element_decl(
    doc: &Document,
    node: NodeId,
    target_ns: &Option<String>,
    local_elem_ns: &Option<String>,
    schema_target_ns: &Option<String>,
    attribute_groups: &HashMap<(Option<String>, String), AttributeGroupDef>,
    model_groups: &HashMap<(Option<String>, String), ModelGroupDef>,
    block_default_ext: bool,
    block_default_rst: bool,
) -> XmlResult<ElementDecl> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for element declaration"))?;

    let name = elem
        .get_attribute("name")
        .ok_or_else(|| XmlError::validation("Element declaration missing 'name' attribute"))?
        .to_string();

    // Determine effective namespace based on form attribute
    let effective_ns = match elem.get_attribute("form") {
        Some("qualified") => schema_target_ns.clone(),
        Some("unqualified") => None,
        _ => target_ns.clone(), // Use default (from elementFormDefault)
    };

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
    let fixed = elem.get_attribute("fixed").map(|s| s.to_string());

    // Parse block attribute (or use blockDefault)
    let (block_ext, block_rst) = if let Some(block) = elem.get_attribute("block") {
        let mut ext = false;
        let mut rst = false;
        for token in block.split_whitespace() {
            match token {
                "extension" => ext = true,
                "restriction" => rst = true,
                "#all" => {
                    ext = true;
                    rst = true;
                }
                _ => {}
            }
        }
        (ext, rst)
    } else {
        (block_default_ext, block_default_rst)
    };

    let type_ref = if let Some(type_name) = elem.get_attribute("type") {
        resolve_type_name(type_name, schema_target_ns)
    } else {
        // Check for inline type definition
        let mut inline_type = TypeRef::BuiltIn(BuiltInType::AnyType);
        for child in doc.children(node) {
            if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
                let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || child_elem.name.prefix.as_deref() == Some("xs")
                    || child_elem.name.prefix.as_deref() == Some("xsd");
                if is_xs && child_elem.name.local_name == "complexType" {
                    inline_type = TypeRef::Inline(Box::new(parse_complex_type(
                        doc,
                        child,
                        local_elem_ns,
                        target_ns,
                        schema_target_ns,
                        attribute_groups,
                        model_groups,
                        block_default_ext,
                        block_default_rst,
                    )?));
                } else if is_xs && child_elem.name.local_name == "simpleType" {
                    inline_type = TypeRef::Inline(Box::new(parse_simple_type(doc, child)?));
                }
            }
        }
        inline_type
    };

    Ok(ElementDecl {
        name,
        namespace: effective_ns,
        type_ref,
        min_occurs,
        max_occurs,
        nillable,
        block_extension: block_ext,
        block_restriction: block_rst,
        is_ref: false,
        substitution_group: parse_substitution_group(elem, schema_target_ns),
        is_abstract: elem.get_attribute("abstract") == Some("true"),
        fixed,
        identity_constraints: parse_identity_constraints(doc, node),
    })
}

/// Parse identity constraints (`xs:key`, `xs:unique`, `xs:keyref`) from an element declaration.
///
/// Each constraint has a `name`, a `selector` XPath, one or more `field` XPaths,
/// and an optional `refer` attribute (for `xs:keyref`).
fn parse_identity_constraints(doc: &Document, elem_node: NodeId) -> Vec<IdentityConstraint> {
    let mut constraints = Vec::new();
    for child in doc.children(elem_node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }
            let kind = match child_elem.name.local_name.as_ref() {
                "key" => IdentityConstraintKind::Key,
                "unique" => IdentityConstraintKind::Unique,
                "keyref" => IdentityConstraintKind::KeyRef,
                _ => continue,
            };
            if let Some(ce) = doc.element(child) {
                let name = ce.get_attribute("name").unwrap_or("").to_string();
                let refer = ce.get_attribute("refer").map(|s| {
                    // Strip namespace prefix from refer if present
                    if let Some(colon) = s.find(':') {
                        s[colon + 1..].to_string()
                    } else {
                        s.to_string()
                    }
                });

                let mut selector = String::new();
                let mut fields = Vec::new();

                for gc in doc.children(child) {
                    if let Some(NodeKind::Element(gc_elem)) = doc.node_kind(gc) {
                        let gc_is_xs = gc_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                            || gc_elem.name.prefix.as_deref() == Some("xs")
                            || gc_elem.name.prefix.as_deref() == Some("xsd");
                        if !gc_is_xs {
                            continue;
                        }
                        if let Some(gce) = doc.element(gc) {
                            match gc_elem.name.local_name.as_ref() {
                                "selector" => {
                                    selector = gce.get_attribute("xpath").unwrap_or("").to_string();
                                }
                                "field" => {
                                    fields
                                        .push(gce.get_attribute("xpath").unwrap_or("").to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                }

                debug_log!(
                    "parsed identity constraint: kind={:?} name={} selector={} fields={:?} refer={:?}",
                    kind, name, selector, fields, refer
                );

                constraints.push(IdentityConstraint {
                    name,
                    kind,
                    selector,
                    fields,
                    refer,
                });
            }
        }
    }
    constraints
}

/// Parse the `substitutionGroup` attribute from an element declaration.
///
/// Returns `Some((namespace, local_name))` of the head element, or `None`
/// if no substitution group is declared.
fn parse_substitution_group(
    elem: &crate::dom::Element,
    schema_target_ns: &Option<String>,
) -> Option<(Option<String>, String)> {
    let sg = elem.get_attribute("substitutionGroup")?;
    // The substitutionGroup value is a QName
    if let Some(colon) = sg.find(':') {
        let prefix = &sg[..colon];
        let local = &sg[colon + 1..];
        // Resolve the prefix to a namespace URI from the element's namespace declarations
        let ns_uri = elem
            .attributes
            .iter()
            .find(|a| a.name.prefix.as_deref() == Some("xmlns") && a.name.local_name == prefix)
            .map(|a| a.value.to_string())
            .or_else(|| {
                // For elements in the schema's target namespace with a matching prefix,
                // use the target namespace
                schema_target_ns.clone()
            });
        Some((ns_uri, local.to_string()))
    } else {
        // Unprefixed: use the target namespace (substitution group heads are top-level elements)
        Some((schema_target_ns.clone(), sg.to_string()))
    }
}

/// Resolve a type name string (possibly with `xs:`/`xsd:` prefix) to a `TypeRef`.
///
/// If the name matches a built-in XSD type, returns `TypeRef::BuiltIn`.
/// Otherwise returns `TypeRef::Named` with the appropriate namespace.
pub(super) fn resolve_type_name(type_name: &str, target_ns: &Option<String>) -> TypeRef {
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

    // Even without an xs:/xsd: prefix, check if the local name matches a built-in type.
    // This handles schemas where the XSD namespace is the default namespace
    // (e.g., xmlns="http://www.w3.org/2001/XMLSchema").
    if prefix.is_empty() {
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

/// Map a local type name to the corresponding `BuiltInType` variant.
///
/// Returns `None` if the name does not match any XSD built-in type.
pub(super) fn parse_builtin_type(name: &str) -> Option<BuiltInType> {
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

/// Parse a `<xs:complexType>` element into a `TypeDef::Complex`.
///
/// Handles direct compositor children (`sequence`, `choice`, `all`),
/// `group` references, `attribute`/`attributeGroup`/`anyAttribute`,
/// and `simpleContent`/`complexContent` with `extension`/`restriction`.
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_complex_type(
    doc: &Document,
    node: NodeId,
    local_elem_ns: &Option<String>,
    target_ns: &Option<String>,
    schema_target_ns: &Option<String>,
    attribute_groups: &HashMap<(Option<String>, String), AttributeGroupDef>,
    model_groups: &HashMap<(Option<String>, String), ModelGroupDef>,
    block_default_ext: bool,
    block_default_rst: bool,
) -> XmlResult<TypeDef> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for complexType"))?;

    let name = elem.get_attribute("name").map(|s| s.to_string());
    let mixed = elem.get_attribute("mixed") == Some("true");

    // Parse block attribute on complexType (or use blockDefault)
    let (block_ext, block_rst) = if let Some(block) = elem.get_attribute("block") {
        let mut ext = false;
        let mut rst = false;
        for token in block.split_whitespace() {
            match token {
                "extension" => ext = true,
                "restriction" => rst = true,
                "#all" => {
                    ext = true;
                    rst = true;
                }
                _ => {}
            }
        }
        (ext, rst)
    } else {
        (block_default_ext, block_default_rst)
    };

    let mut content = ContentModel::Empty;
    let mut attributes = Vec::new();
    let mut attribute_wildcard: Option<AttributeWildcard> = None;
    let mut base_type: Option<(Option<String>, String)> = None;
    let mut derived_by_extension: Option<bool> = None;
    let mut group_ref: Option<(Option<String>, String)> = None;
    let mut attribute_group_refs: Vec<(Option<String>, String)> = Vec::new();

    for child in doc.children(node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");

            if !is_xs {
                continue;
            }

            match child_elem.name.local_name.as_ref() {
                "sequence" => {
                    let min_occ = child_elem
                        .get_attribute("minOccurs")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    let max_occ = match child_elem.get_attribute("maxOccurs") {
                        Some("unbounded") => MaxOccurs::Unbounded,
                        Some(s) => MaxOccurs::Bounded(s.parse().unwrap_or(1)),
                        None => MaxOccurs::Bounded(1),
                    };
                    content = ContentModel::Sequence(
                        parse_particles(
                            doc,
                            child,
                            local_elem_ns,
                            schema_target_ns,
                            attribute_groups,
                            model_groups,
                            block_default_ext,
                            block_default_rst,
                        )?,
                        min_occ,
                        max_occ,
                    );
                }
                "choice" => {
                    let min_occ = child_elem
                        .get_attribute("minOccurs")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    let max_occ = match child_elem.get_attribute("maxOccurs") {
                        Some("unbounded") => MaxOccurs::Unbounded,
                        Some(s) => MaxOccurs::Bounded(s.parse().unwrap_or(1)),
                        None => MaxOccurs::Bounded(1),
                    };
                    content = ContentModel::Choice(
                        parse_particles(
                            doc,
                            child,
                            local_elem_ns,
                            schema_target_ns,
                            attribute_groups,
                            model_groups,
                            block_default_ext,
                            block_default_rst,
                        )?,
                        min_occ,
                        max_occ,
                    );
                }
                "all" => {
                    content = ContentModel::All(parse_particles(
                        doc,
                        child,
                        local_elem_ns,
                        schema_target_ns,
                        attribute_groups,
                        model_groups,
                        block_default_ext,
                        block_default_rst,
                    )?);
                }
                "group" => {
                    // Resolve xs:group ref to set content model
                    if let Some(ref_name) = child_elem.get_attribute("ref") {
                        let local_name = strip_prefix(ref_name);
                        let ref_ns = resolve_ref_namespace(doc, child, ref_name, schema_target_ns);
                        let key = (ref_ns, local_name.to_string());
                        // Track the unresolved reference for potential re-resolution after redefine
                        group_ref = Some(key.clone());
                        if let Some(mg) = model_groups.get(&key) {
                            content = mg.content.clone();
                        }
                    }
                }
                "attribute" => {
                    attributes.push(parse_attribute_decl(doc, child)?);
                }
                "anyAttribute" => {
                    let new_wc = parse_any_attribute(child_elem, target_ns);
                    // Intersect with existing wildcard if present
                    attribute_wildcard = match attribute_wildcard {
                        Some(existing_wc) => existing_wc.intersect(&new_wc),
                        None => Some(new_wc),
                    };
                }
                "attributeGroup" => {
                    // Resolve attributeGroup ref
                    if let Some(ref_name) = child_elem.get_attribute("ref") {
                        let local_name = strip_prefix(ref_name);
                        let ref_ns = resolve_ref_namespace(doc, child, ref_name, target_ns);
                        let key = (ref_ns, local_name.to_string());
                        // Track the unresolved reference for potential re-resolution after redefine
                        attribute_group_refs.push(key.clone());
                        if let Some(ag) = attribute_groups.get(&key) {
                            attributes.extend(ag.attributes.iter().cloned());
                            // Merge wildcard: intersect if both have one
                            if let Some(ref ag_wc) = ag.wildcard {
                                attribute_wildcard = match attribute_wildcard {
                                    Some(existing_wc) => existing_wc.intersect(ag_wc),
                                    None => Some(ag_wc.clone()),
                                };
                            }
                        }
                    }
                }
                "simpleContent" | "complexContent" => {
                    // Handle extension/restriction
                    for grandchild in doc.children(child) {
                        if let Some(NodeKind::Element(gc_elem)) = doc.node_kind(grandchild) {
                            let gc_is_xs = gc_elem.name.namespace_uri.as_deref()
                                == Some(XS_NAMESPACE)
                                || gc_elem.name.prefix.as_deref() == Some("xs")
                                || gc_elem.name.prefix.as_deref() == Some("xsd");
                            if !gc_is_xs {
                                continue;
                            }
                            match gc_elem.name.local_name.as_ref() {
                                "extension" | "restriction" => {
                                    let is_extension = gc_elem.name.local_name == "extension";
                                    derived_by_extension = Some(is_extension);
                                    if let Some(base) = gc_elem.get_attribute("base") {
                                        // Track base type for block checking
                                        // Type references always resolve against the schema target namespace
                                        let base_ref = resolve_type_name(base, schema_target_ns);
                                        if let TypeRef::Named(ns, ln) = &base_ref {
                                            base_type = Some((ns.clone(), ln.clone()));
                                        }
                                        content = ContentModel::SimpleContent(Box::new(base_ref));
                                    }
                                    // Parse attributes and anyAttribute within extension/restriction
                                    let mut local_wildcard: Option<AttributeWildcard> = None;
                                    for gc_child in doc.children(grandchild) {
                                        if let Some(NodeKind::Element(gc_child_elem)) =
                                            doc.node_kind(gc_child)
                                        {
                                            let gcce_is_xs =
                                                gc_child_elem.name.namespace_uri.as_deref()
                                                    == Some(XS_NAMESPACE)
                                                    || gc_child_elem.name.prefix.as_deref()
                                                        == Some("xs")
                                                    || gc_child_elem.name.prefix.as_deref()
                                                        == Some("xsd");
                                            if !gcce_is_xs {
                                                continue;
                                            }
                                            match gc_child_elem.name.local_name.as_ref() {
                                                "attribute" => {
                                                    attributes
                                                        .push(parse_attribute_decl(doc, gc_child)?);
                                                }
                                                "anyAttribute" => {
                                                    local_wildcard = Some(parse_any_attribute(
                                                        gc_child_elem,
                                                        target_ns,
                                                    ));
                                                }
                                                "sequence" => {
                                                    let min_occ = gc_child_elem
                                                        .get_attribute("minOccurs")
                                                        .and_then(|s| s.parse().ok())
                                                        .unwrap_or(1);
                                                    let max_occ = match gc_child_elem
                                                        .get_attribute("maxOccurs")
                                                    {
                                                        Some("unbounded") => MaxOccurs::Unbounded,
                                                        Some(s) => MaxOccurs::Bounded(
                                                            s.parse().unwrap_or(1),
                                                        ),
                                                        None => MaxOccurs::Bounded(1),
                                                    };
                                                    content = ContentModel::Sequence(
                                                        parse_particles(
                                                            doc,
                                                            gc_child,
                                                            local_elem_ns,
                                                            schema_target_ns,
                                                            attribute_groups,
                                                            model_groups,
                                                            block_default_ext,
                                                            block_default_rst,
                                                        )?,
                                                        min_occ,
                                                        max_occ,
                                                    );
                                                }
                                                "choice" => {
                                                    let min_occ = gc_child_elem
                                                        .get_attribute("minOccurs")
                                                        .and_then(|s| s.parse().ok())
                                                        .unwrap_or(1);
                                                    let max_occ = match gc_child_elem
                                                        .get_attribute("maxOccurs")
                                                    {
                                                        Some("unbounded") => MaxOccurs::Unbounded,
                                                        Some(s) => MaxOccurs::Bounded(
                                                            s.parse().unwrap_or(1),
                                                        ),
                                                        None => MaxOccurs::Bounded(1),
                                                    };
                                                    content = ContentModel::Choice(
                                                        parse_particles(
                                                            doc,
                                                            gc_child,
                                                            local_elem_ns,
                                                            schema_target_ns,
                                                            attribute_groups,
                                                            model_groups,
                                                            block_default_ext,
                                                            block_default_rst,
                                                        )?,
                                                        min_occ,
                                                        max_occ,
                                                    );
                                                }
                                                "attributeGroup" => {
                                                    if let Some(ref_name) =
                                                        gc_child_elem.get_attribute("ref")
                                                    {
                                                        let ag_local = strip_prefix(ref_name);
                                                        let ag_ns = resolve_ref_namespace(
                                                            doc, gc_child, ref_name, target_ns,
                                                        );
                                                        let key = (ag_ns, ag_local.to_string());
                                                        if let Some(ag) = attribute_groups.get(&key)
                                                        {
                                                            attributes.extend(
                                                                ag.attributes.iter().cloned(),
                                                            );
                                                            if let Some(ref ag_wc) = ag.wildcard {
                                                                local_wildcard =
                                                                    match local_wildcard {
                                                                        Some(existing_wc) => {
                                                                            existing_wc
                                                                                .intersect(ag_wc)
                                                                        }
                                                                        None => Some(ag_wc.clone()),
                                                                    };
                                                            }
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    // For extension: wildcard is union of base + derived
                                    // For restriction: wildcard from derived overrides base
                                    // (Base wildcard will be resolved during validation
                                    //  since we don't have access to all types yet)
                                    if is_extension {
                                        // Store the derived wildcard; base wildcard merge
                                        // happens during validation via type resolution
                                        attribute_wildcard = local_wildcard;
                                    } else {
                                        // Restriction: derived wildcard (if present) replaces base
                                        // If no wildcard in restriction, it means no wildcard
                                        attribute_wildcard = local_wildcard;
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
        attribute_wildcard,
        base_type,
        derived_by_extension,
        block_extension: block_ext,
        block_restriction: block_rst,
        group_ref,
        attribute_group_refs,
    }))
}

/// Parse an `xs:anyAttribute` element into an `AttributeWildcard`.
///
/// Handles `processContents` (skip/lax/strict) and `namespace` constraints
/// (`##any`, `##other`, `##local`, `##targetNamespace`, or space-separated URI list).
pub(super) fn parse_any_attribute(
    elem: &crate::dom::Element,
    target_ns: &Option<String>,
) -> AttributeWildcard {
    let process_contents = match elem.get_attribute("processContents") {
        Some("skip") => ProcessContents::Skip,
        Some("lax") => ProcessContents::Lax,
        Some("strict") => ProcessContents::Strict,
        _ => ProcessContents::Strict, // default per spec
    };

    let namespace_constraint = match elem.get_attribute("namespace") {
        None | Some("##any") => NamespaceConstraint::Any,
        Some("##other") => NamespaceConstraint::Other(target_ns.clone()),
        Some("##local") => NamespaceConstraint::Local,
        Some("##targetNamespace") => NamespaceConstraint::TargetNamespace(target_ns.clone()),
        Some(ns_list) => {
            // Space-separated list of URIs, possibly with ##local and ##targetNamespace
            let mut uris = Vec::new();
            for part in ns_list.split_whitespace() {
                match part {
                    "##local" => uris.push("##local".to_string()),
                    "##targetNamespace" => {
                        if let Some(tns) = target_ns {
                            uris.push(tns.clone());
                        }
                    }
                    uri => uris.push(uri.to_string()),
                }
            }
            NamespaceConstraint::List(uris)
        }
    };

    AttributeWildcard {
        namespace_constraint,
        process_contents,
    }
}

/// Parse a top-level `<xs:attributeGroup>` definition.
///
/// Collects attribute declarations, nested attributeGroup references, and
/// an optional `anyAttribute` wildcard.
pub(super) fn parse_attribute_group_def(
    doc: &Document,
    node: NodeId,
    target_ns: &Option<String>,
    global_attributes: &HashMap<(Option<String>, String), AttributeDecl>,
    attribute_groups: &HashMap<(Option<String>, String), AttributeGroupDef>,
) -> XmlResult<AttributeGroupDef> {
    let mut attributes = Vec::new();
    let mut wildcard: Option<AttributeWildcard> = None;

    for child in doc.children(node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }
            match child_elem.name.local_name.as_ref() {
                "attribute" => {
                    // Check if this is an attribute reference
                    if let Some(ref_name) = child_elem.get_attribute("ref") {
                        let local_name = strip_prefix(ref_name);
                        let ref_ns = resolve_ref_namespace(doc, child, ref_name, target_ns);
                        let key = (ref_ns, local_name.to_string());
                        if let Some(global_attr) = global_attributes.get(&key) {
                            // Use the global attribute declaration but allow
                            // local overrides for use/required
                            let mut attr = global_attr.clone();
                            if child_elem.get_attribute("use") == Some("required") {
                                attr.required = true;
                            } else if child_elem.get_attribute("use") == Some("prohibited") {
                                attr.prohibited = true;
                            }
                            attributes.push(attr);
                        } else {
                            // Global attribute not found; create a placeholder
                            let required = child_elem.get_attribute("use") == Some("required");
                            let prohibited = child_elem.get_attribute("use") == Some("prohibited");
                            attributes.push(AttributeDecl {
                                name: local_name.to_string(),
                                type_ref: TypeRef::BuiltIn(BuiltInType::String),
                                required,
                                default: None,
                                prohibited,
                            });
                        }
                    } else {
                        attributes.push(parse_attribute_decl(doc, child)?);
                    }
                }
                "attributeGroup" => {
                    // Resolve attributeGroup ref (used in redefine self-references)
                    if let Some(ref_name) = child_elem.get_attribute("ref") {
                        let local_name = strip_prefix(ref_name);
                        let ref_ns = resolve_ref_namespace(doc, child, ref_name, target_ns);
                        let key = (ref_ns, local_name.to_string());
                        if let Some(ag) = attribute_groups.get(&key) {
                            attributes.extend(ag.attributes.iter().cloned());
                            if let Some(ref ag_wc) = ag.wildcard {
                                wildcard = match wildcard {
                                    Some(existing_wc) => existing_wc.intersect(ag_wc),
                                    None => Some(ag_wc.clone()),
                                };
                            }
                        }
                    }
                }
                "anyAttribute" => {
                    wildcard = Some(parse_any_attribute(child_elem, target_ns));
                }
                _ => {}
            }
        }
    }

    Ok(AttributeGroupDef {
        attributes,
        wildcard,
    })
}

/// Parse a top-level `<xs:group>` definition (model group definition).
///
/// A model group contains exactly one compositor child: `sequence`, `choice`, or `all`.
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_model_group_def(
    doc: &Document,
    node: NodeId,
    local_elem_ns: &Option<String>,
    schema_target_ns: &Option<String>,
    attribute_groups: &HashMap<(Option<String>, String), AttributeGroupDef>,
    model_groups: &HashMap<(Option<String>, String), ModelGroupDef>,
    block_default_ext: bool,
    block_default_rst: bool,
) -> XmlResult<ModelGroupDef> {
    // A model group definition contains exactly one compositor child: sequence, choice, or all
    let mut content = ContentModel::Empty;

    for child in doc.children(node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }
            match child_elem.name.local_name.as_ref() {
                "sequence" => {
                    let min_occ = child_elem
                        .get_attribute("minOccurs")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    let max_occ = match child_elem.get_attribute("maxOccurs") {
                        Some("unbounded") => MaxOccurs::Unbounded,
                        Some(s) => MaxOccurs::Bounded(s.parse().unwrap_or(1)),
                        None => MaxOccurs::Bounded(1),
                    };
                    content = ContentModel::Sequence(
                        parse_particles(
                            doc,
                            child,
                            local_elem_ns,
                            schema_target_ns,
                            attribute_groups,
                            model_groups,
                            block_default_ext,
                            block_default_rst,
                        )?,
                        min_occ,
                        max_occ,
                    );
                }
                "choice" => {
                    let min_occ = child_elem
                        .get_attribute("minOccurs")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    let max_occ = match child_elem.get_attribute("maxOccurs") {
                        Some("unbounded") => MaxOccurs::Unbounded,
                        Some(s) => MaxOccurs::Bounded(s.parse().unwrap_or(1)),
                        None => MaxOccurs::Bounded(1),
                    };
                    content = ContentModel::Choice(
                        parse_particles(
                            doc,
                            child,
                            local_elem_ns,
                            schema_target_ns,
                            attribute_groups,
                            model_groups,
                            block_default_ext,
                            block_default_rst,
                        )?,
                        min_occ,
                        max_occ,
                    );
                }
                "all" => {
                    content = ContentModel::All(parse_particles(
                        doc,
                        child,
                        local_elem_ns,
                        schema_target_ns,
                        attribute_groups,
                        model_groups,
                        block_default_ext,
                        block_default_rst,
                    )?);
                }
                _ => {}
            }
        }
    }

    Ok(ModelGroupDef { content })
}

/// Strip namespace prefix from a QName (e.g., `"xs:string"` → `"string"`).
pub(super) fn strip_prefix(qname: &str) -> &str {
    match qname.find(':') {
        Some(pos) => &qname[pos + 1..],
        None => qname,
    }
}

/// Resolve the namespace URI for a QName `ref=` attribute at an XSD node.
///
/// For prefixed refs (e.g. `"b:foo"`) the prefix is resolved against the
/// in-scope namespace declarations on the `ref`-bearing node — typically the
/// foreign namespace introduced via `xs:import`. For unprefixed refs the
/// fallback namespace (usually the schema's `targetNamespace`) is returned.
///
/// This is the central namespace-resolution point for every `ref=` site in
/// the schema parser: `xs:element ref`, `xs:attribute ref`, `xs:group ref`,
/// and `xs:attributeGroup ref`. Keeping them all routed through this helper
/// guarantees they stay in sync.
pub(super) fn resolve_ref_namespace(
    doc: &Document,
    node: NodeId,
    ref_name: &str,
    fallback_ns: &Option<String>,
) -> Option<String> {
    match ref_name.find(':') {
        Some(colon_idx) => {
            let prefix = &ref_name[..colon_idx];
            build_resolver_for_node(doc, node)
                .resolve(prefix)
                .map(|uri| uri.to_string())
        }
        None => fallback_ns.clone(),
    }
}

/// Parse the children of a compositor (`sequence`, `choice`, `all`) into a
/// vector of `Particle` values.
///
/// Handles nested `element`, `sequence`, `choice`, `group` references, and `any` wildcards.
#[allow(clippy::too_many_arguments)]
fn parse_particles(
    doc: &Document,
    node: NodeId,
    local_elem_ns: &Option<String>,
    schema_target_ns: &Option<String>,
    attribute_groups: &HashMap<(Option<String>, String), AttributeGroupDef>,
    model_groups: &HashMap<(Option<String>, String), ModelGroupDef>,
    block_default_ext: bool,
    block_default_rst: bool,
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

            match child_elem.name.local_name.as_ref() {
                "element" => {
                    // Check if this is an element reference (<element ref="..."/>)
                    if let Some(ref_name) = child_elem.get_attribute("ref") {
                        let local_name = strip_prefix(ref_name);
                        // Prefixed refs resolve via the in-scope namespace
                        // declarations on the `<xs:element>` node (typically
                        // a foreign namespace introduced via xs:import);
                        // unprefixed refs fall back to the schema's target
                        // namespace. Routed through the same helper as every
                        // other `ref=` site in this module.
                        let ref_ns = resolve_ref_namespace(doc, child, ref_name, schema_target_ns);
                        particles.push(Particle {
                            kind: ParticleKind::Element(ElementDecl {
                                name: local_name.to_string(),
                                namespace: ref_ns,
                                type_ref: TypeRef::BuiltIn(BuiltInType::AnyType),
                                min_occurs,
                                max_occurs,
                                nillable: false,
                                block_extension: block_default_ext,
                                block_restriction: block_default_rst,
                                is_ref: true,
                                substitution_group: None,
                                is_abstract: false,
                                fixed: None,
                                identity_constraints: Vec::new(),
                            }),
                            min_occurs,
                            max_occurs,
                        });
                    } else {
                        let decl = parse_element_decl(
                            doc,
                            child,
                            local_elem_ns,
                            local_elem_ns,
                            schema_target_ns,
                            attribute_groups,
                            model_groups,
                            block_default_ext,
                            block_default_rst,
                        )?;
                        particles.push(Particle {
                            kind: ParticleKind::Element(decl),
                            min_occurs,
                            max_occurs,
                        });
                    }
                }
                "sequence" => {
                    let sub = parse_particles(
                        doc,
                        child,
                        local_elem_ns,
                        schema_target_ns,
                        attribute_groups,
                        model_groups,
                        block_default_ext,
                        block_default_rst,
                    )?;
                    particles.push(Particle {
                        kind: ParticleKind::Sequence(sub),
                        min_occurs,
                        max_occurs,
                    });
                }
                "choice" => {
                    let sub = parse_particles(
                        doc,
                        child,
                        local_elem_ns,
                        schema_target_ns,
                        attribute_groups,
                        model_groups,
                        block_default_ext,
                        block_default_rst,
                    )?;
                    particles.push(Particle {
                        kind: ParticleKind::Choice(sub),
                        min_occurs,
                        max_occurs,
                    });
                }
                "group" => {
                    // Resolve xs:group ref
                    if let Some(ref_name) = child_elem.get_attribute("ref") {
                        let local_name = strip_prefix(ref_name);
                        let ref_ns = resolve_ref_namespace(doc, child, ref_name, schema_target_ns);
                        let key = (ref_ns, local_name.to_string());
                        if let Some(mg) = model_groups.get(&key) {
                            // Inline the model group's content as a particle
                            match &mg.content {
                                ContentModel::Sequence(ps, _, _) => {
                                    particles.push(Particle {
                                        kind: ParticleKind::Sequence(ps.clone()),
                                        min_occurs,
                                        max_occurs,
                                    });
                                }
                                ContentModel::Choice(ps, _, _) => {
                                    particles.push(Particle {
                                        kind: ParticleKind::Choice(ps.clone()),
                                        min_occurs,
                                        max_occurs,
                                    });
                                }
                                ContentModel::All(ps) => {
                                    // All groups are wrapped as a sequence for inlining
                                    particles.push(Particle {
                                        kind: ParticleKind::Sequence(ps.clone()),
                                        min_occurs,
                                        max_occurs,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "any" => {
                    // Parse xs:any element wildcard — reuse same namespace/processContents
                    // parsing logic as xs:anyAttribute
                    let wc = parse_any_attribute(child_elem, schema_target_ns);
                    particles.push(Particle {
                        kind: ParticleKind::Any {
                            namespace_constraint: wc.namespace_constraint,
                            process_contents: wc.process_contents,
                        },
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

/// Parse an `<xs:attribute>` declaration into an `AttributeDecl`.
///
/// Handles both `name` and `ref` attributes, inline `<xs:simpleType>` children,
/// `use` (required/prohibited), and `default`.
fn parse_attribute_decl(doc: &Document, node: NodeId) -> XmlResult<AttributeDecl> {
    let elem = doc
        .element(node)
        .ok_or_else(|| XmlError::validation("Expected element node for attribute declaration"))?;

    // Handle <attribute ref="..."/> — create a placeholder decl with the ref name
    let name = if let Some(n) = elem.get_attribute("name") {
        n.to_string()
    } else if let Some(ref_name) = elem.get_attribute("ref") {
        strip_prefix(ref_name).to_string()
    } else {
        return Err(XmlError::validation(
            "Attribute declaration missing 'name' or 'ref' attribute",
        ));
    };

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
    let prohibited = elem.get_attribute("use") == Some("prohibited");
    let default = elem.get_attribute("default").map(|s| s.to_string());

    Ok(AttributeDecl {
        name,
        type_ref,
        required,
        default,
        prohibited,
    })
}

/// Parse an `<xs:simpleType>` declaration into a `TypeDef::Simple`.
///
/// Handles:
/// - `<xs:list itemType="...">` — sets `is_list` and `item_type`
/// - `<xs:restriction base="...">` — resolves built-in or user-defined base types
/// - Inline `<xs:simpleType>` base within restriction (no `base` attribute)
/// - All facets: length, minLength, maxLength, pattern, enumeration, min/maxInclusive,
///   min/maxExclusive, totalDigits, fractionDigits, whiteSpace
/// - Derived list types: NMTOKENS, IDREFS, ENTITIES
pub(super) fn parse_simple_type(doc: &Document, node: NodeId) -> XmlResult<TypeDef> {
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
                    if prefix == "xs" || prefix == "xsd" {
                        // Explicitly XSD-prefixed: always a built-in type
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
                    } else if prefix.is_empty() {
                        // Unprefixed: try built-in first, fall back to user-defined
                        if matches!(local, "NMTOKENS" | "IDREFS" | "ENTITIES") {
                            is_list = true;
                            item_type = match local {
                                "NMTOKENS" => Some(BuiltInType::NMTOKEN),
                                "IDREFS" => Some(BuiltInType::IDREF),
                                "ENTITIES" => Some(BuiltInType::ENTITY),
                                _ => None,
                            };
                            base = parse_builtin_type(local).unwrap_or(BuiltInType::String);
                        } else if let Some(bt) = parse_builtin_type(local) {
                            base = bt;
                        } else {
                            // Not a built-in type — user-defined type
                            base_type_local = Some(local.to_string());
                        }
                    } else {
                        // Non-builtin base type — store for later resolution
                        base_type_local = Some(local.to_string());
                    }
                } else {
                    // No base attribute — check for inline <simpleType> child as base type
                    for inner_child in doc.children(child) {
                        if let Some(NodeKind::Element(inner_elem)) = doc.node_kind(inner_child) {
                            let inner_is_xs = inner_elem.name.namespace_uri.as_deref()
                                == Some(XS_NAMESPACE)
                                || inner_elem.name.prefix.as_deref() == Some("xs")
                                || inner_elem.name.prefix.as_deref() == Some("xsd");
                            if inner_is_xs && inner_elem.name.local_name == "simpleType" {
                                if let Ok(TypeDef::Simple(inner_st)) =
                                    parse_simple_type(doc, inner_child)
                                {
                                    base = inner_st.base;
                                    is_list = inner_st.is_list;
                                    item_type = inner_st.item_type;
                                    item_type_local = inner_st._item_type_local.clone();
                                }
                                break;
                            }
                        }
                    }
                }

                // Parse facets
                for facet_child in doc.children(child) {
                    if let Some(NodeKind::Element(facet_elem)) = doc.node_kind(facet_child) {
                        let value = facet_elem.get_attribute("value").unwrap_or("").to_string();

                        match facet_elem.name.local_name.as_ref() {
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
