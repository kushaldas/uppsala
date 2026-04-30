//! Imperative XML writer for constructing XML documents and fragments.
//!
//! [`XmlWriter`] provides a push-based API for building XML output without
//! needing to construct a full DOM tree first. This is useful for generating
//! XML fragments, building templates, or any scenario where streaming
//! construction is more natural than tree manipulation.
//!
//! # Example
//!
//! ```
//! use uppsala::XmlWriter;
//!
//! let mut w = XmlWriter::new();
//! w.write_declaration();
//! w.start_element("root", &[("xmlns", "http://example.com")]);
//! w.start_element("child", &[("id", "1")]);
//! w.text("Hello, world!");
//! w.end_element("child");
//! w.empty_element("empty", &[]);
//! w.end_element("root");
//!
//! let xml = w.into_string();
//! assert!(xml.starts_with("<?xml"));
//! ```

use std::borrow::Cow;

/// An imperative XML writer that builds output incrementally.
///
/// Content is written to an internal buffer. Text and attribute values are
/// automatically escaped. Use [`raw`](XmlWriter::raw) to inject pre-escaped
/// content.
pub struct XmlWriter {
    buf: String,
}

impl XmlWriter {
    /// Create a new empty XML writer.
    pub fn new() -> Self {
        XmlWriter { buf: String::new() }
    }

    /// Create a new XML writer with a pre-allocated buffer capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        XmlWriter {
            buf: String::with_capacity(capacity),
        }
    }

    /// Write the XML declaration: `<?xml version="1.0" encoding="UTF-8"?>`.
    pub fn write_declaration(&mut self) {
        self.buf
            .push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    }

    /// Write a custom XML declaration with the specified version, optional encoding,
    /// and optional standalone flag.
    pub fn write_declaration_full(
        &mut self,
        version: &str,
        encoding: Option<&str>,
        standalone: Option<bool>,
    ) {
        self.buf.push_str("<?xml version=\"");
        self.buf.push_str(&safe_xml_version(version));
        self.buf.push('"');
        if let Some(enc) = encoding {
            self.buf.push_str(" encoding=\"");
            self.buf.push_str(&safe_xml_encoding(enc));
            self.buf.push('"');
        }
        if let Some(sa) = standalone {
            self.buf.push_str(" standalone=\"");
            self.buf.push_str(if sa { "yes" } else { "no" });
            self.buf.push('"');
        }
        self.buf.push_str("?>");
    }

    /// Open an element with the given name and attributes.
    ///
    /// Attributes are written as `key="escaped_value"`. You must call
    /// [`end_element`](XmlWriter::end_element) with the same name to close it.
    ///
    /// # Example
    ///
    /// ```
    /// use uppsala::XmlWriter;
    ///
    /// let mut w = XmlWriter::new();
    /// w.start_element("div", &[("class", "main"), ("id", "content")]);
    /// w.text("Hello");
    /// w.end_element("div");
    /// assert_eq!(w.into_string(), r#"<div class="main" id="content">Hello</div>"#);
    /// ```
    pub fn start_element(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.buf.push('<');
        self.buf.push_str(name);
        for &(key, val) in attrs {
            self.buf.push(' ');
            self.buf.push_str(key);
            self.buf.push_str("=\"");
            write_escaped_attr_to_string(&mut self.buf, val);
            self.buf.push('"');
        }
        self.buf.push('>');
    }

    /// Write a self-closing empty element: `<name attr="val"/>`.
    ///
    /// # Example
    ///
    /// ```
    /// use uppsala::XmlWriter;
    ///
    /// let mut w = XmlWriter::new();
    /// w.empty_element("br", &[]);
    /// assert_eq!(w.into_string(), "<br/>");
    /// ```
    pub fn empty_element(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.buf.push('<');
        self.buf.push_str(name);
        for &(key, val) in attrs {
            self.buf.push(' ');
            self.buf.push_str(key);
            self.buf.push_str("=\"");
            write_escaped_attr_to_string(&mut self.buf, val);
            self.buf.push('"');
        }
        self.buf.push_str("/>");
    }

    /// Open an element with attributes whose values implement `AsRef<str>`.
    ///
    /// This is a more flexible version of [`start_element`](Self::start_element)
    /// that accepts owned `String` values directly, avoiding the need to build
    /// a temporary `Vec<(&str, &str)>` when some values are computed.
    ///
    /// # Example
    ///
    /// ```
    /// use uppsala::XmlWriter;
    ///
    /// let mut w = XmlWriter::new();
    /// let count = 42.to_string();
    /// w.start_element_with("item", [("id", count.as_str()), ("type", "fixed")]);
    /// w.end_element("item");
    /// assert_eq!(w.into_string(), r#"<item id="42" type="fixed"></item>"#);
    /// ```
    pub fn start_element_with<I, K, V>(&mut self, name: &str, attrs: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.buf.push('<');
        self.buf.push_str(name);
        for (key, val) in attrs {
            self.buf.push(' ');
            self.buf.push_str(key.as_ref());
            self.buf.push_str("=\"");
            write_escaped_attr_to_string(&mut self.buf, val.as_ref());
            self.buf.push('"');
        }
        self.buf.push('>');
    }

    /// Write a self-closing empty element with generic attribute values.
    ///
    /// This is a more flexible version of [`empty_element`](Self::empty_element)
    /// that accepts owned `String` values directly.
    ///
    /// # Example
    ///
    /// ```
    /// use uppsala::XmlWriter;
    ///
    /// let mut w = XmlWriter::new();
    /// let id = 7.to_string();
    /// w.empty_element_with("br", [("id", id.as_str())]);
    /// assert_eq!(w.into_string(), r#"<br id="7"/>"#);
    /// ```
    pub fn empty_element_with<I, K, V>(&mut self, name: &str, attrs: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.buf.push('<');
        self.buf.push_str(name);
        for (key, val) in attrs {
            self.buf.push(' ');
            self.buf.push_str(key.as_ref());
            self.buf.push_str("=\"");
            write_escaped_attr_to_string(&mut self.buf, val.as_ref());
            self.buf.push('"');
        }
        self.buf.push_str("/>");
    }

    /// Write an expanded empty element: `<name attr="val"></name>`.
    ///
    /// This is the form required by W3C Canonical XML (C14N).
    pub fn empty_element_expanded(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.buf.push('<');
        self.buf.push_str(name);
        for &(key, val) in attrs {
            self.buf.push(' ');
            self.buf.push_str(key);
            self.buf.push_str("=\"");
            write_escaped_attr_to_string(&mut self.buf, val);
            self.buf.push('"');
        }
        self.buf.push_str("></");
        self.buf.push_str(name);
        self.buf.push('>');
    }

    /// Close an element: `</name>`.
    pub fn end_element(&mut self, name: &str) {
        self.buf.push_str("</");
        self.buf.push_str(name);
        self.buf.push('>');
    }

    /// Write escaped text content.
    ///
    /// Special characters (`&`, `<`, `>`, `\r`) are automatically escaped.
    pub fn text(&mut self, content: &str) {
        write_escaped_text_to_string(&mut self.buf, content);
    }

    /// Write a CDATA section: `<![CDATA[content]]>`.
    ///
    /// If `content` contains the CDATA terminator `]]>` it is split across
    /// multiple CDATA sections per the standard workaround, so the emitted
    /// text reparses to exactly the input. Callers do not need to
    /// pre-validate content.
    pub fn cdata(&mut self, content: &str) {
        self.buf.push_str("<![CDATA[");
        self.buf.push_str(&split_cdata_content(content));
        self.buf.push_str("]]>");
    }

    /// Write a comment: `<!--content-->`.
    ///
    /// Sequences of `-` characters are automatically separated by spaces and
    /// a trailing `-` is padded, so a comment with any content remains
    /// well-formed (comments must not contain `--` or end with `-` per
    /// XML 1.0 section 2.5). The sanitized output round-trips to a single
    /// well-formed comment rather than terminating early and smuggling
    /// markup.
    pub fn comment(&mut self, content: &str) {
        self.buf.push_str("<!--");
        self.buf.push_str(&sanitize_comment_content(content));
        self.buf.push_str("-->");
    }

    /// Write a processing instruction: `<?target data?>` or `<?target?>`.
    ///
    /// Two sanitizations are applied. A `target` that case-insensitively
    /// matches the reserved name `xml` is renamed to `_xml` so the emitted
    /// PI cannot be confused with an XML declaration on reparse. If `data`
    /// contains the PI terminator `?>`, a space is inserted between the
    /// two characters so the PI does not terminate early.
    pub fn processing_instruction(&mut self, target: &str, data: Option<&str>) {
        self.buf.push_str("<?");
        self.buf.push_str(&sanitize_pi_target(target));
        if let Some(d) = data {
            self.buf.push(' ');
            self.buf.push_str(&sanitize_pi_data(d));
        }
        self.buf.push_str("?>");
    }

    /// Inject raw, pre-escaped XML content.
    ///
    /// No escaping is performed. Use this when you have XML content that is
    /// already properly escaped or when embedding pre-built fragments.
    pub fn raw(&mut self, xml: &str) {
        self.buf.push_str(xml);
    }

    /// Get a reference to the current output.
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// Consume the writer and return the output string.
    pub fn into_string(self) -> String {
        self.buf
    }

    /// Consume the writer and return the output as bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf.into_bytes()
    }

    /// Returns the current length of the output in bytes.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Returns true if no output has been written.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Default for XmlWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for XmlWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.buf)
    }
}

// ─── Structural-markup sanitizers (F-13 / F-14 / F-15) ─────────────────────
//
// These three functions prevent "round-trip injection" attacks where an
// attacker-controlled comment, PI, or CDATA body contains the section's
// own terminator (`-->`, `?>`, `]]>`) and thereby smuggles arbitrary XML
// into the emitted output. Each returns `Cow::Borrowed` when the input is
// already safe (the common case) and `Cow::Owned` only when sanitization
// is needed. Shared between `XmlWriter` and the DOM serializer so both
// entry points close the same hole.

/// Sanitize comment content so it cannot contain `--` or end with `-`,
/// both of which would break XML 1.0 comment well-formedness (and in the
/// adversarial case let the comment terminate early and smuggle markup).
///
/// Consecutive `-` characters are separated by a space; a trailing `-`
/// gets a trailing space. The transform is reversible *semantically* (the
/// intent of the text is preserved; a human reading the comment sees the
/// same words) but byte-inequivalent.
pub(crate) fn sanitize_comment_content(s: &str) -> Cow<'_, str> {
    if !s.contains("--") && !s.ends_with('-') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev_was_dash = false;
    for c in s.chars() {
        if c == '-' && prev_was_dash {
            out.push(' ');
        }
        out.push(c);
        prev_was_dash = c == '-';
    }
    if out.ends_with('-') {
        out.push(' ');
    }
    Cow::Owned(out)
}

/// Sanitize PI data so it cannot contain the PI terminator `?>`. A space
/// is inserted between the `?` and `>` so the byte sequence no longer
/// matches the parser's terminator scan.
pub(crate) fn sanitize_pi_data(s: &str) -> Cow<'_, str> {
    if !s.contains("?>") {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("?>", "? >"))
}

/// Sanitize a PI target so it cannot collide with the reserved name
/// `xml` (case-insensitive per XML 1.0 section 2.6). Without this, a
/// programmatic `processing_instruction("xml", ...)` would emit bytes
/// syntactically indistinguishable from an XML declaration - and an
/// attacker who controls the target name could force a malformed or
/// reparse-rejected document. Renaming to `_xml` preserves the "this
/// is a PI" intent while making the output unambiguously a PI node.
pub(crate) fn sanitize_pi_target(s: &str) -> Cow<'_, str> {
    if s.eq_ignore_ascii_case("xml") {
        Cow::Owned(format!("_{}", s))
    } else {
        Cow::Borrowed(s)
    }
}

/// Split CDATA content at every occurrence of `]]>` using the standard
/// `]]]]><![CDATA[>` workaround. XML 1.0 forbids `]]>` inside a single
/// CDATA section, but two adjacent CDATA sections that each contain half
/// the sequence reparse to the original text.
///
/// Example: `"hello]]>world"` becomes `"hello]]]]><![CDATA[>world"`. When
/// the caller wraps that in `<![CDATA[ ... ]]>` the emitted document is
/// `<![CDATA[hello]]]]><![CDATA[>world]]>`, which reparses as two adjacent
/// CDATA sections concatenating to `"hello]]>world"`.
pub(crate) fn split_cdata_content(s: &str) -> Cow<'_, str> {
    if !s.contains("]]>") {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("]]>", "]]]]><![CDATA[>"))
}

/// Return `s` if it matches the XML 1.0 `VersionNum` production
/// (`'1.' [0-9]+`); otherwise return a safe fallback `"1.0"`.
///
/// Without this, an attacker who can mutate `Document::xml_declaration`
/// or pass an attacker-controlled string to
/// [`XmlWriter::write_declaration_full`] can close the enclosing
/// `<?xml ... ?>` early with a `"?>` byte pair and smuggle arbitrary
/// markup ahead of the root element. The same smuggle class the
/// comment / PI / CDATA sanitizers above close for those node kinds.
pub(crate) fn safe_xml_version(s: &str) -> Cow<'_, str> {
    if is_valid_xml_version(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Borrowed("1.0")
    }
}

/// XML 1.0 §2.8 `VersionNum ::= '1.' [0-9]+`.
fn is_valid_xml_version(s: &str) -> bool {
    let rest = match s.strip_prefix("1.") {
        Some(r) => r,
        None => return false,
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// Return `s` if it matches the XML 1.0 `EncName` production
/// (`[A-Za-z] ([A-Za-z0-9._] | '-')*`); otherwise return a safe
/// fallback `"UTF-8"`. Same threat model and rationale as
/// [`safe_xml_version`].
pub(crate) fn safe_xml_encoding(s: &str) -> Cow<'_, str> {
    if is_valid_xml_encoding(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Borrowed("UTF-8")
    }
}

/// XML 1.0 §4.3.3 `EncName ::= [A-Za-z] ([A-Za-z0-9._] | '-')*`.
fn is_valid_xml_encoding(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

// ─── Internal escaping helpers (write directly to String, no allocation) ───

/// Write text content with XML escaping directly to a String.
fn write_escaped_text_to_string(buf: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            '\r' => buf.push_str("&#xD;"),
            _ => buf.push(c),
        }
    }
}

/// Write attribute value with XML escaping directly to a String.
fn write_escaped_attr_to_string(buf: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            '"' => buf.push_str("&quot;"),
            '\t' => buf.push_str("&#x9;"),
            '\n' => buf.push_str("&#xA;"),
            '\r' => buf.push_str("&#xD;"),
            _ => buf.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Pure-function tests for the sanitizers ────────────────────────

    #[test]
    fn sanitize_comment_passes_safe_content() {
        assert!(matches!(
            sanitize_comment_content("hello world"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(sanitize_comment_content(""), Cow::Borrowed(_)));
        assert!(matches!(
            sanitize_comment_content("single - dash"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn sanitize_comment_separates_consecutive_dashes() {
        assert_eq!(&*sanitize_comment_content("a--b"), "a- -b");
        assert_eq!(&*sanitize_comment_content("a---b"), "a- - -b");
        // `"--"` ends with `-` after separator insertion, so the
        // trailing-dash fixup also kicks in.
        assert_eq!(&*sanitize_comment_content("--"), "- - ");
        assert_eq!(&*sanitize_comment_content("-->"), "- ->");
    }

    #[test]
    fn sanitize_comment_pads_trailing_dash() {
        assert_eq!(&*sanitize_comment_content("foo-"), "foo- ");
        assert_eq!(&*sanitize_comment_content("-"), "- ");
        assert_eq!(&*sanitize_comment_content("a--"), "a- - ");
    }

    #[test]
    fn sanitize_pi_data_inserts_space_in_terminator() {
        assert!(matches!(sanitize_pi_data("safe data"), Cow::Borrowed(_)));
        assert_eq!(&*sanitize_pi_data("a?>b"), "a? >b");
        assert_eq!(&*sanitize_pi_data("?>?>"), "? >? >");
        assert_eq!(&*sanitize_pi_data(""), "");
    }

    #[test]
    fn sanitize_pi_target_renames_reserved_xml() {
        // Reserved name (case-insensitive) is renamed.
        assert_eq!(&*sanitize_pi_target("xml"), "_xml");
        assert_eq!(&*sanitize_pi_target("XML"), "_XML");
        assert_eq!(&*sanitize_pi_target("Xml"), "_Xml");
        assert_eq!(&*sanitize_pi_target("xMl"), "_xMl");
    }

    #[test]
    fn sanitize_pi_target_passes_legitimate_names() {
        // Any other name is Borrowed-through.
        assert!(matches!(sanitize_pi_target("xsl"), Cow::Borrowed(_)));
        assert!(matches!(
            sanitize_pi_target("xml-stylesheet"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(sanitize_pi_target("xmlrpc"), Cow::Borrowed(_)));
        assert!(matches!(sanitize_pi_target(""), Cow::Borrowed(_)));
    }

    #[test]
    fn split_cdata_preserves_safe_content() {
        assert!(matches!(
            split_cdata_content("hello world"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(split_cdata_content(""), Cow::Borrowed(_)));
    }

    #[test]
    fn split_cdata_splits_terminator() {
        assert_eq!(
            &*split_cdata_content("hello]]>world"),
            "hello]]]]><![CDATA[>world"
        );
        assert_eq!(&*split_cdata_content("]]>"), "]]]]><![CDATA[>");
    }

    // ─── Round-trip smuggle-prevention tests (F-13 / F-14 / F-15) ──────

    #[test]
    fn roundtrip_comment_smuggle_is_blocked() {
        // Attacker-controlled comment text tries to close the comment
        // early and inject a sibling element.
        let mut w = XmlWriter::new();
        w.start_element("r", &[]);
        w.comment("safe --> <injected/> <!--trailing");
        w.end_element("r");
        let out = w.into_string();

        // The emitted XML must reparse without any injected element
        // becoming a sibling of <r>.
        let doc = crate::parse(&out).expect("sanitized output must reparse");
        let root = doc.document_element().unwrap();
        let element_children: Vec<_> = doc
            .children(root)
            .into_iter()
            .filter(|c| matches!(doc.node_kind(*c), Some(crate::NodeKind::Element(_))))
            .collect();
        assert!(
            element_children.is_empty(),
            "comment sanitization failed; output smuggled an element: {:?}",
            out
        );
    }

    #[test]
    fn roundtrip_pi_smuggle_is_blocked() {
        let mut w = XmlWriter::new();
        w.start_element("r", &[]);
        w.processing_instruction("x", Some("?><injected/>"));
        w.end_element("r");
        let out = w.into_string();

        let doc = crate::parse(&out).expect("sanitized output must reparse");
        let root = doc.document_element().unwrap();
        let element_children: Vec<_> = doc
            .children(root)
            .into_iter()
            .filter(|c| matches!(doc.node_kind(*c), Some(crate::NodeKind::Element(_))))
            .collect();
        assert!(
            element_children.is_empty(),
            "PI sanitization failed; output smuggled an element: {:?}",
            out
        );
    }

    #[test]
    fn roundtrip_pi_reserved_xml_target_is_renamed() {
        // Attacker constructs a PI with the reserved `xml` target, hoping
        // to either forge an XML declaration (start-of-document) or reach
        // a parser-rejection DoS (elsewhere). Sanitization renames the
        // target to `_xml`, and the document reparses as a well-formed
        // <r> with one ordinary PI child.
        let mut w = XmlWriter::new();
        w.start_element("r", &[]);
        w.processing_instruction("xml", Some("version=\"1.0\" standalone=\"yes\""));
        w.end_element("r");
        let out = w.into_string();

        assert!(
            !out.contains("<?xml "),
            "reserved `xml` target must not reach the output: {:?}",
            out
        );
        let doc = crate::parse(&out).expect("sanitized output must reparse");
        let root = doc.document_element().unwrap();
        let pi_children: Vec<_> = doc
            .children(root)
            .into_iter()
            .filter_map(|c| match doc.node_kind(c) {
                Some(crate::NodeKind::ProcessingInstruction(pi)) => Some(pi),
                _ => None,
            })
            .collect();
        assert_eq!(pi_children.len(), 1, "expected exactly one PI child");
        assert_eq!(&*pi_children[0].target, "_xml");
    }

    // ─── XML-declaration version/encoding validation (M-1) ────────────

    #[test]
    fn safe_xml_version_passes_valid() {
        assert!(matches!(safe_xml_version("1.0"), Cow::Borrowed(_)));
        assert!(matches!(safe_xml_version("1.1"), Cow::Borrowed(_)));
        assert!(matches!(safe_xml_version("1.10"), Cow::Borrowed(_)));
        assert_eq!(&*safe_xml_version("1.0"), "1.0");
        assert_eq!(&*safe_xml_version("1.42"), "1.42");
    }

    #[test]
    fn safe_xml_version_rejects_invalid() {
        // Empty, wrong major, missing minor, trailing garbage, injection.
        assert_eq!(&*safe_xml_version(""), "1.0");
        assert_eq!(&*safe_xml_version("1"), "1.0");
        assert_eq!(&*safe_xml_version("1."), "1.0");
        assert_eq!(&*safe_xml_version("2.0"), "1.0");
        assert_eq!(&*safe_xml_version("1.0a"), "1.0");
        assert_eq!(&*safe_xml_version("1.0\"?><x/><?y "), "1.0");
        assert_eq!(&*safe_xml_version("1.0 "), "1.0");
    }

    #[test]
    fn safe_xml_encoding_passes_valid() {
        assert!(matches!(safe_xml_encoding("UTF-8"), Cow::Borrowed(_)));
        assert!(matches!(safe_xml_encoding("utf-8"), Cow::Borrowed(_)));
        assert!(matches!(safe_xml_encoding("ISO-8859-1"), Cow::Borrowed(_)));
        assert!(matches!(safe_xml_encoding("US_ASCII.1"), Cow::Borrowed(_)));
        assert_eq!(&*safe_xml_encoding("UTF-8"), "UTF-8");
    }

    #[test]
    fn safe_xml_encoding_rejects_invalid() {
        // Empty, digit-first, leading dash, injection, control chars.
        assert_eq!(&*safe_xml_encoding(""), "UTF-8");
        assert_eq!(&*safe_xml_encoding("1UTF"), "UTF-8");
        assert_eq!(&*safe_xml_encoding("-foo"), "UTF-8");
        assert_eq!(&*safe_xml_encoding("UTF-8\"?><x/>"), "UTF-8");
        assert_eq!(&*safe_xml_encoding("utf 8"), "UTF-8");
        assert_eq!(&*safe_xml_encoding("utf\x00"), "UTF-8");
    }

    #[test]
    fn roundtrip_xml_writer_declaration_version_injection_blocked() {
        // Attacker-controlled version string tries to close the
        // declaration early and inject a root-sibling PI.
        let mut w = XmlWriter::new();
        w.write_declaration_full("1.0\"?><!-- smuggled -->", Some("UTF-8"), None);
        w.start_element("r", &[]);
        w.end_element("r");
        let out = w.into_string();
        assert!(
            !out.contains("smuggled"),
            "attacker-controlled version must not reach output: {:?}",
            out
        );
        let doc = crate::parse(&out).expect("sanitized output must reparse");
        assert_eq!(doc.xml_declaration.as_ref().unwrap().version, "1.0");
    }

    #[test]
    fn roundtrip_xml_writer_declaration_encoding_injection_blocked() {
        let mut w = XmlWriter::new();
        w.write_declaration_full("1.0", Some("UTF-8\"?><inject/><?x "), None);
        w.start_element("r", &[]);
        w.end_element("r");
        let out = w.into_string();
        assert!(
            !out.contains("<inject"),
            "attacker-controlled encoding must not reach output: {:?}",
            out
        );
        let doc = crate::parse(&out).expect("sanitized output must reparse");
        // Root must still be <r/>, not the smuggled sibling.
        let root = doc.document_element().unwrap();
        match doc.node_kind(root) {
            Some(crate::NodeKind::Element(e)) => {
                assert_eq!(&*e.name.local_name, "r");
            }
            _ => panic!("expected element root"),
        }
        assert_eq!(
            doc.xml_declaration.as_ref().unwrap().encoding.as_deref(),
            Some("UTF-8")
        );
    }

    #[test]
    fn roundtrip_dom_declaration_version_injection_blocked() {
        // Same threat model, exercised through the DOM serializer path.
        let mut doc = crate::parse("<r/>").expect("parse");
        doc.xml_declaration = Some(crate::dom::XmlDeclaration {
            version: "1.0\"?><forged/><?y ".into(),
            encoding: Some("UTF-8".into()),
            standalone: None,
        });
        let out = doc.to_xml();
        assert!(
            !out.contains("<forged"),
            "DOM-mutation version injection not blocked: {:?}",
            out
        );
        let reparsed = crate::parse(&out).expect("sanitized output must reparse");
        assert_eq!(reparsed.xml_declaration.as_ref().unwrap().version, "1.0");
    }

    #[test]
    fn roundtrip_dom_declaration_encoding_injection_blocked() {
        let mut doc = crate::parse("<r/>").expect("parse");
        doc.xml_declaration = Some(crate::dom::XmlDeclaration {
            version: "1.0".into(),
            encoding: Some("UTF-8\"?><forged/><?y ".into()),
            standalone: None,
        });
        let out = doc.to_xml();
        assert!(
            !out.contains("<forged"),
            "DOM-mutation encoding injection not blocked: {:?}",
            out
        );
        let reparsed = crate::parse(&out).expect("sanitized output must reparse");
        assert_eq!(
            reparsed
                .xml_declaration
                .as_ref()
                .unwrap()
                .encoding
                .as_deref(),
            Some("UTF-8")
        );
    }

    #[test]
    fn roundtrip_cdata_smuggle_is_blocked() {
        let mut w = XmlWriter::new();
        w.start_element("r", &[]);
        w.cdata("safe]]><injected/>more");
        w.end_element("r");
        let out = w.into_string();

        let doc = crate::parse(&out).expect("split CDATA must reparse");
        let root = doc.document_element().unwrap();
        // Exactly one (concatenated) CDATA text child, no smuggled elements.
        let element_children: Vec<_> = doc
            .children(root)
            .into_iter()
            .filter(|c| matches!(doc.node_kind(*c), Some(crate::NodeKind::Element(_))))
            .collect();
        assert!(
            element_children.is_empty(),
            "CDATA split failed; output smuggled an element: {:?}",
            out
        );
        // And the semantic text content must round-trip unchanged.
        assert_eq!(
            doc.text_content_deep(root),
            "safe]]><injected/>more",
            "CDATA split must preserve the original text semantically"
        );
    }
}
