//! Integration tests for Namespaces in XML 1.0 (Third Edition).
//!
//! Tests cover namespace declarations, prefix resolution, default namespaces,
//! namespace scoping, undeclaration, and error cases.

use uppsala::dom::NodeKind;

// ─── Basic namespace declarations ────────────────────────

#[test]
fn default_namespace() {
    let xml = r#"<root xmlns="http://example.com">text</root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(
        elem.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );
    assert!(elem.name.prefix.is_none());
    assert_eq!(elem.name.local_name, "root");
}

#[test]
fn prefixed_namespace() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(
        elem.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );
    assert_eq!(elem.name.prefix.as_deref(), Some("ns"));
    assert_eq!(elem.name.local_name, "root");
}

#[test]
fn multiple_prefixes() {
    let xml = r#"<root xmlns:a="http://a.com" xmlns:b="http://b.com"><a:child/><b:child/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 2);

    let a_child = doc.element(children[0]).unwrap();
    assert_eq!(a_child.name.namespace_uri.as_deref(), Some("http://a.com"));
    assert_eq!(a_child.name.prefix.as_deref(), Some("a"));

    let b_child = doc.element(children[1]).unwrap();
    assert_eq!(b_child.name.namespace_uri.as_deref(), Some("http://b.com"));
    assert_eq!(b_child.name.prefix.as_deref(), Some("b"));
}

// ─── Namespace inheritance ──────────────────────────────

#[test]
fn default_namespace_inherited_by_children() {
    let xml = r#"<root xmlns="http://example.com"><child><grandchild/></child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];
    let grandchild = doc.children(child)[0];

    let child_elem = doc.element(child).unwrap();
    assert_eq!(
        child_elem.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );

    let gc_elem = doc.element(grandchild).unwrap();
    assert_eq!(
        gc_elem.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );
}

#[test]
fn prefixed_namespace_inherited_by_children() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"><ns:child/></ns:root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];
    let child_elem = doc.element(child).unwrap();
    assert_eq!(
        child_elem.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );
}

// ─── Namespace scoping and shadowing ────────────────────

#[test]
fn namespace_shadowing() {
    let xml = r#"<root xmlns:ns="http://outer.com"><ns:child xmlns:ns="http://inner.com"><ns:grandchild/></ns:child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];
    let grandchild = doc.children(child)[0];

    let child_elem = doc.element(child).unwrap();
    assert_eq!(
        child_elem.name.namespace_uri.as_deref(),
        Some("http://inner.com")
    );

    let gc_elem = doc.element(grandchild).unwrap();
    assert_eq!(
        gc_elem.name.namespace_uri.as_deref(),
        Some("http://inner.com")
    );
}

#[test]
fn default_namespace_override() {
    let xml = r#"<root xmlns="http://outer.com"><child xmlns="http://inner.com"/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];

    let root_elem = doc.element(root).unwrap();
    assert_eq!(
        root_elem.name.namespace_uri.as_deref(),
        Some("http://outer.com")
    );

    let child_elem = doc.element(child).unwrap();
    assert_eq!(
        child_elem.name.namespace_uri.as_deref(),
        Some("http://inner.com")
    );
}

// ─── Default namespace undeclaration ────────────────────

#[test]
fn default_namespace_undeclaration() {
    let xml = r#"<root xmlns="http://example.com"><child xmlns=""/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];

    let root_elem = doc.element(root).unwrap();
    assert_eq!(
        root_elem.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );

    let child_elem = doc.element(child).unwrap();
    // After undeclaration, no namespace
    assert!(child_elem.name.namespace_uri.is_none());
}

// ─── Namespace on attributes ────────────────────────────

#[test]
fn prefixed_attribute() {
    let xml = r#"<root xmlns:ns="http://example.com" ns:attr="value"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    let attr = &elem.attributes[0];
    assert_eq!(
        attr.name.namespace_uri.as_deref(),
        Some("http://example.com")
    );
    assert_eq!(attr.name.prefix.as_deref(), Some("ns"));
    assert_eq!(attr.name.local_name, "attr");
    assert_eq!(attr.value, "value");
}

#[test]
fn unprefixed_attribute_no_namespace() {
    // Per the namespace spec, unprefixed attributes have no namespace
    let xml = r#"<root xmlns="http://example.com" attr="value"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    let attr = &elem.attributes[0];
    assert!(attr.name.namespace_uri.is_none());
    assert_eq!(attr.name.local_name, "attr");
}

// ─── Namespace declarations stored on elements ──────────

#[test]
fn namespace_declarations_map() {
    let xml = r#"<root xmlns="http://default.com" xmlns:ns="http://ns.com"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(
        elem.namespace_declarations.iter().find(|(p, _)| p.is_empty()).map(|(_, u)| &**u),
        Some("http://default.com")
    );
    assert_eq!(
        elem.namespace_declarations.iter().find(|(p, _)| &**p == "ns").map(|(_, u)| &**u),
        Some("http://ns.com")
    );
}

// ─── xml: prefix (always bound) ─────────────────────────

#[test]
fn xml_prefix_always_available() {
    // The xml: prefix is always bound to http://www.w3.org/XML/1998/namespace
    let xml = r#"<root xml:lang="en"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    let attr = &elem.attributes[0];
    assert_eq!(
        attr.name.namespace_uri.as_deref(),
        Some("http://www.w3.org/XML/1998/namespace")
    );
    assert_eq!(attr.name.prefix.as_deref(), Some("xml"));
    assert_eq!(attr.name.local_name, "lang");
}

// ─── Non-namespace-aware mode ───────────────────────────

#[test]
fn non_namespace_aware_preserves_prefixes() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"><ns:child/></ns:root>"#;
    let parser = uppsala::Parser::with_namespace_aware(false);
    let doc = parser.parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    // Without namespace processing, the full name should be the raw tag name
    // and no namespace URI should be set
    assert!(elem.name.namespace_uri.is_none());
}

// ─── SAML-like namespace document ───────────────────────

#[test]
fn saml_like_namespaces() {
    let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion">
  <saml:Assertion>
    <saml:Subject>
      <saml:NameID>user@example.com</saml:NameID>
    </saml:Subject>
  </saml:Assertion>
</samlp:Response>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let root_elem = doc.element(root).unwrap();
    assert_eq!(root_elem.name.local_name, "Response");
    assert_eq!(
        root_elem.name.namespace_uri.as_deref(),
        Some("urn:oasis:names:tc:SAML:2.0:protocol")
    );

    let assertion =
        doc.get_elements_by_tag_name_ns("urn:oasis:names:tc:SAML:2.0:assertion", "Assertion");
    assert_eq!(assertion.len(), 1);

    let name_ids =
        doc.get_elements_by_tag_name_ns("urn:oasis:names:tc:SAML:2.0:assertion", "NameID");
    assert_eq!(name_ids.len(), 1);
    assert_eq!(doc.text_content_deep(name_ids[0]), "user@example.com");
}

// ─── Namespace error cases ──────────────────────────────

#[test]
fn error_undeclared_prefix() {
    let xml = "<ns:root/>";
    let result = uppsala::parse(xml);
    assert!(result.is_err(), "Undeclared prefix should be an error");
}

#[test]
fn error_undeclared_prefix_on_attribute() {
    let xml = r#"<root ns:attr="val"/>"#;
    let result = uppsala::parse(xml);
    assert!(
        result.is_err(),
        "Undeclared prefix on attribute should be an error"
    );
}
