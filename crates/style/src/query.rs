//! Selector matching outside the cascade: `querySelector`, `matches`,
//! `closest` for scripts.

use page::{Document, NodeId};
use selectors::context::{MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode, SelectorCaches};
use selectors::matching::matches_selector_list;
use selectors::parser::{ParseRelative, SelectorList};

use crate::selector::{ElementRef, ElementState, SelectorParser, Simple};

/// A parsed selector list (`"a.x, #y > p"`).
pub struct Selectors(SelectorList<Simple>);

impl Selectors {
    pub fn parse(text: &str) -> Option<Selectors> {
        let mut parser = cssparser::Parser::new(text);
        SelectorList::parse(&SelectorParser, &mut parser, ParseRelative::No).ok().map(Selectors)
    }

    pub fn matches(&self, doc: &Document, id: NodeId) -> bool {
        if doc.element(id).is_none() {
            return false;
        }
        let state = ElementState::default();
        let mut caches = SelectorCaches::default();
        let mut cx = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        matches_selector_list(&self.0, ElementRef::new(doc, id, &state), &mut cx)
    }

    /// Matching descendants of `root`, in document order.
    pub fn query_all(&self, doc: &Document, root: NodeId) -> Vec<NodeId> {
        doc.descendants(root).filter(|&n| n != root && self.matches(doc, n)).collect()
    }
}
