//! Comprehensive tests for XML serialization, round-trip fidelity, and the
//! `XmlWriter` builder.
//!
//! The suite is grouped by serialization surface. Simple round-trip cases pin
//! byte-for-byte behavior for parsed XML, while later sections cover security
//! defaults, subtree serialization, writer escaping, and streaming output.

mod common;
use common::parse;

// ─── Round-trip: to_xml() ───────────────────────────────────────────────────

// These tests cover parsed XML that should serialize back to the same compact
// representation. They intentionally use small fixtures so each XML construct
// is isolated when a round-trip regression occurs.

#[test]
fn roundtrip_simple() {
    let xml = "<root><child>text</child></root>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_self_closing() {
    let xml = "<root><empty/></root>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attributes() {
    let xml = r#"<root attr="value"/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_entities_in_text() {
    let xml = "<r>&lt;&amp;&gt;</r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_xml_declaration() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><r/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_xml_declaration_standalone() {
    let xml = r#"<?xml version="1.0" standalone="yes"?><r/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_comment() {
    let xml = "<r><!-- a comment --></r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_processing_instruction() {
    let xml = "<r><?mypi some data?></r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_pi_no_data() {
    let xml = "<r><?mypi?></r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_cdata() {
    let xml = "<r><![CDATA[<not>xml</not>]]></r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_mixed_content() {
    let xml = "<r>text<b>bold</b>more</r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_deep_nesting() {
    let xml = "<a><b><c><d><e>deep</e></d></c></b></a>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_multiple_attributes() {
    let xml = r#"<r a="1" b="2" c="3"/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_unicode_text() {
    let xml = "<r>日本語テキスト</r>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_unicode_attribute() {
    let xml = r#"<r attr="日本語"/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_empty_document_element() {
    let xml = "<root/>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attr_with_quote() {
    // Attribute value containing &quot;
    let xml = r#"<r a="say &quot;hello&quot;"/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attr_with_amp() {
    let xml = r#"<r a="a &amp; b"/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn roundtrip_attr_with_lt() {
    let xml = r#"<r a="a &lt; b"/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

// ─── DOCTYPE handling ───────────────────────────────────────────────────────

// DOCTYPE declarations are stored on the Document but omitted by default when
// serializing. These tests pin both the secure default and the explicit trusted
// opt-in path.

#[test]
fn doctype_omitted_by_default_system() {
    let xml = r#"<?xml version="1.0"?><!DOCTYPE root SYSTEM "root.dtd"><root/>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(
        doc.doctype.as_deref(),
        Some(r#"<!DOCTYPE root SYSTEM "root.dtd">"#)
    );
    assert_eq!(doc.to_xml(), r#"<?xml version="1.0"?><root/>"#);
    let opts = uppsala::XmlWriteOptions::compact().with_doctype(true);
    assert_eq!(doc.to_xml_with_options(&opts), xml);
}

#[test]
fn doctype_omitted_by_default_public() {
    let xml = r#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd"><html/>"#;
    let doc = parse(xml).unwrap();
    assert!(doc.doctype.is_some());
    assert_eq!(doc.to_xml(), r#"<?xml version="1.0"?><html/>"#);
    let opts = uppsala::XmlWriteOptions::compact().with_doctype(true);
    assert_eq!(doc.to_xml_with_options(&opts), xml);
}

#[test]
fn doctype_omitted_by_default_internal_subset() {
    let xml =
        "<?xml version=\"1.0\"?><!DOCTYPE root [\n<!ELEMENT root (#PCDATA)>\n]><root>hello</root>";
    let doc = parse(xml).unwrap();
    assert!(doc.doctype.is_some());
    assert_eq!(doc.to_xml(), "<?xml version=\"1.0\"?><root>hello</root>");
    let opts = uppsala::XmlWriteOptions::compact().with_doctype(true);
    assert_eq!(doc.to_xml_with_options(&opts), xml);
}

#[test]
fn no_doctype_is_none() {
    let xml = "<root/>";
    let doc = parse(xml).unwrap();
    assert!(doc.doctype.is_none());
}

// ─── Escaping edge cases ────────────────────────────────────────────────────

// Escaping tests verify that the serializer emits XML syntax characters as
// entity references in text and attribute-value positions.

#[test]
fn text_escaping_amp_lt_gt() {
    let doc = parse("<r>&amp;&lt;&gt;</r>").unwrap();
    let output = doc.to_xml();
    assert_eq!(output, "<r>&amp;&lt;&gt;</r>");
}

#[test]
fn attr_escaping_quote() {
    let doc = parse(r#"<r a="&quot;"/>"#).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, r#"<r a="&quot;"/>"#);
}

// ─── Display trait ──────────────────────────────────────────────────────────

// Display is intended as a convenience wrapper over the compact XML
// serializer, so it should stay byte-equivalent with `to_xml()`.

#[test]
fn display_matches_to_xml() {
    let xml =
        r#"<?xml version="1.0" encoding="UTF-8"?><root attr="val"><child>text</child></root>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(format!("{}", doc), doc.to_xml());
}

#[test]
fn display_simple() {
    let doc = parse("<r>hello</r>").unwrap();
    assert_eq!(format!("{}", doc), "<r>hello</r>");
}

// ─── node_to_xml (subtree serialization) ────────────────────────────────────

// Subtree serialization excludes document-level metadata and writes only the
// selected node and descendants. Text nodes still need normal escaping.

#[test]
fn node_to_xml_document_element() {
    let xml = r#"<?xml version="1.0"?><root><child>text</child></root>"#;
    let doc = parse(xml).unwrap();
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
    let doc = parse(xml).unwrap();
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
    let doc = parse(xml).unwrap();
    let root_elem = doc.document_element().unwrap();
    let children = doc.children(root_elem);
    assert_eq!(doc.node_to_xml(children[0]), "hello &amp; world");
}

// ─── write_to (io::Write streaming) ────────────────────────────────────────

// The streaming API should produce the same bytes as `to_xml()` without
// requiring callers to allocate the final String themselves.

#[test]
fn write_to_vec() {
    let xml = "<root><child>text</child></root>";
    let doc = parse(xml).unwrap();
    let mut buf: Vec<u8> = Vec::new();
    doc.write_to(&mut buf).unwrap();
    assert_eq!(String::from_utf8(buf).unwrap(), xml);
}

#[test]
fn write_to_matches_to_xml() {
    let xml =
        r#"<?xml version="1.0" encoding="UTF-8"?><root attr="val"><child>text</child></root>"#;
    let doc = parse(xml).unwrap();
    let mut buf: Vec<u8> = Vec::new();
    doc.write_to(&mut buf).unwrap();
    assert_eq!(String::from_utf8(buf).unwrap(), doc.to_xml());
}

// ─── XmlWriteOptions: expand_empty_elements ─────────────────────────────────

// `expand_empty_elements` is required for canonical-style output. These tests
// verify it affects both nested empty elements and an empty document element.

#[test]
fn expand_empty_elements() {
    let xml = "<root><empty/></root>";
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::compact().with_expand_empty_elements(true);
    assert_eq!(
        doc.to_xml_with_options(&opts),
        "<root><empty></empty></root>"
    );
}

#[test]
fn expand_empty_root() {
    let xml = "<root/>";
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::compact().with_expand_empty_elements(true);
    assert_eq!(doc.to_xml_with_options(&opts), "<root></root>");
}

#[test]
fn self_closing_default() {
    let xml = "<root><empty/></root>";
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

// ─── XmlWriteOptions: pretty-printing ───────────────────────────────────────

// Pretty-printing should indent element-only content while preserving mixed
// content exactly, because injected whitespace would change text semantics.

#[test]
fn pretty_print_simple() {
    let xml = "<root><a/><b/></root>";
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let expected = "<root>\n  <a/>\n  <b/>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

#[test]
fn pretty_print_nested() {
    let xml = "<root><a><b/></a></root>";
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let expected = "<root>\n  <a>\n    <b/>\n  </a>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

#[test]
fn pretty_print_mixed_content_not_indented() {
    // Mixed content (text + elements) should NOT be indented
    let xml = "<r>text<b>bold</b>more</r>";
    let doc = parse(xml).unwrap();
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
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("\t");
    assert_eq!(doc.to_xml_with_options(&opts), "<root>\n\t<a/>\n</root>\n");
}

#[test]
fn pretty_print_with_declaration() {
    let xml = r#"<?xml version="1.0"?><root><a/></root>"#;
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let expected = "<?xml version=\"1.0\"?><root>\n  <a/>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

#[test]
fn pretty_print_expand_empty() {
    let xml = "<root><a/></root>";
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ").with_expand_empty_elements(true);
    let expected = "<root>\n  <a></a>\n</root>\n";
    assert_eq!(doc.to_xml_with_options(&opts), expected);
}

// ─── node_to_xml_with_options ───────────────────────────────────────────────

// Options passed to subtree serialization should apply to the selected node in
// the same way they apply to full-document serialization.

#[test]
fn node_to_xml_with_expand_empty() {
    let xml = "<root><a/></root>";
    let doc = parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    let opts = uppsala::XmlWriteOptions::compact().with_expand_empty_elements(true);
    assert_eq!(doc.node_to_xml_with_options(children[0], &opts), "<a></a>");
}

// ─── write_node_to_with_options ─────────────────────────────────────────────

// The node-level streaming writer must produce exactly what the String-returning
// subtree serializer produces, and must surface the sink's error instead of
// swallowing it.

/// A `fmt::Write` sink that accepts `budget` bytes and then fails.
struct FailAfter {
    budget: usize,
}

impl std::fmt::Write for FailAfter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        if s.len() > self.budget {
            return Err(std::fmt::Error);
        }
        self.budget -= s.len();
        Ok(())
    }
}

#[test]
fn write_node_to_with_options_matches_node_to_xml_in_a_reused_buffer() {
    let xml = concat!(
        r#"<md:EntitiesDescriptor xmlns:md="urn:md" xmlns:ds="urn:ds">"#,
        r#"<md:EntityDescriptor entityID="a&amp;b"><ds:Signature/>"#,
        "<md:Text>caf\u{e9} &lt;&amp;&gt;</md:Text></md:EntityDescriptor>",
        "<md:EntityDescriptor entityID=\"c\"><x xmlns=\"urn:x\"><y/></x></md:EntityDescriptor>",
        "</md:EntitiesDescriptor>"
    );
    let doc = parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let entities = doc.children(root);
    let compact = uppsala::XmlWriteOptions::compact();
    let pretty = uppsala::XmlWriteOptions::pretty("  ").with_expand_empty_elements(true);

    // One buffer, reused across fragments and option sets: the stream must
    // equal the String API byte for byte every time.
    let mut buf = String::new();
    for &id in entities.iter().chain(std::iter::once(&root)) {
        for opts in [&compact, &pretty] {
            buf.clear();
            buf.reserve(doc.node_serialized_size_hint(id));
            doc.write_node_to_with_options(id, &mut buf, opts).unwrap();
            assert_eq!(buf, doc.node_to_xml_with_options(id, opts));
        }
    }
    // The streamed fragment is a standalone, re-parsable document: the prefix
    // declared on the ancestor is not re-emitted (no `xmlns:md` is stored on
    // the child), which is the documented node_to_xml behaviour.
    buf.clear();
    doc.write_node_to_with_options(entities[0], &mut buf, &compact)
        .unwrap();
    assert!(buf.starts_with("<md:EntityDescriptor entityID=\"a&amp;b\">"));
    assert!(buf.contains("<md:Text>caf\u{e9} &lt;&amp;&gt;</md:Text>"));
    assert!(!buf.contains("xmlns:md"));
    // The size hint for a parsed node is its source length; for the whole
    // document it is the input length.
    assert_eq!(
        doc.node_serialized_size_hint(entities[0]),
        doc.node_range(entities[0]).unwrap().len()
    );
    assert_eq!(doc.node_serialized_size_hint(doc.root()), xml.len());
}

#[test]
fn write_node_to_with_options_propagates_sink_errors() {
    let doc = parse("<root><a x=\"1\">text<b/></a></root>").unwrap();
    let root = doc.document_element().unwrap();
    let opts = uppsala::XmlWriteOptions::compact();
    // Fails on the very first write.
    assert!(doc
        .write_node_to_with_options(root, &mut FailAfter { budget: 0 }, &opts)
        .is_err());
    // Fails part-way through an element (after the start tag opens).
    assert!(doc
        .write_node_to_with_options(root, &mut FailAfter { budget: 8 }, &opts)
        .is_err());
    // A sink with enough room succeeds and sees the full output.
    let full = doc.node_to_xml(root);
    let mut sink = FailAfter { budget: full.len() };
    doc.write_node_to_with_options(root, &mut sink, &opts)
        .unwrap();
    assert_eq!(sink.budget, 0);
    // A built (unparsed) node has no source range and a zero hint; writing it
    // still works.
    let mut built = uppsala::Document::new();
    let el = built.create_element(uppsala::QName::local("made"));
    let built_root = built.root();
    built.append_child(built_root, el);
    assert_eq!(built.node_serialized_size_hint(el), 0);
    let mut out = String::new();
    built
        .write_node_to_with_options(el, &mut out, &opts)
        .unwrap();
    assert_eq!(out, "<made/>");
}

// ─── Namespace declarations in serialization ────────────────────────────────

// Namespace declarations are part of the element start tag and must survive
// parse/serialize cycles so prefixed and default namespaces remain meaningful.

#[test]
fn namespace_declarations_preserved() {
    let xml = r#"<root xmlns="http://example.com"><child/></root>"#;
    let doc = parse(xml).unwrap();
    let output = doc.to_xml();
    assert!(output.contains(r#"xmlns="http://example.com""#));
}

#[test]
fn prefixed_namespace_preserved() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"><ns:child/></ns:root>"#;
    let doc = parse(xml).unwrap();
    let output = doc.to_xml();
    assert!(output.contains(r#"xmlns:ns="http://example.com""#));
    assert!(output.contains("<ns:root"));
    assert!(output.contains("<ns:child"));
}

// ─── XmlWriter builder tests ────────────────────────────────────────────────

// XmlWriter is the programmatic construction API. These tests cover the same
// XML constructs as DOM serialization, but through direct builder calls.

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
    // XML signature key material exercises nested prefixed element names.
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
    // ECKeyValue output combines a default namespace, an empty element with an
    // attribute, and base64-like text content.
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

// This pins the combination of streaming output and non-default write options.

#[test]
fn write_to_with_pretty_options() {
    let xml = "<root><a/><b/></root>";
    let doc = parse(xml).unwrap();
    let opts = uppsala::XmlWriteOptions::pretty("  ");
    let mut buf: Vec<u8> = Vec::new();
    doc.write_to_with_options(&mut buf, &opts).unwrap();
    let result = String::from_utf8(buf).unwrap();
    assert_eq!(result, "<root>\n  <a/>\n  <b/>\n</root>\n");
}

// ─── Namespace-aware serialization (issue #2) ────────────────────────────────
//
// Programmatically-built elements carry a namespace URI on their QName but no
// stored `xmlns` declaration. Serialization must synthesize the declarations
// needed for the output to be namespace-well-formed, while leaving parsed
// documents byte-identical.

#[test]
fn ns_prefixless_element_emits_default_declaration() {
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::with_namespace("urn:example", "Foo"));
    doc.append_child(doc.root(), el);
    let out = doc.to_xml();
    assert_eq!(out, r#"<Foo xmlns="urn:example"/>"#);
    // Output must re-parse and preserve the namespace.
    let re = parse(&out).unwrap();
    let root = re.document_element().unwrap();
    assert!(re
        .element(root)
        .unwrap()
        .matches_name_ns("urn:example", "Foo"));
}

#[test]
fn ns_prefixed_element_emits_prefix_declaration() {
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::full("p", "urn:foo", "Foo"));
    doc.append_child(doc.root(), el);
    assert_eq!(doc.to_xml(), r#"<p:Foo xmlns:p="urn:foo"/>"#);
    assert!(parse(&doc.to_xml()).is_ok());
}

#[test]
fn ns_child_inherits_without_redeclaring() {
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let parent = doc.create_element(QName::full("p", "urn:foo", "Parent"));
    doc.append_child(doc.root(), parent);
    let child = doc.create_element(QName::full("p", "urn:foo", "Child"));
    doc.append_child(parent, child);
    // The child reuses the in-scope binding rather than re-declaring it.
    assert_eq!(
        doc.to_xml(),
        r#"<p:Parent xmlns:p="urn:foo"><p:Child/></p:Parent>"#
    );
}

#[test]
fn ns_nested_distinct_namespaces_each_declared() {
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let parent = doc.create_element(QName::full("a", "urn:a", "Parent"));
    doc.append_child(doc.root(), parent);
    let child = doc.create_element(QName::full("b", "urn:b", "Child"));
    doc.append_child(parent, child);
    assert_eq!(
        doc.to_xml(),
        r#"<a:Parent xmlns:a="urn:a"><b:Child xmlns:b="urn:b"/></a:Parent>"#
    );
}

#[test]
fn ns_attribute_with_prefix_declared() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("Foo"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el)
        .unwrap()
        .set_attribute(QName::full("x", "urn:x", "attr"), Cow::Borrowed("v"));
    assert_eq!(doc.to_xml(), r#"<Foo xmlns:x="urn:x" x:attr="v"/>"#);
    assert!(parse(&doc.to_xml()).is_ok());
}

#[test]
fn ns_attribute_without_prefix_gets_synthetic_prefix() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("Foo"));
    doc.append_child(doc.root(), el);
    // A namespaced attribute with no prefix cannot use the default namespace, so
    // serialization allocates one.
    doc.element_mut(el)
        .unwrap()
        .set_attribute(QName::with_namespace("urn:x", "attr"), Cow::Borrowed("v"));
    let out = doc.to_xml();
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.document_element().unwrap();
    assert_eq!(
        re.element(root).unwrap().get_attribute_ns("urn:x", "attr"),
        Some("v"),
        "attribute namespace lost on round-trip: {out}"
    );
}

#[test]
fn ns_no_namespace_child_undeclares_default() {
    use uppsala::{Document, NodeKind, QName};
    let mut doc = Document::new();
    let parent = doc.create_element(QName::with_namespace("urn:p", "Parent"));
    doc.append_child(doc.root(), parent);
    let child = doc.create_element(QName::local("Child"));
    doc.append_child(parent, child);
    let out = doc.to_xml();
    assert_eq!(out, r#"<Parent xmlns="urn:p"><Child xmlns=""/></Parent>"#);
    // The child must round-trip back to no namespace.
    let re = parse(&out).unwrap();
    let proot = re.document_element().unwrap();
    let pchild = re
        .children(proot)
        .into_iter()
        .find(|&c| matches!(re.node_kind(c), Some(NodeKind::Element(_))))
        .unwrap();
    assert!(
        re.element(pchild).unwrap().name.namespace_uri.is_none(),
        "child should be in no namespace: {out}"
    );
}

#[test]
fn ns_fragment_serialization_is_self_contained() {
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let parent = doc.create_element(QName::full("p", "urn:foo", "Parent"));
    doc.append_child(doc.root(), parent);
    let child = doc.create_element(QName::full("p", "urn:foo", "Child"));
    doc.append_child(parent, child);
    // Serializing the child on its own declares the namespace it uses.
    assert_eq!(doc.node_to_xml(child), r#"<p:Child xmlns:p="urn:foo"/>"#);
}

#[test]
fn ns_parsed_document_roundtrips_byte_identical() {
    // The synthesis path must be a no-op for parsed documents: stored
    // declarations already satisfy every QName.
    let xml = r#"<p:Foo xmlns:p="urn:foo" xmlns="urn:def"><p:Bar/><Baz/></p:Foo>"#;
    let doc = parse(xml).unwrap();
    assert_eq!(doc.to_xml(), xml);
}

#[test]
fn ns_same_prefix_conflict_does_not_misbind() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    // The element carries a stored declaration `p -> urn:old`, but its QName uses
    // prefix `p` bound to `urn:new`. A start tag cannot declare `p` twice, so the
    // element name must be rewritten to a fresh prefix that is bound to `urn:new`
    // — otherwise `p:Foo` would silently resolve to `urn:old`.
    let mut doc = Document::new();
    let el = doc.create_element(QName::full("p", "urn:new", "Foo"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el)
        .unwrap()
        .namespace_declarations
        .push((Cow::Borrowed("p"), Cow::Borrowed("urn:old")));

    let out = doc.to_xml();
    // Output must re-parse and the element must actually be in urn:new.
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.document_element().unwrap();
    assert_eq!(
        re.element(root).unwrap().name.namespace_uri.as_deref(),
        Some("urn:new"),
        "element silently re-bound to the wrong namespace: {out}"
    );
    // The conflicting stored prefix is preserved for any descendants that use it.
    assert!(
        out.contains(r#"xmlns:p="urn:old""#),
        "stored declaration dropped: {out}"
    );
}

#[test]
fn ns_stored_default_does_not_capture_no_namespace_element() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    // The element is in no namespace but carries a stored default-namespace
    // declaration (xmlns="urn:x"). Emitting that stored declaration would put the
    // element in urn:x, so the synthesized `xmlns=""` undeclaration must win and
    // the stored default must be suppressed.
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("Foo"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el)
        .unwrap()
        .namespace_declarations
        .push((Cow::Borrowed(""), Cow::Borrowed("urn:x")));

    let out = doc.to_xml();
    assert_eq!(out, r#"<Foo xmlns=""/>"#);
    let re = parse(&out).unwrap();
    let root = re.document_element().unwrap();
    assert!(
        re.element(root).unwrap().name.namespace_uri.is_none(),
        "element wrongly captured by stored default namespace: {out}"
    );
}

#[test]
fn ns_xml_prefix_with_foreign_uri_is_reassigned() {
    use uppsala::{Document, QName};
    // The `xml` prefix is reserved and bound to the XML namespace. A QName that
    // uses prefix `xml` for a *different* URI must not serialize as `xml:...`,
    // which would silently re-bind it to the XML namespace on re-parse.
    let mut doc = Document::new();
    let el = doc.create_element(QName::full("xml", "urn:custom", "Foo"));
    doc.append_child(doc.root(), el);
    let out = doc.to_xml();
    assert!(
        !out.contains("<xml:Foo"),
        "reserved xml prefix misused: {out}"
    );
    let re = parse(&out).unwrap();
    let root = re.document_element().unwrap();
    assert_eq!(
        re.element(root).unwrap().name.namespace_uri.as_deref(),
        Some("urn:custom"),
        "element silently re-bound to the XML namespace: {out}"
    );
}

#[test]
fn ns_xml_namespace_uses_reserved_prefix_without_declaration() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    // The genuine XML namespace must serialize with the `xml` prefix and never be
    // declared (it is implicitly bound).
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el).unwrap().set_attribute(
        QName::full("xml", "http://www.w3.org/XML/1998/namespace", "lang"),
        Cow::Borrowed("en"),
    );
    let out = doc.to_xml();
    assert_eq!(out, r#"<r xml:lang="en"/>"#);
    assert!(parse(&out).is_ok());
}

#[test]
fn ns_xml_prefixed_attribute_with_foreign_uri_is_reassigned() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el)
        .unwrap()
        .set_attribute(QName::full("xml", "urn:custom", "a"), Cow::Borrowed("v"));
    let out = doc.to_xml();
    assert!(
        !out.contains("xml:a="),
        "reserved xml prefix misused on attribute: {out}"
    );
    let re = parse(&out).unwrap();
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(root.get_attribute_ns("urn:custom", "a"), Some("v"), "{out}");
}

#[test]
fn ns_two_attributes_same_prefix_distinct_uris() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    // Two attributes both want prefix `p` but for different URIs; only one can
    // keep `p`, the other must be rewritten so both resolve correctly.
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("Foo"));
    doc.append_child(doc.root(), el);
    {
        let e = doc.element_mut(el).unwrap();
        e.set_attribute(QName::full("p", "urn:a", "one"), Cow::Borrowed("1"));
        e.set_attribute(QName::full("p", "urn:b", "two"), Cow::Borrowed("2"));
    }
    let out = doc.to_xml();
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(root.get_attribute_ns("urn:a", "one"), Some("1"), "{out}");
    assert_eq!(root.get_attribute_ns("urn:b", "two"), Some("2"), "{out}");
}

#[test]
fn ns_xmlns_prefix_does_not_masquerade_as_declaration() {
    use uppsala::{Document, QName};
    // The `xmlns` prefix is reserved and implicitly bound to the XMLNS
    // namespace. A programmatic QName using prefix `xmlns` must not serialize as
    // `xmlns:...`: an `xmlns:`-prefixed element name would re-parse bound to the
    // XMLNS namespace (silently losing `urn:custom`), and an `xmlns`/`xmlns:*`
    // attribute would be parsed as a namespace declaration instead of an
    // attribute.
    let mut doc = Document::new();
    let el = doc.create_element(QName::full("xmlns", "urn:custom", "Foo"));
    doc.append_child(doc.root(), el);
    let out = doc.to_xml();
    assert!(
        !out.contains("<xmlns:Foo"),
        "reserved xmlns prefix misused as element name: {out}"
    );
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.document_element().unwrap();
    assert_eq!(
        re.element(root).unwrap().name.namespace_uri.as_deref(),
        Some("urn:custom"),
        "element silently lost its namespace: {out}"
    );
}

#[test]
fn ns_xmlns_prefixed_attribute_does_not_masquerade_as_declaration() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el)
        .unwrap()
        .set_attribute(QName::full("xmlns", "urn:custom", "a"), Cow::Borrowed("v"));
    let out = doc.to_xml();
    assert!(
        !out.contains("xmlns:a="),
        "reserved xmlns prefix misused as attribute name: {out}"
    );
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(root.get_attribute_ns("urn:custom", "a"), Some("v"), "{out}");
}

#[test]
fn ns_reserved_prefix_strip_is_single_pass_idempotent() {
    // Regression (fuzz_roundtrip): the parser leniently accepts a multi-colon
    // element name like `xmlns:xmlns:C` (prefix `xmlns`, local `xmlns:C`). When
    // the serializer strips the reserved `xmlns` prefix it must sanitize the
    // remaining local name as an NCName; emitting the raw `xmlns:C` re-parses as
    // a *new* `xmlns`-prefixed element, so serialization strips one layer per
    // round instead of being a one-pass fixpoint. Assert `to_xml()` output is a
    // fixed point of parse->serialize for the whole family of stacked names.
    for input in [
        "<xmlns:xmlns:C/>",
        "<xmlns:xmlns:xmlns:Z/>",
        "<a><xmlns:xmlns:V/></a>",
        "<xml:xml:foo/>",
    ] {
        let out1 = parse(input).unwrap().to_xml();
        let out2 = parse(&out1).unwrap().to_xml();
        assert_eq!(
            out1, out2,
            "serialization not a one-pass fixpoint for {input:?}: {out1:?} -> {out2:?}"
        );
        // And the emitted name carries no colon (it is a bare NCName or `_`).
        let inner = out1.trim_start_matches("<a>").trim_end_matches("</a>");
        assert!(
            !inner
                .trim_start_matches('<')
                .split(['/', '>'])
                .next()
                .unwrap()
                .contains(':'),
            "emitted element name still contains a colon: {out1:?}"
        );
    }
}

#[test]
fn ns_xmlns_namespace_uri_is_dropped_on_serialization() {
    use uppsala::{Document, QName};
    // The XMLNS namespace cannot be bound to any prefix (the parser ignores such
    // bindings), so a QName carrying it is serialized without a namespace rather
    // than emitting a forbidden `xmlns:nsN="...2000/xmlns/"` declaration that
    // would not be namespace-well-formed.
    let mut doc = Document::new();
    let el = doc.create_element(QName::full("p", "http://www.w3.org/2000/xmlns/", "Foo"));
    doc.append_child(doc.root(), el);
    let out = doc.to_xml();
    assert!(
        !out.contains("2000/xmlns"),
        "forbidden XMLNS binding emitted: {out}"
    );
    assert!(
        !out.contains("xmlns:") && !out.starts_with("<xmlns"),
        "XMLNS namespace serialized as a declaration: {out}"
    );
    // Output must be well-formed and re-parse with the namespace dropped.
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.document_element().unwrap();
    assert_eq!(
        re.element(root).unwrap().name.local_name.as_ref(),
        "Foo",
        "{out}"
    );
}

#[test]
fn ns_xmlns_namespace_attribute_is_dropped_on_serialization() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    doc.element_mut(el).unwrap().set_attribute(
        QName::full("p", "http://www.w3.org/2000/xmlns/", "a"),
        Cow::Borrowed("v"),
    );
    let out = doc.to_xml();
    assert!(
        !out.contains("2000/xmlns") && !out.contains("xmlns:"),
        "forbidden XMLNS binding emitted on attribute: {out}"
    );
    // Well-formed and re-parses; the bare attribute survives without a namespace.
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(root.get_attribute("a"), Some("v"), "{out}");
}

#[test]
fn ns_stored_reserved_declarations_are_filtered() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    // Stored namespace declarations holding reserved bindings (the `xmlns`
    // prefix, the `xml` prefix bound to a non-XML URI, or any prefix bound to the
    // XMLNS namespace) must not be emitted: the parser's resolver ignores them, so
    // serializing them would produce namespace-not-well-formed output.
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    {
        let e = doc.element_mut(el).unwrap();
        e.namespace_declarations
            .push((Cow::Borrowed("xmlns"), Cow::Borrowed("urn:bad")));
        e.namespace_declarations
            .push((Cow::Borrowed("xml"), Cow::Borrowed("urn:notxml")));
        e.namespace_declarations.push((
            Cow::Borrowed("p"),
            Cow::Borrowed("http://www.w3.org/2000/xmlns/"),
        ));
    }
    let out = doc.to_xml();
    assert_eq!(out, "<r/>", "reserved declarations were emitted: {out}");
    assert!(parse(&out).is_ok(), "output not well-formed: {out}");
}

#[test]
fn ns_invalid_inscope_prefix_is_not_reused() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    // An in-scope prefix that is not a valid XML NCName must not be reused for a
    // namespaced-but-prefixless QName: doing so would yield a name like
    // `bad prefix:attr` that sanitizes to `_` (dropping the local name). A fresh
    // valid prefix is allocated instead, so the attribute still re-parses into its
    // namespace.
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    {
        let e = doc.element_mut(el).unwrap();
        // Stored declaration with an invalid (non-NCName) prefix bound to urn:x.
        e.namespace_declarations
            .push((Cow::Borrowed("bad prefix"), Cow::Borrowed("urn:x")));
        // Prefixless attribute in the same namespace would trigger prefix reuse.
        e.set_attribute(QName::with_namespace("urn:x", "attr"), Cow::Borrowed("v"));
    }
    let out = doc.to_xml();
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(
        root.get_attribute_ns("urn:x", "attr"),
        Some("v"),
        "attribute lost its namespace via invalid prefix reuse: {out}"
    );
}

#[test]
fn ns_reserved_element_prefix_without_uri_is_stripped() {
    use uppsala::{Document, QName};
    // A QName carrying a reserved prefix but no namespace URI must not serialize
    // verbatim: `xml`/`xmlns` are implicitly bound, so `xml:Foo`/`xmlns:Foo`
    // would re-parse into the XML/XMLNS namespace, silently changing the
    // element's namespace. The bare local name is emitted instead.
    for prefix in ["xml", "xmlns"] {
        let mut doc = Document::new();
        let mut name = QName::local("Foo");
        name.prefix = Some(std::borrow::Cow::Borrowed(prefix));
        let el = doc.create_element(name);
        doc.append_child(doc.root(), el);
        let out = doc.to_xml();
        assert_eq!(
            out, "<Foo/>",
            "reserved prefix {prefix} not stripped: {out}"
        );
        let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
        let root = re.element(re.document_element().unwrap()).unwrap();
        assert!(
            root.name.namespace_uri.is_none(),
            "spurious namespace: {out}"
        );
    }
}

#[test]
fn ns_reserved_attribute_prefix_without_uri_is_stripped() {
    use std::borrow::Cow;
    use uppsala::{Document, QName};
    for prefix in ["xml", "xmlns"] {
        let mut doc = Document::new();
        let el = doc.create_element(QName::local("r"));
        doc.append_child(doc.root(), el);
        let mut name = QName::local("a");
        name.prefix = Some(Cow::Borrowed(prefix));
        doc.element_mut(el)
            .unwrap()
            .set_attribute(name, Cow::Borrowed("v"));
        let out = doc.to_xml();
        assert_eq!(
            out, r#"<r a="v"/>"#,
            "reserved prefix {prefix} not stripped: {out}"
        );
        let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
        let root = re.element(re.document_element().unwrap()).unwrap();
        assert_eq!(root.get_attribute("a"), Some("v"), "{out}");
    }
}

#[test]
fn declare_namespace_emits_prefixed_declaration() {
    use uppsala::{Document, QName};
    // declare_namespace records a binding in namespace_declarations, so it
    // serializes as a real xmlns:p="..." declaration (not an attribute).
    let mut doc = Document::new();
    let el = doc.create_element(QName::full("p", "urn:ex", "Foo"));
    doc.append_child(doc.root(), el);
    assert!(doc.declare_namespace(el, Some("p"), "urn:ex"));
    let out = doc.to_xml();
    assert_eq!(out, r#"<p:Foo xmlns:p="urn:ex"/>"#, "{out}");
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(root.name.namespace_uri.as_deref(), Some("urn:ex"), "{out}");
}

#[test]
fn declare_namespace_default_on_element_in_that_namespace() {
    use uppsala::{Document, QName};
    // An element in `urn:ex` with a declared default namespace serializes as
    // xmlns="urn:ex" (the lxml clark+nsmap shape), with the URI preserved.
    let mut doc = Document::new();
    let el = doc.create_element(QName::with_namespace("urn:ex", "kml"));
    doc.append_child(doc.root(), el);
    assert!(doc.declare_namespace(el, None, "urn:ex"));
    let out = doc.to_xml();
    assert_eq!(out, r#"<kml xmlns="urn:ex"/>"#, "{out}");
    let re = parse(&out).unwrap_or_else(|e| panic!("must re-parse: {out} ({e})"));
    let root = re.element(re.document_element().unwrap()).unwrap();
    assert_eq!(root.name.namespace_uri.as_deref(), Some("urn:ex"), "{out}");
}

#[test]
fn declare_namespace_upserts_existing_prefix() {
    use uppsala::{Document, QName};
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    doc.declare_namespace(el, Some("p"), "urn:old");
    doc.declare_namespace(el, Some("p"), "urn:new"); // updates in place
    let decls = &doc.element(el).unwrap().namespace_declarations;
    assert_eq!(decls.len(), 1, "prefix p must not be duplicated");
    assert_eq!(decls[0].1.as_ref(), "urn:new");
    // Non-element node returns false.
    let txt = doc.create_text("x");
    assert!(!doc.declare_namespace(txt, Some("p"), "urn:x"));
}

#[test]
fn ns_reserved_prefix_strip_undeclares_default_namespace() {
    // Regression (fuzz_roundtrip, ADR 0017): stripping a reserved `xml`/`xmlns`
    // prefix leaves an unprefixed name in no namespace. If a non-empty default
    // namespace is in scope, the bare name must be emitted with `xmlns=""`;
    // otherwise the re-parse captures it into the default namespace, silently
    // changing the element's namespace (and, when the default was the XML
    // namespace, breaking the byte-level fixpoint via `xml:` re-prefixing).
    let input = r#"<r xmlns="urn:x"><xmlns:c/></r>"#;
    let out1 = parse(input).unwrap().to_xml();
    assert!(
        out1.contains(r#"<c xmlns=""/>"#),
        "stripped name must undeclare the default namespace: {out1}"
    );
    let doc2 = parse(&out1).unwrap();
    assert_eq!(out1, doc2.to_xml(), "not a one-pass fixpoint: {out1}");
    let root = doc2.document_element().unwrap();
    let child = doc2.children_iter(root).next().unwrap();
    assert_eq!(
        doc2.element(child).unwrap().name.namespace_uri.as_deref(),
        None,
        "stripped element must stay in no namespace: {out1}"
    );
}

#[test]
fn ns_default_declaration_of_xml_namespace_is_never_emitted() {
    use uppsala::{Document, QName};
    // A programmatic default-namespace binding to the XML namespace is not
    // representable (the parser rejects `xmlns="...XML/1998/namespace"`), so
    // the serializer drops it rather than emit output that does not re-parse.
    let mut doc = Document::new();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(doc.root(), el);
    doc.declare_namespace(el, None, "http://www.w3.org/XML/1998/namespace");
    let out = doc.to_xml();
    assert_eq!(out, "<r/>", "forbidden default declaration must be dropped");
    parse(&out).expect("output must re-parse");
}
