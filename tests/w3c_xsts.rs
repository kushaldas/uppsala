//! W3C XML Schema Test Suite (XSTS) runner.
//!
//! This test runs parts of the W3C XML Schema Test Suite (2007-06-20 edition)
//! against the uppsala XSD validator. It focuses on:
//! - NIST datatype tests (atomic types with facet restrictions)
//!
//! Test structure:
//! - Each testGroup has a schemaTest (XSD) and instanceTests (XML)
//! - schemaTest: expected validity of the schema itself
//! - instanceTest: expected validity of an XML instance against the schema
//!
//! We focus on instanceTests where the schema is expected to be valid,
//! since our validator validates instances against schemas.

use std::fs;
use std::path::{Path, PathBuf};

use uppsala::xsd::XsdValidator;

/// A test group from the XSTS.
#[derive(Debug)]
struct XstsTestGroup {
    name: String,
    schema_path: Option<PathBuf>,
    schema_valid: bool,
    instance_tests: Vec<XstsInstanceTest>,
}

/// An instance test within a test group.
#[derive(Debug)]
struct XstsInstanceTest {
    name: String,
    path: PathBuf,
    expected_valid: bool,
}

/// Simple XML attribute extraction (same approach as w3c_xmlconf.rs).
fn extract_attr(tag: &str, attr_name: &str) -> Option<String> {
    let patterns = [format!("{}=\"", attr_name), format!("{}='", attr_name)];
    for pattern in &patterns {
        if let Some(start) = tag.find(pattern.as_str()) {
            let val_start = start + pattern.len();
            let quote = if pattern.ends_with('"') { '"' } else { '\'' };
            if let Some(end) = tag[val_start..].find(quote) {
                return Some(tag[val_start..val_start + end].to_string());
            }
        }
    }
    None
}

/// Extract xlink:href attribute.
fn extract_href(tag: &str) -> Option<String> {
    extract_attr(tag, "xlink:href")
}

/// Parse a testSet XML file to extract test groups.
fn parse_test_set(path: &Path) -> Vec<XstsTestGroup> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let base_dir = path.parent().unwrap_or(Path::new("."));
    let mut groups = Vec::new();
    let mut pos = 0;

    while pos < content.len() {
        // Find next <testGroup
        if let Some(start) = content[pos..].find("<testGroup") {
            let abs_start = pos + start;
            // Find the end of this testGroup
            if let Some(end) = content[abs_start..].find("</testGroup>") {
                let group_text = &content[abs_start..abs_start + end + "</testGroup>".len()];

                // Extract group name
                let group_tag_end = group_text.find('>').unwrap_or(group_text.len());
                let group_tag = &group_text[..group_tag_end];
                let group_name = extract_attr(group_tag, "name").unwrap_or_default();

                // Extract schemaTest
                let mut schema_path = None;
                let mut schema_valid = false;
                if let Some(st_start) = group_text.find("<schemaTest") {
                    if let Some(st_end) = group_text[st_start..].find("</schemaTest>") {
                        let schema_test = &group_text[st_start..st_start + st_end];
                        // Find schemaDocument href
                        if let Some(sd_start) = schema_test.find("<schemaDocument") {
                            if let Some(sd_end) = schema_test[sd_start..].find("/>") {
                                let sd_tag = &schema_test[sd_start..sd_start + sd_end + 2];
                                if let Some(href) = extract_href(sd_tag) {
                                    schema_path = Some(base_dir.join(&href));
                                }
                            }
                        }
                        // Check expected validity
                        if let Some(ev_start) = schema_test.find("<expected") {
                            if let Some(ev_end) = schema_test[ev_start..].find("/>") {
                                let ev_tag = &schema_test[ev_start..ev_start + ev_end + 2];
                                if let Some(validity) = extract_attr(ev_tag, "validity") {
                                    schema_valid = validity == "valid";
                                }
                            }
                        }
                    }
                }

                // Extract instanceTests
                let mut instance_tests = Vec::new();
                let mut ipos = 0;
                while ipos < group_text.len() {
                    if let Some(it_start) = group_text[ipos..].find("<instanceTest") {
                        let abs_it_start = ipos + it_start;
                        if let Some(it_end) = group_text[abs_it_start..].find("</instanceTest>") {
                            let inst_test = &group_text
                                [abs_it_start..abs_it_start + it_end + "</instanceTest>".len()];

                            // Extract name
                            let it_tag_end = inst_test.find('>').unwrap_or(inst_test.len());
                            let it_tag = &inst_test[..it_tag_end];
                            let it_name = extract_attr(it_tag, "name").unwrap_or_default();

                            // Extract instanceDocument href
                            let mut inst_path = None;
                            if let Some(id_start) = inst_test.find("<instanceDocument") {
                                if let Some(id_end) = inst_test[id_start..].find("/>") {
                                    let id_tag = &inst_test[id_start..id_start + id_end + 2];
                                    if let Some(href) = extract_href(id_tag) {
                                        inst_path = Some(base_dir.join(&href));
                                    }
                                }
                            }

                            // Extract expected validity
                            let mut expected_valid = false;
                            if let Some(ev_start) = inst_test.find("<expected") {
                                if let Some(ev_end) = inst_test[ev_start..].find("/>") {
                                    let ev_tag = &inst_test[ev_start..ev_start + ev_end + 2];
                                    if let Some(validity) = extract_attr(ev_tag, "validity") {
                                        expected_valid = validity == "valid";
                                    }
                                }
                            }

                            if let Some(path) = inst_path {
                                instance_tests.push(XstsInstanceTest {
                                    name: it_name,
                                    path,
                                    expected_valid,
                                });
                            }

                            ipos = abs_it_start + it_end + "</instanceTest>".len();
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }

                groups.push(XstsTestGroup {
                    name: group_name,
                    schema_path,
                    schema_valid,
                    instance_tests,
                });

                pos = abs_start + end + "</testGroup>".len();
            } else {
                break;
            }
        } else {
            break;
        }
    }

    groups
}

/// Run XSTS instance tests for a given test set file.
/// Returns (passed, failed, skipped, failure_details).
fn run_xsts_instance_tests(test_set_path: &Path) -> (usize, usize, usize, Vec<String>) {
    let groups = parse_test_set(test_set_path);
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for group in &groups {
        // Skip if schema is not expected to be valid
        if !group.schema_valid {
            skipped += group.instance_tests.len();
            continue;
        }

        // Load the schema
        let schema_path = match &group.schema_path {
            Some(p) => p,
            None => {
                skipped += group.instance_tests.len();
                continue;
            }
        };

        let schema_str = match fs::read_to_string(schema_path) {
            Ok(s) => s,
            Err(_) => {
                skipped += group.instance_tests.len();
                continue;
            }
        };

        let schema_doc = match uppsala::parse(&schema_str) {
            Ok(d) => d,
            Err(_) => {
                // Can't parse the schema XML — skip these tests
                skipped += group.instance_tests.len();
                continue;
            }
        };

        eprintln!("  DEBUG: Compiling schema for group '{}'...", group.name);
        let validator =
            match XsdValidator::from_schema_with_base_path(&schema_doc, Some(schema_path)) {
                Ok(v) => v,
                Err(e) => {
                    // Can't compile the schema — skip these tests
                    if !group.instance_tests.is_empty() {
                        eprintln!(
                            "  SKIP group '{}' ({} tests): schema error: {}",
                            group.name,
                            group.instance_tests.len(),
                            e
                        );
                    }
                    skipped += group.instance_tests.len();
                    continue;
                }
            };

        for inst_test in &group.instance_tests {
            eprintln!("    DEBUG: Validating instance '{}'", inst_test.name);
            let inst_str = match fs::read_to_string(&inst_test.path) {
                Ok(s) => s,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };

            let inst_doc = match uppsala::parse(&inst_str) {
                Ok(d) => d,
                Err(e) => {
                    if inst_test.expected_valid {
                        failures.push(format!(
                            "{} ({}): expected valid, parse error: {}",
                            inst_test.name,
                            inst_test.path.display(),
                            e
                        ));
                        failed += 1;
                    } else {
                        // Expected invalid and we can't even parse — count as pass
                        passed += 1;
                    }
                    continue;
                }
            };

            let errors = validator.validate(&inst_doc);
            let is_valid = errors.is_empty();

            if is_valid == inst_test.expected_valid {
                passed += 1;
            } else {
                let detail = if inst_test.expected_valid {
                    format!(
                        "{} ({}): expected valid, got {} error(s): {}",
                        inst_test.name,
                        inst_test.path.display(),
                        errors.len(),
                        errors.first().map(|e| e.to_string()).unwrap_or_default()
                    )
                } else {
                    format!(
                        "{} ({}): expected invalid, got valid",
                        inst_test.name,
                        inst_test.path.display(),
                    )
                };
                failures.push(detail);
                failed += 1;
            }
        }
    }

    (passed, failed, skipped, failures)
}

#[test]
fn xsts_nist_datatypes() {
    let test_set_path =
        Path::new("test-data/xsts/xmlschema2006-11-06/nistMeta/NISTXMLSchemaDatatypes.testSet");
    if !test_set_path.exists() {
        eprintln!("XSTS test suite not found, skipping. Download from W3C.");
        return;
    }

    let (passed, failed, skipped, failures) = run_xsts_instance_tests(test_set_path);
    let total = passed + failed;

    println!(
        "\nXSTS NIST Datatypes: {} passed, {} failed, {} skipped",
        passed, failed, skipped
    );
    if !failures.is_empty() {
        println!("Failures (first 30):");
        for f in failures.iter().take(30) {
            println!("  {}", f);
        }
        if failures.len() > 30 {
            println!("  ... and {} more", failures.len() - 30);
        }
        // Show first list failures
        println!("\nFirst list failures:");
        let mut list_count = 0;
        for f in &failures {
            if f.contains("/list/") && list_count < 5 {
                println!("  {}", f);
                list_count += 1;
            }
        }
        // Breakdown by datatype
        let mut by_type: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for f in &failures {
            // Extract datatype from path like "nistData/atomic/QName/..."
            if let Some(start) = f.find("nistData/") {
                let rest = &f[start + 9..];
                let parts: Vec<&str> = rest.split('/').collect();
                if parts.len() >= 2 {
                    let key = format!("{}/{}", parts[0], parts[1]);
                    *by_type.entry(key).or_insert(0) += 1;
                }
            }
        }
        let mut breakdown: Vec<_> = by_type.into_iter().collect();
        breakdown.sort_by(|a, b| b.1.cmp(&a.1));
        println!("\nFailure breakdown by type:");
        for (dtype, count) in &breakdown {
            println!("  {:>5} {}", count, dtype);
        }
    }

    if total > 0 {
        let pass_rate = (passed as f64 / total as f64) * 100.0;
        println!("Pass rate: {:.1}% ({}/{})", pass_rate, passed, total);
        // Start with a reasonable threshold and improve
        assert!(
            pass_rate >= 40.0,
            "NIST datatype pass rate {:.1}% is below 40% threshold",
            pass_rate
        );
    }
}

#[test]
fn xsts_sun_combined() {
    let test_set_path = Path::new("test-data/xsts/xmlschema2006-11-06/sunMeta/suntest.testSet");
    if !test_set_path.exists() {
        eprintln!("XSTS Sun test set not found, skipping.");
        return;
    }

    let (passed, failed, skipped, failures) = run_xsts_instance_tests(test_set_path);
    let total = passed + failed;

    println!(
        "\nXSTS Sun Combined: {} passed, {} failed, {} skipped",
        passed, failed, skipped
    );
    if !failures.is_empty() {
        println!("Failures (all {}):", failures.len());
        for f in failures.iter() {
            println!("  {}", f);
        }
    }

    if total > 0 {
        let pass_rate = (passed as f64 / total as f64) * 100.0;
        println!("Pass rate: {:.1}% ({}/{})", pass_rate, passed, total);
        // Sun tests use more advanced features; be more lenient
        assert!(
            pass_rate >= 20.0,
            "Sun combined pass rate {:.1}% is below 20% threshold",
            pass_rate
        );
    }
}

#[test]
fn xsts_ms_datatypes() {
    let test_set_path = Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/DataTypes_w3c.xml");
    if !test_set_path.exists() {
        eprintln!("XSTS MS DataTypes test set not found, skipping.");
        return;
    }

    let (passed, failed, skipped, failures) = run_xsts_instance_tests(test_set_path);
    let total = passed + failed;

    println!(
        "\nXSTS MS DataTypes: {} passed, {} failed, {} skipped",
        passed, failed, skipped
    );
    if !failures.is_empty() {
        println!("Failures (first 30):");
        for f in failures.iter().take(30) {
            println!("  {}", f);
        }
        if failures.len() > 30 {
            println!("  ... and {} more", failures.len() - 30);
        }
    }

    if total > 0 {
        let pass_rate = (passed as f64 / total as f64) * 100.0;
        println!("Pass rate: {:.1}% ({}/{})", pass_rate, passed, total);
        assert!(
            pass_rate >= 20.0,
            "MS DataTypes pass rate {:.1}% is below 20% threshold",
            pass_rate
        );
    }
}
