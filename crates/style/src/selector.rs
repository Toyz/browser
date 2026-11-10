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

    fn parse_host(&self) -> bool {
        true
    }

    fn parse_slotted(&self) -> bool {
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

/// Interaction state that dynamic pseudo-classes match against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementState {
    /// The element under the pointer and all its ancestors (`:hover`).
    pub hovered: std::collections::HashSet<NodeId>,
    /// Same, while the mouse button is held (`:active`).
    pub active: std::collections::HashSet<NodeId>,
    pub focused: Option<NodeId>,
    /// The element the URL's `#fragment` points at (`:target`).
    pub target: Option<NodeId>,
    /// Checked checkboxes/radios/options (`:checked`), once the user has
    /// touched the page's form controls; until then the `checked` and
    /// `selected` attributes decide.
    pub checked: Option<std::sync::Arc<std::collections::HashSet<NodeId>>>,
}

/// An element of our document, as `selectors` sees it.
#[derive(Clone, Copy)]
pub struct ElementRef<'a> {
    pub doc: &'a Document,
    pub id: NodeId,
    pub state: &'a ElementState,
}

impl fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ElementRef({:?})", self.id)
    }
}

impl<'a> ElementRef<'a> {
    pub fn new(doc: &'a Document, id: NodeId, state: &'a ElementState) -> Self {
        Self { doc, id, state }
    }

    fn data(&self) -> &'a page::ElementData {
        self.doc.element(self.id).expect("ElementRef to a non-element")
    }

    fn is_form_control(&self) -> bool {
        matches!(&*self.data().name.local, "button" | "input" | "select" | "textarea" | "option" | "fieldset")
    }

    /// A text field the user can type in.
    fn is_editable(&self) -> bool {
        let e = self.data();
        let text_input = match &*e.name.local {
            "textarea" => true,
            "input" => matches!(
                e.attr("type").unwrap_or("text").to_ascii_lowercase().as_str(),
                "text" | "search" | "email" | "url" | "tel" | "password" | "number"
            ),
            _ => false,
        };
        text_input && e.attr("readonly").is_none() && e.attr("disabled").is_none()
    }

    fn element_at(&self, id: NodeId) -> Option<Self> {
        self.doc.element(id).map(|_| Self { doc: self.doc, id, state: self.state })
    }

    /// The host of the shadow tree this element is in (`None`: the document's).
    pub fn tree(&self) -> Option<NodeId> {
        self.doc.tree_of(self.id)
    }

    /// The slot this element is assigned to: in a flattened document, a
    /// slot holds its assigned nodes, which belong to another tree.
    pub fn slot(&self) -> Option<NodeId> {
        self.doc.shadow.as_ref()?;
        let p = self.doc.parent(self.id)?;
        let slot = self.doc.element(p).is_some_and(|e| e.is_html() && &*e.name.local == "slot");
        (slot && self.doc.tree_of(p) != self.tree()).then_some(p)
    }

    fn sibling_elements(&self) -> (&'a [NodeId], usize) {
        // A slotted element's siblings are its host's children, not just
        // the ones assigned to the same slot.
        let light = self.slot().and_then(|slot| {
            let host = self.doc.tree_of(slot)?;
            self.doc.shadow.as_ref()?.light.get(&host).map(Vec::as_slice)
        });
        let siblings = light.unwrap_or_else(|| {
            let parent = self.doc.parent(self.id).expect("sibling query on a detached node");
            self.doc.children(parent)
        });
        let ix = siblings.iter().position(|&c| c == self.id).unwrap_or(0);
        (siblings, ix)
    }
}

impl selectors::Element for ElementRef<'_> {
    type Impl = Simple;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.doc.node(self.id))
    }

    /// The parent in this element's own tree: none at the top of a shadow
    /// tree (its parent is the shadow root), the host for a slotted one.
    fn parent_element(&self) -> Option<Self> {
        let parent = self.doc.parent(self.id)?;
        if self.doc.shadow.is_some() {
            let tree = self.tree();
            if tree.is_some() && tree == Some(parent) {
                return None;
            }
            if self.slot().is_some() {
                return self.element_at(self.doc.tree_of(parent)?);
            }
        }
        self.element_at(parent)
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        let tree = self.tree();
        tree.is_some() && self.doc.parent(self.id) == tree
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        self.element_at(self.tree()?)
    }

    fn assigned_slot(&self) -> Option<Self> {
        self.element_at(self.slot()?)
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
            PseudoClass::Inert("hover") => self.state.hovered.contains(&self.id),
            PseudoClass::Inert("active") => self.state.active.contains(&self.id),
            PseudoClass::Inert("focus" | "focus-visible") => self.state.focused == Some(self.id),
            PseudoClass::Inert("focus-within") => {
                let mut node = self.state.focused;
                while let Some(n) = node {
                    if n == self.id {
                        return true;
                    }
                    node = self.doc.parent(n);
                }
                false
            }
            PseudoClass::Inert("target") => self.state.target == Some(self.id),
            PseudoClass::Inert("checked") => match &self.state.checked {
                Some(set) => set.contains(&self.id),
                None => {
                    let e = self.data();
                    match &*e.name.local {
                        "input" => e.attr("checked").is_some(),
                        "option" => e.attr("selected").is_some(),
                        _ => false,
                    }
                }
            },
            PseudoClass::Inert("disabled") => self.is_form_control() && self.data().attr("disabled").is_some(),
            PseudoClass::Inert("enabled") => self.is_form_control() && self.data().attr("disabled").is_none(),
            PseudoClass::Inert("required") => self.is_form_control() && self.data().attr("required").is_some(),
            PseudoClass::Inert("optional") => self.is_form_control() && self.data().attr("required").is_none(),
            PseudoClass::Inert("read-only") => !self.is_editable(),
            PseudoClass::Inert("read-write") => self.is_editable(),
            PseudoClass::Inert("placeholder-shown") => {
                let e = self.data();
                self.is_editable() && e.attr("placeholder").is_some() && e.attr("value").is_none_or(str::is_empty)
            }
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
        let e = self.data();
        e.is_html() && &*e.name.local == "slot"
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
