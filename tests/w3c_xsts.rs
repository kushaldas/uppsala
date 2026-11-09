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
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use uppsala::xsd::XsdValidator;

/// A test group from the XSTS.
#[derive(Debug)]
struct XstsTestGroup {
    name: String,
    schema_path: Option<PathBuf>,
    schema_valid: bool,
    schema_status: String,
    instance_tests: Vec<XstsInstanceTest>,
}

/// An instance test within a test group.
#[derive(Debug)]
struct XstsInstanceTest {
    name: String,
    path: PathBuf,
    expected_valid: bool,
    status: String,
}

/// Outcome of a single schema or instance test.
#[derive(Debug)]
enum XstsOutcome {
    Pass,
    /// Expected invalid, but the validator accepted it.
    FalseAccept(String),
    /// Expected valid, but the validator rejected it.
    FalseRefuse(String),
    /// The parser or the validator panicked.
    Panic(String),
    Skip(String),
}

/// Result of one schema or instance test.
#[derive(Debug)]
struct XstsCaseResult {
    kind: &'static str,
    group: String,
    name: String,
    path: PathBuf,
    status: String,
    expected_valid: bool,
    outcome: XstsOutcome,
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

/// Extract the `status` of a test's `<current>` element ("accepted", "queried", ...).
fn extract_status(test_text: &str) -> String {
    if let Some(c_start) = test_text.find("<current") {
        if let Some(c_end) = test_text[c_start..].find('>') {
            let c_tag = &test_text[c_start..c_start + c_end];
            if let Some(status) = extract_attr(c_tag, "status") {
                return status;
            }
        }
    }
    String::new()
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
                let mut schema_status = String::new();
                if let Some(st_start) = group_text.find("<schemaTest") {
                    if let Some(st_end) = group_text[st_start..].find("</schemaTest>") {
                        let schema_test = &group_text[st_start..st_start + st_end];
                        schema_status = extract_status(schema_test);
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
                                    status: extract_status(inst_test),
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
                    schema_status,
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

/// Run the XSTS cases of a test set file and return one result per case.
///
/// Instance tests are run against the group's schema when the schema is
/// expected to be valid and compiles; otherwise they are skipped. When
/// `check_schemas` is true, each group's schemaTest is also checked: the
/// schema must compile exactly when it is expected to be valid.
/// When `enforce_qname_length_facets` is false, QName/NOTATION length facets are skipped
/// (needed for NIST tests which expect them to be ignored per W3C Bug #4009).
fn run_xsts_cases(
    test_set_path: &Path,
    enforce_qname_length_facets: bool,
    check_schemas: bool,
) -> Vec<XstsCaseResult> {
    let groups = parse_test_set(test_set_path);
    let mut results = Vec::new();

    for group in &groups {
        let skip_instances = |results: &mut Vec<XstsCaseResult>, reason: &str| {
            for inst_test in &group.instance_tests {
                results.push(XstsCaseResult {
                    kind: "instance",
                    group: group.name.clone(),
                    name: inst_test.name.clone(),
                    path: inst_test.path.clone(),
                    status: inst_test.status.clone(),
                    expected_valid: inst_test.expected_valid,
                    outcome: XstsOutcome::Skip(reason.to_string()),
                });
            }
        };

        // Skip if schema is not expected to be valid
        if !group.schema_valid && !check_schemas {
            skip_instances(&mut results, "schema expected invalid");
            continue;
        }

        // Load the schema
        let schema_path = match &group.schema_path {
            Some(p) => p,
            None => {
                skip_instances(&mut results, "no schema document");
                continue;
            }
        };
        let mut schema_case = XstsCaseResult {
            kind: "schema",
            group: group.name.clone(),
            name: group.name.clone(),
            path: schema_path.clone(),
            status: group.schema_status.clone(),
            expected_valid: group.schema_valid,
            outcome: XstsOutcome::Pass,
        };

        let schema_str = match fs::read_to_string(schema_path) {
            Ok(s) => s,
            Err(e) => {
                if check_schemas {
                    schema_case.outcome = XstsOutcome::Skip(format!("unreadable: {}", e));
                    results.push(schema_case);
                }
                skip_instances(&mut results, "schema unreadable");
                continue;
            }
        };

        // Parse and compile the schema inside one panic boundary, so that a
        // panic in either is reported for this group only. A schema that
        // fails to parse is rejected.
        let compiled = panic::catch_unwind(AssertUnwindSafe(|| {
            let schema_doc =
                uppsala::parse(&schema_str).map_err(|e| format!("schema parse error: {}", e))?;
            if group.schema_valid {
                eprintln!("  DEBUG: Compiling schema for group '{}'...", group.name);
            }
            XsdValidator::from_schema_with_base_path(&schema_doc, Some(schema_path))
                .map_err(|e| format!("schema error: {}", e))
        }));
        let validator = match compiled {
            Ok(Ok(v)) => {
                if !group.schema_valid {
                    schema_case.outcome =
                        XstsOutcome::FalseAccept("expected invalid schema, compiled".to_string());
                }
                Some(v)
            }
            Ok(Err(e)) => {
                if group.schema_valid {
                    // Can't compile the schema — skip these tests
                    if !group.instance_tests.is_empty() {
                        eprintln!(
                            "  SKIP group '{}' ({} tests): {}",
                            group.name,
                            group.instance_tests.len(),
                            e
                        );
                    }
                    schema_case.outcome = XstsOutcome::FalseRefuse(e);
                }
                None
            }
            Err(p) => {
                let message = panic_message(&p);
                eprintln!(
                    "  PANIC parsing or compiling schema for group '{}' ({} tests skipped): {}",
                    group.name,
                    group.instance_tests.len(),
                    message
                );
                schema_case.outcome = XstsOutcome::Panic(message);
                None
            }
        };
        // A panic is always reported, even when schema tests are not scored,
        // so that callers can count it.
        if check_schemas || matches!(schema_case.outcome, XstsOutcome::Panic(_)) {
            results.push(schema_case);
        }

        if !group.schema_valid {
            skip_instances(&mut results, "schema expected invalid");
            continue;
        }
        let mut validator = match validator {
            Some(v) => v,
            None => {
                skip_instances(&mut results, "schema did not compile");
                continue;
            }
        };
        validator.set_enforce_qname_length_facets(enforce_qname_length_facets);

        for inst_test in &group.instance_tests {
            eprintln!("    DEBUG: Validating instance '{}'", inst_test.name);
            let mut case = XstsCaseResult {
                kind: "instance",
                group: group.name.clone(),
                name: inst_test.name.clone(),
                path: inst_test.path.clone(),
                status: inst_test.status.clone(),
                expected_valid: inst_test.expected_valid,
                outcome: XstsOutcome::Pass,
            };
            let inst_str = match fs::read_to_string(&inst_test.path) {
                Ok(s) => s,
                Err(e) => {
                    case.outcome = XstsOutcome::Skip(format!("unreadable: {}", e));
                    results.push(case);
                    continue;
                }
            };

            // Parse and validate the instance inside one panic boundary, so
            // that a panic in either is reported for this case only.
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                let inst_doc = match uppsala::parse(&inst_str) {
                    Ok(d) => d,
                    Err(e) => {
                        // Expected invalid and we can't even parse — count as pass
                        return if inst_test.expected_valid {
                            XstsOutcome::FalseRefuse(format!("expected valid, parse error: {}", e))
                        } else {
                            XstsOutcome::Pass
                        };
                    }
                };
                let errors = validator.validate(&inst_doc);
                let is_valid = errors.is_empty();
                if is_valid == inst_test.expected_valid {
                    XstsOutcome::Pass
                } else if inst_test.expected_valid {
                    XstsOutcome::FalseRefuse(format!(
                        "expected valid, got {} error(s): {}",
                        errors.len(),
                        errors.first().map(|e| e.to_string()).unwrap_or_default()
                    ))
                } else {
                    XstsOutcome::FalseAccept("expected invalid, got valid".to_string())
                }
            }));
            case.outcome = match outcome {
                Ok(outcome) => outcome,
                Err(p) => XstsOutcome::Panic(panic_message(&p)),
            };
            results.push(case);
        }
    }

    results
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    format!("panic: {}", msg)
}

/// Counts of a run of a test set's instance tests.
#[derive(Debug, Default)]
struct XstsCounts {
    passed: usize,
    failed: usize,
    skipped: usize,
    /// One line per failed instance test, panics included.
    failures: Vec<String>,
    /// One line per panic caught while parsing or compiling a schema, or
    /// while parsing or validating an instance.
    panics: Vec<String>,
    /// One entry per group with skipped instance tests: the group, the
    /// reason and the number of instance tests skipped.
    skipped_groups: Vec<(String, String, usize)>,
}

/// Run XSTS instance tests for a given test set file and count them.
/// A panic while parsing or validating an instance counts as a failed
/// instance test; a panic while parsing or compiling a schema skips the
/// group's instance tests. Both are also listed in `panics`.
/// When `enforce_qname_length_facets` is false, QName/NOTATION length facets are skipped
/// (needed for NIST tests which expect them to be ignored per W3C Bug #4009).
fn run_xsts_instance_tests(test_set_path: &Path, enforce_qname_length_facets: bool) -> XstsCounts {
    let mut counts = XstsCounts::default();

    for case in run_xsts_cases(test_set_path, enforce_qname_length_facets, false) {
        let line = |detail: &str| format!("{} ({}): {}", case.name, case.path.display(), detail);
        if case.kind == "schema" {
            // Only a panic while parsing or compiling the schema is reported
            // here.
            if let XstsOutcome::Panic(detail) = &case.outcome {
                counts
                    .panics
                    .push(format!("schema of group {}", line(detail)));
            }
            continue;
        }
        match &case.outcome {
            XstsOutcome::Pass => counts.passed += 1,
            XstsOutcome::Skip(reason) => {
                counts.skipped += 1;
                match counts.skipped_groups.last_mut() {
                    Some((group, last_reason, n))
                        if *group == case.group && last_reason == reason =>
                    {
                        *n += 1
                    }
                    _ => counts
                        .skipped_groups
                        .push((case.group.clone(), reason.clone(), 1)),
                }
            }
            XstsOutcome::FalseAccept(detail) | XstsOutcome::FalseRefuse(detail) => {
                counts.failures.push(line(detail));
                counts.failed += 1;
            }
            XstsOutcome::Panic(detail) => {
                counts.failures.push(line(detail));
                counts.panics.push(format!("instance {}", line(detail)));
                counts.failed += 1;
            }
        }
    }

    counts
}

/// Print the skipped groups and the panics of a default run, then check it
/// against the counts recorded for the test set: no panic, at least
/// `min_passed` passed, at most `max_failed` failed and at most
/// `max_skipped` skipped, and every skipped group listed in `known_skips`.
/// An improvement passes; any regression, including a newly skipped group,
/// fails.
fn check_xsts_counts(
    label: &str,
    counts: &XstsCounts,
    min_passed: usize,
    max_failed: usize,
    max_skipped: usize,
    known_skips: &[&str],
) {
    for (group, reason, n) in &counts.skipped_groups {
        println!(
            "XSTS {} skipped group '{}': {} test(s), {}",
            label, group, n, reason
        );
    }
    for panic in &counts.panics {
        println!("XSTS {} PANIC: {}", label, panic);
    }
    assert!(
        counts.panics.is_empty(),
        "XSTS {}: {} panic(s) caught:\n  {}",
        label,
        counts.panics.len(),
        counts.panics.join("\n  ")
    );
    assert!(
        counts.passed >= min_passed,
        "XSTS {}: {} passed, fewer than the recorded {}",
        label,
        counts.passed,
        min_passed
    );
    assert!(
        counts.failed <= max_failed,
        "XSTS {}: {} failed, more than the recorded {}",
        label,
        counts.failed,
        max_failed
    );
    assert!(
        counts.skipped <= max_skipped,
        "XSTS {}: {} skipped, more than the recorded {}",
        label,
        counts.skipped,
        max_skipped
    );
    let unexpected: Vec<&str> = counts
        .skipped_groups
        .iter()
        .map(|(group, _, _)| group.as_str())
        .filter(|group| !known_skips.contains(group))
        .collect();
    assert!(
        unexpected.is_empty(),
        "XSTS {}: groups skipped that were not before: {:?}",
        label,
        unexpected
    );
}

/// Run every schema and instance test of a test set, print pass/fail/skip
/// counts per kind, and write one line per case to
/// `$CARGO_TARGET_TMPDIR/xsts/<file stem>.tsv`:
/// outcome, kind, status, expected, group, name, path, detail.
fn run_xsts_full_report(label: &str, test_set_path: &Path) {
    if !test_set_path.exists() {
        eprintln!("XSTS {} test set not found, skipping.", label);
        return;
    }

    let cases = run_xsts_cases(test_set_path, true, true);
    let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("xsts");
    fs::create_dir_all(&out_dir).expect("create xsts output directory");
    let stem = test_set_path.file_stem().unwrap().to_string_lossy();
    let out_path = out_dir.join(format!("{}.tsv", stem));

    let mut lines = String::new();
    for kind in ["schema", "instance"] {
        let (mut pass, mut false_accept, mut false_refuse, mut panics, mut skip) = (0, 0, 0, 0, 0);
        for case in cases.iter().filter(|c| c.kind == kind) {
            let (outcome, detail) = match &case.outcome {
                XstsOutcome::Pass => {
                    pass += 1;
                    ("pass", "")
                }
                XstsOutcome::FalseAccept(d) => {
                    false_accept += 1;
                    ("false-accept", d.as_str())
                }
                XstsOutcome::FalseRefuse(d) => {
                    false_refuse += 1;
                    ("false-refuse", d.as_str())
                }
                XstsOutcome::Panic(d) => {
                    panics += 1;
                    ("panic", d.as_str())
                }
                XstsOutcome::Skip(d) => {
                    skip += 1;
                    ("skip", d.as_str())
                }
            };
            let detail: String = detail
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(300)
                .collect();
            lines.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                outcome,
                case.kind,
                case.status,
                if case.expected_valid {
                    "valid"
                } else {
                    "invalid"
                },
                case.group,
                case.name,
                case.path.display().to_string().replace('\\', "/"),
                detail
            ));
        }
        println!(
            "XSTS {} {} tests: {} passed, {} failed ({} false accept, {} false refuse, {} panic), {} skipped",
            label,
            kind,
            pass,
            false_accept + false_refuse + panics,
            false_accept,
            false_refuse,
            panics,
            skip
        );
    }
    fs::write(&out_path, lines).expect("write xsts case list");
    println!("XSTS {} per-case results: {}", label, out_path.display());
}

/// Sweep every schema and instance document referenced by a testSet through
/// the scan-only pull parser, asserting it accepts/rejects each file exactly
/// like the DOM parser (with the same error text on rejection). One sweep per
/// XSTS family so the pull parser gets the same per-suite regression coverage
/// as `Parser::parse`.
fn run_pull_agreement_sweep(test_set_path: &Path, family: &str) {
    if !test_set_path.exists() {
        eprintln!("XSTS test set not found, skipping pull agreement sweep.");
        return;
    }

    let groups = parse_test_set(test_set_path);
    let mut files: Vec<PathBuf> = Vec::new();
    for group in &groups {
        if let Some(path) = &group.schema_path {
            files.push(path.clone());
        }
        for inst_test in &group.instance_tests {
            files.push(inst_test.path.clone());
        }
    }
    files.sort();
    files.dedup();

    let mut checked = 0;
    let mut skipped = 0;
    let mut divergences = Vec::new();

    for path in &files {
        // The pull parser is a &str surface; unreadable or non-UTF-8 files
        // are out of scope.
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let dom_err = uppsala::parse(&content).err().map(|e| e.to_string());
        let scan_err = uppsala::PullParser::new(&content)
            .find_map(|event| event.err())
            .map(|e| e.to_string());

        checked += 1;
        if dom_err != scan_err {
            divergences.push(format!(
                "  {}: DOM parser {:?} vs pull scan {:?}",
                path.display(),
                dom_err,
                scan_err
            ));
        }
    }

    println!(
        "XSTS {} pull-vs-DOM agreement: {} files checked, {} skipped, {} divergences",
        family,
        checked,
        skipped,
        divergences.len()
    );
    assert!(
        divergences.is_empty(),
        "pull event stream diverged from the DOM parser:\n{}",
        divergences.join("\n")
    );
}

#[test]
fn xsts_nist_datatypes_pull_agreement() {
    run_pull_agreement_sweep(
        Path::new("test-data/xsts/xmlschema2006-11-06/nistMeta/NISTXMLSchemaDatatypes.testSet"),
        "NIST Datatypes",
    );
}

#[test]
fn xsts_ms_datatypes_pull_agreement() {
    run_pull_agreement_sweep(
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/DataTypes_w3c.xml"),
        "MS DataTypes",
    );
}

#[test]
fn xsts_sun_combined_pull_agreement() {
    run_pull_agreement_sweep(
        Path::new("test-data/xsts/xmlschema2006-11-06/sunMeta/suntest.testSet"),
        "Sun Combined",
    );
}

#[test]
fn xsts_nist_datatypes() {
    let test_set_path =
        Path::new("test-data/xsts/xmlschema2006-11-06/nistMeta/NISTXMLSchemaDatatypes.testSet");
    if !test_set_path.exists() {
        eprintln!("XSTS test suite not found, skipping. Download from W3C.");
        return;
    }

    // NIST tests expect QName/NOTATION length facets to be ignored (W3C Bug #4009)
    let counts = run_xsts_instance_tests(test_set_path, false);
    let XstsCounts {
        passed,
        failed,
        skipped,
        ref failures,
        ..
    } = counts;

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
        for f in failures {
            if f.contains("/list/") && list_count < 5 {
                println!("  {}", f);
                list_count += 1;
            }
        }
        // Breakdown by datatype
        let mut by_type: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for f in failures {
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
        breakdown.sort_by_key(|b| std::cmp::Reverse(b.1));
        println!("\nFailure breakdown by type:");
        for (dtype, count) in &breakdown {
            println!("  {:>5} {}", count, dtype);
        }
    }

    // Recorded counts: 19217 passed, 0 failed, 0 skipped.
    check_xsts_counts("NIST Datatypes", &counts, 19217, 0, 0, &[]);
}

#[test]
fn xsts_sun_combined() {
    let test_set_path = Path::new("test-data/xsts/xmlschema2006-11-06/sunMeta/suntest.testSet");
    if !test_set_path.exists() {
        eprintln!("XSTS Sun test set not found, skipping.");
        return;
    }

    let counts = run_xsts_instance_tests(test_set_path, true);
    let XstsCounts {
        passed,
        failed,
        skipped,
        ref failures,
        ..
    } = counts;

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

    // Recorded counts: 199 passed, 0 failed, 0 skipped.
    check_xsts_counts("Sun Combined", &counts, 199, 0, 0, &[]);
}

#[test]
fn xsts_ms_datatypes() {
    let test_set_path = Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/DataTypes_w3c.xml");
    if !test_set_path.exists() {
        eprintln!("XSTS MS DataTypes test set not found, skipping.");
        return;
    }

    let counts = run_xsts_instance_tests(test_set_path, true);
    let XstsCounts {
        passed,
        failed,
        skipped,
        ref failures,
        ..
    } = counts;

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

    // Recorded counts: 1212 passed, 0 failed, 1 skipped. The skipped test is
    // group anyURI_a004_1339, whose schema includes an absolute ftp:// URI,
    // which the schema loader refuses by design; no other group may be skipped.
    check_xsts_counts("MS DataTypes", &counts, 1212, 0, 1, &["anyURI_a004_1339"]);
}

// Full schema + instance runs over the remaining XSTS 2006 test sets. These
// are conformance reports, not regression gates: run them explicitly with
// `cargo test --test w3c_xsts -- --ignored --nocapture xsts_full_`.

#[test]
#[ignore]
fn xsts_full_ms_datatypes() {
    run_xsts_full_report(
        "MS DataTypes",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/DataTypes_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_simple_type() {
    run_xsts_full_report(
        "MS SimpleType",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/SimpleType_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_attribute() {
    run_xsts_full_report(
        "MS Attribute",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Attribute_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_element() {
    run_xsts_full_report(
        "MS Element",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Element_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_complex_type() {
    run_xsts_full_report(
        "MS ComplexType",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/ComplexType_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_regex() {
    run_xsts_full_report(
        "MS Regex",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Regex_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_particles() {
    run_xsts_full_report(
        "MS Particles",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Particles_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_identity_constraint() {
    run_xsts_full_report(
        "MS IdentityConstraint",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/IdentityConstraint_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_model_groups() {
    run_xsts_full_report(
        "MS ModelGroups",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/ModelGroups_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_additional() {
    run_xsts_full_report(
        "MS Additional",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Additional_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_wildcards() {
    run_xsts_full_report(
        "MS Wildcards",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Wildcards_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_group() {
    run_xsts_full_report(
        "MS Group",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Group_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_schema() {
    run_xsts_full_report(
        "MS Schema",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Schema_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_attribute_group() {
    run_xsts_full_report(
        "MS AttributeGroup",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/AttributeGroup_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_notations() {
    run_xsts_full_report(
        "MS Notations",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Notations_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_annotations() {
    run_xsts_full_report(
        "MS Annotations",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Annotations_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_ms_errata10() {
    run_xsts_full_report(
        "MS Errata10",
        Path::new("test-data/xsts/xmlschema2006-11-06/msMeta/Errata10_w3c.xml"),
    );
}

#[test]
#[ignore]
fn xsts_full_boeing() {
    run_xsts_full_report(
        "Boeing",
        Path::new("test-data/xsts/xmlschema2006-11-06/boeingMeta/BoeingXSDTestSet.testSet"),
    );
}
