//! Generator for `src/xsd_regex_tables.rs`, the character tables of the XSD
//! regular expression engine. It is run by hand, never by the build:
//!
//! ```text
//! cargo run --example gen_unicode_tables -- \
//!     <UnicodeData.txt> <DerivedGeneralCategory.txt> <xml-1.0-second-edition.html> \
//!     src/xsd_regex_tables.rs
//! ```
//!
//! Inputs:
//! - `UnicodeData.txt` and `extracted/DerivedGeneralCategory.txt` from one
//!   version of the Unicode Character Database
//!   (<https://www.unicode.org/Public/UCD/latest/ucd/>). The general category
//!   of every code point is read from `DerivedGeneralCategory.txt` and must
//!   agree with `UnicodeData.txt` (its `First>`/`Last>` ranges included);
//!   any disagreement stops the generator.
//! - The HTML text of XML 1.0 (Second Edition), the edition XSD 1.0 (Second
//!   Edition) references for `\i` and `\c`. Its Appendix B "Character Classes"
//!   gives the `BaseChar`, `Ideographic`, `CombiningChar`, `Digit` and
//!   `Extender` productions behind `Letter` and `NameChar`.
//!
//! The output is plain Rust data, written in one fixed layout, so two runs on
//! the same inputs give byte-identical files. The generator uses only the
//! standard library.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::process::ExitCode;

/// The general categories, in the order of the generated enum.
const CATEGORIES: [(&str, &str); 30] = [
    ("Lu", "uppercase letter"),
    ("Ll", "lowercase letter"),
    ("Lt", "titlecase letter"),
    ("Lm", "modifier letter"),
    ("Lo", "other letter"),
    ("Mn", "nonspacing mark"),
    ("Mc", "spacing combining mark"),
    ("Me", "enclosing mark"),
    ("Nd", "decimal digit number"),
    ("Nl", "letter number"),
    ("No", "other number"),
    ("Pc", "connector punctuation"),
    ("Pd", "dash punctuation"),
    ("Ps", "open punctuation"),
    ("Pe", "close punctuation"),
    ("Pi", "initial quote punctuation"),
    ("Pf", "final quote punctuation"),
    ("Po", "other punctuation"),
    ("Sm", "math symbol"),
    ("Sc", "currency symbol"),
    ("Sk", "modifier symbol"),
    ("So", "other symbol"),
    ("Zs", "space separator"),
    ("Zl", "line separator"),
    ("Zp", "paragraph separator"),
    ("Cc", "control"),
    ("Cf", "format"),
    ("Cs", "surrogate"),
    ("Co", "private use"),
    ("Cn", "unassigned"),
];

const CODE_POINTS: usize = 0x11_0000;
const UNASSIGNED: u8 = 29;

fn category_index(name: &str) -> Result<u8, String> {
    CATEGORIES
        .iter()
        .position(|(c, _)| *c == name)
        .map(|i| i as u8)
        .ok_or_else(|| format!("unknown general category '{name}'"))
}

fn code_point(hex: &str) -> Result<usize, String> {
    let cp = usize::from_str_radix(hex.trim(), 16)
        .map_err(|e| format!("bad code point '{hex}': {e}"))?;
    if cp >= CODE_POINTS {
        return Err(format!("code point {cp:X} out of range"));
    }
    Ok(cp)
}

/// The Unicode version named on the first line of `DerivedGeneralCategory.txt`
/// (`# DerivedGeneralCategory-18.0.0.txt`).
fn ucd_version(derived: &str) -> Result<String, String> {
    let first = derived.lines().next().unwrap_or("");
    first
        .trim()
        .strip_prefix("# DerivedGeneralCategory-")
        .and_then(|rest| rest.strip_suffix(".txt"))
        .map(str::to_owned)
        .ok_or_else(|| format!("no version on the first line: '{first}'"))
}

/// General categories from `DerivedGeneralCategory.txt`. Every code point it
/// does not list is `Cn`; a code point listed twice is an error.
fn read_derived(derived: &str) -> Result<Vec<u8>, String> {
    let mut cats = vec![UNASSIGNED; CODE_POINTS];
    let mut seen = vec![false; CODE_POINTS];
    for line in derived.lines() {
        let data = line.split('#').next().unwrap_or("").trim();
        if data.is_empty() {
            continue;
        }
        let (range, cat) = data
            .split_once(';')
            .ok_or_else(|| format!("bad line '{line}'"))?;
        let cat = category_index(cat.trim())?;
        let (first, last) = match range.trim().split_once("..") {
            Some((a, b)) => (code_point(a)?, code_point(b)?),
            None => (code_point(range)?, code_point(range)?),
        };
        if first > last {
            return Err(format!("reversed range in '{line}'"));
        }
        for cp in first..=last {
            if seen[cp] {
                return Err(format!("{cp:04X} listed twice"));
            }
            seen[cp] = true;
            cats[cp] = cat;
        }
    }
    Ok(cats)
}

/// General categories from `UnicodeData.txt`, its `<…, First>`/`<…, Last>`
/// pairs expanded. Every code point it does not list is `Cn`.
fn read_unicode_data(data: &str) -> Result<Vec<u8>, String> {
    let mut cats = vec![UNASSIGNED; CODE_POINTS];
    let mut open: Option<(usize, u8)> = None;
    for line in data.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(';').collect();
        if fields.len() < 3 {
            return Err(format!("bad line '{line}'"));
        }
        let cp = code_point(fields[0])?;
        let cat = category_index(fields[2])?;
        if fields[1].ends_with(", First>") {
            open = Some((cp, cat));
            continue;
        }
        if fields[1].ends_with(", Last>") {
            let (first, first_cat) = open.take().ok_or_else(|| format!("unpaired '{line}'"))?;
            if first_cat != cat || first > cp {
                return Err(format!("bad range ending at '{line}'"));
            }
            cats[first..=cp].fill(cat);
            continue;
        }
        cats[cp] = cat;
    }
    if open.is_some() {
        return Err("unterminated First> range".into());
    }
    Ok(cats)
}

/// The text of `html` with tags removed, `&nbsp;` read as a space and every
/// run of white space collapsed to one space.
fn strip_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            _ => text.push(ch),
        }
    }
    let text = text.replace("&nbsp;", " ").replace("&#160;", " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Production name to its inclusive ranges, in written order.
type Productions = BTreeMap<String, Vec<(u32, u32)>>;

/// The productions `[85]` to `[89]` of XML 1.0 Appendix B, by name, each as
/// its list of inclusive ranges in written order.
fn read_appendix_b(html: &str) -> Result<(String, Productions), String> {
    let title_start = html.find("<title>").ok_or("no <title>")? + "<title>".len();
    let title_end = html[title_start..].find("</title>").ok_or("no </title>")? + title_start;
    let title = html[title_start..title_end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !title.contains("(XML) 1.0 (Second Edition)") {
        return Err(format!("not XML 1.0 (Second Edition): '{title}'"));
    }
    let anchor = "name=\"CharClasses\"";
    if html.matches(anchor).count() != 1 {
        return Err("expected one Appendix B anchor".into());
    }
    let start = html.find(anchor).ok_or("no Appendix B")?;
    let text = strip_tags(&html[start..]);
    let end = text
        .find("The character classes defined here")
        .ok_or("end of the Appendix B productions not found")?;
    let text = &text[..end];
    let names = [
        "BaseChar",
        "Ideographic",
        "CombiningChar",
        "Digit",
        "Extender",
    ];
    let mut productions = BTreeMap::new();
    for (i, name) in names.iter().enumerate() {
        let marker = format!("[{}] {} ::=", 85 + i, name);
        let begin = text
            .find(&marker)
            .ok_or_else(|| format!("production '{marker}' not found"))?;
        let body_start = begin + marker.len();
        let body_end = match names.get(i + 1) {
            Some(_) => text[body_start..]
                .find(&format!("[{}]", 86 + i))
                .map(|p| body_start + p)
                .ok_or_else(|| format!("end of '{name}' not found"))?,
            None => text.len(),
        };
        productions.insert((*name).to_owned(), hex_ranges(&text[body_start..body_end])?);
    }
    Ok((title, productions))
}

/// Every `#xHHHH` and `[#xHHHH-#xHHHH]` of a production body, in order.
fn hex_ranges(body: &str) -> Result<Vec<(u32, u32)>, String> {
    let hex_at = |s: &str| -> (u32, usize) {
        let digits: String = s.chars().take_while(char::is_ascii_hexdigit).collect();
        (
            u32::from_str_radix(&digits, 16).unwrap_or(u32::MAX),
            digits.len(),
        )
    };
    let mut ranges = Vec::new();
    let mut rest = body;
    while let Some(p) = rest.find("#x") {
        let (first, len) = hex_at(&rest[p + 2..]);
        if len == 0 || first == u32::MAX {
            return Err(format!("bad hex near '{}'", &rest[p..]));
        }
        rest = &rest[p + 2 + len..];
        let last = if let Some(after) = rest.strip_prefix("-#x") {
            let (last, len) = hex_at(after);
            if len == 0 || last < first {
                return Err(format!("bad range near '{rest}'"));
            }
            rest = &after[len..];
            last
        } else {
            first
        };
        ranges.push((first, last));
    }
    Ok(ranges)
}

/// The union of `ranges`, sorted, with overlapping and adjacent ranges merged.
fn merge(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (first, last) in ranges {
        match merged.last_mut() {
            Some(prev) if first <= prev.1.saturating_add(1) => prev.1 = prev.1.max(last),
            _ => merged.push((first, last)),
        }
    }
    merged
}

fn write_ranges(out: &mut String, name: &str, doc: &str, ranges: &[(u32, u32)]) {
    let _ = writeln!(out, "{doc}");
    out.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(
        out,
        "pub(crate) static {name}: [(u32, u32); {}] = [",
        ranges.len()
    );
    for (first, last) in ranges {
        let _ = writeln!(out, "    (0x{first:04X}, 0x{last:04X}),");
    }
    out.push_str("];\n\n");
    // Page index over the BMP (every range lies in it): entry `p` counts the
    // ranges that end before page `p`; entry 256 counts them all.
    let pages: Vec<usize> = (0u32..=256)
        .map(|page| ranges.partition_point(|r| r.1 < page << 8))
        .collect();
    let base = name.strip_suffix("_RANGES").unwrap_or(name);
    let _ = writeln!(
        out,
        "/// For each page of 256 code points of the BMP, the number of ranges of\n\
         /// [`{name}`] that end before the page; entry 256 is their count. The ranges\n\
         /// that meet page `p` are those from entry `p` to entry `p + 1`, inclusive."
    );
    out.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(out, "pub(crate) static {base}_PAGES: [u16; 257] = [");
    for row in pages.chunks(16) {
        let cells: Vec<String> = row.iter().map(|i| i.to_string()).collect();
        let _ = writeln!(out, "    {},", cells.join(", "));
    }
    out.push_str("];\n");
}

fn generate(unicode_data: &str, derived: &str, xml_html: &str) -> Result<String, String> {
    let version = ucd_version(derived)?;
    let cats = read_derived(derived)?;
    let check = read_unicode_data(unicode_data)?;
    if let Some(cp) = (0..CODE_POINTS).find(|&cp| cats[cp] != check[cp]) {
        return Err(format!(
            "{cp:04X}: DerivedGeneralCategory.txt says {}, UnicodeData.txt says {}",
            CATEGORIES[cats[cp] as usize].0, CATEGORIES[check[cp] as usize].0
        ));
    }
    let (title, productions) = read_appendix_b(xml_html)?;
    let counts: Vec<String> = productions
        .iter()
        .map(|(n, r)| format!("{n} {}", r.len()))
        .collect();
    eprintln!(
        "UCD {version}; {title}; Appendix B items: {}",
        counts.join(", ")
    );

    let production = |name: &str| productions[name].clone();
    let mut start = production("BaseChar");
    start.extend(production("Ideographic"));
    start.push((u32::from('_'), u32::from('_')));
    start.push((u32::from(':'), u32::from(':')));
    let mut name = start.clone();
    name.extend(production("Digit"));
    name.extend(production("CombiningChar"));
    name.extend(production("Extender"));
    name.push((u32::from('.'), u32::from('.')));
    name.push((u32::from('-'), u32::from('-')));
    let start = merge(start);
    let name = merge(name);
    if start.iter().chain(&name).any(|r| r.1 > 0xFFFF) {
        return Err("an XML 1.0 name range lies outside the BMP".into());
    }

    let mut runs: Vec<(usize, u8)> = Vec::new();
    for (cp, &cat) in cats.iter().enumerate().skip(0x80) {
        if runs.last().map(|r| r.1) != Some(cat) {
            runs.push((cp, cat));
        }
    }

    let mut out = String::new();
    let _ = write!(
        out,
        "//! Character tables for the XSD regular expression engine ([`crate::xsd_regex`]).
//!
//! Generated by `examples/gen_unicode_tables.rs`; do not edit by hand. Sources:
//! - the general categories of the Unicode Character Database, version {version}
//!   (`UnicodeData.txt` and `extracted/DerivedGeneralCategory.txt`), behind
//!   `\\p{{..}}`, `\\P{{..}}`, `\\d`, `\\D`, `\\w` and `\\W`;
//! - Appendix B, \"Character Classes\", of XML 1.0 (Second Edition), the edition
//!   XSD 1.0 (Second Edition) references, behind `\\i`, `\\I`, `\\c` and `\\C`.
//!
//! The data derived from the Unicode Character Database {version} (`UnicodeData.txt`,
//! `extracted/DerivedGeneralCategory.txt`) is Copyright Unicode, Inc., used under the
//! Unicode License v3, see LICENSE-UNICODE.

/// A Unicode general category. `Cs` is listed for completeness; no `char`
/// has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneralCategory {{
"
    );
    for (code, meaning) in CATEGORIES {
        let _ = writeln!(out, "    /// `{code}`, {meaning}.");
        let _ = writeln!(out, "    {code},");
    }
    out.push_str("}\n\nuse GeneralCategory::*;\n\n");
    out.push_str("/// The general category of each ASCII character, indexed by code point.\n");
    out.push_str("#[rustfmt::skip]\n");
    out.push_str("pub(crate) static ASCII_CATEGORIES: [GeneralCategory; 128] = [\n");
    for row in 0..16 {
        let cells: Vec<&str> = (0..8)
            .map(|i| CATEGORIES[cats[row * 8 + i] as usize].0)
            .collect();
        let _ = writeln!(out, "    {}, // 0x{:02X}", cells.join(", "), row * 8);
    }
    out.push_str("];\n\n");
    let _ = writeln!(
        out,
        "/// The general categories from U+0080 up: each entry is the first code point\n\
         /// of a run and the category of the whole run, which ends where the next\n\
         /// entry starts (the last one at U+10FFFF). Sorted by code point."
    );
    out.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(
        out,
        "pub(crate) static CATEGORY_RUNS: [(u32, GeneralCategory); {}] = [",
        runs.len()
    );
    for (cp, cat) in &runs {
        let _ = writeln!(out, "    (0x{cp:04X}, {}),", CATEGORIES[*cat as usize].0);
    }
    out.push_str("];\n\n");

    // For each 256-code-point page, the index of the run holding its first
    // code point (U+0080 for page 0); one more entry closes the last page.
    let pages = CODE_POINTS >> 8;
    let page_runs: Vec<usize> = (0..=pages)
        .map(|page| {
            let first = (page << 8).clamp(0x80, CODE_POINTS - 1);
            runs.partition_point(|r| r.0 <= first) - 1
        })
        .collect();
    if runs.len() > usize::from(u16::MAX) {
        return Err("too many runs for a u16 page index".into());
    }
    let _ = writeln!(
        out,
        "/// For each page of 256 code points, the index in [`CATEGORY_RUNS`] of the\n\
         /// run holding the page's first code point (U+0080 for page 0). The runs of a\n\
         /// page lie between its entry and the next one, inclusive."
    );
    out.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(
        out,
        "pub(crate) static CATEGORY_PAGES: [u16; {}] = [",
        page_runs.len()
    );
    for row in page_runs.chunks(16) {
        let cells: Vec<String> = row.iter().map(|i| i.to_string()).collect();
        let _ = writeln!(out, "    {},", cells.join(", "));
    }
    out.push_str("];\n\n");
    write_ranges(
        &mut out,
        "XML_NAME_START_RANGES",
        "/// `\\i`: XML 1.0 (Second Edition) `Letter | '_' | ':'`, where `Letter` is\n\
         /// `BaseChar | Ideographic`. Sorted, disjoint, inclusive ranges.",
        &start,
    );
    out.push('\n');
    write_ranges(
        &mut out,
        "XML_NAME_RANGES",
        "/// `\\c`: XML 1.0 (Second Edition) `NameChar`, that is `Letter | Digit | '.' |\n\
         /// '-' | '_' | ':' | CombiningChar | Extender`. Sorted, disjoint, inclusive\n\
         /// ranges.",
        &name,
    );
    Ok(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!(
            "usage: gen_unicode_tables <UnicodeData.txt> <DerivedGeneralCategory.txt> \
             <xml-1.0-second-edition.html> <output.rs>"
        );
        return ExitCode::FAILURE;
    }
    let read = |path: &str| fs::read_to_string(path).map_err(|e| format!("{path}: {e}"));
    let result = (|| -> Result<(), String> {
        let out = generate(&read(&args[1])?, &read(&args[2])?, &read(&args[3])?)?;
        fs::write(&args[4], out).map_err(|e| format!("{}: {e}", args[4]))
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gen_unicode_tables: {e}");
            ExitCode::FAILURE
        }
    }
}
