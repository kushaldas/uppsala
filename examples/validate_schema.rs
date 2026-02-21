//! Validate XML documents against XSD schemas.
//!
//! Demonstrates basic schema validation, type checking, and error reporting.
//!
//! Run with: `cargo run --example validate_schema`

use uppsala::{parse, XsdValidator};

fn main() {
    // ── Example 1: Simple type validation ──
    println!("=== Example 1: Simple Types ===\n");

    let schema = r#"
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="age" type="xs:positiveInteger"/>
</xs:schema>
"#;

    let valid_doc = "<age>25</age>";
    let invalid_doc = "<age>-5</age>";

    let schema_doc = parse(schema).expect("Failed to parse schema");
    let validator = XsdValidator::from_schema(&schema_doc).expect("Failed to build validator");

    let doc = parse(valid_doc).unwrap();
    let errors = validator.validate(&doc);
    println!("'{}' => {} errors", valid_doc, errors.len());

    let doc = parse(invalid_doc).unwrap();
    let errors = validator.validate(&doc);
    println!("'{}' => {} error(s):", invalid_doc, errors.len());
    for e in &errors {
        println!("  {}", e);
    }
    println!();

    // ── Example 2: Complex type with sequence ──
    println!("=== Example 2: Complex Type with Sequence ===\n");

    let schema = r#"
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="person">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="name" type="xs:string"/>
        <xs:element name="email" type="xs:string"/>
        <xs:element name="age" type="xs:positiveInteger"/>
      </xs:sequence>
      <xs:attribute name="id" type="xs:ID" use="required"/>
    </xs:complexType>
  </xs:element>
</xs:schema>
"#;

    let valid_instance = r#"
<person id="p1">
  <name>Alice</name>
  <email>alice@example.com</email>
  <age>30</age>
</person>
"#;

    let invalid_instance = r#"
<person id="p1">
  <email>alice@example.com</email>
  <name>Alice</name>
  <age>30</age>
</person>
"#;

    let schema_doc = parse(schema).expect("Failed to parse schema");
    let validator = XsdValidator::from_schema(&schema_doc).expect("Failed to build validator");

    let doc = parse(valid_instance).unwrap();
    let errors = validator.validate(&doc);
    println!("Valid person (correct order) => {} errors", errors.len());

    let doc = parse(invalid_instance).unwrap();
    let errors = validator.validate(&doc);
    println!(
        "Invalid person (wrong element order) => {} error(s):",
        errors.len()
    );
    for e in &errors {
        println!("  {}", e);
    }
    println!();

    // ── Example 3: Facet restrictions ──
    println!("=== Example 3: Facet Restrictions ===\n");

    let schema = r#"
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="ZipCode">
    <xs:restriction base="xs:string">
      <xs:pattern value="\d{5}(-\d{4})?"/>
    </xs:restriction>
  </xs:simpleType>

  <xs:simpleType name="Rating">
    <xs:restriction base="xs:integer">
      <xs:minInclusive value="1"/>
      <xs:maxInclusive value="5"/>
    </xs:restriction>
  </xs:simpleType>

  <xs:element name="address">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="zip" type="ZipCode"/>
        <xs:element name="rating" type="Rating"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>
"#;

    let schema_doc = parse(schema).expect("Failed to parse schema");
    let validator = XsdValidator::from_schema(&schema_doc).expect("Failed to build validator");

    let test_cases = [
        (
            "<address><zip>12345</zip><rating>3</rating></address>",
            "valid zip + valid rating",
        ),
        (
            "<address><zip>12345-6789</zip><rating>5</rating></address>",
            "valid zip+4 + max rating",
        ),
        (
            "<address><zip>ABCDE</zip><rating>3</rating></address>",
            "invalid zip (letters)",
        ),
        (
            "<address><zip>12345</zip><rating>6</rating></address>",
            "valid zip + rating out of range",
        ),
    ];

    for (xml, description) in &test_cases {
        let doc = parse(xml).unwrap();
        let errors = validator.validate(&doc);
        if errors.is_empty() {
            println!("  [PASS] {}", description);
        } else {
            println!("  [FAIL] {}", description);
            for e in &errors {
                println!("         {}", e);
            }
        }
    }
    println!();

    // ── Example 4: Enumeration ──
    println!("=== Example 4: Enumeration Types ===\n");

    let schema = r#"
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Color">
    <xs:restriction base="xs:string">
      <xs:enumeration value="red"/>
      <xs:enumeration value="green"/>
      <xs:enumeration value="blue"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:element name="color" type="Color"/>
</xs:schema>
"#;

    let schema_doc = parse(schema).expect("Failed to parse schema");
    let validator = XsdValidator::from_schema(&schema_doc).expect("Failed to build validator");

    for value in &["red", "green", "blue", "yellow"] {
        let xml = format!("<color>{}</color>", value);
        let doc = parse(&xml).unwrap();
        let errors = validator.validate(&doc);
        if errors.is_empty() {
            println!("  '{}' => valid", value);
        } else {
            println!("  '{}' => invalid: {}", value, errors[0]);
        }
    }
}
