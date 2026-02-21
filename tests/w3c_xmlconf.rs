//! W3C XML Conformance Test Suite runner.
//!
//! This test runs the W3C XML Conformance Test Suite (20130923 edition)
//! against the uppsala XML parser. It focuses on standalone tests
//! (ENTITIES="none") since our parser does not resolve external entities.
//!
//! Test types:
//! - not-wf: document is not well-formed, parsing MUST fail
//! - valid: document is well-formed and valid, parsing MUST succeed
//! - invalid: document is well-formed but invalid (DTD), parsing should succeed
//! - error: optional errors, we skip these

use std::fs;
use std::path::{Path, PathBuf};

/// A test case from the W3C conformance suite.
#[derive(Debug)]
struct W3cTestCase {
    id: String,
    test_type: String,
    entities: String,
    uri: String,
    edition: Option<String>,
    base_dir: PathBuf,
}

/// Simple extraction of TEST elements from a catalog XML file.
/// We can't use our XML parser because catalogs may have DTD features.
fn parse_catalog(path: &Path, base_dir: &Path) -> Vec<W3cTestCase> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut tests = Vec::new();
    let mut pos = 0;
    let bytes = content.as_bytes();

    while pos < bytes.len() {
        // Find next <TEST
        if let Some(start) = content[pos..].find("<TEST ") {
            let abs_start = pos + start;
            // Find the closing > of this element (could be > or />)
            if let Some(end) = content[abs_start..].find('>') {
                let tag = &content[abs_start..abs_start + end + 1];

                let id = extract_attr(tag, "ID").unwrap_or_default();
                let test_type = extract_attr(tag, "TYPE").unwrap_or_default();
                let entities = extract_attr(tag, "ENTITIES").unwrap_or_default();
                let uri = extract_attr(tag, "URI").unwrap_or_default();
                let edition = extract_attr(tag, "EDITION");

                if !id.is_empty() && !uri.is_empty() {
                    tests.push(W3cTestCase {
                        id,
                        test_type,
                        entities,
                        uri,
                        edition,
                        base_dir: base_dir.to_path_buf(),
                    });
                }

                pos = abs_start + end + 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    tests
}

/// Extract an XML attribute value from a tag string.
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{}=\"", name);
    if let Some(start) = tag.find(&pattern) {
        let val_start = start + pattern.len();
        if let Some(end) = tag[val_start..].find('"') {
            return Some(tag[val_start..val_start + end].to_string());
        }
    }
    // Try single quotes
    let pattern = format!("{}='", name);
    if let Some(start) = tag.find(&pattern) {
        let val_start = start + pattern.len();
        if let Some(end) = tag[val_start..].find('\'') {
            return Some(tag[val_start..val_start + end].to_string());
        }
    }
    None
}

/// Load all test cases from the W3C test suite.
fn load_all_tests() -> Vec<W3cTestCase> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-data")
        .join("xmlconf");

    if !base.exists() {
        return Vec::new();
    }

    let mut all_tests = Vec::new();

    // James Clark XMLTEST
    let xmltest_catalog = base.join("xmltest").join("xmltest.xml");
    all_tests.extend(parse_catalog(&xmltest_catalog, &base.join("xmltest")));

    // Sun tests
    for name in &[
        "sun-valid.xml",
        "sun-invalid.xml",
        "sun-not-wf.xml",
        "sun-error.xml",
    ] {
        let path = base.join("sun").join(name);
        all_tests.extend(parse_catalog(&path, &base.join("sun")));
    }

    // OASIS/NIST
    let oasis_catalog = base.join("oasis").join("oasis.xml");
    all_tests.extend(parse_catalog(&oasis_catalog, &base.join("oasis")));

    // IBM XML 1.0
    for name in &[
        "ibm_oasis_invalid.xml",
        "ibm_oasis_not-wf.xml",
        "ibm_oasis_valid.xml",
    ] {
        let path = base.join("ibm").join(name);
        all_tests.extend(parse_catalog(&path, &base.join("ibm")));
    }

    // Edinburgh University tests (XML 1.0 related only)
    let eduni_catalogs = [
        ("eduni/errata-2e/errata2e.xml", "eduni/errata-2e"),
        ("eduni/errata-3e/errata3e.xml", "eduni/errata-3e"),
        ("eduni/errata-4e/errata4e.xml", "eduni/errata-4e"),
        ("eduni/namespaces/1.0/rmt-ns10.xml", "eduni/namespaces/1.0"),
        (
            "eduni/namespaces/errata-1e/errata1e.xml",
            "eduni/namespaces/errata-1e",
        ),
        ("eduni/misc/ht-bh.xml", "eduni/misc"),
    ];
    for (catalog, base_sub) in &eduni_catalogs {
        let path = base.join(catalog);
        all_tests.extend(parse_catalog(&path, &base.join(base_sub)));
    }

    // Japanese tests
    let jp_catalog = base.join("japanese").join("japanese.xml");
    all_tests.extend(parse_catalog(&jp_catalog, &base.join("japanese")));

    all_tests
}

/// Check if a test applies to XML 1.0 5th edition.
fn is_xml10_5e(test: &W3cTestCase) -> bool {
    match &test.edition {
        None => true, // No edition restriction = applies to all
        Some(ed) => {
            // EDITION can be "1 2 3 4" or "5" or "1 2 3 4 5" etc.
            ed.split_whitespace().any(|e| e == "5")
        }
    }
}

#[test]
fn w3c_xmltest_not_well_formed_standalone() {
    let tests = load_all_tests();
    if tests.is_empty() {
        eprintln!("W3C test suite not found, skipping");
        return;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for test in &tests {
        if test.test_type != "not-wf" {
            continue;
        }
        // Only standalone tests (no external entity resolution needed)
        if test.entities != "none" {
            skipped += 1;
            continue;
        }
        // Only XML 1.0 5th edition tests
        if !is_xml10_5e(test) {
            skipped += 1;
            continue;
        }

        let file_path = test.base_dir.join(&test.uri);
        let bytes = match fs::read(&file_path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let result = uppsala::parse_bytes(&bytes);
        if result.is_err() {
            passed += 1;
        } else {
            failed += 1;
            failures.push(format!(
                "  {} ({}): expected parse error, got success",
                test.id, test.uri
            ));
        }
    }

    eprintln!(
        "W3C not-wf standalone: {} passed, {} failed, {} skipped",
        passed, failed, skipped
    );
    if !failures.is_empty() {
        eprintln!("Failures:");
        for f in &failures {
            eprintln!("{}", f);
        }
    }

    // We expect a high pass rate but not necessarily 100% since some tests
    // may test features we haven't implemented (e.g. DTD validation).
    let total = passed + failed;
    if total > 0 {
        let pass_rate = (passed as f64 / total as f64) * 100.0;
        eprintln!("Pass rate: {:.1}% ({}/{})", pass_rate, passed, total);
        // We should pass at least 70% of standalone not-wf tests
        assert!(
            pass_rate >= 70.0,
            "Pass rate too low: {:.1}% ({} failures out of {})",
            pass_rate,
            failed,
            total
        );
    }
}

#[test]
fn w3c_xmltest_valid_standalone() {
    let tests = load_all_tests();
    if tests.is_empty() {
        eprintln!("W3C test suite not found, skipping");
        return;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for test in &tests {
        if test.test_type != "valid" {
            continue;
        }
        // Only standalone tests
        if test.entities != "none" {
            skipped += 1;
            continue;
        }
        // Only XML 1.0 5th edition
        if !is_xml10_5e(test) {
            skipped += 1;
            continue;
        }

        let file_path = test.base_dir.join(&test.uri);
        let bytes = match fs::read(&file_path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let result = uppsala::parse_bytes(&bytes);
        if result.is_ok() {
            passed += 1;
        } else {
            failed += 1;
            failures.push(format!(
                "  {} ({}): expected success, got error: {}",
                test.id,
                test.uri,
                result.unwrap_err()
            ));
        }
    }

    eprintln!(
        "W3C valid standalone: {} passed, {} failed, {} skipped",
        passed, failed, skipped
    );
    if !failures.is_empty() {
        eprintln!("Failures:");
        for f in &failures {
            eprintln!("{}", f);
        }
    }

    let total = passed + failed;
    if total > 0 {
        let pass_rate = (passed as f64 / total as f64) * 100.0;
        eprintln!("Pass rate: {:.1}% ({}/{})", pass_rate, passed, total);
        // We should pass at least 70% of standalone valid tests
        assert!(
            pass_rate >= 70.0,
            "Pass rate too low: {:.1}% ({} failures out of {})",
            pass_rate,
            failed,
            total
        );
    }
}

#[test]
fn w3c_xmltest_invalid_standalone() {
    // "invalid" tests are well-formed but DTD-invalid.
    // Our parser should successfully parse these (we're testing well-formedness, not DTD validation).
    let tests = load_all_tests();
    if tests.is_empty() {
        eprintln!("W3C test suite not found, skipping");
        return;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for test in &tests {
        if test.test_type != "invalid" {
            continue;
        }
        if test.entities != "none" {
            skipped += 1;
            continue;
        }
        if !is_xml10_5e(test) {
            skipped += 1;
            continue;
        }

        let file_path = test.base_dir.join(&test.uri);
        let bytes = match fs::read(&file_path) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let result = uppsala::parse_bytes(&bytes);
        if result.is_ok() {
            passed += 1;
        } else {
            failed += 1;
            failures.push(format!(
                "  {} ({}): expected success (well-formed), got error: {}",
                test.id,
                test.uri,
                result.unwrap_err()
            ));
        }
    }

    eprintln!(
        "W3C invalid standalone (should parse OK): {} passed, {} failed, {} skipped",
        passed, failed, skipped
    );
    if !failures.is_empty() {
        eprintln!("Failures:");
        for f in &failures {
            eprintln!("{}", f);
        }
    }

    let total = passed + failed;
    if total > 0 {
        let pass_rate = (passed as f64 / total as f64) * 100.0;
        eprintln!("Pass rate: {:.1}% ({}/{})", pass_rate, passed, total);
        // Invalid tests are well-formed, so we should parse them successfully
        assert!(
            pass_rate >= 60.0,
            "Pass rate too low: {:.1}% ({} failures out of {})",
            pass_rate,
            failed,
            total
        );
    }
}
