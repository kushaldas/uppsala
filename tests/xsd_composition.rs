//! Regression tests for XSD schema composition (`xs:import`, `xs:include`,
//! `xs:redefine`) interacting with content models.
//!
//! Each test that needs sibling schema files writes them to a unique
//! tempdir and passes the schema path to `from_schema_with_base_path` so
//! `schemaLocation` resolution works. No external test fixture files.

use std::fs;
use std::path::PathBuf;

use uppsala::{parse, XsdValidator};

fn mkdir_unique(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "uppsala-test-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn validate(schema: &str, schema_path: &std::path::Path, instance: &str) -> Vec<String> {
    let schema_doc = parse(schema).expect("parse schema");
    let validator = XsdValidator::from_schema_with_base_path(&schema_doc, Some(schema_path))
        .expect("build validator");
    let doc = parse(instance).expect("parse instance");
    validator
        .validate(&doc)
        .into_iter()
        .map(|e| format!("{}", e))
        .collect()
}

/// Control case: same-namespace `xs:element ref="..."` inside an unbounded
/// choice in mixed content. This works correctly today and is included so
/// the cross-namespace regression below can be compared against a known-good
/// baseline.
#[test]
fn same_namespace_ref_in_unbounded_choice_mixed_content() {
    let dir = mkdir_unique("same-ns-choice");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:m="urn:test:m"
           targetNamespace="urn:test:m"
           elementFormDefault="qualified">
  <xs:element name="ref">
    <xs:complexType><xs:attribute name="term" type="xs:string"/></xs:complexType>
  </xs:element>
  <xs:element name="p">
    <xs:complexType mixed="true">
      <xs:choice minOccurs="0" maxOccurs="unbounded">
        <xs:element ref="m:ref"/>
        <xs:element name="b" type="xs:string"/>
      </xs:choice>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let schema_path = dir.join("schema.xsd");
    fs::write(&schema_path, schema).unwrap();

    let instance = r#"<m:p xmlns:m="urn:test:m">
Text <m:ref term="x"/> and <m:b>bold</m:b> and <m:ref term="y"/> more.
</m:p>"#;

    let errors = validate(schema, &schema_path, instance);
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "same-namespace ref in unbounded choice should validate, got: {:?}",
        errors
    );
}

/// Regression: when an `xs:element ref="foreign:name"` (resolved across an
/// `xs:import` boundary) appears inside an unbounded choice in mixed content,
/// validation incorrectly reports `Unexpected element ... after choice` for
/// the second and subsequent occurrences. This is the "cross-namespace ref
/// in unbounded choice" bug.
///
/// Schema layout:
///   inner.xsd — defines a global element `i:ref` in namespace `urn:test:inner`
///   outer.xsd — imports inner, declares `o:p` whose content model is
///               mixed + unbounded choice over `i:ref` and a local `b`.
///
/// Instance: `<o:p>` containing two `<i:ref/>` interleaved with text and a
/// `<o:b>`. By spec this is valid (choice is unbounded; mixed allows text).
#[test]
fn cross_namespace_ref_in_unbounded_choice_mixed_content() {
    let dir = mkdir_unique("cross-ns-choice");

    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified">
  <xs:element name="ref">
    <xs:complexType><xs:attribute name="term" type="xs:string"/></xs:complexType>
  </xs:element>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    let outer = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           xmlns:o="urn:test:outer"
           targetNamespace="urn:test:outer"
           elementFormDefault="qualified">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
  <xs:element name="p">
    <xs:complexType mixed="true">
      <xs:choice minOccurs="0" maxOccurs="unbounded">
        <xs:element ref="i:ref"/>
        <xs:element name="b" type="xs:string"/>
      </xs:choice>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let outer_path = dir.join("outer.xsd");
    fs::write(&outer_path, outer).unwrap();

    let instance = r#"<o:p xmlns:o="urn:test:outer" xmlns:i="urn:test:inner">
Text with <i:ref term="x"/> and <o:b>bold</o:b> and <i:ref term="y"/>.
</o:p>"#;

    let errors = validate(outer, &outer_path, instance);
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "cross-namespace ref in unbounded choice should validate, got: {:?}",
        errors
    );
}

/// Regression: `<xs:attribute ref="foreign:attr"/>` across an `xs:import`
/// boundary. Before the fix, the prefix was stripped and the lookup keyed
/// against the outer schema's targetNamespace, so the imported global
/// attribute was never found and the `use="required"` constraint wasn't
/// enforced.
#[test]
fn cross_namespace_attribute_ref_required() {
    let dir = mkdir_unique("cross-ns-attr");

    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified"
           attributeFormDefault="qualified">
  <xs:attribute name="lang" type="xs:string"/>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    let outer = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           xmlns:o="urn:test:outer"
           targetNamespace="urn:test:outer"
           elementFormDefault="qualified">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
  <xs:element name="p">
    <xs:complexType>
      <xs:attribute ref="i:lang" use="required"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let outer_path = dir.join("outer.xsd");
    fs::write(&outer_path, outer).unwrap();

    // Valid — attribute present.
    let ok_instance =
        r#"<o:p xmlns:o="urn:test:outer" xmlns:i="urn:test:inner" i:lang="en"/>"#;
    let errors = validate(outer, &outer_path, ok_instance);
    assert!(
        errors.is_empty(),
        "cross-namespace attribute ref should resolve, got: {:?}",
        errors
    );

    // Invalid — required foreign attribute missing. Pre-fix this would
    // ALSO have produced errors, but for the wrong reason (unresolved
    // local-namespace decl, not the real `use="required"` violation).
    let bad_instance = r#"<o:p xmlns:o="urn:test:outer"/>"#;
    let errors = validate(outer, &outer_path, bad_instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        !errors.is_empty(),
        "missing required cross-namespace attribute should fail validation"
    );
}

/// Regression: `<xs:attributeGroup ref="foreign:group"/>` across an
/// `xs:import` boundary. Pre-fix, the prefix was ignored and the lookup
/// keyed against the outer schema's targetNamespace, so the imported
/// group's attributes were silently dropped from the effective attribute
/// list — any required attributes declared in the group went unenforced.
#[test]
fn cross_namespace_attribute_group_ref() {
    let dir = mkdir_unique("cross-ns-ag");

    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified"
           attributeFormDefault="qualified">
  <xs:attributeGroup name="meta">
    <xs:attribute name="id" type="xs:string" use="required"/>
  </xs:attributeGroup>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    let outer = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           xmlns:o="urn:test:outer"
           targetNamespace="urn:test:outer"
           elementFormDefault="qualified">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
  <xs:element name="p">
    <xs:complexType>
      <xs:attributeGroup ref="i:meta"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let outer_path = dir.join("outer.xsd");
    fs::write(&outer_path, outer).unwrap();

    // Missing imported required attribute must fail.
    let bad_instance = r#"<o:p xmlns:o="urn:test:outer"/>"#;
    let errors = validate(outer, &outer_path, bad_instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        !errors.is_empty(),
        "cross-namespace attributeGroup ref should contribute its required \
         attributes to the effective attribute list; got no errors which \
         means the group was silently dropped"
    );
}

/// Negative: an undeclared prefix in a `ref=` attribute must no longer
/// silently rebind to the schema's targetNamespace. Pre-fix, a typo like
/// `ref="nobdy:foo"` would quietly resolve against the outer schema; this
/// test pins the new fail-closed behaviour (lookup misses, particle does
/// not match anything in the instance).
#[test]
fn undeclared_prefix_in_ref_fails_closed() {
    let dir = mkdir_unique("undeclared-prefix");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:m="urn:test:m"
           targetNamespace="urn:test:m"
           elementFormDefault="qualified">
  <xs:element name="p">
    <xs:complexType>
      <xs:sequence>
        <xs:element ref="nobdy:foo"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let schema_path = dir.join("schema.xsd");
    fs::write(&schema_path, schema).unwrap();

    // Instance that would have accidentally matched pre-fix (an element
    // `<m:foo/>` in targetNamespace) must now NOT match, because the ref
    // resolves to no-namespace.
    let instance = r#"<m:p xmlns:m="urn:test:m"><m:foo/></m:p>"#;
    let errors = validate(schema, &schema_path, instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        !errors.is_empty(),
        "undeclared-prefix ref must fail closed; instead the particle \
         silently matched an element in the wrong namespace"
    );
}
