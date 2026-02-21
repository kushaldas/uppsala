//! Unit tests for the XSD validator.

#[cfg(test)]
mod tests {
    use crate::parse;
    use crate::xsd::types::XsdValidator;

    #[test]
    fn test_validate_string_element() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="root" type="xs:string"/>
        </xs:schema>
        "#;
        let doc_xml = "<root>hello</root>";

        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_integer_valid() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="count" type="xs:integer"/>
        </xs:schema>
        "#;
        let doc_xml = "<count>42</count>";

        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_integer_invalid() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="count" type="xs:integer"/>
        </xs:schema>
        "#;
        let doc_xml = "<count>not-a-number</count>";

        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_validate_boolean() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="flag" type="xs:boolean"/>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();

        for val in &["true", "false", "1", "0"] {
            let input = format!("<flag>{}</flag>", val);
            let doc = parse(&input).unwrap();
            let validator = XsdValidator::from_schema(&schema).unwrap();
            assert!(validator.validate(&doc).is_empty(), "Failed for {}", val);
        }

        let doc = parse("<flag>yes</flag>").unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        assert!(!validator.validate(&doc).is_empty());
    }

    #[test]
    fn test_validate_complex_type_sequence() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="person">
                <xs:complexType>
                    <xs:sequence>
                        <xs:element name="name" type="xs:string"/>
                        <xs:element name="age" type="xs:integer"/>
                    </xs:sequence>
                </xs:complexType>
            </xs:element>
        </xs:schema>
        "#;

        let doc_xml = "<person><name>Alice</name><age>30</age></person>";
        let schema = parse(schema_xml).unwrap();
        let doc = parse(doc_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_required_attribute() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="item">
                <xs:complexType>
                    <xs:sequence/>
                    <xs:attribute name="id" type="xs:string" use="required"/>
                </xs:complexType>
            </xs:element>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();

        // Missing required attribute
        let doc = parse("<item/>").unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let errors = validator.validate(&doc);
        assert!(!errors.is_empty());

        // With required attribute
        let doc = parse(r#"<item id="123"/>"#).unwrap();
        let errors = validator.validate(&doc);
        assert!(errors.is_empty(), "Errors: {:?}", errors);
    }

    #[test]
    fn test_validate_min_max_inclusive() {
        let schema_xml = r#"
        <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="score">
                <xs:simpleType>
                    <xs:restriction base="xs:integer">
                        <xs:minInclusive value="0"/>
                        <xs:maxInclusive value="100"/>
                    </xs:restriction>
                </xs:simpleType>
            </xs:element>
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();

        let doc = parse("<score>50</score>").unwrap();
        assert!(validator.validate(&doc).is_empty());

        let doc = parse("<score>150</score>").unwrap();
        assert!(!validator.validate(&doc).is_empty());

        let doc = parse("<score>-1</score>").unwrap();
        assert!(!validator.validate(&doc).is_empty());
    }

    #[test]
    fn test_validate_enumeration() {
        let schema_xml = r#"
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
        </xs:schema>
        "#;

        let schema = parse(schema_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();

        let doc = parse("<color>red</color>").unwrap();
        assert!(validator.validate(&doc).is_empty());

        let doc = parse("<color>yellow</color>").unwrap();
        assert!(!validator.validate(&doc).is_empty());
    }
}
