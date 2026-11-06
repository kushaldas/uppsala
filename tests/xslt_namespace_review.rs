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
