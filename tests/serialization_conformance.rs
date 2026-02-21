//! Comprehensive tests for XML serialization, round-trip fidelity, and the XmlWriter builder.

// ─── Round-trip: to_xml() ───────────────────────────────────────────────────

#[test]
fn roundtrip_simple() {
    let xml = "<root><child>text</child></root>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_self_closing() {
    let xml = "<root><empty/></root>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attributes() {
    let xml = r#"<root attr="value"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_entities_in_text() {
    let xml = "<r>&lt;&amp;&gt;</r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_xml_declaration() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><r/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_xml_declaration_standalone() {
    let xml = r#"<?xml version="1.0" standalone="yes"?><r/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_comment() {
    let xml = "<r><!-- a comment --></r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_processing_instruction() {
    let xml = "<r><?mypi some data?></r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_pi_no_data() {
    let xml = "<r><?mypi?></r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_cdata() {
    let xml = "<r><![CDATA[<not>xml</not>]]></r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_mixed_content() {
    let xml = "<r>text<b>bold</b>more</r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_deep_nesting() {
    let xml = "<a><b><c><d><e>deep</e></d></c></b></a>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_multiple_attributes() {
    let xml = r#"<r a="1" b="2" c="3"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_unicode_text() {
    let xml = "<r>日本語テキスト</r>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_unicode_attribute() {
    let xml = r#"<r attr="日本語"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_empty_document_element() {
    let xml = "<root/>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attr_with_quote() {
    // Attribute value containing &quot;
    let xml = r#"<r a="say &quot;hello&quot;"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attr_with_amp() {
    let xml = r#"<r a="a &amp; b"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attr_with_lt() {
    let xml = r#"<r a="a &lt; b"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

// ─── DOCTYPE preservation ───────────────────────────────────────────────────

#[test]
fn doctype_preserved_system() {
    let xml = r#"<?xml version="1.0"?><!DOCTYPE root SYSTEM "root.dtd"><root/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(
        doc.doctype.as_deref(),
        Some(r#"<!DOCTYPE root SYSTEM "root.dtd">"#)
    );
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn doctype_preserved_public() {
    let xml = r#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd"><html/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.doctype.is_some());
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn doctype_preserved_internal_subset() {
    let xml =
        "<?xml version=\"1.0\"?><!DOCTYPE root [\n<!ELEMENT root (#PCDATA)>\n]><root>hello</root>";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.doctype.is_some());
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn no_doctype_is_none() {
    let xml = "<root/>";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.doctype.is_none());
}

// ─── Escaping edge cases ────────────────────────────────────────────────────

#[test]
fn text_escaping_amp_lt_gt() {
    let doc = uppsala::parse("<r>&amp;&lt;&gt;</r>").unwrap();
    let output = doc.to_xml();
    assert_eq!(output, "<r>&amp;&lt;&gt;</r>");
}

#[test]
fn attr_escaping_quote() {
    let doc = uppsala::parse(r#"<r a="&quot;"/>"#).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, r#"<r a="&quot;"/>"#);
}

// ─── Display trait ──────────────────────────────────────────────────────────

#[test]
fn display_matches_to_xml() {
    let xml =
        r#"<?xml version="1.0" encoding="UTF-8"?><root attr="val"><child>text</child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(format!("{}", doc), doc.to_xml());
}

#[test]
fn display_simple() {
    let doc = uppsala::parse("<r>hello</r>").unwrap();
    assert_eq!(format!("{}", doc), "<r>hello</r>");
}

// ─── node_to_xml (subtree serialization) ────────────────────────────────────

#[test]
fn node_to_xml_document_element() {
    let xml = r#"<?xml version="1.0"?><root><child>text</child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root_elem = doc.document_element().unwrap();
    // node_to_xml should NOT include XML declaration
    assert_eq!(
        doc.node_to_xml(root_elem),
        "<root><child>text</child></root>"
    );
}

#[test]
fn node_to_xml_subtree() {
    let xml = "<root><a><b>inner</b></a><c/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root_elem = doc.document_element().unwrap();
    let children = doc.children(root_elem);
    // First child is <a>
    assert_eq!(doc.node_to_xml(children[0]), "<a><b>inner</b></a>");
    // Second child is <c/>
    assert_eq!(doc.node_to_xml(children[1]), "<c/>");
}

#[test]
fn node_to_xml_text_node() {
    let xml = "<r>hello &amp; world</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root_elem = doc.document_element().unwrap();
    let children = doc.children(root_elem);
    assert_eq!(doc.node_to_xml(children[0]), "hello &amp; world");
}

// ─── write_to (io::Write streaming) ────────────────────────────────────────

#[test]
fn write_to_vec() {
    let xml = "<root><child>text</child></root>";
    let doc = uppsala::parse(xml).unwrap();
    let mut buf: Vec<u8> = Vec::new();
    doc.write_to(&mut buf).unwrap();
    assert_eq!(String::from_utf8(buf).unwrap(), xml);
}

#[test]
fn write_to_matches_to_xml() {
    let xml =
        r#"<?xml version="1.0" encoding="UTF-8"?><root attr="val"><child>text</child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let mut buf: Vec<u8> = Vec::new();
    doc.write_to(&mut buf).unwrap();
    assert_eq!(String::from_utf8(buf).unwrap(), doc.to_xml());
}

// ─── XmlWriteOptions: expand_empty_elements ─────────────────────────────────

#[test]
fn expand_empty_elements() {
    let xml = "<root><empty/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::compact().with_expand_empty_elements(true);
    assert_eq!(
        doc.to_xml_with_options(&opts),
        "<root><empty></empty></root>"
    );
}

#[test]
fn expand_empty_root() {
    let xml = "<root/>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::compact().with_expand_empty_elements(true);
    assert_eq!(doc.to_xml_with_options(&opts), "<root></root>");
}

#[test]
fn self_closing_default() {
    let xml = "<root><empty/></root>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

// ─── XmlWriteOptions: pretty-printing ───────────────────────────────────────

#[test]
fn pretty_print_simple() {
    let xml = "<root><a/><b/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let expected = "<root>\n  <a/>\n  <b/>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

#[test]
fn pretty_print_nested() {
    let xml = "<root><a><b/></a></root>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let expected = "<root>\n  <a>\n    <b/>\n  </a>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

#[test]
fn pretty_print_mixed_content_not_indented() {
    // Mixed content (text + elements) should NOT be indented
    let xml = "<r>text<b>bold</b>more</r>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    // Mixed content preserved exactly
    assert_eq!(
        doc.to_xml_with_options(&opts),
        "<r>text<b>bold</b>more</r>\n"
    );
}

#[test]
fn pretty_print_with_tab_indent() {
    let xml = "<root><a/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("\t");
    assert_eq!(doc.to_xml_with_options(&opts), "<root>\n\t<a/>\n</root>\n");
}

#[test]
fn pretty_print_with_declaration() {
    let xml = r#"<?xml version="1.0"?><root><a/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let expected = "<?xml version=\"1.0\"?><root>\n  <a/>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

#[test]
fn pretty_print_expand_empty() {
    let xml = "<root><a/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ").with_expand_empty_elements(true);
    let expected = "<root>\n  <a></a>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

// ─── node_to_xml_with_options ───────────────────────────────────────────────

#[test]
fn node_to_xml_with_expand_empty() {
    let xml = "<root><a/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    let opts = uppsala::XmlWriteOptions::compact().with_expand_empty_elements(true);
    assert_eq!(doc.node_to_xml_with_options(children[0], &opts), "<a></a>");
}

// ─── Namespace declarations in serialization ────────────────────────────────

#[test]
fn namespace_declarations_preserved() {
    let xml = r#"<root xmlns="http://example.com"><child/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert!(output.contains(r#"xmlns="http://example.com""#));
}

#[test]
fn prefixed_namespace_preserved() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"><ns:child/></ns:root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert!(output.contains(r#"xmlns:ns="http://example.com""#));
    assert!(output.contains("<ns:root"));
    assert!(output.contains("<ns:child"));
}

// ─── XmlWriter builder tests ────────────────────────────────────────────────

#[test]
fn writer_basic() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("root", &[]);
    w.text("hello");
    w.end_element("root");
    assert_eq!(w.into_string(), "<root>hello</root>");
}

#[test]
fn writer_declaration() {
    let mut w = uppsala::XmlWriter::new();
    w.write_declaration();
    w.start_element("r", &[]);
    w.end_element("r");
    assert_eq!(
        w.into_string(),
        r#"<?xml version="1.0" encoding="UTF-8"?><r></r>"#
    );
}

#[test]
fn writer_declaration_full() {
    let mut w = uppsala::XmlWriter::new();
    w.write_declaration_full("1.0", Some("ISO-8859-1"), Some(true));
    w.empty_element("r", &[]);
    assert_eq!(
        w.into_string(),
        r#"<?xml version="1.0" encoding="ISO-8859-1" standalone="yes"?><r/>"#
    );
}

#[test]
fn writer_attributes() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("div", &[("class", "main"), ("id", "c1")]);
    w.end_element("div");
    assert_eq!(w.into_string(), r#"<div class="main" id="c1"></div>"#);
}

#[test]
fn writer_empty_element() {
    let mut w = uppsala::XmlWriter::new();
    w.empty_element("br", &[]);
    assert_eq!(w.into_string(), "<br/>");
}

#[test]
fn writer_empty_element_expanded() {
    let mut w = uppsala::XmlWriter::new();
    w.empty_element_expanded("br", &[]);
    assert_eq!(w.into_string(), "<br></br>");
}

#[test]
fn writer_empty_element_with_attrs() {
    let mut w = uppsala::XmlWriter::new();
    w.empty_element("input", &[("type", "text"), ("name", "q")]);
    assert_eq!(w.into_string(), r#"<input type="text" name="q"/>"#);
}

#[test]
fn writer_text_escaping() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("r", &[]);
    w.text("a < b & c > d");
    w.end_element("r");
    assert_eq!(w.into_string(), "<r>a &lt; b &amp; c &gt; d</r>");
}

#[test]
fn writer_attr_escaping() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("r", &[("a", "say \"hello\"")]);
    w.end_element("r");
    assert_eq!(w.into_string(), r#"<r a="say &quot;hello&quot;"></r>"#);
}

#[test]
fn writer_attr_whitespace_escaping() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("r", &[("a", "line1\nline2\ttab\rCR")]);
    w.end_element("r");
    assert_eq!(
        w.into_string(),
        r#"<r a="line1&#xA;line2&#x9;tab&#xD;CR"></r>"#
    );
}

#[test]
fn writer_cdata() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("r", &[]);
    w.cdata("<not>xml</not>");
    w.end_element("r");
    assert_eq!(w.into_string(), "<r><![CDATA[<not>xml</not>]]></r>");
}

#[test]
fn writer_comment() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("r", &[]);
    w.comment(" a comment ");
    w.end_element("r");
    assert_eq!(w.into_string(), "<r><!-- a comment --></r>");
}

#[test]
fn writer_pi() {
    let mut w = uppsala::XmlWriter::new();
    w.processing_instruction("php", Some("echo 'hello';"));
    assert_eq!(w.into_string(), "<?php echo 'hello';?>");
}

#[test]
fn writer_pi_no_data() {
    let mut w = uppsala::XmlWriter::new();
    w.processing_instruction("target", None);
    assert_eq!(w.into_string(), "<?target?>");
}

#[test]
fn writer_raw() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("root", &[]);
    w.raw("<pre-built>fragment</pre-built>");
    w.end_element("root");
    assert_eq!(
        w.into_string(),
        "<root><pre-built>fragment</pre-built></root>"
    );
}

#[test]
fn writer_namespace_attrs() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element(
        "ds:Signature",
        &[("xmlns:ds", "http://www.w3.org/2000/09/xmldsig#")],
    );
    w.empty_element("ds:SignedInfo", &[]);
    w.end_element("ds:Signature");
    assert_eq!(
        w.into_string(),
        r#"<ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:SignedInfo/></ds:Signature>"#
    );
}

#[test]
fn writer_rsa_key_value_pattern() {
    // This mimics the pattern from bergshamra's key.rs
    let mut w = uppsala::XmlWriter::new();
    let prefix = "ds";
    w.start_element(&format!("{prefix}:RSAKeyValue"), &[]);
    w.start_element(&format!("{prefix}:Modulus"), &[]);
    w.text("AQAB");
    w.end_element(&format!("{prefix}:Modulus"));
    w.start_element(&format!("{prefix}:Exponent"), &[]);
    w.text("AQAB");
    w.end_element(&format!("{prefix}:Exponent"));
    w.end_element(&format!("{prefix}:RSAKeyValue"));
    assert_eq!(
        w.into_string(),
        "<ds:RSAKeyValue><ds:Modulus>AQAB</ds:Modulus><ds:Exponent>AQAB</ds:Exponent></ds:RSAKeyValue>"
    );
}

#[test]
fn writer_ec_key_value_pattern() {
    // Mimics bergshamra's ECKeyValue pattern
    let mut w = uppsala::XmlWriter::new();
    w.start_element(
        "ECKeyValue",
        &[("xmlns", "http://www.w3.org/2009/xmldsig11#")],
    );
    w.empty_element("NamedCurve", &[("URI", "urn:oid:1.2.840.10045.3.1.7")]);
    w.start_element("PublicKey", &[]);
    w.text("base64data==");
    w.end_element("PublicKey");
    w.end_element("ECKeyValue");
    assert_eq!(
        w.into_string(),
        r#"<ECKeyValue xmlns="http://www.w3.org/2009/xmldsig11#"><NamedCurve URI="urn:oid:1.2.840.10045.3.1.7"/><PublicKey>base64data==</PublicKey></ECKeyValue>"#
    );
}

#[test]
fn writer_len_and_is_empty() {
    let mut w = uppsala::XmlWriter::new();
    assert!(w.is_empty());
    assert_eq!(w.len(), 0);
    w.text("x");
    assert!(!w.is_empty());
    assert_eq!(w.len(), 1);
}

#[test]
fn writer_as_str() {
    let mut w = uppsala::XmlWriter::new();
    w.text("hello");
    assert_eq!(w.as_str(), "hello");
}

#[test]
fn writer_with_capacity() {
    let w = uppsala::XmlWriter::with_capacity(1024);
    assert!(w.is_empty());
}

#[test]
fn writer_display() {
    let mut w = uppsala::XmlWriter::new();
    w.start_element("r", &[]);
    w.text("hi");
    w.end_element("r");
    assert_eq!(format!("{}", w), "<r>hi</r>");
}

#[test]
fn writer_into_bytes() {
    let mut w = uppsala::XmlWriter::new();
    w.text("abc");
    assert_eq!(w.into_bytes(), b"abc");
}

// ─── write_to_with_options ──────────────────────────────────────────────────

#[test]
fn write_to_with_pretty_options() {
    let xml = "<root><a/><b/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let mut buf: Vec<u8> = Vec::new();
    doc.write_to_with_options(&mut buf, &opts).unwrap();
    let result = String::from_utf8(buf).unwrap();
    assert_eq!(result, "<root>\n  <a/>\n  <b/>\n</root>\n");
}
