//! The cascade: find matching rules, order them, apply declarations, compute.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use page::markup5ever::LocalName;
use page::{Document, NodeId};
use precomputed_hash::PrecomputedHash;
use selectors::bloom::BloomFilter;
use selectors::context::{MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode, SelectorCaches};
use selectors::matching::matches_selector;
use selectors::parser::{AncestorHashes, Component};
use url::Url;

use crate::media::{self, MediaEnv};
use crate::props::{Decl, FontSizeSpec, GridCols, LineHeightSpec, WeightSpec, Wide, parse_declaration};
use crate::selector::{ElementRef, ElementState};
use crate::sheet::{StyleRule, Stylesheet, parse_style_attribute, parse_stylesheet};
use crate::values::*;
use crate::{ComputedStyle, CustomProps};

const UA_CSS: &str = include_str!("ua.css");
/// Extra UA rules for quirks-mode documents (HTML spec, rendering § quirks).
const QUIRKS_CSS: &str = "
table {
  font-weight: initial; font-style: initial; font-size: initial;
  line-height: initial; white-space: initial; text-align: initial;
}
";

/// Limits so a hostile page can't make us fetch forever.
const MAX_EXTERNAL_SHEETS: usize = 32;
const MAX_IMPORT_DEPTH: usize = 3;

/// A stylesheet referenced by the document, in document order.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleSource {
    Inline { css: String, media: String },
    External { url: Url, media: String },
}

/// Find `<style>` and `<link rel=stylesheet>` in document order.
pub fn collect_style_sources(doc: &Document) -> Vec<StyleSource> {
    let mut out = Vec::new();
    for id in doc.descendants(doc.root()) {
        let Some(e) = doc.element(id) else { continue };
        if !e.is_html() {
            continue;
        }
        let media = e.attr("media").unwrap_or("").to_owned();
        match &*e.name.local {
            "style" => out.push(StyleSource::Inline { css: doc.text_content(id), media }),
            "link" => {
                let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
                let is_sheet = rel.split_ascii_whitespace().any(|r| r == "stylesheet");
                let alternate = rel.split_ascii_whitespace().any(|r| r == "alternate");
                if is_sheet && !alternate && e.attr("disabled").is_none()
                    && let Some(url) = e.attr("href").and_then(|h| doc.resolve_url(h))
                {
                    out.push(StyleSource::External { url, media });
                }
            }
            _ => {}
        }
    }
    out
}

/// One selector of one rule, filed under its rightmost compound's most specific key.
struct Entry {
    rule: u32,
    selector: u32,
    specificity: u32,
    /// What the selector needs from ancestors, for bloom-filter fast rejection.
    hashes: AncestorHashes,
}

#[derive(Default)]
struct RuleIndex {
    rules: Arc<Vec<StyleRule>>,
    by_id: HashMap<LocalName, Vec<Entry>>,
    by_class: HashMap<LocalName, Vec<Entry>>,
    by_tag: HashMap<LocalName, Vec<Entry>>,
    universal: Vec<Entry>,
}

impl RuleIndex {
    /// Index the selectors that target `pseudo` (`None`: the element itself;
    /// `Some("before")`: its `::before`, etc).
    fn new(rules: Arc<Vec<StyleRule>>, pseudo: Option<&str>) -> Self {
        let mut index = RuleIndex::default();
        for (rule_ix, rule) in rules.iter().enumerate() {
            for (sel_ix, selector) in rule.selectors.slice().iter().enumerate() {
                if selector.pseudo_element().map(|pe| pe.0.as_str()) != pseudo {
                    continue;
                }
                let entry = Entry {
                    rule: rule_ix as u32,
                    selector: sel_ix as u32,
                    specificity: selector.specificity(),
                    hashes: AncestorHashes::new(selector, QuirksMode::NoQuirks),
                };
                let (mut id, mut class, mut tag) = (None, None, None);
                let mut iter = selector.iter();
                if pseudo.is_some() {
                    // The rightmost compound is just `::before`; key on the element's compound.
                    for _ in &mut iter {}
                    iter.next_sequence();
                }
                for component in iter {
                    match component {
                        Component::ID(i) => id = Some(i.0.clone()),
                        Component::Class(c) => class = Some(c.0.clone()),
                        Component::LocalName(l) => tag = Some(l.lower_name.0.clone()),
                        _ => {}
                    }
                }
                match (id, class, tag) {
                    (Some(id), _, _) => index.by_id.entry(id).or_default().push(entry),
                    (_, Some(class), _) => index.by_class.entry(class).or_default().push(entry),
                    (_, _, Some(tag)) => index.by_tag.entry(tag).or_default().push(entry),
                    _ => index.universal.push(entry),
                }
            }
        }
        index.rules = rules;
        index
    }

    fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_class.is_empty() && self.by_tag.is_empty() && self.universal.is_empty()
    }

    fn collect(
        &self,
        el: ElementRef<'_>,
        mode: MatchingMode,
        origin: u8,
        bloom: &BloomFilter,
        caches: &mut SelectorCaches,
        out: &mut Vec<Matched>,
    ) {
        let e = el.doc.element(el.id).unwrap();
        let mut candidates: Vec<&[Entry]> = Vec::with_capacity(4);
        if let Some(id) = e.attr("id")
            && let Some(v) = self.by_id.get(&LocalName::from(id)) {
                candidates.push(v);
            }
        if let Some(classes) = e.attr("class") {
            for class in classes.split_ascii_whitespace() {
                if let Some(v) = self.by_class.get(&LocalName::from(class)) {
                    candidates.push(v);
                }
            }
        }
        if let Some(v) = self.by_tag.get(&e.name.local) {
            candidates.push(v);
        }
        candidates.push(&self.universal);

        let start = out.len();
        for entry in candidates.into_iter().flatten() {
            let rule = &self.rules[entry.rule as usize];
            let selector = &rule.selectors.slice()[entry.selector as usize];
            let mut cx = MatchingContext::new(
                mode,
                Some(bloom),
                caches,
                QuirksMode::NoQuirks,
                NeedsSelectorFlags::No,
                MatchingForInvalidation::No,
            );
            if matches_selector(selector, 0, Some(&entry.hashes), el, &mut cx) {
                out.push(Matched {
                    origin,
                    specificity: entry.specificity,
                    order: entry.rule,
                    normal: rule.normal.clone(),
                    important: rule.important.clone(),
                });
            }
        }
        // A rule matched through several of its selectors applies once, at its highest specificity.
        let matched = &mut out[start..];
        matched.sort_by_key(|m| (m.order, std::cmp::Reverse(m.specificity)));
        let mut seen = std::collections::HashSet::new();
        let mut keep = Vec::new();
        for m in out.drain(start..) {
            if seen.insert(m.order) {
                keep.push(m);
            }
        }
        out.extend(keep);
    }
}

struct Matched {
    origin: u8,
    specificity: u32,
    order: u32,
    normal: Arc<[Decl]>,
    important: Arc<[Decl]>,
}

const ORIGIN_UA: u8 = 0;
const ORIGIN_HINTS: u8 = 1;
const ORIGIN_AUTHOR: u8 = 2;
const ORIGIN_INLINE: u8 = 3;

/// Rule indexes for one origin: the element itself, `::before`, `::after`.
struct Indexes {
    element: RuleIndex,
    before: RuleIndex,
    after: RuleIndex,
}

impl Indexes {
    fn new(rules: Vec<StyleRule>) -> Self {
        let rules = Arc::new(rules);
        Self {
            element: RuleIndex::new(rules.clone(), None),
            before: RuleIndex::new(rules.clone(), Some("before")),
            after: RuleIndex::new(rules, Some("after")),
        }
    }

    fn pseudo(&self, which: Pseudo) -> &RuleIndex {
        match which {
            Pseudo::Before => &self.before,
            Pseudo::After => &self.after,
        }
    }
}

/// Generated-content pseudo-elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pseudo {
    Before,
    After,
}

fn ua_indexes(quirks: bool) -> &'static Indexes {
    static UA: OnceLock<Indexes> = OnceLock::new();
    static UA_QUIRKS: OnceLock<Indexes> = OnceLock::new();
    if quirks {
        UA_QUIRKS.get_or_init(|| {
            let mut rules = parse_stylesheet(UA_CSS).rules;
            rules.extend(parse_stylesheet(QUIRKS_CSS).rules);
            Indexes::new(rules)
        })
    } else {
        UA.get_or_init(|| Indexes::new(parse_stylesheet(UA_CSS).rules))
    }
}

/// A parsed `style` attribute: (normal, !important) declarations.
type ParsedAttr = (Vec<Decl>, Vec<Decl>);
/// (property, value after var() substitution).
type VarKey = (Arc<str>, String);

/// Computes styles for one document. Built once per page load.
/// Everything parsed from a document's stylesheets, independent of the
/// viewport. Cheap to clone and `Send`, so a page keeps it to restyle when the
/// window is resized (media queries can flip).
#[derive(Clone)]
pub struct StyleData {
    rules: Arc<Vec<StyleRule>>,
    /// The distinct media conditions rules depend on.
    conditions: Arc<Vec<Arc<str>>>,
    base: Option<Url>,
    /// Some rule uses `:hover`/`:active`/`:focus`…: interaction can restyle.
    has_state_rules: bool,
    /// ...and some puts it before a combinator or inside `:has()`.
    state_reaches_others: bool,
}

const STATE_PSEUDOS: [&str; 5] = [":hover", ":active", ":focus", ":target", ":checked"];

/// Does a state pseudo-class in this (serialized) selector apply to some
/// element other than the subject: before a combinator, or in `:has()`?
fn state_reaches_others_in(css: &str) -> bool {
    if css.contains(":has(") {
        return true;
    }
    // The start of the last compound: after the last top-level combinator.
    let mut depth = 0i32;
    let mut last = 0;
    for (i, c) in css.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ' ' | '>' | '+' | '~' if depth == 0 => last = i + 1,
            _ => {}
        }
    }
    STATE_PSEUDOS.iter().any(|p| css.match_indices(p).any(|(i, _)| i < last))
}

#[cfg(test)]
mod state_tests {
    #[test]
    fn which_state_rules_reach_other_elements() {
        use super::state_reaches_others_in as reaches;
        assert!(!reaches("a:hover"));
        assert!(!reaches("nav .item:hover"));
        assert!(reaches(".group:not(:hover) > a:hover"));
        assert!(reaches(".group:hover .title"));
        assert!(reaches("input:checked ~ .track"));
        assert!(reaches("div:has(a:hover)"));
    }
}

impl StyleData {
    pub fn from_sheets(sheets: Vec<Stylesheet>, base: Option<Url>) -> Self {
        let rules: Vec<StyleRule> = sheets.into_iter().flat_map(|s| s.rules).collect();
        let mut conditions: Vec<Arc<str>> = Vec::new();
        for rule in &rules {
            for c in rule.media.iter() {
                if !conditions.contains(c) {
                    conditions.push(c.clone());
                }
            }
        }
        let mut has_state_rules = false;
        let mut state_reaches_others = false;
        for r in &rules {
            for sel in r.selectors.slice() {
                let css = cssparser::ToCss::to_css_string(sel);
                if STATE_PSEUDOS.iter().any(|p| css.contains(p)) {
                    has_state_rules = true;
                    state_reaches_others |= state_reaches_others_in(&css);
                }
            }
        }
        Self { rules: Arc::new(rules), conditions: Arc::new(conditions), base, has_state_rules, state_reaches_others }
    }

    /// Does any rule depend on hover/active/focus state?
    pub fn has_state_rules(&self) -> bool {
        self.has_state_rules
    }

    /// Can an element's hover/focus/checked state restyle *other* elements
    /// (`.card:hover .title`, `input:checked + label`, `:has(:hover)`)? If
    /// not, a state change only restyles the elements whose state changed.
    pub fn state_reaches_others(&self) -> bool {
        self.state_reaches_others
    }

    /// Gather, fetch and parse every stylesheet a document uses. `fetch` is
    /// called (in parallel) for `<link>`ed sheets and `@import`s.
    pub fn for_document(doc: &Document, fetch: &(dyn Fn(&Url) -> Option<String> + Sync)) -> Self {
        let mut external = 0;
        let sources: Vec<StyleSource> = collect_style_sources(doc)
            .into_iter()
            // Print stylesheets and the like can never apply on screen.
            .filter(|s| media::can_ever_match(source_media(s)))
            .filter(|s| {
                matches!(s, StyleSource::Inline { .. }) || {
                    external += 1;
                    external <= MAX_EXTERNAL_SHEETS
                }
            })
            .collect();

        let base = doc.base_url();
        let sheets: Vec<Vec<Stylesheet>> = std::thread::scope(|scope| {
            let handles: Vec<_> = sources
                .iter()
                .map(|source| {
                    let base = &base;
                    scope.spawn(move || {
                        let media = source_media(source);
                        match source {
                            StyleSource::Inline { css, .. } => load_sheet(css, base, media, fetch, 0),
                            StyleSource::External { url, .. } => match fetch(url) {
                                Some(css) => load_sheet(&css, url, media, fetch, 0),
                                None => Vec::new(),
                            },
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        Self::from_sheets(sheets.into_iter().flatten().collect(), Some(base))
    }

    /// Which media conditions hold in `env`. Two environments with the same
    /// signature style a page identically (except for viewport units).
    pub fn signature(&self, env: &MediaEnv) -> Vec<bool> {
        // The color scheme too: `color-scheme` / `light-dark()` follow it
        // without any media query.
        self.conditions.iter().map(|c| media::matches(c, env)).chain([env.dark]).collect()
    }

    /// A styler for this viewport: only the rules whose media conditions hold.
    pub fn styler(&self, env: MediaEnv) -> Styler {
        self.styler_with_state(env, Arc::default())
    }

    /// A styler for this viewport and interaction state.
    pub fn styler_with_state(&self, env: MediaEnv, state: Arc<ElementState>) -> Styler {
        let rules: Vec<StyleRule> = self
            .rules
            .iter()
            .filter(|r| r.media.iter().all(|m| media::matches(m, &env)))
            .cloned()
            .collect();
        Styler {
            author: Indexes::new(rules),
            env,
            state,
            root_font_size: Cell::new(16.0),
            base: self.base.clone(),
            element_hashes: RefCell::default(),
            attr_cache: RefCell::default(),
            var_cache: RefCell::default(),
        }
    }
}

/// Computes styles for one document at one viewport size.
pub struct Styler {
    author: Indexes,
    env: MediaEnv,
    state: Arc<ElementState>,
    root_font_size: Cell<f32>,
    /// The document's base URL, for `url()`s in `style=""` and `var()` values.
    base: Option<Url>,
    /// Each styled element's own bloom hashes (tag, namespace, id, classes).
    /// Styling runs top-down, so ancestors are always already here.
    element_hashes: RefCell<HashMap<NodeId, Vec<u32>>>,
    attr_cache: RefCell<HashMap<String, Rc<ParsedAttr>>>,
    var_cache: RefCell<HashMap<VarKey, Rc<Vec<Decl>>>>,
}

impl Styler {
    pub fn new(sheets: Vec<Stylesheet>, env: MediaEnv) -> Self {
        StyleData::from_sheets(sheets, None).styler(env)
    }

    /// Style for a different interaction state (hover, focus...), keeping
    /// everything that doesn't depend on it: the rules, indexes and caches.
    pub fn set_state(&mut self, state: Arc<ElementState>) {
        self.state = state;
    }

    pub fn env(&self) -> &MediaEnv {
        &self.env
    }

    /// Compute the style of element `id`, given its parent's computed style.
    pub fn compute(&self, doc: &Document, id: NodeId, parent: &ComputedStyle) -> ComputedStyle {
        let mut style = ComputedStyle::inherit(parent);
        let Some(e) = doc.element(id) else { return style };
        // An `<svg>` directly inside HTML is styled like a replaced element
        // (the renderer draws it as an image); everything else non-HTML
        // (SVG internals, MathML) would render as stray text.
        let outer_svg = e.name.ns == page::markup5ever::ns!(svg)
            && &*e.name.local == "svg"
            && doc.parent(id).and_then(|p| doc.element(p)).is_some_and(|p| p.is_html());
        if !e.is_html() && !outer_svg {
            style.display = Display::None;
            return style;
        }

        if self.env.dark && el_is_root(doc, id) {
            // `<meta name="color-scheme" content="light dark">`
            style.dark = meta_allows_dark(doc);
        }
        let el = ElementRef::new(doc, id, &self.state);
        let bloom = self.ancestor_bloom(doc, id);
        let mut caches = SelectorCaches::default();
        let mut matched = Vec::new();
        let normal = MatchingMode::Normal;
        ua_indexes(doc.quirks).element.collect(el, normal, ORIGIN_UA, &bloom, &mut caches, &mut matched);
        self.author.element.collect(el, normal, ORIGIN_AUTHOR, &bloom, &mut caches, &mut matched);

        let hints = presentational_hints(e);
        if !hints.is_empty() {
            let parsed = self.parse_attr(&hints);
            matched.push(Matched {
                origin: ORIGIN_HINTS,
                specificity: 0,
                order: 0,
                normal: parsed.0.clone().into(),
                important: Arc::from([]),
            });
        }
        if let Some(inline) = e.attr("style") {
            let parsed = self.parse_attr(inline);
            matched.push(Matched {
                origin: ORIGIN_INLINE,
                specificity: 0,
                order: 0,
                normal: parsed.0.clone().into(),
                important: parsed.1.clone().into(),
            });
        }
        self.cascade(&mut style, parent, matched);
        if el_is_root(doc, id) {
            self.root_font_size.set(style.font_size);
            if !style.display.is_block_level() && style.display != Display::None {
                style.display = Display::Block;
            }
        }
        style
    }

    /// The style of an element's `::before`/`::after`, if it generates content.
    pub fn compute_pseudo(
        &self,
        doc: &Document,
        id: NodeId,
        element_style: &ComputedStyle,
        which: Pseudo,
    ) -> Option<ComputedStyle> {
        let (ua, author) = (ua_indexes(doc.quirks).pseudo(which), self.author.pseudo(which));
        if ua.is_empty() && author.is_empty() {
            return None;
        }
        let el = ElementRef::new(doc, id, &self.state);
        let bloom = self.ancestor_bloom(doc, id);
        let mut caches = SelectorCaches::default();
        let mut matched = Vec::new();
        let mode = MatchingMode::ForStatelessPseudoElement;
        ua.collect(el, mode, ORIGIN_UA, &bloom, &mut caches, &mut matched);
        author.collect(el, mode, ORIGIN_AUTHOR, &bloom, &mut caches, &mut matched);
        if matched.is_empty() {
            return None;
        }
        let mut style = ComputedStyle::inherit(element_style);
        self.cascade(&mut style, element_style, matched);
        (style.content.is_some() && style.display != Display::None).then_some(style)
    }

    fn cascade(&self, style: &mut ComputedStyle, parent: &ComputedStyle, mut matched: Vec<Matched>) {
        matched.sort_by_key(|m| (m.origin, m.specificity, m.order));

        // Cascade order: normal declarations by origin/specificity/order, then
        // author !important (UA !important isn't used by our UA sheet).
        let mut ordered: Vec<&Decl> = Vec::new();
        for m in &matched {
            ordered.extend(m.normal.iter());
        }
        for m in matched.iter().filter(|m| m.origin >= ORIGIN_AUTHOR) {
            ordered.extend(m.important.iter());
        }

        // Pass 1: custom properties.
        let mut custom: Option<HashMap<Arc<str>, Arc<str>>> = None;
        for d in &ordered {
            if let Decl::Custom(name, value) = d {
                custom
                    .get_or_insert_with(|| (*style.custom).clone())
                    .insert(name.clone(), value.clone());
            }
        }
        if let Some(custom) = custom {
            style.custom = CustomProps::new(custom);
        }

        // Pass 2: font-size (everything else may be em-relative).
        // Pass 3: everything else.
        for font_pass in [true, false] {
            for d in &ordered {
                if matches!(d, Decl::Custom(..)) || d.is_font_size() != font_pass {
                    continue;
                }
                match d {
                    Decl::Unresolved { name, raw } => {
                        if let Some(decls) = self.resolve_vars(name, raw, &style.custom) {
                            for d in decls.iter().filter(|d| d.is_font_size() == font_pass) {
                                self.apply(style, parent, d);
                            }
                        }
                    }
                    d => self.apply(style, parent, d),
                }
            }
        }

        finalize(style);
    }

    /// A bloom filter of every ancestor's tag/id/classes, so selectors needing an
    /// ancestor that isn't there are rejected without walking the tree.
    fn ancestor_bloom(&self, doc: &Document, id: NodeId) -> BloomFilter {
        let mut bloom = BloomFilter::new();
        let mut cache = self.element_hashes.borrow_mut();
        let mut ancestor = doc.parent(id);
        while let Some(a) = ancestor {
            if let Some(e) = doc.element(a) {
                let hashes = cache.entry(a).or_insert_with(|| {
                    let mut h = vec![e.name.local.precomputed_hash(), e.name.ns.precomputed_hash()];
                    if let Some(id) = e.attr("id") {
                        h.push(LocalName::from(id).precomputed_hash());
                    }
                    if let Some(classes) = e.attr("class") {
                        h.extend(classes.split_ascii_whitespace().map(|c| LocalName::from(c).precomputed_hash()));
                    }
                    h
                });
                for &h in hashes.iter() {
                    bloom.insert_hash(h);
                }
            }
            ancestor = doc.parent(a);
        }
        bloom
    }

    fn parse_attr(&self, css: &str) -> Rc<ParsedAttr> {
        if let Some(hit) = self.attr_cache.borrow().get(css) {
            return hit.clone();
        }
        let parsed = Rc::new(crate::props::with_base(self.base.as_ref(), || parse_style_attribute(css)));
        self.attr_cache.borrow_mut().insert(css.to_owned(), parsed.clone());
        parsed
    }

    fn resolve_vars(&self, name: &Arc<str>, raw: &str, custom: &CustomProps) -> Option<Rc<Vec<Decl>>> {
        let value = substitute_vars(raw, custom, 0)?;
        let key = (name.clone(), value);
        if let Some(hit) = self.var_cache.borrow().get(&key) {
            return Some(hit.clone());
        }
        let mut out = Vec::new();
        if !value_still_has_var(&key.1) {
            crate::props::with_base(self.base.as_ref(), || parse_declaration(name, &key.1, &mut out));
        }
        let out = Rc::new(out);
        self.var_cache.borrow_mut().insert(key, out.clone());
        Some(out)
    }

    fn resolve_cx(&self, font_size: f32) -> ResolveCx {
        ResolveCx {
            font_size,
            root_font_size: self.root_font_size.get(),
            viewport_width: self.env.width,
            viewport_height: self.env.height,
        }
    }

    fn apply(&self, s: &mut ComputedStyle, parent: &ComputedStyle, d: &Decl) {
        let cx = self.resolve_cx(s.font_size);
        let len = |spec: &LenSpec| spec.resolve(&cx);
        match d {
            Decl::Display(v) => s.display = *v,
            Decl::Position(v) => s.position = *v,
            Decl::Inset(side, v) => s.inset.set(*side, len(v)),
            Decl::Margin(side, v) => s.margin.set(*side, len(v)),
            Decl::Padding(side, v) => s.padding.set(*side, non_auto(len(v))),
            Decl::BorderWidth(side, v) => s.border_width.set(*side, len(v).px_or_zero().max(0.0)),
            Decl::BorderStyle(side, v) => s.border_style.set(*side, *v),
            Decl::BorderColor(side, v) => s.border_color.set(*side, *v),
            Decl::BorderRadius(corner, v) => s.border_radius[*corner] = non_auto(len(v)),
            Decl::BackgroundColor(c) => s.background = Some(*c),
            Decl::BackgroundImage(c) => s.background_image = *c,
            Decl::BackgroundUrl(u) => s.background_url = u.clone(),
            Decl::BackgroundSize(v) => s.background_size = resolve_bg_size(v, &cx),
            Decl::BackgroundPosition(v) => s.background_position = *v,
            Decl::MaskUrl(u) => s.mask_url = u.clone(),
            Decl::MaskSize(v) => s.mask_size = resolve_bg_size(v, &cx),
            Decl::MaskPosition(v) => s.mask_position = *v,
            Decl::Width(v) => s.width = len(v),
            Decl::Height(v) => s.height = len(v),
            Decl::MinWidth(v) => s.min_width = len(v),
            Decl::MinHeight(v) => s.min_height = len(v),
            Decl::MaxWidth(v) => s.max_width = len(v),
            Decl::MaxHeight(v) => s.max_height = len(v),
            Decl::FlexDirection(v) => s.flex_direction = *v,
            Decl::FlexWrap(v) => s.flex_wrap = *v,
            Decl::JustifyContent(v) => s.justify_content = Some(*v),
            Decl::AlignItems(v) => s.align_items = Some(*v),
            Decl::AlignSelf(v) => s.align_self = *v,
            Decl::RowGap(v) => s.row_gap = non_auto(len(v)),
            Decl::ColumnGap(v) => s.column_gap = non_auto(len(v)),
            Decl::FlexGrow(v) => s.flex_grow = v.max(0.0),
            Decl::FlexShrink(v) => s.flex_shrink = v.max(0.0),
            Decl::FlexBasis(v) => s.flex_basis = len(v),
            Decl::GridColumns(GridCols::None) => s.grid_columns = None,
            Decl::GridColumns(GridCols::Count(n)) => s.grid_columns = Some(*n),
            Decl::GridColumns(GridCols::AutoFill(min)) => {
                let min = match len(min) {
                    Length::Px(v) if v > 0.0 => v,
                    _ => 200.0,
                };
                // Unknown container width: estimate from the viewport.
                s.grid_columns = Some(((self.env.width - 32.0) / min).floor().clamp(1.0, 12.0) as u16);
            }
            Decl::GridColumn(v) => s.grid_column = *v,
            Decl::GridTracks(None) => s.grid_tracks = None,
            Decl::GridTracks(Some(specs)) => {
                let tracks: Vec<Track> = specs
                    .iter()
                    .map(|t| match t {
                        TrackSpec::Len(l) => match len(l) {
                            Length::Px(v) => Track::Px(v),
                            _ => Track::Auto,
                        },
                        TrackSpec::Fr(f) => Track::Fr(*f),
                        TrackSpec::Max(l) => match len(l) {
                            Length::Px(v) => Track::Max(v),
                            _ => Track::Fr(1.0),
                        },
                        TrackSpec::Auto => Track::Auto,
                    })
                    .collect();
                // The column count follows the tracks.
                s.grid_columns = Some(tracks.len().clamp(1, 64) as u16);
                s.grid_tracks = Some(tracks.into());
            }
            Decl::GridAreas(v) => s.grid_areas = v.clone(),
            Decl::GridArea(v) => s.grid_area = v.clone(),
            Decl::Overflow(v) => s.overflow = *v,
            Decl::Opacity(v) => s.opacity = *v,
            Decl::Translate(x, y) => s.translate = (len(x), len(y)),
            Decl::Masked(v) => s.masked = *v,
            Decl::Content(v) => s.content = v.clone(),
            Decl::Float(v) => s.float = *v,
            Decl::BorderBox(v) => s.border_box = *v,
            Decl::Cursor(v) => s.cursor = *v,
            Decl::ColorScheme(v) => s.dark = *v && self.env.dark,
            Decl::VerticalAlign(crate::props::VAlignSpec::Keyword(k)) => s.vertical_align = *k,
            Decl::VerticalAlign(crate::props::VAlignSpec::Len(l)) => {
                s.vertical_align = match len(l) {
                    Length::Px(v) => VerticalAlign::Shift(v),
                    // Percentages are of the line height.
                    Length::Percent(p) => VerticalAlign::Shift(p / 100.0 * s.font_size * 1.2),
                    Length::Auto => VerticalAlign::Baseline,
                }
            }
            Decl::Clear(v) => s.clear = *v,
            Decl::TextDecoration { underline, strikethrough } => {
                s.underline = parent.underline || *underline;
                s.strikethrough = parent.strikethrough || *strikethrough;
            }
            Decl::FontSize(spec) => {
                let parent_size = parent.font_size;
                s.font_size = match spec {
                    FontSizeSpec::Smaller => parent_size / 1.2,
                    FontSizeSpec::Larger => parent_size * 1.2,
                    FontSizeSpec::Len(l) => match l.resolve(&self.resolve_cx(parent_size)) {
                        Length::Px(v) => v,
                        Length::Percent(p) => parent_size * p / 100.0,
                        Length::Auto => parent_size,
                    },
                }
                .clamp(1.0, 1000.0);
            }
            Decl::LineHeight(spec) => {
                s.line_height = match spec {
                    LineHeightSpec::Normal => LineHeight::Normal,
                    LineHeightSpec::Number(n) => LineHeight::Number(*n),
                    LineHeightSpec::Len(l) => match len(l) {
                        Length::Px(v) => LineHeight::Px(v),
                        Length::Percent(p) => LineHeight::Px(s.font_size * p / 100.0),
                        Length::Auto => LineHeight::Normal,
                    },
                }
            }
            Decl::FontWeight(WeightSpec::Abs(w)) => s.font_weight = *w,
            Decl::FontWeight(WeightSpec::Bolder) => {
                s.font_weight = match parent.font_weight {
                    0..350 => 400,
                    350..550 => 700,
                    _ => 900,
                }
            }
            Decl::FontWeight(WeightSpec::Lighter) => {
                s.font_weight = match parent.font_weight {
                    0..550 => 100,
                    550..750 => 400,
                    _ => 700,
                }
            }
            Decl::Italic(v) => s.italic = *v,
            Decl::FontFamily(v) => s.font_family = *v,
            Decl::FontNames(v) => s.font_names = v.clone(),
            Decl::Color(Color::CurrentColor) => s.color = parent.color,
            Decl::Color(c) => s.color = *c,
            Decl::WhiteSpace(v) => s.white_space = *v,
            Decl::TextAlign(v) => s.text_align = *v,
            Decl::ListStyle(v) => s.list_style = *v,
            Decl::TextTransform(v) => s.text_transform = *v,
            Decl::Visible(v) => s.visible = *v,
            Decl::Wide(name, wide) => apply_wide(s, parent, name, *wide),
            Decl::Custom(..) | Decl::Unresolved { .. } => {}
        }
    }
}

fn resolve_bg_size(spec: &crate::props::BgSizeSpec, cx: &ResolveCx) -> BgSize {
    use crate::props::BgSizeSpec as S;
    match spec {
        S::Auto => BgSize::Auto,
        S::Contain => BgSize::Contain,
        S::Cover => BgSize::Cover,
        S::Explicit(w, h) => BgSize::Explicit(w.resolve(cx), h.resolve(cx)),
    }
}

fn el_is_root(doc: &Document, id: NodeId) -> bool {
    doc.parent(id) == Some(doc.root())
}

fn source_media(s: &StyleSource) -> &str {
    match s {
        StyleSource::Inline { media, .. } | StyleSource::External { media, .. } => media,
    }
}

/// Parse a sheet and (recursively) the sheets it `@import`s, imports first.
fn load_sheet(
    css: &str,
    base: &Url,
    media: &str,
    fetch: &(dyn Fn(&Url) -> Option<String> + Sync),
    depth: usize,
) -> Vec<Stylesheet> {
    let mut sheet = crate::props::with_base(Some(base), || parse_stylesheet(css));
    let mut out = Vec::new();
    if depth < MAX_IMPORT_DEPTH {
        for (href, import_media) in &sheet.imports {
            if !media::can_ever_match(import_media) {
                continue;
            }
            if let Ok(url) = base.join(href)
                && let Some(css) = fetch(&url)
            {
                for mut imported in load_sheet(&css, &url, import_media, fetch, depth + 1) {
                    add_condition(&mut imported, media);
                    out.push(imported);
                }
            }
        }
    }
    add_condition(&mut sheet, media);
    out.push(sheet);
    out
}

/// Make every rule of `sheet` also depend on `media` (from `<link media>` etc).
fn add_condition(sheet: &mut Stylesheet, media: &str) {
    let media = media.trim();
    if media.is_empty() || media.eq_ignore_ascii_case("all") || media.eq_ignore_ascii_case("screen") {
        return;
    }
    let condition: Arc<str> = media.into();
    for rule in &mut sheet.rules {
        let mut conditions = rule.media.to_vec();
        conditions.push(condition.clone());
        rule.media = conditions.into();
    }
}

fn non_auto(l: Length) -> Length {
    if l.is_auto() { Length::ZERO } else { l }
}

fn meta_allows_dark(doc: &Document) -> bool {
    doc.descendants(doc.root()).any(|n| {
        doc.element(n).is_some_and(|e| {
            &*e.name.local == "meta"
                && e.attr("name").is_some_and(|v| v.eq_ignore_ascii_case("color-scheme"))
                && e.attr("content").is_some_and(|c| c.split_whitespace().any(|w| w.eq_ignore_ascii_case("dark")))
        })
    })
}

/// The browser's own colors in a dark color scheme (Chrome's).
const DARK_TEXT: u32 = 0xe8eaedff;
const DARK_LINK: u32 = 0x8ab4f8ff;
const DARK_MUTED: u32 = 0x9aa0a6ff;
/// The canvas behind a dark page.
pub const DARK_CANVAS: u32 = 0x121212ff;

/// `light-dark()` and system colors, for this element's color scheme.
fn scheme_color(c: Color, dark: bool) -> Color {
    match c {
        Color::LightDark(l, d) => Color::Rgba(if dark { d } else { l }),
        Color::Foreground if dark => Color::Rgba(DARK_TEXT),
        Color::Link if dark => Color::Rgba(DARK_LINK),
        Color::Muted if dark => Color::Rgba(DARK_MUTED),
        c => c,
    }
}

/// Post-cascade fixups.
fn finalize(s: &mut ComputedStyle) {
    let dark = s.dark;
    s.color = scheme_color(s.color, dark);
    for side in Side::ALL {
        s.border_color.set(side, scheme_color(s.border_color.get(side), dark));
    }
    s.background = s.background.map(|c| scheme_color(c, dark));
    s.background_image = s.background_image.map(|c| scheme_color(c, dark));
    for side in Side::ALL {
        if s.border_style.get(side) == BorderStyle::None {
            s.border_width.set(side, 0.0);
        }
        if s.border_color.get(side) == Color::CurrentColor {
            s.border_color.set(side, s.color);
        }
    }
    if s.background == Some(Color::CurrentColor) {
        s.background = Some(s.color);
    }
    if s.background_image == Some(Color::CurrentColor) {
        s.background_image = Some(s.color);
    }
    // Out-of-flow boxes are always block-level ("blockified").
    if matches!(s.position, Position::Absolute | Position::Fixed)
        && matches!(s.display, Display::Inline | Display::InlineBlock)
    {
        s.display = Display::Block;
    }
}

fn apply_wide(s: &mut ComputedStyle, parent: &ComputedStyle, name: &str, wide: Wide) {
    const INHERITED: &[&str] = &[
        "color", "font", "font-size", "font-weight", "font-style", "font-family", "line-height",
        "text-align", "text-transform", "white-space", "visibility", "list-style", "list-style-type",
    ];
    let initial = ComputedStyle::initial();
    let inherits = INHERITED.contains(&name);
    let src = match wide {
        Wide::Inherit => parent,
        Wide::Initial => &initial,
        Wide::Unset if inherits => parent,
        Wide::Unset => &initial,
    };
    match name {
        "color" => s.color = src.color,
        "font" => {
            s.font_size = src.font_size;
            s.font_weight = src.font_weight;
            s.italic = src.italic;
            s.font_family = src.font_family;
            s.font_names = src.font_names.clone();
            s.line_height = src.line_height;
        }
        "font-size" => s.font_size = src.font_size,
        "font-weight" => s.font_weight = src.font_weight,
        "font-style" => s.italic = src.italic,
        "font-family" => {
            s.font_family = src.font_family;
            s.font_names = src.font_names.clone();
        }
        "line-height" => s.line_height = src.line_height,
        "text-align" => s.text_align = src.text_align,
        "text-transform" => s.text_transform = src.text_transform,
        "white-space" => s.white_space = src.white_space,
        "visibility" => s.visible = src.visible,
        "list-style" | "list-style-type" => s.list_style = src.list_style,
        "display" => s.display = src.display,
        "position" => s.position = src.position,
        "width" => s.width = src.width,
        "height" => s.height = src.height,
        "max-width" => s.max_width = src.max_width,
        "min-width" => s.min_width = src.min_width,
        "max-height" => s.max_height = src.max_height,
        "min-height" => s.min_height = src.min_height,
        "opacity" => s.opacity = src.opacity,
        "text-decoration" | "text-decoration-line" => {
            s.underline = parent.underline || src.underline;
            s.strikethrough = parent.strikethrough || src.strikethrough;
        }
        n if n.starts_with("margin") => s.margin = src.margin,
        n if n.starts_with("padding") => s.padding = src.padding,
        n if n.starts_with("background") => {
            s.background = src.background;
            s.background_image = src.background_image;
        }
        // Each border property resets just itself (`border-radius: inherit`
        // used to wipe the whole border).
        n if n.contains("radius") => s.border_radius = src.border_radius,
        n if n.ends_with("-color") && n.starts_with("border") => s.border_color = src.border_color,
        n if n.ends_with("-style") && n.starts_with("border") => s.border_style = src.border_style,
        n if n.ends_with("-width") && n.starts_with("border") => s.border_width = src.border_width,
        n if n.starts_with("border-collapse") || n.starts_with("border-spacing") || n.starts_with("border-image") => {}
        n if n.starts_with("border") => {
            s.border_width = src.border_width;
            s.border_style = src.border_style;
            s.border_color = src.border_color;
        }
        n if n.starts_with("overflow") => s.overflow = src.overflow,
        _ => {}
    }
}

fn value_still_has_var(v: &str) -> bool {
    crate::props::contains_var(v)
}

/// Replace `var(--name, fallback)` references. `None` if a reference can't be
/// resolved (the declaration is then "invalid at computed-value time").
fn substitute_vars(raw: &str, custom: &CustomProps, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    let Some(start) = lower.find("var(") else {
        return Some(raw.to_owned());
    };
    // Find the matching close paren.
    let body_start = start + 4;
    let mut paren = 1;
    let mut end = None;
    for (i, c) in raw[body_start..].char_indices() {
        match c {
            '(' => paren += 1,
            ')' => {
                paren -= 1;
                if paren == 0 {
                    end = Some(body_start + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let body = &raw[body_start..end];
    let (name, fallback) = match body.find(',') {
        Some(i) => (body[..i].trim(), Some(&body[i + 1..])),
        None => (body.trim(), None),
    };
    let replacement = match custom.get(name) {
        Some(value) if !value.trim().is_empty() => substitute_vars(value, custom, depth + 1)?,
        _ => substitute_vars(fallback?, custom, depth + 1)?,
    };
    let rest = substitute_vars(&raw[end + 1..], custom, depth + 1)?;
    Some(format!("{}{}{}", &raw[..start], replacement.trim(), rest))
}

/// HTML attributes that act as styles (`bgcolor`, `<font color>`, `align`...),
/// as a CSS declaration block.
fn presentational_hints(e: &page::ElementData) -> String {
    let mut css = String::new();
    let tag = &*e.name.local;
    let color = |v: &str| -> String {
        let v = v.trim();
        if v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit()) { format!("#{v}") } else { v.to_owned() }
    };
    if let Some(bg) = e.attr("bgcolor") {
        css.push_str(&format!("background-color:{};", color(bg)));
    }
    match tag {
        "font" => {
            if let Some(c) = e.attr("color") {
                css.push_str(&format!("color:{};", color(c)));
            }
            if let Some(size) = e.attr("size").map(str::trim) {
                let n: i32 = if let Some(rel) = size.strip_prefix('+') {
                    3 + rel.parse::<i32>().unwrap_or(0)
                } else if let Some(rel) = size.strip_prefix('-') {
                    3 - rel.parse::<i32>().unwrap_or(0)
                } else {
                    size.parse().unwrap_or(3)
                };
                let px = [10, 13, 16, 18, 24, 32, 48][(n.clamp(1, 7) - 1) as usize];
                css.push_str(&format!("font-size:{px}px;"));
            }
            if let Some(face) = e.attr("face") {
                css.push_str(&format!("font-family:{face};"));
            }
        }
        "body" => {
            if let Some(c) = e.attr("text") {
                css.push_str(&format!("color:{};", color(c)));
            }
        }
        "hr" => {
            if let Some(c) = e.attr("color") {
                css.push_str(&format!("border-color:{};", color(c)));
            }
        }
        _ => {}
    }
    if let Some(align) = e.attr("align").map(|a| a.trim().to_ascii_lowercase()) {
        match tag {
            "table" | "img" if align == "center" => css.push_str("margin-left:auto;margin-right:auto;"),
            "table" | "img" if align == "left" || align == "right" => css.push_str(&format!("float:{align};")),
            "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "td" | "th" | "tr" | "caption"
            | "thead" | "tbody" | "tfoot" | "center" => {
                if matches!(align.as_str(), "left" | "right" | "center" | "justify") {
                    css.push_str(&format!("text-align:{};", if align == "justify" { "left" } else { &align }));
                }
            }
            _ => {}
        }
    }
    if matches!(tag, "table" | "td" | "th" | "img" | "col" | "hr" | "iframe" | "canvas" | "video") {
        for (attr, prop) in [("width", "width"), ("height", "height")] {
            if let Some(v) = e.attr(attr).map(str::trim).filter(|v| !v.is_empty()) {
                if v.ends_with('%') {
                    css.push_str(&format!("{prop}:{v};"));
                } else if let Ok(n) = v.trim_end_matches("px").parse::<f32>() {
                    css.push_str(&format!("{prop}:{n}px;"));
                }
            }
        }
    }
    if matches!(tag, "td" | "th") && e.attr("nowrap").is_some() {
        css.push_str("white-space:nowrap;");
    }
    if matches!(tag, "ol" | "ul" | "li")
        && let Some(t) = e.attr("type")
    {
        let style = match t {
            "1" => "decimal",
            "a" => "lower-alpha",
            "A" => "upper-alpha",
            "i" => "lower-roman",
            "I" => "upper-roman",
            "disc" | "circle" | "square" | "none" => t,
            _ => "",
        };
        if !style.is_empty() {
            css.push_str(&format!("list-style-type:{style};"));
        }
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(html_src: &str, css: &str) -> (Document, Styler) {
        let doc = html::parse(html_src, Url::parse("https://example.com/").unwrap());
        let sheet = parse_stylesheet(css);
        (doc, Styler::new(vec![sheet], MediaEnv::default()))
    }

    /// Compute styles down the ancestor chain of the first element named `tag`.
    fn style_of(doc: &Document, styler: &Styler, tag: &str) -> ComputedStyle {
        let target = doc.find_element(tag).unwrap();
        let mut chain = vec![target];
        while let Some(p) = doc.parent(*chain.last().unwrap()) {
            if doc.element(p).is_none() {
                break;
            }
            chain.push(p);
        }
        let mut style = ComputedStyle::root_parent();
        for id in chain.into_iter().rev() {
            style = styler.compute(doc, id, &style);
        }
        style
    }

    #[test]
    fn specificity_order_and_important() {
        let (doc, s) = styled(
            r#"<p id="x" class="a b">hi</p>"#,
            "#x { color: red } .a.b { color: blue !important } p { color: green; font-size: 2em } .a { margin: 0 auto }",
        );
        let st = style_of(&doc, &s, "p");
        assert_eq!(st.color, Color::Rgba(0x0000ffff), "!important beats the id selector");
        assert_eq!(st.font_size, 32.0);
        assert_eq!(st.margin.left, Length::Auto);
        assert_eq!(st.margin.top, Length::Px(0.0), "author beats UA p margins");
        assert_eq!(st.display, Display::Block, "UA sheet");
    }

    #[test]
    fn inline_style_and_hints() {
        let (doc, s) = styled(
            r##"<table bgcolor="ff6600"><tr><td><span style="color: #00f; font-weight: 700">x</span></td></tr></table>"##,
            "",
        );
        let span = style_of(&doc, &s, "span");
        assert_eq!(span.color, Color::Rgba(0x0000ffff));
        assert_eq!(span.font_weight, 700);
        let table = style_of(&doc, &s, "table");
        assert_eq!(table.background, Some(Color::Rgba(0xff6600ff)));
    }

    #[test]
    fn custom_properties_and_var() {
        let (doc, s) = styled(
            r#"<div><p>x</p></div>"#,
            ":root { --gap: 12px; --fg: #123456 } div { --fg2: var(--fg) } p { color: var(--fg2); padding: var(--gap) var(--missing, 3px); margin: var(--nope) }",
        );
        let p = style_of(&doc, &s, "p");
        assert_eq!(p.color, Color::Rgba(0x123456ff));
        assert_eq!(p.padding.top, Length::Px(12.0));
        assert_eq!(p.padding.right, Length::Px(3.0));
        // Unresolvable var → declaration ignored → UA margin (1em) stays.
        assert_eq!(p.margin.top, Length::Px(16.0));
    }

    #[test]
    fn em_rem_and_inheritance() {
        let (doc, s) = styled(
            r#"<div><p><b>x</b></p></div>"#,
            "html { font-size: 10px } div { font-size: 2em; color: red } p { padding-left: 1em; margin-left: 2rem; line-height: 1.5 }",
        );
        let p = style_of(&doc, &s, "p");
        assert_eq!(p.font_size, 20.0);
        assert_eq!(p.padding.left, Length::Px(20.0));
        assert_eq!(p.margin.left, Length::Px(20.0));
        let b = style_of(&doc, &s, "b");
        assert_eq!(b.color, Color::Rgba(0xff0000ff), "color inherits");
        assert_eq!(b.line_height, LineHeight::Number(1.5), "numbers inherit as numbers");
        assert!(b.is_bold(), "UA b");
    }

    #[test]
    fn structural_selectors() {
        let (doc, s) = styled(
            "<ul><li>a</li><li>b</li></ul><section><h2>t</h2></section>",
            "li:first-child { color: red } li + li { color: blue } section > h2:is(h1, h2) { display: none } ul:has(> li) { padding-left: 0 }",
        );
        let first = style_of(&doc, &s, "li");
        assert_eq!(first.color, Color::Rgba(0xff0000ff));
        assert_eq!(style_of(&doc, &s, "h2").display, Display::None);
        assert_eq!(style_of(&doc, &s, "ul").padding.left, Length::Px(0.0));
        assert_eq!(style_of(&doc, &s, "ul").list_style, ListStyle::Disc);
    }

    #[test]
    fn link_style_and_decoration() {
        let (doc, s) = styled(r#"<a href="/x">l</a><p>t</p>"#, "a { text-decoration: none }");
        let a = style_of(&doc, &s, "a");
        assert_eq!(a.color, Color::Link);
        assert!(!a.underline, "author none beats UA underline");
    }

    #[test]
    fn border_shorthand_and_currentcolor() {
        let (doc, s) = styled("<div>x</div>", "div { color: #f00; border: 2px solid; border-left: none }");
        let d = style_of(&doc, &s, "div");
        assert_eq!(d.border_width.top, 2.0);
        assert_eq!(d.border_width.left, 0.0);
        assert_eq!(d.border_color.top, Color::Rgba(0xff0000ff));
    }

    #[test]
    fn before_and_after_content() {
        let (doc, s) = styled(
            r#"<span class="edit" title="T">edit</span><p>plain</p>"#,
            r#".edit::before { content: "[" } .edit::after { content: "]" attr(title); color: red } p::after { content: none }"#,
        );
        let span_id = doc.find_element("span").unwrap();
        let span = style_of(&doc, &s, "span");
        let before = s.compute_pseudo(&doc, span_id, &span, Pseudo::Before).expect("::before");
        assert_eq!(before.content.as_deref(), Some(&[ContentItem::Str("[".into())][..]));
        let after = s.compute_pseudo(&doc, span_id, &span, Pseudo::After).expect("::after");
        assert_eq!(after.color, Color::Rgba(0xff0000ff));
        assert_eq!(after.content.as_ref().unwrap().len(), 2);
        let p_id = doc.find_element("p").unwrap();
        let p = style_of(&doc, &s, "p");
        assert!(s.compute_pseudo(&doc, p_id, &p, Pseudo::After).is_none());
        assert!(s.compute_pseudo(&doc, p_id, &p, Pseudo::Before).is_none());
    }

    #[test]
    fn quirks_mode_tables_reset_text_align() {
        let (doc, s) = styled("<center><table><tr><td>x</td></tr></table></center>", "");
        assert!(doc.quirks);
        assert_eq!(style_of(&doc, &s, "td").text_align, TextAlign::Left);
        let (doc, s) = styled("<!DOCTYPE html><center><table><tr><td>x</td></tr></table></center>", "");
        assert_eq!(style_of(&doc, &s, "td").text_align, TextAlign::Center);
    }

    #[test]
    fn media_queries_reevaluate_per_viewport() {
        let doc = html::parse("<p>x</p>", Url::parse("https://example.com/").unwrap());
        let data = StyleData::from_sheets(
            vec![parse_stylesheet("p { color: red } @media (max-width: 600px) { p { color: blue } }")],
            None,
        );
        let narrow = MediaEnv { width: 400.0, ..Default::default() };
        let wide = MediaEnv { width: 1200.0, ..Default::default() };
        assert_ne!(data.signature(&narrow), data.signature(&wide));
        let color = |env| {
            let styler = data.styler(env);
            let p = doc.find_element("p").unwrap();
            let mut style = ComputedStyle::root_parent();
            for id in [doc.find_element("html").unwrap(), doc.find_element("body").unwrap(), p] {
                style = styler.compute(&doc, id, &style);
            }
            style.color
        };
        assert_eq!(color(narrow), Color::Rgba(0x0000ffff));
        assert_eq!(color(wide), Color::Rgba(0xff0000ff));
    }

    #[test]
    fn hover_state_matches() {
        let doc = html::parse(r#"<nav><a href="/x">x</a></nav>"#, Url::parse("https://example.com/").unwrap());
        let data = StyleData::from_sheets(vec![parse_stylesheet("a:hover { color: red } nav:hover a { font-weight: 700 }")], None);
        assert!(data.has_state_rules());
        let a = doc.find_element("a").unwrap();
        let style = |state: ElementState| {
            let styler = data.styler_with_state(MediaEnv::default(), Arc::new(state));
            let mut st = ComputedStyle::root_parent();
            let mut chain = vec![a];
            while let Some(p) = doc.parent(*chain.last().unwrap()) {
                if doc.element(p).is_none() {
                    break;
                }
                chain.push(p);
            }
            for id in chain.into_iter().rev() {
                st = styler.compute(&doc, id, &st);
            }
            st
        };
        let idle = style(ElementState::default());
        assert_eq!(idle.color, Color::Link);
        let mut hovered = ElementState::default();
        let mut node = Some(a);
        while let Some(n) = node {
            hovered.hovered.insert(n);
            node = doc.parent(n);
        }
        let hot = style(hovered);
        assert_eq!(hot.color, Color::Rgba(0xff0000ff));
        assert_eq!(hot.font_weight, 700);
    }

    #[test]
    fn collects_sources_in_order() {
        let doc = html::parse(
            r#"<link rel="stylesheet" href="/a.css"><style>p{}</style><link rel="alternate stylesheet" href="/b.css"><link rel=icon href=/i>"#,
            Url::parse("https://example.com/dir/").unwrap(),
        );
        let sources = collect_style_sources(&doc);
        assert_eq!(sources.len(), 2);
        assert!(matches!(&sources[0], StyleSource::External { url, .. } if url.as_str() == "https://example.com/a.css"));
        assert!(matches!(&sources[1], StyleSource::Inline { css, .. } if css == "p{}"));
    }
}
