//! List item facet resolution helpers.
//!
//! When a list type's `itemType` is a user-defined simple type (not a built-in),
//! the item type's facets must be resolved in post-processing passes after
//! initial schema parsing. These functions walk through type references, content
//! models, and particles to resolve and store item facets for later validation.

use std::collections::HashMap;

use super::types::{BuiltInType, ContentModel, Facet, Particle, ParticleKind, TypeDef, TypeRef};

/// Resolve list item facets for an inline SimpleTypeDef within a TypeRef.
/// Also recurses into inline ComplexTypeDefs to resolve their content model particles.
pub(super) fn resolve_inline_list_item_facets(
    type_ref: &mut TypeRef,
    resolved_items: &HashMap<(Option<String>, String), (BuiltInType, Vec<Facet>)>,
    schema_ns: &Option<String>,
) {
    match type_ref {
        TypeRef::Inline(td) => match td.as_mut() {
            TypeDef::Simple(st) => {
                if st.is_list {
                    if let Some(item_name) = &st._item_type_local {
                        let item_key = (schema_ns.clone(), item_name.clone());
                        if let Some((item_base, item_facets)) = resolved_items.get(&item_key) {
                            st.item_type = Some(item_base.clone());
                            st.item_facets = item_facets.clone();
                        }
                    }
                }
            }
            TypeDef::Complex(ct) => {
                resolve_content_model_list_item_facets(&mut ct.content, resolved_items, schema_ns);
            }
        },
        _ => {}
    }
}

/// Resolve list item facets in all inline types within a content model's particles.
pub(super) fn resolve_content_model_list_item_facets(
    content: &mut ContentModel,
    resolved_items: &HashMap<(Option<String>, String), (BuiltInType, Vec<Facet>)>,
    schema_ns: &Option<String>,
) {
    match content {
        ContentModel::Sequence(particles, _, _) | ContentModel::Choice(particles, _, _) => {
            resolve_particles_list_item_facets(particles, resolved_items, schema_ns);
        }
        ContentModel::All(particles) => {
            resolve_particles_list_item_facets(particles, resolved_items, schema_ns);
        }
        _ => {}
    }
}

/// Resolve list item facets in all particles recursively.
fn resolve_particles_list_item_facets(
    particles: &mut [Particle],
    resolved_items: &HashMap<(Option<String>, String), (BuiltInType, Vec<Facet>)>,
    schema_ns: &Option<String>,
) {
    for particle in particles.iter_mut() {
        match &mut particle.kind {
            ParticleKind::Element(decl) => {
                resolve_inline_list_item_facets(&mut decl.type_ref, resolved_items, schema_ns);
            }
            ParticleKind::Sequence(sub) | ParticleKind::Choice(sub) => {
                resolve_particles_list_item_facets(sub, resolved_items, schema_ns);
            }
            ParticleKind::Any { .. } => {}
        }
    }
}
