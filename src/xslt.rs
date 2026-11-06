//! XSLT 1.0 transformation engine.
//!
//! This is a pragmatic XSLT 1.0 implementation layered on top of uppsala's
//! existing XPath 1.0 evaluator (`crate::xpath`). The transformer reuses that
//! evaluator for `select` / `test` / AVT / pattern expressions and adds:
//!
//! - an XSLT stylesheet model parsed from an ordinary XML [`Document`],
//! - XSLT pattern matching for `match=` attributes,
//! - a template-dispatch engine with the built-in template rules,
//! - a result-tree representation with its own serializer that honors
//!   `disable-output-escaping` and the `xsl:output` `method`
//!   (`xml`/`text`), `encoding` (UTF-8 only), and `omit-xml-declaration`
//!   controls. `indent="yes"` is parsed but not yet applied (the serializer
//!   does not pretty-print); stylesheets that need indentation produce it
//!   themselves (e.g. pyFF's `pp.xsl`).
//!
//! The implemented subset targets pyFF's stylesheets ("Tier A"): see the crate
//! documentation for the exact feature list. Notably out of scope for now are
//! `xsl:key`, `xsl:sort`, `format-number`, `xsl:import`/`include`, attribute
//! sets, modes, and the `html` output method.
//!
//! # Examples
//!
//! ```
//! let xslt = r#"<xsl:stylesheet version="1.0"
//!     xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!   <xsl:output method="xml" omit-xml-declaration="yes"/>
//!   <xsl:template match="/">
//!     <out><xsl:value-of select="/greeting/@to"/></out>
//!   </xsl:template>
//! </xsl:stylesheet>"#;
//! let xml = r#"<greeting to="world">hi</greeting>"#;
//! let result = uppsala::transform(xslt, xml).unwrap();
//! assert_eq!(result, "<out>world</out>");
//! ```

use std::collections::HashMap;

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{XmlError, XmlResult};
use crate::xpath::{
    eval_compiled, CompiledPattern, CompiledXPath, FunctionResolver, PatternDispatch,
    VariableResolver, XPathValue, DEFAULT_MAX_XPATH_DEPTH, DEFAULT_MAX_XPATH_NODE_VISITS,
};

/// The XSLT namespace URI.
pub const XSLT_NAMESPACE: &str = "http://www.w3.org/1999/XSL/Transform";

/// Default maximum XSLT template-activation (recursion) depth.
///
/// XSLT lets a stylesheet recurse without bound — a self-recursive
/// `xsl:call-template`, mutually-recursive named templates, or an
/// `xsl:apply-templates select="."` cycle all drive the engine's native call
/// stack. Without a cap, a crafted (or buggy) stylesheet overflows the stack and
/// aborts the process (an uncatchable `SIGABRT`), so this bound converts runaway
/// recursion into a graceful [`XmlError`] instead.
///
/// The engine's per-activation stack cost is substantial (≈1 KB/level in release
/// builds, far more in debug). 500 is chosen conservatively so the default is
/// safe well within the standard 8 MiB main-thread stack *and* the 2 MiB stacks
/// common to async runtimes (tokio) in release builds, while still vastly
/// exceeding any realistic stylesheet: structural recursion over the source is
/// already bounded by the parser's nesting cap
/// ([`crate::parser::DEFAULT_MAX_DEPTH`] = 128), and real-world stylesheets
/// (e.g. pyFF's) recurse only a handful of levels even over large, wide inputs
/// such as 90 MB eduGAIN aggregates (which are shallow — depth ~7). Embedders
/// running on large stacks that genuinely need deeper recursion can raise it via
/// [`Stylesheet::set_max_depth`]. This mirrors the conservative depth caps used
/// by the parser and XPath layers.
pub const DEFAULT_MAX_XSLT_DEPTH: u32 = 500;

/// Conservative accounting overhead charged for each materialized result node.
///
/// The result-tree budget is intentionally approximate: it bounds amplification
/// work before serialization, not exact allocator bytes. Payload string lengths
/// dominate real attack cases, while a small fixed node charge also catches
/// empty-node amplification.
const RESULT_NODE_ACCOUNTING_BYTES: usize = 32;

/// Conservative accounting overhead charged for each materialized result
/// attribute or namespace declaration.
const RESULT_ATTR_ACCOUNTING_BYTES: usize = 16;

// ─── Result tree ──────────────────────────────────────────

/// A node in the transformation result tree.
///
/// This is deliberately separate from [`crate::dom`]'s `Document`: a result
/// text node can carry a `disable_escaping` flag (XSLT
/// `disable-output-escaping`), which a DOM text node cannot represent.
#[derive(Debug, Clone)]
enum ResultNode {
    Element(ResultElement),
    Text {
        value: String,
        disable_escaping: bool,
    },
    Comment(String),
    Pi {
        target: String,
        data: String,
    },
}

#[derive(Debug, Clone)]
struct ResultElement {
    /// Serialized qualified name, e.g. `md:Extensions` or `out`.
    qname: String,
    /// `xmlns`/`xmlns:*` declarations to emit on this element.
    ns_decls: Vec<(Option<String>, String)>,
    attrs: Vec<ResultAttr>,
    children: Vec<ResultNode>,
}

#[derive(Debug, Clone)]
struct ResultAttr {
    namespace_uri: Option<String>,
    qname: String,
    value: String,
}

/// Copied attributes retain their expanded names even when their source
/// ancestor's namespace declarations are absent from the result.
fn fixup_attribute_namespaces(
    element: &mut ResultElement,
    mut charge: impl FnMut(usize) -> XmlResult<()>,
) -> XmlResult<()> {
    let mut next_prefix = 0;
    for attr in &mut element.attrs {
        let Some(uri) = &attr.namespace_uri else {
            continue;
        };
        let (prefix, local) = attr.qname.split_once(':').unwrap_or(("", &attr.qname));
        if uri == "http://www.w3.org/XML/1998/namespace" {
            charge((4 + local.len()).saturating_sub(attr.qname.len()))?;
            attr.qname = format!("xml:{local}");
            continue;
        }
        let mut prefix = prefix.to_string();
        if prefix.is_empty()
            || element
                .ns_decls
                .iter()
                .any(|(p, u)| p.as_deref() == Some(&prefix) && u != uri)
        {
            // Reuse an existing binding before allocating a fresh prefix.
            if let Some((Some(existing), _)) = element
                .ns_decls
                .iter()
                .find(|(p, u)| p.is_some() && u == uri)
            {
                prefix = existing.clone();
            } else {
                loop {
                    let candidate = format!("ns{next_prefix}");
                    next_prefix += 1;
                    if !element
                        .ns_decls
                        .iter()
                        .any(|(p, _)| p.as_deref() == Some(&candidate))
                    {
                        prefix = candidate;
                        break;
                    }
                }
            }
            charge((prefix.len() + 1 + local.len()).saturating_sub(attr.qname.len()))?;
            attr.qname = format!("{prefix}:{local}");
        }
        if !element
            .ns_decls
            .iter()
            .any(|(p, u)| p.as_deref() == Some(&prefix) && u == uri)
        {
            element.ns_decls.push((Some(prefix), uri.clone()));
        }
    }
    Ok(())
}

/// An item produced by executing a sequence constructor. Attributes are kept
/// distinct from nodes so that, when building an element, attribute items
/// (`xsl:attribute`, `xsl:copy`/`copy-of` of an attribute) attach to the
/// enclosing element rather than becoming child content.
#[derive(Debug, Clone)]
enum ResultItem {
    Attr(ResultAttr),
    Node(ResultNode),
}

/// Split a flat item list into element attributes and child nodes. Attributes
/// appearing after child content are dropped (a recoverable XSLT error); Tier A
/// stylesheets always emit attributes first.
fn split_items(items: Vec<ResultItem>) -> (Vec<ResultAttr>, Vec<ResultNode>) {
    let mut attrs = Vec::new();
    let mut children = Vec::new();
    for item in items {
        match item {
            ResultItem::Attr(a) => {
                if children.is_empty() {
                    attrs.push(a);
                }
            }
            ResultItem::Node(n) => children.push(n),
        }
    }
    (attrs, children)
}

/// A value bound to an XSLT variable/parameter: either an XPath value or a
/// result-tree fragment (the body form of `xsl:variable`/`xsl:param`).
#[derive(Clone)]
enum VarValue {
    Value(XPathValue),
    /// Result-tree fragment. Tier A uses it only for string-value (`value-of`,
    /// XPath string coercion) and `copy-of`; it is not navigable as a node-set.
    Rtf(Vec<ResultNode>),
}

impl VarValue {
    /// Coerce to an [`XPathValue`] for use in XPath expressions. A fragment
    /// becomes its string value (concatenated descendant text).
    fn to_xpath(&self) -> XPathValue {
        match self {
            VarValue::Value(v) => v.clone(),
            VarValue::Rtf(nodes) => XPathValue::String(rtf_string_value(nodes)),
        }
    }
}

/// The string value of a result-tree fragment: the concatenation of all text
/// in document order (escaping is irrelevant to string value).
fn rtf_string_value(nodes: &[ResultNode]) -> String {
    let mut s = String::new();
    for n in nodes {
        collect_rtf_text(n, &mut s);
    }
    s
}

fn collect_rtf_text(node: &ResultNode, out: &mut String) {
    match node {
        ResultNode::Text { value, .. } => out.push_str(value),
        ResultNode::Element(e) => {
            for c in &e.children {
                collect_rtf_text(c, out);
            }
        }
        ResultNode::Comment(_) | ResultNode::Pi { .. } => {}
    }
}

fn accounted_attr_bytes(attr: &ResultAttr) -> usize {
    RESULT_ATTR_ACCOUNTING_BYTES
        .saturating_add(attr.qname.len())
        .saturating_add(attr.value.len())
        .saturating_add(attr.namespace_uri.as_ref().map_or(0, String::len))
}

fn accounted_ns_decls_bytes(ns_decls: &[(Option<String>, String)]) -> usize {
    ns_decls.iter().fold(0usize, |acc, (prefix, uri)| {
        acc.saturating_add(
            RESULT_ATTR_ACCOUNTING_BYTES
                .saturating_add(prefix.as_ref().map_or(0, String::len))
                .saturating_add(uri.len()),
        )
    })
}

fn accounted_node_tree_bytes(node: &ResultNode) -> usize {
    match node {
        ResultNode::Element(el) => {
            let attrs = el.attrs.iter().fold(0usize, |acc, attr| {
                acc.saturating_add(accounted_attr_bytes(attr))
            });
            let children = el.children.iter().fold(0usize, |acc, child| {
                acc.saturating_add(accounted_node_tree_bytes(child))
            });
            RESULT_NODE_ACCOUNTING_BYTES
                .saturating_add(el.qname.len())
                .saturating_add(accounted_ns_decls_bytes(&el.ns_decls))
                .saturating_add(attrs)
                .saturating_add(children)
        }
        ResultNode::Text { value, .. } | ResultNode::Comment(value) => {
            RESULT_NODE_ACCOUNTING_BYTES.saturating_add(value.len())
        }
        ResultNode::Pi { target, data } => RESULT_NODE_ACCOUNTING_BYTES
            .saturating_add(target.len())
            .saturating_add(data.len()),
    }
}

fn accounted_node_forest_bytes(nodes: &[ResultNode]) -> usize {
    nodes.iter().fold(0usize, |acc, node| {
        acc.saturating_add(accounted_node_tree_bytes(node))
    })
}

fn sanitize_comment_text(text: String) -> XmlResult<String> {
    if text.contains("--") || text.ends_with('-') {
        return Err(XmlError::validation(
            "xsl:comment content must not contain '--' or end with '-'",
        ));
    }
    Ok(text)
}

fn validate_pi_target(target: &str) -> XmlResult<()> {
    // Reuse the serializer's full-Unicode NCName check so XSLT construction and
    // serialization agree on which targets are valid (the ASCII-only check here
    // previously rejected legal non-ASCII names). NCName already excludes ':'
    // and whitespace; only the reserved "xml" target needs a separate guard.
    if !crate::writer::is_valid_xml_ncname(target) {
        return Err(XmlError::validation(
            "xsl:processing-instruction target is not a valid NCName",
        ));
    }
    if target.eq_ignore_ascii_case("xml") {
        return Err(XmlError::validation(
            "xsl:processing-instruction target must not be the reserved name 'xml'",
        ));
    }
    Ok(())
}

fn validate_result_qname(kind: &str, qname: &str) -> XmlResult<()> {
    if !crate::writer::is_valid_xml_qname(qname) {
        return Err(XmlError::validation(format!(
            "{kind} name is not a valid QName"
        )));
    }
    if qname == "xmlns" || qname.starts_with("xmlns:") {
        return Err(XmlError::validation(format!(
            "{kind} name must not use the reserved xmlns prefix"
        )));
    }
    Ok(())
}

fn sanitize_pi_data(data: String) -> XmlResult<String> {
    if data.contains("?>") {
        return Err(XmlError::validation(
            "xsl:processing-instruction data must not contain '?>'",
        ));
    }
    Ok(data)
}

// ─── Output options (xsl:output) ──────────────────────────

#[derive(Debug, Clone)]
struct OutputOptions {
    method_text: bool,
    /// Parsed from `xsl:output/@indent` but not yet applied — the serializer
    /// does not pretty-print. Retained so the value round-trips and indentation
    /// can be wired up later without an API change.
    indent: bool,
    encoding: String,
    omit_xml_declaration: bool,
}

impl Default for OutputOptions {
    fn default() -> Self {
        OutputOptions {
            method_text: false,
            indent: false,
            encoding: "UTF-8".to_string(),
            omit_xml_declaration: false,
        }
    }
}

// ─── Stylesheet model ─────────────────────────────────────

/// An XPath expression and the namespace bindings at its declaration site.
struct ScopedXPath {
    expression: CompiledXPath,
    namespaces: HashMap<String, String>,
}

impl ScopedXPath {
    fn compile(
        text: &str,
        max_depth: u32,
        namespaces: &HashMap<String, String>,
    ) -> XmlResult<Self> {
        Ok(Self {
            expression: CompiledXPath::compile(text, max_depth)?,
            namespaces: namespaces.clone(),
        })
    }
}

/// An external top-level parameter supplied for one transformation.
/// Expressions are evaluated against the source document root and retain their
/// XPath type. Literal strings bypass XPath parsing entirely.
#[derive(Clone, Debug)]
pub enum ParameterValue {
    /// Literal Unicode text, without XML or XPath escaping.
    String(String),
    /// An XPath 1.0 expression using the stylesheet's namespace bindings.
    Expression(String),
}

/// A compiled XSLT stylesheet, ready to transform source documents.
///
/// Build one with [`Stylesheet::compile`] and apply it with
/// [`Stylesheet::transform`]. Compiling once and transforming many documents
/// avoids re-parsing the stylesheet and re-compiling its XPath expressions.
pub struct Stylesheet {
    templates: Vec<Template>,
    /// Top-level `xsl:variable`/`xsl:param`, in document order.
    globals: Vec<GlobalVar>,
    output: OutputOptions,
    /// Local names of elements whose whitespace-only text children are stripped
    /// from the source (`xsl:strip-space`); `*` means all elements.
    strip_space_all: bool,
    /// Namespace prefix → URI map used to resolve prefixes in XPath
    /// expressions and result QNames (captured from the stylesheet root).
    namespaces: HashMap<String, String>,
    /// Maximum template-activation recursion depth (see
    /// [`DEFAULT_MAX_XSLT_DEPTH`]). Exceeding it aborts the transform with an
    /// error rather than overflowing the stack.
    max_depth: u32,
    /// Optional cap for serialized transform output. `None` preserves the
    /// historical unbounded behavior; fuzzing and other hostile-input callers can
    /// set a ceiling to avoid small stylesheets expanding into huge output.
    max_output_bytes: Option<usize>,
    /// Optional cap for materialized result-tree work before serialization.
    /// This is approximate and monotonic: it accounts for result nodes,
    /// attributes, namespace declarations, and string payload bytes as they are
    /// created, rather than trying to model exact live allocator usage.
    max_result_tree_bytes: Option<usize>,
    /// Enables the broader opt-in EXSLT function library (`math:`/`str:`/`set:`/
    /// `exsl:`); see [`Stylesheet::with_exslt`]. `date:date-time()` is available
    /// regardless. Default `false`.
    exslt_enabled: bool,
    /// External top-level `xsl:param` values supplied via
    /// [`Stylesheet::with_param`], as `(name, string value)`. At transform time
    /// an override replaces the param's default; names that are not declared
    /// params (or name an `xsl:variable`) are ignored.
    param_overrides: Vec<(String, String)>,
}

struct GlobalVar {
    name: String,
    value: ValueSource,
    /// `true` for a top-level `xsl:param` (whose value may be supplied externally
    /// via [`Stylesheet::with_param`]), `false` for an `xsl:variable`.
    is_param: bool,
}

struct Template {
    /// The compiled `match` pattern, if this is a match template.
    pattern: Option<CompiledPattern>,
    namespaces: HashMap<String, String>,
    /// Precomputed dispatch descriptor for `pattern`, used to skip
    /// non-applicable templates cheaply during per-node template selection.
    dispatch: Option<PatternDispatch>,
    /// Explicit `priority`, overriding the pattern's default priority.
    explicit_priority: Option<f64>,
    /// The `name`, if this is a named (callable) template.
    name: Option<String>,
    /// `xsl:param` declarations (name + default value source).
    params: Vec<WithParam>,
    body: Vec<Instruction>,
}

/// A single instruction in a template body (sequence constructor).
enum Instruction {
    /// A literal text node copied to the result.
    LiteralText(String),
    /// A literal result element with attribute templates and a body.
    LiteralElement {
        qname: String,
        ns_decls: Vec<(Option<String>, String)>,
        attrs: Vec<AttrTemplate>,
        body: Vec<Instruction>,
    },
    /// `xsl:value-of`.
    ValueOf {
        select: ScopedXPath,
        disable_escaping: bool,
    },
    /// `xsl:text`.
    XslText {
        value: String,
        disable_escaping: bool,
    },
    /// `xsl:apply-templates` with an optional `select` (defaults to the
    /// children of the current node) and `xsl:with-param` children.
    ApplyTemplates {
        select: Option<ScopedXPath>,
        params: Vec<WithParam>,
    },
    /// `xsl:if`.
    If {
        test: ScopedXPath,
        body: Vec<Instruction>,
    },
    /// `xsl:choose` — the first `when` whose test is true, else `otherwise`.
    Choose {
        whens: Vec<(ScopedXPath, Vec<Instruction>)>,
        otherwise: Vec<Instruction>,
    },
    /// `xsl:for-each` — execute the body for each selected node.
    ForEach {
        select: ScopedXPath,
        body: Vec<Instruction>,
    },
    /// `xsl:variable` declaration (scoped to following siblings).
    Variable { name: String, value: ValueSource },
    /// `xsl:copy` — shallow-copy the current node, then execute the body.
    Copy { body: Vec<Instruction> },
    /// `xsl:copy-of` — deep-copy the selected nodes / fragment.
    CopyOf { select: ScopedXPath },
    /// `xsl:element` with a computed (AVT) name.
    Element { name: Avt, body: Vec<Instruction> },
    /// `xsl:attribute` with a computed (AVT) name; the body is its value.
    Attribute {
        namespace: Option<Avt>,
        name: Avt,
        body: Vec<Instruction>,
        namespaces: HashMap<String, String>,
    },
    /// `xsl:comment`.
    Comment { body: Vec<Instruction> },
    /// `xsl:processing-instruction` with a computed (AVT) name.
    Pi { name: Avt, body: Vec<Instruction> },
    /// `xsl:call-template`.
    CallTemplate {
        name: String,
        params: Vec<WithParam>,
    },
    /// `xsl:message` — emitted to stderr; `terminate="yes"` is treated as a
    /// non-fatal log in this implementation.
    Message { body: Vec<Instruction> },
}

/// The value of an `xsl:variable`/`xsl:param`/`xsl:with-param`: either a
/// `select` expression or a body (result-tree fragment) sequence constructor.
enum ValueSource {
    Select(ScopedXPath),
    Body(Vec<Instruction>),
}

/// An `xsl:with-param` (or the default of an `xsl:param`).
struct WithParam {
    name: String,
    value: ValueSource,
}

/// A literal-result-element attribute, whose value is an attribute value
/// template (literal text interleaved with `{expr}` placeholders).
struct AttrTemplate {
    namespace_uri: Option<String>,
    qname: String,
    value: Avt,
}

/// A parsed attribute value template: a sequence of literal and expression
/// parts that concatenate to the attribute's string value.
struct Avt {
    parts: Vec<AvtPart>,
}

enum AvtPart {
    Literal(String),
    Expr(ScopedXPath),
}

// ─── Public API ───────────────────────────────────────────

impl Stylesheet {
    /// Compile a stylesheet from a parsed XSLT [`Document`].
    pub fn compile(doc: &Document<'_>) -> XmlResult<Stylesheet> {
        let root = doc
            .document_element()
            .ok_or_else(|| XmlError::xpath("Stylesheet has no document element"))?;
        compile_stylesheet(doc, root)
    }

    /// Override the maximum template-activation recursion depth (default
    /// [`DEFAULT_MAX_XSLT_DEPTH`]). Raise it for stylesheets with genuinely deep
    /// recursion running on large stacks; lower it to harden against runaway
    /// recursion on constrained stacks. Returns `self` for chaining.
    pub fn set_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Cap the serialized transform output size in bytes.
    ///
    /// This is opt-in: the default remains unbounded for source compatibility.
    /// It is useful when both the stylesheet and source are attacker-controlled,
    /// because small inputs can legally expand to very large result documents.
    ///
    /// The limit is enforced while serializing the result tree, so an oversized
    /// transform returns an error instead of allocating the final output string.
    /// It is not a complete result-tree allocation budget: templates still run
    /// and build their intermediate result before serialization starts. Use
    /// [`Self::set_max_result_tree_bytes`] as the earlier materialization guard.
    pub fn set_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = Some(max_output_bytes);
        self
    }

    /// Cap materialized result-tree work before serialization.
    ///
    /// This is opt-in: the default remains unbounded for source compatibility.
    /// It is intended for fuzzing and hostile-input callers that run untrusted
    /// stylesheets, where legal XSLT constructs can allocate a very large result
    /// tree before the serializer sees it.
    ///
    /// The cap is conservative rather than an exact heap limit. It charges each
    /// result node, attribute, namespace declaration, and payload string as the
    /// transform constructs them. Temporary result-tree fragments are counted
    /// even if they are later dropped, so callers should choose a ceiling with
    /// normal stylesheet behavior in mind.
    pub fn set_max_result_tree_bytes(mut self, max_result_tree_bytes: usize) -> Self {
        self.max_result_tree_bytes = Some(max_result_tree_bytes);
        self
    }

    /// Enable the opt-in EXSLT extension-function library (`math:`, `str:`,
    /// `set:`, `exsl:`) for this stylesheet's expressions. The stylesheet must
    /// bind the conventional EXSLT prefixes (e.g. `xmlns:str="http://exslt.org/
    /// strings"`); functions are matched by prefix. `date:date-time()` is
    /// available regardless of this flag. Returns `self` for chaining. See the
    /// crate-level documentation for the supported EXSLT function set.
    pub fn with_exslt(mut self, enabled: bool) -> Self {
        self.exslt_enabled = enabled;
        self
    }

    /// Supply an external value for a top-level `xsl:param`, overriding its
    /// default. The value is bound as a string (the common case for external
    /// parameters, e.g. `pubinfo.xsl`'s `$publisher`). Returns `self` for
    /// chaining; call once per parameter. A name that does not match a declared
    /// top-level `xsl:param` (including one that names an `xsl:variable`) is
    /// ignored, matching XSLT's treatment of unused supplied parameters. The last
    /// value supplied for a given name wins.
    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.param_overrides.push((name.into(), value.into()));
        self
    }

    /// Transform `source` and return the serialized result.
    ///
    /// The source document must be ready for XPath evaluation — call
    /// [`Document::prepare_xpath`](crate::dom::Document::prepare_xpath) first if
    /// any expression or pattern uses the attribute axis. The one-shot
    /// [`crate::transform`] helper does this for you.
    pub fn transform(&self, source: &Document<'_>) -> XmlResult<String> {
        self.transform_with_params(source, &[])
    }

    /// Transform with invocation-local parameters, without modifying this sheet.
    ///
    /// Call parameters override `with_param` defaults. Literals bind first, then
    /// expressions in input order; the last binding of a duplicate name wins.
    /// Undeclared parameters and names of variables do not override declarations.
    /// Expressions are evaluated even for unused names, using the source root
    /// (position and size 1) and stylesheet namespaces. Expressions may reference
    /// literal arguments and earlier expression arguments, but not stylesheet globals.
    /// The source must be prepared for XPath, as for [`Self::transform`].
    pub fn transform_with_params(
        &self,
        source: &Document<'_>,
        params: &[(String, ParameterValue)],
    ) -> XmlResult<String> {
        let mut engine = Engine {
            source,
            stylesheet: self,
            globals: Vec::new(),
            locals: Vec::new(),
            depth: 0,
            max_depth: self.max_depth,
            max_result_tree_bytes: self.max_result_tree_bytes,
            result_tree_bytes_used: 0,
            pending_global_messages: None,
        };
        engine.init_globals(params)?;
        let root = source.root();
        let result = engine.apply_to_node(
            Focus {
                node: root,
                position: 1,
                size: 1,
            },
            &[],
        )?;
        let nodes = items_to_nodes(result);
        let mut out = LimitedOutput::new(self.max_output_bytes);
        serialize_result(&nodes, &mut out, &self.output)?;
        Ok(out.into_string())
    }
}

// ─── Compilation ──────────────────────────────────────────

/// Collect the in-scope namespace bindings at `node` (walking ancestors),
/// outermost-first so inner declarations win. The `xsl` prefix binding to the
/// XSLT namespace is excluded (it never appears in result output or in
/// non-XSLT name resolution).
fn in_scope_namespaces(doc: &Document<'_>, node: NodeId) -> HashMap<String, String> {
    let mut map = complete_namespaces(doc, node);
    if map.get("xsl").is_some_and(|uri| uri == XSLT_NAMESPACE) {
        map.remove("xsl");
    }
    map
}

fn complete_namespaces(doc: &Document<'_>, node: NodeId) -> HashMap<String, String> {
    let mut chain = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        chain.push(n);
        cur = doc.parent(n);
    }
    chain.reverse();
    // The xml prefix is in scope even without an explicit xmlns:xml attribute.
    let mut map = HashMap::from([(
        "xml".to_string(),
        crate::namespace::XML_NAMESPACE.to_string(),
    )]);
    for n in chain {
        if let Some(NodeKind::Element(e)) = doc.node_kind(n) {
            for (prefix, uri) in &e.namespace_declarations {
                // DOM mutation can bypass parser validation. Preserve the
                // reserved binding even for programmatically built stylesheets.
                if prefix.as_ref() == "xml" {
                    continue;
                }
                map.insert(prefix.to_string(), uri.to_string());
            }
        }
    }
    map
}

fn compile_stylesheet(doc: &Document<'_>, root: NodeId) -> XmlResult<Stylesheet> {
    // Validate the document element is xsl:stylesheet / xsl:transform.
    let root_el = match doc.node_kind(root) {
        Some(NodeKind::Element(e)) => e,
        _ => return Err(XmlError::xpath("Stylesheet root is not an element")),
    };
    let is_xsl_root = root_el.name.namespace_uri.as_deref() == Some(XSLT_NAMESPACE)
        && matches!(root_el.name.local_name.as_ref(), "stylesheet" | "transform");
    if !is_xsl_root {
        return Err(XmlError::xpath(
            "Stylesheet root must be xsl:stylesheet or xsl:transform",
        ));
    }

    let namespaces = in_scope_namespaces(doc, root);

    let mut output = OutputOptions::default();
    let mut templates = Vec::new();
    let mut globals = Vec::new();
    let mut strip_space_all = false;

    for child in doc.children(root) {
        let el = match doc.node_kind(child) {
            Some(NodeKind::Element(e)) => e,
            _ => continue, // whitespace text / comments between top-level elements
        };
        if el.name.namespace_uri.as_deref() != Some(XSLT_NAMESPACE) {
            continue; // top-level non-XSLT (e.g. extension) elements are ignored
        }
        match el.name.local_name.as_ref() {
            "output" => parse_output(doc, child, &mut output)?,
            "template" => templates.push(compile_template(doc, child, &namespaces)?),
            "variable" | "param" => {
                let name = expanded_param_name(
                    el.get_attribute("name")
                        .ok_or_else(|| XmlError::xpath("top-level variable/param requires name"))?,
                    &in_scope_namespaces(doc, child),
                )?;
                let value = compile_value_source(doc, child, el, &namespaces)?;
                let is_param = el.name.local_name.as_ref() == "param";
                if globals.iter().any(|g: &GlobalVar| g.name == name) {
                    return Err(XmlError::xpath(format!(
                        "duplicate global variable or parameter: {name}"
                    )));
                }
                globals.push(GlobalVar {
                    name,
                    value,
                    is_param,
                });
            }
            "strip-space" if el.get_attribute("elements") == Some("*") => {
                strip_space_all = true;
            }
            // preserve-space, key, decimal-format, attribute-set, import/include
            // are out of scope for Tier A and ignored.
            _ => {}
        }
    }

    Ok(Stylesheet {
        templates,
        globals,
        output,
        strip_space_all,
        namespaces,
        max_depth: DEFAULT_MAX_XSLT_DEPTH,
        max_output_bytes: None,
        max_result_tree_bytes: None,
        exslt_enabled: false,
        param_overrides: Vec::new(),
    })
}

fn parse_output(doc: &Document<'_>, node: NodeId, output: &mut OutputOptions) -> XmlResult<()> {
    if let Some(m) = doc.get_attribute(node, "method") {
        // Only "xml" and "text" are implemented. Fail fast on any other method
        // (e.g. "html") rather than silently serializing as XML, which would
        // mask an incompatible stylesheet. (A prefixed/custom method name is
        // likewise unsupported.)
        match m {
            "xml" => output.method_text = false,
            "text" => output.method_text = true,
            other => {
                return Err(XmlError::xpath(format!(
                    "xsl:output method {other:?} is not supported (only \"xml\" and \"text\")"
                )));
            }
        }
    }
    if let Some(i) = doc.get_attribute(node, "indent") {
        output.indent = i == "yes";
    }
    if let Some(e) = doc.get_attribute(node, "encoding") {
        // The transform result is a Rust `String` (always UTF-8), so only UTF-8
        // can be honored — declaring any other encoding would mislabel the bytes
        // and downstream consumers would mis-decode them. Keep the canonical
        // "UTF-8" and ignore other requests rather than emit a wrong declaration.
        if e.eq_ignore_ascii_case("utf-8") {
            output.encoding = "UTF-8".to_string();
        }
    }
    if let Some(o) = doc.get_attribute(node, "omit-xml-declaration") {
        output.omit_xml_declaration = o == "yes";
    }
    Ok(())
}

fn compile_template(
    doc: &Document<'_>,
    node: NodeId,
    ns: &HashMap<String, String>,
) -> XmlResult<Template> {
    let el = match doc.node_kind(node) {
        Some(NodeKind::Element(e)) => e,
        _ => unreachable!("compile_template called on non-element"),
    };
    let match_attr = el.get_attribute("match");
    let name_attr = el.get_attribute("name").map(|s| s.to_string());

    let pattern = match match_attr {
        Some(m) => Some(CompiledPattern::compile(m, DEFAULT_MAX_XPATH_DEPTH)?),
        None => None,
    };
    let dispatch = pattern.as_ref().map(|p| p.dispatch());
    let explicit_priority = el
        .get_attribute("priority")
        .and_then(|s| s.trim().parse::<f64>().ok());

    // Leading xsl:param children declare the template's parameters; the rest is
    // the body sequence constructor. Per XSLT 1.0 the params must come first, so
    // an xsl:param after any non-whitespace content is an error (insignificant
    // whitespace between params does not end the parameter section).
    let mut params = Vec::new();
    let mut body = Vec::new();
    let mut seen_body = false;
    for child in doc.children(node) {
        match doc.node_kind(child) {
            Some(NodeKind::Element(ce)) if is_xsl(ce, "param") => {
                if seen_body {
                    return Err(XmlError::xpath(
                        "xsl:param must come before all other template content",
                    ));
                }
                let name = expanded_param_name(
                    ce.get_attribute("name")
                        .ok_or_else(|| XmlError::xpath("xsl:param requires name"))?,
                    &in_scope_namespaces(doc, child),
                )?;
                let value = compile_value_source(doc, child, ce, ns)?;
                params.push(WithParam { name, value });
            }
            // Whitespace-only text between params is insignificant and does not
            // start the body (so following xsl:param are still parameters).
            Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) if t.trim().is_empty() => {}
            _ => {
                seen_body = true;
                compile_node_into(doc, child, ns, &mut body)?;
            }
        }
    }

    Ok(Template {
        pattern,
        namespaces: in_scope_namespaces(doc, node),
        dispatch,
        explicit_priority,
        name: name_attr,
        params,
        body,
    })
}

/// True if `el` is an XSLT element with the given local name.
fn is_xsl(el: &crate::dom::Element<'_>, local: &str) -> bool {
    el.name.namespace_uri.as_deref() == Some(XSLT_NAMESPACE) && el.name.local_name.as_ref() == local
}

/// Compile the value of an `xsl:variable`/`xsl:param`/`xsl:with-param`: a
/// `select` expression if present, otherwise the element's body as a
/// result-tree fragment.
fn compile_value_source(
    doc: &Document<'_>,
    node: NodeId,
    el: &crate::dom::Element<'_>,
    _ns: &HashMap<String, String>,
) -> XmlResult<ValueSource> {
    let local_namespaces = in_scope_namespaces(doc, node);
    let ns = &local_namespaces;
    if let Some(sel) = el.get_attribute("select") {
        Ok(ValueSource::Select(ScopedXPath::compile(
            sel,
            DEFAULT_MAX_XPATH_DEPTH,
            ns,
        )?))
    } else {
        Ok(ValueSource::Body(compile_sequence(doc, node, ns)?))
    }
}

/// Compile the `xsl:with-param` children of `node` into parameter bindings.
fn compile_with_params(
    doc: &Document<'_>,
    node: NodeId,
    ns: &HashMap<String, String>,
) -> XmlResult<Vec<WithParam>> {
    let mut params = Vec::new();
    for child in doc.children(node) {
        if let Some(NodeKind::Element(ce)) = doc.node_kind(child) {
            if is_xsl(ce, "with-param") {
                let name = expanded_param_name(
                    ce.get_attribute("name")
                        .ok_or_else(|| XmlError::xpath("xsl:with-param requires name"))?,
                    &in_scope_namespaces(doc, child),
                )?;
                let value = compile_value_source(doc, child, ce, ns)?;
                params.push(WithParam { name, value });
            }
        }
    }
    Ok(params)
}

/// Compile the children of `node` into a sequence of instructions, applying
/// stylesheet whitespace stripping (whitespace-only text nodes are dropped
/// unless inside `xsl:text`).
fn compile_sequence(
    doc: &Document<'_>,
    node: NodeId,
    ns: &HashMap<String, String>,
) -> XmlResult<Vec<Instruction>> {
    let mut out = Vec::new();
    for child in doc.children(node) {
        compile_node_into(doc, child, ns, &mut out)?;
    }
    Ok(out)
}

/// Compile a single child node (text, literal element, or XSLT instruction)
/// into the instruction list, applying stylesheet whitespace stripping.
fn compile_node_into(
    doc: &Document<'_>,
    child: NodeId,
    ns: &HashMap<String, String>,
    out: &mut Vec<Instruction>,
) -> XmlResult<()> {
    match doc.node_kind(child) {
        Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) => {
            let s = t.as_ref();
            if !s.trim().is_empty() {
                out.push(Instruction::LiteralText(s.to_string()));
            }
            // Whitespace-only text in the stylesheet is stripped.
        }
        Some(NodeKind::Element(e)) => {
            if e.name.namespace_uri.as_deref() == Some(XSLT_NAMESPACE) {
                if let Some(instr) = compile_xsl_instruction(doc, child, e, ns)? {
                    out.push(instr);
                }
            } else {
                out.push(compile_literal_element(doc, child, ns)?);
            }
        }
        // Comments / PIs in the template are not copied to the result.
        _ => {}
    }
    Ok(())
}

fn compile_xsl_instruction(
    doc: &Document<'_>,
    node: NodeId,
    el: &crate::dom::Element<'_>,
    _ns: &HashMap<String, String>,
) -> XmlResult<Option<Instruction>> {
    let local_namespaces = in_scope_namespaces(doc, node);
    let ns = &local_namespaces;
    // Helper closures for common attribute reads.
    let compile_sel = |name: &str| -> XmlResult<ScopedXPath> {
        let s = el.get_attribute(name).ok_or_else(|| {
            XmlError::xpath(format!("xsl:{} requires {}", el.name.local_name, name))
        })?;
        ScopedXPath::compile(s, DEFAULT_MAX_XPATH_DEPTH, ns)
    };

    let instr = match el.name.local_name.as_ref() {
        "value-of" => Instruction::ValueOf {
            select: compile_sel("select")?,
            disable_escaping: el.get_attribute("disable-output-escaping") == Some("yes"),
        },
        "text" => {
            // Concatenate every Text/CDATA child: `element_text` returns only the
            // first segment, so a multi-segment `xsl:text` (mixed text + CDATA,
            // entity-expanded splits) would otherwise drop everything after it.
            let mut value = String::new();
            for child in doc.children(node) {
                match doc.node_kind(child) {
                    Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) => value.push_str(t),
                    _ => {}
                }
            }
            Instruction::XslText {
                value,
                disable_escaping: el.get_attribute("disable-output-escaping") == Some("yes"),
            }
        }
        "apply-templates" => Instruction::ApplyTemplates {
            select: match el.get_attribute("select") {
                Some(s) => Some(ScopedXPath::compile(s, DEFAULT_MAX_XPATH_DEPTH, ns)?),
                None => None,
            },
            params: compile_with_params(doc, node, ns)?,
        },
        "if" => Instruction::If {
            test: compile_sel("test")?,
            body: compile_sequence(doc, node, ns)?,
        },
        "choose" => compile_choose(doc, node, ns)?,
        "for-each" => Instruction::ForEach {
            select: compile_sel("select")?,
            body: compile_sequence(doc, node, ns)?,
        },
        "variable" => {
            let name = expanded_param_name(
                el.get_attribute("name")
                    .ok_or_else(|| XmlError::xpath("xsl:variable requires name"))?,
                &in_scope_namespaces(doc, node),
            )?;
            Instruction::Variable {
                name,
                value: compile_value_source(doc, node, el, ns)?,
            }
        }
        "copy" => Instruction::Copy {
            body: compile_sequence(doc, node, ns)?,
        },
        "copy-of" => Instruction::CopyOf {
            select: compile_sel("select")?,
        },
        "element" => Instruction::Element {
            name: compile_avt(
                el.get_attribute("name")
                    .ok_or_else(|| XmlError::xpath("xsl:element requires name"))?,
                ns,
            )?,
            body: compile_sequence(doc, node, ns)?,
        },
        "attribute" => Instruction::Attribute {
            namespace: el
                .get_attribute("namespace")
                .map(|v| compile_avt(v, ns))
                .transpose()?,
            namespaces: complete_namespaces(doc, node),
            name: compile_avt(
                el.get_attribute("name")
                    .ok_or_else(|| XmlError::xpath("xsl:attribute requires name"))?,
                ns,
            )?,
            body: compile_sequence(doc, node, ns)?,
        },
        "comment" => Instruction::Comment {
            body: compile_sequence(doc, node, ns)?,
        },
        "processing-instruction" => Instruction::Pi {
            name: compile_avt(
                el.get_attribute("name")
                    .ok_or_else(|| XmlError::xpath("xsl:processing-instruction requires name"))?,
                ns,
            )?,
            body: compile_sequence(doc, node, ns)?,
        },
        "call-template" => Instruction::CallTemplate {
            name: el
                .get_attribute("name")
                .ok_or_else(|| XmlError::xpath("xsl:call-template requires name"))?
                .to_string(),
            params: compile_with_params(doc, node, ns)?,
        },
        "message" => Instruction::Message {
            body: compile_sequence(doc, node, ns)?,
        },
        // Structural children handled by their parent's compiler; if reached
        // standalone they are no-ops.
        "param" | "with-param" | "sort" | "when" | "otherwise" => return Ok(None),
        other => {
            return Err(XmlError::xpath(format!(
                "Unsupported XSLT instruction (not yet implemented): xsl:{}",
                other
            )))
        }
    };
    Ok(Some(instr))
}

/// Compile an `xsl:choose` into when/otherwise branches.
fn compile_choose(
    doc: &Document<'_>,
    node: NodeId,
    ns: &HashMap<String, String>,
) -> XmlResult<Instruction> {
    let mut whens = Vec::new();
    let mut otherwise = Vec::new();
    for child in doc.children(node) {
        if let Some(NodeKind::Element(ce)) = doc.node_kind(child) {
            if is_xsl(ce, "when") {
                let test = ce
                    .get_attribute("test")
                    .ok_or_else(|| XmlError::xpath("xsl:when requires test"))?;
                whens.push((
                    ScopedXPath::compile(
                        test,
                        DEFAULT_MAX_XPATH_DEPTH,
                        &in_scope_namespaces(doc, child),
                    )?,
                    compile_sequence(doc, child, ns)?,
                ));
            } else if is_xsl(ce, "otherwise") {
                otherwise = compile_sequence(doc, child, ns)?;
            }
        }
    }
    Ok(Instruction::Choose { whens, otherwise })
}

fn compile_literal_element(
    doc: &Document<'_>,
    node: NodeId,
    _ns: &HashMap<String, String>,
) -> XmlResult<Instruction> {
    let local_namespaces = in_scope_namespaces(doc, node);
    let ns = &local_namespaces;
    let el = match doc.node_kind(node) {
        Some(NodeKind::Element(e)) => e,
        _ => unreachable!(),
    };
    let qname = el.name.prefixed_name().to_string();

    // Emit a namespace declaration for the element's own prefix/uri so the
    // result re-parses. (Full namespace fixup arrives in a later milestone.)
    let mut ns_decls: Vec<(Option<String>, String)> = Vec::new();
    if let Some(uri) = el.name.namespace_uri.as_deref() {
        let prefix = el.name.prefix.as_deref().map(|s| s.to_string());
        ns_decls.push((prefix, uri.to_string()));
    }
    // Preserve namespace declarations written explicitly on the literal element
    // (e.g. `xmlns:foo=...`) so prefixes used only by attributes, children, or
    // QName-valued content still resolve in the output. The `xsl` -> XSLT binding
    // never appears in result output, and the serializer suppresses any
    // declaration already in an enclosing result scope, so this cannot duplicate.
    for (prefix, uri) in &el.namespace_declarations {
        if prefix.as_ref() == "xsl" && uri.as_ref() == XSLT_NAMESPACE {
            continue;
        }
        let p = if prefix.is_empty() {
            None
        } else {
            Some(prefix.to_string())
        };
        if !ns_decls
            .iter()
            .any(|(ep, eu)| ep.as_deref() == p.as_deref() && eu.as_str() == uri.as_ref())
        {
            ns_decls.push((p, uri.to_string()));
        }
    }

    let mut attrs = Vec::new();
    for attr in &el.attributes {
        // Skip namespace declarations (they are reconstructed via ns_decls).
        if attr.name.prefix.as_deref() == Some("xmlns")
            || (attr.name.prefix.is_none() && attr.name.local_name.as_ref() == "xmlns")
        {
            continue;
        }
        attrs.push(AttrTemplate {
            namespace_uri: attr.name.namespace_uri.as_deref().map(str::to_owned),
            qname: attr.name.prefixed_name().to_string(),
            value: compile_avt(attr.value.as_ref(), ns)?,
        });
    }

    let body = compile_sequence(doc, node, ns)?;
    Ok(Instruction::LiteralElement {
        qname,
        ns_decls,
        attrs,
        body,
    })
}

/// Parse an attribute value template: text with `{expr}` placeholders and
/// `{{`/`}}` escapes.
fn compile_avt(value: &str, ns: &HashMap<String, String>) -> XmlResult<Avt> {
    // Fast path: no braces means a single literal part, so skip the char buffer
    // and the placeholder scan entirely (the overwhelmingly common attribute).
    if !value.contains(['{', '}']) {
        return Ok(Avt {
            parts: if value.is_empty() {
                Vec::new()
            } else {
                vec![AvtPart::Literal(value.to_string())]
            },
        });
    }
    let mut parts = Vec::new();
    let mut literal = String::new();
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' if i + 1 < chars.len() && chars[i + 1] == '{' => {
                literal.push('{');
                i += 2;
            }
            '}' if i + 1 < chars.len() && chars[i + 1] == '}' => {
                literal.push('}');
                i += 2;
            }
            '{' => {
                if !literal.is_empty() {
                    parts.push(AvtPart::Literal(std::mem::take(&mut literal)));
                }
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(XmlError::xpath(
                        "Unterminated { in attribute value template",
                    ));
                }
                let expr: String = chars[start..i].iter().collect();
                parts.push(AvtPart::Expr(ScopedXPath::compile(
                    &expr,
                    DEFAULT_MAX_XPATH_DEPTH,
                    ns,
                )?));
                i += 1; // consume '}'
            }
            // A lone `}` (not part of a `}}` pair) is a static error in an AVT:
            // a literal right brace must be written as `}}`. Rejecting it surfaces
            // stylesheet bugs instead of silently emitting the stray brace.
            '}' => {
                return Err(XmlError::xpath(
                    "Unescaped '}' in attribute value template (write '}}' for a literal '}')",
                ));
            }
            c => {
                literal.push(c);
                i += 1;
            }
        }
    }
    if !literal.is_empty() {
        parts.push(AvtPart::Literal(literal));
    }
    Ok(Avt { parts })
}

// ─── Execution engine ─────────────────────────────────────

struct Engine<'a, 'b> {
    source: &'a Document<'b>,
    stylesheet: &'a Stylesheet,
    /// Top-level variables/params, evaluated once at the start of a transform.
    globals: Vec<(String, VarValue)>,
    /// The current template's local bindings (params + `xsl:variable`), as a
    /// stack scanned innermost-last. Reset to a fresh frame on each template
    /// application; grown/truncated by `xsl:for-each` and `xsl:variable`.
    locals: Vec<(String, VarValue)>,
    /// Current template-activation recursion depth (number of `execute_template`
    /// frames on the stack), guarded against `max_depth` to prevent runaway
    /// recursion from overflowing the stack.
    depth: u32,
    /// Recursion-depth ceiling (from [`Stylesheet::max_depth`]).
    max_depth: u32,
    /// Optional result-tree construction budget copied from the stylesheet at
    /// transform start.
    max_result_tree_bytes: Option<usize>,
    /// Monotonic accounting for result-tree materialization. It is not decremented
    /// when temporary fragments are dropped; the budget limits work performed,
    /// not exact live heap.
    result_tree_bytes_used: usize,
    /// Messages produced by a speculative global initializer. Publish only
    /// after successful evaluation; a deferred or failed attempt discards them.
    pending_global_messages: Option<Vec<String>>,
}

/// Resolves `$variable` references against the engine's local then global scope.
struct ScopeResolver<'s> {
    locals: &'s [(String, VarValue)],
    globals: &'s [(String, VarValue)],
    namespaces: &'s HashMap<String, String>,
}

impl VariableResolver for ScopeResolver<'_> {
    fn resolve_variable(&self, prefix: Option<&str>, local: &str) -> Option<XPathValue> {
        let key = expanded_param_name(&var_key(prefix, local), self.namespaces).ok()?;
        self.locals
            .iter()
            .rev()
            .chain(self.globals.iter().rev())
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_xpath())
    }
}

/// Resolves the EXSLT extension functions uppsala supports, against the source
/// document (needed for node-set arguments). `date:date-time()` is always
/// available; the broader EXSLT set (`math:`/`str:`/`set:`/`exsl:`) is gated on
/// `enabled` (see [`Stylesheet::with_exslt`]). See [`crate::exslt`].
struct ExsltResolver<'a, 'b> {
    doc: &'a Document<'b>,
    enabled: bool,
}
impl FunctionResolver for ExsltResolver<'_, '_> {
    fn resolve_function(
        &self,
        prefix: Option<&str>,
        local: &str,
        args: &[XPathValue],
    ) -> Option<XmlResult<XPathValue>> {
        crate::exslt::resolve(self.doc, prefix, local, args, self.enabled)
    }
}

/// Resolve API Clark names or QNames to an expanded variable name. Unprefixed
/// variables have no namespace, regardless of the default namespace binding.
fn expanded_param_name(name: &str, namespaces: &HashMap<String, String>) -> XmlResult<String> {
    if name.starts_with('{') {
        if let Some((_, local)) = name.split_once('}') {
            if crate::writer::is_valid_xml_ncname(local) {
                return Ok(name.to_string());
            }
        }
        return Err(XmlError::xpath("invalid expanded parameter name"));
    }
    if !crate::writer::is_valid_xml_qname(name) {
        return Err(XmlError::xpath(format!("invalid parameter name: {name}")));
    }
    if let Some((prefix, local)) = name.split_once(':') {
        let uri = if prefix == "xml" {
            "http://www.w3.org/XML/1998/namespace"
        } else {
            namespaces
                .get(prefix)
                .map(String::as_str)
                .ok_or_else(|| XmlError::xpath(format!("unbound parameter prefix: {prefix}")))?
        };
        return Ok(format!("{{{uri}}}{local}"));
    }
    Ok(name.to_string())
}

/// Reconstruct the lexical QName delivered by the XPath variable resolver.
fn var_key(prefix: Option<&str>, local: &str) -> String {
    match prefix {
        Some(p) => format!("{}:{}", p, local),
        None => local.to_string(),
    }
}

/// The current evaluation focus: the context node plus its position/size in the
/// node list being processed (drives `position()` / `last()`).
#[derive(Clone, Copy)]
struct Focus {
    node: NodeId,
    position: usize,
    size: usize,
}

impl<'a, 'b> Engine<'a, 'b> {
    /// Charge result-tree construction work against the optional transform
    /// budget. The accounting is conservative and monotonic by design: fuzzing
    /// and hostile-input callers need a cheap amplification guard, not an exact
    /// allocator profiler.
    fn charge_result_bytes(&mut self, bytes: usize) -> XmlResult<()> {
        let next = self.result_tree_bytes_used.saturating_add(bytes);
        if let Some(max_bytes) = self.max_result_tree_bytes {
            if next > max_bytes {
                return Err(XmlError::xpath(format!(
                    "XSLT result tree exceeds maximum materialized size of {} bytes",
                    max_bytes
                )));
            }
        }
        self.result_tree_bytes_used = next;
        Ok(())
    }

    fn charge_result_str(&mut self, s: &str) -> XmlResult<()> {
        self.charge_result_bytes(s.len())
    }

    fn charge_result_attr(&mut self, attr: &ResultAttr) -> XmlResult<()> {
        self.charge_result_bytes(accounted_attr_bytes(attr))
    }

    fn charge_result_ns_decls(&mut self, ns_decls: &[(Option<String>, String)]) -> XmlResult<()> {
        self.charge_result_bytes(accounted_ns_decls_bytes(ns_decls))
    }

    fn push_text_item(
        &mut self,
        out: &mut Vec<ResultItem>,
        value: String,
        disable_escaping: bool,
    ) -> XmlResult<()> {
        self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES)?;
        self.charge_result_str(&value)?;
        out.push(ResultItem::Node(ResultNode::Text {
            value,
            disable_escaping,
        }));
        Ok(())
    }

    fn push_attr_item(&mut self, out: &mut Vec<ResultItem>, attr: ResultAttr) -> XmlResult<()> {
        self.charge_result_attr(&attr)?;
        out.push(ResultItem::Attr(attr));
        Ok(())
    }

    fn push_comment_item(&mut self, out: &mut Vec<ResultItem>, text: String) -> XmlResult<()> {
        self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES)?;
        self.charge_result_str(&text)?;
        out.push(ResultItem::Node(ResultNode::Comment(text)));
        Ok(())
    }

    fn push_pi_item(
        &mut self,
        out: &mut Vec<ResultItem>,
        target: String,
        data: String,
    ) -> XmlResult<()> {
        self.charge_result_bytes(
            RESULT_NODE_ACCOUNTING_BYTES
                .saturating_add(target.len())
                .saturating_add(data.len()),
        )?;
        out.push(ResultItem::Node(ResultNode::Pi { target, data }));
        Ok(())
    }

    fn push_element_item(
        &mut self,
        out: &mut Vec<ResultItem>,
        mut element: ResultElement,
    ) -> XmlResult<()> {
        fixup_attribute_namespaces(&mut element, |bytes| self.charge_result_bytes(bytes))?;
        self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES.saturating_add(element.qname.len()))?;
        self.charge_result_ns_decls(&element.ns_decls)?;
        out.push(ResultItem::Node(ResultNode::Element(element)));
        Ok(())
    }

    /// Bind invocation parameters, then resolve global defaults in dependency
    /// order. Missing or circular global dependencies fail the invocation.
    fn init_globals(&mut self, params: &[(String, ParameterValue)]) -> XmlResult<()> {
        let stylesheet = self.stylesheet;
        let focus = Focus {
            node: self.source.root(),
            position: 1,
            size: 1,
        };
        let params = params
            .iter()
            .map(|(name, value)| Ok((expanded_param_name(name, &stylesheet.namespaces)?, value)))
            .collect::<XmlResult<Vec<_>>>()?;
        let defaults = stylesheet
            .param_overrides
            .iter()
            .filter_map(|(name, value)| {
                let name = expanded_param_name(name, &stylesheet.namespaces).ok()?;
                stylesheet
                    .globals
                    .iter()
                    .any(|g| g.is_param && g.name == name)
                    .then_some((name, value))
            })
            .collect::<Vec<_>>();
        // Literal parameters are bound first, matching lxml's strparam behavior.
        // Expressions then see literals and earlier expressions, in input order.
        // Evaluate even unused names to report malformed expressions.
        for (name, value) in defaults {
            self.globals.retain(|(key, _)| key != &name);
            self.globals
                .push((name, VarValue::Value(XPathValue::String(value.clone()))));
        }
        let (literals, expressions): (Vec<_>, Vec<_>) = params
            .into_iter()
            .partition(|(_, value)| matches!(value, ParameterValue::String(_)));
        for (name, value) in literals.into_iter().chain(expressions) {
            let value = match value {
                ParameterValue::String(value) => XPathValue::String(value.clone()),
                ParameterValue::Expression(expr) => {
                    let compiled = ScopedXPath::compile(
                        expr,
                        DEFAULT_MAX_XPATH_DEPTH,
                        &stylesheet.namespaces,
                    )?;
                    self.eval(&compiled, focus)?
                }
            };
            self.globals.retain(|(key, _)| key != &name);
            self.globals.push((name, VarValue::Value(value)));
        }
        // Only declared parameters override stylesheet bindings. Install them
        // before resolving defaults, regardless of declaration order.
        self.globals.retain(|(name, _)| {
            stylesheet
                .globals
                .iter()
                .any(|g| g.is_param && &g.name == name)
        });
        let mut pending: Vec<_> = stylesheet
            .globals
            .iter()
            .filter(|g| !self.globals.iter().any(|(name, _)| name == &g.name))
            .collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut deferred = Vec::new();
            let mut unresolved = None;
            for g in pending {
                match self.eval_global_value(&g.value, focus) {
                    Ok(value) => self.globals.push((g.name.clone(), value)),
                    Err(XmlError::XPath(ref err))
                        if err.message.starts_with("Undefined variable:") =>
                    {
                        unresolved = Some(err.message.clone());
                        deferred.push(g);
                    }
                    Err(err) => return Err(err),
                }
            }
            if deferred.len() == before {
                return Err(XmlError::xpath(format!(
                    "unresolved or circular global dependency: {}",
                    unresolved.unwrap_or_default()
                )));
            }
            pending = deferred;
        }
        Ok(())
    }

    /// Evaluate a global initializer transactionally. Result nodes are private
    /// until success; messages are the engine's only externally visible effect
    /// and are buffered through nested template calls. Restore the local frame
    /// on every exit, including errors in template parameter defaults.
    ///
    /// Work accounting deliberately stays monotonic: discarded construction
    /// still consumes the configured budget, so retries cannot bypass limits.
    fn eval_global_value(&mut self, value: &ValueSource, focus: Focus) -> XmlResult<VarValue> {
        debug_assert!(self.pending_global_messages.is_none());
        self.pending_global_messages = Some(Vec::new());
        let saved_locals = std::mem::take(&mut self.locals);
        let result = self.eval_value_source(value, focus);
        self.locals = saved_locals;
        let messages = self.pending_global_messages.take().unwrap_or_default();
        if result.is_ok() {
            for message in messages {
                eprintln!("xsl:message: {}", message);
            }
        }
        result
    }

    /// Process `focus.node`: run the best-matching template (with `params`), or
    /// the built-in template rule if no user template matches.
    fn apply_to_node(
        &mut self,
        focus: Focus,
        params: &[(String, VarValue)],
    ) -> XmlResult<Vec<ResultItem>> {
        if let Some(tmpl_idx) = self.best_matching_template(focus.node) {
            self.execute_template(tmpl_idx, focus, params)
        } else {
            self.apply_builtin_rule(focus)
        }
    }

    /// XSLT 1.0 built-in template rules (§5.8):
    /// - root/element → apply-templates to children;
    /// - text/CDATA/attribute → copy the string value;
    /// - comment/PI → nothing.
    fn apply_builtin_rule(&mut self, focus: Focus) -> XmlResult<Vec<ResultItem>> {
        match self.source.node_kind(focus.node) {
            Some(NodeKind::Document) | Some(NodeKind::Element(_)) => {
                self.apply_to_children(focus.node)
            }
            Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) => {
                let mut out = Vec::new();
                self.push_text_item(&mut out, t.to_string(), false)?;
                Ok(out)
            }
            Some(NodeKind::Attribute(_, v)) => {
                let mut out = Vec::new();
                self.push_text_item(&mut out, v.to_string(), false)?;
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Apply templates to the children of `node` in document order (the default
    /// `xsl:apply-templates` behavior and the built-in rule for element/root).
    fn apply_to_children(&mut self, node: NodeId) -> XmlResult<Vec<ResultItem>> {
        let children = self.default_children(node);
        self.apply_to_list(&children, &[])
    }

    /// The children of `node` as a default-select node list, with whitespace-only
    /// text nodes removed when `xsl:strip-space elements="*"` is in effect.
    ///
    /// (`xml:space="preserve"` is not yet honored; Tier A stylesheets do not use
    /// it. Explicit `select` expressions are not stripped — only the default
    /// child traversal, which is where indentation whitespace appears.)
    fn default_children(&self, node: NodeId) -> Vec<NodeId> {
        let children = self.source.children(node);
        if !self.stylesheet.strip_space_all {
            return children;
        }
        children
            .into_iter()
            .filter(|&c| !self.is_whitespace_only_text(c))
            .collect()
    }

    fn is_whitespace_only_text(&self, node: NodeId) -> bool {
        matches!(
            self.source.node_kind(node),
            Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) if t.trim().is_empty()
        )
    }

    /// Apply templates to each node in `list`, with position/size set from the
    /// list, passing `params` to each matched template, concatenating results.
    fn apply_to_list(
        &mut self,
        list: &[NodeId],
        params: &[(String, VarValue)],
    ) -> XmlResult<Vec<ResultItem>> {
        let size = list.len();
        let mut out = Vec::new();
        for (i, &n) in list.iter().enumerate() {
            let focus = Focus {
                node: n,
                position: i + 1,
                size,
            };
            out.extend(self.apply_to_node(focus, params)?);
        }
        Ok(out)
    }

    /// Execute a matched/named template, bounding recursion depth. Every
    /// infinite-recursion path (`apply-templates` cycles, `call-template` chains)
    /// flows through here, so a single counter here guards the whole engine
    /// against stack-overflow aborts. Increment-around-call ensures the depth is
    /// always restored, including on the early-return error paths inside the body.
    fn execute_template(
        &mut self,
        tmpl_idx: usize,
        focus: Focus,
        params: &[(String, VarValue)],
    ) -> XmlResult<Vec<ResultItem>> {
        self.depth += 1;
        let result = if self.depth > self.max_depth {
            Err(XmlError::xpath(format!(
                "XSLT recursion limit exceeded ({} template activations); \
                 possible infinite template recursion",
                self.max_depth
            )))
        } else {
            self.execute_template_inner(tmpl_idx, focus, params)
        };
        self.depth -= 1;
        result
    }

    /// The body of [`Self::execute_template`]; never call directly (it does not
    /// account for recursion depth).
    fn execute_template_inner(
        &mut self,
        tmpl_idx: usize,
        focus: Focus,
        params: &[(String, VarValue)],
    ) -> XmlResult<Vec<ResultItem>> {
        let stylesheet = self.stylesheet; // 'a borrow, independent of &mut self
        let tmpl = &stylesheet.templates[tmpl_idx];

        // Templates do not see the caller's local variables — only globals and
        // their own params. Swap in a fresh local frame.
        let saved = std::mem::take(&mut self.locals);
        for p in &tmpl.params {
            let val = match params.iter().find(|(n, _)| n == &p.name) {
                Some((_, v)) => v.clone(),
                // Defaults are evaluated in the callee context and can reference
                // params declared earlier (already pushed onto self.locals).
                None => self.eval_value_source(&p.value, focus)?,
            };
            self.locals.push((p.name.clone(), val));
        }
        let result = self.execute_sequence(&tmpl.body, focus);
        self.locals = saved;
        result
    }

    /// Select the template to apply to `node`: the highest-effective-priority
    /// matching template, breaking ties by document order (later wins, per the
    /// XSLT recovery rule). Effective priority is the template's explicit
    /// `priority` if given, else the matching pattern alternative's default
    /// priority.
    fn best_matching_template(&self, node: NodeId) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, tmpl) in self.stylesheet.templates.iter().enumerate() {
            if let Some(pattern) = &tmpl.pattern {
                let vars = ScopeResolver {
                    locals: &self.locals,
                    globals: &self.globals,
                    namespaces: &tmpl.namespaces,
                };
                // Cheap O(1) pre-filter: skip the full pattern match for nodes
                // this template's pattern can never match (by kind/name). This
                // turns per-node selection from "test every template" into "test
                // only applicable templates" — decisive for stylesheets with many
                // templates (e.g. eidas-cleanup) over large inputs.
                if let Some(dispatch) = &tmpl.dispatch {
                    if !dispatch.could_match(self.source, node) {
                        continue;
                    }
                }
                if let Some(default_prio) = pattern.matches(
                    self.source,
                    node,
                    node,
                    &tmpl.namespaces,
                    &vars,
                    &self.funcs(),
                    DEFAULT_MAX_XPATH_NODE_VISITS,
                ) {
                    let prio = tmpl.explicit_priority.unwrap_or(default_prio);
                    let better = match best {
                        None => true,
                        Some((_, p)) => prio >= p,
                    };
                    if better {
                        best = Some((i, prio));
                    }
                }
            }
        }
        best.map(|(i, _)| i)
    }

    fn execute_sequence(
        &mut self,
        body: &[Instruction],
        focus: Focus,
    ) -> XmlResult<Vec<ResultItem>> {
        // Local variables declared in this sequence are scoped to it: remember
        // the scope depth and truncate back on exit.
        let mark = self.locals.len();
        let mut out = Vec::new();
        for instr in body {
            if let Err(error) = self.execute_instruction(instr, focus, &mut out) {
                self.locals.truncate(mark);
                return Err(error);
            }
        }
        self.locals.truncate(mark);
        Ok(out)
    }

    fn execute_instruction(
        &mut self,
        instr: &Instruction,
        focus: Focus,
        out: &mut Vec<ResultItem>,
    ) -> XmlResult<()> {
        match instr {
            Instruction::LiteralText(s) => self.push_text_item(out, s.clone(), false)?,
            Instruction::XslText {
                value,
                disable_escaping,
            } => self.push_text_item(out, value.clone(), *disable_escaping)?,
            Instruction::ValueOf {
                select,
                disable_escaping,
            } => {
                let val = self.eval(select, focus)?;
                self.push_text_item(out, val.to_string_value(self.source), *disable_escaping)?;
            }
            Instruction::LiteralElement {
                qname,
                ns_decls,
                attrs,
                body,
            } => {
                let mut result_attrs = Vec::with_capacity(attrs.len());
                for attr in attrs {
                    let result_attr = ResultAttr {
                        namespace_uri: attr.namespace_uri.clone(),
                        qname: attr.qname.clone(),
                        value: self.eval_avt(&attr.value, focus)?,
                    };
                    self.charge_result_attr(&result_attr)?;
                    result_attrs.push(result_attr);
                }
                let items = self.execute_sequence(body, focus)?;
                let (body_attrs, children) = split_items(items);
                result_attrs.extend(body_attrs);
                self.push_element_item(
                    out,
                    ResultElement {
                        qname: qname.clone(),
                        ns_decls: ns_decls.clone(),
                        attrs: result_attrs,
                        children,
                    },
                )?;
            }
            Instruction::ApplyTemplates { select, params } => {
                let nodes = match select {
                    Some(expr) => self.eval(expr, focus)?.as_node_set().to_vec(),
                    None => self.default_children(focus.node),
                };
                let evaluated = self.eval_params(params, focus)?;
                out.extend(self.apply_to_list(&nodes, &evaluated)?);
            }
            Instruction::If { test, body } => {
                if self.eval(test, focus)?.to_boolean() {
                    out.extend(self.execute_sequence(body, focus)?);
                }
            }
            Instruction::Choose { whens, otherwise } => {
                let mut matched = false;
                for (test, body) in whens {
                    if self.eval(test, focus)?.to_boolean() {
                        out.extend(self.execute_sequence(body, focus)?);
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    out.extend(self.execute_sequence(otherwise, focus)?);
                }
            }
            Instruction::ForEach { select, body } => {
                let nodes = self.eval(select, focus)?.as_node_set().to_vec();
                let size = nodes.len();
                for (i, &n) in nodes.iter().enumerate() {
                    let child_focus = Focus {
                        node: n,
                        position: i + 1,
                        size,
                    };
                    out.extend(self.execute_sequence(body, child_focus)?);
                }
            }
            Instruction::Variable { name, value } => {
                let val = self.eval_value_source(value, focus)?;
                self.locals.push((name.clone(), val));
            }
            Instruction::Copy { body } => self.execute_copy(body, focus, out)?,
            Instruction::CopyOf { select } => self.execute_copy_of(select, focus, out)?,
            Instruction::Element { name, body } => {
                let qname = self.eval_avt(name, focus)?;
                validate_result_qname("xsl:element", &qname)?;
                let ns_decls = self.element_ns_decls(&qname)?;
                let items = self.execute_sequence(body, focus)?;
                let (attrs, children) = split_items(items);
                self.push_element_item(
                    out,
                    ResultElement {
                        qname,
                        ns_decls,
                        attrs,
                        children,
                    },
                )?;
            }
            Instruction::Attribute {
                namespace,
                name,
                body,
                namespaces,
            } => {
                let explicit_uri = namespace
                    .as_ref()
                    .map(|avt| self.eval_avt(avt, focus))
                    .transpose()?;
                let mut qname = self.eval_avt(name, focus)?;
                validate_result_qname("xsl:attribute", &qname)?;
                let namespace_uri = if let Some(uri) = explicit_uri {
                    if uri.is_empty() {
                        qname = qname.rsplit(':').next().unwrap().to_string();
                        None
                    } else {
                        Some(uri)
                    }
                } else {
                    qname
                        .split_once(':')
                        .map(|(prefix, _)| {
                            namespaces.get(prefix).cloned().ok_or_else(|| {
                                XmlError::xpath(format!("xsl:attribute: unbound prefix {prefix:?}"))
                            })
                        })
                        .transpose()?
                };
                let items = self.execute_sequence(body, focus)?;
                let value = rtf_string_value(&items_to_nodes(items));
                self.push_attr_item(
                    out,
                    ResultAttr {
                        namespace_uri,
                        qname,
                        value,
                    },
                )?;
            }
            Instruction::Comment { body } => {
                let items = self.execute_sequence(body, focus)?;
                let text = sanitize_comment_text(rtf_string_value(&items_to_nodes(items)))?;
                self.push_comment_item(out, text)?;
            }
            Instruction::Pi { name, body } => {
                let target = self.eval_avt(name, focus)?;
                validate_pi_target(&target)?;
                let items = self.execute_sequence(body, focus)?;
                let data = sanitize_pi_data(rtf_string_value(&items_to_nodes(items)))?;
                self.push_pi_item(out, target, data)?;
            }
            Instruction::CallTemplate { name, params } => {
                let idx = self
                    .stylesheet
                    .templates
                    .iter()
                    .position(|t| t.name.as_deref() == Some(name.as_str()))
                    .ok_or_else(|| {
                        XmlError::xpath(format!("xsl:call-template: no template named {:?}", name))
                    })?;
                let evaluated = self.eval_params(params, focus)?;
                out.extend(self.execute_template(idx, focus, &evaluated)?);
            }
            Instruction::Message { body } => {
                // Emit to stderr; treat terminate="yes" as a non-fatal log.
                let items = self.execute_sequence(body, focus)?;
                let text = rtf_string_value(&items_to_nodes(items));
                if let Some(messages) = &mut self.pending_global_messages {
                    messages.push(text);
                } else {
                    eprintln!("xsl:message: {}", text);
                }
            }
        }
        Ok(())
    }

    /// `xsl:copy` — shallow-copy the current node, then run the body to supply
    /// attributes/children (for elements) or content.
    fn execute_copy(
        &mut self,
        body: &[Instruction],
        focus: Focus,
        out: &mut Vec<ResultItem>,
    ) -> XmlResult<()> {
        match self.source.node_kind(focus.node) {
            Some(NodeKind::Document) => {
                // Copying the root just processes the body.
                out.extend(self.execute_sequence(body, focus)?);
            }
            Some(NodeKind::Element(e)) => {
                let qname = e.name.prefixed_name().to_string();
                let ns_decls = element_ns_decls_from_source(e);
                let items = self.execute_sequence(body, focus)?;
                let (attrs, children) = split_items(items);
                self.push_element_item(
                    out,
                    ResultElement {
                        qname,
                        ns_decls,
                        attrs,
                        children,
                    },
                )?;
            }
            Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) => {
                self.push_text_item(out, t.to_string(), false)?;
            }
            Some(NodeKind::Comment(c)) => {
                self.push_comment_item(out, c.to_string())?;
            }
            Some(NodeKind::ProcessingInstruction(pi)) => {
                self.push_pi_item(
                    out,
                    pi.target.to_string(),
                    pi.data.as_ref().map(|d| d.to_string()).unwrap_or_default(),
                )?;
            }
            Some(NodeKind::Attribute(qn, v)) => {
                self.push_attr_item(
                    out,
                    ResultAttr {
                        namespace_uri: qn.namespace_uri.as_deref().map(str::to_owned),
                        qname: qn.prefixed_name().to_string(),
                        value: v.to_string(),
                    },
                )?;
            }
            None => {}
        }
        Ok(())
    }

    /// `xsl:copy-of` — deep-copy the selected source nodes, the string value of
    /// a scalar, or a result-tree-fragment variable verbatim.
    fn execute_copy_of(
        &mut self,
        select: &ScopedXPath,
        focus: Focus,
        out: &mut Vec<ResultItem>,
    ) -> XmlResult<()> {
        // copy-of of a bare RTF variable copies the fragment, not its string.
        if let Some((prefix, local)) = select.expression.as_variable() {
            if let Some(cloned_bytes) = self
                .lookup_var_ref(prefix, local, &select.namespaces)
                .and_then(|value| {
                    if let VarValue::Rtf(nodes) = value {
                        Some(accounted_node_forest_bytes(nodes))
                    } else {
                        None
                    }
                })
            {
                self.charge_result_bytes(cloned_bytes)?;
                let Some(VarValue::Rtf(nodes)) = self.lookup_var(prefix, local, &select.namespaces)
                else {
                    return Ok(());
                };
                for n in nodes {
                    out.push(ResultItem::Node(n));
                }
                return Ok(());
            }
        }
        let val = self.eval(select, focus)?;
        match val {
            XPathValue::NodeSet(nodes) => {
                for n in nodes {
                    // copy-of of the root node (`/`) copies its children — the
                    // document element plus any top-level comments/PIs — not an
                    // empty node. A document has no single result-item form.
                    if let Some(NodeKind::Document) = self.source.node_kind(n) {
                        for child in self.source.children(n) {
                            out.push(self.deep_copy_source(child)?);
                        }
                    } else {
                        out.push(self.deep_copy_source(n)?);
                    }
                }
            }
            other => self.push_text_item(out, other.to_string_value(self.source), false)?,
        }
        Ok(())
    }

    /// Deep-copy a source node into a result item.
    fn deep_copy_source(&mut self, node: NodeId) -> XmlResult<ResultItem> {
        match self.source.node_kind(node) {
            Some(NodeKind::Element(e)) => {
                let qname = e.name.prefixed_name().to_string();
                let ns_decls = element_ns_decls_from_source(e);
                let mut attrs = Vec::new();
                for attr in &e.attributes {
                    if is_xmlns_attr(attr) {
                        continue;
                    }
                    let result_attr = ResultAttr {
                        namespace_uri: attr.name.namespace_uri.as_deref().map(str::to_owned),
                        qname: attr.name.prefixed_name().to_string(),
                        value: attr.value.to_string(),
                    };
                    self.charge_result_attr(&result_attr)?;
                    attrs.push(result_attr);
                }
                let mut children = Vec::new();
                for child in self.source.children(node) {
                    match self.deep_copy_source(child)? {
                        ResultItem::Node(n) => children.push(n),
                        // Child attributes cannot appear; element children are nodes.
                        ResultItem::Attr(a) => {
                            self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES)?;
                            self.charge_result_str(&a.value)?;
                            children.push(ResultNode::Text {
                                value: a.value,
                                disable_escaping: false,
                            });
                        }
                    }
                }
                let mut element = ResultElement {
                    qname,
                    ns_decls,
                    attrs,
                    children,
                };
                fixup_attribute_namespaces(&mut element, |bytes| self.charge_result_bytes(bytes))?;
                self.charge_result_bytes(
                    RESULT_NODE_ACCOUNTING_BYTES.saturating_add(element.qname.len()),
                )?;
                self.charge_result_ns_decls(&element.ns_decls)?;
                Ok(ResultItem::Node(ResultNode::Element(element)))
            }
            Some(NodeKind::Text(t)) | Some(NodeKind::CData(t)) => {
                self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES)?;
                self.charge_result_str(t)?;
                Ok(ResultItem::Node(ResultNode::Text {
                    value: t.to_string(),
                    disable_escaping: false,
                }))
            }
            Some(NodeKind::Comment(c)) => {
                self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES)?;
                self.charge_result_str(c)?;
                Ok(ResultItem::Node(ResultNode::Comment(c.to_string())))
            }
            Some(NodeKind::ProcessingInstruction(pi)) => {
                let target = pi.target.to_string();
                let data = pi.data.as_ref().map(|d| d.to_string()).unwrap_or_default();
                self.charge_result_bytes(
                    RESULT_NODE_ACCOUNTING_BYTES
                        .saturating_add(target.len())
                        .saturating_add(data.len()),
                )?;
                Ok(ResultItem::Node(ResultNode::Pi { target, data }))
            }
            Some(NodeKind::Attribute(qn, v)) => {
                let attr = ResultAttr {
                    namespace_uri: qn.namespace_uri.as_deref().map(str::to_owned),
                    qname: qn.prefixed_name().to_string(),
                    value: v.to_string(),
                };
                self.charge_result_attr(&attr)?;
                Ok(ResultItem::Attr(attr))
            }
            Some(NodeKind::Document) | None => {
                self.charge_result_bytes(RESULT_NODE_ACCOUNTING_BYTES)?;
                Ok(ResultItem::Node(ResultNode::Text {
                    value: String::new(),
                    disable_escaping: false,
                }))
            }
        }
    }

    /// Compute namespace declarations for a result element built by
    /// `xsl:element` with a (possibly prefixed) computed name, resolving the
    /// prefix against the stylesheet's namespace bindings. A prefixed name whose
    /// prefix is not bound is an error — emitting it would produce output with an
    /// unbound prefix (not well-formed XML).
    fn element_ns_decls(&self, qname: &str) -> XmlResult<Vec<(Option<String>, String)>> {
        if let Some((prefix, _local)) = qname.split_once(':') {
            // The `xml` prefix is implicitly bound and is never declared.
            if prefix == "xml" {
                return Ok(Vec::new());
            }
            match self.stylesheet.namespaces.get(prefix) {
                Some(uri) => return Ok(vec![(Some(prefix.to_string()), uri.clone())]),
                None => {
                    return Err(XmlError::xpath(format!(
                        "xsl:element: prefix {prefix:?} in computed name {qname:?} is not bound to a namespace"
                    )));
                }
            }
        }
        Ok(Vec::new())
    }

    /// Look up the raw (un-coerced) value of a variable in scope.
    fn lookup_var(
        &self,
        prefix: Option<&str>,
        local: &str,
        namespaces: &HashMap<String, String>,
    ) -> Option<VarValue> {
        self.lookup_var_ref(prefix, local, namespaces).cloned()
    }

    fn lookup_var_ref(
        &self,
        prefix: Option<&str>,
        local: &str,
        namespaces: &HashMap<String, String>,
    ) -> Option<&VarValue> {
        let key = expanded_param_name(&var_key(prefix, local), namespaces).ok()?;
        self.locals
            .iter()
            .rev()
            .chain(self.globals.iter().rev())
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    /// Evaluate `xsl:with-param` bindings in the current (caller) context.
    fn eval_params(
        &mut self,
        params: &[WithParam],
        focus: Focus,
    ) -> XmlResult<Vec<(String, VarValue)>> {
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            let val = self.eval_value_source(&p.value, focus)?;
            out.push((p.name.clone(), val));
        }
        Ok(out)
    }

    /// Evaluate a variable/param value source to a [`VarValue`].
    fn eval_value_source(&mut self, vs: &ValueSource, focus: Focus) -> XmlResult<VarValue> {
        match vs {
            ValueSource::Select(expr) => Ok(VarValue::Value(self.eval(expr, focus)?)),
            ValueSource::Body(body) => {
                let items = self.execute_sequence(body, focus)?;
                Ok(VarValue::Rtf(items_to_nodes(items)))
            }
        }
    }

    /// The EXSLT function resolver for this transform (carries the source doc and
    /// the opt-in flag).
    fn funcs(&self) -> ExsltResolver<'a, 'b> {
        ExsltResolver {
            doc: self.source,
            enabled: self.stylesheet.exslt_enabled,
        }
    }

    fn eval(&self, expr: &ScopedXPath, focus: Focus) -> XmlResult<XPathValue> {
        let vars = ScopeResolver {
            locals: &self.locals,
            globals: &self.globals,
            namespaces: &expr.namespaces,
        };
        eval_compiled(
            &expr.expression,
            self.source,
            focus.node,
            focus.node,
            focus.position,
            focus.size,
            &expr.namespaces,
            &vars,
            &self.funcs(),
            DEFAULT_MAX_XPATH_NODE_VISITS,
        )
    }

    fn eval_avt(&self, avt: &Avt, focus: Focus) -> XmlResult<String> {
        let mut s = String::new();
        for part in &avt.parts {
            match part {
                AvtPart::Literal(lit) => s.push_str(lit),
                AvtPart::Expr(expr) => {
                    let val = self.eval(expr, focus)?;
                    s.push_str(&val.to_string_value(self.source));
                }
            }
        }
        Ok(s)
    }
}

/// Keep only the node items from an item list (used for result-tree fragments,
/// where leading attributes have no containing element).
fn items_to_nodes(items: Vec<ResultItem>) -> Vec<ResultNode> {
    items
        .into_iter()
        .filter_map(|it| match it {
            ResultItem::Node(n) => Some(n),
            ResultItem::Attr(_) => None,
        })
        .collect()
}

/// True if a source attribute is an `xmlns`/`xmlns:*` declaration.
fn is_xmlns_attr(attr: &crate::dom::Attribute<'_>) -> bool {
    attr.name.prefix.as_deref() == Some("xmlns")
        || (attr.name.prefix.is_none() && attr.name.local_name.as_ref() == "xmlns")
}

/// Reconstruct the namespace declarations to emit for a copied source element.
fn element_ns_decls_from_source(e: &crate::dom::Element<'_>) -> Vec<(Option<String>, String)> {
    let mut decls: Vec<(Option<String>, String)> = e
        .namespace_declarations
        .iter()
        .map(|(p, u)| {
            let prefix = if p.is_empty() {
                None
            } else {
                Some(p.to_string())
            };
            (prefix, u.to_string())
        })
        .collect();
    // Ensure the element's own namespace is declared.
    if let Some(uri) = e.name.namespace_uri.as_deref() {
        let prefix = e.name.prefix.as_deref().map(|s| s.to_string());
        if !decls
            .iter()
            .any(|(p, u)| p.as_deref() == prefix.as_deref() && u == uri)
        {
            decls.push((prefix, uri.to_string()));
        }
    }
    decls
}

/// EXSLT `date:date-time()` — the current instant as ISO-8601 UTC
/// (`YYYY-MM-DDThh:mm:ssZ`). Uses only `std::time` (no external dependency).
pub(crate) fn exslt_date_time() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hour, min, sec) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, min, sec
    )
}

/// Convert a day count since the Unix epoch to a `(year, month, day)` civil
/// date (Howard Hinnant's algorithm, proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ─── Result serialization ─────────────────────────────────

/// Serializer sink with an optional byte ceiling.
///
/// XSLT permits output amplification: a small stylesheet/source pair can
/// produce a very large serialized document. Keeping the check at the sink
/// makes every serializer path share the same limit, including escaped text,
/// escaped attributes, namespace declarations, comments, and processing
/// instructions. The production default is `None`; fuzz and hostile-input
/// callers can opt in through [`Stylesheet::set_max_output_bytes`].
struct LimitedOutput {
    buf: String,
    max_bytes: Option<usize>,
}

impl LimitedOutput {
    fn new(max_bytes: Option<usize>) -> Self {
        LimitedOutput {
            buf: String::new(),
            max_bytes,
        }
    }

    fn into_string(self) -> String {
        self.buf
    }

    fn push(&mut self, c: char) -> XmlResult<()> {
        let mut encoded = [0; 4];
        self.push_str(c.encode_utf8(&mut encoded))
    }

    fn push_str(&mut self, s: &str) -> XmlResult<()> {
        if let Some(max_bytes) = self.max_bytes {
            // `String::len` is already a byte count. Use saturating arithmetic so
            // a near-`usize::MAX` buffer cannot wrap the comparison.
            if self.buf.len().saturating_add(s.len()) > max_bytes {
                return Err(XmlError::xpath(format!(
                    "XSLT output exceeds maximum serialized size of {} bytes",
                    max_bytes
                )));
            }
        }
        self.buf.push_str(s);
        Ok(())
    }
}

/// A borrowed, parent-linked view of the in-scope namespace declarations during
/// serialization. Mirrors [`crate::dom`]'s `NsScope`: rather than cloning the
/// whole accumulated scope at every element (which is O(depth) string clones per
/// element, quadratic over the tree), each element contributes only the
/// declarations it carries and points at its parent's scope.
struct SerScope<'a> {
    parent: Option<&'a SerScope<'a>>,
    local: &'a [(Option<String>, String)],
}

impl SerScope<'_> {
    /// Compare against the nearest binding; a nearer declaration shadows ancestors.
    fn is_declared(&self, prefix: Option<&str>, uri: &str) -> bool {
        let mut cur = Some(self);
        while let Some(s) = cur {
            if let Some((_, bound_uri)) = s.local.iter().find(|(p, _)| p.as_deref() == prefix) {
                return bound_uri == uri;
            }
            cur = s.parent;
        }
        false
    }
}

fn serialize_result(
    nodes: &[ResultNode],
    out: &mut LimitedOutput,
    opts: &OutputOptions,
) -> XmlResult<()> {
    if !opts.method_text && !opts.omit_xml_declaration {
        out.push_str(&format!(
            "<?xml version=\"1.0\" encoding=\"{}\"?>\n",
            opts.encoding
        ))?;
    }
    // The in-scope namespace bindings accumulated from ancestor elements, used to
    // suppress redundant `xmlns` declarations (a copied element re-declares its
    // namespace even when an ancestor already did).
    let scope = SerScope {
        parent: None,
        local: &[],
    };
    for node in nodes {
        serialize_node(node, out, opts, &scope)?;
    }
    Ok(())
}

fn serialize_node(
    node: &ResultNode,
    out: &mut LimitedOutput,
    opts: &OutputOptions,
    scope: &SerScope<'_>,
) -> XmlResult<()> {
    match node {
        ResultNode::Text {
            value,
            disable_escaping,
        } => {
            if opts.method_text || *disable_escaping {
                out.push_str(value)?;
            } else {
                escape_text(value, out)?;
            }
        }
        ResultNode::Comment(c) => {
            if !opts.method_text {
                out.push_str("<!--")?;
                out.push_str(c)?;
                out.push_str("-->")?;
            }
        }
        ResultNode::Pi { target, data } => {
            if !opts.method_text {
                out.push_str("<?")?;
                out.push_str(target)?;
                if !data.is_empty() {
                    out.push(' ')?;
                    out.push_str(data)?;
                }
                out.push_str("?>")?;
            }
        }
        ResultNode::Element(el) => {
            if opts.method_text {
                // text method: emit only descendant text.
                for child in &el.children {
                    serialize_node(child, out, opts, scope)?;
                }
                return Ok(());
            }
            out.push('<')?;
            out.push_str(&el.qname)?;
            // Emit only namespace declarations not already in scope. The child
            // scope borrows this element's declarations and links to the parent
            // scope — no per-element clone of the inherited bindings.
            let child_scope = SerScope {
                parent: Some(scope),
                local: &el.ns_decls,
            };
            for (i, (prefix, uri)) in el.ns_decls.iter().enumerate() {
                // Already declared by an ancestor, or duplicated earlier on this
                // same element.
                let already = scope.is_declared(prefix.as_deref(), uri)
                    || el.ns_decls[..i]
                        .iter()
                        .any(|(p, u)| p.as_deref() == prefix.as_deref() && u == uri);
                if already {
                    continue;
                }
                match prefix {
                    Some(p) => {
                        out.push_str(" xmlns:")?;
                        out.push_str(p)?;
                        out.push_str("=\"")?;
                    }
                    None => out.push_str(" xmlns=\"")?,
                }
                escape_attr(uri, out)?;
                out.push('"')?;
            }
            for attr in &el.attrs {
                out.push(' ')?;
                out.push_str(&attr.qname)?;
                out.push_str("=\"")?;
                escape_attr(&attr.value, out)?;
                out.push('"')?;
            }
            if el.children.is_empty() {
                out.push_str("/>")?;
            } else {
                out.push('>')?;
                for child in &el.children {
                    serialize_node(child, out, opts, &child_scope)?;
                }
                out.push_str("</")?;
                out.push_str(&el.qname)?;
                out.push('>')?;
            }
        }
    }
    Ok(())
}

fn escape_text(s: &str, out: &mut LimitedOutput) -> XmlResult<()> {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;")?,
            '<' => out.push_str("&lt;")?,
            '>' => out.push_str("&gt;")?,
            _ => out.push(c)?,
        }
    }
    Ok(())
}

fn escape_attr(s: &str, out: &mut LimitedOutput) -> XmlResult<()> {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;")?,
            '<' => out.push_str("&lt;")?,
            '>' => out.push_str("&gt;")?,
            '"' => out.push_str("&quot;")?,
            '\t' => out.push_str("&#x9;")?,
            '\n' => out.push_str("&#xA;")?,
            '\r' => out.push_str("&#xD;")?,
            _ => out.push(c)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! XSLT engine tests, grouped by implementation milestone (M1 = vertical
    //! slice, M2 = recursion + built-in rules). Each test states the XSLT
    //! feature(s) it exercises and why the expected output is what it is —
    //! especially where XSLT semantics (whitespace stripping, built-in rules,
    //! document order) make the result non-obvious.

    use crate::transform;
    use crate::{Parser, Stylesheet};

    /// Run `f` on a thread with a generous (32 MiB) stack. The recursion tests
    /// deliberately drive the engine many activations deep; the default test
    /// harness stack (≈2 MiB) is too small to *observe* the guard firing in debug
    /// builds (per-activation stack cost is large), so we give them headroom. The
    /// guard itself protects production callers on normal stacks at the (lower)
    /// default depth — see `DEFAULT_MAX_XSLT_DEPTH`.
    fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(f)
            .unwrap()
            .join()
            .expect("transform thread must not abort (stack overflow)")
    }

    /// Compile `xslt`, cap recursion at `max_depth`, and transform `xml`.
    fn transform_capped(xslt: &str, xml: &str, max_depth: u32) -> crate::XmlResult<String> {
        let style_doc = Parser::new().parse(xslt)?;
        let sheet = Stylesheet::compile(&style_doc)?.set_max_depth(max_depth);
        let mut source = Parser::new().parse(xml)?;
        source.prepare_xpath();
        sheet.transform(&source)
    }

    /// Compile and transform with the opt-in EXSLT library enabled.
    fn transform_exslt(xslt: &str, xml: &str) -> crate::XmlResult<String> {
        let style_doc = Parser::new().parse(xslt)?;
        let sheet = Stylesheet::compile(&style_doc)?.with_exslt(true);
        let mut source = Parser::new().parse(xml)?;
        source.prepare_xpath();
        sheet.transform(&source)
    }

    /// M1 — the architecture-proving slice. A single `match="/"` template, one
    /// literal result element (`<out>`), and an `xsl:value-of` selecting an
    /// attribute from the source. Exercises the whole spine: stylesheet parse →
    /// root match → instruction execution → XPath `select` against the source →
    /// result-tree build → serialize. `omit-xml-declaration="yes"` keeps the
    /// output to just the element so the assertion is exact.
    #[test]
    fn m1_value_of_attribute() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/">
                <out><xsl:value-of select="/greeting/@to"/></out>
            </xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<greeting to="world">hi</greeting>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>world</out>");
    }

    /// M1 — attribute value templates and literal text. The `id="{/doc/@n}"`
    /// attribute is an AVT: the `{...}` is evaluated as XPath and its string
    /// value substituted, while `static` is literal element content copied
    /// verbatim. Confirms AVT evaluation in a literal-result-element attribute.
    #[test]
    fn m1_literal_text_and_avt() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/">
                <result id="{/doc/@n}">static</result>
            </xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<doc n="7"/>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<result id="7">static</result>"#);
    }

    /// M1 — `xsl:output method="text"`. With the text method the serializer
    /// emits no XML declaration, no markup, and does not escape — only the
    /// string value produced by the template. Here that is the text of `/a/b`.
    #[test]
    fn m1_text_method() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="text"/>
            <xsl:template match="/">
                <xsl:value-of select="/a/b"/>
            </xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<a><b>hello</b></a>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "hello");
    }

    /// M2 — recursive dispatch through `xsl:apply-templates` and a built-in
    /// rule. The `match="/"` template wraps `<list>` around
    /// `<xsl:apply-templates/>`, which (with no `select`) processes the
    /// children of the document root — i.e. the `<root>` element. No template
    /// matches `<root>`, so the *built-in element rule* fires and applies
    /// templates to `<root>`'s children, dispatching each `<item>` to the
    /// `match="item"` template. Demonstrates default-select, built-in element
    /// rule, and depth recursion in one pass.
    #[test]
    fn m2_apply_templates_recursion() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><list><xsl:apply-templates/></list></xsl:template>
            <xsl:template match="item"><got><xsl:value-of select="."/></got></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<root><item>a</item><item>b</item></root>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<list><got>a</got><got>b</got></list>");
    }

    /// M2 — the built-in text rule. Only a `match="/"` template exists; there
    /// is no rule for `<doc>`, `<b>`, or text nodes. apply-templates recurses
    /// via the built-in element rule, and the built-in text rule copies each
    /// text node's string value through. The `<b>` element's start/end tags are
    /// NOT emitted (no template produces them) — only its text survives, so
    /// "hello " + "world" => "hello world".
    #[test]
    fn m2_builtin_text_rule() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<doc>hello <b>world</b></doc>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>hello world</out>");
    }

    /// M2 — `xsl:apply-templates` with an explicit `select`. `select="//x"`
    /// gathers only the `<x>` elements (document order, skipping `<y>`), each
    /// dispatched to the `match="x"` template which wraps its value in
    /// brackets. Confirms select-driven node lists and that non-selected
    /// siblings are not processed.
    #[test]
    fn m2_apply_templates_select() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="//x"/></out></xsl:template>
            <xsl:template match="x">[<xsl:value-of select="."/>]</xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><x>1</x><y>2</y><x>3</x></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>[1][3]</out>");
    }

    /// M2 — `position()` / `last()` reflect the apply-templates node list. Three
    /// `<i>` elements are processed; each emits `position()/last()`. The trailing
    /// space is wrapped in `<xsl:text> </xsl:text>` deliberately: a bare " "
    /// text node in the stylesheet is whitespace-only and would be stripped at
    /// compile time, whereas xsl:text content is always preserved. This both
    /// checks positional context and documents the whitespace-stripping rule.
    #[test]
    fn m2_position_last() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r/i"/></out></xsl:template>
            <xsl:template match="i"><xsl:value-of select="position()"/>/<xsl:value-of select="last()"/><xsl:text> </xsl:text></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><i/><i/><i/></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>1/3 2/3 3/3 </out>");
    }

    /// M3 — multi-step patterns and conflict resolution by priority. A bare
    /// `b` (default priority 0) and a two-step `a/b` (default priority 0.5)
    /// both match the inner `<b>`; the more specific `a/b` wins. The top-level
    /// `<b>` (no `a` parent) only matches `b`, so it takes the generic rule.
    #[test]
    fn m3_pattern_priority_specificity() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="//b"/></out></xsl:template>
            <xsl:template match="b">[generic]</xsl:template>
            <xsl:template match="a/b">[specific]</xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><b/><a><b/></a></r>"#;
        // First <b> is a child of <r>: only "b" matches → generic.
        // Second <b> is a child of <a>: both match, "a/b" (0.5) wins → specific.
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>[generic][specific]</out>");
    }

    /// M3 — a union pattern (`x|y`) in a single template's `match`. Each
    /// alternative dispatches to the same body; both `<x>` and `<y>` are
    /// handled by one rule.
    #[test]
    fn m3_union_pattern() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r/*"/></out></xsl:template>
            <xsl:template match="x|y">(<xsl:value-of select="name()"/>)</xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><x/><y/><z/></r>"#;
        // <x> and <y> match the union; <z> falls to the built-in element rule
        // (which produces nothing here, as <z> has no text).
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>(x)(y)</out>");
    }

    /// M3 — an attribute pattern (`@*`) plus an attribute-axis apply-templates.
    /// `select="@*"` produces the element's attribute nodes; the `match="@*"`
    /// template emits each attribute's name and value.
    #[test]
    fn m3_attribute_pattern() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="e/@*"/></out></xsl:template>
            <xsl:template match="@*"><xsl:value-of select="name()"/>=<xsl:value-of select="."/>;</xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<e a="1" b="2"/>"#;
        // ';' separates each attribute (a literal char survives stripping).
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<out>a=1;b=2;</out>"#);
    }

    /// M3 — a predicate in a match pattern. `item[@k='2']` matches only the
    /// `<item>` whose `k` attribute is `2`; the others fall to the built-in
    /// rule. Confirms predicate evaluation reuses the XPath engine correctly.
    #[test]
    fn m3_predicate_pattern() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
            <xsl:template match="item[@k='2']">HIT</xsl:template>
            <xsl:template match="item"/>
        </xsl:stylesheet>"#;
        let xml = r#"<r><item k="1"/><item k="2"/><item k="3"/></r>"#;
        // The empty match="item" template swallows non-matching items; only the
        // k='2' item (higher priority via predicate) emits HIT.
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>HIT</out>");
    }

    /// M3 — namespaced name-test patterns. The pattern prefix (`m:`) resolves
    /// against the stylesheet's namespace bindings, matching the source element
    /// by namespace URI + local name regardless of the source's own prefix.
    #[test]
    fn m3_namespaced_pattern() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:m="urn:m">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="//*"/></out></xsl:template>
            <xsl:template match="m:item">M</xsl:template>
            <xsl:template match="*"/>
        </xsl:stylesheet>"#;
        // Source uses prefix `p` for the same URI; matching is by URI not prefix.
        let xml = r#"<root xmlns:p="urn:m"><p:item/><other/></root>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>M</out>");
    }

    /// M4 — `xsl:if`. The body is emitted only when the test is true; the false
    /// branch contributes nothing (there is no else — that's `xsl:choose`).
    #[test]
    fn m4_if() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r/i"/></out></xsl:template>
            <xsl:template match="i"><xsl:if test=". &gt; 1">[<xsl:value-of select="."/>]</xsl:if></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><i>1</i><i>2</i><i>3</i></r>"#;
        // Only items with value > 1 emit brackets.
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>[2][3]</out>");
    }

    /// M4 — `xsl:choose`/`when`/`otherwise`. The first true `when` wins;
    /// `otherwise` is the fallback. Classic sign classifier.
    #[test]
    fn m4_choose() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r/n"/></out></xsl:template>
            <xsl:template match="n"><xsl:choose>
                <xsl:when test=". &lt; 0">neg</xsl:when>
                <xsl:when test=". = 0">zero</xsl:when>
                <xsl:otherwise>pos</xsl:otherwise>
            </xsl:choose>;</xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><n>-5</n><n>0</n><n>7</n></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>neg;zero;pos;</out>");
    }

    /// M4 — `xsl:for-each` with positional context. Iterates the selected nodes,
    /// setting the focus (and `position()`) for each; no template dispatch.
    #[test]
    fn m4_for_each() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:for-each select="r/x"><xsl:value-of select="position()"/>:<xsl:value-of select="."/>;</xsl:for-each></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><x>a</x><x>b</x></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>1:a;2:b;</out>");
    }

    /// M4 — `xsl:variable` (select form) referenced via `$name`. The variable is
    /// bound once and reused; scoping is to following siblings in the template.
    #[test]
    fn m4_variable_select() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/">
                <xsl:variable name="n" select="count(//item)"/>
                <out count="{$n}"><xsl:value-of select="$n"/></out>
            </xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><item/><item/><item/></r>"#;
        // $n is used both in an AVT and as a value-of; both see the same binding.
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<out count="3">3</out>"#);
    }

    /// M4 — `xsl:variable` body form (a result-tree fragment). Its string value
    /// (used by `value-of`) is the concatenation of the fragment's text.
    #[test]
    fn m4_variable_rtf_string_value() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/">
                <xsl:variable name="frag"><a>x</a><b>y</b></xsl:variable>
                <out><xsl:value-of select="$frag"/></out>
            </xsl:template>
        </xsl:stylesheet>"#;
        let result = transform(xslt, "<r/>").unwrap();
        assert_eq!(result, "<out>xy</out>");
    }

    /// M4 — `xsl:copy-of` of a result-tree-fragment variable copies the fragment
    /// verbatim (not its string value), exercising the bare-`$var` special case.
    #[test]
    fn m4_copy_of_rtf_variable() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/">
                <xsl:variable name="frag"><inner a="1">deep</inner></xsl:variable>
                <out><xsl:copy-of select="$frag"/></out>
            </xsl:template>
        </xsl:stylesheet>"#;
        let result = transform(xslt, "<r/>").unwrap();
        assert_eq!(result, r#"<out><inner a="1">deep</inner></out>"#);
    }

    /// M4 — the identity-style transform: `xsl:copy` of the current node plus
    /// `apply-templates select="@*|node()"`, with `match="@*"` doing `xsl:copy`.
    /// Reproduces a source subtree (attributes + children) into the result.
    #[test]
    fn copied_attributes_preserve_namespace() {
        use crate::dom::NodeKind;
        for body in [
            r#"<xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy>"#,
            r#"<v><xsl:copy-of select="@*|node()"/></v>"#,
            r#"<xsl:copy-of select="."/>"#,
            r#"<v xmlns:p="urn:other"><xsl:copy-of select="@*|node()"/></v>"#,
        ] {
            let style = format!(
                r#"<xsl:stylesheet version="1.0"
                xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                <xsl:template match="/root"><root><xsl:apply-templates/></root></xsl:template>
                <xsl:template match="v">{body}</xsl:template>
                <xsl:template match="@*|text()"><xsl:copy/></xsl:template>
                </xsl:stylesheet>"#
            );
            let output = transform(
                &style,
                r#"<root xmlns:p="urn:source"><v p:type="string">1</v></root>"#,
            )
            .unwrap();
            let doc = Parser::new().parse(&output).unwrap();
            let root = doc.document_element().unwrap();
            let child = doc
                .children(root)
                .into_iter()
                .find(|id| matches!(doc.node_kind(*id), Some(NodeKind::Element(_))))
                .unwrap();
            let Some(NodeKind::Element(element)) = doc.node_kind(child) else {
                panic!()
            };
            let attr = element
                .attributes
                .iter()
                .find(|a| a.name.local_name == "type")
                .unwrap();
            assert_eq!(attr.name.namespace_uri.as_deref(), Some("urn:source"));
            assert_eq!(attr.value, "string");
        }
    }

    #[test]
    fn m4_identity_copy() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="@*|node()"><xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<a x="1"><b>t</b></a>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<a x="1"><b>t</b></a>"#);
    }

    /// M4 — `xsl:copy-of` of selected source nodes (deep copy).
    #[test]
    fn m4_copy_of_nodes() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:copy-of select="//keep"/></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><keep id="1"><c/></keep><drop/></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<out><keep id="1"><c/></keep></out>"#);
    }

    /// `xsl:copy-of select="/"` copies the document root's children (the document
    /// element), not an empty node — a node-set may include the `Document` node.
    #[test]
    fn copy_of_root_copies_document_element() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:copy-of select="/"/></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r a="1"><c>t</c></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<out><r a="1"><c>t</c></r></out>"#);
    }

    /// `xsl:text` concatenates all of its text/CDATA children, not just the first
    /// segment, so a value split across text + CDATA is emitted whole.
    #[test]
    fn xsl_text_concatenates_all_segments() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out><xsl:text>a<![CDATA[b]]>c</xsl:text></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r/>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, r#"<out>abc</out>"#);
    }

    /// An unsupported `xsl:output/@method` (e.g. `html`) is rejected at compile
    /// time rather than silently serialized as XML.
    #[test]
    fn output_method_html_errors() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="html"/>
            <xsl:template match="/"><out/></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r/>"#;
        assert!(transform(xslt, xml).is_err());
    }

    /// A top-level `xsl:param` can be supplied externally via `with_param`,
    /// overriding its default; the value is visible to the templates.
    #[test]
    fn with_param_overrides_top_level_param() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:param name="publisher">default</xsl:param>
            <xsl:template match="/"><out><xsl:value-of select="$publisher"/></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r/>"#;

        // Default (no override) uses the param's body value.
        let style_doc = crate::Parser::new().parse(xslt).unwrap();
        let mut src = crate::Parser::new().parse(xml).unwrap();
        src.prepare_xpath();
        let default_out = Stylesheet::compile(&style_doc)
            .unwrap()
            .transform(&src)
            .unwrap();
        assert_eq!(default_out, r#"<out>default</out>"#);

        // Override replaces it.
        let overridden = Stylesheet::compile(&style_doc)
            .unwrap()
            .with_param("publisher", "ACME")
            .transform(&src)
            .unwrap();
        assert_eq!(overridden, r#"<out>ACME</out>"#);
    }

    /// `xsl:element` with a prefixed name whose prefix is not bound in the
    /// stylesheet is a hard error, not silent unbound-prefix output.
    #[test]
    fn element_unbound_prefix_errors() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><xsl:element name="nope:thing"/></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r/>"#;
        assert!(transform(xslt, xml).is_err());
    }

    /// A literal result element's explicit namespace declarations (here `ex`,
    /// used only by a child) are preserved so the output re-parses.
    #[test]
    fn literal_element_preserves_extra_ns_decls() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out xmlns:ex="urn:ex"><ex:child>v</ex:child></out></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r/>"#;
        let result = transform(xslt, xml).unwrap();
        // The `ex` prefix must be declared in the output for `ex:child` to resolve.
        assert!(
            result.contains("xmlns:ex=\"urn:ex\""),
            "missing ex declaration: {result}"
        );
        // And the result must re-parse as well-formed, namespace-correct XML.
        crate::Parser::new()
            .parse(&result)
            .expect("re-parse output");
    }

    /// A lone `}` in an attribute value template is a static error; a literal
    /// right brace must be written as `}}`.
    #[test]
    fn avt_lone_close_brace_is_error() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out id="a}b"/></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r/>"#;
        assert!(transform(xslt, xml).is_err());
    }

    /// M4 — `xsl:element` with a literal qualified name and `xsl:attribute` with
    /// an AVT name. The element's prefix is declared from the stylesheet's
    /// namespace bindings; the attribute attaches to the enclosing element.
    #[test]
    fn m4_element_and_attribute() {
        // Kept inline: a text node carrying non-whitespace ("body") is preserved
        // verbatim including any surrounding whitespace, so the body is written
        // tight against the element to keep the assertion exact.
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:md="urn:md">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><xsl:element name="md:Item"><xsl:attribute name="id"><xsl:value-of select="/r/@n"/></xsl:attribute>body</xsl:element></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r n="9"/>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(
            result,
            r#"<md:Item xmlns:md="urn:md" id="9">body</md:Item>"#
        );
    }

    /// M4 — `xsl:call-template` with `xsl:with-param`, and `xsl:param` defaults.
    /// The call passes `who`; `punct` falls back to its declared default.
    #[test]
    fn m4_call_template_params() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:template match="/"><out>
                <xsl:call-template name="greet">
                    <xsl:with-param name="who" select="'world'"/>
                </xsl:call-template>
            </out></xsl:template>
            <xsl:template name="greet">
                <xsl:param name="who" select="'nobody'"/>
                <xsl:param name="punct" select="'!'"/>
                <xsl:value-of select="concat('hi ', $who, $punct)"/>
            </xsl:template>
        </xsl:stylesheet>"#;
        let result = transform(xslt, "<r/>").unwrap();
        assert_eq!(result, "<out>hi world!</out>");
    }

    /// M4 — recursive named-template descent passing an accumulating param,
    /// mirroring the pp.xsl indentation pattern (`with-param` carrying an
    /// ever-growing indent string through `apply-templates`).
    #[test]
    fn m4_recursive_with_param() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:apply-templates select="*"><xsl:with-param name="d" select="'0'"/></xsl:apply-templates></xsl:template>
            <xsl:template match="*">
                <xsl:param name="d"/>
                <xsl:value-of select="concat($d, name(), ';')"/>
                <xsl:apply-templates select="*"><xsl:with-param name="d" select="concat($d, '-')"/></xsl:apply-templates>
            </xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<a><b><c/></b></a>"#;
        // depth string grows by '-' each level: a at "0", b at "0-", c at "0--".
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "0a;0-b;0--c;");
    }

    /// M4 — a global `xsl:param` overridable in principle, used across templates.
    /// Globals are evaluated once and visible everywhere.
    #[test]
    fn m4_global_variable() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:variable name="sep" select="'|'"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r/i"/></out></xsl:template>
            <xsl:template match="i"><xsl:value-of select="."/><xsl:value-of select="$sep"/></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><i>a</i><i>b</i></r>"#;
        let result = transform(xslt, xml).unwrap();
        assert_eq!(result, "<out>a|b|</out>");
    }

    #[test]
    fn result_tree_size_cap_errors() {
        // This cap stops amplification while the transform is still building
        // result nodes, before serialization has a chance to reject the final
        // string. The accounting is conservative: node overhead plus payload
        // bytes both count.
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="text"/>
            <xsl:template match="/">abcdefghijklmnopqrstuvwxyz</xsl:template>
        </xsl:stylesheet>"#;
        let style_doc = Parser::new().parse(xslt).unwrap();
        let stylesheet = Stylesheet::compile(&style_doc)
            .unwrap()
            .set_max_result_tree_bytes(8);
        let mut source = Parser::new().parse("<r/>").unwrap();
        source.prepare_xpath();

        let err = stylesheet
            .transform(&source)
            .expect_err("result-tree cap must reject oversized result");
        assert!(err
            .to_string()
            .contains("XSLT result tree exceeds maximum materialized size of 8 bytes"));
    }

    #[test]
    fn output_size_cap_errors() {
        // Hostile stylesheets can turn tiny inputs into very large serialized
        // outputs. The cap is checked by the serializer sink, so this test
        // covers the public opt-in API and the shared error path.
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="text"/>
            <xsl:template match="/">abcdefghijklmnopqrstuvwxyz</xsl:template>
        </xsl:stylesheet>"#;
        let style_doc = Parser::new().parse(xslt).unwrap();
        let stylesheet = Stylesheet::compile(&style_doc)
            .unwrap()
            .set_max_output_bytes(8);
        let mut source = Parser::new().parse("<r/>").unwrap();
        source.prepare_xpath();

        let err = stylesheet
            .transform(&source)
            .expect_err("output cap must reject oversized result");
        assert!(err
            .to_string()
            .contains("XSLT output exceeds maximum serialized size of 8 bytes"));
    }

    /// M5 — `xsl:strip-space elements="*"` drops whitespace-only text between
    /// elements in the source, so the default child traversal does not emit it.
    /// Without stripping, the indentation between `<i>` elements would surface
    /// via the built-in text rule.
    #[test]
    fn m5_strip_space() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:strip-space elements="*"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r/node()"/></out></xsl:template>
            <xsl:template match="i">[<xsl:value-of select="."/>]</xsl:template>
        </xsl:stylesheet>"#;
        // Note: select="r/node()" includes the whitespace text between <i>s, but
        // the default apply-templates inside is what we strip; here the explicit
        // select picks node() so we instead verify via default traversal:
        let xslt2 = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" omit-xml-declaration="yes"/>
            <xsl:strip-space elements="*"/>
            <xsl:template match="/"><out><xsl:apply-templates select="r"/></out></xsl:template>
            <xsl:template match="r"><xsl:apply-templates/></xsl:template>
            <xsl:template match="i">[<xsl:value-of select="."/>]</xsl:template>
        </xsl:stylesheet>"#;
        let _ = xslt; // first form documented but the second exercises stripping
        let xml = "<r>\n  <i>a</i>\n  <i>b</i>\n</r>";
        let result = transform(xslt2, xml).unwrap();
        // The newlines/indentation between <i> elements are stripped.
        assert_eq!(result, "<out>[a][b]</out>");
    }

    /// M5 — EXSLT `date:date-time()` resolves through the function seam and
    /// produces a syntactically valid ISO-8601 UTC timestamp. The exact instant
    /// is time-dependent, so we assert the shape, not the value.
    #[test]
    fn m5_exslt_date_time() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:date="http://exslt.org/dates-and-times">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:value-of select="date:date-time()"/></xsl:template>
        </xsl:stylesheet>"#;
        let result = transform(xslt, "<r/>").unwrap();
        // Shape: YYYY-MM-DDThh:mm:ssZ
        assert_eq!(result.len(), 20, "got {:?}", result);
        assert!(result.ends_with('Z'));
        assert_eq!(&result[4..5], "-");
        assert_eq!(&result[10..11], "T");
    }

    /// DoS guard — a self-recursive named template with no base case must return
    /// a graceful error, not overflow the stack (which would abort the process
    /// with an uncatchable `SIGABRT`). Confirmed pre-fix to abort; the depth cap
    /// converts it to an `XmlError`.
    #[test]
    fn dos_call_template_infinite_recursion_errors() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:template match="/"><xsl:call-template name="loop"/></xsl:template>
            <xsl:template name="loop"><xsl:call-template name="loop"/></xsl:template>
        </xsl:stylesheet>"#;
        let err = on_big_stack(|| transform_capped(xslt, "<r/>", 64)).unwrap_err();
        assert!(
            format!("{err}").contains("recursion limit"),
            "expected recursion-limit error, got: {err}"
        );
    }

    /// DoS guard — an `apply-templates select="."` cycle (the root template
    /// re-dispatching to the root node) must also error gracefully rather than
    /// recurse forever.
    #[test]
    fn dos_apply_templates_self_cycle_errors() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:template match="/"><xsl:apply-templates select="."/></xsl:template>
        </xsl:stylesheet>"#;
        let err = on_big_stack(|| transform_capped(xslt, "<r/>", 64)).unwrap_err();
        assert!(
            format!("{err}").contains("recursion limit"),
            "expected recursion-limit error, got: {err}"
        );
    }

    /// EXSLT — `math:` scalar and node-set aggregate functions resolve when the
    /// library is enabled. `math:max` reads the numeric string-values of the
    /// selected nodes; `math:power` is a scalar computation.
    #[test]
    fn exslt_math() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:math="http://exslt.org/math">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:value-of select="math:max(//n)"/>,<xsl:value-of select="math:power(2,10)"/></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><n>3</n><n>41</n><n>7</n></r>"#;
        assert_eq!(transform_exslt(xslt, xml).unwrap(), "41,1024");
    }

    /// EXSLT — `math:constant` accepts both the intuitive `SQRT2` spelling and
    /// the EXSLT-spec `SQRRT2` typo, rounded to the requested significant figures.
    #[test]
    fn exslt_math_constant_sqrt2() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:math="http://exslt.org/math">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:value-of select="math:constant('SQRT2', 4)"/>,<xsl:value-of select="math:constant('SQRRT2', 4)"/></xsl:template>
        </xsl:stylesheet>"#;
        assert_eq!(transform_exslt(xslt, "<r/>").unwrap(), "1.414,1.414");
    }

    /// `xsl:param` after other template content is an error (XSLT 1.0 requires
    /// params first); insignificant whitespace before it does not count.
    #[test]
    fn param_after_content_errors() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:template match="/"><foo/><xsl:param name="x"/></xsl:template>
        </xsl:stylesheet>"#;
        assert!(transform(xslt, "<r/>").is_err());
    }

    /// A non-UTF-8 `xsl:output/@encoding` is not honored: the result is a UTF-8
    /// string, so the declaration must say UTF-8 rather than mislabel the bytes.
    #[test]
    fn output_encoding_forced_utf8() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="xml" encoding="ISO-8859-1"/>
            <xsl:template match="/"><r/></xsl:template>
        </xsl:stylesheet>"#;
        let out = transform(xslt, "<x/>").unwrap();
        assert!(out.contains(r#"encoding="UTF-8"#), "got {out}");
        assert!(!out.contains("ISO-8859-1"), "got {out}");
    }

    /// EXSLT — `str:padding` builds a fixed-length string from a repeated pad,
    /// and `str:align` right-aligns within a field width.
    #[test]
    fn exslt_str() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:str="http://exslt.org/strings">
            <xsl:output method="text"/>
            <xsl:template match="/">[<xsl:value-of select="str:padding(4, '*')"/>][<xsl:value-of select="str:align('hi', '12345', 'right')"/>]</xsl:template>
        </xsl:stylesheet>"#;
        assert_eq!(transform_exslt(xslt, "<r/>").unwrap(), "[****][   hi]");
    }

    /// EXSLT — `set:distinct` keeps the first node of each distinct string-value;
    /// `count()` of the result confirms duplicates collapsed.
    #[test]
    fn exslt_set_distinct() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:set="http://exslt.org/sets">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:value-of select="count(set:distinct(//c))"/></xsl:template>
        </xsl:stylesheet>"#;
        let xml = r#"<r><c>a</c><c>b</c><c>a</c><c>b</c><c>c</c></r>"#;
        assert_eq!(transform_exslt(xslt, xml).unwrap(), "3");
    }

    /// EXSLT — the broader library is opt-in: without `with_exslt`, an unbound
    /// extension function is an error (whereas `date:date-time()` always works).
    #[test]
    fn exslt_disabled_by_default() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:math="http://exslt.org/math">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:value-of select="math:abs(-5)"/></xsl:template>
        </xsl:stylesheet>"#;
        // Plain `transform` (no with_exslt) → math: unresolved → error.
        assert!(transform(xslt, "<r/>").is_err());
        // Enabled → resolves.
        assert_eq!(transform_exslt(xslt, "<r/>").unwrap(), "5");
    }

    /// The guard must not penalize legitimate deep-but-terminating recursion: a
    /// count-down recursing 300 levels (below the default cap) completes. This is
    /// far deeper than any structural recursion over real input (the parser caps
    /// source nesting at 128) and stands in for genuinely recursive stylesheets.
    #[test]
    fn deep_terminating_recursion_succeeds() {
        let xslt = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:output method="text"/>
            <xsl:template match="/"><xsl:call-template name="d"><xsl:with-param name="i" select="300"/></xsl:call-template></xsl:template>
            <xsl:template name="d"><xsl:param name="i"/>
              <xsl:if test="$i &gt; 0">.<xsl:call-template name="d"><xsl:with-param name="i" select="$i - 1"/></xsl:call-template></xsl:if>
            </xsl:template>
        </xsl:stylesheet>"#;
        let out = on_big_stack(|| transform(xslt, "<r/>")).unwrap();
        assert_eq!(out.len(), 300, "expected 300 dots, got {} chars", out.len());
    }
}
