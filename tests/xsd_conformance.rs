//! Integration tests for XSD 1.1 validation.

// ─── Simple type validation ─────────────────────────────

fn validate_xml_against_xsd(xml: &str, xsd: &str) -> Result<(), String> {
    let schema_doc = uppsala::parse(xsd).map_err(|e| format!("Schema parse error: {}", e))?;
    let validator = uppsala::XsdValidator::from_schema(&schema_doc)
        .map_err(|e| format!("Schema load error: {}", e))?;
    let doc = uppsala::parse(xml).map_err(|e| format!("XML parse error: {}", e))?;
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
