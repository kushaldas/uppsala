//! Regression tests for the core security-hardening review findings.
//!
//! Each test pins a specific vulnerability fixed in the differential review of
//! the `feat/harden` branch. (Findings specific to the XPath 2.0 engine are not
//! included on this security-only branch.) The findings fall into three classes:
//!
//! * **Uncatchable aborts** (stack overflow): F7 (deep linear entity chains).
//!   Before the fix this aborted the whole process via `SIGABRT`; after the fix
//!   it returns a normal `Err`. NOTE: if the fix regresses, the test will abort
//!   the test binary rather than fail cleanly — that is intentional and loud.
//! * **Panics** (catchable): F8 (datetime multibyte slicing), F11 (`substring`
//!   overflow). Verified with `catch_unwind`.
//! * **Resource exhaustion / correctness** (no crash, but pathological cost or
//!   wrong output): F5 (uncharged O(n·m) work), F4 (quadratic regex
//!   allocation), F6 (PI-target markup injection), F10 (duplicate namespace
//!   declarations), F12 (unknown Unicode property bypass).
//!
//! Every group also includes a "legitimate input still works" assertion so a
//! future tightening of a limit cannot silently break valid documents.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use uppsala::{parse, NodeKind, XPathEvaluator, XmlWriter, XsdRegex, XsdValidator};

// ─── F4 — XSD regex repetition must not be quadratic-time (budget bypass) ─────

fn validate_pattern(pattern: &str, body: &str) -> bool {
    let schema = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
          <xs:element name="x"><xs:simpleType><xs:restriction base="xs:string">
          <xs:pattern value="{pattern}"/></xs:restriction></xs:simpleType></xs:element></xs:schema>"#
    );
    let sd = parse(&schema).unwrap();
    let validator = XsdValidator::from_schema(&sd).unwrap();
    let instance_xml = format!("<x>{body}</x>");
    let inst = parse(&instance_xml).unwrap();
    validator.validate(&inst).is_empty()
}

#[test]
fn f4_repetition_pattern_is_near_linear_and_correct() {
    // `a*b*` over a long run of `a` previously allocated an O(N) bitmap per
    // candidate position — O(N^2) work uncharged by the step budget. The lazy
    // allocation makes it ~linear AND still correct (the value validates).
    let n = 400_000;
    let body = "a".repeat(n);
    let start = Instant::now();
    assert!(
        validate_pattern("a*b*", &body),
        "a* over a long string of 'a's must still match"
    );
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "match must stay near-linear, took {:?}",
        start.elapsed()
    );
}

#[test]
fn f4_repetition_pattern_rejects_non_matching_input() {
    // Correctness guard: the lazy-allocation rewrite must not make a
    // non-matching value pass.
    assert!(
        !validate_pattern("a*b*", "abab"),
        "`a*b*` must reject interleaved input"
    );
}

// ─── F5 — XPath 1.0 node-set comparison must be charged ──────────────────────

#[test]
fn f5_disjoint_nodeset_comparison_is_bounded() {
    // `/r/a = /r/b` over disjoint-valued sets built via a cheap child-axis path
    // forced a full O(n·m) string-value scan that the node-visit budget did not
    // charge — minutes of CPU. It now fails fast.
    let mut s = String::from("<r>");
    for _ in 0..16_000 {
        s.push_str("<a>x</a>");
    }
    for _ in 0..16_000 {
        s.push_str("<b>y</b>");
    }
    s.push_str("</r>");
    let mut doc = parse(&s).unwrap();
    doc.prepare_xpath();
    let root = doc.root();
    let evaluator = XPathEvaluator::new();
    let start = Instant::now();
    let result = evaluator.evaluate(&doc, root, "/r/a = /r/b");
    assert!(
        result.is_err(),
        "large disjoint comparison must hit the budget"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "must fail fast, took {:?}",
        start.elapsed()
    );
}

#[test]
fn f5_small_nodeset_comparison_is_correct() {
    // The charge must not break ordinary comparisons: 2 == 2 is true.
    let mut doc = parse("<r><a>1</a><a>2</a><b>2</b></r>").unwrap();
    doc.prepare_xpath();
    let root = doc.root();
    let evaluator = XPathEvaluator::new();
    let value = evaluator.evaluate(&doc, root, "/r/a = /r/b").unwrap();
    assert!(
        value.to_boolean(),
        "node-sets sharing the value 2 compare equal"
    );
}

// ─── F6 — Processing-instruction target must be sanitized ────────────────────

/// Count element children of the document element in `xml`.
fn count_root_child_elements(xml: &str) -> usize {
    let doc = parse(xml).expect("serialized output must re-parse");
    let root = doc.document_element().expect("document element");
    doc.children_iter(root)
        .filter(|&c| matches!(doc.node_kind(c), Some(NodeKind::Element(_))))
        .count()
}

#[test]
fn f6_pi_target_cannot_smuggle_markup() {
    // A PI target containing `?>` plus markup previously broke out of PI
    // position and injected a sibling element. The target is now validated as
    // an NCName (collapsing to `_` when invalid).
    let mut w = XmlWriter::new();
    w.start_element("r", &[]);
    w.processing_instruction("foo?><evil>boom</evil><?x", None);
    w.end_element("r");
    let xml = w.into_string();
    assert!(
        !xml.contains("<evil>"),
        "markup must not be smuggled: {xml}"
    );
    assert_eq!(
        count_root_child_elements(&xml),
        0,
        "no sibling element may be injected via the PI target: {xml}"
    );
}

#[test]
fn f6_reserved_and_valid_pi_targets_are_preserved() {
    // The reserved `xml` target is renamed; a normal target round-trips.
    let mut w = XmlWriter::new();
    w.start_element("r", &[]);
    w.processing_instruction("xml", Some("v"));
    w.processing_instruction("xml-stylesheet", Some("type=\"text/xsl\""));
    w.end_element("r");
    let xml = w.into_string();
    assert!(xml.contains("<?_xml"), "reserved target renamed: {xml}");
    assert!(
        xml.contains("<?xml-stylesheet"),
        "valid target preserved: {xml}"
    );
    assert!(parse(&xml).is_ok());
}

// ─── F7 — Deep linear entity chains must not overflow the stack ──────────────

#[test]
fn f7_deep_entity_chain_fails_closed() {
    // e0 -> e1 -> ... -> eN with a tiny leaf expands to ~1 byte (so the byte
    // budget never trips) yet recursed N frames deep. The depth cap now rejects
    // it with a normal error instead of aborting the process.
    let n = 2_000;
    let mut dtd = String::from("<!DOCTYPE r [\n");
    for i in 0..n {
        dtd.push_str(&format!("<!ENTITY e{i} \"&e{};\">", i + 1));
    }
    dtd.push_str(&format!("<!ENTITY e{n} \"x\">\n]>\n<r>&e0;</r>"));
    assert!(parse(&dtd).is_err(), "deep entity chain must fail closed");
}

#[test]
fn f7_deep_markup_entity_chain_fails_closed() {
    // Markup in replacement text is tokenized on a new stack frame per nested
    // reference, so each level counts against the element depth limit. A chain
    // within the expansion depth cap but past the element depth limit must be
    // refused with a normal error.
    let n = 200;
    let mut dtd = String::from("<!DOCTYPE r [<!ENTITY e0 \"<z/>\">");
    for i in 1..n {
        dtd.push_str(&format!("<!ENTITY e{i} \"<y/>&e{};\">", i - 1));
    }
    dtd.push_str(&format!("]><r>&e{};</r>", n - 1));
    let err = parse(&dtd).expect_err("deep markup entity chain must fail closed");
    assert!(err.to_string().contains("maximum depth"), "{err}");

    let shallow = "<!DOCTYPE r [<!ENTITY a \"<a/>\"><!ENTITY b \"<b>&a;</b>\">]><r>&b;</r>";
    assert_eq!(parse(shallow).unwrap().to_xml(), "<r><b><a/></b></r>");
}

#[test]
fn f7_shallow_entity_nesting_still_expands() {
    // A handful of nested entities must still resolve normally.
    let doc = parse(
        "<!DOCTYPE r [<!ENTITY a \"A\"><!ENTITY b \"&a;&a;\"><!ENTITY c \"&b;&b;\">]><r>&c;</r>",
    )
    .expect("shallow entity nesting must parse");
    let root = doc.document_element().unwrap();
    assert_eq!(doc.element_text(root), Some("AAAA"));
}

// ─── F8 — datetime validators must not panic on multibyte input ──────────────

/// Validate a single-element instance whose type is `type_name` and text is
/// `text`, returning whether validation succeeded (no errors). Panics are
/// surfaced so the test can assert they do not occur.
fn validate_typed_text(type_name: &str, text: &str) -> Result<bool, ()> {
    let schema = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
          <xs:element name="x" type="xs:{type_name}"/></xs:schema>"#
    );
    let sd = parse(&schema).unwrap();
    let validator = XsdValidator::from_schema(&sd).unwrap();
    let instance_xml = format!("<x>{text}</x>");
    let inst = parse(&instance_xml).unwrap();
    catch_unwind(AssertUnwindSafe(|| validator.validate(&inst).is_empty())).map_err(|_| ())
}

#[test]
fn f8_gmonth_gday_gmonthday_reject_multibyte_without_panic() {
    // Fixed byte-index slices (`&s[2..4]` etc.) panicked when a multibyte char
    // straddled the slice boundary. The validators now reject non-ASCII up
    // front. Each must return "invalid" (Ok(false)), never panic (Err).
    for (ty, text) in [
        ("gMonth", "--\u{20AC}"), // <x>--€</x>
        ("gDay", "---\u{20AC}"),  // <x>---€</x>
        ("gMonthDay", "--\u{20AC}-01"),
    ] {
        match validate_typed_text(ty, text) {
            Ok(valid) => assert!(!valid, "{ty} must reject multibyte input"),
            Err(()) => panic!("{ty} validation panicked on multibyte input"),
        }
    }
}

#[test]
fn f8_valid_gmonth_still_accepted() {
    assert_eq!(validate_typed_text("gMonth", "--05"), Ok(true));
}

// ─── F10 — namespace-declaration collisions must not duplicate attributes ────

#[test]
fn f10_colliding_ns_prefixes_produce_well_formed_output() {
    use std::borrow::Cow;
    use uppsala::dom::QName;
    use uppsala::Document;

    // Two distinct invalid prefixes both sanitize to `_`. The serializer must
    // disambiguate them so the output has no duplicate `xmlns:_` attribute and
    // re-parses cleanly.
    let mut doc = Document::new();
    let root = doc.root();
    let el = doc.create_element(QName::local("r"));
    doc.append_child(root, el);
    if let Some(NodeKind::Element(e)) = doc.node_kind_mut(el) {
        e.namespace_declarations
            .push((Cow::Owned("a b".into()), Cow::Owned("urn:1".into())));
        e.namespace_declarations
            .push((Cow::Owned("c d".into()), Cow::Owned("urn:2".into())));
    }
    let xml = doc.to_xml();
    assert!(
        parse(&xml).is_ok(),
        "ns-declaration output must re-parse as well-formed: {xml}"
    );
}

// ─── F11 — XPath 1.0 substring() must not overflow ───────────────────────────

#[test]
fn f11_substring_overflow_is_clamped_not_panicked() {
    // `substring(s, start, +inf)` coerced length to usize::MAX; `start + len`
    // overflowed. `saturating_add` + clamp now returns the remaining string.
    let mut doc = parse("<r/>").unwrap();
    doc.prepare_xpath();
    let root = doc.root();
    let evaluator = XPathEvaluator::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        evaluator.evaluate(&doc, root, "substring('hello', 2, 1 div 0)")
    }));
    let value = result
        .expect("substring must not panic on overflow")
        .expect("evaluation succeeds");
    assert_eq!(value.to_string_value(&doc), "ello");
}

// ─── F12 — XSD regex must reject unknown Unicode properties/blocks ───────────

#[test]
fn f12_unknown_unicode_property_rejected() {
    // Unknown category/block names must fail to compile (fail-closed), so
    // `\P{IsTypo}` cannot match every character.
    assert!(XsdRegex::compile("\\P{unknowncategory}+").is_err());
    assert!(XsdRegex::compile("\\p{unknowncategory}+").is_err());
    assert!(XsdRegex::compile("\\p{IsNotARealBlock}+").is_err());
    assert!(XsdRegex::compile("[\\p{unknowncategory}]").is_err());
}

#[test]
fn f12_known_categories_and_blocks_still_compile() {
    // Valid general categories and the XSD 1.0 block names must still compile.
    for pattern in [
        "\\p{Lu}+",
        "\\P{Nd}+",
        "\\p{IsBasicLatin}+",
        "\\p{IsGreek}+",
        "\\p{IsCJKUnifiedIdeographs}+",
    ] {
        assert!(
            XsdRegex::compile(pattern).is_ok(),
            "valid pattern must compile: {pattern}"
        );
    }
}
