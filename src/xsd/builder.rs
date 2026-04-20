//! XSD schema builder — constructs an `XsdValidator` from a parsed XSD document.
//!
//! Entry points are `XsdValidator::from_schema` (simple) and
//! `from_schema_with_base_path` (supports external `schemaLocation` resolution).
//!
//! The build proceeds in multiple passes:
//!   0. Schema composition (`xs:include`, `xs:redefine`, `xs:import`)
//!   0.5. Global attribute declarations (needed by attributeGroup parsing)
//!   1. Attribute-group and model-group definitions
//!   2. All other top-level declarations (elements, complex/simple types, attributes)
//!   3. Substitution-group map construction (direct + transitive membership)
//!   4. List-type resolution passes (base-type propagation, item-type facets,
//!      inline list-type facets in elements and content-model particles)

use std::collections::HashMap;
use std::path::Path;

use crate::dom::{Document, NodeKind};
use crate::error::{XmlError, XmlResult};

use super::composition::process_schema_composition;
use super::debug_log;
use super::facet_resolution::{
    resolve_content_model_list_item_facets, resolve_inline_list_item_facets,
};
use super::parser::{
    parse_attribute_group_def, parse_complex_type, parse_element_decl, parse_model_group_def,
    parse_simple_type, resolve_type_name,
};
use super::types::{AttributeDecl, BuiltInType, Facet, TypeDef, TypeRef, XsdValidator};
use super::XS_NAMESPACE;

impl XsdValidator {
    /// Build a validator from a parsed XSD schema document.
    ///
    /// Equivalent to `from_schema_with_base_path(schema_doc, None)`.
    pub fn from_schema(schema_doc: &Document) -> XmlResult<Self> {
        Self::from_schema_with_base_path(schema_doc, None)
    }

    /// Set whether length/minLength/maxLength facets on QName and NOTATION types
    /// are enforced. Default is `true` (enforce). Set to `false` to ignore them,
    /// which matches the NIST test suite interpretation of W3C Bug #4009.
    pub fn set_enforce_qname_length_facets(&mut self, enforce: bool) {
        self.enforce_qname_length_facets = enforce;
    }

    /// Build a validator from a parsed XSD schema document, with a base path
    /// for resolving `schemaLocation` attributes in `xs:include` and `xs:redefine`.
    ///
    /// # Passes
    ///
    /// 1. **Pass 0** — schema composition: `xs:include` / `xs:redefine` / `xs:import`
    /// 2. **Pass 0.5** — global attribute declarations (needed before attributeGroup parsing)
    /// 3. **Pass 1** — attribute-group and model-group definitions
    /// 4. **Pass 2** — top-level elements, complex types, simple types, remaining attributes
    /// 5. **Substitution-group construction** — direct + transitive membership map
    /// 6. **List-type resolution** — three sub-passes to propagate `is_list`, `item_type`,
    ///    and `item_facets` through the type graph and into inline declarations
    pub fn from_schema_with_base_path(
        schema_doc: &Document,
        base_path: Option<&Path>,
    ) -> XmlResult<Self> {
        let mut validator = XsdValidator {
            elements: HashMap::new(),
            types: HashMap::new(),
            global_attributes: HashMap::new(),
            attribute_groups: HashMap::new(),
            model_groups: HashMap::new(),
            target_namespace: None,
            block_default_extension: false,
            block_default_restriction: false,
            enforce_qname_length_facets: true,
            substitution_groups: HashMap::new(),
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
            // Parse blockDefault
            if let Some(block_default) = elem.get_attribute("blockDefault") {
                for token in block_default.split_whitespace() {
                    match token {
                        "extension" => validator.block_default_extension = true,
                        "restriction" => validator.block_default_restriction = true,
                        "#all" => {
                            validator.block_default_extension = true;
                            validator.block_default_restriction = true;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Determine the effective namespace for local element declarations:
        // If elementFormDefault="qualified", local elements inherit the target namespace.
        let local_elem_ns = if element_form_qualified {
            validator.target_namespace.clone()
        } else {
            None
        };

        // Pass 0: Process xs:include and xs:redefine to merge external schema declarations
        if base_path.is_some() {
            process_schema_composition(schema_doc, schema_elem, &mut validator, base_path)?;
        }

        // Pass 0.5: Parse global attribute declarations first, since attributeGroup
        // definitions may reference them via <attribute ref="..."/>.
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");
                if !is_xs {
                    continue;
                }
                if elem.name.local_name == "attribute" {
                    if let Some(attr_elem) = schema_doc.element(child) {
                        if let Some(name) = attr_elem.get_attribute("name") {
                            let type_ref = if let Some(type_attr) = attr_elem.get_attribute("type")
                            {
                                resolve_type_name(type_attr, &validator.target_namespace)
                            } else {
                                // Check for inline simpleType child
                                let mut inline_type = None;
                                for gc in schema_doc.children(child) {
                                    if let Some(NodeKind::Element(ge)) = schema_doc.node_kind(gc) {
                                        if ge.name.local_name == "simpleType" {
                                            if let Ok(td) = parse_simple_type(schema_doc, gc) {
                                                inline_type = Some(TypeRef::Inline(Box::new(td)));
                                            }
                                        }
                                    }
                                }
                                inline_type.unwrap_or(TypeRef::BuiltIn(BuiltInType::String))
                            };
                            let required = attr_elem.get_attribute("use") == Some("required");
                            let default = attr_elem.get_attribute("default").map(|s| s.to_string());
                            let decl = AttributeDecl {
                                name: name.to_string(),
                                type_ref,
                                required,
                                default,
                                prohibited: false,
                            };
                            let key = (validator.target_namespace.clone(), name.to_string());
                            validator.global_attributes.insert(key, decl);
                        }
                    }
                }
            }
        }

        // Pass 1: Parse attribute group and model group definitions
        // (both needed by complexType parsing in Pass 2)
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");
                if !is_xs {
                    continue;
                }
                if elem.name.local_name == "attributeGroup" {
                    if let Some(ag_elem) = schema_doc.element(child) {
                        if let Some(name) = ag_elem.get_attribute("name") {
                            let ag_def = parse_attribute_group_def(
                                schema_doc,
                                child,
                                &validator.target_namespace,
                                &validator.global_attributes,
                                &validator.attribute_groups,
                            )?;
                            let key = (validator.target_namespace.clone(), name.to_string());
                            validator.attribute_groups.insert(key, ag_def);
                        }
                    }
                }
                if elem.name.local_name == "group" {
                    if let Some(g_elem) = schema_doc.element(child) {
                        if let Some(name) = g_elem.get_attribute("name") {
                            let mg_def = parse_model_group_def(
                                schema_doc,
                                child,
                                &local_elem_ns,
                                &validator.target_namespace,
                                &validator.attribute_groups,
                                &validator.model_groups,
                                validator.block_default_extension,
                                validator.block_default_restriction,
                            )?;
                            let key = (validator.target_namespace.clone(), name.to_string());
                            validator.model_groups.insert(key, mg_def);
                        }
                    }
                }
            }
        }

        // Pass 2: Process all other top-level children
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let local = &elem.name.local_name;
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");

                if !is_xs {
                    continue;
                }

                match &**local {
                    "element" => {
                        let decl = parse_element_decl(
                            schema_doc,
                            child,
                            &validator.target_namespace,
                            &local_elem_ns,
                            &validator.target_namespace,
                            &validator.attribute_groups,
                            &validator.model_groups,
                            validator.block_default_extension,
                            validator.block_default_restriction,
                        )?;
                        let key = (validator.target_namespace.clone(), decl.name.clone());
                        validator.elements.insert(key, decl);
                    }
                    "complexType" => {
                        let type_def = parse_complex_type(
                            schema_doc,
                            child,
                            &local_elem_ns,
                            &validator.target_namespace,
                            &validator.target_namespace,
                            &validator.attribute_groups,
                            &validator.model_groups,
                            validator.block_default_extension,
                            validator.block_default_restriction,
                        )?;
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
                                    prohibited: false,
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

        // Build substitution group map from element declarations.
        // First, collect direct memberships: member -> head.
        let mut direct_head: HashMap<(Option<String>, String), (Option<String>, String)> =
            HashMap::new();
        for (key, decl) in &validator.elements {
            if let Some(ref sg_head) = decl.substitution_group {
                direct_head.insert(key.clone(), sg_head.clone());
            }
        }
        // Build transitive map: for each element that is a substitution group head,
        // collect all (direct and transitive) members.
        // An element E is a member of head H if:
        //   - E.substitutionGroup == H (direct), or
        //   - E.substitutionGroup == M where M is a member of H (transitive)
        for member_key in direct_head.keys() {
            // Walk up the chain from member to find all heads
            let mut current = member_key.clone();
            let mut chain = vec![member_key.clone()];
            while let Some(head) = direct_head.get(&current) {
                // Add member_key as a member of head
                validator
                    .substitution_groups
                    .entry(head.clone())
                    .or_default()
                    .push(member_key.clone());
                current = head.clone();
                // Prevent infinite loops
                if chain.contains(&current) {
                    break;
                }
                chain.push(current.clone());
            }
        }
        // Deduplicate members
        for members in validator.substitution_groups.values_mut() {
            members.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            members.dedup();
        }
        debug_log!(
            "substitution_groups: {:?}",
            validator.substitution_groups
        );

        // Resolution pass: propagate list type info from base types to derived types.
        // Types that restrict a list type inherit is_list and item_type.
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

        // Resolution pass 3: resolve item type facets for inline list types embedded in
        // element declarations (both global elements and particles inside complex types).
        // Collect resolved item types from the types map first.
        let resolved_items: HashMap<(Option<String>, String), (BuiltInType, Vec<Facet>)> =
            validator
                .types
                .iter()
                .filter_map(|(k, td)| {
                    if let TypeDef::Simple(st) = td {
                        Some((k.clone(), (st.base.clone(), st.facets.clone())))
                    } else {
                        None
                    }
                })
                .collect();

        // Resolve inline list types in global element declarations
        for elem_decl in validator.elements.values_mut() {
            resolve_inline_list_item_facets(
                &mut elem_decl.type_ref,
                &resolved_items,
                &validator.target_namespace,
            );
        }

        // Resolve inline list types in complex type content models
        let type_keys3: Vec<_> = validator.types.keys().cloned().collect();
        for key in type_keys3 {
            if let Some(TypeDef::Complex(ct)) = validator.types.get_mut(&key) {
                resolve_content_model_list_item_facets(
                    &mut ct.content,
                    &resolved_items,
                    &validator.target_namespace,
                );
            }
        }

        Ok(validator)
    }
}
