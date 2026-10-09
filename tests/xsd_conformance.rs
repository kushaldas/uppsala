//! Integration tests for XSD 1.1 validation.

mod common;
use common::parse;

// ─── Simple type validation ─────────────────────────────

fn validate_xml_against_xsd(xml: &str, xsd: &str) -> Result<(), String> {
    let schema_doc = parse(xsd).map_err(|e| format!("Schema parse error: {}", e))?;
    let validator = uppsala::XsdValidator::from_schema(&schema_doc)
        .map_err(|e| format!("Schema load error: {}", e))?;
    let doc = parse(xml).map_err(|e| format!("XML parse error: {}", e))?;
    let errors = validator.validate(&doc);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors
            .iter()
            .map(|e| format!("{}", e))
            .collect::<Vec<_>>()
            .join("; "))
    }
}

#[test]
fn xsd_string_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="name" type="xs:string"/>
</xs:schema>"#;
    let xml = "<name>John Doe</name>";
    assert!(validate_xml_against_xsd(xml, xsd).is_ok());
}

#[test]
fn xsd_integer_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="count" type="xs:integer"/>
</xs:schema>"#;
    let xml = "<count>42</count>";
    assert!(validate_xml_against_xsd(xml, xsd).is_ok());
}

#[test]
fn xsd_integer_invalid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="count" type="xs:integer"/>
</xs:schema>"#;
    let xml = "<count>not_a_number</count>";
    assert!(validate_xml_against_xsd(xml, xsd).is_err());
}

#[test]
fn xsd_boolean_true() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="flag" type="xs:boolean"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<flag>true</flag>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<flag>1</flag>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<flag>false</flag>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<flag>0</flag>", xsd).is_ok());
}

#[test]
fn xsd_boolean_invalid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="flag" type="xs:boolean"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<flag>yes</flag>", xsd).is_err());
}

#[test]
fn xsd_decimal_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="price" type="xs:decimal"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<price>19.99</price>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<price>-3.14</price>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<price>42</price>", xsd).is_ok());
}

#[test]
fn xsd_decimal_invalid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="price" type="xs:decimal"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<price>abc</price>", xsd).is_err());
}

#[test]
fn xsd_float_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="value" type="xs:float"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<value>1.5e2</value>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<value>-0.5</value>", xsd).is_ok());
}

#[test]
fn xsd_double_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="value" type="xs:double"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<value>3.14159</value>", xsd).is_ok());
}

// ─── Integer subtypes ───────────────────────────────────

#[test]
fn xsd_long_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:long"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>9223372036854775807</val>", xsd).is_ok());
}

#[test]
fn xsd_int_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:int"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>2147483647</val>", xsd).is_ok());
}

#[test]
fn xsd_short_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:short"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>32767</val>", xsd).is_ok());
}

#[test]
fn xsd_byte_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:byte"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>127</val>", xsd).is_ok());
}

#[test]
fn xsd_unsigned_int_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:unsignedInt"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>4294967295</val>", xsd).is_ok());
}

#[test]
fn xsd_non_negative_integer_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:nonNegativeInteger"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>0</val>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<val>999</val>", xsd).is_ok());
}

#[test]
fn xsd_non_negative_integer_invalid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:nonNegativeInteger"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>-1</val>", xsd).is_err());
}

#[test]
fn xsd_positive_integer_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:positiveInteger"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>1</val>", xsd).is_ok());
}

#[test]
fn xsd_positive_integer_invalid_zero() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val" type="xs:positiveInteger"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>0</val>", xsd).is_err());
}

// ─── Complex types ──────────────────────────────────────

#[test]
fn xsd_complex_type_sequence() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="person">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="name" type="xs:string"/>
        <xs:element name="age" type="xs:integer"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let xml = "<person><name>Alice</name><age>30</age></person>";
    assert!(validate_xml_against_xsd(xml, xsd).is_ok());
}

#[test]
fn xsd_complex_type_sequence_wrong_order() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="person">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="name" type="xs:string"/>
        <xs:element name="age" type="xs:integer"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let xml = "<person><age>30</age><name>Alice</name></person>";
    assert!(validate_xml_against_xsd(xml, xsd).is_err());
}

#[test]
fn xsd_complex_type_sequence_missing_element() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="person">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="name" type="xs:string"/>
        <xs:element name="age" type="xs:integer"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let xml = "<person><name>Alice</name></person>";
    assert!(validate_xml_against_xsd(xml, xsd).is_err());
}

// ─── Attributes in schema ───────────────────────────────

#[test]
fn xsd_required_attribute_present() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="item">
    <xs:complexType>
      <xs:simpleContent>
        <xs:extension base="xs:string">
          <xs:attribute name="id" type="xs:string" use="required"/>
        </xs:extension>
      </xs:simpleContent>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let xml = r#"<item id="123">content</item>"#;
    assert!(validate_xml_against_xsd(xml, xsd).is_ok());
}

#[test]
fn xsd_required_attribute_missing() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="item">
    <xs:complexType>
      <xs:simpleContent>
        <xs:extension base="xs:string">
          <xs:attribute name="id" type="xs:string" use="required"/>
        </xs:extension>
      </xs:simpleContent>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let xml = "<item>content</item>";
    assert!(validate_xml_against_xsd(xml, xsd).is_err());
}

// ─── Facets ─────────────────────────────────────────────

#[test]
fn xsd_min_max_inclusive() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="score">
    <xs:simpleType>
      <xs:restriction base="xs:integer">
        <xs:minInclusive value="0"/>
        <xs:maxInclusive value="100"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<score>50</score>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<score>0</score>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<score>100</score>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<score>-1</score>", xsd).is_err());
    assert!(validate_xml_against_xsd("<score>101</score>", xsd).is_err());
}

#[test]
fn xsd_min_max_exclusive() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="score">
    <xs:simpleType>
      <xs:restriction base="xs:integer">
        <xs:minExclusive value="0"/>
        <xs:maxExclusive value="100"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<score>50</score>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<score>1</score>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<score>99</score>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<score>0</score>", xsd).is_err());
    assert!(validate_xml_against_xsd("<score>100</score>", xsd).is_err());
}

#[test]
fn xsd_enumeration() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="color">
    <xs:simpleType>
      <xs:restriction base="xs:string">
        <xs:enumeration value="red"/>
        <xs:enumeration value="green"/>
        <xs:enumeration value="blue"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<color>red</color>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<color>green</color>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<color>blue</color>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<color>yellow</color>", xsd).is_err());
}

#[test]
fn xsd_min_length() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="name">
    <xs:simpleType>
      <xs:restriction base="xs:string">
        <xs:minLength value="3"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<name>abc</name>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<name>ab</name>", xsd).is_err());
}

#[test]
fn xsd_max_length() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="code">
    <xs:simpleType>
      <xs:restriction base="xs:string">
        <xs:maxLength value="5"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<code>abc</code>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<code>abcdef</code>", xsd).is_err());
}

#[test]
fn xsd_exact_length() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="zip">
    <xs:simpleType>
      <xs:restriction base="xs:string">
        <xs:length value="5"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<zip>12345</zip>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<zip>1234</zip>", xsd).is_err());
    assert!(validate_xml_against_xsd("<zip>123456</zip>", xsd).is_err());
}

// XSD 1.0 Part 2 4.3.1-4.3.3: for string and the types derived from it, and
// for anyURI, length is measured in characters (code points), not in UTF-8
// bytes. Each table row is (repeat count, expected valid) against a limit of 4.

const LENGTH_LIMIT: usize = 4;

fn length_facet_schema(base: &str, facet: &str, limit: usize) -> String {
    format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="e">
    <xs:simpleType>
      <xs:restriction base="{}"><xs:{} value="{}"/></xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#,
        base, facet, limit
    )
}

/// Check `facet` at `LENGTH_LIMIT` on xs:string for values of 3, 4 and 5
/// repetitions of each character in `chars`.
fn check_length_facet(facet: &str, chars: &[char], expected: [(usize, bool); 3]) {
    let xsd = length_facet_schema("xs:string", facet, LENGTH_LIMIT);
    for ch in chars {
        for (count, valid) in expected {
            let value: String = std::iter::repeat(*ch).take(count).collect();
            let result = validate_xml_against_xsd(&format!("<e>{}</e>", value), &xsd);
            assert_eq!(
                result.is_ok(),
                valid,
                "{} {}: {} x {:?} ({} bytes): {:?}",
                facet,
                LENGTH_LIMIT,
                count,
                ch,
                value.len(),
                result
            );
        }
    }
}

/// Two-, three- and four-byte characters in UTF-8.
const MULTIBYTE: [char; 3] = ['\u{142}', '\u{20AC}', '\u{1F600}'];

#[test]
fn xsd_length_counts_characters_not_bytes() {
    check_length_facet("length", &MULTIBYTE, [(3, false), (4, true), (5, false)]);
}

#[test]
fn xsd_min_length_counts_characters_not_bytes() {
    check_length_facet("minLength", &MULTIBYTE, [(3, false), (4, true), (5, true)]);
}

#[test]
fn xsd_max_length_counts_characters_not_bytes() {
    check_length_facet("maxLength", &MULTIBYTE, [(3, true), (4, true), (5, false)]);
}

#[test]
fn xsd_length_facets_on_ascii_are_unchanged() {
    check_length_facet("length", &['a'], [(3, false), (4, true), (5, false)]);
    check_length_facet("minLength", &['a'], [(3, false), (4, true), (5, true)]);
    check_length_facet("maxLength", &['a'], [(3, true), (4, true), (5, false)]);
}

#[test]
fn xsd_length_facets_count_characters_through_a_derived_string_type() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Base">
    <xs:restriction base="xs:string"/>
  </xs:simpleType>
  <xs:simpleType name="Derived">
    <xs:restriction base="Base">
      <xs:minLength value="2"/>
      <xs:maxLength value="5"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:element name="e" type="Derived"/>
</xs:schema>"#;
    for (value, valid) in [
        ("\u{20AC}", false),
        ("\u{20AC}\u{20AC}", true),
        ("\u{20AC}\u{20AC}\u{20AC}\u{20AC}\u{20AC}", true),
        ("\u{20AC}\u{20AC}\u{20AC}\u{20AC}\u{20AC}\u{20AC}", false),
    ] {
        let result = validate_xml_against_xsd(&format!("<e>{}</e>", value), xsd);
        assert_eq!(result.is_ok(), valid, "{:?}: {:?}", value, result);
    }
}

#[test]
fn xsd_length_facets_measure_the_whitespace_normalized_value() {
    // xs:token collapses whitespace first: "  \u{142} \u{142}  " is 3 characters.
    let xsd = length_facet_schema("xs:token", "maxLength", 3);
    for (value, valid) in [
        ("  \u{142} \u{142}  ", true),
        ("\u{142}  \u{142}\u{142}", false),
    ] {
        let result = validate_xml_against_xsd(&format!("<e>{}</e>", value), &xsd);
        assert_eq!(result.is_ok(), valid, "{:?}: {:?}", value, result);
    }
}

#[test]
fn xsd_any_uri_length_counts_characters() {
    // "urn:x:" plus three two-byte characters: 9 characters, 12 bytes.
    let xsd = length_facet_schema("xs:anyURI", "length", 9);
    for (value, valid) in [
        ("urn:x:\u{142}\u{142}", false),
        ("urn:x:\u{142}\u{142}\u{142}", true),
        ("urn:x:\u{142}\u{142}\u{142}\u{142}", false),
    ] {
        let result = validate_xml_against_xsd(&format!("<e>{}</e>", value), &xsd);
        assert_eq!(result.is_ok(), valid, "{:?}: {:?}", value, result);
    }
}

#[test]
fn xsd_qname_length_counts_characters_of_uri_and_local_name() {
    // The namespace "urn:" plus three two-byte characters (7) and the local
    // name "ab" (2) are 9 characters; in bytes they would be 12.
    let xsd = length_facet_schema("xs:QName", "maxLength", 9);
    for (value, valid) in [("p:ab", true), ("p:abc", false)] {
        let xml = format!(
            r#"<e xmlns:p="urn:{}">{}</e>"#,
            "\u{142}\u{142}\u{142}", value
        );
        let result = validate_xml_against_xsd(&xml, &xsd);
        assert_eq!(result.is_ok(), valid, "{:?}: {:?}", value, result);
    }
    // An unbound prefix is refused by the QName check itself, whatever its
    // length.
    let result = validate_xml_against_xsd("<e>q:ab</e>", &xsd);
    assert!(
        result.as_ref().is_err_and(|e| e.contains("not bound")),
        "{:?}",
        result
    );
}

#[test]
fn xsd_total_digits() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val">
    <xs:simpleType>
      <xs:restriction base="xs:decimal">
        <xs:totalDigits value="5"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>12345</val>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<val>123.45</val>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<val>123456</val>", xsd).is_err());
}

#[test]
fn xsd_fraction_digits() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="val">
    <xs:simpleType>
      <xs:restriction base="xs:decimal">
        <xs:fractionDigits value="2"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<val>12.34</val>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<val>12.345</val>", xsd).is_err());
}

// ─── Date/time types ────────────────────────────────────

#[test]
fn xsd_date_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="d" type="xs:date"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<d>2024-01-15</d>", xsd).is_ok());
}

#[test]
fn xsd_date_invalid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="d" type="xs:date"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<d>not-a-date</d>", xsd).is_err());
}

#[test]
fn xsd_datetime_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="dt" type="xs:dateTime"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<dt>2024-01-15T10:30:00</dt>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<dt>2024-01-15T10:30:00Z</dt>", xsd).is_ok());
}

#[test]
fn xsd_time_valid() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="t" type="xs:time"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<t>10:30:00</t>", xsd).is_ok());
}

// ─── anyURI type ────────────────────────────────────────

#[test]
fn xsd_any_uri() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="url" type="xs:anyURI"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<url>http://example.com</url>", xsd).is_ok());
}

// ─── Wrong root element ─────────────────────────────────

#[test]
fn xsd_wrong_root_element() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="expected" type="xs:string"/>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<unexpected>data</unexpected>", xsd).is_err());
}

// ─── Nested complex types ───────────────────────────────

#[test]
fn xsd_nested_complex_types() {
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="order">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="customer">
          <xs:complexType>
            <xs:sequence>
              <xs:element name="name" type="xs:string"/>
            </xs:sequence>
          </xs:complexType>
        </xs:element>
        <xs:element name="total" type="xs:decimal"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let xml = "<order><customer><name>Bob</name></customer><total>99.95</total></order>";
    assert!(validate_xml_against_xsd(xml, xsd).is_ok());
}

// ─── List-type inheritance (issue #12) ─────────────────────────────

#[test]
fn xsd_list_inheritance() {
    // An inline <xs:simpleType> restricting a named list type must inherit
    // `is_list` from the base, so a `length` facet counts list *items*, not
    // characters. Regression test for issue #12.
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="DoubleListSimpleType">
    <xs:list itemType="xs:double"/>
  </xs:simpleType>
  <xs:element name="ThreePoint" nillable="false">
    <xs:simpleType>
      <xs:restriction base="DoubleListSimpleType">
        <xs:length value="3" fixed="true"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;

    // Exactly three list items — valid regardless of each item's char length.
    assert!(validate_xml_against_xsd("<ThreePoint>1.2 3.4 4.5</ThreePoint>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<ThreePoint>1 2 3</ThreePoint>", xsd).is_ok());

    // Wrong number of items — must fail the length facet.
    assert!(validate_xml_against_xsd("<ThreePoint>1.2 3.4</ThreePoint>", xsd).is_err());
    assert!(validate_xml_against_xsd("<ThreePoint>1.2 3.4 3 4</ThreePoint>", xsd).is_err());

    // Item type is still enforced: a non-double item must fail.
    assert!(validate_xml_against_xsd("<ThreePoint>1.2 abc 4.5</ThreePoint>", xsd).is_err());
}

#[test]
fn xsd_derived_list_inherits_item_facets() {
    // A named list type derived via <restriction> over another list type must
    // inherit the base list's item-level facets (e.g. a pattern on a
    // user-defined item type). Otherwise item constraints are silently dropped
    // and invalid items are accepted (fail-open). Regression for the
    // differential-review finding on `list_bases` / builder list passes.
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="HexByte">
    <xs:restriction base="xs:string">
      <xs:pattern value="[0-9A-Fa-f]{2}"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="HexList">
    <xs:list itemType="HexByte"/>
  </xs:simpleType>
  <xs:simpleType name="ShortHexList">
    <xs:restriction base="HexList">
      <xs:maxLength value="3"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:element name="data" type="ShortHexList"/>
</xs:schema>"#;

    // Valid: two well-formed hex bytes, within maxLength.
    assert!(validate_xml_against_xsd("<data>AA BB</data>", xsd).is_ok());
    // Item pattern must still be enforced through the derived list type.
    assert!(validate_xml_against_xsd("<data>AA ZZ</data>", xsd).is_err());
    // List-level maxLength (item count) must still be enforced.
    assert!(validate_xml_against_xsd("<data>AA BB CC DD</data>", xsd).is_err());
}

#[test]
fn xsd_inline_restriction_over_derived_list_enforces_item_facets() {
    // Combines both fixes: an *inline* simpleType restricting a *derived* list
    // type must inherit both is_list and the ultimate base list's item facets.
    let xsd = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="HexByte">
    <xs:restriction base="xs:string">
      <xs:pattern value="[0-9A-Fa-f]{2}"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="HexList">
    <xs:list itemType="HexByte"/>
  </xs:simpleType>
  <xs:simpleType name="ShortHexList">
    <xs:restriction base="HexList">
      <xs:maxLength value="4"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:element name="data">
    <xs:simpleType>
      <xs:restriction base="ShortHexList">
        <xs:length value="2"/>
      </xs:restriction>
    </xs:simpleType>
  </xs:element>
</xs:schema>"#;

    // Exactly two well-formed hex bytes.
    assert!(validate_xml_against_xsd("<data>AA BB</data>", xsd).is_ok());
    // length=2 counts items, not characters.
    assert!(validate_xml_against_xsd("<data>AA BB CC</data>", xsd).is_err());
    // Item pattern enforced transitively through the inline restriction.
    assert!(validate_xml_against_xsd("<data>AA ZZ</data>", xsd).is_err());
}

// ─── Nullable xs:choice regression (fastkml / KML issue) ────────────────
//
// A `<choice>` whose alternatives are all optional (every alternative has
// minOccurs=0) is nullable: it is satisfied by matching nothing. Such a
// choice appearing in a sequence must not reject a following element that
// belongs to a later particle. Regression for the KML AbstractFeatureType
// pattern, where an optional `Snippet|snippet` choice precedes the
// substitution-group feature list and previously caused
// "Element 'Placemark' does not match any choice alternative".

#[test]
fn nullable_choice_in_sequence_skips_to_next_particle() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root">
    <xs:complexType>
      <xs:sequence>
        <xs:choice>
          <xs:element name="a" type="xs:string" minOccurs="0"/>
          <xs:element name="b" type="xs:string" minOccurs="0"/>
        </xs:choice>
        <xs:element name="c" type="xs:string" minOccurs="0" maxOccurs="unbounded"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    // `c` follows the all-optional choice; the choice must consume nothing.
    assert!(validate_xml_against_xsd("<root><c>x</c></root>", xsd).is_ok());
    // The choice can still be taken.
    assert!(validate_xml_against_xsd("<root><a>x</a><c>y</c></root>", xsd).is_ok());
    // Empty is valid too.
    assert!(validate_xml_against_xsd("<root/>", xsd).is_ok());
}

#[test]
fn required_choice_with_no_optional_alternatives_still_errors() {
    // A choice with no optional alternative is NOT nullable: a non-matching
    // child must still be rejected (guards against the fix over-relaxing).
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root">
    <xs:complexType>
      <xs:choice>
        <xs:element name="a" type="xs:string"/>
        <xs:element name="b" type="xs:string"/>
      </xs:choice>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    assert!(validate_xml_against_xsd("<root><a>x</a></root>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<root><c>x</c></root>", xsd).is_err());
}

#[test]
fn kml_style_optional_choice_before_substitution_group() {
    // Faithful reduction of the KML AbstractFeatureType / DocumentType shape:
    // an optional inline choice inside the (extended) sequence, followed by a
    // reference to an abstract substitution-group head. A substituting member
    // (Placemark) as the only child must validate.
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:kml" xmlns:k="urn:kml" elementFormDefault="qualified">
  <xs:element name="AbstractFeatureGroup" type="k:AbstractFeatureType" abstract="true"/>
  <xs:complexType name="AbstractFeatureType" abstract="true">
    <xs:sequence>
      <xs:element name="name" type="xs:string" minOccurs="0"/>
      <xs:choice>
        <xs:element name="Snippet" type="xs:string" minOccurs="0"/>
        <xs:element name="snippet" type="xs:string" minOccurs="0"/>
      </xs:choice>
    </xs:sequence>
  </xs:complexType>
  <xs:element name="Placemark" type="k:PlacemarkType" substitutionGroup="k:AbstractFeatureGroup"/>
  <xs:complexType name="PlacemarkType">
    <xs:complexContent><xs:extension base="k:AbstractFeatureType"/></xs:complexContent>
  </xs:complexType>
  <xs:element name="Document" type="k:DocumentType"/>
  <xs:complexType name="DocumentType">
    <xs:complexContent>
      <xs:extension base="k:AbstractFeatureType">
        <xs:sequence>
          <xs:element ref="k:AbstractFeatureGroup" minOccurs="0" maxOccurs="unbounded"/>
        </xs:sequence>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
</xs:schema>"#;
    let xml = r#"<Document xmlns="urn:kml"><Placemark/></Document>"#;
    let result = validate_xml_against_xsd(xml, xsd);
    assert!(result.is_ok(), "{:?}", result);
}

#[test]
fn nullable_choice_via_optional_sequence_alternative() {
    // A choice alternative can be nullable through its *content*: a sequence
    // whose sub-particles are all optional matches the empty sequence, so the
    // enclosing choice is nullable even though the alternative's own minOccurs
    // is 1. A following element must still be accepted.
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root">
    <xs:complexType>
      <xs:sequence>
        <xs:choice>
          <xs:sequence>
            <xs:element name="a" type="xs:string" minOccurs="0"/>
            <xs:element name="b" type="xs:string" minOccurs="0"/>
          </xs:sequence>
        </xs:choice>
        <xs:element name="c" type="xs:string" minOccurs="0" maxOccurs="unbounded"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let result = validate_xml_against_xsd("<root><c>x</c></root>", xsd);
    assert!(result.is_ok(), "{:?}", result);
    assert!(validate_xml_against_xsd("<root><a>x</a><c>y</c></root>", xsd).is_ok());
    assert!(validate_xml_against_xsd("<root/>", xsd).is_ok());
}

// ─── xs:import schemaLocation hint semantics ────────────────

/// A unique tempdir that removes itself (and its contents) on drop, so import
/// regression tests can write sibling schema files without leaving artifacts in
/// the system temp dir. Derefs to `Path`, so call sites use `dir.join(...)`
/// unchanged.
struct TempDir {
    path: std::path::PathBuf,
}

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup; ignore errors (e.g. already removed).
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Create a unique self-cleaning tempdir so a test can write sibling schema
/// files and resolve `schemaLocation` against them — no dependency on excluded
/// `test-data/`. Keep the returned guard alive for the duration of the test.
fn import_test_dir(label: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "uppsala-xsdimport-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&path).expect("create tempdir");
    TempDir { path }
}

/// Regression for the composite-schema import bug (see ADR 0011 and
/// `xsd_bug.md`): an `xs:import` whose `schemaLocation` cannot be resolved
/// (here a `classpath:` URI) must be skipped — the location is only a hint per
/// XSD 1.0 Part 1 §4.2.3 — instead of aborting the whole schema build. Before
/// the fix the build failed with "absolute URI not supported", so the root
/// element declared in a *sibling, resolvable* import (`urn:inner` `Thing`)
/// could never be found ("No element declaration found for 'Thing'"). Fixtures
/// are written to a tempdir so the test always runs (no `test-data/` reliance).
#[test]
fn import_with_unresolvable_hint_is_skipped_and_root_resolves() {
    let dir = import_test_dir("hint");

    std::fs::write(
        dir.join("inner.xsd"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<schema targetNamespace="urn:inner" xmlns="http://www.w3.org/2001/XMLSchema"
        xmlns:i="urn:inner" elementFormDefault="qualified">
  <element name="Thing" type="i:ThingType"/>
  <complexType name="ThingType"><attribute name="id" type="string"/></complexType>
</schema>"#,
    )
    .unwrap();

    let composite_src = r#"<?xml version="1.0" encoding="UTF-8"?>
<schema targetNamespace="urn:aggregate" xmlns="http://www.w3.org/2001/XMLSchema" version="1.0">
  <import namespace="urn:unresolvable" schemaLocation="classpath:/schema/does-not-exist.xsd"/>
  <import namespace="urn:inner" schemaLocation="inner.xsd"/>
</schema>"#;
    let composite_path = dir.join("composite.xsd");
    std::fs::write(&composite_path, composite_src).unwrap();

    let schema_doc = parse(composite_src).expect("parse composite.xsd");
    // Build must succeed despite the unresolvable `classpath:` import hint.
    let validator =
        uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&composite_path))
            .expect("composite schema must build despite an unresolvable import hint");

    // The root element comes only from the resolvable `inner.xsd` import.
    let doc = parse(r#"<i:Thing xmlns:i="urn:inner" id="x"/>"#).expect("parse instance");
    let errors = validator.validate(&doc);
    assert!(
        errors.is_empty(),
        "expected <i:Thing> to validate against the imported declaration, got: {errors:?}"
    );
}

/// Counterpart to the hint-skip rule: an `xs:import` whose `schemaLocation`
/// *resolves* to a real, readable file that is **not** well-formed is a genuine
/// error and must surface (only an *unresolvable* location is skipped).
#[test]
fn import_of_resolvable_malformed_schema_errors() {
    let dir = import_test_dir("malformed");

    // Resolvable sibling file, but not well-formed XML.
    std::fs::write(dir.join("broken.xsd"), "<schema><not-closed>").unwrap();

    let composite_src = r#"<?xml version="1.0" encoding="UTF-8"?>
<schema targetNamespace="urn:aggregate" xmlns="http://www.w3.org/2001/XMLSchema">
  <import namespace="urn:broken" schemaLocation="broken.xsd"/>
</schema>"#;
    let composite_path = dir.join("composite.xsd");
    std::fs::write(&composite_path, composite_src).unwrap();

    let schema_doc = parse(composite_src).expect("parse composite.xsd");
    let result =
        uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&composite_path));
    assert!(
        result.is_err(),
        "a resolvable-but-malformed imported schema must surface an error, not be skipped"
    );
}

// ─── libxml2-compatible lenient datatype mode ──────────────

/// Validate with lenient mode toggled on the validator.
fn validate_lenient(xml: &str, xsd: &str, lenient: bool) -> Result<(), String> {
    let schema_doc = parse(xsd).map_err(|e| format!("Schema parse error: {e}"))?;
    let mut validator = uppsala::XsdValidator::from_schema(&schema_doc)
        .map_err(|e| format!("Schema load error: {e}"))?;
    validator.set_lenient(lenient);
    let doc = parse(xml).map_err(|e| format!("XML parse error: {e}"))?;
    let errors = validator.validate(&doc);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; "))
    }
}

/// Strict mode rejects an `anyURI` containing a space (RFC 3987); lenient mode
/// accepts it, matching libxml2. Regression for ADR 0012.
#[test]
fn anyuri_space_strict_rejected_lenient_accepted() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="loc" type="xs:anyURI"/>
    </xs:schema>"#;
    let xml = "<loc>geo:1.0, 2.0</loc>";
    assert!(
        validate_lenient(xml, xsd, false).is_err(),
        "strict mode must reject an anyURI containing a space"
    );
    assert!(
        validate_lenient(xml, xsd, true).is_ok(),
        "lenient mode must accept an anyURI containing a space"
    );
}

/// A whitespace-separated value that reaches `anyURI` validation as a single
/// value (e.g. when list typing is not applied) is rejected in strict mode but
/// accepted in lenient mode — the observable libxml2 result for SAML
/// `protocolSupportEnumeration`-style values.
#[test]
fn anyuri_multitoken_value_lenient() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="e">
        <xs:complexType><xs:attribute name="protos" type="xs:anyURI"/></xs:complexType>
      </xs:element>
    </xs:schema>"#;
    let xml = r#"<e protos="urn:a urn:b http://x/y"/>"#;
    assert!(validate_lenient(xml, xsd, false).is_err());
    assert!(validate_lenient(xml, xsd, true).is_ok());
}

/// Lenient mode must not turn off unrelated datatype checks: a malformed
/// integer is still rejected.
#[test]
fn lenient_mode_keeps_other_datatype_checks() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="n" type="xs:int"/>
    </xs:schema>"#;
    assert!(validate_lenient("<n>not-an-int</n>", xsd, true).is_err());
    assert!(validate_lenient("<n>42</n>", xsd, true).is_ok());
}

/// A list-typed attribute inherited through a CROSS-IMPORT `xsi:type` extension
/// chain is validated per item, not collapsed to its item type and applied to
/// the whole value. This pins the behaviour investigated for cross-import
/// `xsi:type` extension chains: list items must be split and validated
/// individually (modeled here with a list-of-`int`). Fixtures (base declares the list attribute;
/// ext, in another namespace, extends it; composite imports both) are written to a tempdir so
/// the test always runs (no `test-data/` reliance).
#[test]
fn cross_import_xsi_type_list_attribute_validates_per_item() {
    let dir = import_test_dir("list");

    std::fs::write(
        dir.join("list-base.xsd"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<schema targetNamespace="urn:list-base" xmlns="http://www.w3.org/2001/XMLSchema"
        xmlns:b="urn:list-base" elementFormDefault="unqualified">
  <element name="e" type="b:Base"/>
  <complexType name="Base"><attribute name="nums" type="b:intList"/></complexType>
  <simpleType name="intList"><list itemType="int"/></simpleType>
</schema>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("list-ext.xsd"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<schema targetNamespace="urn:list-ext" xmlns="http://www.w3.org/2001/XMLSchema"
        xmlns:x="urn:list-ext" xmlns:b="urn:list-base">
  <import namespace="urn:list-base" schemaLocation="list-base.xsd"/>
  <complexType name="Derived">
    <complexContent>
      <extension base="b:Base"><attribute name="z" type="string"/></extension>
    </complexContent>
  </complexType>
</schema>"#,
    )
    .unwrap();
    let composite_src = r#"<?xml version="1.0" encoding="UTF-8"?>
<schema targetNamespace="urn:list-agg" xmlns="http://www.w3.org/2001/XMLSchema">
  <import namespace="urn:list-ext" schemaLocation="list-ext.xsd"/>
  <import namespace="urn:list-base" schemaLocation="list-base.xsd"/>
</schema>"#;
    let composite_path = dir.join("list-composite.xsd");
    std::fs::write(&composite_path, composite_src).unwrap();

    let schema_doc = parse(composite_src).expect("parse list-composite.xsd");
    let validator =
        uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&composite_path))
            .expect("composite schema builds");

    // Valid: every list item is a valid int — must pass. (If the list type were
    // collapsed to a single `int`, "1 2 3" would be rejected as one value.)
    let ok_doc = parse(
        r#"<b:e xmlns:b="urn:list-base" xmlns:x="urn:list-ext"
              xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
              xsi:type="x:Derived" nums="1 2 3" z="hi"/>"#,
    )
    .expect("parse ok instance");
    let ok_errors = validator.validate(&ok_doc);
    assert!(
        ok_errors.is_empty(),
        "valid list-of-int via cross-import xsi:type should pass, got: {ok_errors:?}"
    );

    // Invalid: one bad item — must be reported per item ("abc"), proving the
    // value is split and each item validated against the list's item type.
    let bad_doc = parse(
        r#"<b:e xmlns:b="urn:list-base" xmlns:x="urn:list-ext"
              xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
              xsi:type="x:Derived" nums="1 abc 3" z="hi"/>"#,
    )
    .expect("parse bad instance");
    let bad_errors = validator.validate(&bad_doc);
    assert!(
        bad_errors.iter().any(|e| e.message.contains("'abc'")),
        "invalid list item should be reported per item, got: {bad_errors:?}"
    );
}

// Regression coverage for GitHub issue #43.
#[test]
fn xsd_inherited_simple_content_values_and_children() {
    // Exercise both built-in values and named simple types with facets.
    for base in ["xs:decimal", "Amount"] {
        let xsd = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:simpleType name="Amount">
    <xs:restriction base="xs:decimal">
      <xs:minInclusive value="0"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:complexType name="Base">
    <xs:simpleContent><xs:extension base="{base}"/></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="Derived">
    <xs:simpleContent><xs:extension base="Base"/></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="Leaf">
    <xs:simpleContent><xs:extension base="Derived"/></xs:simpleContent>
  </xs:complexType>
  <xs:element name="base" type="Base"/>
  <xs:element name="leaf" type="Leaf"/>
</xs:schema>"#
        );
        let validator = uppsala::XsdValidator::from_schema(&parse(&xsd).unwrap()).unwrap();
        for (xml, valid) in [
            ("<base>12.50</base>", true),
            ("<base>garbage</base>", false),
            ("<base><child/>12.50</base>", false),
            (
                "<leaf> <![CDATA[12.50]]><!-- comment --><?pi ok?> </leaf>",
                true,
            ),
            ("<leaf>garbage</leaf>", false),
            (
                r#"<base xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
                         xsi:type="Leaf">garbage</base>"#,
                false,
            ),
            ("<leaf>-1</leaf>", base == "xs:decimal"),
            ("<leaf><child>12.50</child></leaf>", false),
        ] {
            let errors = validator.validate(&parse(xml).unwrap());
            assert_eq!(errors.is_empty(), valid, "{base}: {xml}: {errors:?}");
        }
    }
}

#[test]
fn xsd_inherited_simple_content_preserves_derived_attributes() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="Base">
    <xs:simpleContent><xs:extension base="xs:decimal">
      <xs:attribute name="legacy" type="xs:string"/>
      <xs:attribute name="id" type="xs:integer"/>
    </xs:extension></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="Restricted">
    <xs:simpleContent><xs:restriction base="Base">
      <xs:attribute name="legacy" use="prohibited"/>
      <xs:attribute name="id" type="xs:positiveInteger" use="required"/>
    </xs:restriction></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="Leaf">
    <xs:simpleContent><xs:extension base="Restricted">
      <xs:attribute name="label" type="xs:string" use="required"/>
    </xs:extension></xs:simpleContent>
  </xs:complexType>
  <xs:element name="item" type="Leaf"/>
</xs:schema>"#;
    let valid = r#"<item id="1" label="price">12.50</item>"#;
    let result = validate_xml_against_xsd(valid, xsd);
    assert!(result.is_ok(), "{result:?}");
    for xml in [
        r#"<item id="1" label="price" legacy="old">12.50</item>"#,
        r#"<item label="price">12.50</item>"#,
        r#"<item id="0" label="price">12.50</item>"#,
        r#"<item id="1">12.50</item>"#,
    ] {
        assert!(validate_xml_against_xsd(xml, xsd).is_err(), "{xml}");
    }
}

#[test]
fn xsd_simple_content_inheritance_cycles_are_rejected() {
    for base in ["A", "B"] {
        let xsd = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:complexType name="A">
    <xs:simpleContent><xs:extension base="{base}"/></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="B">
    <xs:simpleContent><xs:restriction base="A"/></xs:simpleContent>
  </xs:complexType>
  <xs:element name="item">
    <xs:complexType><xs:simpleContent>
      <xs:extension base="A"/>
    </xs:simpleContent></xs:complexType>
  </xs:element>
</xs:schema>"#
        );
        let error = validate_xml_against_xsd("<item>12.50</item>", &xsd).unwrap_err();
        assert!(
            error.contains("Complex type derivation cycle detected"),
            "base={base}: {error}"
        );
    }
}

#[test]
fn xsd_unresolved_simple_content_base_names_the_type() {
    // An unresolvable value type is reported with the same wording as any other
    // unresolved type reference, so the missing name is visible either way.
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:o="urn:other" xmlns="urn:me" targetNamespace="urn:me">
  <xs:import namespace="urn:other"/>
  <xs:complexType name="T">
    <xs:simpleContent><xs:extension base="o:Thing">
      <xs:attribute name="a" type="xs:string"/>
    </xs:extension></xs:simpleContent>
  </xs:complexType>
  <xs:element name="r" type="T"/>
</xs:schema>"#;
    let validator = uppsala::XsdValidator::from_schema(&parse(xsd).unwrap()).unwrap();
    let errors = validator.validate(&parse(r#"<r xmlns="urn:me" a="1">12.5</r>"#).unwrap());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].message, "Type '{urn:other}Thing' not found");
}

#[test]
fn xsd_simple_content_checks_preserve_complex_content_extensions() {
    // Particleless complex-content extensions share the parser's SimpleContent
    // representation. Follow the chain before deciding whether children are forbidden.
    for content in [
        r#"<xs:complexContent><xs:extension base="xs:anyType"/></xs:complexContent>"#,
        r#"<xs:sequence><xs:element name="a"/></xs:sequence>"#,
        r#"<xs:choice><xs:element name="a"/><xs:element name="b"/></xs:choice>"#,
        r#"<xs:all><xs:element name="a"/></xs:all>"#,
        r#"<xs:simpleContent><xs:extension base="xs:decimal"/></xs:simpleContent>"#,
    ] {
        let schema = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
          <xs:complexType name="Base">{content}</xs:complexType>
          <xs:complexType name="Derived"><xs:complexContent><xs:extension base="Base"/></xs:complexContent></xs:complexType>
          <xs:complexType name="Leaf"><xs:complexContent><xs:extension base="Derived"/></xs:complexContent></xs:complexType>
          <xs:element name="base" type="Base"/><xs:element name="derived" type="Derived"/><xs:element name="leaf" type="Leaf"/>
        </xs:schema>"#
        );
        let validator = uppsala::XsdValidator::from_schema(&parse(&schema).unwrap()).unwrap();
        for name in ["base", "derived", "leaf"] {
            let xml = format!("<{name}><a/></{name}>");
            let errors = validator.validate(&parse(&xml).unwrap());
            let simple = content.contains("simpleContent");
            assert_eq!(errors.is_empty(), !simple, "{content}, {xml}: {errors:?}");
            if simple {
                for (value, valid) in [("12.50", true), ("garbage", false)] {
                    let xml = format!("<{name}>{value}</{name}>");
                    let errors = validator.validate(&parse(&xml).unwrap());
                    assert_eq!(errors.is_empty(), valid, "{xml}: {errors:?}");
                }
            }
        }
    }
}
