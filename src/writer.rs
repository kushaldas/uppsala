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
        self.buf.push_str(version);
        self.buf.push('"');
        if let Some(enc) = encoding {
            self.buf.push_str(" encoding=\"");
            self.buf.push_str(enc);
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
    /// The content is written verbatim (no escaping). It is the caller's
    /// responsibility to ensure the content does not contain `]]>`.
    pub fn cdata(&mut self, content: &str) {
        self.buf.push_str("<![CDATA[");
        self.buf.push_str(content);
        self.buf.push_str("]]>");
    }

    /// Write a comment: `<!--content-->`.
    ///
    /// The content is written verbatim. It is the caller's responsibility
    /// to ensure the content does not contain `--`.
    pub fn comment(&mut self, content: &str) {
        self.buf.push_str("<!--");
        self.buf.push_str(content);
        self.buf.push_str("-->");
    }

    /// Write a processing instruction: `<?target data?>` or `<?target?>`.
    pub fn processing_instruction(&mut self, target: &str, data: Option<&str>) {
        self.buf.push_str("<?");
        self.buf.push_str(target);
        if let Some(d) = data {
            self.buf.push(' ');
            self.buf.push_str(d);
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
