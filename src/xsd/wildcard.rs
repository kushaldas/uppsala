//! Wildcard namespace constraint operations.
//!
//! Provides functions for checking namespace membership against wildcard
//! constraints, computing the intersection and union of namespace constraints,
//! and determining the stricter of two processContents values.
//!
//! These are used by [`AttributeWildcard`](super::types::AttributeWildcard) methods
//! and by element wildcard validation in the validation module.

use super::types::{NamespaceConstraint, ProcessContents};

/// Check if a namespace URI matches a wildcard namespace constraint.
///
/// Works for both attribute and element wildcards. Returns `true` if the
/// given namespace (or absence thereof) is allowed by the constraint.
pub(crate) fn wildcard_allows_namespace(
    constraint: &NamespaceConstraint,
    ns: &Option<String>,
) -> bool {
    match constraint {
        NamespaceConstraint::Any => true,
        NamespaceConstraint::Other(target_ns) => {
            match ns {
                None => false, // ##other excludes no-namespace
                Some(uri) => match target_ns {
                    Some(tns) => uri != tns,
                    None => true,
                },
            }
        }
        NamespaceConstraint::Local => ns.is_none(),
        NamespaceConstraint::TargetNamespace(target_ns) => ns == target_ns,
        NamespaceConstraint::List(uris) => match ns {
            None => uris.iter().any(|u| u == "##local"),
            Some(uri) => uris.iter().any(|u| u == uri),
        },
    }
}

/// Return the stricter of two processContents values.
///
/// Ordering: Strict > Lax > Skip. Used when intersecting wildcards to
/// ensure the most restrictive validation mode is preserved.
pub(super) fn stricter_process_contents(
    a: &ProcessContents,
    b: &ProcessContents,
) -> ProcessContents {
    match (a, b) {
        (ProcessContents::Strict, _) | (_, ProcessContents::Strict) => ProcessContents::Strict,
        (ProcessContents::Lax, _) | (_, ProcessContents::Lax) => ProcessContents::Lax,
        _ => ProcessContents::Skip,
    }
}

/// Compute the intersection of two namespace constraints.
///
/// Returns `None` if the intersection is empty (no namespace allowed by both).
/// Used when merging attribute groups that both define wildcards — the result
/// only allows namespaces permitted by both wildcards.
pub(super) fn intersect_namespace_constraints(
    a: &NamespaceConstraint,
    b: &NamespaceConstraint,
) -> Option<NamespaceConstraint> {
    match (a, b) {
        // Any intersected with anything is that thing
        (NamespaceConstraint::Any, other) | (other, NamespaceConstraint::Any) => {
            Some(other.clone())
        }
        // Two lists: intersection of URI sets
        (NamespaceConstraint::List(list_a), NamespaceConstraint::List(list_b)) => {
            let result: Vec<String> = list_a
                .iter()
                .filter(|u| list_b.contains(u))
                .cloned()
                .collect();
            if result.is_empty() {
                None // empty intersection — no namespace allowed
            } else {
                Some(NamespaceConstraint::List(result))
            }
        }
        // Other intersected with Other: still Other (same target)
        (NamespaceConstraint::Other(tns_a), NamespaceConstraint::Other(_tns_b)) => {
            Some(NamespaceConstraint::Other(tns_a.clone()))
        }
        // List intersected with Other: keep URIs from list that are not target NS
        (NamespaceConstraint::List(list), NamespaceConstraint::Other(tns))
        | (NamespaceConstraint::Other(tns), NamespaceConstraint::List(list)) => {
            let result: Vec<String> = list
                .iter()
                .filter(|u| {
                    if *u == "##local" {
                        return false; // ##other excludes unqualified
                    }
                    match tns {
                        Some(t) => *u != t,
                        None => true,
                    }
                })
                .cloned()
                .collect();
            if result.is_empty() {
                None
            } else {
                Some(NamespaceConstraint::List(result))
            }
        }
        // Local intersected with Local: Local
        (NamespaceConstraint::Local, NamespaceConstraint::Local) => {
            Some(NamespaceConstraint::Local)
        }
        // Local intersected with Other: empty (Other excludes no-namespace)
        (NamespaceConstraint::Local, NamespaceConstraint::Other(_))
        | (NamespaceConstraint::Other(_), NamespaceConstraint::Local) => None,
        // Local intersected with TargetNamespace: empty (disjoint)
        (NamespaceConstraint::Local, NamespaceConstraint::TargetNamespace(_))
        | (NamespaceConstraint::TargetNamespace(_), NamespaceConstraint::Local) => None,
        // Local intersected with List: keep only ##local if present
        (NamespaceConstraint::Local, NamespaceConstraint::List(list))
        | (NamespaceConstraint::List(list), NamespaceConstraint::Local) => {
            if list.iter().any(|u| u == "##local") {
                Some(NamespaceConstraint::Local)
            } else {
                None
            }
        }
        // TargetNamespace intersected with TargetNamespace: same
        (NamespaceConstraint::TargetNamespace(a_tns), NamespaceConstraint::TargetNamespace(_)) => {
            Some(NamespaceConstraint::TargetNamespace(a_tns.clone()))
        }
        // TargetNamespace intersected with Other: empty (Other excludes target)
        (NamespaceConstraint::TargetNamespace(_), NamespaceConstraint::Other(_))
        | (NamespaceConstraint::Other(_), NamespaceConstraint::TargetNamespace(_)) => None,
        // TargetNamespace intersected with List: keep target NS if in list
        (NamespaceConstraint::TargetNamespace(tns), NamespaceConstraint::List(list))
        | (NamespaceConstraint::List(list), NamespaceConstraint::TargetNamespace(tns)) => match tns
        {
            Some(t) if list.contains(t) => Some(NamespaceConstraint::TargetNamespace(tns.clone())),
            _ => None,
        },
    }
}

/// Compute the union of two namespace constraints.
///
/// Used when computing the effective wildcard for complex type extensions —
/// the derived type's wildcard is unioned with the base type's wildcard.
/// Falls back to `Any` for combinations that don't have a more specific result.
pub(super) fn union_namespace_constraints(
    a: &NamespaceConstraint,
    b: &NamespaceConstraint,
) -> NamespaceConstraint {
    match (a, b) {
        // Any union anything = Any
        (NamespaceConstraint::Any, _) | (_, NamespaceConstraint::Any) => NamespaceConstraint::Any,
        // Two lists: union of URI sets
        (NamespaceConstraint::List(list_a), NamespaceConstraint::List(list_b)) => {
            let mut result = list_a.clone();
            for u in list_b {
                if !result.contains(u) {
                    result.push(u.clone());
                }
            }
            NamespaceConstraint::List(result)
        }
        // Other cases: conservative fallback to Any
        _ => NamespaceConstraint::Any,
    }
}
