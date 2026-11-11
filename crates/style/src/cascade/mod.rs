//! The cascade: find matching rules, order them, apply declarations, compute.

mod apply;
mod dark;
mod index;
mod layers;
mod size;
mod sources;
mod vars;
pub use vars::computed_custom;

pub use dark::DARK_CANVAS;
pub use sources::{StyleSource, collect_style_sources};
use self::{apply::*, dark::*, index::*, layers::*, sources::*};

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
use crate::sheet::{FontFace, Keyframes, StyleRule, Stylesheet, parse_style_attribute, parse_stylesheet};
use crate::values::*;
use crate::{ComputedStyle, CustomProps, SvgPaint};

/// Generated-content pseudo-elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pseudo {
    Before,
    After,
    /// A text field's placeholder text (only its color is used).
    Placeholder,
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
    /// What `:has()` arguments match, if any rule has one (see `cache`).
    has_keys: Option<Arc<crate::cache::HasKeys>>,
    keyframes: Arc<HashMap<Arc<str>, Arc<Keyframes>>>,
    font_faces: Arc<Vec<FontFace>>,
    registered: Arc<Registry>,
    /// Custom property maps handed out so far (see `Styler::intern_custom`),
    /// shared by every restyle of the page.
    custom_maps: CustomInterner,
}

/// Equal custom property maps, shared: by hash of their entries.
type CustomInterner = Arc<std::sync::Mutex<HashMap<u64, Vec<CustomProps>>>>;

impl StyleData {
    pub fn from_sheets(sheets: Vec<Stylesheet>, base: Option<Url>) -> Self {
        let mut keyframes: HashMap<Arc<str>, Arc<Keyframes>> = HashMap::new();
        let mut rules: Vec<StyleRule> = Vec::new();
        let mut font_faces: Vec<FontFace> = Vec::new();
        let mut layers = LayerOrder::default();
        let mut registry = Registry::default();
        for sheet in sheets {
            for r in sheet.properties {
                // The first registration counts.
                if !registry.initial.contains_key(&r.name) {
                    if !r.inherits {
                        registry.reset.push((r.name.clone(), r.initial.clone()));
                    }
                    registry.initial.insert(r.name, r.initial);
                }
            }
            for name in &sheet.layers {
                layers.declare(name);
            }
            rules.extend(sheet.rules);
            for face in sheet.font_faces {
                if !font_faces.contains(&face) {
                    font_faces.push(face);
                }
            }
            for (name, frames) in sheet.keyframes {
                // Later definitions win.
                keyframes.insert(name, Arc::new(frames));
            }
        }
        let keyframes = Arc::new(keyframes);
        layers.rank(&mut rules);
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
        let mut has_keys: Option<crate::cache::HasKeys> = None;
        for r in &rules {
            for sel in r.selectors.slice() {
                let css = cssparser::ToCss::to_css_string(sel);
                if css.contains(":has(") {
                    has_keys.get_or_insert_with(Default::default).add(&css);
                }
                if STATE_PSEUDOS.iter().any(|p| css.contains(p)) {
                    has_state_rules = true;
                    state_reaches_others |= state_reaches_others_in(&css);
                }
            }
        }
        Self {
            rules: Arc::new(rules),
            conditions: Arc::new(conditions),
            base,
            has_state_rules,
            state_reaches_others,
            has_keys: has_keys.map(Arc::new),
            keyframes,
            font_faces: Arc::new(font_faces),
            registered: Arc::new(registry),
            custom_maps: CustomInterner::default(),
        }
    }

    /// Every family name some rule's `font-family` mentions (lowercase).
    pub fn used_font_families(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for rule in self.rules.iter() {
            for d in rule.normal.iter().chain(rule.important.iter()) {
                match d {
                    Decl::FontNames(names) => out.extend(names.iter().map(|n| n.to_lowercase())),
                    // `font-family: var(--font)`: can't tell; any face might be it.
                    Decl::Unresolved { name, raw } if name.contains("font") => {
                        out.extend(raw.split(',').map(|n| n.trim().trim_matches(|c| c == '"' || c == '\'').to_lowercase()));
                        out.extend(self.font_faces.iter().map(|f| f.family.to_lowercase()));
                    }
                    _ => {}
                }
            }
        }
        out
    }

    /// The page's web fonts (`@font-face`).
    pub fn font_faces(&self) -> &[FontFace] {
        &self.font_faces
    }

    /// Which stylesheets these are (equal for clones of one `StyleData`).
    pub(crate) fn identity(&self) -> usize {
        Arc::as_ptr(&self.rules) as *const () as usize
    }

    pub(crate) fn has_keys(&self) -> Option<Arc<crate::cache::HasKeys>> {
        self.has_keys.clone()
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
        Self::for_document_with(doc, fetch, &|_| None)
    }

    /// The same, with some `<style>` elements' sheets given in place of
    /// their text (see `collect_style_sources_with`).
    pub fn for_document_with(doc: &Document, fetch: &(dyn Fn(&Url) -> Option<String> + Sync), overrides: &dyn Fn(NodeId) -> Option<String>) -> Self {
        let mut external = 0;
        let sources: Vec<StyleSource> = sources::collect_style_sources_with(doc, overrides)
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
        Self::parse_sources(&sources, &base, fetch)
    }

    fn parse_sources(sources: &[StyleSource], base: &Url, fetch: &(dyn Fn(&Url) -> Option<String> + Sync)) -> Self {
        let base = base.clone();
        let sheets: Vec<Vec<Stylesheet>> = std::thread::scope(|scope| {
            let handles: Vec<_> = sources
                .iter()
                .map(|source| {
                    let base = &base;
                    scope.spawn(move || {
                        let media = source_media(source);
                        let mut sheets = match source {
                            StyleSource::Inline { css, .. } => parsed_sheets(css, base, media, fetch),
                            StyleSource::External { url, .. } => match fetch(url) {
                                Some(css) => parsed_sheets(&css, url, media, fetch),
                                None => Vec::new(),
                            },
                        };
                        if let Some(tree) = source_scope(source) {
                            for rule in sheets.iter_mut().flat_map(|s| s.rules.iter_mut()) {
                                rule.scope = Some(tree);
                            }
                        }
                        sheets
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
        // The document's rules, and each shadow tree's.
        let mut rules: Vec<StyleRule> = Vec::new();
        let mut scoped: HashMap<NodeId, Vec<StyleRule>> = HashMap::new();
        for r in self.rules.iter().filter(|r| r.media.iter().all(|m| media::matches(m, &env))) {
            match r.scope {
                None => rules.push(r.clone()),
                Some(tree) => scoped.entry(tree).or_default().push(r.clone()),
            }
        }
        Styler {
            author: Indexes::new(rules),
            scoped: scoped.into_iter().map(|(tree, rules)| (tree, Indexes::new(rules))).collect(),
            env,
            state,
            root_font_size: Cell::new(16.0),
            base: self.base.clone(),
            element_hashes: RefCell::default(),
            attr_cache: RefCell::default(),
            var_cache: RefCell::default(),
            keyframes: self.keyframes.clone(),
            registered: self.registered.clone(),
            custom_maps: self.custom_maps.clone(),
        }
    }
}

/// Computes styles for one document at one viewport size.
pub struct Styler {
    author: Indexes,
    /// Shadow trees' rules, by host.
    scoped: HashMap<NodeId, Indexes>,
    env: MediaEnv,
    state: Arc<ElementState>,
    root_font_size: Cell<f32>,
    /// The document's base URL, for `url()`s in `style=""` and `var()` values.
    base: Option<Url>,
    /// Each styled element's own bloom hashes (tag, namespace, id, classes),
    /// with a hash of the `id` and `class` they were made from: a styler
    /// kept across DOM changes (`getComputedStyle`'s) recomputes them when
    /// those change (else a parent's new class never matched `.a .b`).
    element_hashes: RefCell<HashMap<NodeId, (u64, Vec<u32>)>>,
    attr_cache: RefCell<HashMap<String, Rc<ParsedAttr>>>,
    var_cache: RefCell<HashMap<VarKey, Rc<Vec<Decl>>>>,
    keyframes: Arc<HashMap<Arc<str>, Arc<Keyframes>>>,
    registered: Arc<Registry>,
    custom_maps: CustomInterner,
}

/// The page's `@property` registrations.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    /// Each registered property's initial value.
    pub initial: HashMap<Arc<str>, Option<Arc<str>>>,
    /// Those that don't inherit: (name, initial value).
    pub reset: Vec<(Arc<str>, Option<Arc<str>>)>,
}

impl Styler {
    /// The shared copy of a custom property map. Every style carries its
    /// element's (inherited) variables, hundreds on some sites; with each
    /// restyle making new maps, telling an unchanged layout from a changed
    /// one compared them all, box by box (160ms on a YouTube update).
    /// Shared maps compare by pointer.
    fn intern_custom(&self, map: HashMap<Arc<str>, Arc<str>>) -> CustomProps {
        use std::hash::{Hash, Hasher};
        // Order-independent: the sum of each entry's hash.
        let key = map.iter().fold(map.len() as u64, |acc, entry| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            entry.hash(&mut h);
            acc.wrapping_add(h.finish())
        });
        let mut maps = self.custom_maps.lock().unwrap_or_else(|e| e.into_inner());
        if maps.len() > 16_384 {
            maps.clear();
        }
        let bucket = maps.entry(key).or_default();
        if let Some(found) = bucket.iter().find(|m| ***m == map) {
            return found.clone();
        }
        let shared = CustomProps::new(map);
        bucket.push(shared.clone());
        shared
    }

    /// The styles of an element at each keyframe of `@keyframes name`
    /// (offset, style), with the element's own style at 0% and 100% where
    /// the keyframes don't say.
    pub fn keyframe_styles(&self, name: &str, base: &ComputedStyle) -> Option<Vec<(f32, ComputedStyle)>> {
        let frames = self.keyframes.get(name)?;
        let mut out: Vec<(f32, ComputedStyle)> = Vec::new();
        for (offset, decls) in frames.iter() {
            let mut st = base.clone();
            for d in decls.iter() {
                match d {
                    Decl::Unresolved { name, raw } => match self.resolve_vars(name, raw, &st.custom) {
                        Some(resolved) if !resolved.is_empty() => {
                            for d in resolved.iter() {
                                self.apply(&mut st, base, d);
                            }
                        }
                        _ => apply_wide(&mut st, base, name, Wide::Unset),
                    },
                    d => self.apply(&mut st, base, d),
                }
            }
            finalize(&mut st);
            out.push((*offset, st));
        }
        if out.first().is_none_or(|f| f.0 > 0.0) {
            out.insert(0, (0.0, base.clone()));
        }
        if out.last().is_none_or(|f| f.0 < 1.0) {
            out.push((1.0, base.clone()));
        }
        Some(out)
    }

    /// The custom properties `@keyframes name` animates, per keyframe
    /// (`border-rotate { to { --angle: 360deg } }`): those change what
    /// `var()`s resolve to, so frames are whole restyles (see
    /// `compute_with_custom`).
    pub fn keyframe_custom(&self, name: &str) -> Option<Vec<(f32, Vec<(Arc<str>, Arc<str>)>)>> {
        let frames = self.keyframes.get(name)?;
        let out: Vec<(f32, Vec<(Arc<str>, Arc<str>)>)> = frames
            .iter()
            .map(|(offset, decls)| {
                let custom = decls
                    .iter()
                    .filter_map(|d| match d {
                        Decl::Custom(n, v) => Some((n.clone(), v.clone())),
                        _ => None,
                    })
                    .collect();
                (*offset, custom)
            })
            .collect();
        out.iter().any(|(_, c): &(f32, Vec<_>)| !c.is_empty()).then_some(out)
    }

    /// An element's style with some custom properties overridden (inherited
    /// from its parent as if set there).
    pub fn compute_with_custom(&self, doc: &Document, id: NodeId, parent: &ComputedStyle, overrides: &[(Arc<str>, Arc<str>)]) -> ComputedStyle {
        let mut parent = parent.clone();
        let mut custom = (*parent.custom).clone();
        for (k, v) in overrides {
            custom.insert(k.clone(), v.clone());
        }
        parent.custom = Arc::new(custom);
        self.compute(doc, id, &parent)
    }

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
        ua_indexes(doc.quirks).element.collect(el, normal, ORIGIN_UA, Scope::UA, &bloom, &mut caches, &mut matched);
        self.collect_author(el, normal, |ix| &ix.element, &bloom, &mut caches, &mut matched);

        let hints = presentational_hints(e);
        if !hints.is_empty() {
            let parsed = self.parse_attr(&hints);
            matched.push(Matched {
                origin: ORIGIN_HINTS,
                specificity: 0,
                order: 0,
                normal: parsed.0.clone().into(),
                important: Arc::from([]),
                layer: UNLAYERED,
                containers: None,
                inner: false,
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
                layer: UNLAYERED,
                containers: None,
                inner: false,
            });
        }
        self.cascade(&mut style, parent, matched);
        let root = el_is_root(doc, id);
        if root {
            self.root_font_size.set(style.font_size);
            if !style.display.is_block_level() && style.display != Display::None {
                style.display = Display::Block;
            }
        }
        self.estimate_size(&mut style, parent, root);
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
        let ua = ua_indexes(doc.quirks).pseudo(which);
        if ua.is_empty() && self.author.pseudo(which).is_empty() && self.scoped.values().all(|ix| ix.pseudo(which).is_empty()) {
            return None;
        }
        let el = ElementRef::new(doc, id, &self.state);
        let bloom = self.ancestor_bloom(doc, id);
        let mut caches = SelectorCaches::default();
        let mut matched = Vec::new();
        let mode = MatchingMode::ForStatelessPseudoElement;
        ua.collect(el, mode, ORIGIN_UA, Scope::UA, &bloom, &mut caches, &mut matched);
        self.collect_author(el, mode, |ix| ix.pseudo(which), &bloom, &mut caches, &mut matched);
        if matched.is_empty() {
            return None;
        }
        let mut style = ComputedStyle::inherit(element_style);
        self.cascade(&mut style, element_style, matched);
        let generates = which == Pseudo::Placeholder || style.content.is_some();
        (generates && style.display != Display::None).then_some(style)
    }

    fn cascade(&self, style: &mut ComputedStyle, parent: &ComputedStyle, mut matched: Vec<Matched>) {
        // `@container` rules: against the nearest query container (the
        // viewport when there's none).
        if matched.iter().any(|m| m.containers.is_some()) {
            let size = parent.query_container.unwrap_or((self.env.width, self.env.height));
            matched.retain(|m| m.containers.iter().flat_map(|c| c.iter()).all(|c| media::container_matches(c, size)));
        }
        // Cascade order: normal declarations by origin, layer, specificity
        // and order; then author !important ones, where layers count the
        // other way round (an earlier layer's !important wins).
        // Between shadow trees ("context"), the outer tree's normal
        // declarations win and the inner one's !important ones do.
        matched.sort_by_key(|m| (m.origin, !m.inner, m.layer, m.specificity, m.order));
        let mut ordered: Vec<&Decl> = Vec::new();
        for m in &matched {
            ordered.extend(m.normal.iter());
        }
        let mut important: Vec<&Matched> = matched.iter().filter(|m| m.origin >= ORIGIN_AUTHOR && !m.important.is_empty()).collect();
        important.sort_by_key(|m| (m.origin, m.inner, std::cmp::Reverse(m.layer), m.specificity, m.order));
        for m in important {
            ordered.extend(m.important.iter());
        }

        // Pass 1: custom properties.
        let mut custom: Option<HashMap<Arc<str>, Arc<str>>> = None;
        // Registered ones that don't inherit start over at their initial
        // value (Tailwind's `--tw-*`: a parent's translate isn't its
        // children's).
        let inherited_differs = |name: &Arc<str>, initial: &Option<Arc<str>>| style.custom.get(name) != initial.as_ref();
        if self.registered.reset.iter().any(|(n, i)| inherited_differs(n, i)) {
            let map = custom.get_or_insert_with(|| (*style.custom).clone());
            for (name, initial) in &self.registered.reset {
                match initial {
                    Some(v) => map.insert(name.clone(), v.clone()),
                    None => map.remove(name),
                };
            }
        }
        for d in &ordered {
            if let Decl::Custom(name, value) = d {
                custom
                    .get_or_insert_with(|| (*style.custom).clone())
                    .insert(name.clone(), value.clone());
            }
        }
        if let Some(custom) = custom {
            style.custom = self.intern_custom(custom);
        }

        // Pass 2: font-size (everything else may be em-relative).
        // Pass 3: everything else.
        for font_pass in [true, false] {
            for d in &ordered {
                // The `font` shorthand (when it's `inherit` or has a
                // `var()`) is both: its size first, then the rest in order
                // with everything else (a lower-precedence `font-family`
                // mustn't win by coming in a later pass).
                let both = matches!(d, Decl::Unresolved { name, .. } | Decl::Wide(name, _) if &**name == "font");
                if matches!(d, Decl::Custom(..)) || (d.is_font_size() != font_pass && !both) {
                    continue;
                }
                // ...the rest only, the second time: the size stays as the
                // first pass left it (a later `font-size` won there).
                let size = style.font_size;
                match d {
                    Decl::Unresolved { name, raw } => match self.resolve_vars(name, raw, &style.custom) {
                        Some(decls) if !decls.is_empty() => {
                            for d in decls.iter().filter(|d| d.is_font_size() == font_pass) {
                                self.apply(style, parent, d);
                            }
                        }
                        // Invalid at computed-value time (an undefined
                        // variable, or a value that doesn't parse once
                        // substituted): the property is `unset`, not left
                        // as earlier rules had it.
                        _ => apply_wide(style, parent, name, Wide::Unset),
                    },
                    d => self.apply(style, parent, d),
                }
                if both && !font_pass {
                    style.font_size = size;
                }
            }
        }

        finalize(style);
        // Flex and grid items are block-level ("blockified"): a <span> in a
        // flex row is a box, and its block `::before` stays inside it
        // (GitHub's tabs reserve their bold width with one).
        if matches!(parent.display, Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid) {
            style.display = style.display.blockified();
        }
    }

    /// Author rules matching `el`: those of the tree it's in (a shadow
    /// tree's, or the document's), its own shadow tree's `:host` rules if
    /// it's a host, and the `::slotted` rules of each tree it's slotted into.
    fn collect_author<'s>(
        &'s self,
        el: ElementRef<'_>,
        mode: MatchingMode,
        index: impl Fn(&'s Indexes) -> &'s RuleIndex,
        bloom: &BloomFilter,
        caches: &mut SelectorCaches,
        out: &mut Vec<Matched>,
    ) {
        let own = Scope { host: el.tree(), ..Scope::UA };
        let tree = match own.host {
            None => Some(&self.author),
            Some(host) => self.scoped.get(&host),
        };
        if let Some(ix) = tree {
            index(ix).collect(el, mode, ORIGIN_AUTHOR, own, bloom, caches, out);
        }
        if self.scoped.is_empty() {
            return;
        }
        if let Some(ix) = self.scoped.get(&el.id) {
            let host = Scope { host: Some(el.id), featureless: true, inner: true, slotted_only: false };
            index(ix).collect(el, mode, ORIGIN_AUTHOR, host, bloom, caches, out);
        }
        let mut slot = el.slot();
        while let Some(s) = slot {
            let Some(host) = el.doc.tree_of(s) else { break };
            if let Some(ix) = self.scoped.get(&host) {
                let slotted = Scope { host: Some(host), featureless: false, inner: true, slotted_only: true };
                index(ix).collect(el, mode, ORIGIN_AUTHOR, slotted, bloom, caches, out);
            }
            slot = ElementRef::new(el.doc, s, el.state).slot();
        }
    }

    /// A bloom filter of every ancestor's tag/id/classes, so selectors needing an
    /// ancestor that isn't there are rejected without walking the tree.
    fn ancestor_bloom(&self, doc: &Document, id: NodeId) -> BloomFilter {
        let mut bloom = BloomFilter::new();
        let mut cache = self.element_hashes.borrow_mut();
        let mut ancestor = doc.parent(id);
        while let Some(a) = ancestor {
            if let Some(e) = doc.element(a) {
                let key = {
                    use std::hash::{Hash, Hasher};
                    let mut k = std::collections::hash_map::DefaultHasher::new();
                    (e.attr("id"), e.attr("class")).hash(&mut k);
                    k.finish()
                };
                let entry = cache.entry(a).or_insert_with(|| (key.wrapping_add(1), Vec::new()));
                if entry.0 != key {
                    let mut h = vec![e.name.local.precomputed_hash(), e.name.ns.precomputed_hash()];
                    if let Some(id) = e.attr("id") {
                        h.push(LocalName::from(id).precomputed_hash());
                    }
                    if let Some(classes) = e.attr("class") {
                        h.extend(classes.split_ascii_whitespace().map(|c| LocalName::from(c).precomputed_hash()));
                    }
                    *entry = (key, h);
                }
                for &h in entry.1.iter() {
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

}

#[cfg(test)]
mod tests;
