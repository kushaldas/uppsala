//! Namespaces in XML 1.0 (Third Edition) implementation.
//!
//! This module provides namespace prefix resolution with proper scoping.
//! The [`NamespaceResolver`] maintains a stack of scopes, each of which maps
//! prefixes to namespace URIs. When the parser enters an element with
//! `xmlns` declarations a new scope is pushed, and when the element closes
//! the scope is popped.

use crate::dom::{Document, NodeId, NodeKind};

/// Well-known namespace URIs.
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
pub const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// A single namespace scope that maps prefixes to namespace URIs.
#[derive(Debug, Clone)]
struct NamespaceScope {
    /// Maps prefix (empty string for default namespace) to namespace URI.
    bindings: Vec<(String, String)>,
}

impl NamespaceScope {
    fn new() -> Self {
        NamespaceScope {
            bindings: Vec::new(),
        }
    }
}

/// Resolves namespace prefixes to URIs, maintaining a stack of scopes.
///
/// The resolver always has the built-in `xml` prefix bound to
/// `http://www.w3.org/XML/1998/namespace` and the `xmlns` prefix bound to
/// `http://www.w3.org/2000/xmlns/`.
#[derive(Debug, Clone)]
pub struct NamespaceResolver {
    scopes: Vec<NamespaceScope>,
}

impl NamespaceResolver {
    /// Create a new resolver with the built-in `xml` and `xmlns` bindings.
    pub fn new() -> Self {
        let mut root_scope = NamespaceScope::new();
        root_scope
            .bindings
            .push(("xml".to_string(), XML_NAMESPACE.to_string()));
        root_scope
            .bindings
            .push(("xmlns".to_string(), XMLNS_NAMESPACE.to_string()));
        NamespaceResolver {
            scopes: vec![root_scope],
        }
    }

    /// Push a new (empty) namespace scope. Call this when entering an element.
    pub fn push_scope(&mut self) {
        self.scopes.push(NamespaceScope::new());
    }

    /// Pop the current namespace scope. Call this when leaving an element.
    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Declare a namespace binding in the current scope.
    ///
    /// `prefix` is the empty string for a default namespace declaration.
    pub fn declare(&mut self, prefix: String, uri: String) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.push((prefix, uri));
        }
    }

    /// Resolve a prefixed name to a namespace URI.
    ///
    /// Searches from the innermost scope outward.
    pub fn resolve(&self, prefix: &str) -> Option<&str> {
        for scope in self.scopes.iter().rev() {
            for (p, uri) in scope.bindings.iter().rev() {
                if p == prefix {
                    return Some(uri.as_str());
                }
            }
        }
        None
    }

    /// Resolve the default namespace (empty prefix).
    pub fn resolve_default(&self) -> Option<&str> {
        for scope in self.scopes.iter().rev() {
            for (p, uri) in scope.bindings.iter().rev() {
                if p.is_empty() {
                    if uri.is_empty() {
                        return None; // xmlns="" undeclares the default namespace
                    }
                    return Some(uri.as_str());
                }
            }
        }
        None
    }

    /// Return all in-scope namespace bindings (prefix -> URI).
    pub fn in_scope_namespaces(&self) -> Vec<(&str, &str)> {
        let mut result: Vec<(&str, &str)> = Vec::new();
        let mut seen_prefixes = std::collections::HashSet::new();
        for scope in self.scopes.iter().rev() {
            for (p, uri) in scope.bindings.iter().rev() {
                if seen_prefixes.insert(p.as_str()) {
                    result.push((p.as_str(), uri.as_str()));
                }
            }
        }
        result
    }

    /// Return the depth of the namespace scope stack.
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }
}

impl Default for NamespaceResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a `NamespaceResolver` from a document by walking from a node up to
/// the root, collecting all in-scope namespace declarations.
pub fn build_resolver_for_node(doc: &Document, node_id: NodeId) -> NamespaceResolver {
    let mut resolver = NamespaceResolver::new();

    // Collect ancestor chain (from root down to the node)
    let mut chain = Vec::new();
    let mut current = Some(node_id);
    while let Some(id) = current {
        chain.push(id);
        current = doc.parent(id);
    }
    chain.reverse();

    // Walk from root to node, pushing scopes
    for &id in &chain {
        if let Some(NodeKind::Element(elem)) = doc.node_kind(id) {
            resolver.push_scope();
            for (prefix, uri) in &elem.namespace_declarations {
                resolver.declare(prefix.clone(), uri.clone());
            }
        }
    }

    resolver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_prefix() {
        let mut resolver = NamespaceResolver::new();
        resolver.push_scope();
        resolver.declare(
            "soap".to_string(),
            "http://www.w3.org/2003/05/soap-envelope".to_string(),
        );
        assert_eq!(
            resolver.resolve("soap"),
            Some("http://www.w3.org/2003/05/soap-envelope")
        );
    }

    #[test]
    fn test_resolve_default_namespace() {
        let mut resolver = NamespaceResolver::new();
        resolver.push_scope();
        resolver.declare(String::new(), "http://example.com/default".to_string());
        assert_eq!(
            resolver.resolve_default(),
            Some("http://example.com/default")
        );
    }

    #[test]
    fn test_scope_shadowing() {
        let mut resolver = NamespaceResolver::new();
        resolver.push_scope();
        resolver.declare("ns".to_string(), "http://first.com".to_string());
        assert_eq!(resolver.resolve("ns"), Some("http://first.com"));

        resolver.push_scope();
        resolver.declare("ns".to_string(), "http://second.com".to_string());
        assert_eq!(resolver.resolve("ns"), Some("http://second.com"));

        resolver.pop_scope();
        assert_eq!(resolver.resolve("ns"), Some("http://first.com"));
    }

    #[test]
    fn test_undeclare_default_namespace() {
        let mut resolver = NamespaceResolver::new();
        resolver.push_scope();
        resolver.declare(String::new(), "http://example.com".to_string());
        assert_eq!(resolver.resolve_default(), Some("http://example.com"));

        resolver.push_scope();
        resolver.declare(String::new(), String::new());
        assert_eq!(resolver.resolve_default(), None);
    }

    #[test]
    fn test_builtin_xml_prefix() {
        let resolver = NamespaceResolver::new();
        assert_eq!(
            resolver.resolve("xml"),
            Some("http://www.w3.org/XML/1998/namespace")
        );
    }
}
