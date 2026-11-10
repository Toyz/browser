//! The page model: our "DOM".
//!
//! An arena of nodes addressed by stable [`NodeId`]s. The parser builds it, the
//! style system reads it, the renderer turns it into GPUI elements, and later
//! JS mutates it. GPUI never owns it — it's plain long-lived state.

use markup5ever::{LocalName, QualName, ns};
use slotmap::{SlotMap, new_key_type};
use url::Url;

pub use markup5ever;

new_key_type! {
    /// Stable handle to a node. Stays valid across mutations until the node is removed.
    pub struct NodeId;
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: QualName,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct ElementData {
    pub name: QualName,
    pub attrs: Vec<Attribute>,
    /// For `<template>`: the separate fragment holding its contents.
    pub template_contents: Option<NodeId>,
}

impl ElementData {
    pub fn local_name(&self) -> &LocalName {
        &self.name.local
    }

    pub fn is_html(&self) -> bool {
        self.name.ns == ns!(html)
    }

    /// Attribute lookup by local name, ignoring namespaced attributes (xlink:href etc).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name.ns == ns!() && &*a.name.local == name)
            .map(|a| a.value.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum NodeData {
    Document,
    /// A detached root, e.g. `<template>` contents.
    Fragment,
    Doctype { name: String },
    Element(ElementData),
    Text(String),
    Comment(String),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

impl Node {
    pub fn element(&self) -> Option<&ElementData> {
        match &self.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match &self.data {
            NodeData::Text(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    nodes: SlotMap<NodeId, Node>,
    root: NodeId,
    /// The document's URL (after redirects). Relative URLs resolve against [`Document::base_url`].
    pub url: Url,
    /// No (or an ancient) doctype: render with legacy "quirks mode" rules.
    pub quirks: bool,
    /// Parsed for a browser running scripts: `<noscript>` holds raw text
    /// and isn't shown.
    pub scripting: bool,
    /// For a document flattened for rendering (shadow trees in their
    /// hosts, slots holding their nodes): which tree each node is in.
    pub shadow: Option<std::sync::Arc<ShadowTrees>>,
}

/// The shadow trees of a flattened document. Selectors and stylesheets
/// stay inside their tree: a shadow tree's styles reach its own elements,
/// its host through `:host` and nodes slotted into it through `::slotted`;
/// the document's don't reach into shadow trees.
#[derive(Debug, Default, Clone)]
pub struct ShadowTrees {
    /// The host of the shadow tree each of its nodes is in (nodes absent
    /// are in the document's tree).
    pub tree: std::collections::HashMap<NodeId, NodeId>,
    /// Each host's own children (in the flattened document, a host holds
    /// its shadow tree and slots hold these).
    pub light: std::collections::HashMap<NodeId, Vec<NodeId>>,
    /// Constructed stylesheets (`adoptedStyleSheets`), each for a shadow
    /// tree (by host) or the document (`None`), after its own sheets.
    pub adopted: Vec<(Option<NodeId>, String)>,
}

impl ShadowTrees {
    /// The host of the shadow tree `n` is in (`None`: the document's).
    pub fn tree_of(&self, n: NodeId) -> Option<NodeId> {
        self.tree.get(&n).copied()
    }
}

impl Document {
    pub fn new(url: Url) -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(Node {
            parent: None,
            children: Vec::new(),
            data: NodeData::Document,
        });
        Self { nodes, root, url, quirks: false, scripting: false, shadow: None }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The host of the shadow tree `n` is in (`None`: the document's).
    pub fn tree_of(&self, n: NodeId) -> Option<NodeId> {
        self.shadow.as_ref().and_then(|s| s.tree_of(n))
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        self.nodes.get(id)?.element()
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match &mut self.nodes.get_mut(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// (Unknown ids, e.g. from another document, have no children.)
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map(|n| &n.children[..]).unwrap_or(&[])
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id)?.parent
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    // ---- construction & mutation ----

    pub fn create(&mut self, data: NodeData) -> NodeId {
        self.nodes.insert(Node {
            parent: None,
            children: Vec::new(),
            data,
        })
    }

    pub fn create_element(&mut self, name: QualName, attrs: Vec<Attribute>) -> NodeId {
        self.create(NodeData::Element(ElementData {
            name,
            attrs,
            template_contents: None,
        }))
    }

    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.create(NodeData::Text(text.into()))
    }

    /// Append `child` as the last child of `parent`, detaching it first.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    /// Insert `child` immediately before `sibling`, detaching it first.
    pub fn insert_before(&mut self, sibling: NodeId, child: NodeId) {
        self.detach(child);
        let parent = self.nodes[sibling]
            .parent
            .expect("insert_before: sibling has no parent");
        let ix = self.index_in_parent(parent, sibling);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.insert(ix, child);
    }

    /// Append text to `parent`, merging into a trailing text node if there is one.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(&last) = self.nodes[parent].children.last()
            && let NodeData::Text(existing) = &mut self.nodes[last].data
        {
            existing.push_str(text);
            return;
        }
        let node = self.create_text(text);
        self.append(parent, node);
    }

    /// Insert text before `sibling`, merging into a preceding text node if there is one.
    pub fn insert_text_before(&mut self, sibling: NodeId, text: &str) {
        let parent = self.nodes[sibling].parent.expect("sibling has no parent");
        let ix = self.index_in_parent(parent, sibling);
        if ix > 0 {
            let prev = self.nodes[parent].children[ix - 1];
            if let NodeData::Text(existing) = &mut self.nodes[prev].data {
                existing.push_str(text);
                return;
            }
        }
        let node = self.create_text(text);
        self.insert_before(sibling, node);
    }

    /// Remove `id` from its parent's children (the node stays alive in the arena).
    pub fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id].parent.take() {
            self.nodes[parent].children.retain(|&c| c != id);
        }
    }

    /// Move all of `from`'s children to the end of `to`.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        let children = std::mem::take(&mut self.nodes[from].children);
        for &child in &children {
            self.nodes[child].parent = Some(to);
        }
        self.nodes[to].children.extend(children);
    }

    fn index_in_parent(&self, parent: NodeId, child: NodeId) -> usize {
        self.nodes[parent]
            .children
            .iter()
            .position(|&c| c == child)
            .expect("child not found in parent")
    }

    // ---- queries ----

    /// Depth-first, document-order iterator over `id` and its descendants.
    /// Copy `id` (and everything under it) from `other` into this document,
    /// unattached.
    pub fn import(&mut self, other: &Document, id: NodeId) -> NodeId {
        let mut data = other.node(id).data.clone();
        // A template's contents live outside its children: copy them too
        // (the id is `other`'s, meaningless here).
        let contents = match &mut data {
            NodeData::Element(e) => e.template_contents.take(),
            _ => None,
        };
        let copy = self.create(data);
        if let Some(c) = contents {
            let c = self.import(other, c);
            if let Some(NodeData::Element(e)) = self.data_mut(copy) {
                e.template_contents = Some(c);
            }
        }
        for &child in other.children(id) {
            let c = self.import(other, child);
            self.append(copy, c);
        }
        copy
    }

    /// Deep-copy `id` within this document, unattached (template contents
    /// included, as cloning a template does).
    pub fn clone_subtree(&mut self, id: NodeId) -> NodeId {
        let mut data = self.node(id).data.clone();
        let contents = match &mut data {
            NodeData::Element(e) => e.template_contents.take(),
            _ => None,
        };
        let children = self.children(id).to_vec();
        let copy = self.create(data);
        if let Some(c) = contents {
            let c = self.clone_subtree(c);
            if let Some(NodeData::Element(e)) = self.data_mut(copy) {
                e.template_contents = Some(c);
            }
        }
        for child in children {
            let c = self.clone_subtree(child);
            self.append(copy, c);
        }
        copy
    }

    /// Give `parent` exactly these children (each now says `parent` is its
    /// parent; whatever listed them before still does). For rendering a
    /// flattened copy of the document (shadow trees, slots), not for
    /// editing one.
    pub fn set_children(&mut self, parent: NodeId, children: Vec<NodeId>) {
        for &c in &children {
            if let Some(n) = self.nodes.get_mut(c) {
                n.parent = Some(parent);
            }
        }
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children = children;
        }
    }

    /// Mutable access to a node's data (text content, attributes).
    pub fn data_mut(&mut self, id: NodeId) -> Option<&mut NodeData> {
        self.nodes.get_mut(id).map(|n| &mut n.data)
    }

    pub fn descendants(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        // (Nothing, for a node not in this document.)
        let mut stack = if self.nodes.contains_key(id) { vec![id] } else { Vec::new() };
        std::iter::from_fn(move || {
            let next = stack.pop()?;
            if let Some(node) = self.nodes.get(next) {
                stack.extend(node.children.iter().rev().copied());
            }
            Some(next)
        })
    }

    /// First HTML element with this local name, in document order.
    pub fn find_element(&self, name: &str) -> Option<NodeId> {
        self.descendants(self.root).find(|&id| {
            self.element(id)
                .is_some_and(|e| e.is_html() && &*e.name.local == name)
        })
    }

    /// Concatenated text of all descendant text nodes.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        for n in self.descendants(id) {
            if let NodeData::Text(t) = &self.nodes[n].data {
                out.push_str(t);
            }
        }
        out
    }

    pub fn body(&self) -> Option<NodeId> {
        self.find_element("body")
    }

    /// `<title>` text with whitespace collapsed.
    pub fn title(&self) -> Option<String> {
        let title = self.find_element("title")?;
        let text = self
            .text_content(title)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        (!text.is_empty()).then_some(text)
    }

    /// The URL relative references resolve against: `<base href>` if present, else the document URL.
    pub fn base_url(&self) -> Url {
        self.find_element("base")
            .and_then(|id| self.element(id)?.attr("href").map(str::to_owned))
            .and_then(|href| self.url.join(&href).ok())
            .unwrap_or_else(|| self.url.clone())
    }

    /// Resolve a (possibly relative) reference from the page.
    pub fn resolve_url(&self, href: &str) -> Option<Url> {
        self.base_url().join(href.trim()).ok()
    }
}

/// Convenience for building HTML element names in code and tests.
pub fn html_name(local: &str) -> QualName {
    QualName::new(None, ns!(html), LocalName::from(local))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::new(Url::parse("https://example.com/a/b.html").unwrap())
    }

    #[test]
    fn append_and_text_merging() {
        let mut d = doc();
        let p = d.create_element(html_name("p"), vec![]);
        d.append(d.root(), p);
        d.append_text(p, "Hello ");
        d.append_text(p, "world");
        assert_eq!(d.children(p).len(), 1);
        assert_eq!(d.text_content(p), "Hello world");
    }

    #[test]
    fn insert_before_and_reparent() {
        let mut d = doc();
        let root = d.root();
        let a = d.create_element(html_name("a"), vec![]);
        let b = d.create_element(html_name("b"), vec![]);
        d.append(root, b);
        d.insert_before(b, a);
        assert_eq!(d.children(root), &[a, b]);

        let x = d.create_text("x");
        d.append(a, x);
        d.reparent_children(a, b);
        assert!(d.children(a).is_empty());
        assert_eq!(d.parent(x), Some(b));
    }

    #[test]
    fn resolves_relative_urls() {
        let d = doc();
        assert_eq!(d.resolve_url("c.html").unwrap().as_str(), "https://example.com/a/c.html");
        assert_eq!(d.resolve_url("/x").unwrap().as_str(), "https://example.com/x");
    }
}
