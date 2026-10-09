//! DOM (Document Object Model) based on the XML Information Set specification.
//!
//! This module provides an arena-based tree representation of XML documents.
//! Each node is identified by a [`NodeId`] and stored in a central arena within
//! the [`Document`]. This avoids reference-counting overhead and makes tree
//! mutation straightforward.

use crate::fasthash::{FastHashMap, FastHashSet};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
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

    /// Check whether this QName matches the given namespace URI and local name.
    ///
    /// Pass `Some("...")` for namespaced names or `None` for names without a
    /// namespace.
    ///
    /// # Example
    ///
    /// ```
    /// use uppsala::QName;
    /// let q = QName::with_namespace("urn:example", "Foo");
    /// assert!(q.matches(Some("urn:example"), "Foo"));
    /// assert!(!q.matches(Some("urn:other"), "Foo"));
    /// assert!(!q.matches(None, "Foo"));
    /// ```
    pub fn matches(&self, namespace_uri: Option<&str>, local_name: &str) -> bool {
        *self.local_name == *local_name && self.namespace_uri.as_deref() == namespace_uri
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

/// Clone a borrowed [`QName`] into an owned (`'static`) one. Used when importing
/// a subtree from another document, where every string must be owned so the
/// result does not borrow from the source. The `'static` result coerces to any
/// `QName<'a>` via the type's covariance in its lifetime.
fn own_qname(q: &QName<'_>) -> QName<'static> {
    QName {
        namespace_uri: q.namespace_uri.as_ref().map(|s| Cow::Owned(s.to_string())),
        prefix: q.prefix.as_ref().map(|s| Cow::Owned(s.to_string())),
        local_name: Cow::Owned(q.local_name.to_string()),
    }
}

/// An iterator over the children of a node.
///
/// This is a zero-allocation alternative to [`Document::children()`] which
/// returns a `Vec<NodeId>`. It walks the linked sibling chain directly.
pub struct ChildrenIter<'d, 'a> {
    doc: &'d Document<'a>,
    next: Option<NodeId>,
    next_back: Option<NodeId>,
}

impl<'d, 'a> Iterator for ChildrenIter<'d, 'a> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.next?;
        if Some(id) == self.next_back {
            self.next = None;
            self.next_back = None;
        } else {
            self.next = self.doc.nodes.get(id.0).and_then(|n| n.next_sibling);
        }
        Some(id)
    }
}

impl<'d, 'a> DoubleEndedIterator for ChildrenIter<'d, 'a> {
    fn next_back(&mut self) -> Option<NodeId> {
        let id = self.next_back?;
        if Some(id) == self.next {
            self.next = None;
            self.next_back = None;
        } else {
            self.next_back = self.doc.nodes.get(id.0).and_then(|n| n.prev_sibling);
        }
        Some(id)
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

    /// Check whether this element matches the given namespace URI and local name.
    ///
    /// Convenience wrapper around `self.name.matches(Some(ns), local)`.
    ///
    /// # Example
    ///
    /// ```
    /// let xml = r#"<saml:Issuer xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion">x</saml:Issuer>"#;
    /// let doc = uppsala::parse(xml).unwrap();
    /// let root = doc.document_element().unwrap();
    /// let elem = doc.element(root).unwrap();
    /// assert!(elem.matches_name_ns("urn:oasis:names:tc:SAML:2.0:assertion", "Issuer"));
    /// assert!(!elem.matches_name_ns("urn:other", "Issuer"));
    /// ```
    pub fn matches_name_ns(&self, namespace_uri: &str, local_name: &str) -> bool {
        self.name.matches(Some(namespace_uri), local_name)
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
    pub(crate) attribute_nodes: FastHashMap<NodeId, Vec<NodeId>>,
    /// Precomputed document-order position per node, indexed by `NodeId`.
    /// Populated by [`Self::prepare_xpath`]; empty otherwise. Lets node-set
    /// deduplication sort by an O(1) key instead of walking each node to the root
    /// (which is quadratic over wide trees). Unreachable nodes get `u32::MAX`.
    pub(crate) doc_order: Vec<u32>,
    /// Set when a tree/attribute mutation has invalidated the XPath caches
    /// (`attribute_nodes` + `doc_order`), so the next [`Self::prepare_xpath`]
    /// rebuilds them. `true` initially (caches unbuilt); cleared once built and
    /// re-set by every mutator. Keeps `prepare_xpath` a build-once cost on the
    /// common parse -> prepare -> query path while still allowing a mutated
    /// document to be re-prepared.
    pub(crate) xpath_dirty: bool,
    /// Recycle pool of arena slots that previously held virtual attribute
    /// nodes. The arena is append-only, so without reuse every re-preparation
    /// after a mutation would append a full fresh set of attribute nodes and
    /// orphan the old ones; a mutate -> query -> mutate -> query workload
    /// (e.g. pyFF's `set_entity_attributes`) then grows the arena
    /// quadratically and exhausts memory. Re-preparation keeps an element's
    /// existing slots when its attribute count is unchanged (so attribute
    /// `NodeId`s stay stable across unrelated mutations); only the slots of
    /// elements whose attribute list changed shape are drained in here and
    /// overwritten in place by [`Self::build_attribute_nodes`] before new
    /// ones are allocated, keeping the arena size flat.
    pub(crate) attr_node_pool: Vec<NodeId>,
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
            attribute_nodes: FastHashMap::default(),
            doc_order: Vec::new(),
            xpath_dirty: true,
            attr_node_pool: Vec::new(),
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
            doc_order: self.doc_order,
            xpath_dirty: self.xpath_dirty,
            attr_node_pool: self.attr_node_pool,
            input: "",
        }
    }

    /// Returns the root (Document) node id.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Returns the document element (the single top-level element), if any.
    pub fn document_element(&self) -> Option<NodeId> {
        self.children_iter(self.root)
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
        // Stable reuse: if this element already has a slot set of the right
        // size from a previous prepare_xpath generation, refresh those same
        // slots in place. Attribute NodeIds for elements whose attribute list
        // did not change shape therefore survive re-preparation, so a handle
        // cached across an unrelated mutation keeps pointing at the same
        // attribute instead of aliasing whatever attribute happens to land in
        // a recycled slot.
        if let Some(ids) = self.attribute_nodes.remove(&element_id) {
            if ids.len() == attrs.len() {
                for (id, (name, value)) in ids.iter().zip(attrs) {
                    self.nodes[id.0] = NodeData {
                        kind: NodeKind::Attribute(name, value),
                        parent: Some(element_id),
                        first_child: None,
                        last_child: None,
                        next_sibling: None,
                        prev_sibling: None,
                        byte_pos: 0,
                        byte_end_pos: 0,
                    };
                }
                self.attribute_nodes.insert(element_id, ids);
                return;
            }
            // The attribute count changed: this element's old slots go to the
            // pool and a fresh set is assigned below.
            self.attr_node_pool.extend(ids);
        }
        let mut attr_ids = Vec::with_capacity(attrs.len());
        for (name, value) in attrs {
            // Reuse a recycled slot from a previous prepare_xpath generation
            // when one is available (see `attr_node_pool`); the arena is
            // append-only, so this is what keeps repeated re-preparations from
            // growing it without bound. Fall back to appending a fresh slot.
            let attr_id = match self.attr_node_pool.pop() {
                Some(id) => {
                    self.nodes[id.0] = NodeData {
                        kind: NodeKind::Attribute(name, value),
                        parent: Some(element_id),
                        first_child: None,
                        last_child: None,
                        next_sibling: None,
                        prev_sibling: None,
                        byte_pos: 0,
                        byte_end_pos: 0,
                    };
                    id
                }
                None => {
                    let id = self.alloc_node(NodeKind::Attribute(name, value), 0);
                    // Set parent to the element (attribute nodes have an owner element)
                    if let Some(node) = self.nodes.get_mut(id.0) {
                        node.parent = Some(element_id);
                    }
                    id
                }
            };
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
    ///
    /// # Handle stability
    ///
    /// The returned `NodeId`s stay valid across a mutation followed by
    /// re-[`prepare_xpath()`](Self::prepare_xpath) as long as the element's
    /// own attribute count is unchanged. If attributes were added to or
    /// removed from the element (or the element no longer has any), its old
    /// attribute `NodeId`s are invalidated and their arena slots may be
    /// repurposed for other attributes -- do not read through them.
    pub fn get_attribute_nodes(&self, element_id: NodeId) -> &[NodeId] {
        self.attribute_nodes
            .get(&element_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Build virtual attribute nodes for all elements in the document.
    /// Must be called before XPath evaluation if the document was parsed
    /// without attribute node construction (the default for performance).
    ///
    /// Re-preparing after a mutation recycles the arena slots of superseded
    /// virtual attribute nodes rather than leaking them. Attribute `NodeId`s
    /// obtained earlier (from XPath results or
    /// [`get_attribute_nodes()`](Self::get_attribute_nodes)) remain valid for
    /// elements whose attribute count is unchanged; for elements whose
    /// attribute list changed shape they are invalidated and may be
    /// repurposed for different attributes.
    pub fn prepare_xpath(&mut self) {
        // Build-once on the common parse -> prepare -> query path: skip the work
        // when no mutation has invalidated the caches since the last prepare.
        // A mutation sets `xpath_dirty`, so a document edited after an earlier
        // prepare still refreshes here (guarding on "is the cache empty?" instead
        // would make re-preparation a silent no-op and leave a stale
        // document-order index, corrupting node-set ordering/dedup). Rebuilding
        // is O(n) and runs only on the first prepare or after an edit, never per
        // node.
        if !self.xpath_dirty {
            return;
        }
        // Recycle superseded virtual attribute nodes instead of orphaning
        // them: the arena is append-only, so leaking a full set per
        // re-preparation makes a mutate/query loop grow the arena without
        // bound (observed as multi-GB blowups in pyFF). Elements whose
        // attribute count is unchanged keep their exact slot set (refreshed in
        // place by `build_attribute_nodes`), so their attribute NodeIds remain
        // stable across re-preparation. Only the slots of elements that no
        // longer carry attributes -- and, inside `build_attribute_nodes`, of
        // elements whose attribute count changed -- go through the pool and
        // may be repurposed; NodeIds pointing at those were invalidated by the
        // mutation itself.
        let element_ids: Vec<NodeId> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match &n.kind {
                NodeKind::Element(e) if !e.attributes.is_empty() => Some(NodeId(i)),
                _ => None,
            })
            .collect();
        if self.attribute_nodes.is_empty() {
            // First preparation knows the number of virtual attributes up
            // front. Reserve once instead of repeatedly moving the growing
            // node arena as each element's attributes are appended. Existing
            // recycled slots do not need new arena space. Reservations remain
            // best-effort, like the parser's arena capacity hint.
            let attributes = element_ids.iter().fold(0usize, |count, &id| {
                count.saturating_add(self.element(id).map_or(0, |e| e.attributes.len()))
            });
            let _ = self
                .nodes
                .try_reserve(attributes.saturating_sub(self.attr_node_pool.len()));
            let _ = self.attribute_nodes.try_reserve(element_ids.len());
        }
        if !self.attribute_nodes.is_empty() {
            let live: FastHashSet<NodeId> = element_ids.iter().copied().collect();
            let stale: Vec<NodeId> = self
                .attribute_nodes
                .keys()
                .filter(|k| !live.contains(k))
                .copied()
                .collect();
            for key in stale {
                if let Some(ids) = self.attribute_nodes.remove(&key) {
                    self.attr_node_pool.extend(ids);
                }
            }
        }
        for elem_id in element_ids {
            self.build_attribute_nodes(elem_id);
        }
        self.compute_doc_order();
        self.xpath_dirty = false;
    }

    /// Mark the XPath caches (virtual attribute nodes + document-order index)
    /// stale so the next [`Self::prepare_xpath`] rebuilds them. Called by every
    /// tree/attribute mutation; setting a bool is cheap enough to never matter on
    /// a hot path, and it cannot be forgotten the way an external invalidation
    /// call could.
    fn invalidate_xpath_caches(&mut self) {
        self.xpath_dirty = true;
    }

    /// Assign each node its document-order position into `self.doc_order`, by a
    /// preorder tree walk: an element, then its attribute nodes (in attribute
    /// order), then its children (in document order) — matching XPath 1.0
    /// document order. Must run after attribute nodes are built. Unreachable
    /// nodes keep `u32::MAX`. O(n); used by node-set dedup to avoid per-call
    /// ancestor re-indexing (see `dedup_document_order` in `xpath.rs`).
    fn compute_doc_order(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let mut order = vec![u32::MAX; self.nodes.len()];
        let mut counter: u32 = 0;
        let mut stack: Vec<NodeId> = vec![self.root];
        while let Some(n) = stack.pop() {
            order[n.0] = counter;
            counter = counter.wrapping_add(1);
            if let Some(attrs) = self.attribute_nodes.get(&n) {
                for &a in attrs {
                    if let Some(slot) = order.get_mut(a.0) {
                        *slot = counter;
                        counter = counter.wrapping_add(1);
                    }
                }
            }
            // Push children in reverse document order so they pop in order.
            let start = stack.len();
            let mut child = self.nodes[n.0].first_child;
            while let Some(cid) = child {
                stack.push(cid);
                child = self.nodes[cid.0].next_sibling;
            }
            stack[start..].reverse();
        }
        self.doc_order = order;
    }

    /// Whether the document-order index has been computed (`prepare_xpath`).
    pub(crate) fn doc_order_ready(&self) -> bool {
        !self.doc_order.is_empty()
    }

    /// The document-order position of `id` (`u32::MAX` if unreachable or out of
    /// range). Only meaningful when [`Self::doc_order_ready`].
    pub(crate) fn doc_order_at(&self, id: NodeId) -> u32 {
        self.doc_order.get(id.0).copied().unwrap_or(u32::MAX)
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

    /// Declare a namespace binding on an element node.
    ///
    /// `prefix` is `None` (or `""`) for the default namespace. If the element
    /// already declares `prefix`, its URI is updated in place; otherwise a new
    /// declaration is appended. This writes to [`Element::namespace_declarations`]
    /// — the proper home for `xmlns`/`xmlns:*` output — rather than adding an
    /// attribute, so it serializes as a real namespace declaration.
    ///
    /// Returns `true` if `node` is an element (and the binding was recorded),
    /// `false` otherwise. Note that the serializer still filters reserved
    /// bindings (the `xmlns` prefix, `xml` bound to a non-XML URI, or any prefix
    /// bound to the XMLNS namespace), so declaring those here is a no-op on output.
    pub fn declare_namespace(
        &mut self,
        node: NodeId,
        prefix: Option<&str>,
        uri: impl Into<Cow<'a, str>>,
    ) -> bool {
        let prefix = prefix.unwrap_or("");
        match self.element_mut(node) {
            Some(el) => {
                let uri = uri.into();
                match el
                    .namespace_declarations
                    .iter_mut()
                    .find(|(p, _)| p.as_ref() == prefix)
                {
                    Some(slot) => slot.1 = uri,
                    None => {
                        let prefix = if prefix.is_empty() {
                            Cow::Borrowed("")
                        } else {
                            Cow::Owned(prefix.to_string())
                        };
                        el.namespace_declarations.push((prefix, uri));
                    }
                }
                true
            }
            None => false,
        }
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
        // Conservatively invalidate: the caller may rename, re-kind, or edit the
        // attributes of the node through this `&mut` (also covers `element_mut`,
        // which delegates here).
        self.invalidate_xpath_caches();
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

    /// Get the text of an element's first Text or CDATA child, zero-copy.
    ///
    /// This is the common operation of reading the text inside an element like
    /// `<Name>value</Name>`. Unlike [`text_content_deep`](Self::text_content_deep)
    /// this does **not** allocate — it returns a borrowed `&str` from the
    /// original parsed input.
    ///
    /// Returns `None` if the node has no text/CDATA children.
    ///
    /// # Example
    ///
    /// ```
    /// let doc = uppsala::parse("<name>hello</name>").unwrap();
    /// let root = doc.document_element().unwrap();
    /// assert_eq!(doc.element_text(root), Some("hello"));
    /// ```
    pub fn element_text(&self, id: NodeId) -> Option<&str> {
        let mut child = self.nodes.get(id.0).and_then(|n| n.first_child);
        while let Some(cid) = child {
            match self.node_kind(cid) {
                Some(NodeKind::Text(t)) => return Some(t),
                Some(NodeKind::CData(t)) => return Some(t),
                _ => {}
            }
            child = self.nodes.get(cid.0).and_then(|n| n.next_sibling);
        }
        None
    }

    /// Get an attribute value by local name directly from a node ID.
    ///
    /// This is a convenience shortcut for `doc.element(id)?.get_attribute(name)`.
    ///
    /// # Example
    ///
    /// ```
    /// let doc = uppsala::parse(r#"<item id="42" status="active"/>"#).unwrap();
    /// let root = doc.document_element().unwrap();
    /// assert_eq!(doc.get_attribute(root, "id"), Some("42"));
    /// assert_eq!(doc.get_attribute(root, "missing"), None);
    /// ```
    pub fn get_attribute(&self, id: NodeId, local_name: &str) -> Option<&str> {
        self.element(id)?.get_attribute(local_name)
    }

    /// Get an attribute value by namespace URI and local name directly from a node ID.
    ///
    /// This is a convenience shortcut for `doc.element(id)?.get_attribute_ns(ns, name)`.
    pub fn get_attribute_ns(
        &self,
        id: NodeId,
        namespace_uri: &str,
        local_name: &str,
    ) -> Option<&str> {
        self.element(id)?
            .get_attribute_ns(namespace_uri, local_name)
    }

    /// Get the parent of a node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.parent)
    }

    /// Get the children of a node.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut current = self.nodes.get(id.0).and_then(|n| n.first_child);
        let mut steps = 0usize;
        while let Some(child_id) = current {
            if child_id.0 >= self.nodes.len() || steps >= self.nodes.len() {
                break;
            }
            result.push(child_id);
            current = self.nodes.get(child_id.0).and_then(|n| n.next_sibling);
            steps += 1;
        }
        result
    }

    /// Count the children of a node in O(children) time without allocating.
    pub fn children_count(&self, id: NodeId) -> usize {
        self.children_iter(id).count()
    }

    /// Return a zero-allocation iterator over the children of a node.
    ///
    /// This is more efficient than [`children()`](Self::children) when you
    /// don't need the results as a `Vec`.
    ///
    /// # Example
    ///
    /// ```
    /// let doc = uppsala::parse("<r><a/><b/><c/></r>").unwrap();
    /// let root = doc.document_element().unwrap();
    /// let names: Vec<&str> = doc.children_iter(root)
    ///     .filter_map(|id| doc.element(id))
    ///     .map(|e| e.name.local_name.as_ref())
    ///     .collect();
    /// assert_eq!(names, vec!["a", "b", "c"]);
    /// ```
    pub fn children_iter(&self, id: NodeId) -> ChildrenIter<'_, 'a> {
        ChildrenIter {
            doc: self,
            next_back: self.nodes.get(id.0).and_then(|n| n.last_child),
            next: self.nodes.get(id.0).and_then(|n| n.first_child),
        }
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
        for child in self.children_iter(id) {
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
        for child in self.children_iter(id) {
            self.collect_elements_by_tag_name_ns(child, namespace_uri, local_name, results);
        }
    }

    /// Find the first direct child element matching a namespace URI and local name.
    ///
    /// Unlike [`get_elements_by_tag_name_ns`](Self::get_elements_by_tag_name_ns)
    /// which searches all descendants, this only looks at immediate children.
    ///
    /// # Example
    ///
    /// ```
    /// let xml = r#"<r xmlns:a="urn:a"><a:x/><a:y/><a:x/></r>"#;
    /// let doc = uppsala::parse(xml).unwrap();
    /// let root = doc.document_element().unwrap();
    /// let x = doc.first_child_element_by_name_ns(root, "urn:a", "x");
    /// assert!(x.is_some());
    /// let elem = doc.element(x.unwrap()).unwrap();
    /// assert_eq!(elem.name.local_name.as_ref(), "x");
    /// ```
    pub fn first_child_element_by_name_ns(
        &self,
        parent: NodeId,
        namespace_uri: &str,
        local_name: &str,
    ) -> Option<NodeId> {
        let mut child = self.nodes.get(parent.0).and_then(|n| n.first_child);
        while let Some(cid) = child {
            if let Some(elem) = self.element(cid) {
                if elem.matches_name_ns(namespace_uri, local_name) {
                    return Some(cid);
                }
            }
            child = self.nodes.get(cid.0).and_then(|n| n.next_sibling);
        }
        None
    }

    /// Find all direct child elements matching a namespace URI and local name.
    ///
    /// Unlike [`get_elements_by_tag_name_ns`](Self::get_elements_by_tag_name_ns)
    /// which searches all descendants, this only looks at immediate children.
    ///
    /// # Example
    ///
    /// ```
    /// let xml = r#"<r xmlns:a="urn:a"><a:x/><a:y/><a:x/></r>"#;
    /// let doc = uppsala::parse(xml).unwrap();
    /// let root = doc.document_element().unwrap();
    /// let xs = doc.child_elements_by_name_ns(root, "urn:a", "x");
    /// assert_eq!(xs.len(), 2);
    /// ```
    pub fn child_elements_by_name_ns(
        &self,
        parent: NodeId,
        namespace_uri: &str,
        local_name: &str,
    ) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut child = self.nodes.get(parent.0).and_then(|n| n.first_child);
        while let Some(cid) = child {
            if let Some(elem) = self.element(cid) {
                if elem.matches_name_ns(namespace_uri, local_name) {
                    result.push(cid);
                }
            }
            child = self.nodes.get(cid.0).and_then(|n| n.next_sibling);
        }
        result
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
                for child in self.children_iter(id) {
                    self.collect_text(child, buf);
                }
            }
        }
    }

    // ─── Tree mutation ───

    /// Append a child node to a parent. Detaches the child from any previous parent.
    ///
    /// The document root and virtual XPath attribute nodes are not part of the
    /// sibling-linked tree and cannot be appended; such calls are a no-op (as
    /// are self/ancestor cycles and invalid ids).
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        if !self.can_reparent(parent, child) {
            return;
        }
        // Detach from old parent first
        self.detach(child);
        self.append_child_unchecked(parent, child);
    }

    /// Append a freshly-allocated child node to a parent without detaching.
    /// The child must have no parent, no siblings. Used during parsing for speed.
    #[inline]
    pub(crate) fn append_child_unchecked(&mut self, parent: NodeId, child: NodeId) {
        self.invalidate_xpath_caches();
        debug_assert!(self.valid_node_id(parent));
        debug_assert!(self.valid_node_id(child));
        debug_assert!(!self.is_ancestor_of(child, parent));
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
        if new_child == reference
            || !self.can_reparent(parent, new_child)
            || self.parent(reference) != Some(parent)
            // A virtual attribute node passes the parent check (its `parent`
            // is the owner element) but is not in the child list; splicing
            // around its empty sibling links would corrupt the real children.
            || !self.is_linkable_node(reference)
        {
            return;
        }
        self.invalidate_xpath_caches();
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
        if new_child == reference
            || !self.can_reparent(parent, new_child)
            || self.parent(reference) != Some(parent)
            // See insert_before: an attribute node is not a real child.
            || !self.is_linkable_node(reference)
        {
            return;
        }
        self.invalidate_xpath_caches();
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
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        if self.parent(child) != Some(parent) {
            return;
        }
        self.detach(child);
    }

    /// Replace an old child with a new child under the given parent.
    pub fn replace_child(&mut self, parent: NodeId, new_child: NodeId, old_child: NodeId) {
        if new_child == old_child
            || !self.can_reparent(parent, new_child)
            || self.parent(old_child) != Some(parent)
            // See insert_before: an attribute node is not a real child, so it
            // cannot be replaced; splicing `new_child` in via its empty
            // sibling links would make it the parent's sole child.
            || !self.is_linkable_node(old_child)
        {
            return;
        }
        self.invalidate_xpath_caches();
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
    ///
    /// Virtual XPath attribute nodes are not part of the sibling-linked tree;
    /// detaching one is a no-op (remove the attribute from its owner element
    /// instead).
    pub fn detach(&mut self, id: NodeId) {
        // A virtual attribute node has `parent = Some(owner)` but sits in no
        // child list; the unlink logic below would interpret its empty sibling
        // links as "only child" and wipe the owner's real child list. There is
        // nothing to detach for such nodes (see `is_linkable_node`).
        if !self.is_linkable_node(id) {
            return;
        }
        let (parent_id, prev, next) = match self.nodes.get(id.0) {
            Some(n) => (n.parent, n.prev_sibling, n.next_sibling),
            None => return,
        };
        if let Some(parent_id) = parent_id {
            // Only an attached node changes the tree shape; a parentless node is
            // a no-op and leaves the caches valid.
            self.invalidate_xpath_caches();
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

    /// Deep-copy a node and its entire subtree from another document into this
    /// one, returning the new (detached) root node id.
    ///
    /// `NodeId`s are scoped to a single document's arena, so an element cannot be
    /// reparented across documents directly; moving a subtree between documents
    /// requires cloning it. This is the native equivalent of the per-node Python
    /// clone the etree layer used to do for cross-tree `append`/`deepcopy`, which
    /// dominated pyFF's aggregation step (see pyFF/performance.md): it recreates
    /// the whole subtree in one native pass instead of one FFI call per node.
    ///
    /// All copied strings (names, namespace declarations, attribute values, text)
    /// are taken by value as owned data, so the result is independent of `src`'s
    /// lifetime. The element's own namespace declarations are copied; namespaces
    /// inherited from ancestors *outside* the subtree are the caller's
    /// responsibility (mirroring the previous behaviour). Document and virtual
    /// attribute nodes cannot be imported and yield `None`.
    pub fn import_subtree<'b>(&mut self, src: &Document<'b>, src_id: NodeId) -> Option<NodeId> {
        // Only invalidate the XPath caches when the import actually mutates this
        // document. A rejected root (invalid `src_id`, or a `Document`/`Attribute`
        // node) returns `None` from `import_node_rec` before creating any node, so
        // a no-op import must not force an unnecessary `prepare_xpath()` rebuild.
        let new_id = self.import_node_rec(src, src_id)?;
        self.invalidate_xpath_caches();
        Some(new_id)
    }

    /// Replace the attached document tree with a deep copy of another document.
    ///
    /// The destination arena is kept alive. When both documents have a document
    /// element, the existing document element's [`NodeId`] is reused and its
    /// payload and children are replaced. This keeps root-level live views
    /// attached to the replacement tree. Other previously attached nodes are
    /// detached, not overwritten, so their ids cannot alias replacement nodes.
    ///
    /// XML declaration and DOCTYPE metadata are copied from `src`. Imported
    /// nodes contain owned strings and have no source byte ranges because those
    /// ranges belong to `src`'s input buffer.
    pub fn replace_tree_from<'b>(&mut self, src: &Document<'b>) {
        // Import first so the attached tree remains unchanged if a future
        // fallible import path is introduced. Document children can never be
        // virtual attribute nodes, so every valid child is importable.
        let mut replacements: Vec<NodeId> = src
            .children_iter(src.root())
            .filter_map(|child| self.import_subtree(src, child))
            .collect();

        // Preserve the existing document element id so bindings with a live
        // root proxy continue to observe the replacement. Descendant handles
        // are detached because matching arbitrary old and new subtrees by
        // position would make stale ids silently identify unrelated nodes.
        if let (Some(old_root), Some((new_root_index, new_root))) = (
            self.document_element(),
            replacements
                .iter()
                .copied()
                .enumerate()
                .find(|(_, id)| matches!(self.node_kind(*id), Some(NodeKind::Element(_)))),
        ) {
            let new_element = self
                .element(new_root)
                .expect("replacement document element must be an element")
                .clone();
            for child in self.children(old_root) {
                self.detach(child);
            }
            for child in self.children(new_root) {
                self.append_child(old_root, child);
            }
            *self
                .node_kind_mut(old_root)
                .expect("existing document element id must remain valid") =
                NodeKind::Element(new_element);
            // The imported payload does not refer to the destination's source
            // text; ensure source APIs report it as programmatically replaced.
            self.nodes[old_root.0].byte_pos = 0;
            self.nodes[old_root.0].byte_end_pos = 0;
            replacements[new_root_index] = old_root;
        }

        for child in self.children(self.root) {
            self.detach(child);
        }
        for child in replacements {
            self.append_child(self.root, child);
        }

        self.xml_declaration = src
            .xml_declaration
            .as_ref()
            .cloned()
            .map(XmlDeclaration::into_static);
        self.doctype = src
            .doctype
            .as_ref()
            .map(|doctype| Cow::Owned(doctype.to_string()));
    }

    /// Recursive worker for [`import_subtree`](Self::import_subtree). Creates the
    /// node for `src_id` in this document, then imports each child and appends it.
    fn import_node_rec<'b>(&mut self, src: &Document<'b>, src_id: NodeId) -> Option<NodeId> {
        let new_id = match src.node_kind(src_id)? {
            NodeKind::Element(e) => {
                let new_id = self.create_element(own_qname(&e.name));
                // Copy attributes and the element's own namespace declarations.
                // Building owned (`'static`) data that coerces into this
                // document's lifetime `'a`.
                let attributes: Vec<Attribute<'a>> = e
                    .attributes
                    .iter()
                    .map(|a| Attribute {
                        name: own_qname(&a.name),
                        value: Cow::Owned(a.value.to_string()),
                    })
                    .collect();
                let ns_decls: Vec<(Cow<'a, str>, Cow<'a, str>)> = e
                    .namespace_declarations
                    .iter()
                    .map(|(p, u)| (Cow::Owned(p.to_string()), Cow::Owned(u.to_string())))
                    .collect();
                // `create_element` always allocates an element node, so this is
                // an invariant rather than a recoverable case: a non-element here
                // would mean a silently partially-copied subtree. Fail loudly.
                let el = self
                    .element_mut(new_id)
                    .expect("create_element must allocate an element node");
                el.attributes = attributes;
                el.namespace_declarations = ns_decls;
                new_id
            }
            NodeKind::Text(t) => self.create_text(Cow::Owned(t.to_string())),
            NodeKind::CData(t) => self.create_cdata(Cow::Owned(t.to_string())),
            NodeKind::Comment(t) => self.create_comment(Cow::Owned(t.to_string())),
            NodeKind::ProcessingInstruction(pi) => self.create_processing_instruction(
                Cow::Owned(pi.target.to_string()),
                pi.data.as_ref().map(|d| Cow::Owned(d.to_string())),
            ),
            // The document root and virtual XPath attribute nodes are not part of
            // a movable subtree.
            NodeKind::Document | NodeKind::Attribute(_, _) => return None,
        };
        // Import children in order. `children_iter` walks `src`'s sibling links
        // without allocating a `Vec` per recursion level; iterating `src` while
        // mutating `self` is sound because they are distinct arenas (the iterator
        // borrows `src`, never `self`).
        for child in src.children_iter(src_id) {
            if let Some(child_new) = self.import_node_rec(src, child) {
                self.append_child_unchecked(new_id, child_new);
            }
        }
        Some(new_id)
    }

    fn valid_node_id(&self, id: NodeId) -> bool {
        id.0 < self.nodes.len()
    }

    /// True when `id` denotes a node that participates in the sibling-linked
    /// tree. The document root and virtual XPath attribute nodes do not: an
    /// attribute node carries `parent = Some(owner_element)` but is never in
    /// the owner's child list, so child-list surgery that trusts its (empty)
    /// sibling links would wipe the owner's real `first_child`/`last_child`.
    /// Every mutator that unlinks or splices around a node checks this first.
    fn is_linkable_node(&self, id: NodeId) -> bool {
        !matches!(
            self.node_kind(id),
            None | Some(NodeKind::Document) | Some(NodeKind::Attribute(_, _))
        )
    }

    fn can_reparent(&self, parent: NodeId, child: NodeId) -> bool {
        self.valid_node_id(parent)
            && self.valid_node_id(child)
            && parent != child
            && self.is_linkable_node(child)
            && !self.is_ancestor_of(child, parent)
    }

    fn is_ancestor_of(&self, maybe_ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        let mut steps = 0usize;
        while let Some(id) = current {
            if id == maybe_ancestor {
                return true;
            }
            if steps >= self.nodes.len() {
                return true;
            }
            current = self.nodes.get(id.0).and_then(|n| n.parent);
            steps += 1;
        }
        false
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
        // Iterative pre-order walk using the zero-allocation child iterator:
        // the previous recursive `collect_descendants` allocated a `Vec` per
        // node (children()), which dominated wide-tree traversal cost.
        let mut stack = Vec::new();
        for child in self.children_iter(id).rev() {
            stack.push(child);
        }
        while let Some(current) = stack.pop() {
            result.push(current);
            for child in self.children_iter(current).rev() {
                stack.push(child);
            }
        }
        result
    }

    // ─── Serialization ───

    /// Serialize the document back to an XML string (compact, no indentation).
    pub fn to_xml(&self) -> String {
        let mut output = String::with_capacity(self.serialized_size_hint(self.root));
        // write_document_to cannot fail when writing to String
        self.write_document_to(&mut output, &XmlWriteOptions::default())
            .unwrap();
        output
    }

    /// Serialize the document with formatting options.
    pub fn to_xml_with_options(&self, opts: &XmlWriteOptions) -> String {
        let mut output = String::with_capacity(self.serialized_size_hint(self.root));
        self.write_document_to(&mut output, opts).unwrap();
        output
    }

    /// Initial buffer capacity for serializing `id`: the length of its source
    /// range when it was parsed (serialized output is usually within a few
    /// percent of the input), or zero for a built node. Only a hint: a
    /// mutated tree may serialize longer, and `String` grows as usual then.
    /// Reserving up front avoids the doubling reallocations (and the page
    /// faults of each fresh larger block) that otherwise dominate the
    /// whole-document path on multi-megabyte inputs.
    fn serialized_size_hint(&self, id: NodeId) -> usize {
        if id == self.root {
            return self.input.len();
        }
        self.node_range(id).map(|r| r.len()).unwrap_or(0)
    }

    /// Serialize a single node (and its subtree) to an XML string.
    ///
    /// Useful for extracting XML fragments without the XML declaration or DOCTYPE.
    pub fn node_to_xml(&self, id: NodeId) -> String {
        let mut output = String::with_capacity(self.serialized_size_hint(id));
        let binds = self.ancestor_ns_bindings(id);
        let scope = NsScope {
            parent: None,
            local: &binds,
        };
        self.write_node_to(
            id,
            &mut output,
            &XmlWriteOptions::default(),
            0,
            false,
            &scope,
        )
        .unwrap();
        output
    }

    /// Serialize a single node (and its subtree) with formatting options.
    pub fn node_to_xml_with_options(&self, id: NodeId, opts: &XmlWriteOptions) -> String {
        let mut output = String::with_capacity(self.serialized_size_hint(id));
        self.write_node_to_with_options(id, &mut output, opts)
            .unwrap();
        output
    }

    /// Serialize a single node (and its subtree) into any `fmt::Write` sink
    /// with formatting options: the node-level counterpart of
    /// [`write_to_with_options`](Self::write_to_with_options). The sink
    /// receives exactly the text `node_to_xml_with_options` would return, so a
    /// caller that serializes repeatedly can reuse one buffer instead of
    /// paying for a fresh multi-megabyte allocation per call.
    ///
    /// Namespace bindings declared on `id`'s ancestors are treated as in
    /// scope (and so not re-declared), exactly as in `node_to_xml`. No XML
    /// declaration or DOCTYPE is written. An error from `out` is returned as
    /// soon as it occurs; the sink may then hold a partial document.
    ///
    /// # Examples
    ///
    /// ```
    /// use uppsala::{parse, XmlWriteOptions};
    ///
    /// let doc = parse("<r><a x=\"1\"/><b/></r>").unwrap();
    /// let root = doc.document_element().unwrap();
    /// let opts = XmlWriteOptions::compact();
    /// let mut buf = String::new();
    /// for child in doc.children(root) {
    ///     buf.clear();
    ///     buf.reserve(doc.node_serialized_size_hint(child));
    ///     doc.write_node_to_with_options(child, &mut buf, &opts).unwrap();
    ///     assert_eq!(buf, doc.node_to_xml_with_options(child, &opts));
    /// }
    /// ```
    pub fn write_node_to_with_options(
        &self,
        id: NodeId,
        out: &mut dyn fmt::Write,
        opts: &XmlWriteOptions,
    ) -> fmt::Result {
        let binds = self.ancestor_ns_bindings(id);
        let scope = NsScope {
            parent: None,
            local: &binds,
        };
        self.write_node_to(id, out, opts, 0, false, &scope)
    }

    /// Suggested buffer capacity for serializing `id`, for callers of
    /// [`write_node_to_with_options`](Self::write_node_to_with_options) that
    /// size their own sink: the length of the node's source range when it was
    /// parsed (the whole input length for the document node), or `0` for a
    /// node built programmatically. A hint only, never an upper bound.
    pub fn node_serialized_size_hint(&self, id: NodeId) -> usize {
        self.serialized_size_hint(id)
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
            out.write_str(&crate::writer::safe_xml_version(&decl.version))?;
            out.write_char('"')?;
            if let Some(enc) = &decl.encoding {
                out.write_str(" encoding=\"")?;
                out.write_str(&crate::writer::safe_xml_encoding(enc))?;
                out.write_char('"')?;
            }
            if let Some(sa) = decl.standalone {
                out.write_str(" standalone=\"")?;
                out.write_str(if sa { "yes" } else { "no" })?;
                out.write_char('"')?;
            }
            out.write_str("?>")?;
        }
        if opts.include_doctype {
            if let Some(dt) = &self.doctype {
                out.write_str(dt)?;
            }
        }
        let root_scope = NsScope {
            parent: None,
            local: &[],
        };
        for child in self.children(self.root) {
            self.write_node_to(child, out, opts, 0, opts.indent.is_some(), &root_scope)?;
        }
        Ok(())
    }

    /// Collect the namespace bindings in scope at `id` from its ancestors (not
    /// including `id` itself), outermost-first. Used to seed fragment
    /// serialization so a namespace already declared on an enclosing element is
    /// treated as in-scope and not redundantly re-declared.
    fn ancestor_ns_bindings(&self, id: NodeId) -> Vec<NsDecl<'_>> {
        let mut chain = Vec::new();
        let mut cur = self.parent(id);
        while let Some(p) = cur {
            chain.push(p);
            cur = self.parent(p);
        }
        chain.reverse();
        let mut binds = Vec::new();
        for nid in chain {
            if let Some(NodeKind::Element(e)) = self.node_kind(nid) {
                for (pfx, uri) in &e.namespace_declarations {
                    binds.push((Cow::Borrowed(pfx.as_ref()), Cow::Borrowed(uri.as_ref())));
                }
            }
        }
        binds
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
        scope: &NsScope,
    ) -> fmt::Result {
        match self.node_kind(id) {
            Some(NodeKind::Element(elem)) => {
                if indent_self {
                    write_indent(out, opts, depth)?;
                }
                out.write_char('<')?;
                // Namespace-aware serialization: alongside the element's stored
                // declarations, synthesize any declarations needed so its own
                // QName and its attributes' QNames resolve under the current
                // scope (issue #2). For parsed documents the stored declarations
                // already satisfy every QName, so nothing extra is emitted.
                let (elem_name_override, attr_overrides, child_local) =
                    plan_element_namespaces(elem, scope);
                // Emit the element name piecewise (prefix, ':', local) with the
                // same sanitization safe_xml_qname applied to the joined string,
                // so the common valid-name case allocates nothing. The override
                // path (planner-synthesized names) still writes the built string.
                match &elem_name_override {
                    Some(name) => out.write_str(&crate::writer::safe_xml_qname(name))?,
                    None => crate::writer::write_qname_sanitized(
                        out,
                        elem.name.prefix.as_deref(),
                        &elem.name.local_name,
                    )?,
                }
                // Track names already emitted for this start tag so sanitized
                // programmatic attributes cannot collide into duplicate XML.
                // Holds Cows: valid unique names (every parsed document) are
                // recorded as borrows, so the tracking allocates nothing.
                // Sized up front: one allocation per element instead of the
                // 4/8/16 growth steps a `push` loop would take.
                let mut seen_attrs: Vec<Cow<'_, str>> =
                    Vec::with_capacity(child_local.len() + elem.attributes.len());
                // Namespace declarations. `child_local` holds every binding this
                // element introduces (stored + synthesized) in order; emit only
                // the *last* binding per prefix so a synthesized override — e.g. an
                // `xmlns=""` undeclaration — wins over a conflicting stored
                // declaration instead of being dropped as a duplicate.
                // Sanitized prefixes can also collide (two distinct invalid
                // prefixes both collapse to `_`), which is disambiguated against
                // names already emitted for this start tag so output re-parses.
                //
                // Precompute the last index per prefix so the "last binding wins"
                // dedup is O(n) rather than O(n^2) in the number of bindings.
                // A handful of bindings (the norm) is cheaper to scan than to
                // hash; the map is only built past that.
                let last_idx: Option<HashMap<&str, usize>> = if child_local.len() > 8 {
                    let mut m = HashMap::with_capacity(child_local.len());
                    for (i, (prefix, _)) in child_local.iter().enumerate() {
                        m.insert(prefix.as_ref(), i);
                    }
                    Some(m)
                } else {
                    None
                };
                for (i, (prefix, uri)) in child_local.iter().enumerate() {
                    let shadowed = match &last_idx {
                        Some(m) => m.get(prefix.as_ref()) != Some(&i),
                        None => child_local[i + 1..].iter().any(|(p, _)| p == prefix),
                    };
                    if shadowed {
                        continue; // shadowed by a later binding for the same prefix
                    }
                    let (prefix, uri) = (prefix.as_ref(), uri.as_ref());
                    if prefix.is_empty() {
                        // A default-namespace declaration has the fixed name
                        // `xmlns`, which cannot be disambiguated; skip a
                        // duplicate rather than emit a malformed document.
                        if seen_attrs.iter().any(|name| name == "xmlns") {
                            continue;
                        }
                        seen_attrs.push(Cow::Borrowed("xmlns"));
                        out.write_str(" xmlns=\"")?;
                    } else {
                        let safe = crate::writer::safe_xml_ncname(prefix).into_owned();
                        let mut candidate = safe.clone();
                        // Build the full `xmlns:<candidate>` name once per suffix
                        // attempt and reuse it for the membership test and the
                        // final push, rather than re-`format!`ing it inside the
                        // predicate for every entry already in `seen_attrs`.
                        let mut full = format!("xmlns:{}", candidate);
                        let mut suffix = 1usize;
                        while seen_attrs.iter().any(|name| name.as_ref() == full) {
                            candidate = format!("{}_{}", safe, suffix);
                            full = format!("xmlns:{}", candidate);
                            suffix += 1;
                        }
                        seen_attrs.push(Cow::Owned(full));
                        out.write_str(" xmlns:")?;
                        out.write_str(&candidate)?;
                        out.write_str("=\"")?;
                    }
                    write_escaped_attr(out, uri)?;
                    out.write_char('"')?;
                }
                // Attributes. A namespaced attribute without a usable prefix
                // gets one via `attr_overrides` (see `plan_element_namespaces`).
                // The common case (valid unprefixed name, no override -- the
                // bulk of any parsed document) borrows straight from the
                // element and allocates nothing; only prefixed attributes pay
                // the `prefix:local` join, exactly as the old code did.
                for (attr, override_name) in elem.attributes.iter().zip(attr_overrides.iter()) {
                    out.write_char(' ')?;
                    match override_name {
                        Some(name) => {
                            let aname = crate::writer::unique_safe_xml_qname(name, &mut seen_attrs);
                            out.write_str(&aname)?;
                        }
                        None => match attr.name.prefix.as_deref() {
                            None => {
                                let aname = crate::writer::unique_safe_xml_qname(
                                    &attr.name.local_name,
                                    &mut seen_attrs,
                                );
                                out.write_str(&aname)?;
                            }
                            Some(_) => {
                                let joined = attr.name.prefixed_name().into_owned();
                                let aname = crate::writer::unique_safe_xml_qname_owned(
                                    joined,
                                    &mut seen_attrs,
                                );
                                out.write_str(&aname)?;
                            }
                        },
                    }
                    out.write_str("=\"")?;
                    write_escaped_attr(out, &attr.value)?;
                    out.write_char('"')?;
                }
                // Child scope: the inherited scope extended with the bindings this
                // element introduced (stored + synthesized).
                let child_scope = NsScope {
                    parent: Some(scope),
                    local: &child_local,
                };
                // Re-emit the element name for close tags with the same
                // override/piecewise logic as the open tag above (the name is
                // deterministic, so open and close always match).
                let write_close_name = |out: &mut dyn fmt::Write| -> fmt::Result {
                    match &elem_name_override {
                        Some(name) => out.write_str(&crate::writer::safe_xml_qname(name)),
                        None => crate::writer::write_qname_sanitized(
                            out,
                            elem.name.prefix.as_deref(),
                            &elem.name.local_name,
                        ),
                    }
                };
                // Walk children via the sibling links rather than materialising
                // a Vec per element per serialize.
                if self.first_child(id).is_none() {
                    if opts.expand_empty_elements {
                        out.write_str("></")?;
                        write_close_name(out)?;
                        out.write_char('>')?;
                    } else {
                        out.write_str("/>")?;
                    }
                } else {
                    out.write_char('>')?;
                    // Determine if this is "element-only" content for pretty-printing.
                    // If any child is text or CDATA, we treat it as mixed content
                    // and do NOT insert newlines/indent (to preserve whitespace
                    // semantics). Only probed when pretty-printing; the compact
                    // path never pays this extra sibling walk.
                    let element_only = opts.indent.is_some() && {
                        let mut only = true;
                        let mut c = self.first_child(id);
                        while let Some(cid) = c {
                            if matches!(
                                self.node_kind(cid),
                                Some(NodeKind::Text(_)) | Some(NodeKind::CData(_))
                            ) {
                                only = false;
                                break;
                            }
                            c = self.next_sibling(cid);
                        }
                        only
                    };
                    if element_only {
                        out.write_char('\n')?;
                    }
                    let mut child = self.first_child(id);
                    while let Some(cid) = child {
                        self.write_node_to(cid, out, opts, depth + 1, element_only, &child_scope)?;
                        child = self.next_sibling(cid);
                    }
                    if element_only {
                        write_indent(out, opts, depth)?;
                    }
                    out.write_str("</")?;
                    write_close_name(out)?;
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
                // F-15: split content containing `]]>` across adjacent
                // CDATA sections so attacker-crafted DOM nodes cannot
                // smuggle markup through the serializer.
                out.write_str("<![CDATA[")?;
                out.write_str(&crate::writer::split_cdata_content(text))?;
                out.write_str("]]>")?;
            }
            Some(NodeKind::Comment(text)) => {
                if indent_self {
                    write_indent(out, opts, depth)?;
                }
                // F-13: pad consecutive dashes so content cannot break
                // XML 1.0 comment well-formedness and terminate the
                // comment early.
                out.write_str("<!--")?;
                out.write_str(&crate::writer::sanitize_comment_content(text))?;
                out.write_str("-->")?;
                if indent_self {
                    out.write_char('\n')?;
                }
            }
            Some(NodeKind::ProcessingInstruction(pi)) => {
                if indent_self {
                    write_indent(out, opts, depth)?;
                }
                // F-14: rename a reserved `xml` target so the emitted PI
                // cannot collide with an XML declaration, and insert a
                // space between `?` and `>` in data so the PI cannot
                // terminate early.
                out.write_str("<?")?;
                out.write_str(&crate::writer::sanitize_pi_target(&pi.target))?;
                if let Some(data) = &pi.data {
                    out.write_char(' ')?;
                    out.write_str(&crate::writer::sanitize_pi_data(data))?;
                }
                out.write_str("?>")?;
                if indent_self {
                    out.write_char('\n')?;
                }
            }
            Some(NodeKind::Document) => {
                for child in self.children(id) {
                    self.write_node_to(child, out, opts, depth, indent_self, scope)?;
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

/// A `(prefix, namespace_uri)` binding; an empty prefix is the default namespace.
/// `Cow` so stored declarations can be borrowed from the element (no per-element
/// allocation), while synthesized bindings own their strings.
type NsDecl<'a> = (Cow<'a, str>, Cow<'a, str>);

/// In-scope namespace bindings during serialization, modeled as a borrowed
/// linked list of per-element frames so no cloning happens per element. Each
/// frame's `local` holds the declarations introduced by one element. The `xml`
/// prefix is always implicitly bound.
struct NsScope<'a> {
    parent: Option<&'a NsScope<'a>>,
    local: &'a [NsDecl<'a>],
}

impl<'a> NsScope<'a> {
    /// Resolve a prefix to its namespace URI in scope. Returns `Some("")` for an
    /// explicitly undeclared default namespace, `None` if the prefix is unbound.
    fn resolve(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(crate::namespace::XML_NAMESPACE);
        }
        let mut cur = Some(self);
        while let Some(s) = cur {
            for (p, u) in s.local.iter().rev() {
                if p.as_ref() == prefix {
                    return Some(u.as_ref());
                }
            }
            cur = s.parent;
        }
        None
    }

    /// Find a non-empty prefix currently bound to `uri`, respecting shadowing
    /// (the innermost binding for each prefix wins). The reserved `xml` and
    /// `xmlns` prefixes are never returned: reusing them for an arbitrary URI
    /// would defeat the "reserved prefixes are never rebound" guarantee and
    /// produce output that re-parses into the wrong namespace. Prefixes that are
    /// not valid XML NCNames are also skipped: reusing an invalid programmatic
    /// prefix would yield a QName like `bad prefix:Foo` that `safe_xml_qname`
    /// collapses to `_` (dropping the local name); the caller allocates a fresh
    /// `nsN` prefix instead.
    fn prefix_for(&self, uri: &str) -> Option<String> {
        // Track seen prefixes by reference (no cloning); the innermost binding
        // for each prefix is its effective one, so a prefix seen earlier shadows
        // any later (outer) binding.
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(self);
        while let Some(s) = cur {
            for (p, u) in s.local.iter().rev() {
                if seen.insert(p.as_ref())
                    && !p.is_empty()
                    && p.as_ref() != "xml"
                    && p.as_ref() != "xmlns"
                    && crate::writer::is_valid_xml_ncname(p.as_ref())
                    && u.as_ref() == uri
                {
                    return Some(p.as_ref().to_string());
                }
            }
            cur = s.parent;
        }
        None
    }
}

/// Allocate a fresh `nsN` prefix that is not currently bound in `scope` plus the
/// declarations collected so far for this element.
fn alloc_ns_prefix(scope: &NsScope, child_local: &[NsDecl]) -> String {
    let mut n = 0usize;
    loop {
        let cand = format!("ns{}", n);
        let taken = {
            let cur = NsScope {
                parent: Some(scope),
                local: child_local,
            };
            cur.resolve(&cand).is_some()
        };
        if !taken {
            return cand;
        }
        n += 1;
    }
}

/// Return an existing non-empty prefix bound to `uri`, or allocate a fresh one
/// and record a declaration for it in `child_local`. Used when a prefix is
/// required (namespaced attribute) or when the desired prefix is unusable.
fn prefix_for_or_alloc<'e>(
    scope: &NsScope,
    child_local: &mut Vec<NsDecl<'e>>,
    uri: &'e str,
) -> String {
    let reuse = {
        let cur = NsScope {
            parent: Some(scope),
            local: child_local,
        };
        cur.prefix_for(uri)
    };
    if let Some(p) = reuse {
        return p;
    }
    let p = alloc_ns_prefix(scope, child_local);
    child_local.push((Cow::Owned(p.clone()), Cow::Borrowed(uri)));
    p
}

/// Ensure `desired_prefix` (empty string = default namespace) resolves to `uri`
/// for this element, recording any declaration that must be emitted in
/// `child_local`. Returns `Some(prefix)` when the QName must be rewritten to use
/// a *different* prefix — because `desired_prefix` is already declared on this
/// same start tag bound to another URI and a start tag cannot carry two bindings
/// for one prefix — or `None` when `desired_prefix` works as-is.
fn ensure_binding<'e>(
    scope: &NsScope,
    child_local: &mut Vec<NsDecl<'e>>,
    desired_prefix: &'e str,
    uri: &'e str,
) -> Option<String> {
    let already_bound = {
        let cur = NsScope {
            parent: Some(scope),
            local: child_local,
        };
        cur.resolve(desired_prefix) == Some(uri)
    };
    if already_bound {
        return None; // already correctly bound (here or via an ancestor)
    }
    if !child_local
        .iter()
        .any(|(p, _)| p.as_ref() == desired_prefix)
    {
        // Not declared on this element yet: declare it here, shadowing any
        // ancestor binding. The QName keeps its own prefix.
        child_local.push((Cow::Borrowed(desired_prefix), Cow::Borrowed(uri)));
        return None;
    }
    // Same-element conflict: `desired_prefix` is already bound here to a different
    // URI and cannot be redeclared, so bind `uri` to a different prefix and
    // rewrite the QName to use it (otherwise the QName would silently resolve to
    // the colliding declaration).
    Some(prefix_for_or_alloc(scope, child_local, uri))
}

/// If a non-empty default namespace is in scope (inherited or declared on this
/// element), record an `xmlns=""` undeclaration so an unprefixed element name
/// emitted here is not captured by that default namespace on re-parse. Used for
/// elements in no namespace and for elements whose reserved/unrepresentable
/// prefix the planner strips down to a bare local name (ADR 0017).
fn undeclare_default_ns(scope: &NsScope, child_local: &mut Vec<NsDecl<'_>>) {
    let default_ns_set = {
        let cur = NsScope {
            parent: Some(scope),
            local: child_local,
        };
        cur.resolve("").is_some_and(|d| !d.is_empty())
    };
    if default_ns_set {
        child_local.push((Cow::Borrowed(""), Cow::Borrowed("")));
    }
}

/// Compute the namespace bindings a serialized element introduces so its own
/// QName and its attributes' QNames resolve correctly under the inherited
/// `scope`. Returns:
/// - `elem_name_override` — a replacement element tag name, set only when the
///   element's own prefix collides with a stored declaration or is the reserved
///   `xml` prefix used for a non-XML namespace,
/// - `attr_overrides` — a per-attribute display-name override, set only for a
///   namespaced attribute that needs a prefix it does not carry (or whose prefix
///   collides / is reserved), and
/// - `child_local` — every binding (stored + synthesized) this element
///   introduces, in order; the serializer emits the last binding per prefix and
///   uses it as the child scope.
///
/// For a parsed document the stored declarations already satisfy every QName, so
/// nothing is synthesized and output is byte-identical to before. The per-element
/// cost is building the small planning vectors (`child_local` borrows the stored
/// declarations rather than cloning them; `attr_overrides`); synthesized bindings
/// and renamed QNames allocate only when actually required.
fn plan_element_namespaces<'e>(
    elem: &'e Element,
    scope: &NsScope,
) -> (Option<String>, Vec<Option<String>>, Vec<NsDecl<'e>>) {
    let xml_ns = crate::namespace::XML_NAMESPACE;
    let xmlns_ns = crate::namespace::XMLNS_NAMESPACE;
    // Borrow the stored declarations, but drop the reserved bindings the parser's
    // `NamespaceResolver::declare` ignores (namespace.rs): the `xmlns` prefix can
    // never be declared, the `xml` prefix may only bind the XML namespace, the
    // XML namespace may only be bound to the `xml` prefix (in particular never
    // as the default namespace), and no prefix may bind the XMLNS namespace.
    // Emitting them (e.g. `xmlns:xmlns=...`) would produce
    // namespace-not-well-formed output.
    let mut child_local: Vec<NsDecl<'e>> = elem
        .namespace_declarations
        .iter()
        .filter(|(p, u)| {
            p.as_ref() != "xmlns"
                && !(p.as_ref() == "xml" && u.as_ref() != xml_ns)
                && !(u.as_ref() == xml_ns && p.as_ref() != "xml")
                && u.as_ref() != xmlns_ns
        })
        .map(|(p, u)| (Cow::Borrowed(p.as_ref()), Cow::Borrowed(u.as_ref())))
        .collect();

    // Element QName.
    let elem_name_override = match (
        elem.name.prefix.as_deref(),
        elem.name.namespace_uri.as_deref(),
    ) {
        (None, None) => {
            // Unprefixed element in no namespace: if a non-empty default
            // namespace is in scope it would otherwise capture this element, so
            // undeclare it with `xmlns=""`.
            undeclare_default_ns(scope, &mut child_local);
            None
        }
        // A reserved prefix with no namespace URI must still be stripped: `xml`
        // and `xmlns` are implicitly bound, so `xml:Foo` would re-parse into the
        // XML namespace and `xmlns:Foo` into the XMLNS namespace, silently
        // changing the element's namespace. Serialize the bare local name,
        // sanitized as an NCName: the parser may have accepted a multi-colon
        // name (`xmlns:xmlns:C` → local `xmlns:C`), and emitting that verbatim
        // would re-parse as a *new* `xmlns`-prefixed element, so serialization
        // would strip one layer per round instead of being a one-pass fixpoint.
        // The stripped name is unprefixed and in no namespace, so like the
        // `(None, None)` arm it must not be captured by an in-scope default
        // namespace (ADR 0017).
        (Some("xml"), None) | (Some("xmlns"), None) => {
            undeclare_default_ns(scope, &mut child_local);
            Some(crate::writer::safe_xml_ncname(&elem.name.local_name).into_owned())
        }
        (Some(_), None) => None, // prefixed but no URI: leave the name as-is
        // The XML namespace is bound to `xml` and only `xml`, and is never
        // declared. Any other prefix (or none) for that URI is rewritten to
        // `xml`; the `xml` prefix used for any other URI is reassigned, since it
        // is reserved and cannot be rebound.
        (Some("xml"), Some(u)) if u == xml_ns => None,
        (_, Some(u)) if u == xml_ns => Some(format!("xml:{}", elem.name.local_name)),
        // The XMLNS namespace cannot be bound to any prefix: the parser ignores
        // every binding to it, so a synthesized `xmlns:nsN="...2000/xmlns/"`
        // declaration would not be namespace-well-formed and would not re-parse.
        // The namespace is unrepresentable, so drop it and serialize the bare
        // local name (never emitting an `xmlns`/`xmlns:*` name). NCName-sanitize
        // it so a multi-colon local name cannot re-form a prefixed name on
        // re-parse (see the reserved-prefix arm above), and undeclare any
        // in-scope default namespace so the bare name is not captured by it
        // (ADR 0017).
        (_, Some(u)) if u == xmlns_ns => {
            undeclare_default_ns(scope, &mut child_local);
            Some(crate::writer::safe_xml_ncname(&elem.name.local_name).into_owned())
        }
        // The reserved `xml`/`xmlns` prefixes bound to any *other* (representable)
        // URI are rebound to a fresh non-reserved prefix; emitting them verbatim
        // would re-parse as the XML namespace / as a declaration.
        (Some("xml"), Some(u)) | (Some("xmlns"), Some(u)) => {
            let pfx = prefix_for_or_alloc(scope, &mut child_local, u);
            Some(format!("{}:{}", pfx, elem.name.local_name))
        }
        (Some(p), Some(u)) => ensure_binding(scope, &mut child_local, p, u)
            .map(|q| format!("{}:{}", q, elem.name.local_name)),
        (None, Some(u)) => ensure_binding(scope, &mut child_local, "", u)
            .map(|q| format!("{}:{}", q, elem.name.local_name)),
    };

    // Attributes. An empty prefix never works for an attribute (attributes are
    // not in the default namespace), so a namespaced-but-prefixless attribute is
    // always given a prefix.
    let mut attr_overrides = Vec::with_capacity(elem.attributes.len());
    for attr in &elem.attributes {
        let override_name = match (
            attr.name.prefix.as_deref(),
            attr.name.namespace_uri.as_deref(),
        ) {
            // A reserved prefix with no namespace URI is stripped: `xml`/`xmlns`
            // are implicitly bound, so `xml:foo` would re-parse into the XML
            // namespace and `xmlns:foo` would be read as a namespace declaration,
            // changing the attribute's effective namespace.
            (None, None) if attr.name.local_name.as_ref() == "xmlns" => Some("xmlns_".to_string()),
            (Some("xml"), None) | (Some("xmlns"), None) => {
                Some(if attr.name.local_name.as_ref() == "xmlns" {
                    "xmlns_".to_string()
                } else {
                    // NCName-sanitize so a multi-colon local name cannot re-form
                    // a prefixed attribute on re-parse (see the element arm).
                    crate::writer::safe_xml_ncname(&attr.name.local_name).into_owned()
                })
            }
            (_, None) => None,
            (Some("xml"), Some(u)) if u == xml_ns => None,
            (_, Some(u)) if u == xml_ns => Some(format!("xml:{}", attr.name.local_name)),
            // XMLNS namespace: unrepresentable (see the element-name planning
            // above), so drop it and serialize the bare local name rather than
            // emit a forbidden `xmlns:nsN="...2000/xmlns/"` declaration.
            (_, Some(u)) if u == xmlns_ns => Some(if attr.name.local_name.as_ref() == "xmlns" {
                "xmlns_".to_string()
            } else {
                crate::writer::safe_xml_ncname(&attr.name.local_name).into_owned()
            }),
            // Reserved `xml`/`xmlns` prefixes on a representable URI: rebind to a
            // fresh non-reserved prefix so the attribute does not masquerade as a
            // namespace declaration.
            (Some("xml"), Some(u)) | (Some("xmlns"), Some(u)) => {
                let pfx = prefix_for_or_alloc(scope, &mut child_local, u);
                Some(format!("{}:{}", pfx, attr.name.local_name))
            }
            (Some(p), Some(u)) => ensure_binding(scope, &mut child_local, p, u)
                .map(|q| format!("{}:{}", q, attr.name.local_name)),
            (None, Some(u)) => {
                let pfx = prefix_for_or_alloc(scope, &mut child_local, u);
                Some(format!("{}:{}", pfx, attr.name.local_name))
            }
        };
        attr_overrides.push(override_name);
    }

    (elem_name_override, attr_overrides, child_local)
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
    /// Include the raw DOCTYPE declaration when serializing.
    ///
    /// Disabled by default so parsed DTDs are not handed to downstream XML
    /// processors unless the caller deliberately opts into trusted DTD
    /// round-tripping.
    pub include_doctype: bool,
}

impl XmlWriteOptions {
    /// Compact output: no indentation, self-closing empty elements.
    pub fn compact() -> Self {
        XmlWriteOptions {
            indent: None,
            expand_empty_elements: false,
            include_doctype: false,
        }
    }

    /// Pretty-printed output with the given indentation string.
    pub fn pretty(indent: impl Into<String>) -> Self {
        XmlWriteOptions {
            indent: Some(indent.into()),
            expand_empty_elements: false,
            include_doctype: false,
        }
    }

    /// Set whether empty elements use expanded form (`<foo></foo>`).
    pub fn with_expand_empty_elements(mut self, expand: bool) -> Self {
        self.expand_empty_elements = expand;
        self
    }

    /// Set whether the parsed raw DOCTYPE declaration is serialized.
    pub fn with_doctype(mut self, include: bool) -> Self {
        self.include_doctype = include;
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
    // Run-based: bulk-copies unescaped runs instead of a virtual write_char
    // per character (see writer::write_escaped_run_dyn). Byte-identical
    // output to the previous per-character loop.
    crate::writer::write_escaped_run_dyn(out, s, false)
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
    // Run-based; see write_escaped_text above.
    crate::writer::write_escaped_run_dyn(out, s, true)
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

#[cfg(test)]
mod dom_tests {
    use super::*;
    use crate::parser::Parser;

    /// `prepare_xpath()` refreshes the document-order index after a mutation, so
    /// a node appended following an earlier `prepare_xpath()` gets a valid
    /// position once re-prepared (the cache is not a one-shot no-op).
    #[test]
    fn prepare_xpath_reindexes_after_mutation() {
        let mut doc = Parser::new().parse("<r><a/></r>").unwrap();
        doc.prepare_xpath();

        // Append a new element; before re-preparation it has no order entry
        // (the index Vec predates the node).
        let r = doc.first_child(doc.root).unwrap();
        let b = doc.create_element(QName::local("b"));
        doc.append_child(r, b);
        assert_eq!(doc.doc_order_at(b), u32::MAX, "new node not yet indexed");

        // Re-preparing must refresh the index so the new node is ordered.
        doc.prepare_xpath();
        assert_ne!(
            doc.doc_order_at(b),
            u32::MAX,
            "re-prepare did not reindex the appended node"
        );
    }

    /// Repeated mutate -> `prepare_xpath()` rounds must not grow the arena
    /// beyond the genuinely new nodes: the virtual attribute nodes of the
    /// previous generation are recycled through `attr_node_pool`, not
    /// orphaned. Without recycling, a mutate/query loop (pyFF's
    /// `set_entity_attributes` pattern) appends a full fresh attribute-node
    /// set per round and the arena grows quadratically until OOM.
    #[test]
    fn prepare_xpath_recycles_attribute_nodes() {
        let mut doc = Parser::new()
            .parse(r#"<r a="1" b="2"><c d="3"/></r>"#)
            .unwrap();
        doc.prepare_xpath();
        let baseline = doc.nodes.len();
        let r = doc.first_child(doc.root).unwrap();
        for i in 0..100 {
            // Each round: one real new element (arena +1), then re-prepare.
            // The three attribute nodes (a, b, d) must reuse their slots.
            let e = doc.create_element(QName::local("x"));
            doc.append_child(r, e);
            doc.prepare_xpath();
            assert_eq!(
                doc.nodes.len(),
                baseline + i + 1,
                "arena grew beyond the real nodes added (attribute nodes leaked)"
            );
        }
    }

    /// An attribute `NodeId` cached before an unrelated mutation must still
    /// denote the same attribute after re-preparation: elements whose
    /// attribute count is unchanged keep their exact slot set, so recycling
    /// cannot silently alias the handle to a different element's attribute
    /// (previously `<c public>`'s handle could end up reading `<r secret>`).
    #[test]
    fn prepare_xpath_keeps_attribute_node_ids_stable() {
        let mut doc = Parser::new()
            .parse(r#"<r secret="TOPSECRET"><c public="hello"/></r>"#)
            .unwrap();
        doc.prepare_xpath();
        let r = doc.first_child(doc.root()).unwrap();
        let c = doc.first_child(r).unwrap();
        let cached = doc.get_attribute_nodes(c)[0];

        // Unrelated mutation (no attribute changes on c), then re-prepare.
        let e = doc.create_element(QName::local("x"));
        doc.append_child(r, e);
        doc.prepare_xpath();

        assert_eq!(doc.get_attribute_nodes(c), &[cached]);
        match doc.node_kind(cached) {
            Some(NodeKind::Attribute(name, value)) => {
                assert_eq!(name.local_name, "public");
                assert_eq!(value, "hello");
            }
            other => panic!("expected attribute node, got {:?}", other),
        }

        // Changing c's attribute *count* invalidates its handles (documented);
        // the old slot is recycled, not leaked, and the new set is coherent.
        doc.element_mut(c)
            .unwrap()
            .set_attribute(QName::local("extra"), Cow::Borrowed("1"));
        doc.prepare_xpath();
        let ids = doc.get_attribute_nodes(c);
        assert_eq!(ids.len(), 2);
        for &id in ids {
            assert!(matches!(doc.node_kind(id), Some(NodeKind::Attribute(..))));
        }
    }

    /// Virtual attribute nodes carry `parent = Some(owner)` but live in no
    /// child list, so tree mutators must reject them: previously
    /// `append_child`/`detach`/`remove_child`/`insert_before`/`insert_after`/
    /// `replace_child` interpreted an attribute node's empty sibling links as
    /// "only child" and silently wiped the owner element's real child list
    /// (`<r a="1"><c b="2"/><tail/></r>` serialized as `<r a="1"/>` after
    /// `append_child(c, attr_of_r)`). All such calls are now no-ops.
    #[test]
    fn tree_mutators_reject_virtual_attribute_nodes() {
        let src = r#"<r a="1"><c b="2"/><tail/></r>"#;
        let mut doc = Parser::new().parse(src).unwrap();
        doc.prepare_xpath();
        let r = doc.first_child(doc.root()).unwrap();
        let c = doc.first_child(r).unwrap();
        let attr = doc.get_attribute_nodes(r)[0];
        let orphan = doc.create_element(QName::local("x"));

        doc.append_child(c, attr);
        doc.detach(attr);
        doc.remove_child(r, attr);
        doc.insert_before(r, orphan, attr);
        doc.insert_after(r, orphan, attr);
        doc.replace_child(r, orphan, attr);

        // The tree is untouched: both children of <r> survive, the attribute
        // node still belongs to its owner, and nothing was spliced in.
        assert_eq!(doc.to_xml(), src);
        assert_eq!(doc.children(r).len(), 2);
        assert_eq!(doc.parent(attr), Some(r));
        assert_eq!(doc.parent(orphan), None);
        assert!(matches!(
            doc.node_kind(attr),
            Some(NodeKind::Attribute(name, value))
                if name.local_name == "a" && value == "1"
        ));

        // The document root is equally non-reparentable.
        let root = doc.root();
        doc.append_child(c, root);
        assert_eq!(doc.parent(root), None);
        assert_eq!(doc.to_xml(), src);
    }

    /// `import_subtree` deep-copies an element subtree (name, namespace
    /// declarations, attributes, nested text/children) from one document into
    /// another, producing an independent, attachable node.
    #[test]
    fn import_subtree_deep_copies_across_documents() {
        let src = Parser::new()
            .parse(r#"<m:Root xmlns:m="urn:m"><m:Child a="1">hi<m:Leaf/></m:Child></m:Root>"#)
            .unwrap();
        let src_root = src.document_element().unwrap();
        let src_child = src.first_child(src_root).unwrap();

        let mut dst = Parser::new().parse("<dst/>").unwrap();
        let dst_root = dst.document_element().unwrap();

        let imported = dst
            .import_subtree(&src, src_child)
            .expect("element subtree should import");
        dst.append_child(dst_root, imported);

        // The cloned subtree serializes with its name, namespace, attribute and
        // descendants intact under the destination root.
        let out = dst.to_xml();
        assert!(
            out.contains(r#"<m:Child"#)
                && out.contains(r#"a="1""#)
                && out.contains("hi")
                && out.contains("<m:Leaf"),
            "unexpected import output: {out}"
        );

        // The copy is independent: mutating the destination subtree must not be
        // visible through the source document.
        if let Some(NodeKind::Element(e)) = dst.node_kind_mut(imported) {
            e.set_attribute(QName::local("a"), Cow::Borrowed("2"));
        }
        assert_eq!(
            src.get_attribute(src_child, "a"),
            Some("1"),
            "source must be untouched by destination mutation"
        );
    }

    #[test]
    fn replace_tree_from_preserves_root_id_and_detaches_old_descendants() {
        let mut dst = crate::parse("<old><child/></old>").unwrap().into_static();
        let old_root = dst.document_element().unwrap();
        let old_child = dst.children(old_root)[0];
        dst.prepare_xpath();

        let src =
            crate::parse(r#"<?xml version="1.0"?><!DOCTYPE new><new><value>42</value></new>"#)
                .unwrap();
        dst.replace_tree_from(&src);

        assert_eq!(dst.document_element(), Some(old_root));
        assert_eq!(dst.parent(old_root), Some(dst.root()));
        assert_eq!(dst.parent(old_child), None);
        assert_eq!(dst.element(old_root).unwrap().name.local_name, "new");
        assert_eq!(
            dst.to_xml(),
            r#"<?xml version="1.0"?><new><value>42</value></new>"#
        );
        assert_eq!(dst.doctype.as_deref(), Some("<!DOCTYPE new>"));

        assert_eq!(dst.node_range(old_root), None);

        // Replacement invalidates the old XPath caches and rebuilding them
        // indexes only the newly attached tree.
        dst.prepare_xpath();
        assert_eq!(dst.parent(old_root), Some(dst.root()));
    }

    /// Without a mutation, a second `prepare_xpath()` is a no-op: it does not
    /// rebuild the caches (no extra virtual attribute nodes are allocated),
    /// preserving the build-once cost of the document-order index.
    #[test]
    fn prepare_xpath_is_idempotent_without_mutation() {
        let mut doc = Parser::new().parse(r#"<r x="1"><a y="2"/></r>"#).unwrap();
        doc.prepare_xpath();
        let nodes_after_first = doc.nodes.len();

        // No mutation in between -> the second call must short-circuit.
        doc.prepare_xpath();
        assert_eq!(
            doc.nodes.len(),
            nodes_after_first,
            "redundant prepare_xpath rebuilt the caches (allocated more attribute nodes)"
        );
    }
}
