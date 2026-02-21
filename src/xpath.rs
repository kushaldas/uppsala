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

use std::collections::HashMap;

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
    pub fn to_number(&self, doc: &Document) -> f64 {
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
    pub fn to_string_value(&self, doc: &Document) -> String {
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
fn string_value_of_node(doc: &Document, id: NodeId) -> String {
    match doc.node_kind(id) {
        Some(NodeKind::Document) | Some(NodeKind::Element(_)) => doc.text_content_deep(id),
        Some(NodeKind::Text(t)) => t.clone(),
        Some(NodeKind::CData(t)) => t.clone(),
        Some(NodeKind::Comment(c)) => c.clone(),
        Some(NodeKind::ProcessingInstruction(pi)) => pi.data.clone().unwrap_or_default(),
        Some(NodeKind::Attribute(_, v)) => v.clone(),
        None => String::new(),
    }
}

/// The XPath evaluator.
pub struct XPathEvaluator {
    /// Namespace prefix mappings for XPath expressions.
    namespaces: HashMap<String, String>,
}

impl XPathEvaluator {
    /// Create a new evaluator with no namespace bindings.
    pub fn new() -> Self {
        XPathEvaluator {
            namespaces: HashMap::new(),
        }
    }

    /// Register a namespace prefix for use in XPath expressions.
    pub fn add_namespace(&mut self, prefix: impl Into<String>, uri: impl Into<String>) {
        self.namespaces.insert(prefix.into(), uri.into());
    }

    /// Evaluate an XPath expression from the given context node.
    pub fn evaluate(&self, doc: &Document, context: NodeId, expr: &str) -> XmlResult<XPathValue> {
        let tokens = tokenize(expr)?;
        let mut parser = XPathParser::new(&tokens);
        let ast = parser.parse_expr()?;
        let ctx = EvalContext {
            node: context,
            position: 1,
            size: 1,
            doc,
            namespaces: &self.namespaces,
        };
        evaluate_expr(&ast, &ctx)
    }

    /// Convenience: evaluate and return the resulting node set.
    pub fn select_nodes(
        &self,
        doc: &Document,
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
    DoubleColon,
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
                        tokens.push(Token::PrefixedName(prefix, local));
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
    // Filter (path with predicates)
    Filter(Box<Expr>, Vec<Expr>),
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
    // Function call
    FunctionCall(String, Vec<Expr>),
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

struct XPathParser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> XPathParser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        XPathParser { tokens, pos: 0 }
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

    fn parse_or_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_and_expr()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_equality_expr()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let right = self.parse_equality_expr()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_relational_expr()?;
        loop {
            match self.peek() {
                Some(Token::Eq) => {
                    self.advance();
                    let right = self.parse_relational_expr()?;
                    left = Expr::Eq(Box::new(left), Box::new(right));
                }
                Some(Token::NotEq) => {
                    self.advance();
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
        loop {
            match self.peek() {
                Some(Token::Lt) => {
                    self.advance();
                    let right = self.parse_additive_expr()?;
                    left = Expr::Lt(Box::new(left), Box::new(right));
                }
                Some(Token::Gt) => {
                    self.advance();
                    let right = self.parse_additive_expr()?;
                    left = Expr::Gt(Box::new(left), Box::new(right));
                }
                Some(Token::LtEq) => {
                    self.advance();
                    let right = self.parse_additive_expr()?;
                    left = Expr::LtEq(Box::new(left), Box::new(right));
                }
                Some(Token::GtEq) => {
                    self.advance();
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
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.advance();
                    let right = self.parse_multiplicative_expr()?;
                    left = Expr::Add(Box::new(left), Box::new(right));
                }
                Some(Token::Minus) => {
                    self.advance();
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
        loop {
            match self.peek() {
                Some(Token::Star) => {
                    // Only treat as multiply if left is not a step
                    // This is context-dependent; for simplicity, check if there's
                    // something on the left that could be a number/expression
                    self.advance();
                    let right = self.parse_unary_expr()?;
                    left = Expr::Mul(Box::new(left), Box::new(right));
                }
                Some(Token::Div) => {
                    self.advance();
                    let right = self.parse_unary_expr()?;
                    left = Expr::Div(Box::new(left), Box::new(right));
                }
                Some(Token::Mod) => {
                    self.advance();
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
            self.advance();
            let expr = self.parse_unary_expr()?;
            Ok(Expr::Negate(Box::new(expr)))
        } else {
            self.parse_union_expr()
        }
    }

    fn parse_union_expr(&mut self) -> XmlResult<Expr> {
        let mut left = self.parse_path_expr()?;
        while matches!(self.peek(), Some(Token::Pipe)) {
            self.advance();
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
            Some(Token::FunctionName(_)) => self.parse_function_call(),
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_expr()?;
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
            let expr = self.parse_expr()?;
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
            args.push(self.parse_expr()?);
            while matches!(self.peek(), Some(Token::Comma)) {
                self.advance();
                args.push(self.parse_expr()?);
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

struct EvalContext<'a> {
    node: NodeId,
    position: usize,
    size: usize,
    doc: &'a Document,
    namespaces: &'a HashMap<String, String>,
}

fn evaluate_expr(expr: &Expr, ctx: &EvalContext) -> XmlResult<XPathValue> {
    match expr {
        Expr::Path(steps) => {
            let mut nodes = vec![ctx.node];
            for step in steps {
                nodes = apply_step(step, &nodes, ctx)?;
            }
            Ok(XPathValue::NodeSet(dedup_document_order(nodes)))
        }
        Expr::AbsolutePath(steps) => {
            // Find the document root
            let mut root = ctx.node;
            while let Some(p) = ctx.doc.parent(root) {
                root = p;
            }
            let mut nodes = vec![root];
            for step in steps {
                nodes = apply_step(step, &nodes, ctx)?;
            }
            Ok(XPathValue::NodeSet(dedup_document_order(nodes)))
        }
        Expr::Union(left, right) => {
            let left_val = evaluate_expr(left, ctx)?;
            let right_val = evaluate_expr(right, ctx)?;
            let mut nodes = left_val.as_node_set().to_vec();
            nodes.extend_from_slice(right_val.as_node_set());
            Ok(XPathValue::NodeSet(dedup_document_order(nodes)))
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
            Ok(XPathValue::Boolean(xpath_equal(&l, &r, ctx.doc)))
        }
        Expr::NotEq(left, right) => {
            let l = evaluate_expr(left, ctx)?;
            let r = evaluate_expr(right, ctx)?;
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
        Expr::FunctionCall(name, args) => evaluate_function(name, args, ctx),
        Expr::Filter(base, predicates) => {
            let base_val = evaluate_expr(base, ctx)?;
            let mut nodes = base_val.as_node_set().to_vec();
            for pred in predicates {
                nodes = apply_predicate(pred, &nodes, ctx)?;
            }
            Ok(XPathValue::NodeSet(nodes))
        }
    }
}

/// XPath equality comparison (handles node-set vs string/number/boolean).
fn xpath_equal(left: &XPathValue, right: &XPathValue, doc: &Document) -> bool {
    match (left, right) {
        (XPathValue::NodeSet(ls), XPathValue::NodeSet(rs)) => {
            // Two node-sets: true if any pair of string-values are equal
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

fn apply_step(step: &Step, context_nodes: &[NodeId], ctx: &EvalContext) -> XmlResult<Vec<NodeId>> {
    let mut result = Vec::new();
    for &node in context_nodes {
        let axis_nodes = select_axis(&step.axis, node, ctx.doc);
        for &candidate in &axis_nodes {
            if matches_node_test(&step.node_test, candidate, ctx.doc, ctx.namespaces) {
                result.push(candidate);
            }
        }
    }
    // Apply predicates
    for pred in &step.predicates {
        result = apply_predicate(pred, &result, ctx)?;
    }
    Ok(result)
}

fn apply_predicate(pred: &Expr, nodes: &[NodeId], ctx: &EvalContext) -> XmlResult<Vec<NodeId>> {
    let size = nodes.len();
    let mut result = Vec::new();
    for (i, &node) in nodes.iter().enumerate() {
        let pred_ctx = EvalContext {
            node,
            position: i + 1,
            size,
            doc: ctx.doc,
            namespaces: ctx.namespaces,
        };
        let val = evaluate_expr(pred, &pred_ctx)?;
        let keep = match &val {
            XPathValue::Number(n) => (*n - (i + 1) as f64).abs() < f64::EPSILON,
            _ => val.to_boolean(),
        };
        if keep {
            result.push(node);
        }
    }
    Ok(result)
}

fn select_axis(axis: &Axis, node: NodeId, doc: &Document) -> Vec<NodeId> {
    match axis {
        Axis::Child => doc.children(node),
        Axis::Descendant => doc.descendants(node),
        Axis::Parent => doc.parent(node).into_iter().collect(),
        Axis::Ancestor => doc.ancestors(node),
        Axis::Self_ => vec![node],
        Axis::DescendantOrSelf => {
            let mut result = vec![node];
            result.extend(doc.descendants(node));
            result
        }
        Axis::AncestorOrSelf => {
            let mut result = vec![node];
            result.extend(doc.ancestors(node));
            result
        }
        Axis::FollowingSibling => {
            let mut result = Vec::new();
            let mut current = doc.next_sibling(node);
            while let Some(sib) = current {
                result.push(sib);
                current = doc.next_sibling(sib);
            }
            result
        }
        Axis::PrecedingSibling => {
            let mut result = Vec::new();
            let mut current = doc.previous_sibling(node);
            while let Some(sib) = current {
                result.push(sib);
                current = doc.previous_sibling(sib);
            }
            result
        }
        Axis::Following => {
            // All nodes after this node in document order
            collect_following(doc, node)
        }
        Axis::Preceding => {
            // All nodes before this node in document order
            collect_preceding(doc, node)
        }
        Axis::Attribute => {
            // Return pre-allocated virtual attribute nodes for this element.
            doc.get_attribute_nodes(node).to_vec()
        }
        Axis::Namespace => {
            // Namespace nodes are virtual; return empty.
            Vec::new()
        }
    }
}

fn collect_following(doc: &Document, node: NodeId) -> Vec<NodeId> {
    let mut result = Vec::new();
    // Go to next sibling or ancestor's next sibling
    let mut current = node;
    loop {
        if let Some(next) = doc.next_sibling(current) {
            result.push(next);
            result.extend(doc.descendants(next));
            current = next;
            // Continue to get more siblings
            continue;
        }
        // No more siblings, go up
        if let Some(parent) = doc.parent(current) {
            current = parent;
        } else {
            break;
        }
    }
    result
}

fn collect_preceding(doc: &Document, node: NodeId) -> Vec<NodeId> {
    let mut result = Vec::new();
    let mut current = node;
    loop {
        if let Some(prev) = doc.previous_sibling(current) {
            // Add descendants in reverse, then the sibling itself
            let descs = doc.descendants(prev);
            for d in descs.into_iter().rev() {
                result.push(d);
            }
            result.push(prev);
            current = prev;
            continue;
        }
        if let Some(parent) = doc.parent(current) {
            if doc.parent(parent).is_some() {
                // Don't include ancestors
                current = parent;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    result
}

fn matches_node_test(
    test: &NodeTest,
    node: NodeId,
    doc: &Document,
    namespaces: &HashMap<String, String>,
) -> bool {
    match test {
        NodeTest::Wildcard => matches!(
            doc.node_kind(node),
            Some(NodeKind::Element(_)) | Some(NodeKind::Attribute(_, _))
        ),
        NodeTest::Name(name) => match doc.node_kind(node) {
            Some(NodeKind::Element(e)) => e.name.local_name == *name,
            Some(NodeKind::Attribute(qn, _)) => qn.local_name == *name,
            _ => false,
        },
        NodeTest::PrefixedName(prefix, local) => match doc.node_kind(node) {
            Some(NodeKind::Element(e)) => {
                if let Some(expected_ns) = namespaces.get(prefix) {
                    e.name.local_name == *local
                        && e.name.namespace_uri.as_deref() == Some(expected_ns.as_str())
                } else {
                    // Fall back to matching by prefix
                    e.name.prefix.as_deref() == Some(prefix.as_str()) && e.name.local_name == *local
                }
            }
            Some(NodeKind::Attribute(qn, _)) => {
                if let Some(expected_ns) = namespaces.get(prefix) {
                    qn.local_name == *local
                        && qn.namespace_uri.as_deref() == Some(expected_ns.as_str())
                } else {
                    qn.prefix.as_deref() == Some(prefix.as_str()) && qn.local_name == *local
                }
            }
            _ => false,
        },
        NodeTest::PrefixWildcard(prefix) => match doc.node_kind(node) {
            Some(NodeKind::Element(e)) => {
                if let Some(expected_ns) = namespaces.get(prefix) {
                    e.name.namespace_uri.as_deref() == Some(expected_ns.as_str())
                } else {
                    e.name.prefix.as_deref() == Some(prefix.as_str())
                }
            }
            Some(NodeKind::Attribute(qn, _)) => {
                if let Some(expected_ns) = namespaces.get(prefix) {
                    qn.namespace_uri.as_deref() == Some(expected_ns.as_str())
                } else {
                    qn.prefix.as_deref() == Some(prefix.as_str())
                }
            }
            _ => false,
        },
        NodeTest::NodeType(nt) => match nt.as_str() {
            "node" => true,
            "text" => matches!(
                doc.node_kind(node),
                Some(NodeKind::Text(_)) | Some(NodeKind::CData(_))
            ),
            "comment" => matches!(doc.node_kind(node), Some(NodeKind::Comment(_))),
            "processing-instruction" => matches!(
                doc.node_kind(node),
                Some(NodeKind::ProcessingInstruction(_))
            ),
            _ => false,
        },
    }
}

fn evaluate_function(name: &str, args: &[Expr], ctx: &EvalContext) -> XmlResult<XPathValue> {
    match name {
        "last" => Ok(XPathValue::Number(ctx.size as f64)),
        "position" => Ok(XPathValue::Number(ctx.position as f64)),
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
                Some(NodeKind::Element(e)) => e.name.local_name.clone(),
                Some(NodeKind::Attribute(qn, _)) => qn.local_name.clone(),
                Some(NodeKind::ProcessingInstruction(pi)) => pi.target.clone(),
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
                Some(NodeKind::Element(e)) => e.name.namespace_uri.clone().unwrap_or_default(),
                Some(NodeKind::Attribute(qn, _)) => qn.namespace_uri.clone().unwrap_or_default(),
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
                Some(NodeKind::Element(e)) => e.name.prefixed_name(),
                Some(NodeKind::Attribute(qn, _)) => qn.prefixed_name(),
                Some(NodeKind::ProcessingInstruction(pi)) => pi.target.clone(),
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
            let start = evaluate_expr(&args[1], ctx)?.to_number(ctx.doc).round() as i64 - 1;
            let chars: Vec<char> = s.chars().collect();
            let start = start.max(0) as usize;
            if args.len() == 3 {
                let len = evaluate_expr(&args[2], ctx)?.to_number(ctx.doc).round() as usize;
                let end = (start + len).min(chars.len());
                let result: String = chars[start.min(chars.len())..end].iter().collect();
                Ok(XPathValue::String(result))
            } else {
                let result: String = chars[start.min(chars.len())..].iter().collect();
                Ok(XPathValue::String(result))
            }
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
            Ok(XPathValue::Number(n.round()))
        }
        "id" => {
            // Simple implementation: find elements with matching ID attributes
            if args.len() != 1 {
                return Err(XmlError::xpath("id() takes exactly 1 argument"));
            }
            let val = evaluate_expr(&args[0], ctx)?.to_string_value(ctx.doc);
            let ids: Vec<&str> = val.split_whitespace().collect();
            let mut result = Vec::new();
            collect_elements_with_id(ctx.doc, ctx.doc.root(), &ids, &mut result);
            Ok(XPathValue::NodeSet(result))
        }
        _ => Err(XmlError::xpath(format!("Unknown function: {}()", name))),
    }
}

fn collect_elements_with_id(doc: &Document, node: NodeId, ids: &[&str], result: &mut Vec<NodeId>) {
    if let Some(NodeKind::Element(e)) = doc.node_kind(node) {
        // Check for "id" or "ID" attribute
        for attr in &e.attributes {
            if (attr.name.local_name == "id" || attr.name.local_name == "ID")
                && ids.contains(&attr.value.as_str())
            {
                result.push(node);
                break;
            }
        }
    }
    for child in doc.children(node) {
        collect_elements_with_id(doc, child, ids, result);
    }
}

/// Remove duplicate NodeIds and maintain document order.
fn dedup_document_order(mut nodes: Vec<NodeId>) -> Vec<NodeId> {
    // NodeId(usize) - document order is by arena index
    nodes.sort_by_key(|n| n.0);
    nodes.dedup();
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    fn parse_and_eval(xml: &str, xpath: &str) -> XPathValue {
        let doc = Parser::new().parse(xml).unwrap();
        let eval = XPathEvaluator::new();
        let root = doc.document_element().unwrap();
        eval.evaluate(&doc, root, xpath).unwrap()
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
}
