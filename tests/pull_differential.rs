//! Differential coverage for the public pull parser path.
//!
//! The stable DOM parser delegates to `PullParser` internally, so this is not
//! an independent XML oracle. It does explicitly pin the pull-to-DOM entry
//! point and option wiring against the same regression-oriented corpus used by
//! the normal parser tests.

use uppsala::pull::{document_from_pull, PullEvent, PullParser};
use uppsala::{Document, Parser, XmlResult, XmlWriteOptions};

fn full_xml(doc: &Document<'_>) -> String {
    doc.to_xml_with_options(&XmlWriteOptions::compact().with_doctype(true))
}

fn root_range(doc: &Document<'_>) -> Option<std::ops::Range<usize>> {
    doc.document_element().and_then(|root| doc.node_range(root))
}

fn unwrap_doc<'a>(result: XmlResult<Document<'a>>, name: &str, path: &str) -> Document<'a> {
    match result {
        Ok(doc) => doc,
        Err(err) => panic!("{name}: {path} parse failed: {err}"),
    }
}

fn unwrap_err_text<T>(result: XmlResult<T>, name: &str, path: &str) -> String {
    match result {
        Ok(_) => panic!("{name}: {path} parse unexpectedly succeeded"),
        Err(err) => err.to_string(),
    }
}

/// Drive the raw event stream (no DOM) to exhaustion, enforcing the stream
/// invariants of ADR 0018 on every event: start/end element balance with
/// matching names and depths, namespace-event balance, in-bounds byte ranges,
/// and a fused iterator after an error. Returns `Ok(())` on clean exhaustion
/// or the error text of the first failure.
fn scan_events(mut pull: PullParser<'_>, input: &str, name: &str) -> Result<(), String> {
    let mut open: Vec<(String, u32)> = Vec::new();
    let mut ns_starts = 0usize;
    let mut ns_ends = 0usize;

    let check_range = |what: &str, byte_start: usize, byte_end: usize| {
        assert!(
            byte_start <= byte_end && byte_end <= input.len(),
            "{name}: {what} byte range {byte_start}..{byte_end} out of bounds (len {})",
            input.len()
        );
    };

    while let Some(item) = pull.next() {
        let event = match item {
            Ok(event) => event,
            Err(err) => {
                assert!(
                    pull.next().is_none(),
                    "{name}: iterator not fused after error"
                );
                return Err(err.to_string());
            }
        };
        match event {
            PullEvent::StartElement {
                name: qname,
                byte_start,
                byte_end,
                depth,
                ..
            } => {
                check_range("StartElement", byte_start, byte_end);
                assert_eq!(
                    depth as usize,
                    open.len(),
                    "{name}: StartElement depth mismatch"
                );
                open.push((qname.to_string(), depth));
            }
            PullEvent::EndElement {
                name: qname,
                byte_start,
                byte_end,
                depth,
            } => {
                check_range("EndElement", byte_start, byte_end);
                let (open_name, open_depth) = open
                    .pop()
                    .unwrap_or_else(|| panic!("{name}: EndElement without matching start"));
                assert_eq!(qname.to_string(), open_name, "{name}: EndElement name");
                assert_eq!(depth, open_depth, "{name}: EndElement depth");
            }
            PullEvent::StartNamespace { .. } => ns_starts += 1,
            PullEvent::EndNamespace => {
                ns_ends += 1;
                assert!(
                    ns_ends <= ns_starts,
                    "{name}: EndNamespace without matching start"
                );
            }
            PullEvent::Text {
                content,
                byte_start,
                byte_end,
            } => {
                check_range("Text", byte_start, byte_end);
                assert!(!content.is_empty(), "{name}: empty Text event");
            }
            PullEvent::CData {
                byte_start,
                byte_end,
                ..
            } => check_range("CData", byte_start, byte_end),
            PullEvent::Comment {
                byte_start,
                byte_end,
                ..
            } => check_range("Comment", byte_start, byte_end),
            PullEvent::ProcessingInstruction {
                byte_start,
                byte_end,
                ..
            } => check_range("ProcessingInstruction", byte_start, byte_end),
            PullEvent::XmlDeclaration(_) | PullEvent::Doctype(_) => {}
        }
    }

    assert!(
        open.is_empty(),
        "{name}: {} elements left open after exhaustion",
        open.len()
    );
    assert_eq!(
        ns_starts, ns_ends,
        "{name}: unbalanced namespace events after exhaustion"
    );
    Ok(())
}

fn assert_pull_dom_matches_parser<'a>(
    name: &str,
    xml: &'a str,
    parser: Parser,
    make_pull: impl Fn() -> PullParser<'a>,
) {
    let normal = unwrap_doc(parser.parse(xml), name, "Parser::parse");
    let from_pull = unwrap_doc(
        document_from_pull(xml, make_pull()),
        name,
        "document_from_pull",
    );

    assert_eq!(
        full_xml(&from_pull),
        full_xml(&normal),
        "{name}: serialized XML differs"
    );
    assert_eq!(
        from_pull.xml_declaration, normal.xml_declaration,
        "{name}: XML declaration differs"
    );
    assert_eq!(from_pull.doctype, normal.doctype, "{name}: DOCTYPE differs");
    assert_eq!(
        root_range(&from_pull),
        root_range(&normal),
        "{name}: document-element source range differs"
    );

    if let Err(err) = scan_events(make_pull(), xml, name) {
        panic!("{name}: scan-only event stream failed: {err}");
    }
}

fn assert_default_match(name: &str, xml: &str) {
    assert_pull_dom_matches_parser(name, xml, Parser::new(), || PullParser::new(xml));
}

fn assert_same_failure<'a>(
    name: &str,
    xml: &'a str,
    parser: Parser,
    make_pull: impl Fn() -> PullParser<'a>,
) {
    let normal = unwrap_err_text(parser.parse(xml), name, "Parser::parse");
    let from_pull = unwrap_err_text(
        document_from_pull(xml, make_pull()),
        name,
        "document_from_pull",
    );
    assert_eq!(from_pull, normal, "{name}: error text differs");

    let scan = scan_events(make_pull(), xml, name).expect_err(&format!(
        "{name}: scan-only event stream unexpectedly succeeded"
    ));
    assert_eq!(scan, normal, "{name}: scan error text differs");
}

fn nested_document(depth: usize) -> String {
    let mut xml = String::new();
    for _ in 0..depth {
        xml.push_str("<n>");
    }
    xml.push('x');
    for _ in 0..depth {
        xml.push_str("</n>");
    }
    xml
}

#[test]
fn pull_dom_matches_parser_for_regression_corpus() {
    for (name, xml) in [
        ("minimal document", "<r/>"),
        ("simple text", "<greeting>Hello, world!</greeting>"),
        ("mixed content", "<p>Hello <em>world</em>!</p>"),
        ("self closing siblings", "<root><br/><hr /><img  /></root>"),
        (
            "xml declaration",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?><root attr="value" foo='bar'/>"#,
        ),
        ("predefined entities", "<root>&lt;&gt;&amp;&apos;&quot;</root>"),
        ("numeric character references", "<root>&#65;&#x41;</root>"),
        ("hex character references", "<root>&#x41;&#x42;&#x43;</root>"),
        ("unicode character reference", "<root>&#x2603;</root>"),
        (
            "attribute entities",
            r#"<root attr="a&amp;b&lt;c&gt;d&quot;e&apos;f"/>"#,
        ),
        ("attribute character references", r#"<root attr="&#65;&#x42;"/>"#),
        ("empty attribute", r#"<root attr=""/>"#),
        ("cdata", "<root><![CDATA[<not>&xml;]]></root>"),
        ("empty cdata", "<r><![CDATA[]]></r>"),
        (
            "cdata whitespace",
            "<r><![CDATA[  spaces  \n  and newlines  ]]></r>",
        ),
        (
            "comments and pi",
            "<root><!-- a comment --><?xml-stylesheet type=\"text/xsl\"?><child/>tail</root>",
        ),
        ("prolog comment", "<!-- prolog comment --><r/>"),
        ("trailing comment", "<r/><!-- trailing comment -->"),
        ("pi no data", "<r><?target?></r>"),
        (
            "prolog pi",
            "<?xml-stylesheet type='text/xsl' href='style.xsl'?><r/>",
        ),
        (
            "doctype system",
            r#"<?xml version="1.0"?><!DOCTYPE root SYSTEM "root.dtd"><root/>"#,
        ),
        (
            "doctype public",
            r#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd"><html/>"#,
        ),
        (
            "doctype internal subset",
            "<?xml version=\"1.0\"?><!DOCTYPE root [\n<!ELEMENT root (#PCDATA)>\n]><root>hello</root>",
        ),
        (
            "entity expansion",
            r#"<!DOCTYPE r [<!ENTITY x "hi">]><r>&x;</r>"#,
        ),
        (
            "nested entity expansion",
            r#"<!DOCTYPE r [<!ENTITY a "A"><!ENTITY b "&a;&a;"><!ENTITY c "&b;&b;">]><r>&c;</r>"#,
        ),
        (
            "entity free dtd",
            r#"<!DOCTYPE r [ <!ELEMENT r EMPTY> <!ATTLIST r a CDATA #IMPLIED> ]><r/>"#,
        ),
        (
            "entity expanding to nothing",
            r#"<!DOCTYPE r [<!ENTITY e "">]><r>&e;</r>"#,
        ),
        (
            "empty entity between elements",
            r#"<!DOCTYPE r [<!ENTITY e "">]><r><a/>&e;<b/></r>"#,
        ),
        (
            "namespaced elements",
            r#"<r xmlns:a="urn:a"><a:item/><item/></r>"#,
        ),
        (
            "default namespace shadowing",
            r#"<r xmlns="urn:outer"><a xmlns=""><b xmlns="urn:inner"/></a></r>"#,
        ),
        (
            "prefixed namespace shadowing",
            r#"<root xmlns:ns="http://outer.com"><ns:child xmlns:ns="http://inner.com"><ns:grandchild/></ns:child></root>"#,
        ),
        (
            "namespace declaration map",
            r#"<root xmlns="http://default.com" xmlns:ns="http://ns.com"/>"#,
        ),
        (
            "saml namespace shape",
            r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion"><saml:Assertion><saml:Subject><saml:NameID>user@example.com</saml:NameID></saml:Subject></saml:Assertion></samlp:Response>"#,
        ),
        (
            "namespaced attributes",
            r#"<r xmlns:a="urn:x" xmlns:b="urn:y" a:id="first" b:id="second"/>"#,
        ),
        (
            "redundant xml namespace declaration",
            r#"<r xmlns:xml="http://www.w3.org/XML/1998/namespace"><xml:a xml:lang="en"/></r>"#,
        ),
        ("writer sanitized name output", r#"<_ _="value"></_>"#),
        ("unicode text", "<r>日本語テキスト</r>"),
        ("unicode attribute", r#"<r attr="日本語"/>"#),
        (
            "replacement character output",
            "<r a=\"x\u{FFFD}y\">t\u{FFFD}u</r>",
        ),
        (
            "writer sanitized attribute collisions",
            r#"<r _="one" __1="two" __2="three"></r>"#,
        ),
        ("roundtrip attr with lt", r#"<r a="a &lt; b"/>"#),
        (
            "xsd schema snippet",
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="x" type="xs:string"/></xs:schema>"#,
        ),
        (
            "xsd identity namespace schema",
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:r="urn:root" xmlns:v="urn:vehicle" targetNamespace="urn:root" elementFormDefault="qualified"><xs:element name="root"><xs:complexType><xs:sequence><xs:any namespace="##any" processContents="skip" minOccurs="0" maxOccurs="unbounded"/></xs:sequence></xs:complexType><xs:key name="vehicle_ids"><xs:selector xpath=".//v:vehicle"/><xs:field xpath="@v:id"/></xs:key></xs:element></xs:schema>"###,
        ),
        (
            "saml metadata fixture",
            include_str!("../test-data/pyff-xslt/sample-metadata.xml"),
        ),
        (
            "atom aggregate fixture",
            include_str!("../test-data/pyff-xslt/atom-feed-sample.xml"),
        ),
        (
            "bounded quadratic entity fixture",
            include_str!("../audit/pocs/quadratic_blowup.xml"),
        ),
    ] {
        assert_default_match(name, xml);
    }
}

#[test]
fn pull_dom_matches_parser_with_parser_options() {
    let namespace_disabled = r#"<r xmlns:a="urn:a"><a:item a:k="v"/></r>"#;
    assert_pull_dom_matches_parser(
        "namespace awareness disabled",
        namespace_disabled,
        Parser::with_namespace_aware(false),
        || PullParser::with_namespace_aware(namespace_disabled, false),
    );

    let dtd_free = "<r><a><b>text</b></a></r>";
    assert_pull_dom_matches_parser(
        "forbid_dtd on dtd-free document",
        dtd_free,
        Parser::new().with_forbid_dtd(true),
        || PullParser::new(dtd_free).with_forbid_dtd(true),
    );

    let entity_free_dtd =
        r#"<!DOCTYPE r [ <!ELEMENT r EMPTY> <!ATTLIST r a CDATA #IMPLIED> ]><r/>"#;
    assert_pull_dom_matches_parser(
        "forbid_entities allows entity-free dtd",
        entity_free_dtd,
        Parser::new().with_forbid_entities(true),
        || PullParser::new(entity_free_dtd).with_forbid_entities(true),
    );

    let bounded_depth = nested_document(8);
    assert_pull_dom_matches_parser(
        "custom max depth accepts bounded document",
        &bounded_depth,
        Parser::new().with_max_depth(16),
        || PullParser::new(&bounded_depth).with_max_depth(16),
    );

    let bounded_entities = r#"<!DOCTYPE doc [<!ENTITY s "XXXXXXXXXXXXXXXX">]><doc>&s;&s;</doc>"#;
    assert_pull_dom_matches_parser(
        "custom entity budget accepts bounded expansion",
        bounded_entities,
        Parser::new().with_max_entity_expansion(1 << 16),
        || PullParser::new(bounded_entities).with_max_entity_expansion(1 << 16),
    );
}

#[test]
fn pull_dom_fails_like_parser_for_invalid_regression_corpus() {
    for (name, xml) in [
        ("empty document", ""),
        ("two roots", "<a/><b/>"),
        ("mismatched end tag", "<a></b>"),
        ("unclosed element", "<r>"),
        ("duplicate attributes", r#"<r a="1" a="2"/>"#),
        (
            "duplicate expanded attributes",
            r#"<r xmlns:a="urn:x" xmlns:b="urn:x" a:id="first" b:id="second"/>"#,
        ),
        ("bare ampersand", "<r>a & b</r>"),
        ("lt in attribute", r#"<r a="<"/>"#),
        ("bad comment", "<r><!-- -- --></r>"),
        ("cdata close in text", "<r>]]></r>"),
        ("reserved xml pi target", "<r><?XML data?></r>"),
        ("empty decimal character reference", "<r>&#;</r>"),
        ("empty hex character reference", "<r>&#x;</r>"),
        ("invalid xml character", "<r>\u{0001}</r>"),
        (
            "duplicate doctype declaration",
            r#"<!DOCTYPE a SYSTEM "a.dtd"><!DOCTYPE b SYSTEM "b.dtd"><r/>"#,
        ),
        (
            "billion laughs fixture hits entity budget",
            include_str!("../audit/pocs/billion_laughs.xml"),
        ),
        (
            "reserved namespace as default",
            r#"<r xmlns="http://www.w3.org/XML/1998/namespace"/>"#,
        ),
        (
            "xmlns namespace as default",
            r#"<r xmlns="http://www.w3.org/2000/xmlns/"/>"#,
        ),
        (
            "xml namespace bound to other prefix",
            r#"<r xmlns:foo="http://www.w3.org/XML/1998/namespace"/>"#,
        ),
        (
            "xmlns namespace bound to prefix",
            r#"<r xmlns:foo="http://www.w3.org/2000/xmlns/"/>"#,
        ),
        ("empty xmlns declaration prefix", r#"<r xmlns:="urn:x"/>"#),
        (
            "multi colon xmlns declaration prefix",
            r#"<r xmlns:a:b="urn:x"/>"#,
        ),
        ("undeclared element prefix", r#"<p:r/>"#),
        ("undeclared attribute prefix", r#"<r p:a="1"/>"#),
        ("multi colon element name", "<xmlns:xmlns:C/>"),
        (
            "xmlns element prefix under default namespace",
            r#"<r xmlns="urn:x"><xmlns:c/></r>"#,
        ),
        ("undeclared namespace prefix", r#"<r xmlns:p=""/>"#),
        ("colon in pi target", "<r><?a:b x?></r>"),
        (
            "colon in entity name",
            r#"<!DOCTYPE r [<!ENTITY a:b "x">]><r/>"#,
        ),
    ] {
        assert_same_failure(name, xml, Parser::new(), || PullParser::new(xml));
    }
}

#[test]
fn pull_dom_fails_like_parser_for_option_regressions() {
    type MakeParser = fn() -> Parser;
    type MakePull = fn(&str) -> PullParser<'_>;
    let forbid_dtd_parser: MakeParser = || Parser::new().with_forbid_dtd(true);
    let forbid_dtd_pull: MakePull = |xml| PullParser::new(xml).with_forbid_dtd(true);
    let forbid_entities_parser: MakeParser = || Parser::new().with_forbid_entities(true);
    let forbid_entities_pull: MakePull = |xml| PullParser::new(xml).with_forbid_entities(true);

    for (name, xml, make_parser, make_pull) in [
        (
            "forbid_dtd rejects external doctype",
            r#"<!DOCTYPE r SYSTEM "r.dtd"><r/>"#,
            forbid_dtd_parser,
            forbid_dtd_pull,
        ),
        (
            "forbid_dtd rejects internal subset",
            r#"<!DOCTYPE r [ <!ELEMENT r EMPTY> ]><r/>"#,
            forbid_dtd_parser,
            forbid_dtd_pull,
        ),
        (
            "forbid_entities rejects general entity",
            r#"<!DOCTYPE r [ <!ENTITY x "y"> ]><r/>"#,
            forbid_entities_parser,
            forbid_entities_pull,
        ),
        (
            "forbid_entities rejects parameter entity",
            r#"<!DOCTYPE r [ <!ENTITY % p "<!ELEMENT r EMPTY>"> ]><r/>"#,
            forbid_entities_parser,
            forbid_entities_pull,
        ),
    ] {
        assert_same_failure(name, xml, make_parser(), || make_pull(xml));
    }

    let too_deep = nested_document(8);
    assert_same_failure(
        "max depth rejects deep document",
        &too_deep,
        Parser::new().with_max_depth(4),
        || PullParser::new(&too_deep).with_max_depth(4),
    );

    let over_budget = r#"<!DOCTYPE doc [<!ENTITY s "XXXXXXXXXXXXXXXX">]><doc>&s;&s;&s;</doc>"#;
    assert_same_failure(
        "max entity expansion rejects over-budget expansion",
        over_budget,
        Parser::new().with_max_entity_expansion(32),
        || PullParser::new(over_budget).with_max_entity_expansion(32),
    );

    let mut chain = String::from("<!DOCTYPE r [");
    for i in 0..300 {
        chain.push_str(&format!("<!ENTITY e{i} \"&e{};\">", i + 1));
    }
    chain.push_str("<!ENTITY e300 \"x\">]><r>&e0;</r>");
    assert_same_failure(
        "deep entity chain fails closed",
        &chain,
        Parser::new(),
        || PullParser::new(&chain),
    );

    let content_model = format!("{}a{}", "(".repeat(10), ")".repeat(10));
    let deep_dtd =
        format!("<!DOCTYPE r [<!ELEMENT r {content_model}><!ELEMENT a EMPTY>]><r><a/></r>");
    assert_same_failure(
        "dtd content model depth uses parser limit",
        &deep_dtd,
        Parser::new().with_max_depth(5),
        || PullParser::new(&deep_dtd).with_max_depth(5),
    );
}
