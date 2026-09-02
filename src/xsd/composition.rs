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
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{XmlError, XmlResult};

use super::parser::{
    parse_attribute_group_def, parse_complex_type, parse_model_group_def, parse_simple_type,
};
use super::types::{
    AttributeDecl, ContentModel, ElementDecl, Particle, ParticleKind, TypeDef, TypeRef,
    XsdValidator,
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

struct ResolvedSchemaPath {
    path: PathBuf,
    identity: Option<FileIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
    Some(FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<FileIdentity> {
    None
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
/// F-10 containment, F-11 cycle detection, and the first half of the
/// F-12 TOCTOU defense.
///
/// Returns:
/// * `Ok(Some(resolved))` — load this path through `read_resolved_schema`,
///   not directly. The path has been canonicalized and paired with the file
///   identity observed during resolution so the read side can detect a later
///   symlink swap before trusting bytes from the opened handle.
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
) -> XmlResult<Option<ResolvedSchemaPath>> {
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

    let path = canonical.unwrap_or(resolved_path);
    let identity = fs::metadata(&path).ok().and_then(|m| file_identity(&m));
    Ok(Some(ResolvedSchemaPath { path, identity }))
}

fn read_resolved_schema(
    resolved: &ResolvedSchemaPath,
    schema_location: &str,
    kind: &str,
) -> XmlResult<Option<String>> {
    let mut file = match File::open(&resolved.path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };

    if let Some(expected) = resolved.identity {
        let actual = file
            .metadata()
            .ok()
            .and_then(|metadata| file_identity(&metadata));
        if actual != Some(expected) {
            return Err(XmlError::validation(format!(
                "Cannot resolve {} schemaLocation '{}': file changed during resolution",
                kind, schema_location
            )));
        }
    }

    let mut contents = String::new();
    match file.read_to_string(&mut contents) {
        Ok(_) => Ok(Some(contents)),
        Err(_) => Ok(None),
    }
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

    // `base_path` is either the schema *directory* or the schema *file*:
    //   * the public entry (`from_file`, and the etree `XMLSchema(file=...)`
    //     facade, which passes `os.path.dirname(file)`) supplies a directory,
    //     since the top-level schema is handed in as a string with no file of
    //     its own;
    //   * recursive `xs:import`/`xs:include`/`xs:redefine` loads pass the
    //     resolved schema *file* path (see the `from_schema_with_composition_state`
    //     calls below).
    // `schemaLocation` is resolved relative to the directory in both cases.
    //
    // Only a path that is *known to be a regular file* (the recursive loads) is
    // reduced to its parent directory. A directory, or any path that cannot be
    // stat'd (missing or unreadable), is kept as the base directory itself. This
    // matters for the fail-closed contract below: a bad base directory must
    // still fail to canonicalize and be rejected, rather than silently resolving
    // against its parent -- which a `!is_dir()` test would do, since `is_dir()`
    // also returns false for a missing/unreadable directory.
    //
    // (The previous unconditional `.parent()` treated the public entry's
    // directory as a file and stripped one level, so every import/include
    // silently failed to resolve and all imported declarations -- types *and*
    // elements -- went missing, surfacing as "No element declaration found" /
    // "Type not found".)
    let base_dir: Option<PathBuf> = base_path.map(|p| {
        if p.is_file() {
            // A file always has a parent; an empty parent denotes the current
            // directory, so fall back to "." rather than the file path itself
            // (joining onto the file would yield e.g. `schema.xsd/inner.xsd`).
            match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => PathBuf::from("."),
            }
        } else {
            p.to_path_buf()
        }
    });
    // Canonicalize the base once per call; reused as the containment
    // anchor for every schemaLocation resolved in the loop below.
    //
    // Fail closed when the caller supplied a base directory we cannot
    // canonicalize (permission denied, missing, race with `rm -rf`,
    // etc.). Falling through to `canonical_base = None` would skip the
    // containment check inside `resolve_include_path` and re-open the
    // arbitrary-file-read window F-10 closed.
    let canonical_base = match &base_dir {
        Some(b) => Some(b.canonicalize().map_err(|e| {
            XmlError::validation(format!(
                "Failed to canonicalize schema base directory '{}': {}",
                b.display(),
                e
            ))
        })?),
        None => None,
    };

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

                    // Resolve the schemaLocation to a contained canonical path
                    // and remember the resolved file identity. The subsequent
                    // read verifies the opened handle still has that identity;
                    // the race is closed by the resolve/read pair, not by this
                    // path helper alone.
                    let kind = if is_redefine { "redefine" } else { "include" };
                    let resolved_schema = match resolve_include_path(
                        schema_location,
                        base_dir.as_deref(),
                        canonical_base.as_deref(),
                        state,
                        kind,
                    )? {
                        Some(p) => p,
                        None => continue,
                    };

                    // Load through the resolved descriptor. Canonicalization
                    // proves the path was contained at check time; on Unix the
                    // stored file identity also proves the opened handle is the
                    // same file, closing the symlink-swap race. Other platforms
                    // retain canonical containment but need platform handle APIs
                    // for the same post-open identity guarantee.
                    let ext_str =
                        match read_resolved_schema(&resolved_schema, schema_location, kind)? {
                            Some(s) => s,
                            None => continue,
                        };
                    let ext_doc = match crate::parse(&ext_str) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                    // Build a sub-validator from the external schema,
                    // propagating the visited set and incrementing depth.
                    // Decrement on every exit path (Ok or Err) so a
                    // failure deep in the include tree cannot leave
                    // `state.depth` desynced for any later sibling
                    // includes.
                    state.depth += 1;
                    let ext_validator_res = XsdValidator::from_schema_with_composition_state(
                        &ext_doc,
                        Some(&resolved_schema.path),
                        state,
                    );
                    state.depth -= 1;
                    let ext_validator = ext_validator_res?;

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

                    // `xs:import/@schemaLocation` is only a *hint* (XSD 1.0 Part 1
                    // §4.2.3): a processor may ignore it and is not obliged to
                    // resolve it. Unlike `xs:include`/`xs:redefine` (which still treat
                    // absolute-URI schemes and base-directory escapes as hard errors),
                    // an import whose location cannot be resolved — unsupported schemes,
                    // a missing file, or a path outside the base directory — is skipped
                    // rather than aborting the whole build. This matches
                    // libxml2/Xerces and is what lets composite schemas (e.g.
                    // pyFF's `schema.xsd`) build: their imported schemas carry
                    // redundant absolute/classpath import hints for namespaces
                    // already supplied by a sibling, resolvable import.
                    //
                    // Only an *unresolvable* location is skipped. Once the hint
                    // resolves to a real, readable file the imported schema is
                    // genuinely present, so a malformed (non-well-formed) or
                    // semantically broken target is a real error and is surfaced,
                    // not silently dropped.
                    let resolved_schema = match resolve_include_path(
                        schema_location,
                        base_dir.as_deref(),
                        canonical_base.as_deref(),
                        state,
                        "import",
                    ) {
                        Ok(Some(p)) => p,
                        Ok(None) | Err(_) => continue,
                    };

                    // Load the external schema, verifying after open that the
                    // handle still matches the resolved file identity where the
                    // platform exposes one through std. A post-resolution open or
                    // read failure (`Ok(None)` — the file vanished or became
                    // unreadable after resolving) is treated as the hint failing
                    // to resolve and is skipped, consistent with the hint
                    // semantics above; only a file-identity mismatch is surfaced
                    // (the `?`), since that signals a TOCTOU swap rather than an
                    // absent hint.
                    let ext_str =
                        match read_resolved_schema(&resolved_schema, schema_location, "import")? {
                            Some(s) => s,
                            None => continue,
                        };
                    // The file resolved and its bytes were read: a parse failure
                    // is a real broken-schema error, surfaced with context rather
                    // than skipped.
                    let ext_doc = crate::parse(&ext_str).map_err(|e| {
                        XmlError::validation(format!(
                            "imported schema '{schema_location}' (resolved to {}) is not well-formed: {e}",
                            resolved_schema.path.display()
                        ))
                    })?;

                    // Build a sub-validator from the external schema.
                    // Same balanced-decrement pattern as the include /
                    // redefine branch above: decrement runs on Ok and
                    // Err alike, so a failure inside the import chain
                    // cannot desync `state.depth` for sibling imports.
                    state.depth += 1;
                    let ext_validator_res = XsdValidator::from_schema_with_composition_state(
                        &ext_doc,
                        Some(&resolved_schema.path),
                        state,
                    );
                    state.depth -= 1;
                    let ext_validator = ext_validator_res?;

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
        let mut new_attr = attr.clone();
        // Chameleon: global attributes take on the including schema's target
        // namespace, matching their re-keyed lookup entry.
        if chameleon && new_attr.namespace.is_none() {
            new_attr.namespace = target_ns.clone();
        }
        validator
            .global_attributes
            .entry(new_key)
            .or_insert(new_attr);
    }

    for (key, ag) in &ext.attribute_groups {
        let new_key = rekey(key);
        let mut new_ag = ag.clone();
        if chameleon {
            chameleon_fixup_attribute_decls(&mut new_ag.attributes, &target_ns);
        }
        validator.attribute_groups.entry(new_key).or_insert(new_ag);
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
/// For complex types, fixes the `base_type` reference, the attribute uses,
/// and recurses into the content model.
fn chameleon_fixup_type_def(td: &mut TypeDef, target_ns: &Option<String>) {
    match td {
        TypeDef::Complex(ref mut ct) => {
            // Fix base_type reference
            if let Some((ref mut ns, _)) = ct.base_type {
                if ns.is_none() {
                    *ns = target_ns.clone();
                }
            }
            chameleon_fixup_attribute_decls(&mut ct.attributes, target_ns);
            chameleon_fixup_attribute_decls(&mut ct.own_attributes, target_ns);
            for ag_key in &mut ct.attribute_group_refs {
                if ag_key.0.is_none() {
                    ag_key.0 = target_ns.clone();
                }
            }
            chameleon_fixup_content_model(&mut ct.content, target_ns);
        }
        TypeDef::Simple(_) => {
            // Simple types don't reference namespaced components that need fixing
        }
    }
}

/// Fix up attribute uses for chameleon include: references to the module's
/// global attributes follow those globals into the including schema's target
/// namespace, and local uses qualified via `form`/`attributeFormDefault`
/// (whose namespace was None because the module had no targetNamespace) take
/// the target namespace as well. Local unqualified declarations stay in no
/// namespace.
fn chameleon_fixup_attribute_decls(attributes: &mut [AttributeDecl], target_ns: &Option<String>) {
    for attr in attributes {
        if (attr.is_ref || attr.qualified) && attr.namespace.is_none() {
            attr.namespace = target_ns.clone();
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
                // Rebuild attributes starting from the attributes declared directly
                // on this type (bare `<xsd:attribute>` children, preserved in
                // `own_attributes`), then re-append the (possibly updated)
                // attribute group contributions. Previously this rebuilt
                // `attributes` from only the group refs, silently discarding any
                // attributes declared directly on the type alongside an
                // attributeGroup ref.
                let mut new_attrs = ct.own_attributes.clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn read_resolved_schema_detects_stale_file_identity() {
        let dir = std::env::temp_dir().join(format!(
            "uppsala-composition-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("include.xsd");
        fs::write(&path, "old").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let resolved = ResolvedSchemaPath {
            path: path.clone(),
            identity: file_identity(&metadata),
        };
        let replacement = dir.join("replacement.xsd");
        fs::write(&replacement, "new").unwrap();
        fs::rename(&replacement, &path).unwrap();

        let err = read_resolved_schema(&resolved, "include.xsd", "include")
            .expect_err("changed file identity must be rejected");
        assert!(err.to_string().contains("file changed during resolution"));

        fs::remove_dir_all(&dir).ok();
    }
}
