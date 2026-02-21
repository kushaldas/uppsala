//! XSD type definitions and core data structures.
//!
//! Contains all the structs and enums that represent the XSD type system:
//! element declarations, type definitions (simple and complex), content models,
//! particles, facets, wildcards, identity constraints, and built-in types.
//!
//! These types are used throughout the XSD validation pipeline — from schema
//! parsing (builder/parser) through validation.

use std::collections::HashMap;

use super::wildcard::{
    intersect_namespace_constraints, stricter_process_contents, union_namespace_constraints,
};

/// An XSD validator that holds a compiled schema and validates documents against it.
///
/// Built from a parsed XSD schema document via [`XsdValidator::from_schema`] or
/// [`XsdValidator::from_schema_with_base_path`]. Holds all top-level declarations
/// (elements, types, attributes, groups) and provides the [`validate`](XsdValidator::validate)
/// method to check instance documents.
pub struct XsdValidator {
    /// Top-level element declarations: (namespace_uri, local_name) -> ElementDecl
    pub(super) elements: HashMap<(Option<String>, String), ElementDecl>,
    /// Named type definitions: (namespace_uri, local_name) -> TypeDef
    pub(super) types: HashMap<(Option<String>, String), TypeDef>,
    /// Global attribute declarations: (namespace_uri, local_name) -> AttributeDecl
    pub(super) global_attributes: HashMap<(Option<String>, String), AttributeDecl>,
    /// Attribute group definitions: (namespace_uri, local_name) -> AttributeGroupDef
    pub(super) attribute_groups: HashMap<(Option<String>, String), AttributeGroupDef>,
    /// Model group definitions: (namespace_uri, local_name) -> ModelGroupDef
    pub(super) model_groups: HashMap<(Option<String>, String), ModelGroupDef>,
    /// Target namespace of the schema.
    pub(super) target_namespace: Option<String>,
    /// Schema-level blockDefault for extension.
    pub(super) block_default_extension: bool,
    /// Schema-level blockDefault for restriction.
    pub(super) block_default_restriction: bool,
    /// Whether to enforce length/minLength/maxLength facets on QName and NOTATION types.
    /// NIST tests expect these to be ignored (Bug #4009), MS tests expect enforcement.
    /// Default: true (enforce).
    pub(super) enforce_qname_length_facets: bool,
    /// Substitution group membership: head_key -> vec of member keys (transitive).
    /// Each key is (namespace, local_name).
    pub(super) substitution_groups:
        HashMap<(Option<String>, String), Vec<(Option<String>, String)>>,
}

/// An element declaration parsed from the schema.
///
/// Represents either a top-level (global) element or a local element within
/// a content model. Contains the element's name, namespace, type reference,
/// occurrence constraints, and optional features like nillability and
/// substitution groups.
#[derive(Debug, Clone)]
pub(crate) struct ElementDecl {
    pub(super) name: String,
    pub(super) namespace: Option<String>,
    pub(super) type_ref: TypeRef,
    /// Minimum number of occurrences (parsed for spec completeness; occurrence
    /// checking is done on the `Particle` wrapper, not here).
    #[allow(dead_code)]
    pub(super) min_occurs: u64,
    /// Maximum number of occurrences (parsed for spec completeness; occurrence
    /// checking is done on the `Particle` wrapper, not here).
    #[allow(dead_code)]
    pub(super) max_occurs: MaxOccurs,
    pub(super) nillable: bool,
    /// Block constraint on this element (blocks xsi:type substitution).
    pub(super) block_extension: bool,
    pub(super) block_restriction: bool,
    /// True if this element was created from an `<element ref="..."/>` reference.
    /// At validation time, the actual type is resolved from the global element map.
    pub(super) is_ref: bool,
    /// The substitution group head element: (namespace, local_name).
    /// Set when an element declares substitutionGroup="...".
    pub(super) substitution_group: Option<(Option<String>, String)>,
    /// Whether this element is abstract (cannot appear directly in instances).
    pub(super) is_abstract: bool,
    /// Identity constraints declared on this element.
    pub(super) identity_constraints: Vec<IdentityConstraint>,
}

/// An identity constraint (xs:key, xs:unique, xs:keyref).
///
/// Identity constraints enforce uniqueness and referential integrity within
/// a scope defined by the selector XPath expression. Fields identify the
/// values that make up the constraint key.
#[derive(Debug, Clone)]
pub(crate) struct IdentityConstraint {
    /// Name of the constraint.
    pub(super) name: String,
    /// Kind: Key, Unique, or KeyRef.
    pub(super) kind: IdentityConstraintKind,
    /// XPath selector expression (restricted subset).
    pub(super) selector: String,
    /// XPath field expressions (restricted subset). One or more.
    pub(super) fields: Vec<String>,
    /// For keyref: the name of the referred key/unique constraint.
    pub(super) refer: Option<String>,
}

/// The kind of identity constraint.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IdentityConstraintKind {
    Key,
    Unique,
    KeyRef,
}

/// Reference to a type - either a named type or an anonymous inline type.
///
/// Used in element declarations and simple content to point to the type
/// that governs validation. Can be a named reference (resolved at validation
/// time), an inline anonymous type, or a direct built-in type reference.
#[derive(Debug, Clone)]
pub(crate) enum TypeRef {
    Named(Option<String>, String), // (namespace, local_name)
    Inline(Box<TypeDef>),
    BuiltIn(BuiltInType),
}

/// A type definition (complex or simple).
#[derive(Debug, Clone)]
pub(crate) enum TypeDef {
    Complex(ComplexTypeDef),
    Simple(SimpleTypeDef),
}
/// Namespace constraint for attribute/element wildcards.
///
/// Defines which namespaces are allowed or disallowed by a wildcard
/// (`xs:any` or `xs:anyAttribute`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NamespaceConstraint {
    /// ##any — any namespace
    Any,
    /// ##other — any namespace other than the target namespace
    Other(Option<String>), // holds the target namespace to exclude
    /// ##local — no namespace (unqualified attributes only)
    Local,
    /// ##targetNamespace — only the target namespace
    TargetNamespace(Option<String>), // holds the target namespace
    /// Explicit list of namespace URIs
    List(Vec<String>),
}

/// processContents for wildcards.
///
/// Controls how content matched by a wildcard is validated:
/// - `Strict`: must have a matching declaration; validated against it
/// - `Lax`: validated if a declaration exists; otherwise skipped
/// - `Skip`: no validation performed
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProcessContents {
    Skip,
    Lax,
    Strict,
}

/// An attribute wildcard (xs:anyAttribute).
///
/// Allows attributes from specified namespaces with configurable validation
/// strictness. Used in complex type definitions to allow extensibility.
#[derive(Debug, Clone)]
pub(crate) struct AttributeWildcard {
    pub(super) namespace_constraint: NamespaceConstraint,
    pub(super) process_contents: ProcessContents,
}

impl AttributeWildcard {
    /// Check if an attribute with the given namespace is allowed by this wildcard.
    pub(super) fn allows_namespace(&self, attr_ns: &Option<String>) -> bool {
        match &self.namespace_constraint {
            NamespaceConstraint::Any => true,
            NamespaceConstraint::Other(target_ns) => {
                // ##other: allow any namespace except the target namespace AND except no-namespace
                match attr_ns {
                    None => false, // unqualified attributes not allowed by ##other
                    Some(ns) => {
                        match target_ns {
                            Some(tns) => ns != tns,
                            None => true, // no target namespace, so all namespaced attrs OK
                        }
                    }
                }
            }
            NamespaceConstraint::Local => {
                // ##local: only no-namespace (unqualified) attributes
                attr_ns.is_none()
            }
            NamespaceConstraint::TargetNamespace(target_ns) => {
                // ##targetNamespace: only attributes in the target namespace
                attr_ns == target_ns
            }
            NamespaceConstraint::List(uris) => match attr_ns {
                None => uris.iter().any(|u| u == "##local"),
                Some(ns) => uris.iter().any(|u| u == ns),
            },
        }
    }

    /// Compute the intersection of two wildcards (used when merging attribute groups).
    ///
    /// Returns `None` if the intersection is empty (no namespace allowed by both).
    /// The `processContents` of the result is the stricter of the two inputs.
    pub(super) fn intersect(&self, other: &AttributeWildcard) -> Option<AttributeWildcard> {
        let ns = intersect_namespace_constraints(
            &self.namespace_constraint,
            &other.namespace_constraint,
        )?;
        // processContents: use stricter of the two (strict > lax > skip)
        let pc = stricter_process_contents(&self.process_contents, &other.process_contents);
        Some(AttributeWildcard {
            namespace_constraint: ns,
            process_contents: pc,
        })
    }

    /// Compute the union of two wildcards (used for extension).
    ///
    /// The `processContents` of the result uses the derived type's value.
    pub(super) fn union(&self, other: &AttributeWildcard) -> AttributeWildcard {
        let ns =
            union_namespace_constraints(&self.namespace_constraint, &other.namespace_constraint);
        // processContents: use the derived type's processContents
        AttributeWildcard {
            namespace_constraint: ns,
            process_contents: other.process_contents.clone(),
        }
    }
}

/// An attribute group definition (for resolving attributeGroup refs).
///
/// Groups a set of attribute declarations and an optional attribute wildcard
/// that can be referenced by name from complex type definitions.
#[derive(Debug, Clone)]
pub(crate) struct AttributeGroupDef {
    pub(super) attributes: Vec<AttributeDecl>,
    pub(super) wildcard: Option<AttributeWildcard>,
}

/// A model group definition (for resolving xs:group refs).
///
/// Contains a content model (sequence, choice, or all) that can be
/// referenced by name from complex type definitions.
#[derive(Debug, Clone)]
pub(crate) struct ModelGroupDef {
    /// The content model compositor (sequence, choice, or all) with its particles.
    pub(super) content: ContentModel,
}

/// A complex type definition.
///
/// Represents an XSD complex type with its content model, attributes,
/// derivation information, and block constraints. Complex types can
/// contain element children, mixed content, or simple content.
#[derive(Debug, Clone)]
pub(crate) struct ComplexTypeDef {
    pub(super) name: Option<String>,
    pub(super) content: ContentModel,
    pub(super) attributes: Vec<AttributeDecl>,
    pub(super) mixed: bool,
    pub(super) attribute_wildcard: Option<AttributeWildcard>,
    /// Base type reference (namespace, local_name) if derived.
    pub(super) base_type: Option<(Option<String>, String)>,
    /// Whether this type was derived by extension (true) or restriction (false).
    pub(super) derived_by_extension: Option<bool>,
    /// Block constraint: blocks xsi:type substitution by these derivation methods.
    pub(super) block_extension: bool,
    pub(super) block_restriction: bool,
    /// Unresolved model group reference (namespace, local_name) from xs:group ref.
    /// Used to re-resolve after xs:redefine updates the group definition.
    pub(super) group_ref: Option<(Option<String>, String)>,
    /// Unresolved attribute group references (namespace, local_name) from xs:attributeGroup ref.
    /// Used to re-resolve after xs:redefine updates the attribute group definition.
    pub(super) attribute_group_refs: Vec<(Option<String>, String)>,
}

/// Content model for a complex type.
///
/// Defines how child elements and text content are structured within a
/// complex type. Each variant represents a different XSD compositor or
/// content kind.
#[derive(Debug, Clone)]
pub(crate) enum ContentModel {
    Empty,
    Sequence(Vec<Particle>, u64, MaxOccurs), // particles, min_occurs, max_occurs
    Choice(Vec<Particle>, u64, MaxOccurs),   // particles, min_occurs, max_occurs
    All(Vec<Particle>),
    SimpleContent(Box<TypeRef>),
    /// Any content (xs:anyType). Placeholder for future xs:anyType handling.
    #[allow(dead_code)]
    Any,
}

/// A particle in a content model (element ref, group, etc.).
///
/// Wraps a [`ParticleKind`] with occurrence constraints (minOccurs/maxOccurs).
#[derive(Debug, Clone)]
pub(crate) struct Particle {
    pub(super) kind: ParticleKind,
    pub(super) min_occurs: u64,
    pub(super) max_occurs: MaxOccurs,
}

/// The kind of content a particle represents.
#[derive(Debug, Clone)]
pub(crate) enum ParticleKind {
    Element(ElementDecl),
    Sequence(Vec<Particle>),
    Choice(Vec<Particle>),
    /// An xs:any element wildcard particle.
    Any {
        namespace_constraint: NamespaceConstraint,
        process_contents: ProcessContents,
    },
}

/// Maximum occurrence count for elements and model groups.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MaxOccurs {
    Bounded(u64),
    Unbounded,
}

/// An attribute declaration.
///
/// Represents a single attribute with its name, type, and usage information.
/// Used in complex type definitions and attribute group definitions.
#[derive(Debug, Clone)]
pub(crate) struct AttributeDecl {
    pub(super) name: String,
    pub(super) type_ref: TypeRef,
    pub(super) required: bool,
    /// Default value (parsed for spec completeness; not yet enforced during validation).
    #[allow(dead_code)]
    pub(super) default: Option<String>,
    pub(super) prohibited: bool,
}

/// A simple type definition.
///
/// Represents an XSD simple type (atomic, list, or union) with its base
/// built-in type and facet restrictions. List types additionally track
/// their item type and item-level facets.
#[derive(Debug, Clone)]
pub(crate) struct SimpleTypeDef {
    pub(super) name: Option<String>,
    pub(super) base: BuiltInType,
    pub(super) facets: Vec<Facet>,
    /// Whether this type is a list type (items separated by whitespace).
    pub(super) is_list: bool,
    /// For list types, the built-in type of each item.
    pub(super) item_type: Option<BuiltInType>,
    /// For list types, facets inherited from the item type (when item type is a user-defined simple type).
    pub(super) item_facets: Vec<Facet>,
    /// Non-builtin base type local name, for resolving list inheritance.
    pub(super) _base_type_local: Option<String>,
    /// Non-builtin item type local name, for resolving in post-processing.
    pub(super) _item_type_local: Option<String>,
}

/// Built-in XSD datatypes.
///
/// Covers all 44+ built-in types from XSD Part 2 (Datatypes), including
/// primitive types (string, boolean, decimal, float, double, etc.) and
/// derived types (integer, long, int, short, byte, etc.).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BuiltInType {
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
///
/// Each variant represents a different constraining facet from XSD Part 2.
/// Facets are applied during validation to restrict the value space of
/// a simple type.
#[derive(Debug, Clone)]
pub(crate) enum Facet {
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
    WhiteSpace(#[allow(dead_code)] WhiteSpaceHandling),
}

/// Whitespace handling mode for the whiteSpace facet.
///
/// Controls how whitespace in text content is normalized before validation:
/// - `Preserve`: no normalization
/// - `Replace`: all whitespace characters replaced with spaces
/// - `Collapse`: replace + collapse consecutive spaces + trim
#[derive(Debug, Clone)]
pub(crate) enum WhiteSpaceHandling {
    Preserve,
    Replace,
    Collapse,
}
