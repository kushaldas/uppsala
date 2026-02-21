//! DOM (Document Object Model) based on the XML Information Set specification.
//!
//! This module provides an arena-based tree representation of XML documents.
//! Each node is identified by a [`NodeId`] and stored in a central arena within
//! the [`Document`]. This avoids reference-counting overhead and makes tree
//! mutation straightforward.

use std::collections::HashMap;
use std::fmt;

/// A unique identifier for a node within a [`Document`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) usize);

/// A qualified name consisting of an optional namespace URI, optional prefix,
/// and a local name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QName {
    /// The namespace URI, if any.
    pub namespace_uri: Option<String>,
    /// The namespace prefix, if any (e.g. `"soap"` in `soap:Envelope`).
    pub prefix: Option<String>,
    /// The local part of the name.
    pub local_name: String,
}

impl QName {
    /// Create a QName with only a local name (no namespace).
    pub fn local(name: impl Into<String>) -> Self {
        QName {
            namespace_uri: None,
            prefix: None,
            local_name: name.into(),
        }
    }

    /// Create a QName with a namespace URI and local name.
    pub fn with_namespace(namespace_uri: impl Into<String>, local_name: impl Into<String>) -> Self {
        QName {
            namespace_uri: Some(namespace_uri.into()),
            prefix: None,
            local_name: local_name.into(),
        }
    }

    /// Create a QName with prefix, namespace URI, and local name.
    pub fn full(
        prefix: impl Into<String>,
        namespace_uri: impl Into<String>,
        local_name: impl Into<String>,
    ) -> Self {
        QName {
            namespace_uri: Some(namespace_uri.into()),
            prefix: Some(prefix.into()),
            local_name: local_name.into(),
        }
    }

    /// Returns the prefixed form (e.g. `"soap:Envelope"`) or just the local name.
    pub fn prefixed_name(&self) -> String {
        match &self.prefix {
            Some(p) => format!("{}:{}", p, self.local_name),
            None => self.local_name.clone(),
        }
    }
}

impl fmt::Display for QName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.namespace_uri, &self.prefix) {
            (Some(ns), Some(p)) => write!(f, "{{{}}}{}:{}", ns, p, self.local_name),
            (Some(ns), None) => write!(f, "{{{}}}{}", ns, self.local_name),
            _ => write!(f, "{}", self.local_name),
        }
    }
}

/// An XML attribute (part of the Infoset attribute information item).
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// The qualified name of the attribute.
    pub name: QName,
    /// The normalized attribute value.
    pub value: String,
}

/// The XML declaration (`<?xml version="1.0" encoding="UTF-8"?>`).
#[derive(Debug, Clone, PartialEq)]
pub struct XmlDeclaration {
    pub version: String,
    pub encoding: Option<String>,
    pub standalone: Option<bool>,
}

/// A processing instruction (`<?target data?>`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingInstruction {
    pub target: String,
    pub data: Option<String>,
}

/// The different kinds of nodes in the DOM tree.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// The document root (Infoset document information item).
    Document,
    /// An element node (Infoset element information item).
    Element(Element),
    /// A text node (Infoset character information item).
    Text(String),
    /// A CDATA section.
    CData(String),
    /// A comment node (Infoset comment information item).
    Comment(String),
    /// A processing instruction (Infoset PI information item).
    ProcessingInstruction(ProcessingInstruction),
    /// A virtual attribute node (used by XPath evaluation).
    /// Not part of the normal child tree.
    Attribute(QName, String),
}

/// An element with its qualified name and attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// The qualified name of the element.
    pub name: QName,
    /// The element's attributes.
    pub attributes: Vec<Attribute>,
    /// In-scope namespace declarations on this element.
    /// Maps prefix (empty string for default namespace) to namespace URI.
    pub namespace_declarations: HashMap<String, String>,
}

impl Element {
    /// Get an attribute value by local name (ignoring namespace).
    pub fn get_attribute(&self, local_name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name.local_name == local_name)
            .map(|a| a.value.as_str())
    }

    /// Get an attribute value by namespace URI and local name.
    pub fn get_attribute_ns(&self, namespace_uri: &str, local_name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| {
                a.name.local_name == local_name
                    && a.name.namespace_uri.as_deref() == Some(namespace_uri)
            })
            .map(|a| a.value.as_str())
    }

    /// Set or update an attribute. Returns the old value if the attribute already existed.
    pub fn set_attribute(&mut self, name: QName, value: String) -> Option<String> {
        for attr in &mut self.attributes {
            if attr.name == name {
                let old = std::mem::replace(&mut attr.value, value);
                return Some(old);
            }
        }
        self.attributes.push(Attribute { name, value });
        None
    }

    /// Remove an attribute by local name. Returns the removed value if found.
    pub fn remove_attribute(&mut self, local_name: &str) -> Option<String> {
        if let Some(pos) = self
            .attributes
            .iter()
            .position(|a| a.name.local_name == local_name)
        {
            Some(self.attributes.remove(pos).value)
        } else {
            None
        }
    }
}

/// Internal representation of a node in the arena.
#[derive(Debug, Clone)]
pub(crate) struct NodeData {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Source location (line, column) for error reporting.
    pub line: usize,
    pub column: usize,
}

/// An XML document represented as an arena-based tree.
///
/// Nodes are stored in a flat `Vec` and referenced by [`NodeId`]. This provides
/// O(1) node access and simple tree mutation without reference counting.
#[derive(Debug, Clone)]
pub struct Document {
    /// The node arena.
    pub(crate) nodes: Vec<NodeData>,
    /// The root node id (always NodeId(0), the Document node).
    root: NodeId,
    /// Optional XML declaration.
    pub xml_declaration: Option<XmlDeclaration>,
    /// Raw DOCTYPE declaration text, preserved verbatim for round-trip fidelity.
    /// e.g. `<!DOCTYPE root SYSTEM "root.dtd">` or `<!DOCTYPE html>`.
    pub doctype: Option<String>,
    /// Attribute nodes for each element, keyed by element NodeId.
    /// These are virtual nodes used by XPath attribute axis traversal.
    pub(crate) attribute_nodes: HashMap<NodeId, Vec<NodeId>>,
}

impl Document {
    /// Create a new empty document.
    pub fn new() -> Self {
        let root_node = NodeData {
            kind: NodeKind::Document,
            parent: None,
            children: Vec::new(),
            line: 0,
            column: 0,
        };
        Document {
            nodes: vec![root_node],
            root: NodeId(0),
            xml_declaration: None,
            doctype: None,
            attribute_nodes: HashMap::new(),
        }
    }

    /// Returns the root (Document) node id.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Returns the document element (the single top-level element), if any.
    pub fn document_element(&self) -> Option<NodeId> {
        self.children(self.root)
            .into_iter()
            .find(|&id| matches!(self.node_kind(id), Some(NodeKind::Element(_))))
    }

    /// Allocate a new node in the arena and return its id.
    pub(crate) fn alloc_node(&mut self, kind: NodeKind, line: usize, column: usize) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(NodeData {
            kind,
            parent: None,
            children: Vec::new(),
            line,
            column,
        });
        id
    }

    /// Allocate virtual attribute nodes for an element.
    /// Call this after adding an element with attributes to enable XPath attribute axis.
    pub(crate) fn build_attribute_nodes(&mut self, element_id: NodeId) {
        let attrs: Vec<(QName, String)> = match self.node_kind(element_id) {
            Some(NodeKind::Element(e)) => e
                .attributes
                .iter()
                .map(|a| (a.name.clone(), a.value.clone()))
                .collect(),
            _ => return,
        };
        let mut attr_ids = Vec::with_capacity(attrs.len());
        for (name, value) in attrs {
            let attr_id = self.alloc_node(NodeKind::Attribute(name, value), 0, 0);
            // Set parent to the element (attribute nodes have an owner element)
            if let Some(node) = self.nodes.get_mut(attr_id.0) {
                node.parent = Some(element_id);
            }
            attr_ids.push(attr_id);
        }
        if !attr_ids.is_empty() {
            self.attribute_nodes.insert(element_id, attr_ids);
        }
    }

    /// Get the attribute nodes for an element (used by XPath).
    pub(crate) fn get_attribute_nodes(&self, element_id: NodeId) -> &[NodeId] {
        self.attribute_nodes
            .get(&element_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Create a new element node (not yet attached to the tree).
    pub fn create_element(&mut self, name: QName) -> NodeId {
        self.alloc_node(
            NodeKind::Element(Element {
                name,
                attributes: Vec::new(),
                namespace_declarations: HashMap::new(),
            }),
            0,
            0,
        )
    }

    /// Create a new text node (not yet attached to the tree).
    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.alloc_node(NodeKind::Text(text.into()), 0, 0)
    }

    /// Create a new comment node (not yet attached to the tree).
    pub fn create_comment(&mut self, text: impl Into<String>) -> NodeId {
        self.alloc_node(NodeKind::Comment(text.into()), 0, 0)
    }

    /// Create a new processing instruction node (not yet attached to the tree).
    pub fn create_processing_instruction(
        &mut self,
        target: impl Into<String>,
        data: Option<String>,
    ) -> NodeId {
        self.alloc_node(
            NodeKind::ProcessingInstruction(ProcessingInstruction {
                target: target.into(),
                data,
            }),
            0,
            0,
        )
    }

    /// Create a new CDATA node (not yet attached to the tree).
    pub fn create_cdata(&mut self, text: impl Into<String>) -> NodeId {
        self.alloc_node(NodeKind::CData(text.into()), 0, 0)
    }

    // ─── Tree access ───

    /// Get the kind of a node.
    pub fn node_kind(&self, id: NodeId) -> Option<&NodeKind> {
        self.nodes.get(id.0).map(|n| &n.kind)
    }

    /// Get a mutable reference to a node's kind.
    pub fn node_kind_mut(&mut self, id: NodeId) -> Option<&mut NodeKind> {
        self.nodes.get_mut(id.0).map(|n| &mut n.kind)
    }

    /// Get the element data for an element node.
    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match self.node_kind(id) {
            Some(NodeKind::Element(e)) => Some(e),
            _ => None,
        }
    }

    /// Get mutable element data for an element node.
    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match self.node_kind_mut(id) {
            Some(NodeKind::Element(e)) => Some(e),
            _ => None,
        }
    }

    /// Get the text content of a text or CDATA node.
    pub fn text_content(&self, id: NodeId) -> Option<&str> {
        match self.node_kind(id) {
            Some(NodeKind::Text(t)) => Some(t.as_str()),
            Some(NodeKind::CData(t)) => Some(t.as_str()),
            _ => None,
        }
    }

    /// Get the parent of a node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.parent)
    }

    /// Get the children of a node.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        self.nodes
            .get(id.0)
            .map(|n| n.children.clone())
            .unwrap_or_default()
    }

    /// Get the source line of a node.
    pub fn node_line(&self, id: NodeId) -> usize {
        self.nodes.get(id.0).map(|n| n.line).unwrap_or(0)
    }

    /// Get the source column of a node.
    pub fn node_column(&self, id: NodeId) -> usize {
        self.nodes.get(id.0).map(|n| n.column).unwrap_or(0)
    }

    /// Get all descendant element nodes matching a local name.
    pub fn get_elements_by_tag_name(&self, local_name: &str) -> Vec<NodeId> {
        let mut results = Vec::new();
        self.collect_elements_by_tag_name(self.root, local_name, &mut results);
        results
    }

    fn collect_elements_by_tag_name(
        &self,
        id: NodeId,
        local_name: &str,
        results: &mut Vec<NodeId>,
    ) {
        if let Some(NodeKind::Element(e)) = self.node_kind(id) {
            if e.name.local_name == local_name {
                results.push(id);
            }
        }
        for child in self.children(id) {
            self.collect_elements_by_tag_name(child, local_name, results);
        }
    }

    /// Get all descendant element nodes matching a namespace URI and local name.
    pub fn get_elements_by_tag_name_ns(
        &self,
        namespace_uri: &str,
        local_name: &str,
    ) -> Vec<NodeId> {
        let mut results = Vec::new();
        self.collect_elements_by_tag_name_ns(self.root, namespace_uri, local_name, &mut results);
        results
    }

    fn collect_elements_by_tag_name_ns(
        &self,
        id: NodeId,
        namespace_uri: &str,
        local_name: &str,
        results: &mut Vec<NodeId>,
    ) {
        if let Some(NodeKind::Element(e)) = self.node_kind(id) {
            if e.name.local_name == local_name
                && e.name.namespace_uri.as_deref() == Some(namespace_uri)
            {
                results.push(id);
            }
        }
        for child in self.children(id) {
            self.collect_elements_by_tag_name_ns(child, namespace_uri, local_name, results);
        }
    }

    /// Collect all text content of this node and its descendants (depth-first).
    pub fn text_content_deep(&self, id: NodeId) -> String {
        let mut buf = String::new();
        self.collect_text(id, &mut buf);
        buf
    }

    fn collect_text(&self, id: NodeId, buf: &mut String) {
        match self.node_kind(id) {
            Some(NodeKind::Text(t)) => buf.push_str(t),
            Some(NodeKind::CData(t)) => buf.push_str(t),
            _ => {
                for child in self.children(id) {
                    self.collect_text(child, buf);
                }
            }
        }
    }

    // ─── Tree mutation ───

    /// Append a child node to a parent. Detaches the child from any previous parent.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        // Detach from old parent first
        self.detach(child);
        // Set new parent
        if let Some(node) = self.nodes.get_mut(child.0) {
            node.parent = Some(parent);
        }
        // Add to new parent's children
        if let Some(node) = self.nodes.get_mut(parent.0) {
            node.children.push(child);
        }
    }

    /// Insert a child before a reference node. Both must share the same parent.
    pub fn insert_before(&mut self, parent: NodeId, new_child: NodeId, reference: NodeId) {
        self.detach(new_child);
        if let Some(node) = self.nodes.get_mut(new_child.0) {
            node.parent = Some(parent);
        }
        if let Some(node) = self.nodes.get_mut(parent.0) {
            if let Some(pos) = node.children.iter().position(|&c| c == reference) {
                node.children.insert(pos, new_child);
            } else {
                // If reference not found, append at the end
                node.children.push(new_child);
            }
        }
    }

    /// Insert a child after a reference node.
    pub fn insert_after(&mut self, parent: NodeId, new_child: NodeId, reference: NodeId) {
        self.detach(new_child);
        if let Some(node) = self.nodes.get_mut(new_child.0) {
            node.parent = Some(parent);
        }
        if let Some(node) = self.nodes.get_mut(parent.0) {
            if let Some(pos) = node.children.iter().position(|&c| c == reference) {
                node.children.insert(pos + 1, new_child);
            } else {
                node.children.push(new_child);
            }
        }
    }

    /// Remove a child from its parent. The node remains in the arena but is detached.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        if let Some(node) = self.nodes.get_mut(parent.0) {
            node.children.retain(|&c| c != child);
        }
        if let Some(node) = self.nodes.get_mut(child.0) {
            node.parent = None;
        }
    }

    /// Replace an old child with a new child under the given parent.
    pub fn replace_child(&mut self, parent: NodeId, new_child: NodeId, old_child: NodeId) {
        self.detach(new_child);
        if let Some(node) = self.nodes.get_mut(new_child.0) {
            node.parent = Some(parent);
        }
        if let Some(node) = self.nodes.get_mut(parent.0) {
            if let Some(pos) = node.children.iter().position(|&c| c == old_child) {
                node.children[pos] = new_child;
            }
        }
        if let Some(node) = self.nodes.get_mut(old_child.0) {
            node.parent = None;
        }
    }

    /// Detach a node from its parent (internal helper).
    fn detach(&mut self, id: NodeId) {
        if let Some(parent_id) = self.nodes.get(id.0).and_then(|n| n.parent) {
            if let Some(parent) = self.nodes.get_mut(parent_id.0) {
                parent.children.retain(|&c| c != id);
            }
            if let Some(node) = self.nodes.get_mut(id.0) {
                node.parent = None;
            }
        }
    }

    // ─── Navigation helpers ───

    /// Get the first child of a node.
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes
            .get(id.0)
            .and_then(|n| n.children.first().copied())
    }

    /// Get the last child of a node.
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes
            .get(id.0)
            .and_then(|n| n.children.last().copied())
    }

    /// Get the next sibling of a node.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.parent(id)?;
        let children = &self.nodes[parent.0].children;
        let pos = children.iter().position(|&c| c == id)?;
        children.get(pos + 1).copied()
    }

    /// Get the previous sibling of a node.
    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.parent(id)?;
        let children = &self.nodes[parent.0].children;
        let pos = children.iter().position(|&c| c == id)?;
        if pos > 0 {
            Some(children[pos - 1])
        } else {
            None
        }
    }

    /// Return all ancestor node ids from the node up to (but not including) the root.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut current = self.parent(id);
        while let Some(pid) = current {
            result.push(pid);
            current = self.parent(pid);
        }
        result
    }

    /// Depth-first pre-order traversal of descendants (not including the node itself).
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        self.collect_descendants(id, &mut result);
        result
    }

    fn collect_descendants(&self, id: NodeId, result: &mut Vec<NodeId>) {
        for child in self.children(id) {
            result.push(child);
            self.collect_descendants(child, result);
        }
    }

    // ─── Serialization ───

    /// Serialize the document back to an XML string (compact, no indentation).
    pub fn to_xml(&self) -> String {
        let mut output = String::new();
        // write_document_to cannot fail when writing to String
        self.write_document_to(&mut output, &XmlWriteOptions::default())
            .unwrap();
        output
    }

    /// Serialize the document with formatting options.
    pub fn to_xml_with_options(&self, opts: &XmlWriteOptions) -> String {
        let mut output = String::new();
        self.write_document_to(&mut output, opts).unwrap();
        output
    }

    /// Serialize a single node (and its subtree) to an XML string.
    ///
    /// Useful for extracting XML fragments without the XML declaration or DOCTYPE.
    pub fn node_to_xml(&self, id: NodeId) -> String {
        let mut output = String::new();
        self.write_node_to(id, &mut output, &XmlWriteOptions::default(), 0, false)
            .unwrap();
        output
    }

    /// Serialize a single node (and its subtree) with formatting options.
    pub fn node_to_xml_with_options(&self, id: NodeId, opts: &XmlWriteOptions) -> String {
        let mut output = String::new();
        self.write_node_to(id, &mut output, opts, 0, false).unwrap();
        output
    }

    /// Write the entire document to any `io::Write` sink (file, socket, Vec<u8>, etc.)
    /// without intermediate String allocation.
    pub fn write_to(&self, writer: &mut dyn std::io::Write) -> std::io::Result<()> {
        let opts = XmlWriteOptions::default();
        self.write_to_with_options(writer, &opts)
    }

    /// Write the entire document to an `io::Write` sink with formatting options.
    pub fn write_to_with_options(
        &self,
        writer: &mut dyn std::io::Write,
        opts: &XmlWriteOptions,
    ) -> std::io::Result<()> {
        let mut adapter = IoWriteAdapter { inner: writer };
        self.write_document_to(&mut adapter, opts)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }

    /// Internal: write the full document (declaration + DOCTYPE + nodes) to a `fmt::Write` sink.
    fn write_document_to(&self, out: &mut dyn fmt::Write, opts: &XmlWriteOptions) -> fmt::Result {
        if let Some(decl) = &self.xml_declaration {
            out.write_str("<?xml version=\"")?;
            out.write_str(&decl.version)?;
            out.write_char('"')?;
            if let Some(enc) = &decl.encoding {
                out.write_str(" encoding=\"")?;
                out.write_str(enc)?;
                out.write_char('"')?;
            }
            if let Some(sa) = decl.standalone {
                out.write_str(" standalone=\"")?;
                out.write_str(if sa { "yes" } else { "no" })?;
                out.write_char('"')?;
            }
            out.write_str("?>")?;
        }
        if let Some(dt) = &self.doctype {
            out.write_str(dt)?;
        }
        for child in self.children(self.root) {
            self.write_node_to(child, out, opts, 0, opts.indent.is_some())?;
        }
        Ok(())
    }

    /// Internal: write a single node and its subtree to a `fmt::Write` sink.
    ///
    /// `indent_self` — if true, write indentation before this node (set by parent
    /// when it detects element-only content during pretty-printing).
    fn write_node_to(
        &self,
        id: NodeId,
        out: &mut dyn fmt::Write,
        opts: &XmlWriteOptions,
        depth: usize,
        indent_self: bool,
    ) -> fmt::Result {
        match self.node_kind(id) {
            Some(NodeKind::Element(elem)) => {
                if indent_self {
                    write_indent(out, opts, depth)?;
                }
                out.write_char('<')?;
                out.write_str(&elem.name.prefixed_name())?;
                // Namespace declarations
                for (prefix, uri) in &elem.namespace_declarations {
                    if prefix.is_empty() {
                        out.write_str(" xmlns=\"")?;
                    } else {
                        out.write_str(" xmlns:")?;
                        out.write_str(prefix)?;
                        out.write_str("=\"")?;
                    }
                    write_escaped_attr(out, uri)?;
                    out.write_char('"')?;
                }
                // Attributes
                for attr in &elem.attributes {
                    out.write_char(' ')?;
                    out.write_str(&attr.name.prefixed_name())?;
                    out.write_str("=\"")?;
                    write_escaped_attr(out, &attr.value)?;
                    out.write_char('"')?;
                }
                let children = self.children(id);
                if children.is_empty() {
                    if opts.expand_empty_elements {
                        out.write_str("></")?;
                        out.write_str(&elem.name.prefixed_name())?;
                        out.write_char('>')?;
                    } else {
                        out.write_str("/>")?;
                    }
                } else {
                    out.write_char('>')?;
                    // Determine if this is "element-only" content for pretty-printing.
                    // If any child is text or CDATA, we treat it as mixed content
                    // and do NOT insert newlines/indent (to preserve whitespace semantics).
                    let element_only = opts.indent.is_some()
                        && children.iter().all(|&cid| {
                            !matches!(
                                self.node_kind(cid),
                                Some(NodeKind::Text(_)) | Some(NodeKind::CData(_))
                            )
                        });
                    if element_only {
                        out.write_char('\n')?;
                    }
                    for child in &children {
                        self.write_node_to(*child, out, opts, depth + 1, element_only)?;
                    }
                    if element_only {
                        write_indent(out, opts, depth)?;
                    }
                    out.write_str("</")?;
                    out.write_str(&elem.name.prefixed_name())?;
                    out.write_char('>')?;
                }
                // Trailing newline after the document element when pretty-printing
                if indent_self {
                    out.write_char('\n')?;
                }
            }
            Some(NodeKind::Text(text)) => {
                write_escaped_text(out, text)?;
            }
            Some(NodeKind::CData(text)) => {
                out.write_str("<![CDATA[")?;
                out.write_str(text)?;
                out.write_str("]]>")?;
            }
            Some(NodeKind::Comment(text)) => {
                if indent_self {
                    write_indent(out, opts, depth)?;
                }
                out.write_str("<!--")?;
                out.write_str(text)?;
                out.write_str("-->")?;
                if indent_self {
                    out.write_char('\n')?;
                }
            }
            Some(NodeKind::ProcessingInstruction(pi)) => {
                if indent_self {
                    write_indent(out, opts, depth)?;
                }
                out.write_str("<?")?;
                out.write_str(&pi.target)?;
                if let Some(data) = &pi.data {
                    out.write_char(' ')?;
                    out.write_str(data)?;
                }
                out.write_str("?>")?;
                if indent_self {
                    out.write_char('\n')?;
                }
            }
            Some(NodeKind::Document) => {
                for child in self.children(id) {
                    self.write_node_to(child, out, opts, depth, indent_self)?;
                }
            }
            Some(NodeKind::Attribute(_, _)) => {
                // Virtual attribute nodes are not serialized as children.
            }
            None => {}
        }
        Ok(())
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_document_to(f, &XmlWriteOptions::default())
    }
}

// ─── Serialization options ───

/// Options controlling XML serialization output format.
#[derive(Debug, Clone)]
pub struct XmlWriteOptions {
    /// Indentation string per level (e.g. `"  "`, `"\t"`).
    /// `None` means compact output with no extra whitespace.
    pub indent: Option<String>,
    /// Use `<foo></foo>` instead of `<foo/>` for empty elements.
    /// Required for W3C Canonical XML (C14N).
    pub expand_empty_elements: bool,
}

impl XmlWriteOptions {
    /// Compact output: no indentation, self-closing empty elements.
    pub fn compact() -> Self {
        XmlWriteOptions {
            indent: None,
            expand_empty_elements: false,
        }
    }

    /// Pretty-printed output with the given indentation string.
    pub fn pretty(indent: impl Into<String>) -> Self {
        XmlWriteOptions {
            indent: Some(indent.into()),
            expand_empty_elements: false,
        }
    }

    /// Set whether empty elements use expanded form (`<foo></foo>`).
    pub fn with_expand_empty_elements(mut self, expand: bool) -> Self {
        self.expand_empty_elements = expand;
        self
    }
}

impl Default for XmlWriteOptions {
    fn default() -> Self {
        Self::compact()
    }
}

// ─── Escaping and helpers ───

/// Write indentation for the given depth.
fn write_indent(out: &mut dyn fmt::Write, opts: &XmlWriteOptions, depth: usize) -> fmt::Result {
    if let Some(ref indent) = opts.indent {
        for _ in 0..depth {
            out.write_str(indent)?;
        }
    }
    Ok(())
}

/// Write text content with XML escaping to a `fmt::Write` sink.
///
/// Per XML 1.0 and C14N rules:
/// - `&` → `&amp;`
/// - `<` → `&lt;`
/// - `>` → `&gt;`
/// - `\r` → `&#xD;` (preserves CR on round-trip; XML parser normalizes CR)
fn write_escaped_text(out: &mut dyn fmt::Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '&' => out.write_str("&amp;")?,
            '<' => out.write_str("&lt;")?,
            '>' => out.write_str("&gt;")?,
            '\r' => out.write_str("&#xD;")?,
            _ => out.write_char(c)?,
        }
    }
    Ok(())
}

/// Write attribute value with XML escaping to a `fmt::Write` sink.
///
/// Per XML 1.0 and C14N rules:
/// - `&` → `&amp;`
/// - `<` → `&lt;`
/// - `>` → `&gt;`
/// - `"` → `&quot;`
/// - `\t` → `&#x9;` (preserves tab; XML parser normalizes to space)
/// - `\n` → `&#xA;` (preserves newline; XML parser normalizes to space)
/// - `\r` → `&#xD;` (preserves CR; XML parser normalizes CR)
fn write_escaped_attr(out: &mut dyn fmt::Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '&' => out.write_str("&amp;")?,
            '<' => out.write_str("&lt;")?,
            '>' => out.write_str("&gt;")?,
            '"' => out.write_str("&quot;")?,
            '\t' => out.write_str("&#x9;")?,
            '\n' => out.write_str("&#xA;")?,
            '\r' => out.write_str("&#xD;")?,
            _ => out.write_char(c)?,
        }
    }
    Ok(())
}

/// Adapter that allows writing to an `io::Write` via the `fmt::Write` trait.
struct IoWriteAdapter<'a> {
    inner: &'a mut dyn std::io::Write,
}

impl<'a> fmt::Write for IoWriteAdapter<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.inner.write_all(s.as_bytes()).map_err(|_| fmt::Error)
    }
}
