//! Regression tests for XSD schema composition (`xs:import`, `xs:include`,
//! `xs:redefine`) interacting with content models.
//!
//! Each test that needs sibling schema files writes them to a unique
//! tempdir and passes the schema path to `from_schema_with_base_path` so
//! `schemaLocation` resolution works. No external test fixture files.

mod common;
use common::parse;

use std::fs;
use std::path::PathBuf;

use uppsala::XsdValidator;

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

/// Regression: `xs:import` must resolve `schemaLocation` when the caller passes
/// the schema *directory* as `base_path`, not just the schema *file*.
///
/// The public entry points hand a directory to `from_schema_with_base_path`:
/// the schema is supplied as a string (no file of its own), and the pyuppsala
/// `XsdValidator.from_file(schema_xml, base_path)` / etree
/// `XMLSchema(file=...)` facade passes `os.path.dirname(file)`. Composition
/// used to do `base_path.parent()` unconditionally, treating that directory as
/// a file and stripping one level, so every `xs:import`/`xs:include` silently
/// failed to resolve and *all* imported declarations (types **and** the global
/// element used to validate the instance root) went missing -- surfacing as
/// "No element declaration found for '<root>'". This test passes the directory
/// (as the real callers do) and asserts the imported global element resolves.
///
/// The sibling tests above pass the schema *file* path, whose `.parent()` is
/// the directory, so they validated correctly even with the bug and never
/// exercised this path.
#[test]
fn import_resolves_with_directory_base_path() {
    let dir = mkdir_unique("import-dir-base");

    // Imported schema declares a global element (and its type) in its own
    // namespace -- this is the element used to validate the instance root.
    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified">
  <xs:element name="Thing" type="i:ThingType"/>
  <xs:complexType name="ThingType">
    <xs:attribute name="id" type="xs:string"/>
  </xs:complexType>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    // Entry schema (different targetNamespace) only imports the inner one.
    let composite = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:aggregate" version="1.0">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
</xs:schema>"#;

    let instance = r#"<i:Thing xmlns:i="urn:test:inner" id="x"/>"#;

    // Pass the DIRECTORY as base_path (what the public callers do), NOT the
    // schema file path.
    let schema_doc = parse(composite).expect("parse composite schema");
    let validator = XsdValidator::from_schema_with_base_path(&schema_doc, Some(dir.as_path()))
        .expect("build validator from directory base_path");
    let doc = parse(instance).expect("parse instance");
    let errors: Vec<String> = validator
        .validate(&doc)
        .into_iter()
        .map(|e| format!("{e}"))
        .collect();
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "imported global element must resolve when base_path is the schema \
         directory, got: {errors:?}",
    );
}

/// Regression: a `base_path` directory that does not exist must fail closed,
/// not silently resolve `schemaLocation` against its (existing) parent.
///
/// The effective base directory is computed by reducing only a *known regular
/// file* to its parent; a missing/unreadable path is kept as the directory so
/// the canonicalize-or-reject guard fires. A `!is_dir()` test would instead
/// treat the missing directory as a file and fall back to its parent, which may
/// canonicalize successfully and re-open the contained-resolution hole.
#[test]
fn missing_base_directory_fails_closed() {
    let parent = mkdir_unique("missing-base-parent");
    let missing = parent.join("does-not-exist");

    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:agg" version="1.0">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
</xs:schema>"#;

    let schema_doc = parse(schema).expect("parse schema");
    let built = XsdValidator::from_schema_with_base_path(&schema_doc, Some(missing.as_path()));
    fs::remove_dir_all(&parent).ok();

    assert!(
        built.is_err(),
        "a missing base directory must fail closed (canonicalize error), not \
         resolve imports against its parent",
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
    let ok_instance = r#"<o:p xmlns:o="urn:test:outer" xmlns:i="urn:test:inner" i:lang="en"/>"#;
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

/// F-10: a schema that uses `xs:include schemaLocation="/etc/passwd"` or
/// any absolute path outside its own base directory must be rejected.
/// Before the fix, the loader `std::fs::read_to_string`d the path verbatim.
#[test]
fn absolute_schema_location_is_rejected() {
    let dir = std::env::temp_dir().join(format!(
        "uppsala-f10-abs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&dir).unwrap();

    // Put the "secret" schema OUTSIDE the schema's base directory.
    let outside =
        std::env::temp_dir().join(format!("uppsala-f10-outside-{}.xsd", std::process::id()));
    fs::write(
        &outside,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="leaked" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    // The schema we hand the validator tries to include it by absolute path.
    let schema_path = dir.join("evil.xsd");
    let schema = format!(
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="{}"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#,
        outside.display()
    );
    fs::write(&schema_path, &schema).unwrap();

    let schema_doc = parse(&schema).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&schema_path));

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&outside);

    let err = built
        .err()
        .expect("absolute schemaLocation must be rejected");
    let msg = format!("{}", err);
    assert!(
        msg.contains("escapes the schema's base directory")
            || msg.contains("absolute URI not supported"),
        "expected containment error, got: {}",
        msg
    );
}

/// F-10: `schemaLocation="../../../../etc/passwd"` that canonicalizes to
/// a path outside the schema's directory must be rejected.
#[test]
fn parent_traversal_schema_location_is_rejected() {
    let dir = mkdir_unique("f10-traversal");
    let nested = dir.join("sub");
    fs::create_dir_all(&nested).unwrap();

    // "Secret" file lives in `dir` (one level up from `nested`).
    let secret = dir.join("secret.xsd");
    fs::write(
        &secret,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="secret" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    // The schema's base dir is `nested/`; the include escapes upward.
    let schema_path = nested.join("evil.xsd");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="../secret.xsd"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#;
    fs::write(&schema_path, schema).unwrap();

    let schema_doc = parse(schema).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&schema_path));
    fs::remove_dir_all(&dir).ok();

    assert!(
        built.is_err(),
        "`../` traversal out of schema base dir must be rejected"
    );
}

/// F-10 positive control: an include within the same directory works fine.
#[test]
fn same_directory_include_is_allowed() {
    let dir = mkdir_unique("f10-same-dir");

    let inner_path = dir.join("inner.xsd");
    fs::write(
        &inner_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="inner_leaf" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    let schema_path = dir.join("outer.xsd");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="inner.xsd"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#;
    fs::write(&schema_path, schema).unwrap();

    let schema_doc = parse(schema).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&schema_path));
    fs::remove_dir_all(&dir).ok();

    let _validator = built.expect("same-dir include must be allowed");
}

/// F-11: a.xsd includes b.xsd which includes a.xsd. Before the fix this
/// recursed until the thread stack overflowed. With the visited-paths
/// set the second `xs:include` is short-circuited and the build succeeds.
#[test]
fn circular_include_terminates() {
    let dir = mkdir_unique("f11-circular");

    let a_path = dir.join("a.xsd");
    let b_path = dir.join("b.xsd");
    fs::write(
        &a_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="b.xsd"/>
  <xs:element name="a_leaf" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();
    fs::write(
        &b_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="a.xsd"/>
  <xs:element name="b_leaf" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    let schema_src = fs::read_to_string(&a_path).unwrap();
    let schema_doc = parse(&schema_src).unwrap();
    // Without the visited set this call recurses until SIGABRT.
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&a_path));
    fs::remove_dir_all(&dir).ok();
    assert!(
        built.is_ok(),
        "circular include must terminate cleanly, got: {:?}",
        built.err()
    );
}

/// F-11: include-nesting past `MAX_INCLUDE_DEPTH` errors with a clear
/// message instead of exhausting the stack.
#[test]
fn deep_include_chain_rejected() {
    let dir = mkdir_unique("f11-deep");
    // 20 schemas each including the next one. Exceeds MAX_INCLUDE_DEPTH = 16.
    let n = 20usize;
    for i in 0..n {
        let next = i + 1;
        let body = if next < n {
            format!(r#"<xs:include schemaLocation="s{}.xsd"/>"#, next)
        } else {
            String::new()
        };
        let path = dir.join(format!("s{}.xsd", i));
        fs::write(
            &path,
            format!(
                r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  {}
  <xs:element name="e{}" type="xs:string"/>
</xs:schema>"#,
                body, i
            ),
        )
        .unwrap();
    }

    let entry = dir.join("s0.xsd");
    let schema_src = fs::read_to_string(&entry).unwrap();
    let schema_doc = parse(&schema_src).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&entry));
    fs::remove_dir_all(&dir).ok();

    let err = built.err().expect("20-deep include chain must be rejected");
    assert!(
        format!("{}", err).contains("nesting exceeds maximum depth"),
        "expected include-depth error, got: {}",
        err
    );
}

/// When the caller supplies a `base_path` whose parent directory cannot
/// be canonicalized, the composition layer must fail closed rather than
/// silently dropping the F-10 containment check. Pre-fix, the
/// containment-anchor was `base_dir.canonicalize().ok()`, so any
/// canonicalize failure (missing dir, permission denied, race) collapsed
/// `canonical_base` to `None` and left the include path unchecked.
#[test]
fn uncanonicalizable_base_path_fails_closed() {
    // Construct a base_path whose parent directory does NOT exist.
    let bogus_parent = std::env::temp_dir().join(format!(
        "uppsala-nonexistent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let bogus_schema = bogus_parent.join("schema.xsd");
    assert!(
        !bogus_parent.exists(),
        "test precondition: parent must not exist"
    );

    // Schema body itself is benign; it just has to reach
    // process_schema_composition (which fires on any xs:include /
    // xs:redefine / xs:import — even one with a missing target).
    let schema_src = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="other.xsd"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#;
    let schema_doc = parse(schema_src).expect("parse");
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&bogus_schema));

    let err = built
        .err()
        .expect("uncanonicalizable base directory must fail closed");
    assert!(
        format!("{}", err).contains("canonicalize schema base directory"),
        "expected canonicalize error, got: {}",
        err
    );
}

/// Regression: `reresolve_types_after_redefine` used to rebuild a complex
/// type's attribute list from *only* its `attributeGroup` refs after any
/// `xs:redefine` elsewhere in the schema, discarding attributes declared
/// directly on the type (bare `<xs:attribute>` children) that coexist with
/// an `attributeGroup ref`. This is a very common real-world pattern (a type
/// with its own required attributes plus a generic, wildcard-based
/// extensibility attribute group). The fix preserves directly-declared
/// attributes by tracking them separately (`own_attributes`) and merging
/// them back in during re-resolution instead of replacing the list outright.
///
/// Schema layout:
///   base.xsd — declares attributeGroup "Extensible" (an `##other` wildcard)
///     and complexType "Widget" with both a bare attribute `id` and an
///     `attributeGroup ref="Extensible"`.
///   wrapper.xsd — `xs:include`s base.xsd, then `xs:redefine`s base.xsd
///     again to redefine a type entirely unrelated to Widget. Per the buggy
///     behavior, this redefine alone was enough to trigger a schema-wide
///     re-resolution pass that wiped Widget's `id` attribute.
#[test]
fn redefine_reresolution_preserves_directly_declared_attributes() {
    let dir = mkdir_unique("redefine-attr-preserve");

    let base_path = dir.join("base.xsd");
    fs::write(
        &base_path,
        r###"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:redefine-attr"
           xmlns="urn:test:redefine-attr"
           elementFormDefault="qualified">
  <xs:attributeGroup name="Extensible">
    <xs:anyAttribute namespace="##other" processContents="lax"/>
  </xs:attributeGroup>
  <xs:complexType name="Widget">
    <xs:sequence>
      <xs:element name="Name" type="xs:string"/>
    </xs:sequence>
    <xs:attribute name="id" type="xs:string"/>
    <xs:attributeGroup ref="Extensible"/>
  </xs:complexType>
  <xs:complexType name="Unrelated">
    <xs:sequence>
      <xs:element name="Placeholder" type="xs:string" minOccurs="0"/>
    </xs:sequence>
  </xs:complexType>
  <xs:element name="Root" type="Widget"/>
</xs:schema>"###,
    )
    .unwrap();

    let wrapper_path = dir.join("wrapper.xsd");
    let wrapper_schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:redefine-attr"
           xmlns="urn:test:redefine-attr"
           elementFormDefault="qualified">
  <xs:redefine schemaLocation="base.xsd">
    <xs:complexType name="Unrelated">
      <xs:complexContent>
        <xs:restriction base="Unrelated">
          <xs:sequence>
            <xs:element name="Placeholder" type="xs:string" minOccurs="0"/>
          </xs:sequence>
        </xs:restriction>
      </xs:complexContent>
    </xs:complexType>
  </xs:redefine>
</xs:schema>"#;
    fs::write(&wrapper_path, wrapper_schema).unwrap();

    let instance = r#"<Root xmlns="urn:test:redefine-attr" id="w1">
  <Name>Widget One</Name>
</Root>"#;

    let errors = validate(wrapper_schema, &wrapper_path, instance);
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "directly-declared attribute 'id' should remain valid after an \
         unrelated xs:redefine elsewhere in the schema, got: {:?}",
        errors
    );
}

/// Regression test: an `xs:attributeGroup ref` nested inside a
/// `complexContent`/`simpleContent` extension or restriction (as opposed to
/// one declared directly under `complexType`) must also be tracked in
/// `attribute_group_refs` so `reresolve_types_after_redefine` picks it up
/// when the referenced group is redefined.
///
/// Schema layout:
///   base.xsd — declares attributeGroup "Extensible" (a required `legacy`
///     attribute) and complexType "Widget", which extends "Base" via
///     `complexContent`/`extension`, mixing a bare attribute `id` with an
///     `attributeGroup ref="Extensible"` inside the extension (the second,
///     previously-unrecorded parse path).
///   wrapper.xsd — `xs:redefine`s base.xsd, replacing "Extensible" so it
///     requires a `token` attribute instead of `legacy`.
///
/// Without recording the nested ref, Widget is never re-resolved after the
/// redefine, so it keeps requiring the stale `legacy` attribute and rejects
/// `token` as disallowed (no wildcard). With the fix, Widget picks up the
/// redefined group's `token` requirement.
#[test]
fn nested_attribute_group_ref_in_extension_is_reresolved_after_redefine() {
    let dir = mkdir_unique("redefine-nested-ag");

    let base_path = dir.join("base.xsd");
    fs::write(
        &base_path,
        r###"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:redefine-nested-ag"
           xmlns="urn:test:redefine-nested-ag"
           elementFormDefault="qualified">
  <xs:attributeGroup name="Extensible">
    <xs:attribute name="legacy" type="xs:string" use="required"/>
  </xs:attributeGroup>
  <xs:complexType name="Base">
    <xs:sequence>
      <xs:element name="Name" type="xs:string"/>
    </xs:sequence>
  </xs:complexType>
  <xs:complexType name="Widget">
    <xs:complexContent>
      <xs:extension base="Base">
        <xs:attribute name="id" type="xs:string" use="required"/>
        <xs:attributeGroup ref="Extensible"/>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
  <xs:element name="Root" type="Widget"/>
</xs:schema>"###,
    )
    .unwrap();

    let wrapper_path = dir.join("wrapper.xsd");
    let wrapper_schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:redefine-nested-ag"
           xmlns="urn:test:redefine-nested-ag"
           elementFormDefault="qualified">
  <xs:redefine schemaLocation="base.xsd">
    <xs:attributeGroup name="Extensible">
      <xs:attribute name="token" type="xs:string" use="required"/>
    </xs:attributeGroup>
  </xs:redefine>
</xs:schema>"#;
    fs::write(&wrapper_path, wrapper_schema).unwrap();

    let valid_instance = r#"<Root xmlns="urn:test:redefine-nested-ag" id="w1" token="abc">
  <Name>Widget One</Name>
</Root>"#;
    let errors_valid = validate(wrapper_schema, &wrapper_path, valid_instance);

    let stale_instance = r#"<Root xmlns="urn:test:redefine-nested-ag" id="w1" legacy="old">
  <Name>Widget One</Name>
</Root>"#;
    let errors_stale = validate(wrapper_schema, &wrapper_path, stale_instance);

    fs::remove_dir_all(&dir).ok();

    assert!(
        errors_valid.is_empty(),
        "Widget should accept the redefined group's 'token' attribute and no \
         longer require the stale 'legacy' attribute, got: {:?}",
        errors_valid
    );

    assert!(
        !errors_stale.is_empty(),
        "Widget should reject the stale pre-redefine 'legacy' attribute and \
         report the missing required 'token' attribute, got: {:?}",
        errors_stale
    );
}

/// Regression test: a chameleon-included (no-namespace) module's complex
/// type must have both `own_attributes` and `attribute_group_refs` re-keyed
/// into the including schema's target namespace, not just `attributes`.
/// Otherwise, once *any* `xs:redefine` in the composed schema triggers
/// `reresolve_types_after_redefine`, the type is rebuilt from the
/// still-unqualified `own_attributes` and its `attributeGroup` ref lookup
/// (keyed with `None` namespace) misses the group, which was itself
/// re-keyed to the target namespace when merged — silently dropping the
/// group's attributes and de-qualifying the directly-declared one.
///
/// Schema layout:
///   module.xsd — no `targetNamespace`; declares attributeGroup
///     "Extensible" (attribute `ext`), complexType "Widget" (qualified bare
///     attribute `id` plus `attributeGroup ref="Extensible"`), and an
///     unrelated complexType "Unrelated".
///   wrapper.xsd — `xs:redefine`s module.xsd, chameleon-including it into
///     `urn:cham-redefine-ag`, and self-referencingly redefines "Unrelated"
///     (unconnected to Widget) purely to trigger the schema-wide
///     re-resolution pass.
#[test]
fn chameleon_include_attribute_group_ref_survives_unrelated_redefine() {
    let dir = mkdir_unique("chameleon-redefine-ag");

    let module_path = dir.join("module.xsd");
    fs::write(
        &module_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           elementFormDefault="qualified"
           attributeFormDefault="qualified">
  <xs:attributeGroup name="Extensible">
    <xs:attribute name="ext" type="xs:string"/>
  </xs:attributeGroup>
  <xs:complexType name="Widget">
    <xs:sequence>
      <xs:element name="Name" type="xs:string"/>
    </xs:sequence>
    <xs:attribute name="id" type="xs:string" use="required"/>
    <xs:attributeGroup ref="Extensible"/>
  </xs:complexType>
  <xs:complexType name="Unrelated">
    <xs:sequence>
      <xs:element name="Placeholder" type="xs:string" minOccurs="0"/>
    </xs:sequence>
  </xs:complexType>
  <xs:element name="Root" type="Widget"/>
</xs:schema>"#,
    )
    .unwrap();

    let wrapper_path = dir.join("wrapper.xsd");
    let wrapper_schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:cham-redefine-ag"
           xmlns="urn:cham-redefine-ag"
           elementFormDefault="qualified">
  <xs:redefine schemaLocation="module.xsd">
    <xs:complexType name="Unrelated">
      <xs:complexContent>
        <xs:restriction base="Unrelated">
          <xs:sequence>
            <xs:element name="Placeholder" type="xs:string" minOccurs="0"/>
          </xs:sequence>
        </xs:restriction>
      </xs:complexContent>
    </xs:complexType>
  </xs:redefine>
</xs:schema>"#;
    fs::write(&wrapper_path, wrapper_schema).unwrap();

    let instance = r#"<t:Root xmlns:t="urn:cham-redefine-ag" t:id="w1" t:ext="extra">
  <t:Name>Widget One</t:Name>
</t:Root>"#;

    let errors = validate(wrapper_schema, &wrapper_path, instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        errors.is_empty(),
        "chameleon-included Widget's directly-declared 'id' attribute and \
         its attributeGroup-contributed 'ext' attribute must both remain \
         valid (namespace-qualified and resolvable) after an unrelated \
         xs:redefine elsewhere in the schema, got: {:?}",
        errors
    );
}
