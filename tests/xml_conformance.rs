//! Integration tests for XML 1.0 (Fifth Edition) conformance.
//!
//! These tests cover well-formedness constraints, character handling,
//! entity references, CDATA sections, comments, processing instructions,
//! XML declarations, and edge cases from the W3C XML 1.0 specification.

use uppsala::dom::{NodeKind, QName};
use uppsala::error::XmlError;

// ─── Well-formed documents ───────────────────────────────

#[test]
fn minimal_document() {
    let doc = uppsala::parse("<r/>").unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.name.local_name, "r");
}

#[test]
fn simple_element_with_text() {
    let doc = uppsala::parse("<greeting>Hello, world!</greeting>").unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "Hello, world!");
}

#[test]
fn nested_elements() {
    let xml = "<a><b><c>deep</c></b></a>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children_a = doc.children(root);
    assert_eq!(children_a.len(), 1);
    let b = children_a[0];
    let children_b = doc.children(b);
    assert_eq!(children_b.len(), 1);
    let c = children_b[0];
    assert_eq!(doc.text_content_deep(c), "deep");
}

#[test]
fn multiple_children() {
    let xml = "<root><a/><b/><c/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 3);
    let names: Vec<_> = children
        .iter()
        .filter_map(|&id| doc.element(id).map(|e| &*e.name.local_name))
        .collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn mixed_content() {
    let xml = "<p>Hello <em>world</em>!</p>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "Hello world!");
    let children = doc.children(root);
    assert_eq!(children.len(), 3); // text, em, text
}

#[test]
fn self_closing_elements() {
    let xml = "<root><br/><hr /><img  /></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 3);
}

#[test]
fn empty_element_with_children_none() {
    let xml = "<empty></empty>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 0);
}

// ─── Attributes ──────────────────────────────────────────

#[test]
fn single_attribute() {
    let xml = r#"<root attr="value"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("attr"), Some("value"));
}

#[test]
fn multiple_attributes() {
    let xml = r#"<root a="1" b="2" c="3"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("a"), Some("1"));
    assert_eq!(elem.get_attribute("b"), Some("2"));
    assert_eq!(elem.get_attribute("c"), Some("3"));
    assert_eq!(elem.attributes.len(), 3);
}

#[test]
fn attribute_single_quotes() {
    let xml = "<root attr='value'/>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("attr"), Some("value"));
}

#[test]
fn attribute_with_entities() {
    let xml = r#"<root attr="a&amp;b&lt;c&gt;d&quot;e&apos;f"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("attr"), Some("a&b<c>d\"e'f"));
}

#[test]
fn attribute_with_character_references() {
    let xml = r#"<root attr="&#65;&#x42;"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("attr"), Some("AB"));
}

#[test]
fn attribute_empty_value() {
    let xml = r#"<root attr=""/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("attr"), Some(""));
}

// ─── Entity references in content ────────────────────────

#[test]
fn entity_references_in_text() {
    let xml = "<r>&lt;&gt;&amp;&quot;&apos;</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "<>&\"'");
}

#[test]
fn character_references_decimal() {
    let xml = "<r>&#65;&#66;&#67;</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "ABC");
}

#[test]
fn character_references_hex() {
    let xml = "<r>&#x41;&#x42;&#x43;</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "ABC");
}

#[test]
fn unicode_character_reference() {
    let xml = "<r>&#x2603;</r>"; // snowman
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "\u{2603}");
}

// ─── CDATA sections ─────────────────────────────────────

#[test]
fn cdata_section_basic() {
    let xml = "<r><![CDATA[Hello <world> & friends]]></r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "Hello <world> & friends");
}

#[test]
fn cdata_section_empty() {
    let xml = "<r><![CDATA[]]></r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "");
}

#[test]
fn cdata_preserves_whitespace() {
    let xml = "<r><![CDATA[  spaces  \n  and newlines  ]]></r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "  spaces  \n  and newlines  ");
}

// ─── Comments ───────────────────────────────────────────

#[test]
fn comment_basic() {
    let xml = "<r><!-- this is a comment --></r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 1);
    match doc.node_kind(children[0]) {
        Some(NodeKind::Comment(c)) => assert_eq!(c, " this is a comment "),
        other => panic!("Expected comment, got {:?}", other),
    }
}

#[test]
fn comment_in_prolog() {
    let xml = "<!-- prolog comment --><r/>";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

#[test]
fn comment_after_root() {
    let xml = "<r/><!-- trailing comment -->";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

// ─── Processing instructions ────────────────────────────

#[test]
fn processing_instruction_basic() {
    let xml = "<r><?target data?></r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 1);
    match doc.node_kind(children[0]) {
        Some(NodeKind::ProcessingInstruction(pi)) => {
            assert_eq!(pi.target, "target");
            assert_eq!(pi.data.as_deref(), Some("data"));
        }
        other => panic!("Expected PI, got {:?}", other),
    }
}

#[test]
fn processing_instruction_no_data() {
    let xml = "<r><?target?></r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 1);
    match doc.node_kind(children[0]) {
        Some(NodeKind::ProcessingInstruction(pi)) => {
            assert_eq!(pi.target, "target");
            assert!(pi.data.is_none() || pi.data.as_deref() == Some(""));
        }
        other => panic!("Expected PI, got {:?}", other),
    }
}

#[test]
fn processing_instruction_in_prolog() {
    let xml = "<?xml-stylesheet type='text/xsl' href='style.xsl'?><r/>";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

// ─── XML declaration ────────────────────────────────────

#[test]
fn xml_declaration_version_only() {
    let xml = "<?xml version=\"1.0\"?><r/>";
    let doc = uppsala::parse(xml).unwrap();
    let decl = doc.xml_declaration.as_ref().unwrap();
    assert_eq!(decl.version, "1.0");
    assert!(decl.encoding.is_none());
    assert!(decl.standalone.is_none());
}

#[test]
fn xml_declaration_with_encoding() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><r/>";
    let doc = uppsala::parse(xml).unwrap();
    let decl = doc.xml_declaration.as_ref().unwrap();
    assert_eq!(decl.version, "1.0");
    assert_eq!(decl.encoding.as_deref(), Some("UTF-8"));
}

#[test]
fn xml_declaration_with_standalone() {
    let xml = "<?xml version=\"1.0\" standalone=\"yes\"?><r/>";
    let doc = uppsala::parse(xml).unwrap();
    let decl = doc.xml_declaration.as_ref().unwrap();
    assert_eq!(decl.standalone, Some(true));
}

#[test]
fn xml_declaration_full() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?><r/>";
    let doc = uppsala::parse(xml).unwrap();
    let decl = doc.xml_declaration.as_ref().unwrap();
    assert_eq!(decl.version, "1.0");
    assert_eq!(decl.encoding.as_deref(), Some("UTF-8"));
    assert_eq!(decl.standalone, Some(false));
}

// ─── Well-formedness errors ─────────────────────────────

#[test]
fn error_mismatched_tags() {
    let result = uppsala::parse("<a></b>");
    assert!(result.is_err());
}

#[test]
fn error_no_root_element() {
    let result = uppsala::parse("");
    assert!(result.is_err());
}

#[test]
fn error_two_root_elements() {
    let result = uppsala::parse("<a/><b/>");
    assert!(result.is_err());
}

#[test]
fn error_duplicate_attributes() {
    let result = uppsala::parse(r#"<r a="1" a="2"/>"#);
    assert!(result.is_err());
}

#[test]
fn error_unclosed_element() {
    let result = uppsala::parse("<r>");
    assert!(result.is_err());
}

#[test]
fn error_ampersand_in_content() {
    // Bare & in content is not well-formed
    let result = uppsala::parse("<r>a & b</r>");
    assert!(result.is_err());
}

#[test]
fn error_lt_in_attribute() {
    // < in attribute value is not well-formed
    let result = uppsala::parse(r#"<r a="<"/>"#);
    assert!(result.is_err());
}

#[test]
fn error_double_hyphen_in_comment() {
    // -- inside a comment is not well-formed
    let result = uppsala::parse("<r><!-- -- --></r>");
    assert!(result.is_err());
}

#[test]
fn error_cdata_end_in_content() {
    // ]]> in regular text content is not well-formed
    let result = uppsala::parse("<r>]]></r>");
    assert!(result.is_err());
}

#[test]
fn error_pi_target_xml() {
    // PI target "xml" (any case variation) is reserved
    let result = uppsala::parse("<r><?XML data?></r>");
    assert!(result.is_err());
}

// ─── Line ending normalization ──────────────────────────

#[test]
fn line_ending_crlf_to_lf() {
    let xml = "<r>line1\r\nline2</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "line1\nline2");
}

#[test]
fn line_ending_bare_cr_to_lf() {
    let xml = "<r>line1\rline2</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "line1\nline2");
}

// ─── BOM handling ───────────────────────────────────────

#[test]
fn bom_utf8_skipped() {
    let xml = "\u{FEFF}<r/>";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

// ─── Whitespace handling ────────────────────────────────

#[test]
fn whitespace_in_prolog() {
    let xml = "  \n  <r/>";
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

#[test]
fn whitespace_between_elements() {
    let xml = "<root>\n  <child/>\n  <child/>\n</root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    // Text nodes contain whitespace between elements
    let children = doc.children(root);
    assert!(children.len() >= 2); // At least the 2 child elements
}

// ─── Serialization roundtrip ────────────────────────────

#[test]
fn roundtrip_simple() {
    let xml = "<root><child>text</child></root>";
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, xml);
}

#[test]
fn roundtrip_self_closing() {
    let xml = "<root><empty/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, xml);
}

#[test]
fn roundtrip_attributes() {
    let xml = r#"<root attr="value"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, xml);
}

#[test]
fn roundtrip_entities_in_text() {
    let xml = "<r>&lt;&amp;&gt;</r>";
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, xml);
}

#[test]
fn roundtrip_xml_declaration() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><r/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let output = doc.to_xml();
    assert_eq!(output, xml);
}

// ─── DOCTYPE handling (should be skipped) ───────────────

#[test]
fn doctype_skipped() {
    let xml = r#"<?xml version="1.0"?><!DOCTYPE root SYSTEM "root.dtd"><root/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

#[test]
fn doctype_with_internal_subset() {
    let xml = r#"<!DOCTYPE root [<!ELEMENT root EMPTY>]><root/>"#;
    let doc = uppsala::parse(xml).unwrap();
    assert!(doc.document_element().is_some());
}

// ─── Deeply nested documents ────────────────────────────

#[test]
fn deeply_nested_100_levels() {
    let mut xml = String::new();
    for i in 0..100 {
        xml.push_str(&format!("<n{}>", i));
    }
    xml.push_str("leaf");
    for i in (0..100).rev() {
        xml.push_str(&format!("</n{}>", i));
    }
    let doc = uppsala::parse(&xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.name.local_name, "n0");
}

// ─── Unicode content ────────────────────────────────────

#[test]
fn unicode_element_content() {
    let xml = "<r>日本語テスト</r>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.text_content_deep(root), "日本語テスト");
}

#[test]
fn unicode_in_attribute() {
    let xml = r#"<r attr="日本語"/>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let elem = doc.element(root).unwrap();
    assert_eq!(elem.get_attribute("attr"), Some("日本語"));
}

// ─── DOM mutation ───────────────────────────────────────

#[test]
fn dom_append_child() {
    let mut doc = uppsala::parse("<root/>").unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.create_element(QName::local("child"));
    doc.append_child(root, child);
    let children = doc.children(root);
    assert_eq!(children.len(), 1);
    let elem = doc.element(children[0]).unwrap();
    assert_eq!(elem.name.local_name, "child");
}

#[test]
fn dom_remove_child() {
    let mut doc = uppsala::parse("<root><a/><b/><c/></root>").unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(children.len(), 3);
    let b = children[1];
    doc.remove_child(root, b);
    let children = doc.children(root);
    assert_eq!(children.len(), 2);
    let names: Vec<_> = children
        .iter()
        .filter_map(|&id| doc.element(id).map(|e| e.name.local_name.clone()))
        .collect();
    assert_eq!(names, vec!["a", "c"]);
}

#[test]
fn dom_insert_before() {
    let mut doc = uppsala::parse("<root><a/><c/></root>").unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    let c = children[1];
    let b = doc.create_element(QName::local("b"));
    doc.insert_before(root, b, c);
    let children = doc.children(root);
    assert_eq!(children.len(), 3);
    let names: Vec<_> = children
        .iter()
        .filter_map(|&id| doc.element(id).map(|e| e.name.local_name.clone()))
        .collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn dom_replace_child() {
    let mut doc = uppsala::parse("<root><old/></root>").unwrap();
    let root = doc.document_element().unwrap();
    let old = doc.children(root)[0];
    let new_elem = doc.create_element(QName::local("new"));
    doc.replace_child(root, new_elem, old);
    let children = doc.children(root);
    assert_eq!(children.len(), 1);
    let elem = doc.element(children[0]).unwrap();
    assert_eq!(elem.name.local_name, "new");
}

#[test]
fn dom_text_node() {
    let mut doc = uppsala::parse("<root/>").unwrap();
    let root = doc.document_element().unwrap();
    let text = doc.create_text("hello");
    doc.append_child(root, text);
    assert_eq!(doc.text_content_deep(root), "hello");
}

// ─── Navigation ─────────────────────────────────────────

#[test]
fn navigation_parent() {
    let doc = uppsala::parse("<root><child/></root>").unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];
    assert_eq!(doc.parent(child), Some(root));
}

#[test]
fn navigation_siblings() {
    let doc = uppsala::parse("<root><a/><b/><c/></root>").unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    let a = children[0];
    let b = children[1];
    let c = children[2];
    assert_eq!(doc.next_sibling(a), Some(b));
    assert_eq!(doc.next_sibling(b), Some(c));
    assert_eq!(doc.next_sibling(c), None);
    assert_eq!(doc.previous_sibling(c), Some(b));
    assert_eq!(doc.previous_sibling(b), Some(a));
    assert_eq!(doc.previous_sibling(a), None);
}

#[test]
fn navigation_first_last_child() {
    let doc = uppsala::parse("<root><a/><b/><c/></root>").unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    assert_eq!(doc.first_child(root), Some(children[0]));
    assert_eq!(doc.last_child(root), Some(children[2]));
}

#[test]
fn navigation_ancestors() {
    let doc = uppsala::parse("<a><b><c/></b></a>").unwrap();
    let root = doc.document_element().unwrap();
    let b = doc.children(root)[0];
    let c = doc.children(b)[0];
    let ancestors = doc.ancestors(c);
    // ancestors: b, a (root element), document node
    assert_eq!(ancestors.len(), 3);
    assert_eq!(doc.element(ancestors[0]).unwrap().name.local_name, "b");
    assert_eq!(doc.element(ancestors[1]).unwrap().name.local_name, "a");
}

#[test]
fn navigation_descendants() {
    let doc = uppsala::parse("<a><b><c/><d/></b><e/></a>").unwrap();
    let root = doc.document_element().unwrap();
    let descendants = doc.descendants(root);
    // b, c, d, e
    assert_eq!(descendants.len(), 4);
}

// ─── Element search ─────────────────────────────────────

#[test]
fn get_elements_by_tag_name() {
    let doc = uppsala::parse("<root><item/><nested><item/></nested><item/></root>").unwrap();
    let items = doc.get_elements_by_tag_name("item");
    assert_eq!(items.len(), 3);
}

#[test]
fn get_elements_by_tag_name_ns() {
    let xml = r#"<root xmlns:ns="http://example.com"><ns:item/><item/><ns:item/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let items = doc.get_elements_by_tag_name_ns("http://example.com", "item");
    assert_eq!(items.len(), 2);
}

// ─── Large document ─────────────────────────────────────

#[test]
fn large_document_1000_elements() {
    let mut xml = String::from("<root>");
    for i in 0..1000 {
        xml.push_str(&format!("<item id=\"{}\">text{}</item>", i, i));
    }
    xml.push_str("</root>");
    let doc = uppsala::parse(&xml).unwrap();
    let items = doc.get_elements_by_tag_name("item");
    assert_eq!(items.len(), 1000);
}
