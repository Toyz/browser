//! Glue between the page model and Servo's `selectors` crate: our selector
//! vocabulary ([`Simple`]) and how an element of our tree answers matching
//! questions ([`ElementRef`]).

use std::fmt;

use cssparser::ToCss;
use page::markup5ever::{LocalName, Namespace, ns};
use page::{Document, NodeData, NodeId};
use precomputed_hash::PrecomputedHash;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::MatchingContext;
use selectors::matching::ElementSelectorFlags;
use cssparser::ParseError;
use selectors::parser::SelectorParseErrorKind;
use selectors::{OpaqueElement, SelectorImpl};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simple;

/// An interned identifier: local names, ids, classes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct Ident(pub LocalName);

impl<'a> From<&'a str> for Ident {
    fn from(s: &'a str) -> Self {
        Ident(LocalName::from(s))
    }
}

impl ToCss for Ident {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_identifier(&self.0, dest)
    }
}

impl PrecomputedHash for Ident {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttrValue(pub String);

impl<'a> From<&'a str> for AttrValue {
    fn from(s: &'a str) -> Self {
        AttrValue(s.to_owned())
    }
}

impl AsRef<str> for AttrValue {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for AttrValue {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

/// Pseudo-classes we parse. Only link-ish ones ever match: we have no
/// hover/focus state on page content (yet), and form state comes later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoClass {
    Link,
    AnyLink,
    Defined,
    /// Parsed so rules containing it stay valid, but never matches.
    Inert(&'static str),
}

const INERT_PSEUDO_CLASSES: &[&str] = &[
    "visited", "hover", "active", "focus", "focus-visible", "focus-within", "target",
    "target-within", "checked", "disabled", "enabled", "required", "optional", "read-only",
    "read-write", "placeholder-shown", "default", "indeterminate", "valid", "invalid",
    "in-range", "out-of-range", "user-valid", "user-invalid", "autofill", "fullscreen",
    "modal", "popover-open", "open", "closed", "playing", "paused", "picture-in-picture",
    "blank", "local-link", "scope", "current", "past", "future",
];

impl ToCss for PseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            PseudoClass::Link => ":link",
            PseudoClass::AnyLink => ":any-link",
            PseudoClass::Defined => ":defined",
            PseudoClass::Inert(name) => return write!(dest, ":{name}"),
        })
    }
}

impl selectors::parser::NonTSPseudoClass for PseudoClass {
    fn is_active_or_hover(&self) -> bool {
        matches!(self, PseudoClass::Inert("active" | "hover"))
    }

    fn is_user_action_state(&self) -> bool {
        matches!(self, PseudoClass::Inert("active" | "hover" | "focus"))
    }
}

/// Pseudo-elements parse but never match (no generated content yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PseudoElement(pub String);

impl ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        write!(dest, "::{}", self.0)
    }
}

impl selectors::parser::PseudoElement for PseudoElement {}

impl SelectorImpl for Simple {
    type ExtraMatchingData<'a> = ();
    type AttrValue = AttrValue;
    type Identifier = Ident;
    type LocalName = Ident;
    type NamespaceUrl = Namespace;
    type NamespacePrefix = Ident;
    type BorrowedNamespaceUrl = Namespace;
    type BorrowedLocalName = Ident;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoElement;
}

/// Parser configuration: which selector features we accept.
pub struct SelectorParser;

impl<'i> selectors::Parser<'i> for SelectorParser {
    type Impl = Simple;
    type Error = SelectorParseErrorKind;

    fn parse_is_and_where(&self) -> bool {
        true
    }

    fn parse_has(&self) -> bool {
        true
    }

    fn parse_nth_child_of(&self) -> bool {
        true
    }

    fn is_is_alias(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case("-webkit-any") || name.eq_ignore_ascii_case("-moz-any")
    }

    fn parse_non_ts_pseudo_class(&self, name: &str) -> Result<PseudoClass, ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "link" => Ok(PseudoClass::Link),
            "any-link" => Ok(PseudoClass::AnyLink),
            "defined" => Ok(PseudoClass::Defined),
            other => match INERT_PSEUDO_CLASSES.iter().find(|&&p| p == other) {
                Some(&p) => Ok(PseudoClass::Inert(p)),
                None => Err(ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement)),
            },
        }
    }

    fn parse_pseudo_element(&self, name: &str) -> Result<PseudoElement, ParseError<Self::Error>> {
        Ok(PseudoElement(name.to_ascii_lowercase()))
    }

    fn default_namespace(&self) -> Option<Namespace> {
        None
    }

    fn namespace_for_prefix(&self, prefix: &Ident) -> Option<Namespace> {
        match &*prefix.0 {
            "svg" => Some(ns!(svg)),
            "html" => Some(ns!(html)),
            _ => None,
        }
    }
}

/// An element of our document, as `selectors` sees it.
#[derive(Clone, Copy)]
pub struct ElementRef<'a> {
    pub doc: &'a Document,
    pub id: NodeId,
}

impl fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ElementRef({:?})", self.id)
    }
}

impl<'a> ElementRef<'a> {
    pub fn new(doc: &'a Document, id: NodeId) -> Self {
        Self { doc, id }
    }

    fn data(&self) -> &'a page::ElementData {
        self.doc.element(self.id).expect("ElementRef to a non-element")
    }

    fn element_at(&self, id: NodeId) -> Option<Self> {
        self.doc.element(id).map(|_| Self { doc: self.doc, id })
    }

    fn sibling_elements(&self) -> (&'a [NodeId], usize) {
        let parent = self.doc.parent(self.id).expect("sibling query on a detached node");
        let siblings = self.doc.children(parent);
        let ix = siblings.iter().position(|&c| c == self.id).unwrap();
        (siblings, ix)
    }
}

impl selectors::Element for ElementRef<'_> {
    type Impl = Simple;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.doc.node(self.id))
    }

    fn parent_element(&self) -> Option<Self> {
        self.element_at(self.doc.parent(self.id)?)
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let (siblings, ix) = self.sibling_elements();
        siblings[..ix].iter().rev().find_map(|&id| self.element_at(id))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let (siblings, ix) = self.sibling_elements();
        siblings[ix + 1..].iter().find_map(|&id| self.element_at(id))
    }

    fn first_element_child(&self) -> Option<Self> {
        self.doc.children(self.id).iter().find_map(|&id| self.element_at(id))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.data().is_html()
    }

    fn has_local_name(&self, name: &Ident) -> bool {
        self.data().name.local == name.0
    }

    fn has_namespace(&self, ns: &Namespace) -> bool {
        &self.data().name.ns == ns
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.data().name == other.data().name
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        local_name: &Ident,
        operation: &AttrSelectorOperation<&AttrValue>,
    ) -> bool {
        self.data().attrs.iter().any(|attr| {
            let ns_ok = match ns {
                NamespaceConstraint::Any => true,
                NamespaceConstraint::Specific(ns) => attr.name.ns == **ns,
            };
            ns_ok && attr.name.local == local_name.0 && operation.eval_str(&attr.value)
        })
    }

    fn match_non_ts_pseudo_class(&self, pc: &PseudoClass, _cx: &mut MatchingContext<Simple>) -> bool {
        match pc {
            PseudoClass::Link | PseudoClass::AnyLink => self.is_link(),
            PseudoClass::Defined => true,
            PseudoClass::Inert(_) => false,
        }
    }

    fn match_pseudo_element(&self, _pe: &PseudoElement, _cx: &mut MatchingContext<Simple>) -> bool {
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        let e = self.data();
        e.is_html() && matches!(&*e.name.local, "a" | "area") && e.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &Ident, case: CaseSensitivity) -> bool {
        self.data()
            .attr("id")
            .is_some_and(|v| case.eq(v.as_bytes(), id.0.as_bytes()))
    }

    fn has_class(&self, name: &Ident, case: CaseSensitivity) -> bool {
        self.data().attr("class").is_some_and(|classes| {
            classes
                .split_ascii_whitespace()
                .any(|c| case.eq(c.as_bytes(), name.0.as_bytes()))
        })
    }

    fn has_custom_state(&self, _name: &Ident) -> bool {
        false
    }

    fn imported_part(&self, _name: &Ident) -> Option<Ident> {
        None
    }

    fn is_part(&self, _name: &Ident) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.doc.children(self.id).iter().all(|&c| match &self.doc.node(c).data {
            NodeData::Element(_) => false,
            NodeData::Text(t) => t.is_empty(),
            _ => true,
        })
    }

    fn is_root(&self) -> bool {
        self.doc
            .parent(self.id)
            .is_some_and(|p| matches!(self.doc.node(p).data, NodeData::Document))
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}
