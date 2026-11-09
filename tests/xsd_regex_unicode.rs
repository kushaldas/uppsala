//! Unicode character classes of XSD pattern facets (XSD 1.0 Part 2, Appendix
//! F): `\d` is `\p{Nd}` in every script and plane, `\w` is every character
//! outside the categories P, Z and C, `\p{..}` and `\P{..}` follow the general
//! categories, `\i` and `\c` are XML 1.0 (Second Edition) `Letter` and
//! `NameChar`, and `\p{Is..}` takes XSD 1.0's fixed block list.

mod common;
use common::parse;

use uppsala::{XsdRegex, XsdValidator};

/// Whether `pattern` matches the one-character string `ch`.
fn matches(pattern: &str, ch: char) -> bool {
    XsdRegex::compile(pattern)
        .unwrap_or_else(|e| panic!("{pattern}: {e}"))
        .is_match(&ch.to_string())
}

/// Assert `pattern` matches exactly the characters marked `true`, and that
/// the complement `complement` matches exactly the others.
fn check_class(pattern: &str, complement: &str, cases: &[(char, bool)]) {
    for &(ch, expected) in cases {
        assert_eq!(
            matches(pattern, ch),
            expected,
            "{pattern} on U+{:04X}",
            ch as u32
        );
        assert_eq!(
            matches(complement, ch),
            !expected,
            "{complement} on U+{:04X}",
            ch as u32
        );
    }
}

fn ch(cp: u32) -> char {
    char::from_u32(cp).expect("a scalar value")
}

#[test]
fn d_matches_the_decimal_digits_of_every_script() {
    // (first, last) of decimal digit ranges in several scripts and planes,
    // and the characters just outside each, none of which is Nd.
    let ranges = [
        (0x0030, 0x0039),   // ASCII
        (0x0660, 0x0669),   // Arabic-Indic
        (0x06F0, 0x06F9),   // Extended Arabic-Indic
        (0x07C0, 0x07C9),   // NKo
        (0x0966, 0x096F),   // Devanagari
        (0x0BE6, 0x0BEF),   // Tamil, zero added after the rest
        (0x0E50, 0x0E59),   // Thai
        (0xFF10, 0xFF19),   // fullwidth
        (0x104A0, 0x104A9), // Osmanya
        (0x1D7CE, 0x1D7FF), // mathematical digits, five runs
        (0x1E950, 0x1E959), // Adlam
        (0x1FBF0, 0x1FBF9), // segmented digits
    ];
    for (first, last) in ranges {
        for digit in [ch(first), ch(last)] {
            for pattern in [r"\d", r"\p{Nd}", r"[\d]", r"\p{N}"] {
                assert!(
                    matches(pattern, digit),
                    "{pattern} on U+{:04X}",
                    digit as u32
                );
            }
            for pattern in [r"\D", r"\P{Nd}", r"[^\d]", r"[\D]"] {
                assert!(
                    !matches(pattern, digit),
                    "{pattern} on U+{:04X}",
                    digit as u32
                );
            }
        }
        for outside in [ch(first - 1), ch(last + 1)] {
            assert!(!matches(r"\d", outside), "\\d on U+{:04X}", outside as u32);
            assert!(matches(r"\D", outside), "\\D on U+{:04X}", outside as u32);
        }
    }
    // Numbers that are not decimal digits: superscript two (No), Roman
    // numeral twelve (Nl), Tamil number ten (No), Ethiopic digit one (No).
    for other in ['\u{00B2}', '\u{216B}', '\u{0BF0}', '\u{1369}'] {
        assert!(!matches(r"\d", other), "U+{:04X}", other as u32);
        assert!(matches(r"\D", other), "U+{:04X}", other as u32);
        assert!(matches(r"\p{N}", other), "U+{:04X}", other as u32);
    }
}

#[test]
fn supplementary_plane_digits_match_d_in_sequence() {
    let re = XsdRegex::compile(r"\d{4}").unwrap();
    assert!(re.is_match("\u{1D7CE}\u{1D7D9}\u{1E950}\u{1FBF9}"));
    assert!(re.is_match("0\u{0661}\u{104A2}9"));
    assert!(!re.is_match("\u{1D7CE}\u{1D7D9}\u{1E950}"));
    assert!(!re.is_match("\u{1D7CE}\u{1D7D9}\u{1E950}\u{1D400}"));
    assert!(!XsdRegex::compile("[0-9]").unwrap().is_match("\u{1D7CE}"));
    assert!(XsdRegex::compile(r"\D+")
        .unwrap()
        .is_match("\u{1D400}\u{10000}\u{20000}"));
    assert!(!XsdRegex::compile(r"\D").unwrap().is_match("\u{1E950}"));
}

#[test]
fn w_excludes_punctuation_separators_and_others() {
    let cases = [
        // Letters, marks, numbers and symbols are word characters.
        ('a', true),
        ('Z', true),
        ('\u{0416}', true),  // Cyrillic Zhe, Lu
        ('\u{4E2D}', true),  // CJK ideograph, Lo
        ('\u{01C5}', true),  // Lt
        ('\u{02B0}', true),  // Lm
        ('\u{20000}', true), // CJK Extension B, Lo
        ('\u{0301}', true),  // Mn
        ('\u{0903}', true),  // Mc
        ('7', true),
        ('\u{0669}', true), // Nd
        ('\u{00B2}', true), // No
        ('\u{216B}', true), // Nl
        ('$', true),        // Sc
        ('+', true),        // Sm
        ('^', true),        // Sk
        ('\u{00A9}', true), // So
        // Punctuation is not.
        ('_', false),        // Pc
        ('-', false),        // Pd
        ('(', false),        // Ps
        (')', false),        // Pe
        ('\u{00AB}', false), // Pi
        ('\u{00BB}', false), // Pf
        ('.', false),
        ('!', false),
        ('\u{060C}', false), // Arabic comma, Po
        // Separators are not.
        (' ', false),
        ('\u{00A0}', false),
        ('\u{3000}', false),
        ('\u{2028}', false), // Zl
        ('\u{2029}', false), // Zp
        // Other characters are not: controls, format, private use, unassigned.
        ('\t', false),
        ('\u{007F}', false),
        ('\u{0085}', false),
        ('\u{00AD}', false),  // Cf
        ('\u{200B}', false),  // Cf
        ('\u{E000}', false),  // Co
        ('\u{F0000}', false), // Co, plane 15
        ('\u{0378}', false),  // Cn
        ('\u{E0080}', false), // Cn
    ];
    check_class(r"\w", r"\W", &cases);
    check_class(r"[\w]", r"[^\w]", &cases);
    check_class(r"[\w]", r"[\W]", &cases);
    // The same through classes whose category members are folded into one set
    // (every `\p{Lu}` sample is a word character already).
    check_class(r"[\w\p{Lu}]", r"[^\w\p{Lu}]", &cases);
    check_class(r"[\w\p{Lu}]", r"[\W\p{Zs}-[\p{Lu}]]", &cases);
}

#[test]
fn d_and_not_d_split_letters_punctuation_separators_and_controls() {
    let cases = [
        ('5', true),
        ('\u{0966}', true),
        ('a', false),
        ('\u{0416}', false),
        ('.', false),
        ('_', false),
        (' ', false),
        ('\u{3000}', false),
        ('\t', false),
        ('\u{0085}', false),
        ('\u{E000}', false),
        ('\u{0378}', false),
    ];
    check_class(r"\d", r"\D", &cases);
    // The same through classes whose category members are folded into one set.
    check_class(r"[\d\p{Nd}]", r"[\D\p{Lu}]", &cases);
    check_class(r"[\p{Nd}\d]", r"[^\d\p{Nd}]", &cases);
}

#[test]
fn p_matches_each_general_category() {
    let samples: [(&str, char); 31] = [
        ("Lu", 'A'),
        ("Ll", 'a'),
        ("Lt", '\u{01C5}'),
        ("Lm", '\u{02B0}'),
        ("Lo", '\u{05D0}'),
        ("Mn", '\u{0301}'),
        ("Mc", '\u{0903}'),
        ("Me", '\u{20DD}'),
        ("Nd", '\u{0BE6}'),
        ("Nl", '\u{216B}'),
        ("No", '\u{00B2}'),
        ("Pc", '_'),
        ("Pd", '-'),
        ("Ps", '('),
        ("Pe", ')'),
        ("Pi", '\u{00AB}'),
        ("Pf", '\u{00BB}'),
        ("Po", '!'),
        ("Sm", '+'),
        ("Sc", '$'),
        ("Sk", '^'),
        ("So", '\u{00A9}'),
        ("Zs", ' '),
        ("Zl", '\u{2028}'),
        ("Zp", '\u{2029}'),
        ("Cc", '\u{0085}'),
        ("Cf", '\u{200B}'),
        ("Co", '\u{E000}'),
        ("Cn", '\u{0378}'),
        ("Lo", '\u{20000}'),
        ("Co", '\u{100000}'),
    ];
    for (category, sample) in samples {
        let group = &category[..1];
        for (name, positive) in [(category, true), (group, true)] {
            assert_eq!(
                matches(&format!(r"\p{{{name}}}"), sample),
                positive,
                "{name}"
            );
            assert_eq!(
                matches(&format!(r"\P{{{name}}}"), sample),
                !positive,
                "{name}"
            );
        }
        // Every other category of the sample set refuses the sample.
        for (other, _) in samples.iter().filter(|(c, _)| *c != category) {
            assert!(
                !matches(&format!(r"\p{{{other}}}"), sample),
                "\\p{{{other}}} on U+{:04X}",
                sample as u32
            );
        }
    }
    // Cs is not an XSD category, and unknown names are refused.
    for name in ["Cs", "Lx", "L&", "IsLu", "Letter"] {
        assert!(
            XsdRegex::compile(&format!(r"\p{{{name}}}")).is_err(),
            "{name}"
        );
        assert!(
            XsdRegex::compile(&format!(r"[\P{{{name}}}]")).is_err(),
            "{name}"
        );
    }
}

#[test]
fn is_block_names_are_the_xsd_1_0_list_with_its_ranges() {
    let cases: [(&str, u32, bool); 22] = [
        ("IsBasicLatin", 0x7F, true),
        ("IsBasicLatin", 0x80, false),
        ("IsGreek", 0x03B1, true),
        ("IsHangulSyllables", 0xD7A3, true),
        ("IsHangulSyllables", 0xD7A4, false),
        ("IsCJKUnifiedIdeographsExtensionA", 0x4DB5, true),
        ("IsCJKUnifiedIdeographsExtensionA", 0x4DB6, false),
        ("IsArabicPresentationForms-B", 0xFEFE, true),
        ("IsArabicPresentationForms-B", 0xFEFF, false),
        ("IsSpecials", 0xFEFF, true),
        ("IsSpecials", 0xFFF0, true),
        ("IsSpecials", 0xFFFD, true),
        ("IsSpecials", 0xFFEF, false),
        ("IsPrivateUse", 0xE000, true),
        ("IsPrivateUse", 0xF0000, false),
        ("IsOldItalic", 0x10300, true),
        ("IsCJKUnifiedIdeographsExtensionB", 0x2A6D6, true),
        ("IsCJKUnifiedIdeographsExtensionB", 0x2A6D7, false),
        ("IsTags", 0xE0001, true),
        ("IsCombiningMarksforSymbols", 0x20DD, true),
        ("IsHighSurrogates", 0xD7FF, false),
        ("IsLowSurrogates", 0xE000, false),
    ];
    for (name, cp, expected) in cases {
        assert_eq!(
            matches(&format!(r"\p{{{name}}}"), ch(cp)),
            expected,
            "{name} U+{cp:04X}"
        );
        assert_eq!(
            matches(&format!(r"\P{{{name}}}"), ch(cp)),
            !expected,
            "{name} U+{cp:04X}"
        );
    }
    // Other block names of later Unicode versions are not recognized.
    for name in [
        "IsNKo",
        "IsCyrillicExtended-A",
        "IsSupplementaryPrivateUseArea-A",
        "IsGreekAndCoptic",
        "IsBasicLatin1",
    ] {
        assert!(
            XsdRegex::compile(&format!(r"\p{{{name}}}")).is_err(),
            "{name}"
        );
        assert!(
            XsdRegex::compile(&format!(r"\P{{{name}}}")).is_err(),
            "{name}"
        );
    }
}

#[test]
fn block_names_accepted_before_still_build_with_their_ranges() {
    // Names outside XSD 1.0's list that earlier releases accepted, with the
    // ranges they matched: each still compiles, matches its first and last
    // code point and not the code points just outside.
    let blocks: [(&str, u32, u32); 20] = [
        ("GreekandCoptic", 0x0370, 0x03FF),
        ("CyrillicSupplement", 0x0500, 0x052F),
        ("Tagalog", 0x1700, 0x171F),
        ("Hanunoo", 0x1720, 0x173F),
        ("Buhid", 0x1740, 0x175F),
        ("Tagbanwa", 0x1760, 0x177F),
        ("Limbu", 0x1900, 0x194F),
        ("TaiLe", 0x1950, 0x197F),
        ("KhmerSymbols", 0x19E0, 0x19FF),
        ("PhoneticExtensions", 0x1D00, 0x1D7F),
        ("CombiningDiacriticalMarksforSymbols", 0x20D0, 0x20FF),
        ("MiscellaneousMathematicalSymbols-A", 0x27C0, 0x27EF),
        ("SupplementalArrows-A", 0x27F0, 0x27FF),
        ("SupplementalArrows-B", 0x2900, 0x297F),
        ("MiscellaneousMathematicalSymbols-B", 0x2980, 0x29FF),
        ("SupplementalMathematicalOperators", 0x2A00, 0x2AFF),
        ("KatakanaPhoneticExtensions", 0x31F0, 0x31FF),
        ("YijingHexagramSymbols", 0x4DC0, 0x4DFF),
        ("PrivateUseArea", 0xE000, 0xF8FF),
        ("VariationSelectors", 0xFE00, 0xFE0F),
    ];
    for (name, first, last) in blocks {
        let p = format!(r"\p{{Is{name}}}");
        let not_p = format!(r"\P{{Is{name}}}");
        for (cp, inside) in [
            (first, true),
            (last, true),
            (first - 1, false),
            (last + 1, false),
        ] {
            // U+DFFF, before PrivateUseArea, is not a character.
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            assert_eq!(matches(&p, c), inside, "{p} U+{cp:04X}");
            assert_eq!(matches(&not_p, c), !inside, "{not_p} U+{cp:04X}");
        }
    }
}

#[test]
fn i_and_c_are_the_xml_1_0_second_edition_name_classes() {
    let cases: [(u32, bool, bool); 22] = [
        // (code point, \i, \c)
        (u32::from('A'), true, true),
        (u32::from('_'), true, true),
        (u32::from(':'), true, true),
        (u32::from('1'), false, true),
        (u32::from('-'), false, true),
        (u32::from('.'), false, true),
        (u32::from(' '), false, false),
        (0x00E9, true, true),   // BaseChar
        (0x4E00, true, true),   // Ideographic
        (0x9FA5, true, true),   // Ideographic, last
        (0x3007, true, true),   // Ideographic
        (0x0301, false, true),  // CombiningChar
        (0x00B7, false, true),  // Extender
        (0x0660, false, true),  // Digit
        (0x0BE6, false, false), // a digit since Unicode 4.1, not in Digit
        (0x0221, false, false), // a letter since Unicode 4.0, not in BaseChar
        (0x9FA6, false, false), // past Ideographic
        (0x3400, false, false), // CJK Extension A
        (0x2160, false, false), // Roman numeral, Nl
        (0x00D7, false, false), // multiplication sign
        (0x1D400, false, false),
        (0x20000, false, false),
    ];
    for (cp, initial, name) in cases {
        let c = ch(cp);
        assert_eq!(matches(r"\i", c), initial, "\\i U+{cp:04X}");
        assert_eq!(matches(r"\I", c), !initial, "\\I U+{cp:04X}");
        assert_eq!(matches(r"\c", c), name, "\\c U+{cp:04X}");
        assert_eq!(matches(r"\C", c), !name, "\\C U+{cp:04X}");
    }
}

// ─── Through a schema ───────────────────────────────────────────────────────

const XS: &str = r#"xmlns:xs="http://www.w3.org/2001/XMLSchema""#;

fn validator(body: &str) -> XsdValidator {
    let xsd = format!(r#"<xs:schema {XS}>{body}</xs:schema>"#);
    XsdValidator::from_schema(&parse(&xsd).expect("schema parses")).expect("schema builds")
}

fn assert_values(validator: &XsdValidator, cases: &[(&str, bool)]) {
    for (value, valid) in cases {
        let xml = format!("<e>{value}</e>");
        let errors = validator.validate(&parse(&xml).expect("instance parses"));
        assert_eq!(errors.is_empty(), *valid, "{value:?}: {errors:?}");
    }
}

#[test]
fn digit_pattern_with_leading_nonzero_accepts_digits_of_any_script() {
    let v = validator(
        r#"<xs:element name="e"><xs:simpleType><xs:restriction base="xs:string">
             <xs:pattern value="[1-9]\d{3}(\d[1-9]|[1-9]\d)"/>
           </xs:restriction></xs:simpleType></xs:element>"#,
    );
    assert_values(
        &v,
        &[
            ("123456", true),
            ("12345\u{0663}", true),
            ("1\u{0662}3456", true),
            ("023456", false),
            ("12345", false),
            ("12345a", false),
            ("12345\u{00B2}", false),
        ],
    );
}

#[test]
fn fixed_length_digit_pattern_accepts_digits_of_any_script() {
    let v = validator(
        r#"<xs:element name="e"><xs:simpleType><xs:restriction base="xs:string">
             <xs:pattern value="\d{8}"/>
           </xs:restriction></xs:simpleType></xs:element>"#,
    );
    assert_values(
        &v,
        &[
            ("87654321", true),
            ("8765432\u{0669}", true),
            ("\u{1D7CE}7654321", true),
            ("8765432", false),
            ("8765432x", false),
            ("8765432\u{2165}", false),
        ],
    );
}

#[test]
fn alternation_of_two_digit_lengths_accepts_digits_of_any_script() {
    let v = validator(
        r#"<xs:element name="e"><xs:simpleType><xs:restriction base="xs:string">
             <xs:pattern value="\d{5}|\d{11}"/>
           </xs:restriction></xs:simpleType></xs:element>"#,
    );
    assert_values(
        &v,
        &[
            ("24680", true),
            ("2468\u{0667}", true),
            ("1357913579\u{0667}", true),
            ("246801", false),
            ("2468x", false),
        ],
    );
}
