//! DOM (Document Object Model) based on the XML Information Set specification.
//!
//! This module provides an arena-based tree representation of XML documents.
//! Each node is identified by a [`NodeId`] and stored in a central arena within
//! the [`Document`]. This avoids reference-counting overhead and makes tree
//! mutation straightforward.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

/// A unique identifier for a node within a [`Document`].
///
/// Node IDs are lightweight handles (just a `usize` index into the document's
/// arena). They are [`Copy`], [`Hash`], and can be compared for equality.
/// Use [`NodeId::index()`] to get the raw index and [`NodeId::new()`] to
/// construct from a raw index (e.g. for FFI or serialization).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) usize);

impl NodeId {
    /// Create a `NodeId` from a raw arena index.
    ///
    /// The caller is responsible for ensuring the index refers to a valid node
    /// in the intended [`Document`]. Passing an out-of-range index will not
    /// cause undefined behaviour, but operations on the resulting `NodeId` will
    /// return `None` or silently do nothing.
    pub fn new(index: usize) -> Self {
        NodeId(index)
    }

    /// Return the raw arena index of this node.
    pub fn index(&self) -> usize {
        self.0
    }
}

/// A qualified name consisting of an optional namespace URI, optional prefix,
/// and a local name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QName<'a> {
    /// The namespace URI, if any.
    pub namespace_uri: Option<Cow<'a, str>>,
    /// The namespace prefix, if any (e.g. `"soap"` in `soap:Envelope`).
    pub prefix: Option<Cow<'a, str>>,
    /// The local part of the name.
    pub local_name: Cow<'a, str>,
}

impl<'a> QName<'a> {
    /// Create a QName with only a local name (no namespace).
    pub fn local(name: impl Into<Cow<'a, str>>) -> Self {
        QName {
            namespace_uri: None,
            prefix: None,
            local_name: name.into(),
        }
    }

    /// Create a QName with a namespace URI and local name.
    pub fn with_namespace(
        namespace_uri: impl Into<Cow<'a, str>>,
        local_name: impl Into<Cow<'a, str>>,
    ) -> Self {
        QName {
            namespace_uri: Some(namespace_uri.into()),
            prefix: None,
            local_name: local_name.into(),
        }
    }

    /// Create a QName with prefix, namespace URI, and local name.
    pub fn full(
        prefix: impl Into<Cow<'a, str>>,
        namespace_uri: impl Into<Cow<'a, str>>,
        local_name: impl Into<Cow<'a, str>>,
    ) -> Self {
        QName {
            namespace_uri: Some(namespace_uri.into()),
            prefix: Some(prefix.into()),
            local_name: local_name.into(),
        }
    }

    /// Returns the prefixed form (e.g. `"soap:Envelope"`) or just the local name.
    pub fn prefixed_name(&self) -> Cow<'_, str> {
        match &self.prefix {
            Some(p) => Cow::Owned(format!("{}:{}", p, self.local_name)),
            None => Cow::Borrowed(&self.local_name),
        }
    }

    /// Convert this QName into a `'static` lifetime by taking ownership of all data.
    pub fn into_static(self) -> QName<'static> {
        QName {
            namespace_uri: self.namespace_uri.map(|s| Cow::Owned(s.into_owned())),
            prefix: self.prefix.map(|s| Cow::Owned(s.into_owned())),
            local_name: Cow::Owned(self.local_name.into_owned()),
        }
    }
}

impl<'a> fmt::Display for QName<'a> {
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
pub struct Attribute<'a> {
    /// The qualified name of the attribute.
    pub name: QName<'a>,
    /// The normalized attribute value.
    pub value: Cow<'a, str>,
}

impl<'a> Attribute<'a> {
    /// Convert this Attribute into a `'static` lifetime.
    pub fn into_static(self) -> Attribute<'static> {
        Attribute {
            name: self.name.into_static(),
            value: Cow::Owned(self.value.into_owned()),
        }
    }
}

/// The XML declaration (`<?xml version="1.0" encoding="UTF-8"?>`).
#[derive(Debug, Clone, PartialEq)]
pub struct XmlDeclaration<'a> {
    /// The XML version (e.g. `"1.0"`).
    pub version: Cow<'a, str>,
    /// The declared encoding (e.g. `"UTF-8"`), if specified.
    pub encoding: Option<Cow<'a, str>>,
    /// The standalone declaration, if specified (`true` for `"yes"`, `false` for `"no"`).
    pub standalone: Option<bool>,
}

impl<'a> XmlDeclaration<'a> {
    /// Convert this XmlDeclaration into a `'static` lifetime.
    pub fn into_static(self) -> XmlDeclaration<'static> {
        XmlDeclaration {
            version: Cow::Owned(self.version.into_owned()),
            encoding: self.encoding.map(|s| Cow::Owned(s.into_owned())),
            standalone: self.standalone,
        }
    }
}

/// A processing instruction (`<?target data?>`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingInstruction<'a> {
    /// The PI target name (e.g. `"xml-stylesheet"`).
    pub target: Cow<'a, str>,
    /// The PI data string, if any.
    pub data: Option<Cow<'a, str>>,
}

impl<'a> ProcessingInstruction<'a> {
    /// Convert this ProcessingInstruction into a `'static` lifetime.
    pub fn into_static(self) -> ProcessingInstruction<'static> {
        ProcessingInstruction {
            target: Cow::Owned(self.target.into_owned()),
            data: self.data.map(|s| Cow::Owned(s.into_owned())),
        }
    }
}

/// The different kinds of nodes in the DOM tree.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind<'a> {
    /// The document root (Infoset document information item).
    Document,
    /// An element node (Infoset element information item).
    Element(Element<'a>),
    /// A text node (Infoset character information item).
    Text(Cow<'a, str>),
    /// A CDATA section.
    CData(Cow<'a, str>),
    /// A comment node (Infoset comment information item).
    Comment(Cow<'a, str>),
    /// A processing instruction (Infoset PI information item).
    ProcessingInstruction(ProcessingInstruction<'a>),
    /// A virtual attribute node (used by XPath evaluation).
    /// Not part of the normal child tree.
    Attribute(QName<'a>, Cow<'a, str>),
}

impl<'a> NodeKind<'a> {
    /// Convert this NodeKind into a `'static` lifetime.
    pub fn into_static(self) -> NodeKind<'static> {
        match self {
            NodeKind::Document => NodeKind::Document,
            NodeKind::Element(e) => NodeKind::Element(e.into_static()),
            NodeKind::Text(t) => NodeKind::Text(Cow::Owned(t.into_owned())),
            NodeKind::CData(t) => NodeKind::CData(Cow::Owned(t.into_owned())),
            NodeKind::Comment(t) => NodeKind::Comment(Cow::Owned(t.into_owned())),
            NodeKind::ProcessingInstruction(pi) => {
                NodeKind::ProcessingInstruction(pi.into_static())
            }
            NodeKind::Attribute(name, value) => {
                NodeKind::Attribute(name.into_static(), Cow::Owned(value.into_owned()))
            }
        }
    }
}

/// An element with its qualified name and attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Element<'a> {
    /// The qualified name of the element.
    pub name: QName<'a>,
    /// The element's attributes.
    pub attributes: Vec<Attribute<'a>>,
    /// In-scope namespace declarations on this element.
    /// Each pair is (prefix, namespace_uri). Empty prefix for default namespace.
    pub namespace_declarations: Vec<(Cow<'a, str>, Cow<'a, str>)>,
}

impl<'a> Element<'a> {
    /// Get an attribute value by local name (ignoring namespace).
    pub fn get_attribute(&self, local_name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| *a.name.local_name == *local_name)
            .map(|a| &*a.value)
    }

    /// Get an attribute value by namespace URI and local name.
    pub fn get_attribute_ns(&self, namespace_uri: &str, local_name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| {
                *a.name.local_name == *local_name
                    && a.name.namespace_uri.as_deref() == Some(namespace_uri)
            })
            .map(|a| &*a.value)
    }

    /// Set or update an attribute. Returns the old value if the attribute already existed.
    pub fn set_attribute(&mut self, name: QName<'a>, value: Cow<'a, str>) -> Option<Cow<'a, str>> {
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
    pub fn remove_attribute(&mut self, local_name: &str) -> Option<Cow<'a, str>> {
        if let Some(pos) = self
            .attributes
            .iter()
            .position(|a| *a.name.local_name == *local_name)
        {
            Some(self.attributes.remove(pos).value)
        } else {
            None
        }
    }

    /// Convert this Element into a `'static` lifetime.
    pub fn into_static(self) -> Element<'static> {
        Element {
            name: self.name.into_static(),
            attributes: self
                .attributes
                .into_iter()
                .map(|a| a.into_static())
                .collect(),
            namespace_declarations: self
                .namespace_declarations
                .into_iter()
                .map(|(k, v)| (Cow::Owned(k.into_owned()), Cow::Owned(v.into_owned())))
                .collect::<Vec<_>>(),
        }
    }
}

/// Internal representation of a node in the arena.
#[derive(Debug, Clone)]
pub(crate) struct NodeData<'a> {
    pub kind: NodeKind<'a>,
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    /// Byte position in the original input for lazy line/column computation.
    pub byte_pos: usize,
    /// Byte position of the end of this node in the original input.
    pub byte_end_pos: usize,
}

impl<'a> NodeData<'a> {
    /// Convert this NodeData into a `'static` lifetime.
    pub fn into_static(self) -> NodeData<'static> {
        NodeData {
            kind: self.kind.into_static(),
            parent: self.parent,
            first_child: self.first_child,
            last_child: self.last_child,
            next_sibling: self.next_sibling,
            prev_sibling: self.prev_sibling,
            byte_pos: self.byte_pos,
            byte_end_pos: self.byte_end_pos,
        }
    }
}

/// An XML document represented as an arena-based tree.
///
/// Nodes are stored in a flat `Vec` and referenced by [`NodeId`]. This provides
/// O(1) node access and simple tree mutation without reference counting.
#[derive(Debug, Clone)]
pub struct Document<'a> {
    /// The node arena.
    pub(crate) nodes: Vec<NodeData<'a>>,
    /// The root node id (always NodeId(0), the Document node).
    root: NodeId,
    /// Optional XML declaration.
    pub xml_declaration: Option<XmlDeclaration<'a>>,
    /// Raw DOCTYPE declaration text, preserved verbatim for round-trip fidelity.
    /// e.g. `<!DOCTYPE root SYSTEM "root.dtd">` or `<!DOCTYPE html>`.
    pub doctype: Option<Cow<'a, str>>,
    /// Attribute nodes for each element, keyed by element NodeId.
    /// These are virtual nodes used by XPath attribute axis traversal.
    pub(crate) attribute_nodes: HashMap<NodeId, Vec<NodeId>>,
    /// Original input for lazy line/column computation from byte positions.
    pub(crate) input: &'a str,
}

impl<'a> Document<'a> {
    /// Create a new empty document.
    pub fn new() -> Self {
        let root_node = NodeData {
            kind: NodeKind::Document,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            byte_pos: 0,
            byte_end_pos: 0,
        };
        Document {
            nodes: vec![root_node],
            root: NodeId(0),
            xml_declaration: None,
            doctype: None,
            attribute_nodes: HashMap::new(),
            input: "",
        }
    }

    /// Convert this Document into a `'static` lifetime by taking ownership of all data.
    pub fn into_static(self) -> Document<'static> {
        Document {
            nodes: self.nodes.into_iter().map(|n| n.into_static()).collect(),
            root: self.root,
            xml_declaration: self.xml_declaration.map(|d| d.into_static()),
            doctype: self.doctype.map(|s| Cow::Owned(s.into_owned())),
            attribute_nodes: self.attribute_nodes,
            input: "",
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
    pub(crate) fn alloc_node(&mut self, kind: NodeKind<'a>, byte_pos: usize) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(NodeData {
            kind,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            byte_pos,
            byte_end_pos: 0,
        });
        id
    }

    /// Set the byte end position of a node.
    pub(crate) fn set_byte_end_pos(&mut self, id: NodeId, pos: usize) {
        if let Some(node) = self.nodes.get_mut(id.0) {
            node.byte_end_pos = pos;
        }
    }

    /// Allocate virtual attribute nodes for an element.
    /// Call this after adding an element with attributes to enable XPath attribute axis.
    pub(crate) fn build_attribute_nodes(&mut self, element_id: NodeId) {
        let attrs: Vec<(QName<'a>, Cow<'a, str>)> = match self.node_kind(element_id) {
            Some(NodeKind::Element(e)) => e
                .attributes
                .iter()
                .map(|a| (a.name.clone(), a.value.clone()))
                .collect(),
            _ => return,
        };
        let mut attr_ids = Vec::with_capacity(attrs.len());
        for (name, value) in attrs {
            let attr_id = self.alloc_node(NodeKind::Attribute(name, value), 0);
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

    /// Get the virtual attribute node IDs for an element.
    ///
    /// Returns an empty slice if [`prepare_xpath()`](Self::prepare_xpath) has
    /// not been called or the element has no attributes.
    pub fn get_attribute_nodes(&self, element_id: NodeId) -> &[NodeId] {
        self.attribute_nodes
            .get(&element_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Build virtual attribute nodes for all elements in the document.
    /// Must be called before XPath evaluation if the document was parsed
    /// without attribute node construction (the default for performance).
    pub fn prepare_xpath(&mut self) {
        if !self.attribute_nodes.is_empty() {
            return; // Already prepared
        }
        let element_ids: Vec<NodeId> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match &n.kind {
                NodeKind::Element(e) if !e.attributes.is_empty() => Some(NodeId(i)),
                _ => None,
            })
            .collect();
        for elem_id in element_ids {
            self.build_attribute_nodes(elem_id);
        }
    }

    /// Create a new element node (not yet attached to the tree).
    pub fn create_element(&mut self, name: QName<'a>) -> NodeId {
        self.alloc_node(
            NodeKind::Element(Element {
                name,
                attributes: Vec::new(),
                namespace_declarations: Vec::new(),
            }),
            0,
        )
    }

    /// Create a new text node (not yet attached to the tree).
    pub fn create_text(&mut self, text: impl Into<Cow<'a, str>>) -> NodeId {
        self.alloc_node(NodeKind::Text(text.into()), 0)
    }

    /// Create a new comment node (not yet attached to the tree).
    pub fn create_comment(&mut self, text: impl Into<Cow<'a, str>>) -> NodeId {
        self.alloc_node(NodeKind::Comment(text.into()), 0)
    }

    /// Create a new processing instruction node (not yet attached to the tree).
    pub fn create_processing_instruction(
        &mut self,
        target: impl Into<Cow<'a, str>>,
        data: Option<Cow<'a, str>>,
    ) -> NodeId {
        self.alloc_node(
            NodeKind::ProcessingInstruction(ProcessingInstruction {
                target: target.into(),
                data,
            }),
            0,
        )
    }

    /// Create a new CDATA node (not yet attached to the tree).
    pub fn create_cdata(&mut self, text: impl Into<Cow<'a, str>>) -> NodeId {
        self.alloc_node(NodeKind::CData(text.into()), 0)
    }

    // ─── Tree access ───

    /// Get the kind of a node.
    pub fn node_kind(&self, id: NodeId) -> Option<&NodeKind<'a>> {
        self.nodes.get(id.0).map(|n| &n.kind)
    }

    /// Get a mutable reference to a node's kind.
    pub fn node_kind_mut(&mut self, id: NodeId) -> Option<&mut NodeKind<'a>> {
        self.nodes.get_mut(id.0).map(|n| &mut n.kind)
    }

    /// Get the element data for an element node.
    pub fn element(&self, id: NodeId) -> Option<&Element<'a>> {
        match self.node_kind(id) {
            Some(NodeKind::Element(e)) => Some(e),
            _ => None,
        }
    }

    /// Get mutable element data for an element node.
    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element<'a>> {
        match self.node_kind_mut(id) {
            Some(NodeKind::Element(e)) => Some(e),
            _ => None,
        }
    }

    /// Get the text content of a text or CDATA node.
    pub fn text_content(&self, id: NodeId) -> Option<&str> {
        match self.node_kind(id) {
            Some(NodeKind::Text(t)) => Some(t),
            Some(NodeKind::CData(t)) => Some(t),
            _ => None,
        }
    }

    /// Get the parent of a node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.parent)
    }

    /// Get the children of a node.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut current = self.nodes.get(id.0).and_then(|n| n.first_child);
        while let Some(child_id) = current {
            result.push(child_id);
            current = self.nodes.get(child_id.0).and_then(|n| n.next_sibling);
        }
        result
    }

    /// Get the source line of a node (computed lazily from byte position).
    pub fn node_line(&self, id: NodeId) -> usize {
        let byte_pos = match self.nodes.get(id.0) {
            Some(n) => n.byte_pos,
            None => return 0,
        };
        if self.input.is_empty() || byte_pos == 0 {
            return 1;
        }
        self.input.as_bytes()[..byte_pos]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1
    }

    /// Get the source column of a node (computed lazily from byte position).
    pub fn node_column(&self, id: NodeId) -> usize {
        let byte_pos = match self.nodes.get(id.0) {
            Some(n) => n.byte_pos,
            None => return 0,
        };
        if self.input.is_empty() || byte_pos == 0 {
            return 1;
        }
        let bytes = &self.input.as_bytes()[..byte_pos];
        match bytes.iter().rposition(|&b| b == b'\n') {
            Some(nl_pos) => byte_pos - nl_pos,
            None => byte_pos + 1,
        }
    }

    /// Returns the byte range of a node in the original source text.
    ///
    /// The range spans from the opening `<` of the element (or start of text/comment/PI)
    /// to the closing `>` of the end tag (or `/>` for self-closing elements).
    ///
    /// Returns `None` if the node was programmatically created (not parsed from source)
    /// or if the node ID is invalid.
    ///
    /// # Example
    /// ```
    /// let xml = r#"<root><child>text</child></root>"#;
    /// let doc = uppsala::parse(xml).unwrap();
    /// let root = doc.document_element().unwrap();
    /// let child_id = doc.children(root)[0];
    /// let range = doc.node_range(child_id).unwrap();
    /// assert_eq!(&xml[range], "<child>text</child>");
    /// ```
    pub fn node_range(&self, id: NodeId) -> Option<std::ops::Range<usize>> {
        let node = self.nodes.get(id.0)?;
        if node.byte_end_pos == 0 && id.0 != 0 {
            return None; // Programmatically created node
        }
        Some(node.byte_pos..node.byte_end_pos)
    }

    /// Returns the original source text of a node as a string slice.
    ///
    /// This is a convenience method equivalent to `&input[doc.node_range(id)?]`.
    /// Returns the exact text from the original XML input that produced this node.
    ///
    /// Returns `None` if the node was programmatically created or the ID is invalid.
    ///
    /// # Example
    /// ```
    /// let xml = r#"<root><item id="1">hello</item></root>"#;
    /// let doc = uppsala::parse(xml).unwrap();
    /// let root = doc.document_element().unwrap();
    /// let item = doc.children(root)[0];
    /// assert_eq!(doc.node_source(item).unwrap(), r#"<item id="1">hello</item>"#);
    /// ```
    pub fn node_source(&self, id: NodeId) -> Option<&'a str> {
        let range = self.node_range(id)?;
        if range.end > self.input.len() {
            return None;
        }
        Some(&self.input[range])
    }

    /// Returns the original input text that was parsed to create this document.
    ///
    /// Returns an empty string for programmatically constructed documents.
    pub fn input_text(&self) -> &'a str {
        self.input
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
            if *e.name.local_name == *local_name {
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
            if *e.name.local_name == *local_name
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
        self.append_child_unchecked(parent, child);
    }

    /// Append a freshly-allocated child node to a parent without detaching.
    /// The child must have no parent, no siblings. Used during parsing for speed.
    #[inline]
    pub(crate) fn append_child_unchecked(&mut self, parent: NodeId, child: NodeId) {
        // Set new parent
        self.nodes[child.0].parent = Some(parent);
        // Link into parent's child list
        let last = self.nodes[parent.0].last_child;
        if let Some(last_id) = last {
            // Append after last child
            self.nodes[last_id.0].next_sibling = Some(child);
            self.nodes[child.0].prev_sibling = Some(last_id);
            self.nodes[parent.0].last_child = Some(child);
        } else {
            // First child
            self.nodes[parent.0].first_child = Some(child);
            self.nodes[parent.0].last_child = Some(child);
        }
    }

    /// Insert a child before a reference node. Both must share the same parent.
    pub fn insert_before(&mut self, parent: NodeId, new_child: NodeId, reference: NodeId) {
        self.detach(new_child);
        if let Some(node) = self.nodes.get_mut(new_child.0) {
            node.parent = Some(parent);
        }
        let prev = self.nodes.get(reference.0).and_then(|n| n.prev_sibling);
        // Link new_child before reference
        if let Some(nc) = self.nodes.get_mut(new_child.0) {
            nc.prev_sibling = prev;
            nc.next_sibling = Some(reference);
        }
        if let Some(r) = self.nodes.get_mut(reference.0) {
            r.prev_sibling = Some(new_child);
        }
        if let Some(prev_id) = prev {
            if let Some(p) = self.nodes.get_mut(prev_id.0) {
                p.next_sibling = Some(new_child);
            }
        } else {
            // new_child is now the first child
            if let Some(p) = self.nodes.get_mut(parent.0) {
                p.first_child = Some(new_child);
            }
        }
    }

    /// Insert a child after a reference node.
    pub fn insert_after(&mut self, parent: NodeId, new_child: NodeId, reference: NodeId) {
        self.detach(new_child);
        if let Some(node) = self.nodes.get_mut(new_child.0) {
            node.parent = Some(parent);
        }
        let next = self.nodes.get(reference.0).and_then(|n| n.next_sibling);
        if let Some(nc) = self.nodes.get_mut(new_child.0) {
            nc.prev_sibling = Some(reference);
            nc.next_sibling = next;
        }
        if let Some(r) = self.nodes.get_mut(reference.0) {
            r.next_sibling = Some(new_child);
        }
        if let Some(next_id) = next {
            if let Some(n) = self.nodes.get_mut(next_id.0) {
                n.prev_sibling = Some(new_child);
            }
        } else {
            // new_child is now the last child
            if let Some(p) = self.nodes.get_mut(parent.0) {
                p.last_child = Some(new_child);
            }
        }
    }

    /// Remove a child from its parent. The node remains in the arena but is detached.
    pub fn remove_child(&mut self, _parent: NodeId, child: NodeId) {
        self.detach(child);
    }

    /// Replace an old child with a new child under the given parent.
    pub fn replace_child(&mut self, parent: NodeId, new_child: NodeId, old_child: NodeId) {
        self.detach(new_child);
        let prev = self.nodes.get(old_child.0).and_then(|n| n.prev_sibling);
        let next = self.nodes.get(old_child.0).and_then(|n| n.next_sibling);
        // Set new_child links
        if let Some(nc) = self.nodes.get_mut(new_child.0) {
            nc.parent = Some(parent);
            nc.prev_sibling = prev;
            nc.next_sibling = next;
        }
        // Update neighbors
        if let Some(prev_id) = prev {
            if let Some(p) = self.nodes.get_mut(prev_id.0) {
                p.next_sibling = Some(new_child);
            }
        } else if let Some(p) = self.nodes.get_mut(parent.0) {
            p.first_child = Some(new_child);
        }
        if let Some(next_id) = next {
            if let Some(n) = self.nodes.get_mut(next_id.0) {
                n.prev_sibling = Some(new_child);
            }
        } else if let Some(p) = self.nodes.get_mut(parent.0) {
            p.last_child = Some(new_child);
        }
        // Detach old_child
        if let Some(oc) = self.nodes.get_mut(old_child.0) {
            oc.parent = None;
            oc.prev_sibling = None;
            oc.next_sibling = None;
        }
    }

    /// Detach a node from its parent, removing it from the tree.
    ///
    /// The node remains in the arena and can be re-attached elsewhere with
    /// [`append_child`](Self::append_child), [`insert_before`](Self::insert_before),
    /// or [`insert_after`](Self::insert_after).
    pub fn detach(&mut self, id: NodeId) {
        let (parent_id, prev, next) = match self.nodes.get(id.0) {
            Some(n) => (n.parent, n.prev_sibling, n.next_sibling),
            None => return,
        };
        if let Some(parent_id) = parent_id {
            // Update prev sibling or parent's first_child
            if let Some(prev_id) = prev {
                if let Some(p) = self.nodes.get_mut(prev_id.0) {
                    p.next_sibling = next;
                }
            } else if let Some(p) = self.nodes.get_mut(parent_id.0) {
                p.first_child = next;
            }
            // Update next sibling or parent's last_child
            if let Some(next_id) = next {
                if let Some(n) = self.nodes.get_mut(next_id.0) {
                    n.prev_sibling = prev;
                }
            } else if let Some(p) = self.nodes.get_mut(parent_id.0) {
                p.last_child = prev;
            }
            // Clear the detached node's links
            if let Some(node) = self.nodes.get_mut(id.0) {
                node.parent = None;
                node.prev_sibling = None;
                node.next_sibling = None;
            }
        }
    }

    // ─── Navigation helpers ───

    /// Get the first child of a node.
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.first_child)
    }

    /// Get the last child of a node.
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.last_child)
    }

    /// Get the next sibling of a node.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.next_sibling)
    }

    /// Get the previous sibling of a node.
    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.prev_sibling)
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

    /// Write the entire document to any `io::Write` sink (file, socket, `Vec<u8>`, etc.)
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
            .map_err(|e| std::io::Error::other(e.to_string()))
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
                let pname = elem.name.prefixed_name();
                out.write_str(&pname)?;
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
                    let aname = attr.name.prefixed_name();
                    out.write_str(&aname)?;
                    out.write_str("=\"")?;
                    write_escaped_attr(out, &attr.value)?;
                    out.write_char('"')?;
                }
                let children = self.children(id);
                if children.is_empty() {
                    if opts.expand_empty_elements {
                        out.write_str("></")?;
                        out.write_str(&pname)?;
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
                    out.write_str(&pname)?;
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

impl<'a> Default for Document<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> fmt::Display for Document<'a> {
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
struct IoWriteAdapter<'w> {
    inner: &'w mut dyn std::io::Write,
}

impl<'w> fmt::Write for IoWriteAdapter<'w> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.inner.write_all(s.as_bytes()).map_err(|_| fmt::Error)
    }
}
