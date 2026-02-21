//! Integration tests for XPath 1.0.
//!
//! Tests cover axes, node tests, predicates, functions, operators,
//! and the attribute axis implementation.

use uppsala::dom::{NodeId, NodeKind, QName};
use uppsala::xpath::XPathValue;

fn parse_and_eval(xml: &str, xpath: &str) -> XPathValue {
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    eval.evaluate(&doc, root, xpath).unwrap()
}

fn parse_and_select(xml: &str, xpath: &str) -> Vec<NodeId> {
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    eval.select_nodes(&doc, root, xpath).unwrap()
}

// ─── Attribute axis ─────────────────────────────────────

#[test]
fn attribute_axis_simple() {
    let xml = r#"<root attr="hello"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let result = eval.evaluate(&doc, root, "@attr").unwrap();
    assert_eq!(result.to_string_value(&doc), "hello");
}

#[test]
fn attribute_axis_multiple() {
    let xml = r#"<root a="1" b="2" c="3"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();

    assert_eq!(
        eval.evaluate(&doc, root, "@a")
            .unwrap()
            .to_string_value(&doc),
        "1"
    );
    assert_eq!(
        eval.evaluate(&doc, root, "@b")
            .unwrap()
            .to_string_value(&doc),
        "2"
    );
    assert_eq!(
        eval.evaluate(&doc, root, "@c")
            .unwrap()
            .to_string_value(&doc),
        "3"
    );
}

#[test]
fn attribute_axis_wildcard() {
    let xml = r#"<root a="1" b="2" c="3"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, "@*").unwrap();
    assert_eq!(nodes.len(), 3);
}

#[test]
fn attribute_axis_nonexistent() {
    let xml = r#"<root attr="hello"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let result = eval.evaluate(&doc, root, "@nonexistent").unwrap();
    // Should be empty node set
    assert_eq!(result.to_string_value(&doc), "");
}

#[test]
fn attribute_axis_in_predicate() {
    let xml = r#"<root><item id="a">first</item><item id="b">second</item><item id="c">third</item></root>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, "item[@id='b']").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(doc.text_content_deep(nodes[0]), "second");
}

#[test]
fn attribute_axis_string_value() {
    let xml = r#"<root attr="world"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let result = eval
        .evaluate(&doc, root, "concat('hello ', @attr)")
        .unwrap();
    assert_eq!(result.to_string_value(&doc), "hello world");
}

#[test]
fn attribute_axis_on_child_element() {
    let xml = r#"<root><child attr="val"/></root>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let result = eval.evaluate(&doc, root, "child/@attr").unwrap();
    assert_eq!(result.to_string_value(&doc), "val");
}

#[test]
fn attribute_axis_unabbreviated() {
    let xml = r#"<root attr="hello"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let result = eval.evaluate(&doc, root, "attribute::attr").unwrap();
    assert_eq!(result.to_string_value(&doc), "hello");
}

// ─── Child axis ─────────────────────────────────────────

#[test]
fn child_axis_elements() {
    let nodes = parse_and_select("<root><a/><b/><c/></root>", "child::*");
    assert_eq!(nodes.len(), 3);
}

#[test]
fn child_axis_named() {
    let nodes = parse_and_select("<root><a/><b/><a/></root>", "a");
    assert_eq!(nodes.len(), 2);
}

#[test]
fn child_axis_text() {
    let nodes = parse_and_select("<root>hello</root>", "text()");
    assert_eq!(nodes.len(), 1);
}

// ─── Descendant axis ────────────────────────────────────

#[test]
fn descendant_axis() {
    let nodes = parse_and_select(
        "<root><a><b/></a><c><d><e/></d></c></root>",
        "descendant::*",
    );
    // a, b, c, d, e
    assert_eq!(nodes.len(), 5);
}

#[test]
fn descendant_or_self_axis() {
    let nodes = parse_and_select("<root><a><b/></a></root>", "descendant-or-self::*");
    // root, a, b
    assert_eq!(nodes.len(), 3);
}

#[test]
fn double_slash_abbreviation() {
    let nodes = parse_and_select("<root><a><item/></a><b><item/></b></root>", ".//item");
    assert_eq!(nodes.len(), 2);
}

// ─── Parent axis ────────────────────────────────────────

#[test]
fn parent_axis() {
    let xml = "<root><child/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, child, "..").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0], root);
}

// ─── Self axis ──────────────────────────────────────────

#[test]
fn self_axis() {
    let xml = "<root/>";
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, "self::*").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0], root);
}

#[test]
fn self_axis_name_match() {
    let xml = "<root/>";
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, "self::root").unwrap();
    assert_eq!(nodes.len(), 1);
    let nodes = eval.select_nodes(&doc, root, "self::other").unwrap();
    assert_eq!(nodes.len(), 0);
}

// ─── Ancestor axis ──────────────────────────────────────

#[test]
fn ancestor_axis() {
    let xml = "<a><b><c/></b></a>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let b = doc.children(root)[0];
    let c = doc.children(b)[0];
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, c, "ancestor::*").unwrap();
    // b and a
    assert_eq!(nodes.len(), 2);
}

// ─── Sibling axes ───────────────────────────────────────

#[test]
fn following_sibling_axis() {
    let xml = "<root><a/><b/><c/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let a = doc.children(root)[0];
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, a, "following-sibling::*").unwrap();
    assert_eq!(nodes.len(), 2);
}

#[test]
fn preceding_sibling_axis() {
    let xml = "<root><a/><b/><c/></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let children = doc.children(root);
    let c = children[2];
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, c, "preceding-sibling::*").unwrap();
    assert_eq!(nodes.len(), 2);
}

// ─── Predicates ─────────────────────────────────────────

#[test]
fn predicate_position_number() {
    let nodes = parse_and_select("<root><a/><b/><c/></root>", "*[2]");
    assert_eq!(nodes.len(), 1);
    // Second child
}

#[test]
fn predicate_last() {
    let nodes = parse_and_select("<root><a/><b/><c/></root>", "*[last()]");
    assert_eq!(nodes.len(), 1);
}

#[test]
fn predicate_boolean() {
    let nodes = parse_and_select("<root><a/><b/><c/></root>", "*[position() > 1]");
    assert_eq!(nodes.len(), 2);
}

#[test]
fn predicate_nested() {
    let xml = r#"<root><item id="1"><sub/></item><item id="2"/><item id="3"><sub/></item></root>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, "item[sub]").unwrap();
    assert_eq!(nodes.len(), 2); // items with sub children
}

// ─── Absolute paths ─────────────────────────────────────

#[test]
fn absolute_path_from_root() {
    let xml = "<root><child>text</child></root>";
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let child = doc.children(root)[0];
    let eval = uppsala::XPathEvaluator::new();
    // Evaluate from a child but use absolute path
    let nodes = eval.select_nodes(&doc, child, "/root/child").unwrap();
    assert_eq!(nodes.len(), 1);
}

// ─── Operators ──────────────────────────────────────────

#[test]
fn operator_equality() {
    let val = parse_and_eval("<r/>", "1 = 1");
    assert!(val.to_boolean());
}

#[test]
fn operator_inequality() {
    let val = parse_and_eval("<r/>", "1 != 2");
    assert!(val.to_boolean());
}

#[test]
fn operator_less_than() {
    let val = parse_and_eval("<r/>", "1 < 2");
    assert!(val.to_boolean());
}

#[test]
fn operator_greater_than() {
    let val = parse_and_eval("<r/>", "2 > 1");
    assert!(val.to_boolean());
}

#[test]
fn operator_less_equal() {
    let val = parse_and_eval("<r/>", "1 <= 1");
    assert!(val.to_boolean());
}

#[test]
fn operator_greater_equal() {
    let val = parse_and_eval("<r/>", "2 >= 1");
    assert!(val.to_boolean());
}

#[test]
fn operator_and() {
    let val = parse_and_eval("<r/>", "true() and true()");
    assert!(val.to_boolean());
    let val = parse_and_eval("<r/>", "true() and false()");
    assert!(!val.to_boolean());
}

#[test]
fn operator_or() {
    let val = parse_and_eval("<r/>", "false() or true()");
    assert!(val.to_boolean());
    let val = parse_and_eval("<r/>", "false() or false()");
    assert!(!val.to_boolean());
}

#[test]
fn operator_addition() {
    let val = parse_and_eval("<r/>", "2 + 3");
    match val {
        XPathValue::Number(n) => assert!((n - 5.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn operator_subtraction() {
    let val = parse_and_eval("<r/>", "10 - 4");
    match val {
        XPathValue::Number(n) => assert!((n - 6.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn operator_multiplication() {
    let val = parse_and_eval("<r/>", "3 * 4");
    match val {
        XPathValue::Number(n) => assert!((n - 12.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn operator_div() {
    let val = parse_and_eval("<r/>", "10 div 3");
    match val {
        XPathValue::Number(n) => assert!((n - 10.0 / 3.0).abs() < 1e-10),
        _ => panic!("Expected number"),
    }
}

#[test]
fn operator_mod() {
    let val = parse_and_eval("<r/>", "10 mod 3");
    match val {
        XPathValue::Number(n) => assert!((n - 1.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn operator_union() {
    let xml = "<root><a/><b/><c/></root>";
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, "a | c").unwrap();
    assert_eq!(nodes.len(), 2);
}

// ─── Functions ──────────────────────────────────────────

#[test]
fn function_count() {
    let val = parse_and_eval("<root><a/><b/><c/></root>", "count(*)");
    match val {
        XPathValue::Number(n) => assert!((n - 3.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_position() {
    // Within a predicate context
    let nodes = parse_and_select("<root><a/><b/><c/></root>", "*[position() = 2]");
    assert_eq!(nodes.len(), 1);
}

#[test]
fn function_last() {
    let nodes = parse_and_select("<root><a/><b/><c/></root>", "*[position() = last()]");
    assert_eq!(nodes.len(), 1);
}

#[test]
fn function_string() {
    let val = parse_and_eval("<r>hello</r>", "string()");
    assert_eq!(
        val.to_string_value(&uppsala::parse("<r/>").unwrap()),
        "hello"
    );
}

#[test]
fn function_concat() {
    let val = parse_and_eval("<r/>", "concat('a', 'b', 'c')");
    match val {
        XPathValue::String(s) => assert_eq!(s, "abc"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_starts_with() {
    let val = parse_and_eval("<r/>", "starts-with('hello', 'hel')");
    assert!(val.to_boolean());
    let val = parse_and_eval("<r/>", "starts-with('hello', 'xyz')");
    assert!(!val.to_boolean());
}

#[test]
fn function_contains() {
    let val = parse_and_eval("<r/>", "contains('hello world', 'world')");
    assert!(val.to_boolean());
    let val = parse_and_eval("<r/>", "contains('hello', 'xyz')");
    assert!(!val.to_boolean());
}

#[test]
fn function_substring() {
    let val = parse_and_eval("<r/>", "substring('12345', 2, 3)");
    match val {
        XPathValue::String(s) => assert_eq!(s, "234"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_substring_before() {
    let val = parse_and_eval("<r/>", "substring-before('1999/04/01', '/')");
    match val {
        XPathValue::String(s) => assert_eq!(s, "1999"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_substring_after() {
    let val = parse_and_eval("<r/>", "substring-after('1999/04/01', '/')");
    match val {
        XPathValue::String(s) => assert_eq!(s, "04/01"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_string_length() {
    let val = parse_and_eval("<r/>", "string-length('hello')");
    match val {
        XPathValue::Number(n) => assert!((n - 5.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_normalize_space() {
    let val = parse_and_eval("<r/>", "normalize-space('  hello   world  ')");
    match val {
        XPathValue::String(s) => assert_eq!(s, "hello world"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_translate() {
    let val = parse_and_eval("<r/>", "translate('bar', 'abc', 'ABC')");
    match val {
        XPathValue::String(s) => assert_eq!(s, "BAr"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_not() {
    let val = parse_and_eval("<r/>", "not(false())");
    assert!(val.to_boolean());
    let val = parse_and_eval("<r/>", "not(true())");
    assert!(!val.to_boolean());
}

#[test]
fn function_true_false() {
    let val = parse_and_eval("<r/>", "true()");
    assert!(val.to_boolean());
    let val = parse_and_eval("<r/>", "false()");
    assert!(!val.to_boolean());
}

#[test]
fn function_number() {
    let val = parse_and_eval("<r/>", "number('42')");
    match val {
        XPathValue::Number(n) => assert!((n - 42.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_sum() {
    let xml = "<root><a>1</a><b>2</b><c>3</c></root>";
    let val = parse_and_eval(xml, "sum(*)");
    match val {
        XPathValue::Number(n) => assert!((n - 6.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_floor() {
    let val = parse_and_eval("<r/>", "floor(2.7)");
    match val {
        XPathValue::Number(n) => assert!((n - 2.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_ceiling() {
    let val = parse_and_eval("<r/>", "ceiling(2.3)");
    match val {
        XPathValue::Number(n) => assert!((n - 3.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_round() {
    let val = parse_and_eval("<r/>", "round(2.5)");
    match val {
        XPathValue::Number(n) => assert!((n - 3.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
    let val = parse_and_eval("<r/>", "round(2.4)");
    match val {
        XPathValue::Number(n) => assert!((n - 2.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }
}

#[test]
fn function_boolean_coercion() {
    // number 0 -> false, nonzero -> true
    let val = parse_and_eval("<r/>", "boolean(0)");
    assert!(!val.to_boolean());
    let val = parse_and_eval("<r/>", "boolean(1)");
    assert!(val.to_boolean());
    // empty string -> false, non-empty -> true
    let val = parse_and_eval("<r/>", "boolean('')");
    assert!(!val.to_boolean());
    let val = parse_and_eval("<r/>", "boolean('x')");
    assert!(val.to_boolean());
}

#[test]
fn function_local_name() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let val = eval.evaluate(&doc, root, "local-name()").unwrap();
    match val {
        XPathValue::String(s) => assert_eq!(s, "root"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_name() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let val = eval.evaluate(&doc, root, "name()").unwrap();
    match val {
        XPathValue::String(s) => assert_eq!(s, "ns:root"),
        _ => panic!("Expected string"),
    }
}

#[test]
fn function_namespace_uri() {
    let xml = r#"<ns:root xmlns:ns="http://example.com"/>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let val = eval.evaluate(&doc, root, "namespace-uri()").unwrap();
    match val {
        XPathValue::String(s) => assert_eq!(s, "http://example.com"),
        _ => panic!("Expected string"),
    }
}

// ─── Complex XPath expressions ──────────────────────────

#[test]
fn complex_path_with_predicates_and_attributes() {
    let xml = r#"<library>
  <book id="1" genre="fiction"><title>Book A</title></book>
  <book id="2" genre="science"><title>Book B</title></book>
  <book id="3" genre="fiction"><title>Book C</title></book>
</library>"#;
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();

    // Find fiction books
    let fiction = eval
        .select_nodes(&doc, root, "book[@genre='fiction']")
        .unwrap();
    assert_eq!(fiction.len(), 2);

    // Count all books
    let count = eval.evaluate(&doc, root, "count(book)").unwrap();
    match count {
        XPathValue::Number(n) => assert!((n - 3.0).abs() < f64::EPSILON),
        _ => panic!("Expected number"),
    }

    // Get title of second book
    let title = eval.evaluate(&doc, root, "book[2]/title").unwrap();
    assert_eq!(title.to_string_value(&doc), "Book B");
}

#[test]
fn xpath_on_deeply_nested() {
    let xml = "<a><b><c><d><e>deep</e></d></c></b></a>";
    let mut doc = uppsala::parse(xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let eval = uppsala::XPathEvaluator::new();
    let nodes = eval.select_nodes(&doc, root, ".//e").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(doc.text_content_deep(nodes[0]), "deep");
}

// ─── Namespace-aware XPath ──────────────────────────────

#[test]
fn xpath_with_namespace_prefix() {
    let xml = r#"<root xmlns:ns="http://example.com"><ns:child>hello</ns:child></root>"#;
    let doc = uppsala::parse(xml).unwrap();
    let root = doc.document_element().unwrap();
    let mut eval = uppsala::XPathEvaluator::new();
    eval.add_namespace("ns", "http://example.com");
    let nodes = eval.select_nodes(&doc, root, "ns:child").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(doc.text_content_deep(nodes[0]), "hello");
}
