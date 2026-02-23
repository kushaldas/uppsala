# Uppsala - Pure Rust XML Library
default:
    @just --list

# Run all tests
test:
    cargo test

# Run unit tests only
unit:
    cargo test --lib

# Run XML 1.0 conformance tests (68 tests)
test-xml:
    cargo test --test xml_conformance

# Run namespace conformance tests (16 tests)
test-ns:
    cargo test --test namespace_conformance

# Run XPath 1.0 conformance tests (66 tests)
test-xpath:
    cargo test --test xpath_conformance

# Run XSD conformance tests (38 tests)
test-xsd:
    cargo test --test xsd_conformance

# Run serialization conformance tests (68 tests)
test-serial:
    cargo test --test serialization_conformance

# Run range conformance tests
test-range:
    cargo test --test range_conformance

# Run W3C XML Conformance Suite (~1208 tests)
test-w3c-xml:
    cargo test --test w3c_xmlconf -- --nocapture

# Run W3C XML Schema Test Suite - all suites (~20156 tests)
test-w3c-xsd:
    cargo test --test w3c_xsts -- --nocapture

# Run NIST Datatypes tests (~19217 tests)
test-nist:
    cargo test --test w3c_xsts xsts_nist_datatypes -- --nocapture

# Run MS DataTypes tests (~1213 tests)
test-ms:
    cargo test --test w3c_xsts xsts_ms_datatypes -- --nocapture

# Run Sun Combined tests (~199 tests)
test-sun:
    cargo test --test w3c_xsts xsts_sun_combined -- --nocapture

# Run all hand-crafted test suites
test-handcrafted: test-xml test-ns test-xpath test-xsd test-serial test-range

# Run all W3C conformance suites
test-w3c: test-w3c-xml test-w3c-xsd

# Check the project compiles without errors
check:
    cargo check

# Build in release mode
build:
    cargo build --release

# Run clippy lints
clippy:
    cargo clippy -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting without modifying files
fmt-check:
    cargo fmt -- --check
