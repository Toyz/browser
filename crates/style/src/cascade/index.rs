//! Rules indexed for matching, and the order matched rules cascade in.

use super::*;

/// One selector of one rule, filed under its rightmost compound's most specific key.
pub(super) struct Entry {
    pub(super) rule: u32,
    pub(super) selector: u32,
    pub(super) specificity: u32,
    /// What the selector needs from ancestors, for bloom-filter fast rejection.
    pub(super) hashes: AncestorHashes,
    /// It's a `::slotted()` selector (the only kind that reaches nodes
    /// slotted into its tree).
    pub(super) slotted: bool,
}

/// Which tree's rules are being matched, and how.
#[derive(Clone, Copy)]
pub(super) struct Scope {
    /// The shadow tree's host (`None`: the document's tree).
    pub(super) host: Option<NodeId>,
    /// Matching a host against its own shadow tree's rules: only `:host`
    /// reaches it there.
    pub(super) featureless: bool,
    /// Only `::slotted()` selectors.
    pub(super) slotted_only: bool,
    /// The rules come from a tree inside the element's own (context order).
    pub(super) inner: bool,
}

impl Scope {
    /// The UA sheet: every tree's elements, matched as they are.
    pub(super) const UA: Scope = Scope { host: None, featureless: false, slotted_only: false, inner: false };
}

#[derive(Default)]
pub(super) struct RuleIndex {
    pub(super) rules: Arc<Vec<StyleRule>>,
    pub(super) by_id: HashMap<LocalName, Vec<Entry>>,
    pub(super) by_class: HashMap<LocalName, Vec<Entry>>,
    pub(super) by_tag: HashMap<LocalName, Vec<Entry>>,
    pub(super) universal: Vec<Entry>,
}

impl RuleIndex {
    /// Index the selectors that target `pseudo` (`None`: the element itself;
    /// `Some("before")`: its `::before`, etc).
    pub(super) fn new(rules: Arc<Vec<StyleRule>>, pseudo: Option<&str>) -> Self {
        let mut index = RuleIndex::default();
        for (rule_ix, rule) in rules.iter().enumerate() {
            for (sel_ix, selector) in rule.selectors.slice().iter().enumerate() {
                // `::-webkit-input-placeholder` is `::placeholder` with an accent.
                let name = selector.pseudo_element().map(|pe| match pe.0.as_str() {
                    "-webkit-input-placeholder" | "-moz-placeholder" => "placeholder",
                    other => other,
                });
                if name != pseudo {
                    continue;
                }
                let entry = Entry {
                    rule: rule_ix as u32,
                    selector: sel_ix as u32,
                    specificity: selector.specificity(),
                    hashes: AncestorHashes::new(selector, QuirksMode::NoQuirks),
                    slotted: selector.iter_raw_match_order().any(|c| matches!(c, Component::Slotted(_))),
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

    pub(super) fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_class.is_empty() && self.by_tag.is_empty() && self.universal.is_empty()
    }

    pub(super) fn collect(
        &self,
        el: ElementRef<'_>,
        mode: MatchingMode,
        origin: u8,
        scope: Scope,
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
        let host = scope.host.map(|h| selectors::Element::opaque(&ElementRef::new(el.doc, h, el.state)));
        for entry in candidates.into_iter().flatten() {
            if scope.slotted_only && !entry.slotted {
                continue;
            }
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
            cx.current_host = host;
            cx.featureless = scope.featureless;
            if matches_selector(selector, 0, Some(&entry.hashes), el, &mut cx) {
                out.push(Matched {
                    origin,
                    specificity: entry.specificity,
                    order: entry.rule,
                    normal: rule.normal.clone(),
                    important: rule.important.clone(),
                    layer: rule.layer_rank,
                    containers: rule.media.iter().any(|m| media::is_container(m)).then(|| rule.media.clone()),
                    inner: scope.inner,
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

pub(super) struct Matched {
    pub(super) origin: u8,
    pub(super) specificity: u32,
    pub(super) order: u32,
    pub(super) normal: Arc<[Decl]>,
    pub(super) important: Arc<[Decl]>,
    /// Its cascade layer's rank: later layers win, unlayered rules most.
    pub(super) layer: u16,
    /// `@container` conditions around the rule (checked against the
    /// element's query container when it's cascaded).
    pub(super) containers: Option<Arc<[Arc<str>]>>,
    /// From an inner shadow tree's `:host` or `::slotted` rules.
    pub(super) inner: bool,
}

pub(super) const ORIGIN_UA: u8 = 0;

pub(super) const ORIGIN_HINTS: u8 = 1;

pub(super) const ORIGIN_AUTHOR: u8 = 2;

pub(super) const ORIGIN_INLINE: u8 = 3;

/// Rule indexes for one origin: the element itself, `::before`, `::after`.
pub(super) struct Indexes {
    pub(super) element: RuleIndex,
    pub(super) before: RuleIndex,
    pub(super) after: RuleIndex,
    pub(super) placeholder: RuleIndex,
}

impl Indexes {
    pub(super) fn new(rules: Vec<StyleRule>) -> Self {
        let rules = Arc::new(rules);
        Self {
            element: RuleIndex::new(rules.clone(), None),
            before: RuleIndex::new(rules.clone(), Some("before")),
            after: RuleIndex::new(rules.clone(), Some("after")),
            placeholder: RuleIndex::new(rules, Some("placeholder")),
        }
    }

    pub(super) fn pseudo(&self, which: Pseudo) -> &RuleIndex {
        match which {
            Pseudo::Before => &self.before,
            Pseudo::After => &self.after,
            Pseudo::Placeholder => &self.placeholder,
        }
    }
}

pub(super) fn ua_indexes(quirks: bool) -> &'static Indexes {
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

pub(super) const STATE_PSEUDOS: [&str; 5] = [":hover", ":active", ":focus", ":target", ":checked"];

/// Does a state pseudo-class in this (serialized) selector apply to some
/// element other than the subject: before a combinator, or in `:has()`?
pub(super) fn state_reaches_others_in(css: &str) -> bool {
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
