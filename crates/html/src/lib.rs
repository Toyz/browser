//! HTML parsing straight into the page model, via html5ever's `TreeSink`.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};

use html5ever::interface::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink as _};
use html5ever::{ParseOpts, QualName, parse_document, parse_fragment as parse_html_fragment};
use page::{Attribute, Document, NodeData, NodeId};
use url::Url;

/// Parse a full HTML document.
///
/// Scripting is reported as *disabled*, so `<noscript>` content is parsed as
/// real markup and rendered — right for a browser that doesn't run JS (yet).
pub fn parse(html: &str, url: Url) -> Document {
    parse_scripted(html, url, false)
}

/// Parse a full HTML document; with `scripting`, `<noscript>` is inert text
/// (as in a browser that runs JavaScript).
pub fn parse_scripted(html: &str, url: Url, scripting: bool) -> Document {
    let mut opts = ParseOpts::default();
    opts.tree_builder.scripting_enabled = scripting;
    let mut doc = Document::new(url);
    doc.scripting = scripting;
    let sink = Sink { doc: RefCell::new(doc) };
    parse_document(sink, opts).one(html)
}

struct Sink {
    doc: RefCell<Document>,
}

fn convert_attrs(attrs: Vec<html5ever::Attribute>) -> Vec<Attribute> {
    attrs
        .into_iter()
        .map(|a| Attribute {
            name: a.name,
            value: a.value.to_string(),
        })
        .collect()
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Document;
    type ElemName<'a> = Ref<'a, QualName>;

    fn finish(self) -> Document {
        self.doc.into_inner()
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {
        // Real-world HTML is full of these; nothing useful to do with them yet.
    }

    fn get_document(&self) -> NodeId {
        self.doc.borrow().root()
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Ref<'a, QualName> {
        Ref::map(self.doc.borrow(), |d| {
            &d.element(*target).expect("elem_name on non-element").name
        })
    }

    fn create_element(&self, name: QualName, attrs: Vec<html5ever::Attribute>, flags: ElementFlags) -> NodeId {
        let mut doc = self.doc.borrow_mut();
        let id = doc.create_element(name, convert_attrs(attrs));
        if flags.template {
            let contents = doc.create(NodeData::Fragment);
            doc.element_mut(id).unwrap().template_contents = Some(contents);
        }
        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.doc.borrow_mut().create(NodeData::Comment(text.to_string()))
    }

    fn create_pi(&self, _target: StrTendril, data: StrTendril) -> NodeId {
        // Processing instructions only occur in XML; keep as a comment.
        self.doc.borrow_mut().create(NodeData::Comment(data.to_string()))
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match child {
            NodeOrText::AppendNode(node) => doc.append(*parent, node),
            NodeOrText::AppendText(text) => doc.append_text(*parent, &text),
        }
    }

    fn append_based_on_parent_node(&self, element: &NodeId, prev_element: &NodeId, child: NodeOrText<NodeId>) {
        let has_parent = self.doc.borrow().parent(*element).is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, name: StrTendril, _public_id: StrTendril, _system_id: StrTendril) {
        let mut doc = self.doc.borrow_mut();
        let node = doc.create(NodeData::Doctype { name: name.to_string() });
        let root = doc.root();
        doc.append(root, node);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.doc
            .borrow()
            .element(*target)
            .and_then(|e| e.template_contents)
            .expect("get_template_contents on non-template")
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.doc.borrow_mut().quirks = mode == QuirksMode::Quirks;
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match new_node {
            NodeOrText::AppendNode(node) => doc.insert_before(*sibling, node),
            NodeOrText::AppendText(text) => doc.insert_text_before(*sibling, &text),
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<html5ever::Attribute>) {
        let mut doc = self.doc.borrow_mut();
        let Some(element) = doc.element_mut(*target) else {
            return;
        };
        for attr in convert_attrs(attrs) {
            if !element.attrs.iter().any(|a| a.name == attr.name) {
                element.attrs.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.doc.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        self.doc.borrow_mut().reparent_children(*node, *new_parent);
    }
}

/// Parse `html` as the contents of a `<context>` element (`innerHTML`): a
/// document whose root's first child holds the parsed nodes.
pub fn parse_fragment(html: &str, context: &str, url: Url) -> Document {
    let mut opts = ParseOpts::default();
    opts.tree_builder.scripting_enabled = true;
    let sink = Sink {
        doc: RefCell::new(Document::new(url)),
    };
    let name = QualName::new(None, html5ever::ns!(html), html5ever::LocalName::from(context));
    parse_html_fragment(sink, opts, name, Vec::new(), true).one(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://example.com/").unwrap()
    }

    #[test]
    fn builds_document_structure() {
        let doc = parse("<title> Hi  there </title><p>One <b>two</b></p>", url());
        assert_eq!(doc.title().as_deref(), Some("Hi there"));
        let body = doc.body().unwrap();
        let p = doc.children(body)[0];
        assert_eq!(&*doc.element(p).unwrap().name.local, "p");
        assert_eq!(doc.text_content(p), "One two");
    }

    #[test]
    fn recovers_from_bad_markup() {
        // Misnested tags trigger the adoption agency algorithm (reparenting, insert_before).
        let doc = parse("<p><b>bold <i>both</b> italic</i></p><table><tr>x<td>cell</td></table>", url());
        let body = doc.body().unwrap();
        let text = doc.text_content(body);
        assert!(text.contains("bold both italic"), "{text}");
        // Foster parenting moves the stray "x" in front of the table.
        assert!(text.find('x').unwrap() < text.find("cell").unwrap());
    }

    #[test]
    fn noscript_is_markup_and_attrs_work() {
        let doc = parse(r#"<noscript><a href="/x">no js</a></noscript>"#, url());
        let a = doc.find_element("a").expect("noscript content parsed as elements");
        assert_eq!(doc.element(a).unwrap().attr("href"), Some("/x"));
    }

    #[test]
    fn detects_quirks_mode() {
        assert!(parse("<html><body>no doctype", url()).quirks);
        assert!(!parse("<!DOCTYPE html><html><body>standards", url()).quirks);
    }

    #[test]
    fn template_contents_are_separate() {
        let doc = parse("<template><p>hidden</p></template>", url());
        let t = doc.find_element("template").unwrap();
        assert!(doc.children(t).is_empty());
        let contents = doc.element(t).unwrap().template_contents.unwrap();
        assert_eq!(doc.text_content(contents), "hidden");
    }
}
