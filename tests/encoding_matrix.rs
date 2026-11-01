//! Encoding & internationalization matrix.
//!
//! uppsala auto-detects UTF-8 and UTF-16 (LE/BE, with or without BOM) in
//! `parse_bytes` (see `lib.rs::decode_xml_bytes`, XML 1.0 Appendix F). This
//! suite proves:
//!
//! - the *same* document encoded six ways parses to the *same* tree;
//! - a declared-vs-actual encoding mismatch has deterministic behavior
//!   (content-based auto-detection wins, and string serialization normalizes
//!   any retained encoding declaration to UTF-8);
//! - multi-script / astral / combining content survives a round trip;
//! - malformed byte streams (odd UTF-16, lone surrogate, invalid UTF-8, NUL,
//!   illegal control chars) produce a clean `Err`, never a panic.
//!
//! The matrix is generated in-process, so this suite needs no vendored corpus
//! and always runs.

use uppsala::{parse, parse_bytes};

/// A document deliberately spanning several scripts and Unicode planes:
/// Latin-1 accent, CJK, Arabic (RTL), Hebrew (RTL), an astral-plane emoji
/// (surrogate pair in UTF-16), and a combining mark.
const DOC: &str = "<doc lang=\"mix\">Hello \u{00e9} \u{4e16}\u{754c} \
                   \u{0645}\u{0631}\u{062d}\u{0628}\u{0627} \u{05e9}\u{05dc}\u{05d5}\u{05dd} \
                   \u{1f600} a\u{0301}</doc>";

fn utf16_le(s: &str, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend_from_slice(&[0xFF, 0xFE]);
    }
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

fn utf16_be(s: &str, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend_from_slice(&[0xFE, 0xFF]);
    }
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

fn utf8(s: &str, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    out.extend_from_slice(s.as_bytes());
    out
}

// ─── Same document, six encodings, one tree ────────────────────────────────

/// UTF-8 (±BOM) and UTF-16 LE/BE (±BOM) must all decode to the identical DOM,
/// verified by comparing the canonical serialization.
#[test]
fn same_document_across_all_encodings() {
    let oracle = parse(DOC).expect("UTF-8 source parses").to_xml();

    let variants: [(&str, Vec<u8>); 6] = [
        ("utf8-no-bom", utf8(DOC, false)),
        ("utf8-bom", utf8(DOC, true)),
        ("utf16le-bom", utf16_le(DOC, true)),
        ("utf16be-bom", utf16_be(DOC, true)),
        ("utf16le-no-bom", utf16_le(DOC, false)),
        ("utf16be-no-bom", utf16_be(DOC, false)),
    ];

    for (name, bytes) in &variants {
        let doc =
            parse_bytes(bytes).unwrap_or_else(|e| panic!("{name}: parse_bytes failed: {e:?}"));
        assert_eq!(
            doc.to_xml(),
            oracle,
            "{name}: decoded tree differs from the UTF-8 oracle"
        );
        eprintln!("encoding {name}: {} bytes -> identical tree", bytes.len());
    }
}

/// The no-BOM UTF-16 detection keys off a leading `\0<` / `<\0`. Confirm both
/// orientations are picked correctly (regression guard for Appendix F).
#[test]
fn utf16_without_bom_is_detected_by_leading_bytes() {
    let le = utf16_le(DOC, false);
    let be = utf16_be(DOC, false);
    assert_eq!(
        &le[0..2],
        &[0x3C, 0x00],
        "UTF-16LE should start with '<' \\0"
    );
    assert_eq!(
        &be[0..2],
        &[0x00, 0x3C],
        "UTF-16BE should start with \\0 '<'"
    );
    assert!(parse_bytes(&le).is_ok());
    assert!(parse_bytes(&be).is_ok());
}

/// Only the first byte order mark is the encoding signature (XML 1.0 §4.3.3).
/// A second U+FEFF is a character before the prolog, so the document is not
/// well-formed, whichever encoding carries it.
#[test]
fn second_byte_order_mark_rejected() {
    let variants: [(&str, Vec<u8>); 3] = [
        ("utf8", utf8("\u{FEFF}<r/>", true)),
        ("utf16le", utf16_le("\u{FEFF}<r/>", true)),
        ("utf16be", utf16_be("\u{FEFF}<r/>", true)),
    ];

    for (name, bytes) in &variants {
        assert!(
            parse_bytes(bytes).is_err(),
            "{name}: a second byte order mark must be rejected"
        );
    }
    assert!(parse_bytes(&utf8("<r/>", true)).is_ok());
}

// ─── Declared-vs-actual mismatch (documented, no smuggling) ────────────────

/// Bytes are UTF-16LE (BOM present) but the XML declaration says UTF-8.
/// Content-based detection (the BOM) wins; the document still decodes to the
/// same tree. The bogus declaration does not smuggle anything.
#[test]
fn declared_utf8_but_actually_utf16_uses_bom() {
    let src = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><doc>\u{4e16}\u{754c}</doc>";
    let oracle = parse(src).unwrap().to_xml();
    let bytes = utf16_le(src, true);
    let doc = parse_bytes(&bytes).expect("BOM-detected UTF-16 decodes despite UTF-8 declaration");
    assert_eq!(doc.to_xml(), oracle);
}

/// Bytes are UTF-8 but the declaration says UTF-16. There is no BOM and the
/// stream starts with `<?` (0x3C 0x3F), so detection stays UTF-8. The parsed
/// declaration is normalized to UTF-8 for string serialization instead of
/// preserving stale metadata.
#[test]
fn declared_utf16_but_actually_utf8_stays_utf8() {
    let src = "<?xml version=\"1.0\" encoding=\"UTF-16\"?><doc>plain</doc>";
    let doc = parse_bytes(src.as_bytes()).expect("UTF-8 bytes decode as UTF-8 regardless of decl");
    assert_eq!(
        doc.document_element().map(|r| doc.text_content_deep(r)),
        Some("plain".into())
    );
    assert_eq!(
        doc.to_xml(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><doc>plain</doc>"
    );
}

// ─── Malformed byte streams → clean error, never a panic ───────────────────

/// An odd-length UTF-16 stream (orphan trailing byte) must be rejected, not
/// silently truncated.
#[test]
fn odd_length_utf16_rejected() {
    let mut bytes = utf16_le("<a/>", true);
    bytes.push(0x00); // orphan byte breaks the 16-bit unit invariant
    assert!(
        parse_bytes(&bytes).is_err(),
        "odd-length UTF-16 must be rejected"
    );
}

/// A lone high surrogate (no trailing low surrogate) is not valid UTF-16 and
/// must fail decoding rather than panic.
#[test]
fn lone_surrogate_utf16_rejected() {
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE]; // LE BOM
    bytes.extend_from_slice(&0x003Cu16.to_le_bytes()); // '<'
    bytes.extend_from_slice(&0xD800u16.to_le_bytes()); // lone high surrogate
    bytes.extend_from_slice(&0x002Fu16.to_le_bytes()); // '/'
    bytes.extend_from_slice(&0x003Eu16.to_le_bytes()); // '>'
    assert!(
        parse_bytes(&bytes).is_err(),
        "lone surrogate must be rejected"
    );
}

/// Invalid / overlong UTF-8 must fail decoding cleanly.
#[test]
fn invalid_utf8_rejected() {
    // 0xC0 0x80 is the classic overlong encoding of NUL; invalid UTF-8.
    let bytes = b"<a>\xC0\x80</a>";
    assert!(
        parse_bytes(bytes).is_err(),
        "overlong UTF-8 must be rejected"
    );
    // A lone continuation byte is also invalid.
    let bytes2 = b"<a>\x80</a>";
    assert!(
        parse_bytes(bytes2).is_err(),
        "stray continuation byte must be rejected"
    );
}

/// A raw NUL and other illegal XML control characters (valid UTF-8, but not
/// legal XML 1.0 characters) must be rejected by well-formedness — not parsed,
/// not panicked on.
#[test]
fn illegal_control_chars_rejected() {
    for (label, ch) in [("NUL", "\u{0000}"), ("SOH", "\u{0001}"), ("VT", "\u{000B}")] {
        let doc = format!("<a>{ch}</a>");
        assert!(
            parse(&doc).is_err(),
            "{label}: illegal XML control char must be rejected"
        );
        // The byte-oriented path must agree and not panic.
        assert!(
            parse_bytes(doc.as_bytes()).is_err(),
            "{label}: parse_bytes must reject too"
        );
    }
}

// ─── Optional vendored i18n corpus (skips if absent) ───────────────────────

/// If W3C i18n files were vendored under test-data/corpus/encoding/, every one
/// must parse or reject without panicking. Purely a no-crash sweep.
#[test]
fn vendored_i18n_corpus_never_panics() {
    use std::path::PathBuf;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-data")
        .join("corpus")
        .join("encoding");
    if !dir.exists() {
        eprintln!("encoding corpus absent, skipping vendored i18n sweep");
        return;
    }
    let mut n = 0usize;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("xml") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                let _ = parse_bytes(&bytes); // must not panic
                n += 1;
            }
        }
    }
    eprintln!("i18n corpus: swept {n} files, no panics");
}
