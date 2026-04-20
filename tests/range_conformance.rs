//! Tests for node_range(), node_source(), and input_text() APIs.

#[test]
fn test_node_range_element() {
    let xml = r#"<root><child>text</child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(
        &xml[doc.node_range(root).unwrap()],
        "<root><child>text</child></root>"
    );
    let child = doc.children(root)[0];
    assert_eq!(&xml[doc.node_range(child).unwrap()], "<child>text</child>");
}

#[test]
fn test_node_range_self_closing() {
    let xml = r#"<root><br/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let br = doc.children(root)[0];
    assert_eq!(&xml[doc.node_range(br).unwrap()], "<br/>");
}

#[test]
fn test_node_range_with_attributes() {
    let xml = r#"<root><item id="1" class="foo">content</item></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let item = doc.children(root)[0];
    assert_eq!(
        &xml[doc.node_range(item).unwrap()],
        r#"<item id="1" class="foo">content</item>"#
    );
}

#[test]
fn test_node_range_with_namespaces() {
    let xml = r#"<root xmlns:ds="urn:ds"><ds:Sig xmlns:ds11="urn:11"/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let sig = doc.children(root)[0];
    assert_eq!(
        &xml[doc.node_range(sig).unwrap()],
        r#"<ds:Sig xmlns:ds11="urn:11"/>"#
    );
}

#[test]
fn test_node_range_text_node() {
    let xml = "<root>hello world</root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let text = doc.children(root)[0];
    let range = doc.node_range(text).unwrap();
    assert_eq!(&xml[range], "hello world");
}

#[test]
fn test_node_range_comment() {
    let xml = "<root><!-- a comment --></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let comment = doc.children(root)[0];
    let range = doc.node_range(comment).unwrap();
    assert_eq!(&xml[range], "<!-- a comment -->");
}

#[test]
fn test_node_range_pi() {
    let xml = "<root><?target data?></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let pi = doc.children(root)[0];
    let range = doc.node_range(pi).unwrap();
    assert_eq!(&xml[range], "<?target data?>");
}

#[test]
fn test_node_range_programmatic_returns_none() {
    let mut doc = uppsala::Document::new();
    let root = doc.root();
    let elem = doc.create_element(uppsala::QName::local("foo"));
    doc.append_child(root, elem);
    assert!(doc.node_range(elem).is_none());
    assert!(doc.node_source(elem).is_none());
}

#[test]
fn test_node_source() {
    let xml = r#"<root><item id="1">hello</item><br/></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(doc.node_source(root).unwrap(), xml);
    let children = doc.children(root);
    assert_eq!(
        doc.node_source(children[0]).unwrap(),
        r#"<item id="1">hello</item>"#
    );
    assert_eq!(doc.node_source(children[1]).unwrap(), "<br/>");
}

#[test]
fn test_input_text() {
    let xml = "<root>test</root>";
    let doc = uppsala::parse(xml).unwrap();
    assert_eq!(doc.input_text(), xml);
}

#[test]
fn test_node_range_nested_elements() {
    let xml = "<a><b><c>deep</c></b></a>";
    let doc = uppsala::parse(xml).unwrap();
    let a = doc.document_element().unwrap();
    let b = doc.children(a)[0];
    let c = doc.children(b)[0];
    assert_eq!(
        &xml[doc.node_range(a).unwrap()],
        "<a><b><c>deep</c></b></a>"
    );
    assert_eq!(&xml[doc.node_range(b).unwrap()], "<b><c>deep</c></b>");
    assert_eq!(&xml[doc.node_range(c).unwrap()], "<c>deep</c>");
}

#[test]
fn test_node_range_mixed_content() {
    let xml = "<root>text1<child/>text2</root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    // text1
    assert_eq!(&xml[doc.node_range(children[0]).unwrap()], "text1");
    // <child/>
    assert_eq!(&xml[doc.node_range(children[1]).unwrap()], "<child/>");
    // text2
    assert_eq!(&xml[doc.node_range(children[2]).unwrap()], "text2");
}

#[test]
fn test_node_range_prolog_comment() {
    let xml = "<!-- prolog --><root/>";
    let doc = uppsala::parse(xml).unwrap();
    let doc_children = doc.children(doc.root());
    // First child of document node should be the comment
    let comment = doc_children[0];
    assert_eq!(&xml[doc.node_range(comment).unwrap()], "<!-- prolog -->");
}

#[test]
fn test_node_range_prolog_pi() {
    let xml = "<?mypi data?><root/>";
    let doc = uppsala::parse(xml).unwrap();
    let doc_children = doc.children(doc.root());
    let pi = doc_children[0];
    assert_eq!(&xml[doc.node_range(pi).unwrap()], "<?mypi data?>");
}

#[test]
fn test_node_range_with_xml_declaration() {
    let xml = r#"<?xml version="1.0"?><root><item>text</item></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    assert_eq!(
        &xml[doc.node_range(root).unwrap()],
        "<root><item>text</item></root>"
    );
}

#[test]
fn test_node_range_cdata() {
    let xml = "<root><![CDATA[some <data>]]></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let cdata = doc.children(root)[0];
    let range = doc.node_range(cdata).unwrap();
    assert_eq!(&xml[range], "<![CDATA[some <data>]]>");
}

#[test]
fn test_node_range_empty_text() {
    // An element with only whitespace text
    let xml = "<root> </root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let text = doc.children(root)[0];
    assert_eq!(&xml[doc.node_range(text).unwrap()], " ");
}

#[test]
fn test_node_source_returns_none_for_static() {
    let xml = "<root>test</root>";
    let doc = uppsala::parse(xml).unwrap();
    let static_doc = doc.into_static();
    let root = static_doc.document_element().unwrap();
    // into_static clears input, so node_source should return None
    assert!(static_doc.node_source(root).is_none());
    // But node_range should still work (byte offsets preserved)
    assert!(static_doc.node_range(root).is_some());
}
