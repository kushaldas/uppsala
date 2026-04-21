//! Schema composition — `xs:include`, `xs:redefine`, and `xs:import`.
//!
//! Handles loading external schema documents referenced by `schemaLocation`
//! attributes, merging their declarations into the main validator, and
//! performing "chameleon include" namespace fixup when a no-namespace schema
//! is included into a target-namespace schema.
//!
//! ## Composition flow
//!
//! 1. **`process_schema_composition`** iterates top-level children of the
//!    `<xs:schema>` element looking for `include`, `redefine`, and `import`.
//! 2. For each, the external schema is loaded from disk, parsed, and built
//!    into a sub-`XsdValidator` via `from_schema_with_base_path`.
//! 3. **`merge_external_declarations`** copies every declaration from the
//!    external validator into the main one.  If `chameleon` is set, all
//!    `None`-namespace keys are re-keyed to the main schema's target namespace.
//! 4. For `xs:redefine`, **`process_redefine_children`** then processes the
//!    inline redefinition elements (simpleType, complexType, group,
//!    attributeGroup) and replaces the previously-merged declarations.
//! 5. **`reresolve_types_after_redefine`** updates complex types whose
//!    group or attributeGroup references may have changed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{XmlError, XmlResult};

use super::parser::{
    parse_attribute_group_def, parse_complex_type, parse_model_group_def, parse_simple_type,
};
use super::types::{
    ContentModel, ElementDecl, Particle, ParticleKind, TypeDef, TypeRef, XsdValidator,
};
use super::XS_NAMESPACE;

/// Maximum depth of `xs:include` / `xs:redefine` / `xs:import` nesting.
///
/// Real-world schemas rarely nest more than 2-3 levels (`A` imports `B`
/// which imports `C`). 16 gives generous headroom while preventing the
/// stack overflow a circular-include chain would otherwise trigger. Used
/// in combination with a per-build visited-paths set so self-referential
/// cycles short-circuit even earlier.
pub(super) const MAX_INCLUDE_DEPTH: u8 = 16;

/// State carried through recursive schema composition to detect cycles
/// and enforce depth limits.
pub(super) struct CompositionState {
    /// Canonicalized absolute paths that have already been loaded during
    /// this `from_schema_with_base_path` call. Reloads short-circuit so
    /// `a.xsd` including `b.xsd` including `a.xsd` terminates cleanly.
    pub(super) visited: HashSet<PathBuf>,
    /// Current recursion depth. Incremented on each external schema
    /// build; errors out when it reaches [`MAX_INCLUDE_DEPTH`].
    pub(super) depth: u8,
}

impl CompositionState {
    /// Fresh state, seeded with the top-level schema's canonical path so
    /// the very first include won't try to re-load the outer document.
    pub(super) fn new(base_path: Option<&Path>) -> Self {
        let mut visited = HashSet::new();
        if let Some(p) = base_path {
            if let Ok(c) = p.canonicalize() {
                visited.insert(c);
            }
        }
        CompositionState { visited, depth: 0 }
    }
}

/// Resolve a `schemaLocation` attribute to a filesystem path, applying
/// F-10 containment, F-11 cycle detection, and the F-12 / TOCTOU fix in
/// a single canonicalize step.
///
/// Returns:
/// * `Ok(Some(canonical))` — load this path. The returned `PathBuf` has
///   already been canonicalized (symlinks resolved), so the caller can
///   pass it directly to `std::fs::read_to_string` without a race
///   between the containment check and the read.
/// * `Ok(None)` — silent-skip: either the target doesn't exist (matches
///   pre-fix behaviour for relative `schemaLocation` typos) or the file
///   was already loaded earlier in this build (cycle short-circuit).
/// * `Err(...)` — reject: the target escapes the schema's base directory,
///   or the attribute value is an absolute URI with a scheme we don't
///   support (`http://`, `ftp://`, ...).
fn resolve_include_path(
    schema_location: &str,
    base_dir: Option<&Path>,
    canonical_base: Option<&Path>,
    state: &mut CompositionState,
    kind: &str,
) -> XmlResult<Option<PathBuf>> {
    let resolved_path = match base_dir {
        Some(dir) => dir.join(schema_location),
        None => PathBuf::from(schema_location),
    };
    let canonical = resolved_path.canonicalize().ok();

    // F-10 containment check. When both canonicalized paths exist we
    // require the resolved path to live under the base directory. When
    // the target canonicalize fails we treat it as missing; the old
    // `is_absolute_uri` error is still surfaced for http/ftp/... values
    // that would never have loaded.
    match (canonical_base, canonical.as_ref()) {
        (Some(cb), Some(c)) if !c.starts_with(cb) => {
            return Err(XmlError::validation(format!(
                "Cannot resolve {} schemaLocation '{}': path escapes the schema's base directory",
                kind, schema_location
            )));
        }
        (Some(_), None) => {
            if is_absolute_uri(schema_location) {
                return Err(XmlError::validation(format!(
                    "Cannot resolve {} schemaLocation '{}': absolute URI not supported",
                    kind, schema_location
                )));
            }
            return Ok(None);
        }
        _ => {}
    }

    // F-11 cycle detection keyed on the canonical path.
    if let Some(ref c) = canonical {
        if !state.visited.insert(c.clone()) {
            return Ok(None);
        }
    }

    // Prefer the canonical path for the subsequent read — that path has
    // had its symlinks resolved, so a concurrent symlink swap cannot
    // redirect the read outside the containment check we just passed.
    // Fall back to the lexical `resolved_path` only when there's no
    // containment to enforce (no base_dir), which is the pre-fix shape.
    Ok(Some(canonical.unwrap_or(resolved_path)))
}

/// Process `xs:include`, `xs:redefine`, and `xs:import` elements in a schema
/// document, loading external schemas and merging their declarations into the
/// validator.
///
/// Called during pass 0 of `from_schema_with_base_path` (only when a base path
/// is available for resolving relative `schemaLocation` URIs).
///
/// `state` carries the visited-paths set and depth counter so circular
/// includes terminate cleanly and pathological chains cannot stack-overflow.
pub(super) fn process_schema_composition(
    schema_doc: &Document,
    schema_elem: NodeId,
    validator: &mut XsdValidator,
    base_path: Option<&Path>,
    state: &mut CompositionState,
) -> XmlResult<()> {
    if state.depth >= MAX_INCLUDE_DEPTH {
        return Err(XmlError::validation(format!(
            "Schema include/import/redefine nesting exceeds maximum depth of {}",
            MAX_INCLUDE_DEPTH
        )));
    }

    let base_dir = base_path.and_then(|p| p.parent());
    // Canonicalize the base once per call; reused as the containment
    // anchor for every schemaLocation resolved in the loop below.
    let canonical_base = base_dir.and_then(|b| b.canonicalize().ok());

    for child in schema_doc.children(schema_elem) {
        if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
            let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || elem.name.prefix.as_deref() == Some("xs")
                || elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }

            match elem.name.local_name.as_ref() {
                "include" | "redefine" => {
                    let is_redefine = elem.name.local_name == "redefine";
                    let schema_location = match elem.get_attribute("schemaLocation") {
                        Some(loc) => loc,
                        None => continue, // No schemaLocation, skip
                    };

                    // Resolve the schemaLocation to a canonical path,
                    // enforcing F-10 containment, F-11 cycle detection,
                    // and closing the TOCTOU window against concurrent
                    // symlink swaps (F-12) in a single helper.
                    let kind = if is_redefine { "redefine" } else { "include" };
                    let canonical_path = match resolve_include_path(
                        schema_location,
                        base_dir,
                        canonical_base.as_deref(),
                        state,
                        kind,
                    )? {
                        Some(p) => p,
                        None => continue,
                    };

                    // Load and parse the external schema from the
                    // canonicalized path — symlinks already resolved,
                    // so the read can't be redirected after the check.
                    let ext_str = match std::fs::read_to_string(&canonical_path) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let ext_doc = match crate::parse(&ext_str) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                    // Build a sub-validator from the external schema,
                    // propagating the visited set and incrementing depth.
                    state.depth += 1;
                    let ext_validator = XsdValidator::from_schema_with_composition_state(
                        &ext_doc,
                        Some(&canonical_path),
                        state,
                    )?;
                    state.depth -= 1;

                    // Determine the effective namespace for included declarations.
                    // "Chameleon include": if the external schema has no targetNamespace
                    // but the including schema does, the included declarations adopt
                    // the including schema's targetNamespace.
                    let chameleon = ext_validator.target_namespace.is_none()
                        && validator.target_namespace.is_some();

                    // Merge declarations from external schema into our validator
                    merge_external_declarations(validator, &ext_validator, chameleon);

                    // For xs:redefine, process inline redefinition children
                    if is_redefine {
                        process_redefine_children(schema_doc, child, validator)?;
                    }
                }
                // xs:import — load an external schema with a different targetNamespace.
                // Unlike xs:include, no chameleon fixup is needed: the imported schema
                // keeps its own targetNamespace and its declarations are merged as-is.
                // (Sun tests: xsd004)
                "import" => {
                    let schema_location = match elem.get_attribute("schemaLocation") {
                        Some(loc) => loc,
                        None => continue, // No schemaLocation, skip (namespace-only import)
                    };

                    // Same resolve-then-read sequence as include/redefine.
                    let canonical_path = match resolve_include_path(
                        schema_location,
                        base_dir,
                        canonical_base.as_deref(),
                        state,
                        "import",
                    )? {
                        Some(p) => p,
                        None => continue,
                    };

                    // Load and parse the external schema.
                    let ext_str = match std::fs::read_to_string(&canonical_path) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let ext_doc = match crate::parse(&ext_str) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                    // Build a sub-validator from the external schema.
                    state.depth += 1;
                    let ext_validator = XsdValidator::from_schema_with_composition_state(
                        &ext_doc,
                        Some(&canonical_path),
                        state,
                    )?;
                    state.depth -= 1;

                    // Import never uses chameleon fixup — the imported schema
                    // has its own targetNamespace which is preserved as-is.
                    merge_external_declarations(validator, &ext_validator, false);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Merge declarations from an external (included) schema validator into the main validator.
/// If `chameleon` is true, re-key declarations from `None` namespace to the main validator's
/// target namespace (chameleon include behavior).
fn merge_external_declarations(validator: &mut XsdValidator, ext: &XsdValidator, chameleon: bool) {
    let target_ns = validator.target_namespace.clone();

    // Helper to re-key a (namespace, name) pair for chameleon includes
    let rekey = |key: &(Option<String>, String)| -> (Option<String>, String) {
        if chameleon && key.0.is_none() {
            (target_ns.clone(), key.1.clone())
        } else {
            key.clone()
        }
    };

    for (key, decl) in &ext.elements {
        let new_key = rekey(key);
        let mut new_decl = decl.clone();
        if chameleon && new_decl.namespace.is_none() {
            new_decl.namespace = target_ns.clone();
        }
        // Chameleon: also re-namespace elements inside content models
        if chameleon {
            chameleon_fixup_element_decl(&mut new_decl, &target_ns);
        }
        validator.elements.entry(new_key).or_insert(new_decl);
    }

    for (key, type_def) in &ext.types {
        let new_key = rekey(key);
        let mut new_td = type_def.clone();
        if chameleon {
            chameleon_fixup_type_def(&mut new_td, &target_ns);
        }
        validator.types.entry(new_key).or_insert(new_td);
    }

    for (key, attr) in &ext.global_attributes {
        let new_key = rekey(key);
        validator
            .global_attributes
            .entry(new_key)
            .or_insert(attr.clone());
    }

    for (key, ag) in &ext.attribute_groups {
        let new_key = rekey(key);
        validator
            .attribute_groups
            .entry(new_key)
            .or_insert(ag.clone());
    }

    for (key, mg) in &ext.model_groups {
        let new_key = rekey(key);
        let mut new_mg = mg.clone();
        if chameleon {
            chameleon_fixup_content_model(&mut new_mg.content, &target_ns);
        }
        validator.model_groups.entry(new_key).or_insert(new_mg);
    }
}

/// Fix up an element declaration's namespace for chameleon include:
/// Set the element's namespace and recursively fix up inline type defs.
fn chameleon_fixup_element_decl(decl: &mut ElementDecl, target_ns: &Option<String>) {
    if decl.namespace.is_none() {
        decl.namespace = target_ns.clone();
    }
    chameleon_fixup_type_ref(&mut decl.type_ref, target_ns);
}

/// Fix up a type reference for chameleon include.
/// Named references with `None` namespace are re-pointed to the target namespace.
fn chameleon_fixup_type_ref(type_ref: &mut TypeRef, target_ns: &Option<String>) {
    match type_ref {
        TypeRef::Named(ref mut ns, _) => {
            if ns.is_none() {
                *ns = target_ns.clone();
            }
        }
        TypeRef::Inline(ref mut td) => {
            chameleon_fixup_type_def(td, target_ns);
        }
        _ => {}
    }
}

/// Fix up a type definition for chameleon include.
/// For complex types, fixes the `base_type` reference and recurses into the content model.
fn chameleon_fixup_type_def(td: &mut TypeDef, target_ns: &Option<String>) {
    match td {
        TypeDef::Complex(ref mut ct) => {
            // Fix base_type reference
            if let Some((ref mut ns, _)) = ct.base_type {
                if ns.is_none() {
                    *ns = target_ns.clone();
                }
            }
            chameleon_fixup_content_model(&mut ct.content, target_ns);
        }
        TypeDef::Simple(_) => {
            // Simple types don't reference namespaced components that need fixing
        }
    }
}

/// Fix up a content model for chameleon include.
/// Recurses into sequences, choices, all groups, and simple content.
fn chameleon_fixup_content_model(content: &mut ContentModel, target_ns: &Option<String>) {
    match content {
        ContentModel::Sequence(ref mut particles, _, _)
        | ContentModel::Choice(ref mut particles, _, _) => {
            chameleon_fixup_particles(particles, target_ns);
        }
        ContentModel::All(ref mut particles) => {
            chameleon_fixup_particles(particles, target_ns);
        }
        ContentModel::SimpleContent(ref mut type_ref) => {
            chameleon_fixup_type_ref(type_ref, target_ns);
        }
        _ => {}
    }
}

/// Fix up particles for chameleon include.
/// Recurses into element declarations and nested sequence/choice particles.
fn chameleon_fixup_particles(particles: &mut [Particle], target_ns: &Option<String>) {
    for particle in particles {
        match &mut particle.kind {
            ParticleKind::Element(ref mut decl) => {
                chameleon_fixup_element_decl(decl, target_ns);
            }
            ParticleKind::Sequence(ref mut sub) | ParticleKind::Choice(ref mut sub) => {
                chameleon_fixup_particles(sub, target_ns);
            }
            ParticleKind::Any { .. } => {}
        }
    }
}

/// Process inline redefinition children within an `xs:redefine` element.
///
/// Handles `simpleType`, `complexType`, `group`, and `attributeGroup` redefinitions.
/// For complex types with self-referencing base types (the common redefine pattern),
/// the old definition is saved under a `__redefine_base_` prefixed key and the new
/// definition's `base_type` is updated to point to it.
fn process_redefine_children(
    doc: &Document,
    redefine_node: NodeId,
    validator: &mut XsdValidator,
) -> XmlResult<()> {
    let target_ns = validator.target_namespace.clone();

    for child in doc.children(redefine_node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }

            match child_elem.name.local_name.as_ref() {
                "simpleType" => {
                    let type_def = parse_simple_type(doc, child)?;
                    if let TypeDef::Simple(ref st) = type_def {
                        if let Some(name) = &st.name {
                            let key = (target_ns.clone(), name.clone());
                            validator.types.insert(key, type_def);
                        }
                    }
                }
                "complexType" => {
                    // For redefine, self-references (base="X" where X is the name
                    // being redefined) should resolve to the OLD definition.
                    // We rename the old definition to a unique key and update the
                    // new definition's base_type to reference the renamed key.
                    let local_elem_ns = target_ns.clone(); // qualified by default in redefined types
                    let type_def = parse_complex_type(
                        doc,
                        child,
                        &local_elem_ns,
                        &target_ns,
                        &target_ns,
                        &validator.attribute_groups,
                        &validator.model_groups,
                        validator.block_default_extension,
                        validator.block_default_restriction,
                    )?;
                    if let TypeDef::Complex(ref ct) = type_def {
                        if let Some(name) = &ct.name {
                            let key = (target_ns.clone(), name.clone());
                            // If the base_type references itself (same name), it's a
                            // self-referencing redefine: save old def under a unique key.
                            if let Some(ref base) = ct.base_type {
                                if base.1 == *name && base.0 == target_ns {
                                    let old_key =
                                        (target_ns.clone(), format!("__redefine_base_{}", name));
                                    if let Some(old_td) = validator.types.get(&key).cloned() {
                                        validator.types.insert(old_key.clone(), old_td);
                                    }
                                    // Update the new definition's base_type to point to the renamed old def
                                    let mut new_td = type_def.clone();
                                    if let TypeDef::Complex(ref mut new_ct) = new_td {
                                        new_ct.base_type =
                                            Some((old_key.0.clone(), old_key.1.clone()));
                                    }
                                    validator.types.insert(key, new_td);
                                } else {
                                    validator.types.insert(key, type_def);
                                }
                            } else {
                                validator.types.insert(key, type_def);
                            }
                        }
                    }
                }
                "group" => {
                    // Redefine a model group: the self-reference inside should
                    // resolve to the OLD group definition.
                    if let Some(g_elem) = doc.element(child) {
                        if let Some(name) = g_elem.get_attribute("name") {
                            // Save the old definition before overwriting
                            let key = (target_ns.clone(), name.to_string());
                            let old_mg = validator.model_groups.get(&key).cloned();

                            // Parse with a temporary model_groups that has the old
                            // definition available for self-reference resolution.
                            // (The current model_groups already has it from the merge.)
                            let local_elem_ns = target_ns.clone();
                            let mg_def = parse_model_group_def(
                                doc,
                                child,
                                &local_elem_ns,
                                &target_ns,
                                &validator.attribute_groups,
                                &validator.model_groups,
                                validator.block_default_extension,
                                validator.block_default_restriction,
                            )?;
                            let _ = old_mg; // suppress unused warning
                            validator.model_groups.insert(key, mg_def);
                        }
                    }
                }
                "attributeGroup" => {
                    if let Some(ag_elem) = doc.element(child) {
                        if let Some(name) = ag_elem.get_attribute("name") {
                            let ag_def = parse_attribute_group_def(
                                doc,
                                child,
                                &target_ns,
                                &validator.global_attributes,
                                &validator.attribute_groups,
                            )?;
                            let key = (target_ns.clone(), name.to_string());
                            validator.attribute_groups.insert(key, ag_def);
                        }
                    }
                }
                _ => {} // annotation, etc.
            }
        }
    }

    // After all redefine children are processed, re-resolve complex types
    // that reference the (possibly updated) model groups and attribute groups.
    reresolve_types_after_redefine(validator);

    Ok(())
}

/// After `xs:redefine` processing, re-resolve any complex types whose group or
/// attributeGroup references may have been updated by the redefinitions.
///
/// This is necessary because the external schema's types were parsed with the
/// OLD group/attributeGroup definitions eagerly inlined; after redefine replaces
/// those definitions, we need to update the types to reflect the new definitions.
fn reresolve_types_after_redefine(validator: &mut XsdValidator) {
    // Collect keys that need re-resolution to avoid borrow issues
    let keys_to_update: Vec<(Option<String>, String)> = validator
        .types
        .iter()
        .filter_map(|(key, td)| {
            if let TypeDef::Complex(ct) = td {
                if ct.group_ref.is_some() || !ct.attribute_group_refs.is_empty() {
                    return Some(key.clone());
                }
            }
            None
        })
        .collect();

    for key in keys_to_update {
        let td = match validator.types.get(&key) {
            Some(td) => td.clone(),
            None => continue,
        };
        if let TypeDef::Complex(mut ct) = td {
            // Re-resolve model group reference
            if let Some(ref mg_key) = ct.group_ref {
                if let Some(mg) = validator.model_groups.get(mg_key) {
                    ct.content = mg.content.clone();
                }
            }
            // Re-resolve attribute group references
            if !ct.attribute_group_refs.is_empty() {
                // Rebuild attributes: start with non-attributeGroup attributes.
                // For simplicity, we re-derive all attributes from the attribute
                // group refs. Any directly declared attributes on the complexType
                // that aren't from group refs would need to be preserved, but
                // in practice the external schema complexTypes only get attributes
                // from attributeGroup refs (which are what we're re-resolving).
                let mut new_attrs = Vec::new();
                let mut new_wildcard = ct.attribute_wildcard.clone();
                for ag_key in &ct.attribute_group_refs {
                    if let Some(ag) = validator.attribute_groups.get(ag_key) {
                        new_attrs.extend(ag.attributes.iter().cloned());
                        if let Some(ref ag_wc) = ag.wildcard {
                            new_wildcard = match new_wildcard {
                                Some(existing_wc) => existing_wc.intersect(ag_wc),
                                None => Some(ag_wc.clone()),
                            };
                        }
                    }
                }
                ct.attributes = new_attrs;
                ct.attribute_wildcard = new_wildcard;
            }
            validator.types.insert(key, TypeDef::Complex(ct));
        }
    }
}

/// Check if a string looks like an absolute URI (starts with a scheme per RFC 3986:
/// `ALPHA *(ALPHA / DIGIT / "+" / "-" / ".") ":"`).
fn is_absolute_uri(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for &b in &bytes[1..] {
        if b == b':' {
            return true;
        }
        if !b.is_ascii_alphanumeric() && b != b'+' && b != b'-' && b != b'.' {
            return false;
        }
    }
    false
}
