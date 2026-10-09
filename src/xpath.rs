//! XPath 1.0 evaluation engine.
//!
//! Implements a subset of XPath 1.0 sufficient for XML-DSig `<Transform>`
//! elements, including:
//!
//! - Abbreviated and unabbreviated axis steps
//! - Predicates with numeric and boolean expressions
//! - Core XPath functions: `text()`, `comment()`, `processing-instruction()`,
//!   `node()`, `last()`, `position()`, `count()`, `local-name()`, `namespace-uri()`,
//!   `name()`, `string()`, `concat()`, `starts-with()`, `contains()`,
//!   `string-length()`, `normalize-space()`, `not()`, `true()`, `false()`,
//!   `number()`, `sum()`, `boolean()`
//! - Axes: `child`, `descendant`, `parent`, `ancestor`, `self`,
//!   `descendant-or-self`, `ancestor-or-self`, `following-sibling`,
//!   `preceding-sibling`, `following`, `preceding`, `attribute`, `namespace`
//! - Operators: `=`, `!=`, `<`, `>`, `<=`, `>=`, `and`, `or`, `+`, `-`, `*`,
//!   `div`, `mod`, `|`

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{XmlError, XmlResult};

/// The result of evaluating an XPath expression.
#[derive(Debug, Clone)]
pub enum XPathValue {
    /// An ordered set of nodes (document order, no duplicates).
    NodeSet(Vec<NodeId>),
    /// A boolean value.
    Boolean(bool),
    /// A floating-point number.
    Number(f64),
    /// A string value.
    String(String),
}

impl XPathValue {
    /// Coerce to boolean per XPath 1.0 rules.
    pub fn to_boolean(&self) -> bool {
        match self {
            XPathValue::Boolean(b) => *b,
            XPathValue::Number(n) => *n != 0.0 && !n.is_nan(),
            XPathValue::String(s) => !s.is_empty(),
            XPathValue::NodeSet(nodes) => !nodes.is_empty(),
        }
    }

    /// Coerce to number per XPath 1.0 rules.
    pub fn to_number(&self, doc: &Document<'_>) -> f64 {
        match self {
            XPathValue::Number(n) => *n,
            XPathValue::Boolean(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            XPathValue::String(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
            XPathValue::NodeSet(_) => {
                let s = self.to_string_value(doc);
                s.trim().parse::<f64>().unwrap_or(f64::NAN)
            }
        }
    }

    /// Coerce to string per XPath 1.0 rules.
    pub fn to_string_value(&self, doc: &Document<'_>) -> String {
        match self {
            XPathValue::String(s) => s.clone(),
            XPathValue::Boolean(b) => {
                if *b {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            XPathValue::Number(n) => {
                if n.is_nan() {
                    "NaN".to_string()
                } else if n.is_infinite() {
                    if *n > 0.0 {
                        "Infinity".to_string()
                    } else {
                        "-Infinity".to_string()
                    }
                } else if *n == 0.0 {
                    "0".to_string()
                } else if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            XPathValue::NodeSet(nodes) => {
                if let Some(&first) = nodes.first() {
                    string_value_of_node(doc, first)
                } else {
                    String::new()
                }
            }
        }
    }

    /// Get the node set, or an empty vec.
    pub fn as_node_set(&self) -> &[NodeId] {
        match self {
            XPathValue::NodeSet(nodes) => nodes,
            _ => &[],
        }
    }
}

/// Get the string-value of a node per XPath 1.0 rules.
pub(crate) fn string_value_of_node(doc: &Document<'_>, id: NodeId) -> String {
    match doc.node_kind(id) {
        Some(NodeKind::Document) | Some(NodeKind::Element(_)) => doc.text_content_deep(id),
        Some(NodeKind::Text(t)) => t.to_string(),
        Some(NodeKind::CData(t)) => t.to_string(),
        Some(NodeKind::Comment(c)) => c.to_string(),
        Some(NodeKind::ProcessingInstruction(pi)) => {
            pi.data.as_ref().map(|d| d.to_string()).unwrap_or_default()
        }
        Some(NodeKind::Attribute(_, v)) => v.to_string(),
        None => String::new(),
    }
}

/// Resolves XPath variable references (`$name`) to values.
///
/// Plain XPath has no variables, so [`XPathEvaluator`] uses a no-op resolver
/// that leaves every reference undefined. A host language layered on top of the
/// engine — notably the XSLT transformer (`crate::xslt`) — supplies a real
/// implementation backed by its variable scope stack.
pub trait VariableResolver {
    /// Resolve `$prefix:local` (or `$local` when `prefix` is `None`) to a value,
    /// or `None` if no such variable is in scope.
    fn resolve_variable(&self, prefix: Option<&str>, local: &str) -> Option<XPathValue>;
}

/// Resolves XPath function calls the core engine does not implement itself.
///
/// The built-in XPath 1.0 functions are dispatched directly; any other call
/// (e.g. an EXSLT extension such as `date:date-time()`) falls through to this
/// hook before erroring. Arguments are evaluated to values before the call.
pub trait FunctionResolver {
    /// Resolve `prefix:local(args)` to a value. Return `None` to fall through to
    /// the standard "unknown function" error.
    fn resolve_function(
        &self,
        prefix: Option<&str>,
        local: &str,
        args: &[XPathValue],
    ) -> Option<XmlResult<XPathValue>>;
}

/// A [`VariableResolver`] that defines no variables (the plain-XPath default).
struct NoVariables;
impl VariableResolver for NoVariables {
    fn resolve_variable(&self, _prefix: Option<&str>, _local: &str) -> Option<XPathValue> {
        None
    }
}

/// A [`FunctionResolver`] that resolves no extension functions (the plain-XPath
/// default).
struct NoFunctions;
impl FunctionResolver for NoFunctions {
    fn resolve_function(
        &self,
        _prefix: Option<&str>,
        _local: &str,
        _args: &[XPathValue],
    ) -> Option<XmlResult<XPathValue>> {
        None
    }
}

/// Split a (possibly prefixed) QName into `(Some(prefix), local)` or
/// `(None, local)`. Only the first colon is significant.
fn split_qname(name: &str) -> (Option<&str>, &str) {
    match name.split_once(':') {
        Some((p, l)) => (Some(p), l),
        None => (None, name),
    }
}

const CACHE_LIMIT: usize = 128;
const CACHE_SOURCE_LIMIT: usize = 4 * 1024;
const CACHE_ENTRY_BYTES: usize = 64 * 1024;
const CACHE_TOTAL_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct ProgramCache {
    programs: HashMap<String, std::sync::Arc<Expr>>,
    retained_bytes: usize,
}

impl ProgramCache {
    fn insert(&mut self, source: &str, program: std::sync::Arc<Expr>) {
        if source.len() > CACHE_SOURCE_LIMIT
            || self.programs.len() >= CACHE_LIMIT
            || self.programs.contains_key(source)
        {
            return;
        }
        let key = source.to_owned();
        // Count allocated capacities, including the root Arc payload/counters.
        // Hash-table metadata is separately bounded by CACHE_LIMIT.
        let bytes = key
            .capacity()
            .saturating_add(std::mem::size_of::<Expr>())
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(program.heap_bytes());
        if bytes > CACHE_ENTRY_BYTES
            || bytes > CACHE_TOTAL_BYTES.saturating_sub(self.retained_bytes)
        {
            return;
        }
        self.programs.insert(key, program);
        self.retained_bytes += bytes;
    }
}

/// The XPath evaluator.
pub struct XPathEvaluator {
    /// Namespace prefix mappings for XPath expressions.
    namespaces: HashMap<String, String>,
    /// Maximum expression-nesting depth enforced by the parser. Defaults
    /// to [`DEFAULT_MAX_XPATH_DEPTH`]; override via [`Self::with_max_depth`].
    max_depth: u32,
    /// Maximum number of axis/predicate node visits per evaluation.
    max_node_visits: usize,
    /// Compiled-expression cache, keyed by expression text.
    ///
    /// Tokenizing + parsing an XPath expression is a significant fraction of
    /// evaluation cost, so repeated evaluation of the same string through one
    /// evaluator skips it. Bounded: at the cap new expressions are evaluated
    /// without being cached (the cache is an advisory optimization).
    ///
    /// `Mutex` + `Arc` rather than `RefCell` + `Rc`: the evaluator is wrapped
    /// in a pyclass by pyuppsala, and pyclasses must be `Send + Sync`.
    program_cache: std::sync::Mutex<ProgramCache>,
}

impl XPathEvaluator {
    /// Create a new evaluator with the implicit `xml` namespace binding and the default
    /// expression-nesting cap ([`DEFAULT_MAX_XPATH_DEPTH`]).
    pub fn new() -> Self {
        XPathEvaluator {
            namespaces: HashMap::from([(
                "xml".to_string(),
                crate::namespace::XML_NAMESPACE.to_string(),
            )]),
            max_depth: DEFAULT_MAX_XPATH_DEPTH,
            max_node_visits: DEFAULT_MAX_XPATH_NODE_VISITS,
            program_cache: std::sync::Mutex::new(ProgramCache::default()),
        }
    }

    /// Register a namespace prefix for use in XPath expressions.
    /// Attempts to rebind the reserved `xml` prefix to another URI are ignored.
    pub fn add_namespace(&mut self, prefix: impl Into<String>, uri: impl Into<String>) {
        let prefix = prefix.into();
        let uri = uri.into();
        if prefix == "xml" && uri != crate::namespace::XML_NAMESPACE {
            return;
        }
        self.namespaces.insert(prefix, uri);
    }

    /// Override the maximum expression-nesting depth. Returns `self` so it
    /// can chain with other builder methods. Changing the limit clears cached
    /// programs so subsequent calls enforce the new parsing limit.
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        if self.max_depth != max_depth {
            self.clear_cache();
            self.max_depth = max_depth;
        }
        self
    }

    /// Release all cached XPath programs and reset their memory accounting.
    pub fn clear_cache(&mut self) {
        self.program_cache = std::sync::Mutex::new(ProgramCache::default());
    }

    /// Override the maximum number of axis/predicate node visits permitted
    /// during one evaluation.
    pub fn with_max_node_visits(mut self, max_node_visits: usize) -> Self {
        self.max_node_visits = max_node_visits;
        self
    }

    /// Evaluate an expression from the given context node.
    ///
    /// Repeated evaluation of the same expression through one evaluator
    /// reuses the parsed AST instead of re-tokenizing and re-parsing per
    /// call. Only successful evaluations are cached, with at most 128 entries,
    /// 4 KiB of source per entry, 64 KiB of owned key/AST allocations per entry,
    /// and 1 MiB of those allocations in total (plus bounded table/allocator
    /// overhead). Larger expressions still evaluate without being cached.
    pub fn evaluate(
        &self,
        doc: &Document<'_>,
        context: NodeId,
        expr: &str,
    ) -> XmlResult<XPathValue> {
        // End the lock before parsing/evaluation; a poisoned advisory cache
        // is bypassed rather than turning a query into a panic.
        let cached = self
            .program_cache
            .lock()
            .ok()
            .and_then(|cache| cache.programs.get(expr).cloned());
        let cache_hit = cached.is_some();
        let program = match cached {
            Some(hit) => hit,
            None => {
                let tokens = tokenize(expr)?;
                let mut parser = XPathParser::new(&tokens, self.max_depth);
                let ast = parser.parse_expr()?;
                if parser.peek().is_some() {
                    return Err(XmlError::xpath(format!(
                        "Unexpected trailing tokens in XPath expression: {:?}",
                        expr
                    )));
                }
                std::sync::Arc::new(ast)
            }
        };

        let budget = EvalBudget::new(self.max_node_visits);
        let ctx = EvalContext {
            node: context,
            current: context,
            position: 1,
            size: 1,
            doc,
            namespaces: &self.namespaces,
            vars: &NoVariables,
            funcs: &NoFunctions,
            budget: &budget,
        };
        let result = evaluate_expr(&program, &ctx);
        if result.is_ok() && !cache_hit {
            if let Ok(mut cache) = self.program_cache.lock() {
                cache.insert(expr, program);
            }
        }
        result
    }

    /// Convenience: evaluate and return the resulting node set.
    pub fn select_nodes(
        &self,
        doc: &Document<'_>,
        context: NodeId,
        expr: &str,
    ) -> XmlResult<Vec<NodeId>> {
        let result = self.evaluate(doc, context, expr)?;
        match result {
            XPathValue::NodeSet(nodes) => Ok(nodes),
            _ => Err(XmlError::xpath("Expression did not evaluate to a node-set")),
        }
    }
}

impl Default for XPathEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Compiled-expression API (for the XSLT layer) ─────────

/// A parsed XPath expression, compiled once and evaluable many times against
/// different contexts.
///
/// The public [`XPathEvaluator::evaluate`] re-tokenizes and re-parses on every
/// call. The XSLT transformer (`crate::xslt`) evaluates the same `select` /
/// `test` / pattern / AVT expression once per node, so it compiles via this type
/// and drives evaluation through [`eval_compiled`] with a per-node context
/// (current node, position/size, namespaces, variable + function resolvers).
pub(crate) struct CompiledXPath {
    ast: Expr,
}

impl CompiledXPath {
    /// Tokenize and parse `expr` with the given nesting cap. Errors on a parse
    /// failure or trailing tokens.
    pub(crate) fn compile(expr: &str, max_depth: u32) -> XmlResult<Self> {
        let tokens = tokenize(expr)?;
        let mut parser = XPathParser::new(&tokens, max_depth);
        let ast = parser.parse_expr()?;
        if parser.peek().is_some() {
            return Err(XmlError::xpath(format!(
                "Unexpected trailing tokens in XPath expression: {:?}",
                expr
            )));
        }
        Ok(CompiledXPath { ast })
    }

    /// If this expression is exactly a bare variable reference (`$name`), return
    /// its `(prefix, local)`. Used by XSLT `xsl:copy-of` to copy a result-tree
    /// fragment variable directly rather than coercing it to a string.
    pub(crate) fn as_variable(&self) -> Option<(Option<&str>, &str)> {
        match &self.ast {
            Expr::Variable(prefix, local) => Some((prefix.as_deref(), local.as_str())),
            _ => None,
        }
    }
}

/// Evaluate a [`CompiledXPath`] against a fully-specified context. The entry
/// point all XSLT expression evaluation flows through.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_compiled(
    compiled: &CompiledXPath,
    doc: &Document<'_>,
    context: NodeId,
    current: NodeId,
    position: usize,
    size: usize,
    namespaces: &HashMap<String, String>,
    vars: &dyn VariableResolver,
    funcs: &dyn FunctionResolver,
    max_node_visits: usize,
) -> XmlResult<XPathValue> {
    let budget = EvalBudget::new(max_node_visits);
    let ctx = EvalContext {
        node: context,
        current,
        position,
        size,
        doc,
        namespaces,
        vars,
        funcs,
        budget: &budget,
    };
    evaluate_expr(&compiled.ast, &ctx)
}

// ─── XSLT match patterns ──────────────────────────────────

/// A compiled XSLT match pattern: a union of location-path alternatives.
///
/// XSLT patterns are a restricted XPath subset, evaluated as a membership test
/// ("does node N match pattern P?") rather than a forward selection. The match
/// reuses [`apply_step`] so predicates and `position()`/`last()` behave exactly
/// as in selection. Each union alternative carries its own default priority
/// (XSLT 1.0 §5.5), and [`CompiledPattern::matches`] returns the highest
/// priority among the matching alternatives.
pub(crate) struct CompiledPattern {
    alts: Vec<PatternAlt>,
}

struct PatternAlt {
    /// True for a pattern rooted at `/` or `//` (anchored at the document root).
    absolute: bool,
    /// Location steps, outermost-first. A `//` is represented (as in the XPath
    /// parser) by an injected `descendant-or-self::node()` step.
    steps: Vec<Step>,
    /// XSLT default priority for this alternative.
    priority: f64,
}

impl CompiledPattern {
    /// Compile a pattern string (e.g. `"md:EntityDescriptor"`, `"a/b"`,
    /// `"@*|text()"`). Reuses the XPath tokenizer/parser, then flattens unions
    /// into alternatives.
    pub(crate) fn compile(pattern: &str, max_depth: u32) -> XmlResult<Self> {
        let tokens = tokenize(pattern)?;
        let mut parser = XPathParser::new(&tokens, max_depth);
        let expr = parser.parse_expr()?;
        if parser.peek().is_some() {
            return Err(XmlError::xpath(format!(
                "Unsupported XSLT match pattern: {:?}",
                pattern
            )));
        }
        let mut alts = Vec::new();
        flatten_pattern(expr, &mut alts)?;
        Ok(CompiledPattern { alts })
    }

    /// Test whether `node` matches the pattern. Returns the default priority of
    /// the highest-priority matching alternative, or `None` if no alternative
    /// matches.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn matches(
        &self,
        doc: &Document<'_>,
        node: NodeId,
        current: NodeId,
        namespaces: &HashMap<String, String>,
        vars: &dyn VariableResolver,
        funcs: &dyn FunctionResolver,
        max_node_visits: usize,
    ) -> Option<f64> {
        let budget = EvalBudget::new(max_node_visits);
        let ctx = EvalContext {
            node,
            current,
            position: 1,
            size: 1,
            doc,
            namespaces,
            vars,
            funcs,
            budget: &budget,
        };
        let mut best: Option<f64> = None;
        for alt in &self.alts {
            if alt_matches(alt, node, &ctx).unwrap_or(false) {
                best = Some(best.map_or(alt.priority, |b| b.max(alt.priority)));
            }
        }
        best
    }

    /// Precompute a cheap dispatch descriptor: the set of node kinds/names this
    /// pattern could possibly match. Callers (XSLT template dispatch) use it to
    /// skip the full [`Self::matches`] for nodes the pattern can never match,
    /// turning per-node "test every template" into "test only applicable
    /// templates". Conservative: anything it cannot statically classify falls
    /// into `any_node` so it is always tested (never a false negative).
    pub(crate) fn dispatch(&self) -> PatternDispatch {
        let mut d = PatternDispatch::default();
        for alt in &self.alts {
            classify_alt(alt, &mut d);
        }
        d
    }
}

/// A coarse, precomputed summary of what node kinds/names a [`CompiledPattern`]
/// can match. See [`CompiledPattern::dispatch`].
#[derive(Default)]
pub(crate) struct PatternDispatch {
    /// Matches any node kind (e.g. a `node()` test or an unclassifiable shape).
    any_node: bool,
    /// Matches any element (a `*`/prefix-wildcard child step).
    any_element: bool,
    /// Matches any attribute (a `@*` step).
    any_attribute: bool,
    text: bool,
    comment: bool,
    pi: bool,
    document: bool,
    /// Specific element local-names matched by name.
    elem_names: Vec<String>,
    /// Specific attribute local-names matched by name.
    attr_names: Vec<String>,
}

impl PatternDispatch {
    /// Could the owning pattern match `node`? A `false` is a guarantee the
    /// pattern does not match (so the expensive check can be skipped); a `true`
    /// means "test it properly".
    pub(crate) fn could_match(&self, doc: &Document<'_>, node: NodeId) -> bool {
        if self.any_node {
            return true;
        }
        match doc.node_kind(node) {
            Some(NodeKind::Element(e)) => {
                self.any_element
                    || self
                        .elem_names
                        .iter()
                        .any(|n| n.as_str() == e.name.local_name.as_ref())
            }
            Some(NodeKind::Attribute(q, _)) => {
                self.any_attribute
                    || self
                        .attr_names
                        .iter()
                        .any(|n| n.as_str() == q.local_name.as_ref())
            }
            Some(NodeKind::Text(_)) | Some(NodeKind::CData(_)) => self.text,
            Some(NodeKind::Comment(_)) => self.comment,
            Some(NodeKind::ProcessingInstruction(_)) => self.pi,
            Some(NodeKind::Document) => self.document,
            None => false,
        }
    }
}

/// Fold one pattern alternative's matchable node kinds/names into `d`.
fn classify_alt(alt: &PatternAlt, d: &mut PatternDispatch) {
    let last = match alt.steps.last() {
        // The `/` pattern (empty steps) matches only the document root.
        None => {
            d.document = true;
            return;
        }
        Some(s) => s,
    };
    // A trailing injected `descendant-or-self::node()` (pattern ending in `//`)
    // is not a real node test — be conservative.
    if matches!(last.axis, Axis::DescendantOrSelf)
        && matches!(&last.node_test, NodeTest::NodeType(nt) if nt == "node")
    {
        d.any_node = true;
        return;
    }
    match last.axis {
        Axis::Attribute => match &last.node_test {
            NodeTest::Name(n) => d.attr_names.push(n.clone()),
            NodeTest::PrefixedName(_, n) => d.attr_names.push(n.clone()),
            NodeTest::Wildcard | NodeTest::PrefixWildcard(_) => d.any_attribute = true,
            // text()/comment() on the attribute axis is nonsensical; be safe.
            NodeTest::NodeType(_) => d.any_node = true,
        },
        Axis::Child => match &last.node_test {
            NodeTest::Name(n) => d.elem_names.push(n.clone()),
            NodeTest::PrefixedName(_, n) => d.elem_names.push(n.clone()),
            NodeTest::Wildcard | NodeTest::PrefixWildcard(_) => d.any_element = true,
            NodeTest::NodeType(nt) => match nt.as_str() {
                "text" => d.text = true,
                "comment" => d.comment = true,
                "processing-instruction" => d.pi = true,
                _ => d.any_node = true, // "node" or unknown
            },
        },
        // Any other axis in a pattern step is unusual; test it always.
        _ => d.any_node = true,
    }
}

/// Flatten a parsed pattern expression into union alternatives. Only path
/// expressions (and unions of them) are valid patterns.
fn flatten_pattern(expr: Expr, alts: &mut Vec<PatternAlt>) -> XmlResult<()> {
    match expr {
        Expr::Union(l, r) => {
            flatten_pattern(*l, alts)?;
            flatten_pattern(*r, alts)?;
            Ok(())
        }
        Expr::Path(steps) => {
            let priority = pattern_priority(&steps);
            alts.push(PatternAlt {
                absolute: false,
                steps,
                priority,
            });
            Ok(())
        }
        Expr::AbsolutePath(steps) => {
            let priority = if steps.is_empty() {
                0.5 // the "/" pattern
            } else {
                pattern_priority(&steps)
            };
            alts.push(PatternAlt {
                absolute: true,
                steps,
                priority,
            });
            Ok(())
        }
        _ => Err(XmlError::xpath("Unsupported XSLT match pattern shape")),
    }
}

/// XSLT 1.0 default priority (§5.5) for one pattern alternative, computed from
/// its last (and only significant) step.
fn pattern_priority(steps: &[Step]) -> f64 {
    // Count "real" steps (a `//` injects a descendant-or-self::node() step that
    // does not count toward multi-step complexity for priority purposes).
    let real: Vec<&Step> = steps
        .iter()
        .filter(|s| {
            !(matches!(s.axis, Axis::DescendantOrSelf)
                && matches!(&s.node_test, NodeTest::NodeType(nt) if nt == "node"))
        })
        .collect();
    let last = match real.last() {
        Some(s) => *s,
        None => return 0.5,
    };
    // Multiple steps, or any predicate, → 0.5.
    if real.len() > 1 || !last.predicates.is_empty() {
        return 0.5;
    }
    match &last.node_test {
        NodeTest::Name(_) | NodeTest::PrefixedName(_, _) => 0.0,
        NodeTest::PrefixWildcard(_) => -0.25,
        NodeTest::Wildcard => -0.5,
        NodeTest::NodeType(_) => -0.5,
    }
}

/// Does `node` match a single pattern alternative?
fn alt_matches(alt: &PatternAlt, node: NodeId, ctx: &EvalContext) -> XmlResult<bool> {
    if alt.steps.is_empty() {
        // The "/" pattern matches only the document root.
        return Ok(alt.absolute && matches!(ctx.doc.node_kind(node), Some(NodeKind::Document)));
    }
    matches_steps(&alt.steps, node, alt.absolute, ctx)
}

/// Match `node` against `steps` (outermost-first) from the right, walking up the
/// tree. `absolute` requires the outermost step to select from the document
/// root.
fn matches_steps(
    steps: &[Step],
    node: NodeId,
    absolute: bool,
    ctx: &EvalContext,
) -> XmlResult<bool> {
    let (last, prefix) = match steps.split_last() {
        Some(v) => v,
        None => return Ok(true),
    };

    // A `//` connector: the injected descendant-or-self::node() step. The prefix
    // must match `node` itself or any ancestor.
    if matches!(last.axis, Axis::DescendantOrSelf)
        && matches!(&last.node_test, NodeTest::NodeType(nt) if nt == "node")
    {
        if prefix.is_empty() {
            // Leading `//` — anchored at the root, matches anything in the tree.
            return Ok(true);
        }
        let mut cur = Some(node);
        while let Some(c) = cur {
            if matches_steps(prefix, c, absolute, ctx)? {
                return Ok(true);
            }
            cur = ctx.doc.parent(c);
        }
        return Ok(false);
    }

    // A normal child/attribute step: `node` must be selected by `last` from its
    // parent.
    let parent = match ctx.doc.parent(node) {
        Some(p) => p,
        None => return Ok(false), // no parent to select `node` from
    };
    // Membership in `apply_step(last, [parent])` requires `node` to (a) be on the
    // step's axis, (b) pass the node-test, and (c) survive the predicates. (a)
    // and (b) are *local* properties of `node` — `parent == doc.parent(node)`
    // guarantees `node` is a child (non-attribute) or attribute of `parent`, so
    // we can check them directly. Doing so first avoids materializing all of
    // `parent`'s children just to discover `node` is the wrong kind: the naive
    // `apply_step` + `contains` is O(siblings) per call, making
    // `best_matching_template` O(width^2) over a wide sibling list (e.g. the
    // thousands of `EntityDescriptor`s in a SAML aggregate, each tested against
    // every template). For the child/attribute axes we therefore short-circuit
    // on the local checks and only fall back to the sibling-materializing path
    // when the node-test passed *and* predicates remain (which may be positional
    // and so need the full set for `position()`/`last()`).
    if matches!(last.axis, Axis::Child | Axis::Attribute) {
        let is_attr = matches!(ctx.doc.node_kind(node), Some(NodeKind::Attribute(_, _)));
        let on_axis = match last.axis {
            Axis::Attribute => is_attr,
            _ => !is_attr, // Axis::Child
        };
        if !on_axis || !matches_node_test(&last.node_test, node, ctx.doc, ctx.namespaces) {
            return Ok(false);
        }
        if !last.predicates.is_empty() {
            if last.predicates.iter().all(predicate_is_per_node) {
                // Every predicate is a position-independent boolean, so a node's
                // membership depends only on itself: evaluate each on `node`
                // alone (position 1, size 1) instead of materializing every
                // sibling. This keeps predicate patterns like
                // `text()[normalize-space(.)='']` linear even under wide mixed
                // content (e.g. the whitespace text nodes between thousands of
                // top-level elements in a SAML aggregate).
                let pred_ctx = EvalContext {
                    node,
                    current: ctx.current,
                    position: 1,
                    size: 1,
                    doc: ctx.doc,
                    namespaces: ctx.namespaces,
                    vars: ctx.vars,
                    funcs: ctx.funcs,
                    budget: ctx.budget,
                };
                for pred in &last.predicates {
                    ctx.budget.charge(1)?;
                    if !evaluate_expr(pred, &pred_ctx)?.to_boolean() {
                        return Ok(false);
                    }
                }
            } else {
                // A positional predicate (`[2]`, `position()`, `last()`) needs the
                // full sibling set for correct numbering.
                let selected = apply_step(last, &[parent], ctx)?;
                if !selected.contains(&node) {
                    return Ok(false);
                }
            }
        }
    } else {
        // Unusual axis in a pattern step: fall back to the general selection.
        let selected = apply_step(last, &[parent], ctx)?;
        if !selected.contains(&node) {
            return Ok(false);
        }
    }

    if prefix.is_empty() {
        // Outermost step matched. For an absolute pattern, the node it was
        // selected from must be the document root.
        return Ok(if absolute {
            matches!(ctx.doc.node_kind(parent), Some(NodeKind::Document))
        } else {
            true
        });
    }
    matches_steps(prefix, parent, absolute, ctx)
}

/// Whether a pattern-step predicate can be evaluated independently per node
/// (singleton context), i.e. it is a position-independent boolean. Conservative:
/// returns `true` only for expressions whose result is *definitely* boolean
/// (comparisons, `and`/`or`, and boolean-returning core functions) and that
/// contain no `position()`/`last()` call. Numeric predicates (`[2]`) and anything
/// uncertain fall back to the full-context selection path.
fn predicate_is_per_node(expr: &Expr) -> bool {
    let boolean_typed = match expr {
        Expr::Eq(..)
        | Expr::NotEq(..)
        | Expr::Lt(..)
        | Expr::Gt(..)
        | Expr::LtEq(..)
        | Expr::GtEq(..)
        | Expr::And(..)
        | Expr::Or(..) => true,
        Expr::FunctionCall(name, _) => matches!(
            name.as_str(),
            "not" | "true" | "false" | "boolean" | "contains" | "starts-with" | "lang"
        ),
        _ => false,
    };
    boolean_typed && !expr_uses_position_or_last(expr)
}

/// Recursively test whether an expression references `position()` or `last()`
/// anywhere (including nested paths/predicates). Used conservatively: a match
/// here forces the full-context predicate path.
fn expr_uses_position_or_last(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall(name, args) => {
            name == "position" || name == "last" || args.iter().any(expr_uses_position_or_last)
        }
        Expr::Path(steps) | Expr::AbsolutePath(steps) => steps
            .iter()
            .any(|s| s.predicates.iter().any(expr_uses_position_or_last)),
        Expr::Union(a, b)
        | Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::NotEq(a, b)
        | Expr::Lt(a, b)
        | Expr::Gt(a, b)
        | Expr::LtEq(a, b)
        | Expr::GtEq(a, b)
        | Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Mul(a, b)
        | Expr::Div(a, b)
        | Expr::Mod(a, b) => expr_uses_position_or_last(a) || expr_uses_position_or_last(b),
        Expr::Negate(a) => expr_uses_position_or_last(a),
        Expr::StringLiteral(_) | Expr::NumberLiteral(_) | Expr::Variable(..) => false,
    }
}

// ─── XPath Tokenizer ──────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Axes
    Axis(String),
    // Node types
    NodeType(String),
    // Names
    Name(String),
    PrefixedName(String, String), // (prefix, local)
    // Literals
    StringLiteral(String),
    Number(f64),
    // Variable reference: ($prefix, local). `None` prefix for unprefixed names.
    Variable(Option<String>, String),
    // Operators
    Slash,
    DoubleSlash,
    Dot,
    DoubleDot,
    At,
    Star,
    Pipe,
    Plus,
    Minus,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    Div,
    Mod,
    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    // Functions
    FunctionName(String),
}

fn tokenize(expr: &str) -> XmlResult<Vec<Token>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Skip whitespace
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        match chars[i] {
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '/' {
                    tokens.push(Token::DoubleSlash);
                    i += 2;
                } else {
                    tokens.push(Token::Slash);
                    i += 1;
                }
            }
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token::DoubleDot);
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                    // Number starting with .
                    let start = i;
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let s: String = chars[start..i].iter().collect();
                    tokens.push(Token::Number(s.parse().unwrap()));
                } else {
                    tokens.push(Token::Dot);
                    i += 1;
                }
            }
            '@' => {
                tokens.push(Token::At);
                i += 1;
            }
            '$' => {
                // Variable reference: `$name` or `$prefix:local`.
                i += 1;
                if i >= chars.len() || !is_xpath_name_start(chars[i]) {
                    return Err(XmlError::xpath("Expected variable name after '$'"));
                }
                let start = i;
                while i < chars.len() && is_xpath_name_char(chars[i]) {
                    i += 1;
                }
                let first: String = chars[start..i].iter().collect();
                // Optional `:local` (a single colon, not the `::` axis separator).
                if i + 1 < chars.len()
                    && chars[i] == ':'
                    && chars[i + 1] != ':'
                    && is_xpath_name_start(chars[i + 1])
                {
                    i += 1; // consume ':'
                    let local_start = i;
                    while i < chars.len() && is_xpath_name_char(chars[i]) {
                        i += 1;
                    }
                    let local: String = chars[local_start..i].iter().collect();
                    tokens.push(Token::Variable(Some(first), local));
                } else {
                    tokens.push(Token::Variable(None, first));
                }
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '|' => {
                tokens.push(Token::Pipe);
                i += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '=' => {
                tokens.push(Token::Eq);
                i += 1;
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::NotEq);
                    i += 2;
                } else {
                    return Err(XmlError::xpath("Unexpected '!'"));
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::LtEq);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::GtEq);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            '"' | '\'' => {
                let quote = chars[i];
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(XmlError::xpath("Unterminated string literal"));
                }
                let s: String = chars[start..i].iter().collect();
                tokens.push(Token::StringLiteral(s));
                i += 1;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                tokens
                    .push(Token::Number(s.parse().map_err(|_| {
                        XmlError::xpath(format!("Invalid number: {}", s))
                    })?));
            }
            c if is_xpath_name_start(c) => {
                let start = i;
                while i < chars.len() && is_xpath_name_char(chars[i]) {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();

                // Check for axis or operator keywords
                // Skip whitespace after name
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }

                if j < chars.len() && chars[j] == ':' && j + 1 < chars.len() && chars[j + 1] == ':'
                {
                    // Axis specifier
                    tokens.push(Token::Axis(name));
                    i = j + 2;
                } else if j < chars.len() && chars[j] == '(' {
                    // Node type test or function
                    match name.as_str() {
                        "node" | "text" | "comment" | "processing-instruction" => {
                            tokens.push(Token::NodeType(name));
                        }
                        _ => {
                            tokens.push(Token::FunctionName(name));
                        }
                    }
                } else if j < chars.len()
                    && chars[j] == ':'
                    && j + 1 < chars.len()
                    && chars[j + 1] != ':'
                {
                    // Prefixed name (ns:local)
                    let prefix = name;
                    i = j + 1;
                    if i < chars.len() && chars[i] == '*' {
                        tokens.push(Token::PrefixedName(prefix, "*".to_string()));
                        i += 1;
                    } else {
                        let local_start = i;
                        while i < chars.len() && is_xpath_name_char(chars[i]) {
                            i += 1;
                        }
                        let local: String = chars[local_start..i].iter().collect();
                        // A prefixed name immediately followed by `(` is a
                        // function call (e.g. an extension function like
                        // `date:date-time()`), not a node test. Carry the full
                        // qualified name so the resolver can split it.
                        let mut k = i;
                        while k < chars.len() && chars[k].is_whitespace() {
                            k += 1;
                        }
                        if k < chars.len() && chars[k] == '(' {
                            tokens.push(Token::FunctionName(format!("{}:{}", prefix, local)));
                        } else {
                            tokens.push(Token::PrefixedName(prefix, local));
                        }
                    }
                } else {
                    // Check for keyword operators
                    match name.as_str() {
                        "and" => tokens.push(Token::And),
                        "or" => tokens.push(Token::Or),
                        "div" => tokens.push(Token::Div),
                        "mod" => tokens.push(Token::Mod),
                        _ => tokens.push(Token::Name(name)),
                    }
                }
            }
            _ => {
                return Err(XmlError::xpath(format!(
                    "Unexpected character: '{}'",
                    chars[i]
                )));
            }
        }
    }

    Ok(tokens)
}

fn is_xpath_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_xpath_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

// ─── XPath AST ─────────────────────────────────────────

#[derive(Debug, Clone)]
enum Expr {
    // Path expressions
    Path(Vec<Step>),
    AbsolutePath(Vec<Step>),
    // Union
    Union(Box<Expr>, Box<Expr>),
    // Binary operators
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    NotEq(Box<Expr>, Box<Expr>),
    Lt(Box<Expr>, Box<Expr>),
    Gt(Box<Expr>, Box<Expr>),
    LtEq(Box<Expr>, Box<Expr>),
    GtEq(Box<Expr>, Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Mod(Box<Expr>, Box<Expr>),
    // Unary
    Negate(Box<Expr>),
    // Literals
    StringLiteral(String),
    NumberLiteral(f64),
    // Variable reference: (prefix, local). `None` prefix for unprefixed names.
    Variable(Option<String>, String),
    // Function call
    FunctionCall(String, Vec<Expr>),
}

impl Expr {
    /// Owned heap capacity below this expression; excludes its inline storage.
    /// Walk iteratively so accounting adds no recursive call-stack pressure.
    fn heap_bytes(&self) -> usize {
        let mut bytes = 0usize;
        let mut pending = vec![self];
        while let Some(expr) = pending.pop() {
            match expr {
                Expr::Path(steps) | Expr::AbsolutePath(steps) => {
                    bytes = bytes.saturating_add(
                        steps.capacity().saturating_mul(std::mem::size_of::<Step>()),
                    );
                    for step in steps {
                        let name_bytes = match &step.node_test {
                            NodeTest::Name(s)
                            | NodeTest::PrefixWildcard(s)
                            | NodeTest::NodeType(s) => s.capacity(),
                            NodeTest::PrefixedName(p, s) => {
                                p.capacity().saturating_add(s.capacity())
                            }
                            NodeTest::Wildcard => 0,
                        };
                        bytes = bytes.saturating_add(name_bytes).saturating_add(
                            step.predicates
                                .capacity()
                                .saturating_mul(std::mem::size_of::<Expr>()),
                        );
                        pending.extend(&step.predicates);
                    }
                }
                Expr::Union(l, r)
                | Expr::Or(l, r)
                | Expr::And(l, r)
                | Expr::Eq(l, r)
                | Expr::NotEq(l, r)
                | Expr::Lt(l, r)
                | Expr::Gt(l, r)
                | Expr::LtEq(l, r)
                | Expr::GtEq(l, r)
                | Expr::Add(l, r)
                | Expr::Sub(l, r)
                | Expr::Mul(l, r)
                | Expr::Div(l, r)
                | Expr::Mod(l, r) => {
                    bytes = bytes.saturating_add(2 * std::mem::size_of::<Expr>());
                    pending.extend([l.as_ref(), r.as_ref()]);
                }
                Expr::Negate(inner) => {
                    bytes = bytes.saturating_add(std::mem::size_of::<Expr>());
                    pending.push(inner);
                }
                Expr::StringLiteral(s) => bytes = bytes.saturating_add(s.capacity()),
                Expr::NumberLiteral(_) => {}
                Expr::Variable(prefix, local) => {
                    bytes = bytes
                        .saturating_add(prefix.as_ref().map_or(0, String::capacity))
                        .saturating_add(local.capacity());
                }
                Expr::FunctionCall(name, args) => {
                    bytes = bytes.saturating_add(name.capacity()).saturating_add(
                        args.capacity().saturating_mul(std::mem::size_of::<Expr>()),
                    );
                    pending.extend(args);
                }
            }
        }
        bytes
    }
}

#[derive(Debug, Clone)]
struct Step {
    axis: Axis,
    node_test: NodeTest,
    predicates: Vec<Expr>,
}

#[derive(Debug, Clone)]
enum Axis {
    Child,
    Descendant,
    Parent,
    Ancestor,
    FollowingSibling,
    PrecedingSibling,
    Following,
    Preceding,
    Attribute,
    Namespace,
    Self_,
    DescendantOrSelf,
    AncestorOrSelf,
}

#[derive(Debug, Clone)]
enum NodeTest {
    Name(String),
    PrefixedName(String, String),
    Wildcard,
    PrefixWildcard(String),
    NodeType(String),
}

// ─── XPath Parser (tokens -> AST) ─────────────────────

/// Default maximum expression-nesting depth. Each `(...)` group, `[...]`
/// predicate, chained leading `-`, and function-call argument counts as
/// one level. XPath has a deep grammar hierarchy (roughly 15 frames per
/// expression-grammar re-entry through or/and/equality/relational/
/// additive/multiplicative/unary/union/path), so a modest cap of 32
/// stays well clear of a 2 MiB thread stack in debug builds while
/// permitting any realistic XPath expression (real-world XPath rarely
/// exceeds 5-10 nesting levels). Override via
/// [`XPathEvaluator::with_max_depth`].
pub const DEFAULT_MAX_XPATH_DEPTH: u32 = 32;

/// Default maximum axis/predicate node visits in one XPath evaluation.
///
/// The cap bounds hostile expressions such as repeated descendant/following
/// axes over large DOMs while leaving ordinary document queries unaffected.
pub const DEFAULT_MAX_XPATH_NODE_VISITS: usize = 100_000;

struct XPathParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Current expression-nesting depth. Bumped around each re-entry
    /// to `parse_expr` (group, predicate, function argument, chained
    /// leading `-`).
    depth: u32,
    /// Configured cap for `depth`. Passed down from `XPathEvaluator`.
    max_depth: u32,
}

impl<'a> XPathParser<'a> {
    fn new(tokens: &'a [Token], max_depth: u32) -> Self {
        XPathParser {
            tokens,
            pos: 0,
            depth: 0,
            max_depth,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, expected: &Token) -> XmlResult<()> {
        match self.advance() {
            Some(t) if t == expected => Ok(()),
            Some(t) => Err(XmlError::xpath(format!(
                "Expected {:?}, got {:?}",
                expected, t
            ))),
            None => Err(XmlError::xpath(format!("Expected {:?}, got EOF", expected))),
        }
    }

    fn parse_expr(&mut self) -> XmlResult<Expr> {
        self.parse_or_expr()
    }

    /// Parse a nested expression at `depth + 1`, returning an error if
    /// the cap is exceeded. Used at every re-entry point to the
    /// expression grammar: `(expr)`, `[pred]`, `fn(arg, ...)`.
    ///
    /// The depth counter is managed by a `DepthGuard` whose `Drop` impl
    /// decrements `self.depth` on every exit path (including the
    /// `?`-early-return when the inner parse fails). This keeps the
    /// counter balanced even across error / panic boundaries.
    fn parse_nested_expr(&mut self) -> XmlResult<Expr> {
        let guard = DepthGuard::enter(self)?;
        guard.parser.parse_expr()
    }

    fn charge_binary_chain(&self, chain_depth: &mut u32) -> XmlResult<()> {
        if *chain_depth >= self.max_depth {
            return Err(XmlError::xpath(format!(
                "XPath expression nesting exceeds maximum depth of {}",
                self.max_depth
            )));
        }
        *chain_depth += 1;
        Ok(())
    }
}

/// RAII helper that bumps `XPathParser::depth` on construction and
/// decrements it on drop. Returns `Err` at construction time if the
/// bump would exceed the cap. The Drop impl runs on every exit path -
/// `Ok`, `?`-propagated `Err`, or panic - so the depth counter stays
/// balanced even when a nested parse fails mid-way.
struct DepthGuard<'p, 'a> {
    parser: &'p mut XPathParser<'a>,
}

impl<'p, 'a> DepthGuard<'p, 'a> {
    fn enter(parser: &'p mut XPathParser<'a>) -> XmlResult<Self> {
        if parser.depth >= parser.max_depth {
            return Err(XmlError::xpath(format!(
                "XPath expression nesting exceeds maximum depth of {}",
                parser.max_depth
            )));
        }
        parser.depth += 1;
        Ok(DepthGuard { parser })
    }
}

impl<'p, 'a> Drop for DepthGuard<'p, 'a> {
    fn drop(&mut self) {
        self.parser.depth -= 1;
    }
}

impl<'a> XPathParser<'a> {
    fn parse_or_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_and_expr()?;
        let mut chain_depth = 0;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            self.charge_binary_chain(&mut chain_depth)?;
            let right = self.parse_and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_equality_expr()?;
        let mut chain_depth = 0;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            self.charge_binary_chain(&mut chain_depth)?;
            let right = self.parse_equality_expr()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_relational_expr()?;
        let mut chain_depth = 0;
        loop {
            match self.peek() {
                Some(Token::Eq) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_relational_expr()?;
                    left = Expr::Eq(Box::new(left), Box::new(right));
                }
                Some(Token::NotEq) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_relational_expr()?;
                    left = Expr::NotEq(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_relational_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_additive_expr()?;
        let mut chain_depth = 0;
        loop {
            match self.peek() {
                Some(Token::Lt) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_additive_expr()?;
                    left = Expr::Lt(Box::new(left), Box::new(right));
                }
                Some(Token::Gt) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_additive_expr()?;
                    left = Expr::Gt(Box::new(left), Box::new(right));
                }
                Some(Token::LtEq) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_additive_expr()?;
                    left = Expr::LtEq(Box::new(left), Box::new(right));
                }
                Some(Token::GtEq) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_additive_expr()?;
                    left = Expr::GtEq(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_additive_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_multiplicative_expr()?;
        let mut chain_depth = 0;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_multiplicative_expr()?;
                    left = Expr::Add(Box::new(left), Box::new(right));
                }
                Some(Token::Minus) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_multiplicative_expr()?;
                    left = Expr::Sub(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_multiplicative_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_unary_expr()?;
        let mut chain_depth = 0;
        loop {
            match self.peek() {
                Some(Token::Star) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_unary_expr()?;
                    left = Expr::Mul(Box::new(left), Box::new(right));
                }
                Some(Token::Div) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_unary_expr()?;
                    left = Expr::Div(Box::new(left), Box::new(right));
                }
                Some(Token::Mod) => {
                    self.advance();
                    self.charge_binary_chain(&mut chain_depth)?;
                    let right = self.parse_unary_expr()?;
                    left = Expr::Mod(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary_expr(&mut self) -> XmlResult<Expr> {
        if matches!(self.peek(), Some(Token::Minus)) {
            // `parse_unary_expr` recurses directly into itself for
            // chained leading minuses (`---...---1`), bypassing
            // `parse_nested_expr`. Use the same `DepthGuard` RAII so a
            // long `-` chain is capped the same way and the counter
            // stays balanced on error paths.
            let guard = DepthGuard::enter(self)?;
            guard.parser.advance();
            let inner = guard.parser.parse_unary_expr()?;
            Ok(Expr::Negate(Box::new(inner)))
        } else {
            self.parse_union_expr()
        }
    }

    fn parse_union_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_path_expr()?;
        let mut chain_depth = 0;
        while matches!(self.peek(), Some(Token::Pipe)) {
            self.advance();
            self.charge_binary_chain(&mut chain_depth)?;
            let right = self.parse_path_expr()?;
            left = Expr::Union(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_path_expr(&mut self) -> XmlResult<Expr> {
        match self.peek() {
            Some(Token::Slash) => {
                self.advance();
                if self.peek().is_none()
                    || matches!(
                        self.peek(),
                        Some(Token::RParen)
                            | Some(Token::RBracket)
                            | Some(Token::Pipe)
                            | Some(Token::And)
                            | Some(Token::Or)
                            | Some(Token::Eq)
                            | Some(Token::NotEq)
                    )
                {
                    // Just "/" means the root node
                    Ok(Expr::AbsolutePath(Vec::new()))
                } else {
                    let steps = self.parse_relative_path()?;
                    Ok(Expr::AbsolutePath(steps))
                }
            }
            Some(Token::DoubleSlash) => {
                self.advance();
                let mut steps = vec![Step {
                    axis: Axis::DescendantOrSelf,
                    node_test: NodeTest::NodeType("node".to_string()),
                    predicates: Vec::new(),
                }];
                steps.extend(self.parse_relative_path()?);
                Ok(Expr::AbsolutePath(steps))
            }
            Some(Token::Number(n)) => {
                let n = *n;
                self.advance();
                Ok(Expr::NumberLiteral(n))
            }
            Some(Token::StringLiteral(s)) => {
                let s = s.clone();
                self.advance();
                Ok(Expr::StringLiteral(s))
            }
            Some(Token::Variable(prefix, local)) => {
                let prefix = prefix.clone();
                let local = local.clone();
                self.advance();
                Ok(Expr::Variable(prefix, local))
            }
            Some(Token::FunctionName(_)) => self.parse_function_call(),
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_nested_expr()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            _ => {
                let steps = self.parse_relative_path()?;
                Ok(Expr::Path(steps))
            }
        }
    }

    fn parse_relative_path(&mut self) -> XmlResult<Vec<Step>> {
        let mut steps = Vec::new();
        steps.push(self.parse_step()?);
        loop {
            match self.peek() {
                Some(Token::Slash) => {
                    self.advance();
                    steps.push(self.parse_step()?);
                }
                Some(Token::DoubleSlash) => {
                    self.advance();
                    steps.push(Step {
                        axis: Axis::DescendantOrSelf,
                        node_test: NodeTest::NodeType("node".to_string()),
                        predicates: Vec::new(),
                    });
                    steps.push(self.parse_step()?);
                }
                _ => break,
            }
        }
        Ok(steps)
    }

    fn parse_step(&mut self) -> XmlResult<Step> {
        match self.peek() {
            Some(Token::Dot) => {
                self.advance();
                Ok(Step {
                    axis: Axis::Self_,
                    node_test: NodeTest::NodeType("node".to_string()),
                    predicates: Vec::new(),
                })
            }
            Some(Token::DoubleDot) => {
                self.advance();
                Ok(Step {
                    axis: Axis::Parent,
                    node_test: NodeTest::NodeType("node".to_string()),
                    predicates: Vec::new(),
                })
            }
            Some(Token::At) => {
                self.advance();
                let node_test = self.parse_node_test()?;
                let predicates = self.parse_predicates()?;
                Ok(Step {
                    axis: Axis::Attribute,
                    node_test,
                    predicates,
                })
            }
            Some(Token::Axis(axis_name)) => {
                let axis = parse_axis_name(axis_name)?;
                self.advance();
                let node_test = self.parse_node_test()?;
                let predicates = self.parse_predicates()?;
                Ok(Step {
                    axis,
                    node_test,
                    predicates,
                })
            }
            _ => {
                let node_test = self.parse_node_test()?;
                let predicates = self.parse_predicates()?;
                Ok(Step {
                    axis: Axis::Child,
                    node_test,
                    predicates,
                })
            }
        }
    }

    fn parse_node_test(&mut self) -> XmlResult<NodeTest> {
        match self.peek() {
            Some(Token::Star) => {
                self.advance();
                Ok(NodeTest::Wildcard)
            }
            Some(Token::NodeType(nt)) => {
                let nt = nt.clone();
                self.advance();
                self.expect(&Token::LParen)?;
                self.expect(&Token::RParen)?;
                Ok(NodeTest::NodeType(nt))
            }
            Some(Token::Name(name)) => {
                let name = name.clone();
                self.advance();
                Ok(NodeTest::Name(name))
            }
            Some(Token::PrefixedName(prefix, local)) => {
                let p = prefix.clone();
                let l = local.clone();
                self.advance();
                if l == "*" {
                    Ok(NodeTest::PrefixWildcard(p))
                } else {
                    Ok(NodeTest::PrefixedName(p, l))
                }
            }
            _ => Err(XmlError::xpath("Expected node test")),
        }
    }

    fn parse_predicates(&mut self) -> XmlResult<Vec<Expr>> {
        let mut predicates = Vec::new();
        while matches!(self.peek(), Some(Token::LBracket)) {
            self.advance();
            let expr = self.parse_nested_expr()?;
            self.expect(&Token::RBracket)?;
            predicates.push(expr);
        }
        Ok(predicates)
    }

    fn parse_function_call(&mut self) -> XmlResult<Expr> {
        let name = match self.advance() {
            Some(Token::FunctionName(n)) => n.clone(),
            _ => return Err(XmlError::xpath("Expected function name")),
        };
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        if !matches!(self.peek(), Some(Token::RParen)) {
            args.push(self.parse_nested_expr()?);
            while matches!(self.peek(), Some(Token::Comma)) {
                self.advance();
                args.push(self.parse_nested_expr()?);
            }
        }
        self.expect(&Token::RParen)?;
        Ok(Expr::FunctionCall(name, args))
    }
}

fn parse_axis_name(name: &str) -> XmlResult<Axis> {
    match name {
        "child" => Ok(Axis::Child),
        "descendant" => Ok(Axis::Descendant),
        "parent" => Ok(Axis::Parent),
        "ancestor" => Ok(Axis::Ancestor),
        "following-sibling" => Ok(Axis::FollowingSibling),
        "preceding-sibling" => Ok(Axis::PrecedingSibling),
        "following" => Ok(Axis::Following),
        "preceding" => Ok(Axis::Preceding),
        "attribute" => Ok(Axis::Attribute),
        "namespace" => Ok(Axis::Namespace),
        "self" => Ok(Axis::Self_),
        "descendant-or-self" => Ok(Axis::DescendantOrSelf),
        "ancestor-or-self" => Ok(Axis::AncestorOrSelf),
        _ => Err(XmlError::xpath(format!("Unknown axis: {}", name))),
    }
}

// ─── XPath Evaluator ───────────────────────────────────

struct EvalContext<'a, 'b> {
    node: NodeId,
    /// The XSLT "current node" (the node `current()` returns). For plain XPath
    /// this equals `node`; inside a predicate it stays fixed at the node the
    /// step is being evaluated against while `node` walks the candidate list.
    current: NodeId,
    position: usize,
    size: usize,
    doc: &'a Document<'b>,
    namespaces: &'a HashMap<String, String>,
    /// Resolver for `$variable` references (XSLT scope; no-op for plain XPath).
    vars: &'a dyn VariableResolver,
    /// Resolver for non-core function calls (XSLT/EXSLT; no-op for plain XPath).
    funcs: &'a dyn FunctionResolver,
    budget: &'a EvalBudget,
}

struct EvalBudget {
    /// Visits still available during this evaluation.
    remaining: Cell<usize>,
    /// The caller-configured cap, retained for stable diagnostics.
    max_visits: usize,
}

impl EvalBudget {
    fn new(max_visits: usize) -> Self {
        Self {
            remaining: Cell::new(max_visits),
            max_visits,
        }
    }

    fn charge(&self, amount: usize) -> XmlResult<()> {
        let remaining = self.remaining.get();
        if amount > remaining {
            return Err(XmlError::xpath(format!(
                "XPath evaluation exceeded maximum node visit budget of {}",
                self.max_visits
            )));
        }
        self.remaining.set(remaining - amount);
        Ok(())
    }
}

fn evaluate_expr(expr: &Expr, ctx: &EvalContext) -> XmlResult<XPathValue> {
    match expr {
        Expr::Path(steps) => {
            // `apply_steps` already returns a deduplicated, document-ordered
            // vector, so an extra dedup pass here would be redundant (and
            // uncharged) work.
            Ok(XPathValue::NodeSet(apply_steps(steps, ctx.node, ctx)?))
        }
        Expr::AbsolutePath(steps) => {
            // Find the document root
            let mut root = ctx.node;
            while let Some(p) = ctx.doc.parent(root) {
                root = p;
            }
            // Already deduplicated and document-ordered by `apply_steps`.
            Ok(XPathValue::NodeSet(apply_steps(steps, root, ctx)?))
        }
        Expr::Union(left, right) => {
            let left_val = evaluate_expr(left, ctx)?;
            let right_val = evaluate_expr(right, ctx)?;
            let mut nodes = left_val.as_node_set().to_vec();
            nodes.extend_from_slice(right_val.as_node_set());
            ctx.budget.charge(nodes.len())?;
            Ok(XPathValue::NodeSet(dedup_document_order(ctx.doc, nodes)))
        }
        Expr::Or(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_boolean();
            if l {
                return Ok(XPathValue::Boolean(true));
            }
            let r = evaluate_expr(right, ctx)?.to_boolean();
            Ok(XPathValue::Boolean(r))
        }
        Expr::And(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_boolean();
            if !l {
                return Ok(XPathValue::Boolean(false));
            }
            let r = evaluate_expr(right, ctx)?.to_boolean();
            Ok(XPathValue::Boolean(r))
        }
        Expr::Eq(left, right) => {
            let l = evaluate_expr(left, ctx)?;
            let r = evaluate_expr(right, ctx)?;
            charge_comparison(&l, &r, ctx)?;
            Ok(XPathValue::Boolean(xpath_equal(&l, &r, ctx.doc)))
        }
        Expr::NotEq(left, right) => {
            let l = evaluate_expr(left, ctx)?;
            let r = evaluate_expr(right, ctx)?;
            charge_comparison(&l, &r, ctx)?;
            Ok(XPathValue::Boolean(!xpath_equal(&l, &r, ctx.doc)))
        }
        Expr::Lt(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Boolean(l < r))
        }
        Expr::Gt(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Boolean(l > r))
        }
        Expr::LtEq(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Boolean(l <= r))
        }
        Expr::GtEq(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Boolean(l >= r))
        }
        Expr::Add(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(l + r))
        }
        Expr::Sub(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(l - r))
        }
        Expr::Mul(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(l * r))
        }
        Expr::Div(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(l / r))
        }
        Expr::Mod(left, right) => {
            let l = evaluate_expr(left, ctx)?.to_number(ctx.doc);
            let r = evaluate_expr(right, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(l % r))
        }
        Expr::Negate(inner) => {
            let n = evaluate_expr(inner, ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(-n))
        }
        Expr::StringLiteral(s) => Ok(XPathValue::String(s.clone())),
        Expr::NumberLiteral(n) => Ok(XPathValue::Number(*n)),
        Expr::Variable(prefix, local) => {
            match ctx.vars.resolve_variable(prefix.as_deref(), local) {
                Some(v) => Ok(v),
                None => {
                    let name = match prefix {
                        Some(p) => format!("{}:{}", p, local),
                        None => local.clone(),
                    };
                    Err(XmlError::xpath(format!("Undefined variable: ${}", name)))
                }
            }
        }
        Expr::FunctionCall(name, args) => evaluate_function(name, args, ctx),
    }
}

/// Charge the evaluation budget for an `=`/`!=` comparison.
///
/// Node-set comparison is an O(n*m) cartesian scan over string-values, and that
/// work is not otherwise charged (the budget only covers node-set *building*).
/// Without this, `(/r/a) = (/r/b)` over disjoint-valued sets built via cheap
/// child-axis paths runs for minutes while staying under the node-visit cap.
///
/// Only node-set work is charged, matching the actual number of string-value
/// scans the comparison performs:
/// - node-set vs node-set: `n * m`
/// - node-set vs scalar: `n` (the scalar is converted once, then scanned against
///   each node)
/// - scalar vs scalar: `0` (no node visits — e.g. `1 = 1` must not consume budget,
///   otherwise it would fail under a zero budget despite touching no nodes)
///
/// The operands are matched on their enum variants rather than via
/// `as_node_set().len()`, because that helper returns an empty slice for *both* a
/// scalar and an empty node-set. An empty node-set short-circuits the comparison
/// with zero scans, so it must charge `0`, not be billed as a 1-wide scalar.
fn charge_comparison(left: &XPathValue, right: &XPathValue, ctx: &EvalContext) -> XmlResult<()> {
    let cost = match (left, right) {
        (XPathValue::NodeSet(a), XPathValue::NodeSet(b)) => a.len().saturating_mul(b.len()),
        (XPathValue::NodeSet(ns), _) | (_, XPathValue::NodeSet(ns)) => ns.len(),
        _ => 0,
    };
    ctx.budget.charge(cost)
}

/// XPath equality comparison (handles node-set vs string/number/boolean).
fn xpath_equal(left: &XPathValue, right: &XPathValue, doc: &Document<'_>) -> bool {
    match (left, right) {
        (XPathValue::NodeSet(ls), XPathValue::NodeSet(rs)) => {
            for &l in ls {
                let lv = string_value_of_node(doc, l);
                for &r in rs {
                    let rv = string_value_of_node(doc, r);
                    if lv == rv {
                        return true;
                    }
                }
            }
            false
        }
        (XPathValue::NodeSet(ns), other) | (other, XPathValue::NodeSet(ns)) => match other {
            XPathValue::Boolean(b) => {
                let ns_bool = !ns.is_empty();
                ns_bool == *b
            }
            XPathValue::Number(n) => {
                for &node in ns {
                    let sv = string_value_of_node(doc, node);
                    if let Ok(nv) = sv.trim().parse::<f64>() {
                        if (nv - n).abs() < f64::EPSILON {
                            return true;
                        }
                    }
                }
                false
            }
            XPathValue::String(s) => {
                for &node in ns {
                    let sv = string_value_of_node(doc, node);
                    if sv == *s {
                        return true;
                    }
                }
                false
            }
            _ => false,
        },
        (XPathValue::Boolean(a), XPathValue::Boolean(b)) => a == b,
        (XPathValue::Number(a), XPathValue::Number(b)) => (a - b).abs() < f64::EPSILON,
        (XPathValue::String(a), XPathValue::String(b)) => a == b,
        (XPathValue::Boolean(_), _) | (_, XPathValue::Boolean(_)) => {
            left.to_boolean() == right.to_boolean()
        }
        (XPathValue::Number(_), _) | (_, XPathValue::Number(_)) => {
            let a = left.to_number(doc);
            let b = right.to_number(doc);
            (a - b).abs() < f64::EPSILON
        }
    }
}

/// Is `step` the `descendant-or-self::node()` step the parser injects for
/// a `//` connector (no predicates)?
fn is_double_slash_step(step: &Step) -> bool {
    matches!(step.axis, Axis::DescendantOrSelf)
        && matches!(&step.node_test, NodeTest::NodeType(nt) if nt == "node")
        && step.predicates.is_empty()
}

/// Evaluate a location path's steps from `start`, fusing each
/// `//child::T` (and `//attribute::T`) pair without predicates into one
/// descendant walk. `descendant-or-self::node()/child::T` selects exactly
/// `descendant::T`, so the fused walk visits every node once, tests it in
/// place and never materializes the intermediate all-nodes set that the
/// two-step form builds and then re-scans child by child.
fn apply_steps(steps: &[Step], start: NodeId, ctx: &EvalContext) -> XmlResult<Vec<NodeId>> {
    let mut nodes = vec![start];
    let mut i = 0;
    while i < steps.len() {
        let step = &steps[i];
        if i + 1 < steps.len() && is_double_slash_step(step) {
            let next = &steps[i + 1];
            if next.predicates.is_empty() {
                match next.axis {
                    Axis::Child => {
                        nodes = apply_descendant_test(&next.node_test, false, false, &nodes, ctx)?;
                        i += 2;
                        continue;
                    }
                    Axis::Attribute => {
                        nodes = apply_descendant_test(&next.node_test, true, true, &nodes, ctx)?;
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        nodes = apply_step(step, &nodes, ctx)?;
        i += 1;
    }
    Ok(nodes)
}

/// Select every node under each context node (plus the node itself when
/// `include_self`) that passes `test`; with `attributes`, test the attribute
/// nodes of each visited element instead of the elements. One budget charge
/// per node visited.
fn apply_descendant_test(
    test: &NodeTest,
    include_self: bool,
    attributes: bool,
    context_nodes: &[NodeId],
    ctx: &EvalContext,
) -> XmlResult<Vec<NodeId>> {
    let doc = ctx.doc;
    let test = ResolvedTest::resolve(test, ctx.namespaces);
    let mut result = Vec::new();
    for &node in context_nodes {
        if attributes {
            walk_descendants(doc, node, include_self, |n| {
                ctx.budget.charge(1)?;
                let attrs = doc.get_attribute_nodes(n);
                ctx.budget.charge(attrs.len())?;
                for &a in attrs {
                    if test.matches(a, doc) {
                        result.push(a);
                    }
                }
                Ok(())
            })?;
        } else {
            walk_descendants(doc, node, include_self, |n| {
                ctx.budget.charge(1)?;
                if test.matches(n, doc) {
                    result.push(n);
                }
                Ok(())
            })?;
        }
    }
    // One context: a pre-order walk is already unique and in document order.
    if context_nodes.len() == 1 {
        return Ok(result);
    }
    Ok(dedup_document_order(doc, result))
}

fn apply_step(step: &Step, context_nodes: &[NodeId], ctx: &EvalContext) -> XmlResult<Vec<NodeId>> {
    let doc = ctx.doc;
    // Resolve the test's namespace prefix once per step, not once per
    // candidate: the per-node check is then a kind match plus at most two
    // string compares with no hashing.
    let test = ResolvedTest::resolve(&step.node_test, ctx.namespaces);
    let mut result = Vec::new();
    for &node in context_nodes {
        let start = result.len();
        match step.axis {
            Axis::Child => {
                // Stream into the result instead of counting siblings, copying
                // the axis, and then allocating a separate filtered vector.
                // Charge every candidate, including non-matches, before push.
                for candidate in doc.children_iter(node) {
                    ctx.budget.charge(1)?;
                    if test.matches(candidate, doc) {
                        result.push(candidate);
                    }
                }
            }
            Axis::Attribute => {
                let candidates = doc.get_attribute_nodes(node);
                ctx.budget.charge(candidates.len())?;
                for &candidate in candidates {
                    if test.matches(candidate, doc) {
                        result.push(candidate);
                    }
                }
            }
            // Test descendants as they are visited rather than collecting the
            // whole subtree first and filtering a second time.
            Axis::Descendant | Axis::DescendantOrSelf => {
                let include_self = matches!(step.axis, Axis::DescendantOrSelf);
                walk_descendants(doc, node, include_self, |n| {
                    ctx.budget.charge(1)?;
                    if test.matches(n, doc) {
                        result.push(n);
                    }
                    Ok(())
                })?;
            }
            _ => {
                for candidate in select_axis(&step.axis, node, ctx)? {
                    if test.matches(candidate, doc) {
                        result.push(candidate);
                    }
                }
            }
        }
        for pred in &step.predicates {
            apply_predicate(pred, &mut result, start, ctx)?;
        }
    }
    // These axes are already unique and in document order for one context,
    // even without prepare_xpath(). Multiple (possibly nested) contexts still
    // require global ordering, as do the reverse and overlapping axes.
    if context_nodes.len() == 1
        && matches!(
            step.axis,
            Axis::Child | Axis::Attribute | Axis::Descendant | Axis::DescendantOrSelf
        )
    {
        return Ok(result);
    }
    Ok(dedup_document_order(doc, result))
}

/// Visit `root`'s descendants in document order (pre-order), and `root`
/// itself first when `include_self`. Follows first-child / next-sibling /
/// parent links, so the walk allocates nothing; `visit` may abort it with an
/// error (the node-visit budget).
fn walk_descendants(
    doc: &Document<'_>,
    root: NodeId,
    include_self: bool,
    mut visit: impl FnMut(NodeId) -> XmlResult<()>,
) -> XmlResult<()> {
    if include_self {
        visit(root)?;
    }
    let mut cur = match doc.first_child(root) {
        Some(c) => c,
        None => return Ok(()),
    };
    loop {
        visit(cur)?;
        if let Some(c) = doc.first_child(cur) {
            cur = c;
            continue;
        }
        loop {
            if let Some(s) = doc.next_sibling(cur) {
                cur = s;
                break;
            }
            match doc.parent(cur) {
                Some(p) if p != root => cur = p,
                _ => return Ok(()),
            }
        }
    }
}

fn apply_predicate(
    pred: &Expr,
    nodes: &mut Vec<NodeId>,
    start: usize,
    ctx: &EvalContext,
) -> XmlResult<()> {
    // Only this parent's suffix participates in position()/last(). Compact it
    // in place without touching results belonging to earlier context nodes.
    let size = nodes.len() - start;
    // Charge the candidate scan up front so a query that is already over budget
    // fails before doing the (potentially expensive) per-candidate predicate
    // evaluation, rather than after. This tightens the DoS bound the budget
    // is meant to provide.
    ctx.budget.charge(size)?;
    let mut kept = start;
    for i in 0..size {
        let node = nodes[start + i];
        let pred_ctx = EvalContext {
            node,
            current: ctx.current,
            position: i + 1,
            size,
            doc: ctx.doc,
            namespaces: ctx.namespaces,
            vars: ctx.vars,
            funcs: ctx.funcs,
            budget: ctx.budget,
        };
        let val = evaluate_expr(pred, &pred_ctx)?;
        let keep = match &val {
            XPathValue::Number(n) => (*n - (i + 1) as f64).abs() < f64::EPSILON,
            _ => val.to_boolean(),
        };
        if keep {
            nodes[kept] = node;
            kept += 1;
        }
    }
    nodes.truncate(kept);
    Ok(())
}

fn select_axis(axis: &Axis, node: NodeId, ctx: &EvalContext) -> XmlResult<Vec<NodeId>> {
    let doc = ctx.doc;
    match axis {
        Axis::Child => {
            // Count children and check the budget before allocating the result.
            // Exact capacity avoids Vec growth, at the cost of traversing the
            // sibling list twice instead of once as doc.children(node) does.
            let count = doc.children_count(node);
            ctx.budget.charge(count)?;
            let mut nodes = Vec::with_capacity(count);
            nodes.extend(doc.children_iter(node));
            Ok(nodes)
        }
        Axis::Descendant => collect_descendants(doc, node, false, ctx.budget),
        Axis::Parent => {
            let nodes: Vec<_> = doc.parent(node).into_iter().collect();
            ctx.budget.charge(nodes.len())?;
            Ok(nodes)
        }
        Axis::Ancestor => {
            let nodes = doc.ancestors(node);
            ctx.budget.charge(nodes.len())?;
            Ok(nodes)
        }
        Axis::Self_ => {
            ctx.budget.charge(1)?;
            Ok(vec![node])
        }
        Axis::DescendantOrSelf => collect_descendants(doc, node, true, ctx.budget),
        Axis::AncestorOrSelf => {
            let mut result = vec![node];
            result.extend(doc.ancestors(node));
            ctx.budget.charge(result.len())?;
            Ok(result)
        }
        Axis::FollowingSibling => {
            let mut result = Vec::new();
            let mut current = doc.next_sibling(node);
            while let Some(sib) = current {
                ctx.budget.charge(1)?;
                result.push(sib);
                current = doc.next_sibling(sib);
            }
            Ok(result)
        }
        Axis::PrecedingSibling => {
            let mut result = Vec::new();
            let mut current = doc.previous_sibling(node);
            while let Some(sib) = current {
                ctx.budget.charge(1)?;
                result.push(sib);
                current = doc.previous_sibling(sib);
            }
            Ok(result)
        }
        Axis::Following => collect_following(doc, node, ctx.budget),
        Axis::Preceding => collect_preceding(doc, node, ctx.budget),
        Axis::Attribute => {
            let nodes = doc.get_attribute_nodes(node).to_vec();
            ctx.budget.charge(nodes.len())?;
            Ok(nodes)
        }
        Axis::Namespace => Ok(Vec::new()),
    }
}

fn collect_descendants(
    doc: &Document<'_>,
    node: NodeId,
    include_self: bool,
    budget: &EvalBudget,
) -> XmlResult<Vec<NodeId>> {
    // Depth-first pre-order is required before positional predicates run.
    // Reverse children onto the stack to visit the first child first.
    let mut result = Vec::new();
    let mut stack = Vec::new();
    if include_self {
        stack.push(node);
    } else {
        stack.extend(doc.children_iter(node).rev());
    }

    while let Some(current) = stack.pop() {
        budget.charge(1)?;
        result.push(current);
        stack.extend(doc.children_iter(current).rev());
    }

    Ok(result)
}

fn collect_following(
    doc: &Document<'_>,
    node: NodeId,
    budget: &EvalBudget,
) -> XmlResult<Vec<NodeId>> {
    let mut result = Vec::new();
    let mut current = node;
    loop {
        if let Some(next) = doc.next_sibling(current) {
            budget.charge(1)?;
            result.push(next);
            result.extend(collect_descendants(doc, next, false, budget)?);
            current = next;
            continue;
        }
        if let Some(parent) = doc.parent(current) {
            current = parent;
        } else {
            break;
        }
    }
    Ok(result)
}

fn collect_preceding(
    doc: &Document<'_>,
    node: NodeId,
    budget: &EvalBudget,
) -> XmlResult<Vec<NodeId>> {
    let mut result = Vec::new();
    let mut current = node;
    loop {
        if let Some(prev) = doc.previous_sibling(current) {
            let descs = collect_descendants(doc, prev, false, budget)?;
            for d in descs.into_iter().rev() {
                result.push(d);
            }
            budget.charge(1)?;
            result.push(prev);
            current = prev;
            continue;
        }
        if let Some(parent) = doc.parent(current) {
            if doc.parent(parent).is_some() {
                current = parent;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    Ok(result)
}

fn matches_node_test(
    test: &NodeTest,
    node: NodeId,
    doc: &Document<'_>,
    namespaces: &HashMap<String, String>,
) -> bool {
    ResolvedTest::resolve(test, namespaces).matches(node, doc)
}

/// A [`NodeTest`] with its namespace prefix already looked up, so a step can
/// resolve once and test each candidate with plain comparisons. An unbound
/// prefix resolves to `Never` (XPath 1.0 §2.3: the test matches nothing).
enum ResolvedTest<'t> {
    AnyNode,
    Wildcard,
    Name(&'t str),
    QName { ns: &'t str, local: &'t str },
    NsWildcard(&'t str),
    Text,
    Comment,
    Pi,
    Never,
}

impl<'t> ResolvedTest<'t> {
    fn resolve(test: &'t NodeTest, namespaces: &'t HashMap<String, String>) -> Self {
        match test {
            NodeTest::Wildcard => ResolvedTest::Wildcard,
            NodeTest::Name(name) => ResolvedTest::Name(name),
            NodeTest::PrefixedName(prefix, local) => match namespaces.get(prefix) {
                Some(ns) => ResolvedTest::QName { ns, local },
                None => ResolvedTest::Never,
            },
            NodeTest::PrefixWildcard(prefix) => match namespaces.get(prefix) {
                Some(ns) => ResolvedTest::NsWildcard(ns),
                None => ResolvedTest::Never,
            },
            NodeTest::NodeType(nt) => match nt.as_str() {
                "node" => ResolvedTest::AnyNode,
                "text" => ResolvedTest::Text,
                "comment" => ResolvedTest::Comment,
                "processing-instruction" => ResolvedTest::Pi,
                _ => ResolvedTest::Never,
            },
        }
    }

    #[inline]
    fn matches(&self, node: NodeId, doc: &Document<'_>) -> bool {
        if matches!(self, ResolvedTest::AnyNode) {
            return true;
        }
        let kind = match doc.node_kind(node) {
            Some(k) => k,
            None => return false,
        };
        // Elements and attributes share the name tests.
        let qname = match kind {
            NodeKind::Element(e) => Some(&e.name),
            NodeKind::Attribute(qn, _) => Some(qn),
            _ => None,
        };
        match self {
            ResolvedTest::AnyNode => true,
            ResolvedTest::Wildcard => qname.is_some(),
            ResolvedTest::Name(name) => {
                qname.is_some_and(|q| q.namespace_uri.is_none() && *q.local_name == **name)
            }
            ResolvedTest::QName { ns, local } => qname.is_some_and(|q| {
                *q.local_name == **local && q.namespace_uri.as_deref() == Some(*ns)
            }),
            ResolvedTest::NsWildcard(ns) => {
                qname.is_some_and(|q| q.namespace_uri.as_deref() == Some(*ns))
            }
            ResolvedTest::Text => matches!(kind, NodeKind::Text(_) | NodeKind::CData(_)),
            ResolvedTest::Comment => matches!(kind, NodeKind::Comment(_)),
            ResolvedTest::Pi => matches!(kind, NodeKind::ProcessingInstruction(_)),
            ResolvedTest::Never => false,
        }
    }
}

/// XPath 1.0 §4.4 `round()`: the nearest integer, with a half-way value
/// rounded toward positive infinity — `round(-2.5)` is `-2`, where
/// `f64::round` (ties away from zero) gives `-3`. The two agree everywhere
/// else, so only negative exact halves are corrected. NaN and ±inf pass
/// through, and `round(-0.5)` keeps the negative-zero sign the spec assigns.
fn xpath_round(n: f64) -> f64 {
    let r = n.round();
    if n - r == 0.5 {
        // Away-from-zero took a negative exact half down; ties go up.
        let up = r + 1.0;
        if up == 0.0 {
            -0.0
        } else {
            up
        }
    } else {
        r
    }
}

fn evaluate_function(name: &str, args: &[Expr], ctx: &EvalContext) -> XmlResult<XPathValue> {
    match name {
        "last" => Ok(XPathValue::Number(ctx.size as f64)),
        "position" => Ok(XPathValue::Number(ctx.position as f64)),
        // XSLT `current()` — the node the outermost expression is being
        // evaluated against (distinct from the predicate context node). For
        // plain XPath this equals the context node.
        "current" => {
            if !args.is_empty() {
                return Err(XmlError::xpath("current() takes no arguments"));
            }
            Ok(XPathValue::NodeSet(vec![ctx.current]))
        }
        "lang" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("lang() takes exactly 1 argument"));
            }
            let target = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            Ok(XPathValue::Boolean(node_lang_matches(
                ctx.doc, ctx.node, &target,
            )))
        }
        "count" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("count() takes exactly 1 argument"));
            }
            let val = evaluate_expr(&args[0], ctx)?;
            Ok(XPathValue::Number(val.as_node_set().len() as f64))
        }
        "local-name" => {
            let node = if args.is_empty() {
                ctx.node
            } else {
                let val = evaluate_expr(&args[0], ctx)?;
                match val.as_node_set().first() {
                    Some(&n) => n,
                    None => return Ok(XPathValue::String(String::new())),
                }
            };
            let name = match ctx.doc.node_kind(node) {
                Some(NodeKind::Element(e)) => e.name.local_name.to_string(),
                Some(NodeKind::Attribute(qn, _)) => qn.local_name.to_string(),
                Some(NodeKind::ProcessingInstruction(pi)) => pi.target.to_string(),
                _ => String::new(),
            };
            Ok(XPathValue::String(name))
        }
        "namespace-uri" => {
            let node = if args.is_empty() {
                ctx.node
            } else {
                let val = evaluate_expr(&args[0], ctx)?;
                match val.as_node_set().first() {
                    Some(&n) => n,
                    None => return Ok(XPathValue::String(String::new())),
                }
            };
            let uri = match ctx.doc.node_kind(node) {
                Some(NodeKind::Element(e)) => {
                    e.name.namespace_uri.as_deref().unwrap_or("").to_string()
                }
                Some(NodeKind::Attribute(qn, _)) => {
                    qn.namespace_uri.as_deref().unwrap_or("").to_string()
                }
                _ => String::new(),
            };
            Ok(XPathValue::String(uri))
        }
        "name" => {
            let node = if args.is_empty() {
                ctx.node
            } else {
                let val = evaluate_expr(&args[0], ctx)?;
                match val.as_node_set().first() {
                    Some(&n) => n,
                    None => return Ok(XPathValue::String(String::new())),
                }
            };
            let name = match ctx.doc.node_kind(node) {
                Some(NodeKind::Element(e)) => e.name.prefixed_name().into_owned(),
                Some(NodeKind::Attribute(qn, _)) => qn.prefixed_name().into_owned(),
                Some(NodeKind::ProcessingInstruction(pi)) => pi.target.to_string(),
                _ => String::new(),
            };
            Ok(XPathValue::String(name))
        }
        "string" => {
            if args.is_empty() {
                Ok(XPathValue::String(string_value_of_node(ctx.doc, ctx.node)))
            } else {
                let val = evaluate_expr(&args[0], ctx)?;
                Ok(XPathValue::String(val.to_string_value(ctx.doc)))
            }
        }
        "concat" => {
            if args.len() < 2 {
                return Err(XmlError::xpath("concat() takes at least 2 arguments"));
            }
            let mut result = String::new();
            for arg in args {
                let val = evaluate_expr(arg, ctx)?;
                result.push_str(&val.to_string_value(ctx.doc));
            }
            Ok(XPathValue::String(result))
        }
        "starts-with" => {
            if args.len() != 2 {
                return Err(XmlError::xpath("starts-with() takes exactly 2 arguments"));
            }
            let s = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let prefix = evaluate_expr(&args[1], ctx)?.to_string_value(ctx.doc);
            Ok(XPathValue::Boolean(s.starts_with(&prefix)))
        }
        "contains" => {
            if args.len() != 2 {
                return Err(XmlError::xpath("contains() takes exactly 2 arguments"));
            }
            let s = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let sub = evaluate_expr(&args[1], ctx)?.to_string_value(ctx.doc);
            Ok(XPathValue::Boolean(s.contains(&sub)))
        }
        "substring" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(XmlError::xpath("substring() takes 2 or 3 arguments"));
            }
            let s = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            // XPath 1.0 §4.2: character positions are 1-based and selected by
            // numeric comparison — position >= round(start) and, in the 3-arg
            // form, position < round(start) + round(length), with `round` the
            // XPath ties-toward-+inf rounding (`xpath_round`, shared with the
            // round() function so `substring(s, -2.5)` and
            // `substring(s, round(-2.5))` agree). Comparing in f64 keeps the
            // spec's NaN/infinity semantics (a NaN bound selects nothing, an
            // infinite length everything after start) and has no integer
            // casts, whose saturation previously overflowed on a
            // huge-negative or -inf start (`i64::MIN - 1`, fuzz_xpath).
            let start = xpath_round(evaluate_expr(&args[1], ctx)?.to_number(ctx.doc));
            let end = if args.len() == 3 {
                start + xpath_round(evaluate_expr(&args[2], ctx)?.to_number(ctx.doc))
            } else {
                f64::INFINITY
            };
            let result: String = s
                .chars()
                .enumerate()
                .filter(|(i, _)| {
                    let p = (i + 1) as f64;
                    p >= start && p < end
                })
                .map(|(_, c)| c)
                .collect();
            Ok(XPathValue::String(result))
        }
        "substring-before" => {
            if args.len() != 2 {
                return Err(XmlError::xpath(
                    "substring-before() takes exactly 2 arguments",
                ));
            }
            let s = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let sub = evaluate_expr(&args[1], ctx)?.to_string_value(ctx.doc);
            let result = if let Some(pos) = s.find(&sub) {
                s[..pos].to_string()
            } else {
                String::new()
            };
            Ok(XPathValue::String(result))
        }
        "substring-after" => {
            if args.len() != 2 {
                return Err(XmlError::xpath(
                    "substring-after() takes exactly 2 arguments",
                ));
            }
            let s = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let sub = evaluate_expr(&args[1], ctx)?.to_string_value(ctx.doc);
            let result = if let Some(pos) = s.find(&sub) {
                s[pos + sub.len()..].to_string()
            } else {
                String::new()
            };
            Ok(XPathValue::String(result))
        }
        "string-length" => {
            let s = if args.is_empty() {
                string_value_of_node(ctx.doc, ctx.node)
            } else {
                evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc)
            };
            Ok(XPathValue::Number(s.chars().count() as f64))
        }
        "normalize-space" => {
            let s = if args.is_empty() {
                string_value_of_node(ctx.doc, ctx.node)
            } else {
                evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc)
            };
            let normalized: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            Ok(XPathValue::String(normalized))
        }
        "translate" => {
            if args.len() != 3 {
                return Err(XmlError::xpath("translate() takes exactly 3 arguments"));
            }
            let s = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let from = evaluate_expr(&args[1], ctx)?.to_string_value(ctx.doc);
            let to = evaluate_expr(&args[2], ctx)?.to_string_value(ctx.doc);
            let from_chars: Vec<char> = from.chars().collect();
            let to_chars: Vec<char> = to.chars().collect();
            let result: String = s
                .chars()
                .filter_map(|c| {
                    if let Some(pos) = from_chars.iter().position(|&fc| fc == c) {
                        to_chars.get(pos).copied()
                    } else {
                        Some(c)
                    }
                })
                .collect();
            Ok(XPathValue::String(result))
        }
        "not" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("not() takes exactly 1 argument"));
            }
            let val = evaluate_expr(&args[0], ctx)?;
            Ok(XPathValue::Boolean(!val.to_boolean()))
        }
        "true" => Ok(XPathValue::Boolean(true)),
        "false" => Ok(XPathValue::Boolean(false)),
        "boolean" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("boolean() takes exactly 1 argument"));
            }
            let val = evaluate_expr(&args[0], ctx)?;
            Ok(XPathValue::Boolean(val.to_boolean()))
        }
        "number" => {
            if args.is_empty() {
                Ok(XPathValue::Number(
                    string_value_of_node(ctx.doc, ctx.node)
                        .trim()
                        .parse::<f64>()
                        .unwrap_or(f64::NAN),
                ))
            } else {
                let val = evaluate_expr(&args[0], ctx)?;
                Ok(XPathValue::Number(val.to_number(ctx.doc)))
            }
        }
        "sum" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("sum() takes exactly 1 argument"));
            }
            let val = evaluate_expr(&args[0], ctx)?;
            let mut total = 0.0f64;
            for &node in val.as_node_set() {
                let sv = string_value_of_node(ctx.doc, node);
                total += sv.trim().parse::<f64>().unwrap_or(f64::NAN);
            }
            Ok(XPathValue::Number(total))
        }
        "floor" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("floor() takes exactly 1 argument"));
            }
            let n = evaluate_expr(&args[0], ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(n.floor()))
        }
        "ceiling" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("ceiling() takes exactly 1 argument"));
            }
            let n = evaluate_expr(&args[0], ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(n.ceil()))
        }
        "round" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("round() takes exactly 1 argument"));
            }
            let n = evaluate_expr(&args[0], ctx)?.to_number(ctx.doc);
            Ok(XPathValue::Number(xpath_round(n)))
        }
        "id" => {
            if args.len() != 1 {
                return Err(XmlError::xpath("id() takes exactly 1 argument"));
            }
            let val = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let ids: Vec<&str> = val.split_whitespace().collect();
            if ids.is_empty() {
                return Ok(XPathValue::NodeSet(Vec::new()));
            }
            let mut result = Vec::new();
            collect_elements_with_id(ctx.doc, ctx.doc.root(), &ids, &mut result, ctx.budget)?;
            Ok(XPathValue::NodeSet(result))
        }
        _ => {
            // Not a core XPath 1.0 function. Evaluate the arguments and consult
            // the injected resolver (XSLT/EXSLT extension functions); fall back
            // to an error if it declines.
            let (prefix, local) = split_qname(name);
            let mut arg_vals = Vec::with_capacity(args.len());
            for a in args {
                arg_vals.push(evaluate_expr(a, ctx)?);
            }
            match ctx.funcs.resolve_function(prefix, local, &arg_vals) {
                Some(result) => result,
                None => Err(XmlError::xpath(format!("Unknown function: {}()", name))),
            }
        }
    }
}

/// Implements XPath `lang(string)`: true if the language of the context node
/// (per the nearest `xml:lang` in scope) is `target` or a sub-language of it.
fn node_lang_matches(doc: &Document<'_>, node: NodeId, target: &str) -> bool {
    // Walk ancestor-or-self looking for an `xml:lang` attribute.
    let mut cur = Some(node);
    while let Some(n) = cur {
        if let Some(NodeKind::Element(e)) = doc.node_kind(n) {
            for attr in &e.attributes {
                let is_xml_lang = attr.name.local_name.as_ref() == "lang"
                    && attr.name.prefix.as_deref() == Some("xml");
                if is_xml_lang {
                    let lang = attr.value.as_ref();
                    let t = target.to_ascii_lowercase();
                    let l = lang.to_ascii_lowercase();
                    // Match the full tag or a `lang-...` subtag prefix.
                    return l == t || l.strip_prefix(&t).is_some_and(|r| r.starts_with('-'));
                }
            }
        }
        cur = doc.parent(n);
    }
    false
}

fn collect_elements_with_id(
    doc: &Document<'_>,
    node: NodeId,
    ids: &[&str],
    result: &mut Vec<NodeId>,
    budget: &EvalBudget,
) -> XmlResult<()> {
    let mut stack = vec![node];

    while let Some(current) = stack.pop() {
        budget.charge(1)?;
        if let Some(NodeKind::Element(e)) = doc.node_kind(current) {
            for attr in &e.attributes {
                if (&*attr.name.local_name == "id" || &*attr.name.local_name == "ID")
                    && ids.contains(&&*attr.value)
                {
                    result.push(current);
                    break;
                }
            }
        }

        for child in doc.children_iter(current).rev() {
            stack.push(child);
        }
    }

    Ok(())
}

/// Remove duplicate NodeIds and return them in the document's current tree order.
fn dedup_document_order(doc: &Document<'_>, mut nodes: Vec<NodeId>) -> Vec<NodeId> {
    if nodes.len() <= 1 {
        return nodes; // nothing to order or dedup
    }

    // Best path: a precomputed document-order index (populated by
    // `prepare_xpath`, so always present for XSLT). Sorting by its O(1) key
    // handles every case — same-parent or spanning parents — without walking the
    // tree. This is what keeps relative paths evaluated per node (e.g. an XSLT
    // template testing `name/text()` on each of thousands of siblings) linear
    // rather than O(width^2).
    if doc.doc_order_ready() {
        // Most forward steps already produce ordered nodes. Avoid allocating
        // cached sort keys in that case; retain the stable fallback for equal
        // keys (including detached nodes) and out-of-order context streams.
        if !nodes.is_sorted_by_key(|&node| doc.doc_order_at(node)) {
            nodes.sort_by_cached_key(|&node| doc.doc_order_at(node));
        }
        nodes.dedup();
        return nodes;
    }

    // Fast path (index not prepared): when every node shares one parent —
    // overwhelmingly common, e.g. an axis step's result or a sibling union like
    // `@*|node()` — relative document order is fully determined by each node's
    // position *within that parent* (attributes, in attribute order, before
    // children, in child order). Ordering by a local key avoids
    // `document_order_key`'s walk to the document root.
    if let Some(parent) = doc.parent(nodes[0]) {
        if nodes[1..].iter().all(|&n| doc.parent(n) == Some(parent)) {
            let mut order = SiblingOrderIndex::default();
            nodes.sort_by_cached_key(|&n| local_order_key(doc, parent, n, &mut order));
            nodes.dedup();
            return nodes;
        }
    }

    // General fallback. A sibling-index memo turns each `position()` lookup from
    // an O(siblings) scan into an amortized O(1) hash lookup. Without it, sorting
    // a wide node-set is O(n^2) (each of n nodes re-scans its parent's child
    // list), which is *uncharged* work that defeats the node-visit budget — a
    // disjoint `(/r/a) = (/r/b)` comparison would spend minutes inside dedup
    // before the comparison charge ever fires.
    let mut order = SiblingOrderIndex::default();
    nodes.sort_by_cached_key(|&node| document_order_key(doc, node, &mut order));
    nodes.dedup();
    nodes
}

/// Document-order sort key for a node *known to share `parent` with all others
/// being sorted*: attributes (tuple tag 0) precede children (tag 1), each ordered
/// by position within its respective list. Indexes only `parent`'s lists.
fn local_order_key(
    doc: &Document<'_>,
    parent: NodeId,
    node: NodeId,
    order: &mut SiblingOrderIndex,
) -> (u8, usize) {
    if matches!(doc.node_kind(node), Some(NodeKind::Attribute(_, _))) {
        (0, order.attr_pos(doc, parent, node).unwrap_or(usize::MAX))
    } else {
        (1, order.child_pos(doc, parent, node).unwrap_or(usize::MAX))
    }
}

/// Memoizes the position of each node within its parent's child / attribute
/// list. The first lookup for a given parent indexes that parent's whole list
/// once; subsequent sibling lookups are O(1).
#[derive(Default)]
struct SiblingOrderIndex {
    pos: HashMap<NodeId, usize>,
    children_indexed: HashSet<NodeId>,
    attrs_indexed: HashSet<NodeId>,
}

impl SiblingOrderIndex {
    fn child_pos(&mut self, doc: &Document<'_>, parent: NodeId, child: NodeId) -> Option<usize> {
        if self.children_indexed.insert(parent) {
            for (i, c) in doc.children_iter(parent).enumerate() {
                self.pos.insert(c, i);
            }
        }
        self.pos.get(&child).copied()
    }

    fn attr_pos(&mut self, doc: &Document<'_>, parent: NodeId, attr: NodeId) -> Option<usize> {
        if self.attrs_indexed.insert(parent) {
            for (i, a) in doc.get_attribute_nodes(parent).iter().enumerate() {
                self.pos.insert(*a, i);
            }
        }
        self.pos.get(&attr).copied()
    }
}

fn document_order_key(
    doc: &Document<'_>,
    node: NodeId,
    order: &mut SiblingOrderIndex,
) -> (u8, Vec<(u8, usize)>, usize) {
    let mut path = Vec::new();
    let mut current = node;

    loop {
        if current == doc.root() {
            path.reverse();
            return (0, path, node.index());
        }

        let Some(parent) = doc.parent(current) else {
            return (1, Vec::new(), node.index());
        };

        if matches!(doc.node_kind(current), Some(NodeKind::Attribute(_, _))) {
            let Some(index) = order.attr_pos(doc, parent, current) else {
                return (1, Vec::new(), node.index());
            };
            path.push((0, index));
        } else {
            let Some(index) = order.child_pos(doc, parent, current) else {
                return (1, Vec::new(), node.index());
            };
            path.push((1, index));
        }

        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    #[test]
    fn cache_reuses_programs_and_clears_on_depth_change() {
        let doc = crate::parse("<r/>").unwrap();
        let mut eval = XPathEvaluator::new();
        let expression = "((1 + 1))";
        for _ in 0..2 {
            assert_eq!(
                eval.evaluate(&doc, doc.root(), expression)
                    .unwrap()
                    .to_number(&doc),
                2.0
            );
        }
        assert_eq!(eval.program_cache.lock().unwrap().programs.len(), 1);
        eval = eval.with_max_depth(1);
        assert_eq!(eval.program_cache.lock().unwrap().retained_bytes, 0);
        assert!(eval.evaluate(&doc, doc.root(), expression).is_err());
        assert!(eval.evaluate(&doc, doc.root(), "1 + 1").is_ok());
        eval.clear_cache();
        assert!(eval.program_cache.lock().unwrap().programs.is_empty());
        assert_eq!(eval.program_cache.lock().unwrap().retained_bytes, 0);
    }

    #[test]
    fn cache_skips_large_sources_and_failed_evaluations() {
        let doc = crate::parse("<r/>").unwrap();
        let eval = XPathEvaluator::new();
        let text = "x".repeat(CACHE_SOURCE_LIMIT);
        let expression = format!("'{text}'");
        assert_eq!(
            eval.evaluate(&doc, doc.root(), &expression)
                .unwrap()
                .to_string_value(&doc),
            text
        );
        assert!(eval.evaluate(&doc, doc.root(), "$missing").is_err());
        assert!(eval.program_cache.lock().unwrap().programs.is_empty());
    }

    #[test]
    fn cache_enforces_owned_capacity_and_aggregate_limits() {
        let mut cache = ProgramCache::default();
        // Capacity, rather than string length, determines retained allocation.
        cache.insert(
            "large",
            std::sync::Arc::new(Expr::StringLiteral(String::with_capacity(
                CACHE_ENTRY_BYTES,
            ))),
        );
        assert!(cache.programs.is_empty());
        for i in 0..CACHE_LIMIT {
            cache.insert(
                &i.to_string(),
                std::sync::Arc::new(Expr::StringLiteral(String::with_capacity(32 * 1024))),
            );
        }
        assert!(!cache.programs.is_empty());
        assert!(cache.programs.len() < CACHE_LIMIT);
        assert!(cache.retained_bytes <= CACHE_TOTAL_BYTES);
        let bytes = cache.retained_bytes;
        cache.insert("0", std::sync::Arc::new(Expr::NumberLiteral(0.0)));
        assert_eq!(cache.retained_bytes, bytes);
    }

    #[test]
    fn cache_accounts_for_nested_ast_allocations() {
        let literal = String::with_capacity(200);
        let args = vec![Expr::StringLiteral(literal)];
        let argument_bytes = args.capacity() * std::mem::size_of::<Expr>() + 200;
        let name = String::from("string");
        let call_bytes = name.capacity() + argument_bytes;
        let predicates = vec![Expr::Negate(Box::new(Expr::FunctionCall(name, args)))];
        let predicate_bytes = predicates.capacity() * std::mem::size_of::<Expr>()
            + std::mem::size_of::<Expr>()
            + call_bytes;
        let prefix = String::from("p");
        let local = String::from("item");
        let names_bytes = prefix.capacity() + local.capacity();
        let steps = vec![Step {
            axis: Axis::Child,
            node_test: NodeTest::PrefixedName(prefix, local),
            predicates,
        }];
        let expected =
            steps.capacity() * std::mem::size_of::<Step>() + names_bytes + predicate_bytes;
        assert_eq!(Expr::Path(steps).heap_bytes(), expected);
    }

    #[test]
    fn cache_concurrent_misses_do_not_duplicate_accounting() {
        let doc = crate::parse("<r/>").unwrap();
        let eval = XPathEvaluator::new();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..20 {
                        assert!(eval.evaluate(&doc, doc.root(), "1 + 1").is_ok());
                    }
                });
            }
        });
        let cache = eval.program_cache.lock().unwrap();
        assert_eq!(cache.programs.len(), 1);
        let bytes = cache.retained_bytes;
        drop(cache);
        let fresh = XPathEvaluator::new();
        fresh.evaluate(&doc, doc.root(), "1 + 1").unwrap();
        assert_eq!(fresh.program_cache.lock().unwrap().retained_bytes, bytes);
    }

    fn parse_and_eval(xml: &str, xpath: &str) -> XPathValue {
        let doc = Parser::new().parse(xml).unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        eval.evaluate(&doc, root, xpath).unwrap()
    }

    /// Node ids selected by `expr`, with `md` bound for namespaced tests.
    fn ids(doc: &Document<'_>, expr: &str) -> Vec<NodeId> {
        let mut eval = XPathEvaluator::new();
        eval.add_namespace("md", "urn:md");
        let root = doc.document_element().unwrap();
        eval.select_nodes(doc, root, expr).unwrap()
    }

    const FUSE_DOC: &str = "<r xmlns:m='urn:md'><a id='1'><b id='2'/><a id='3'><b id='4'/>\
        <m:b id='5'/></a></a><b id='6'><c/></b><!--x-->t<?pi d?><m:b id='7'/></r>";

    /// `FUSE_DOC` parsed with the attribute-node index built, as the attribute
    /// axis requires.
    fn fuse_doc() -> Document<'static> {
        let mut doc = Parser::new().parse(FUSE_DOC).unwrap();
        doc.prepare_xpath();
        doc
    }

    #[test]
    fn fused_double_slash_matches_explicit_steps() {
        // `//T` and `//@T` are evaluated as one descendant walk; the explicit
        // two-step spelling must select exactly the same nodes in the same order.
        let doc = fuse_doc();
        for (fused, explicit) in [
            ("//b", "/descendant-or-self::node()/child::b"),
            ("//md:b", "/descendant-or-self::node()/child::md:b"),
            ("//md:*", "/descendant-or-self::node()/child::md:*"),
            ("//*", "/descendant-or-self::node()/child::*"),
            ("//node()", "/descendant-or-self::node()/child::node()"),
            ("//text()", "/descendant-or-self::node()/child::text()"),
            (
                "//comment()",
                "/descendant-or-self::node()/child::comment()",
            ),
            ("//@id", "/descendant-or-self::node()/attribute::id"),
            ("//@*", "/descendant-or-self::node()/attribute::*"),
            (".//b", "./descendant-or-self::node()/child::b"),
            ("a//b", "a/descendant-or-self::node()/child::b"),
            // Nested contexts overlap: the dedup after the fused walk must hold.
            (
                "//a//b",
                "/descendant-or-self::node()/child::a/descendant-or-self::node()/child::b",
            ),
            ("//a//@id", "//a/descendant-or-self::node()/attribute::id"),
        ] {
            assert_eq!(ids(&doc, fused), ids(&doc, explicit), "{fused}");
            assert!(!ids(&doc, fused).is_empty(), "{fused} selected nothing");
        }
        assert_eq!(ids(&doc, "//b").len(), 3);
        assert_eq!(ids(&doc, "//md:b").len(), 2);
        assert_eq!(ids(&doc, "//@id").len(), 7);
        assert_eq!(ids(&doc, "//a//b").len(), 2);
    }

    #[test]
    fn double_slash_with_predicates_keeps_per_parent_positions() {
        // A predicate on the step after `//` is positional relative to the
        // parent, so the pair is not fused: `//b[1]` is the first `b` child of
        // every parent, not the first `b` in the document.
        let doc = fuse_doc();
        assert_eq!(ids(&doc, "//b[1]").len(), 3);
        assert_eq!(ids(&doc, "//b[last()]").len(), 3);
        assert_eq!(ids(&doc, "//b[@id='4']").len(), 1);
        assert_eq!(
            ids(&doc, "//b[1]"),
            ids(&doc, "/descendant-or-self::node()/child::b[1]")
        );
    }

    #[test]
    fn resolved_test_unbound_prefix_matches_nothing() {
        let doc = fuse_doc();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        assert!(eval.select_nodes(&doc, root, "//zz:b").unwrap().is_empty());
        assert!(eval.select_nodes(&doc, root, "//zz:*").unwrap().is_empty());
        assert!(eval
            .select_nodes(&doc, root, "descendant::zz:b")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn descendant_axis_from_attribute_and_leaf_contexts() {
        let doc = fuse_doc();
        // A leaf element has no descendants; an attribute context has none and
        // its `//` walk must not escape to the owner's siblings.
        assert!(ids(&doc, "//c//*").is_empty());
        assert!(ids(&doc, "//@id//*").is_empty());
        assert!(ids(&doc, "//c/descendant::node()").is_empty());
        assert_eq!(ids(&doc, "//b[@id='6']/descendant-or-self::*").len(), 2);
        // count() over the fused form equals the explicit form.
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        let n = |e: &str| eval.evaluate(&doc, root, e).unwrap().to_number(&doc);
        assert_eq!(
            n("count(//*)"),
            n("count(/descendant-or-self::node()/child::*)")
        );
        assert_eq!(n("count(//*)"), 9.0);
    }

    #[test]
    fn test_child_elements() {
        let doc = Parser::new().parse("<root><a/><b/><c/></root>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        let result = eval.select_nodes(&doc, root, "*").unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_descendant_elements() {
        let doc = Parser::new().parse("<root><a><b/></a><c/></root>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        let result = eval.select_nodes(&doc, root, ".//b").unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_predicate_position() {
        let doc = Parser::new()
            .parse("<root><item>1</item><item>2</item><item>3</item></root>")
            .unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        let result = eval.select_nodes(&doc, root, "item[2]").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(doc.text_content_deep(result[0]), "2");
    }

    #[test]
    fn test_absolute_path() {
        let doc = Parser::new().parse("<root><a><b/></a></root>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        let result = eval.select_nodes(&doc, root, "/root/a/b").unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_text_function() {
        let val = parse_and_eval("<root>hello</root>", "string()");
        match val {
            XPathValue::String(s) => assert_eq!(s, "hello"),
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_count_function() {
        let val = parse_and_eval("<root><a/><a/><a/></root>", "count(a)");
        match val {
            XPathValue::Number(n) => assert_eq!(n, 3.0),
            _ => panic!("Expected number"),
        }
    }

    #[test]
    fn test_boolean_expression() {
        let val = parse_and_eval("<root><a/></root>", "1 = 1");
        assert!(val.to_boolean());
    }

    #[test]
    fn test_not_function() {
        let val = parse_and_eval("<root/>", "not(false())");
        assert!(val.to_boolean());
    }

    #[test]
    fn test_string_functions() {
        let val = parse_and_eval("<root/>", "concat('hello', ' ', 'world')");
        match val {
            XPathValue::String(s) => assert_eq!(s, "hello world"),
            _ => panic!("Expected string"),
        }

        let val = parse_and_eval("<root/>", "starts-with('hello', 'hel')");
        assert!(val.to_boolean());

        let val = parse_and_eval("<root/>", "contains('hello world', 'lo wo')");
        assert!(val.to_boolean());

        let val = parse_and_eval("<root/>", "string-length('hello')");
        match val {
            XPathValue::Number(n) => assert_eq!(n, 5.0),
            _ => panic!("Expected number"),
        }
    }

    /// F-08: deeply-nested `(...)` groups must be rejected cleanly
    /// instead of stack-overflowing the parser.
    #[test]
    fn test_xpath_paren_depth_cap() {
        let mut expr = String::new();
        for _ in 0..1000 {
            expr.push('(');
        }
        expr.push('1');
        for _ in 0..1000 {
            expr.push(')');
        }
        let doc = crate::parse("<r/>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.root();
        let err = eval
            .evaluate(&doc, root, &expr)
            .expect_err("deep paren nesting must be rejected");
        assert!(
            format!("{}", err).contains("maximum depth"),
            "expected depth-cap error, got: {}",
            err
        );
    }

    /// F-08: deeply-nested `[...]` predicates must be rejected.
    #[test]
    fn test_xpath_predicate_depth_cap() {
        // Build `a[a[a[...a[1]...]]]` with 500 nested predicates.
        let mut expr = String::from("a");
        for _ in 0..500 {
            expr.push_str("[a");
        }
        expr.push_str("[1]");
        for _ in 0..500 {
            expr.push(']');
        }
        let doc = crate::parse("<r><a/></r>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.root();
        let err = eval
            .evaluate(&doc, root, &expr)
            .expect_err("deep predicate nesting must be rejected");
        assert!(
            format!("{}", err).contains("maximum depth"),
            "expected depth-cap error, got: {}",
            err
        );
    }

    /// F-08: chained leading unary `-` must be rejected before the
    /// `parse_unary_expr` recursion blows the stack.
    #[test]
    fn test_xpath_unary_minus_depth_cap() {
        let mut expr = String::new();
        for _ in 0..1000 {
            expr.push('-');
        }
        expr.push('1');
        let doc = crate::parse("<r/>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.root();
        let err = eval
            .evaluate(&doc, root, &expr)
            .expect_err("deep unary-minus chain must be rejected");
        assert!(
            format!("{}", err).contains("maximum depth"),
            "expected depth-cap error, got: {}",
            err
        );
    }

    /// Legitimate nesting well under the cap still works.
    #[test]
    fn test_xpath_moderate_nesting_evaluates() {
        let mut expr = String::new();
        for _ in 0..10 {
            expr.push('(');
        }
        expr.push('1');
        for _ in 0..10 {
            expr.push(')');
        }
        let doc = crate::parse("<r/>").unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.root();
        let val = eval.evaluate(&doc, root, &expr).expect("10-deep evaluates");
        match val {
            XPathValue::Number(n) => assert_eq!(n, 1.0),
            _ => panic!("expected number"),
        }
    }

    /// F-1 (review follow-up): custom cap via `with_max_depth` must
    /// fire at the configured value.
    #[test]
    fn test_xpath_with_custom_max_depth() {
        let mut expr = String::new();
        for _ in 0..10 {
            expr.push('(');
        }
        expr.push('1');
        for _ in 0..10 {
            expr.push(')');
        }
        let doc = crate::parse("<r/>").unwrap();
        let root = doc.root();

        // Tight cap of 5 rejects the 10-deep expression.
        let eval = XPathEvaluator::new().with_max_depth(5);
        assert!(
            eval.evaluate(&doc, root, &expr).is_err(),
            "cap of 5 must reject 10-deep expression"
        );

        // Loose cap of 20 admits the same expression.
        let eval = XPathEvaluator::new().with_max_depth(20);
        let val = eval
            .evaluate(&doc, root, &expr)
            .expect("cap of 20 must admit 10-deep expression");
        match val {
            XPathValue::Number(n) => assert_eq!(n, 1.0),
            _ => panic!("expected number"),
        }
    }

    // ── M0: variable + function-resolution seam ──

    /// A test resolver binding a fixed set of `$name` → value pairs.
    struct MapVars(HashMap<String, XPathValue>);
    impl VariableResolver for MapVars {
        fn resolve_variable(&self, prefix: Option<&str>, local: &str) -> Option<XPathValue> {
            let key = match prefix {
                Some(p) => format!("{}:{}", p, local),
                None => local.to_string(),
            };
            self.0.get(&key).cloned()
        }
    }

    /// A test resolver implementing `t:double(n)` → 2n and `t:hi()` → "hi".
    struct TestFns;
    impl FunctionResolver for TestFns {
        fn resolve_function(
            &self,
            prefix: Option<&str>,
            local: &str,
            args: &[XPathValue],
        ) -> Option<XmlResult<XPathValue>> {
            match (prefix, local) {
                (Some("t"), "double") => {
                    let n = args.first().map(|v| v.to_number_isolated()).unwrap_or(0.0);
                    Some(Ok(XPathValue::Number(n * 2.0)))
                }
                (Some("t"), "hi") => Some(Ok(XPathValue::String("hi".to_string()))),
                _ => None,
            }
        }
    }

    impl XPathValue {
        /// Number coercion not needing a document (test helper for scalar values).
        fn to_number_isolated(&self) -> f64 {
            match self {
                XPathValue::Number(n) => *n,
                XPathValue::String(s) => s.trim().parse().unwrap_or(f64::NAN),
                XPathValue::Boolean(b) => {
                    if *b {
                        1.0
                    } else {
                        0.0
                    }
                }
                XPathValue::NodeSet(_) => f64::NAN,
            }
        }
    }

    fn eval_with(
        xml: &str,
        xpath: &str,
        vars: &dyn VariableResolver,
        funcs: &dyn FunctionResolver,
        ns: &HashMap<String, String>,
    ) -> XmlResult<XPathValue> {
        let doc = Parser::new().parse(xml).unwrap();
        let root = doc.document_element().unwrap();
        let compiled = CompiledXPath::compile(xpath, DEFAULT_MAX_XPATH_DEPTH)?;
        eval_compiled(
            &compiled,
            &doc,
            root,
            root,
            1,
            1,
            ns,
            vars,
            funcs,
            DEFAULT_MAX_XPATH_NODE_VISITS,
        )
    }

    /// A `$variable` reference resolves through the injected resolver and
    /// participates in arithmetic: `$x + 1` with `$x = 41` yields `42`.
    #[test]
    fn test_variable_reference() {
        let mut m = HashMap::new();
        m.insert("x".to_string(), XPathValue::Number(41.0));
        let vars = MapVars(m);
        let val = eval_with("<r/>", "$x + 1", &vars, &NoFunctions, &HashMap::new()).unwrap();
        match val {
            XPathValue::Number(n) => assert_eq!(n, 42.0),
            _ => panic!("expected number, got {:?}", val),
        }
    }

    /// A string-valued variable flows into a core function (`concat`),
    /// confirming variables compose with the rest of the expression grammar.
    #[test]
    fn test_variable_in_string_context() {
        let mut m = HashMap::new();
        m.insert(
            "greeting".to_string(),
            XPathValue::String("hello".to_string()),
        );
        let vars = MapVars(m);
        let val = eval_with(
            "<r/>",
            "concat($greeting, ' world')",
            &vars,
            &NoFunctions,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            val.to_string_value(&Parser::new().parse("<r/>").unwrap()),
            "hello world"
        );
    }

    /// An unresolved variable is an error (not an empty value): the default
    /// `NoVariables` resolver returns `None`, which `evaluate_expr` turns into
    /// an "undefined variable" error.
    #[test]
    fn test_undefined_variable_errors() {
        let err = eval_with(
            "<r/>",
            "$missing",
            &NoVariables,
            &NoFunctions,
            &HashMap::new(),
        );
        assert!(err.is_err(), "undefined variable must error");
    }

    /// The function-resolution seam: calls the core engine doesn't know
    /// (`t:double`, `t:hi`) fall through to the injected `FunctionResolver`,
    /// which receives the evaluated arguments and returns a value.
    #[test]
    fn test_extension_function() {
        let val = eval_with(
            "<r/>",
            "t:double(21)",
            &NoVariables,
            &TestFns,
            &HashMap::new(),
        )
        .unwrap();
        match val {
            XPathValue::Number(n) => assert_eq!(n, 42.0),
            _ => panic!("expected number, got {:?}", val),
        }
        let val = eval_with("<r/>", "t:hi()", &NoVariables, &TestFns, &HashMap::new()).unwrap();
        match val {
            XPathValue::String(s) => assert_eq!(s, "hi"),
            _ => panic!("expected string"),
        }
    }

    /// Even with a resolver installed, a function neither the core nor the
    /// resolver recognizes (`t:nope`) is still an error — the seam declines by
    /// returning `None` and the engine reports the unknown function.
    #[test]
    fn test_unknown_function_still_errors() {
        let err = eval_with("<r/>", "t:nope()", &NoVariables, &TestFns, &HashMap::new());
        assert!(err.is_err(), "unresolved extension function must error");
    }

    /// Regression for the lexer fix: a prefixed name immediately followed by
    /// `(` (e.g. `t:double(...)`, the shape EXSLT's `date:date-time()` takes)
    /// must lex as a function call, not as a `prefix:local` node test.
    #[test]
    fn test_prefixed_function_lexing() {
        let compiled = CompiledXPath::compile("t:double(2)", DEFAULT_MAX_XPATH_DEPTH);
        assert!(compiled.is_ok(), "prefixed function name must parse");
    }

    /// A prefixed name test (`x:b`) resolves its prefix against the *per-call*
    /// namespace map passed to `eval_compiled` — the mechanism XSLT uses to give
    /// each expression the namespace context of the stylesheet element it came
    /// from, rather than a single evaluator-global map.
    #[test]
    fn test_namespaced_name_test_via_injected_ns() {
        let mut ns = HashMap::new();
        ns.insert("x".to_string(), "urn:ex".to_string());
        let val = eval_with(
            r#"<r xmlns:x="urn:ex"><x:b>hi</x:b></r>"#,
            "x:b",
            &NoVariables,
            &NoFunctions,
            &ns,
        )
        .unwrap();
        assert_eq!(val.as_node_set().len(), 1);
    }

    /// `lang('en')` walks ancestor-or-self for the nearest `xml:lang` and
    /// matches case-insensitively (and on subtag prefixes); `lang('fr')` does
    /// not match an `en`-tagged subtree. The context node is `<p>`, whose
    /// language is inherited from the `<r xml:lang="en">` ancestor.
    #[test]
    fn test_lang_function() {
        let doc = Parser::new()
            .parse(r#"<r xml:lang="en"><p>x</p></r>"#)
            .unwrap();
        let p = doc
            .document_element()
            .and_then(|r| {
                doc.children(r)
                    .into_iter()
                    .find(|&c| matches!(doc.node_kind(c), Some(NodeKind::Element(_))))
            })
            .unwrap();
        let compiled = CompiledXPath::compile("lang('en')", DEFAULT_MAX_XPATH_DEPTH).unwrap();
        let ns = HashMap::new();
        let val = eval_compiled(
            &compiled,
            &doc,
            p,
            p,
            1,
            1,
            &ns,
            &NoVariables,
            &NoFunctions,
            DEFAULT_MAX_XPATH_NODE_VISITS,
        )
        .unwrap();
        assert!(
            val.to_boolean(),
            "lang('en') should match xml:lang=\"en\" ancestor"
        );

        let compiled2 = CompiledXPath::compile("lang('fr')", DEFAULT_MAX_XPATH_DEPTH).unwrap();
        let val2 = eval_compiled(
            &compiled2,
            &doc,
            p,
            p,
            1,
            1,
            &ns,
            &NoVariables,
            &NoFunctions,
            DEFAULT_MAX_XPATH_NODE_VISITS,
        )
        .unwrap();
        assert!(!val2.to_boolean(), "lang('fr') should not match");
    }

    /// `current()` returns the context node when invoked through plain XPath
    /// (current == context). XSLT later distinguishes the two by passing a
    /// separate `current` node into `eval_compiled`; here they coincide, so the
    /// call yields a one-node set.
    #[test]
    fn test_current_equals_context_in_plain_xpath() {
        let val = eval_with(
            "<r><a/></r>",
            "current()",
            &NoVariables,
            &NoFunctions,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(val.as_node_set().len(), 1);
    }
}
