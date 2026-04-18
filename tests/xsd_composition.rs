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
