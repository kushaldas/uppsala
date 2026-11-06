use std::borrow::Cow;
use uppsala::xslt::Stylesheet;
use uppsala::{Parser, QName};

fn style(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:template match="/">{body}</xsl:template></xsl:stylesheet>"#
    )
}
fn check(output: &str, local: &str, uri: &str, count: usize) {
    let doc = Parser::new().parse(output).unwrap();
    let mut found = 0;
    for id in doc.descendants(doc.document_element().unwrap()) {
        if let Some(e) = doc.element(id) {
            for a in &e.attributes {
                if a.name.local_name == local {
                    assert_eq!(a.name.namespace_uri.as_deref(), Some(uri), "{output}");
                    found += 1;
                }
            }
        }
    }
    assert_eq!(found, count, "{output}");
}
#[test]
fn preserves_constructed_attributes() {
    for keep in [r#"p:keep="x""#, ""] {
        for copy_first in [false, true] {
            let computed = if keep.is_empty() {
                r#"<xsl:attribute xmlns:p="urn:style" name="{'p:keep'}">x</xsl:attribute>"#
            } else {
                ""
            };
            let copy = r#"<xsl:copy-of select="root/@*"/>"#;
            let body = if copy_first {
                format!("{copy}{computed}")
            } else {
                format!("{computed}{copy}")
            };
            let outer_uri = if keep.is_empty() {
                "urn:outer"
            } else {
                "urn:style"
            };
            let xsl = style(&format!(
                r#"<out xmlns:p="{outer_uri}"><v {keep}>{body}</v></out>"#
            ));
            let sd = Parser::new().parse(&xsl).unwrap();
            let mut src = Parser::new()
                .parse(r#"<root xmlns:p="urn:source" p:type="s"/>"#)
                .unwrap();
            src.prepare_xpath();
            let out = Stylesheet::compile(&sd).unwrap().transform(&src).unwrap();
            check(&out, "keep", "urn:style", 1);
            check(&out, "type", "urn:source", 1);
        }
    }
}
#[test]
fn nearest_binding_wins() {
    let xsl = style(
        r#"<out xmlns:p="urn:source"><mid xmlns:p="urn:other"><v><xsl:copy-of select="root/@*"/></v></mid></out>"#,
    );
    let sd = Parser::new().parse(&xsl).unwrap();
    let mut src = Parser::new()
        .parse(r#"<root xmlns:p="urn:source" p:type="s"/>"#)
        .unwrap();
    src.prepare_xpath();
    let out = Stylesheet::compile(&sd).unwrap().transform(&src).unwrap();
    check(&out, "type", "urn:source", 1);
}
#[test]
fn prefixless_dom_attributes() {
    for copy in [
        r#"<v><xsl:copy-of select="root/@*"/></v>"#,
        r#"<xsl:copy-of select="root"/>"#,
        r#"<v><xsl:for-each select="root/@*"><xsl:copy/></xsl:for-each></v>"#,
    ] {
        let xsl = style(&format!("<out>{copy}</out>"));
        let sd = Parser::new().parse(&xsl).unwrap();
        let mut src = Parser::new().parse("<root/>").unwrap();
        let root = src.document_element().unwrap();
        let e = src.element_mut(root).unwrap();
        e.set_attribute(
            QName::with_namespace("urn:source", "type"),
            Cow::Borrowed("s"),
        );
        e.set_attribute(
            QName::with_namespace("http://www.w3.org/XML/1998/namespace", "lang"),
            Cow::Borrowed("en"),
        );
        src.prepare_xpath();
        let out = Stylesheet::compile(&sd).unwrap().transform(&src).unwrap();
        check(&out, "type", "urn:source", 1);
        check(&out, "lang", "http://www.w3.org/XML/1998/namespace", 1);
    }
}
#[test]
fn many_conflicting_attributes_reuse_binding() {
    let attrs: String = (0..500).map(|i| format!(" p:a{i}='v'")).collect();
    let input = format!("<root xmlns:p='urn:source'{attrs}/>");
    let xsl = style(r#"<out><v xmlns:p="urn:other"><xsl:copy-of select="root/@*"/></v></out>"#);
    let sd = Parser::new().parse(&xsl).unwrap();
    let mut src = Parser::new().parse(&input).unwrap();
    src.prepare_xpath();
    let out = Stylesheet::compile(&sd).unwrap().transform(&src).unwrap();
    for i in 0..500 {
        check(&out, &format!("a{i}"), "urn:source", 1);
    }
    assert_eq!(out.matches("=\"urn:source\"").count(), 1);
}

#[test]
fn computed_attribute_explicit_namespace_and_xsl_binding() {
    for (instruction, expected) in [
        (
            r#"<xsl:attribute xmlns:p="urn:other" name="p:a" namespace="{'urn:target'}">v</xsl:attribute>"#,
            Some("urn:target"),
        ),
        (
            r#"<xsl:attribute name="unbound:a" namespace="urn:target">v</xsl:attribute>"#,
            Some("urn:target"),
        ),
        (
            r#"<xsl:attribute name="a" namespace="urn:target">v</xsl:attribute>"#,
            Some("urn:target"),
        ),
        (
            r#"<xsl:attribute name="p:a" namespace="">v</xsl:attribute>"#,
            None,
        ),
        (
            r#"<xsl:attribute name="xsl:a">v</xsl:attribute>"#,
            Some("http://www.w3.org/1999/XSL/Transform"),
        ),
    ] {
        let xsl = style(&format!(
            r#"<out xmlns:p="urn:target"><v>{instruction}</v></out>"#
        ));
        let sd = Parser::new().parse(&xsl).unwrap();
        let src = Parser::new().parse("<root/>").unwrap();
        let output = Stylesheet::compile(&sd).unwrap().transform(&src).unwrap();
        let doc = Parser::new().parse(&output).unwrap();
        let child = doc.children(doc.document_element().unwrap())[0];
        let a = &doc.element(child).unwrap().attributes[0];
        assert_eq!(a.name.local_name, "a");
        assert_eq!(a.name.namespace_uri.as_deref(), expected, "{output}");
    }
}

#[test]
fn namespace_fixup_charges_qname_growth() {
    let long_prefix = "n".repeat(1000);
    let attrs: String = (0..200).map(|i| format!(" p:a{i}='v'")).collect();
    let input = format!("<root xmlns:p='urn:source'{attrs}/>");
    let xsl = style(&format!(
        r#"<out xmlns:p="urn:other" xmlns:{long_prefix}="urn:source"><xsl:copy-of select="root/@*"/></out>"#
    ));
    let sd = Parser::new().parse(&xsl).unwrap();
    let mut src = Parser::new().parse(&input).unwrap();
    src.prepare_xpath();
    let sheet = Stylesheet::compile(&sd)
        .unwrap()
        .set_max_result_tree_bytes(100_000);
    assert!(sheet.transform(&src).is_err());
    assert!(Stylesheet::compile(&sd).unwrap().transform(&src).is_ok());

    // Deep-copy fixup also grows prefixless XML attribute names.
    let mut src = Parser::new().parse("<root/>").unwrap();
    let root = src.document_element().unwrap();
    for i in 0..200 {
        src.element_mut(root).unwrap().set_attribute(
            QName::with_namespace("http://www.w3.org/XML/1998/namespace", format!("a{i}")),
            Cow::Borrowed("v"),
        );
    }
    let xsl = style(r#"<out><xsl:copy-of select="root"/></out>"#);
    let sd = Parser::new().parse(&xsl).unwrap();
    // Budget includes both elements and original attribute storage, but only
    // half the 800 bytes required by the added "xml:" prefixes.
    let original_bytes = 32
        + "out".len()
        + 32
        + "root".len()
        + (0..200)
            .map(|i| 16 + format!("a{i}").len() + 1 + "http://www.w3.org/XML/1998/namespace".len())
            .sum::<usize>();
    let output = Stylesheet::compile(&sd).unwrap().transform(&src).unwrap();
    assert!(output.contains("xml:a199"));
    assert!(Stylesheet::compile(&sd)
        .unwrap()
        .set_max_result_tree_bytes(original_bytes + 400)
        .transform(&src)
        .is_err());
}

fn computed_attribute_result(name: &str, namespace: &str) -> uppsala::XmlResult<String> {
    let xsl = style(&format!(
        r#"<out><xsl:attribute name="{name}" namespace="{namespace}">urn:value</xsl:attribute><child/></out>"#
    ));
    let sd = Parser::new().parse(&xsl)?;
    let src = Parser::new().parse("<root/>")?;
    Stylesheet::compile(&sd)?.transform(&src)
}

#[test]
fn reserved_xml_prefix_is_not_rebound() {
    for namespace in ["urn:target", "{'urn:target'}"] {
        let output = computed_attribute_result("xml:a", namespace).unwrap();
        let doc = Parser::new().parse(&output).unwrap();
        let e = doc.element(doc.document_element().unwrap()).unwrap();
        let a = &e.attributes[0];
        assert_eq!(a.name.namespace_uri.as_deref(), Some("urn:target"));
        assert_ne!(a.name.prefix.as_deref(), Some("xml"));
        assert_eq!(a.name.local_name, "a");
        assert_eq!(a.value, "urn:value");
        assert!(e.name.namespace_uri.is_none());
    }
    let output =
        computed_attribute_result("xml:a", "http://www.w3.org/XML/1998/namespace").unwrap();
    assert!(Parser::new().parse(&output).is_ok());
    assert!(output.contains("xml:a="));
}

#[test]
fn normalized_xmlns_attribute_is_rejected() {
    for name in ["p:xmlns", "{'p:xmlns'}", "xml:xmlns"] {
        for namespace in ["", "{''}"] {
            assert!(
                computed_attribute_result(name, namespace).is_err(),
                "{name}"
            );
        }
    }
    // A namespaced attribute with local name xmlns is an ordinary attribute.
    let output = computed_attribute_result("p:xmlns", "urn:target").unwrap();
    let doc = Parser::new().parse(&output).unwrap();
    let e = doc.element(doc.document_element().unwrap()).unwrap();
    assert!(e.name.namespace_uri.is_none());
    assert_eq!(
        e.attributes[0].name.namespace_uri.as_deref(),
        Some("urn:target")
    );
}

#[test]
fn explicit_xmlns_namespace_is_rejected() {
    for name in ["a", "p:a", "xml:a"] {
        for namespace in [
            "http://www.w3.org/2000/xmlns/",
            "{'http://www.w3.org/2000/xmlns/'}",
        ] {
            assert!(
                computed_attribute_result(name, namespace).is_err(),
                "{name}"
            );
        }
    }
}
