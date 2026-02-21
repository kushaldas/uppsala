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

    /// Serialize the document back to an XML string.
    pub fn to_xml(&self) -> String {
        let mut output = String::new();
        if let Some(decl) = &self.xml_declaration {
            output.push_str("<?xml version=\"");
            output.push_str(&decl.version);
            output.push('"');
            if let Some(enc) = &decl.encoding {
                output.push_str(" encoding=\"");
                output.push_str(enc);
                output.push('"');
            }
            if let Some(sa) = decl.standalone {
                output.push_str(" standalone=\"");
                output.push_str(if sa { "yes" } else { "no" });
                output.push('"');
            }
            output.push_str("?>");
        }
        for child in self.children(self.root) {
            self.serialize_node(child, &mut output);
        }
        output
    }

    fn serialize_node(&self, id: NodeId, out: &mut String) {
        match self.node_kind(id) {
            Some(NodeKind::Element(elem)) => {
                out.push('<');
                out.push_str(&elem.name.prefixed_name());
                // Namespace declarations
                for (prefix, uri) in &elem.namespace_declarations {
                    if prefix.is_empty() {
                        out.push_str(" xmlns=\"");
                    } else {
                        out.push_str(" xmlns:");
                        out.push_str(prefix);
                        out.push_str("=\"");
                    }
                    out.push_str(&escape_attr(uri));
                    out.push('"');
                }
                // Attributes
                for attr in &elem.attributes {
                    out.push(' ');
                    out.push_str(&attr.name.prefixed_name());
                    out.push_str("=\"");
                    out.push_str(&escape_attr(&attr.value));
                    out.push('"');
                }
                let children = self.children(id);
                if children.is_empty() {
                    out.push_str("/>");
                } else {
                    out.push('>');
                    for child in children {
                        self.serialize_node(child, out);
                    }
                    out.push_str("</");
                    out.push_str(&elem.name.prefixed_name());
                    out.push('>');
                }
            }
            Some(NodeKind::Text(text)) => {
                out.push_str(&escape_text(text));
            }
            Some(NodeKind::CData(text)) => {
                out.push_str("<![CDATA[");
                out.push_str(text);
                out.push_str("]]>");
            }
            Some(NodeKind::Comment(text)) => {
                out.push_str("<!--");
                out.push_str(text);
                out.push_str("-->");
            }
            Some(NodeKind::ProcessingInstruction(pi)) => {
                out.push_str("<?");
                out.push_str(&pi.target);
                if let Some(data) = &pi.data {
                    out.push(' ');
                    out.push_str(data);
                }
                out.push_str("?>");
            }
            Some(NodeKind::Document) => {
                for child in self.children(id) {
                    self.serialize_node(child, out);
                }
            }
            Some(NodeKind::Attribute(_, _)) => {
                // Virtual attribute nodes are not serialized as children.
            }
            None => {}
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

/// Escape special characters in text content.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape special characters in attribute values.
fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}
