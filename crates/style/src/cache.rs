//! Restyling a changed document without restyling all of it: each
//! element's computed style is kept with the parent style it was computed
//! from, and reused next time unless something it may depend on changed.
//!
//! What an element's style depends on, beyond its parent's style (checked
//! by comparing that): its own name, attributes and shadow tree; its place
//! among its siblings (`:nth-child`, `:last-child`, `+`, `~`); its ancestors
//! (descendant combinators: any change to an element restyles its whole
//! subtree); its descendants, through `:has()`; interaction state; the
//! root's font size (`rem`); the viewport and the stylesheets. Changes are
//! found by comparing the document with the one the styles were computed
//! for. Anything not handled precisely restyles everything.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use page::{Document, NodeId};

use crate::{ComputedStyle, ElementState, MediaEnv, StyleData};

/// What the elements inside the page's `:has()` arguments are known by:
/// only a change to an element with one of these can change a `:has()`.
#[derive(Debug, Default, Clone)]
pub(crate) struct HasKeys {
    /// Each compound's most telling name: its id, else a class, else its
    /// tag (lowercase), else its attribute names (lowercase).
    names: HashSet<String>,
    /// Some compound can't be keyed (`:has(> *)`, `:has(:first-child)`):
    /// any change might matter.
    any: bool,
    /// The same for compounds with an interaction pseudo-class
    /// (`:has(.button:hover)`): whose state change might matter. [0] for
    /// `:hover`/`:active`, [1] for focus, `:target` and `:checked`.
    state_names: [HashSet<String>; 2],
    state_any: [bool; 2],
}

/// A compound selector being read: what it requires of its element.
#[derive(Default)]
struct Compound {
    id: Option<String>,
    class: Option<String>,
    tag: Option<String>,
    attrs: Vec<String>,
    /// Interaction pseudo-classes: [hover/active, focus/target/checked].
    state: [bool; 2],
    /// Something not keyed (`*`, `:first-child`).
    other: bool,
}

impl HasKeys {
    /// Note the `:has()` arguments in a serialized selector.
    pub(crate) fn add(&mut self, css: &str) {
        let mut rest = css;
        while let Some(at) = rest.find(":has(") {
            let start = at + ":has(".len();
            let mut depth = 1;
            let mut end = rest.len();
            for (i, c) in rest[start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = start + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            self.add_argument(&rest[start..end]);
            rest = &rest[end.min(rest.len())..];
        }
    }

    fn flush(&mut self, c: Compound) {
        let keys: Vec<String> = match c.id.or(c.class).or(c.tag) {
            Some(k) => vec![k],
            None => c.attrs,
        };
        if keys.is_empty() {
            for kind in 0..2 {
                self.state_any[kind] |= c.state[kind];
            }
            if c.other && !c.state.contains(&true) {
                self.any = true;
            }
            return;
        }
        for kind in 0..2 {
            if c.state[kind] {
                self.state_names[kind].extend(keys.iter().cloned());
            }
        }
        self.names.extend(keys);
    }

    fn add_argument(&mut self, arg: &str) {
        let bytes = arg.as_bytes();
        let ident_end = |from: usize| {
            let mut i = from;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_' || bytes[i] >= 0x80 || bytes[i] == b'\\') {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i.min(bytes.len())
        };
        let mut c = Compound::default();
        let mut i = 0;
        let mut compound_start = true;
        while i < bytes.len() {
            match bytes[i] {
                b'.' | b'#' => {
                    let end = ident_end(i + 1);
                    let name = arg[i + 1..end].to_owned();
                    if bytes[i] == b'#' { c.id.get_or_insert(name); } else { c.class.get_or_insert(name); }
                    i = end;
                    compound_start = false;
                }
                b'[' => {
                    let end = ident_end(i + 1);
                    let name = arg[i + 1..end].to_ascii_lowercase();
                    let close = arg[i..].find(']').map_or(bytes.len(), |close| i + close);
                    // `[role="dialog"]`: by its value too.
                    let value = arg[end..close].strip_prefix('=').map(|v| v.trim_matches('"'));
                    c.attrs.push(match value {
                        Some(v) => format!("{name}={v}"),
                        None => name,
                    });
                    i = (close + 1).min(bytes.len());
                    compound_start = false;
                }
                b':' => {
                    let from = if bytes.get(i + 1) == Some(&b':') { i + 2 } else { i + 1 };
                    let end = ident_end(from);
                    // (`:focus-visible`, `:focus-within` too.)
                    let pseudo = &arg[from..end];
                    if ["hover", "active"].contains(&pseudo) {
                        c.state[0] = true;
                        i = end;
                    } else if ["focus", "target", "checked"].iter().any(|p| pseudo.starts_with(p)) {
                        // (`:focus-visible`, `:focus-within` too.)
                        c.state[1] = true;
                        i = end;
                    } else if &arg[from..end] == "not" && bytes.get(end) == Some(&b'(') {
                        // What the element isn't: the rest of its compound
                        // keys it (or nothing does).
                        let mut depth = 0;
                        let mut j = end;
                        while j < bytes.len() {
                            match bytes[j] {
                                b'(' => depth += 1,
                                b')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            j += 1;
                        }
                        c.other = true;
                        i = (j + 1).min(bytes.len());
                    } else {
                        if !matches!(&arg[from..end], "is" | "where") {
                            c.other = true;
                        }
                        i = end;
                    }
                    compound_start = false;
                }
                b' ' | b'>' | b'+' | b'~' | b',' | b'(' | b')' => {
                    if !compound_start {
                        self.flush(std::mem::take(&mut c));
                    }
                    i += 1;
                    compound_start = true;
                }
                b'*' => {
                    c.other = true;
                    i += 1;
                    compound_start = false;
                }
                ch if compound_start && (ch.is_ascii_alphabetic() || ch == b'-' || ch == b'_') => {
                    let end = ident_end(i);
                    c.tag = Some(arg[i..end].to_ascii_lowercase());
                    i = end;
                    compound_start = false;
                }
                _ => i += 1,
            }
        }
        if !compound_start {
            self.flush(c);
        }
        if std::env::var_os("FEATHER_STYLE_DEBUG").is_some() {
            eprintln!("style cache: :has({arg}) -> any {} state_any {:?}", self.any, self.state_any);
        }
    }

    fn element_has(names: &HashSet<String>, doc: &Document, id: NodeId) -> bool {
        let Some(e) = doc.element(id) else { return false };
        if names.contains(&*e.name.local) {
            return true;
        }
        e.attrs.iter().any(|a| {
            let name = &*a.name.local;
            names.contains(name)
                || names.contains(&format!("{name}={}", a.value))
                || match name {
                    "class" => a.value.split_ascii_whitespace().any(|c| names.contains(c)),
                    "id" => names.contains(&*a.value),
                    _ => false,
                }
        })
    }

    /// Might a change to this element (as it is in `doc`) change a `:has()`?
    fn matters(&self, doc: &Document, id: NodeId) -> bool {
        self.any || Self::element_has(&self.names, doc, id)
    }

    /// Might this element's hover/active (`kind` 0) or focus/target/checked
    /// (1) state changing change a `:has()`?
    fn state_matters(&self, kind: usize, doc: &Document, id: NodeId) -> bool {
        self.state_any[kind] || Self::element_has(&self.state_names[kind], doc, id)
    }
}

/// What a set of styles was computed with: reusable only for the same.
#[derive(Clone)]
struct Key {
    data: usize,
    env: (u32, u32, bool, bool),
    quirks: bool,
}

impl Key {
    fn of(data: &StyleData, env: &MediaEnv, doc: &Document) -> Self {
        Self { data: data.identity(), env: (env.width.to_bits(), env.height.to_bits(), env.dark, env.scripting), quirks: doc.quirks }
    }

    fn same(&self, other: &Key) -> bool {
        self.data == other.data && self.env == other.env && self.quirks == other.quirks
    }
}

struct Entry {
    parent: Arc<ComputedStyle>,
    style: Arc<ComputedStyle>,
}

/// The styles of a page's last build, for its next one.
#[derive(Default)]
pub struct StyleCache {
    last: Option<Built>,
}

struct Built {
    key: Key,
    doc: Arc<Document>,
    state: Arc<ElementState>,
    styles: HashMap<NodeId, Entry>,
}

/// One build's use of the cache: what may be reused, and what was computed
/// (the next build's cache).
pub struct StyleSession {
    old: Option<Built>,
    /// Nodes whose subtrees must be computed afresh.
    dirty: HashSet<NodeId>,
    /// The root's style changed (`rem`s with it): nothing is reused.
    off: bool,
    /// Why nothing could be reused, if so.
    pub why: &'static str,
    key: Key,
    doc: Arc<Document>,
    state: Arc<ElementState>,
    styles: HashMap<NodeId, Entry>,
    /// Parent styles seen this build, shared by their children's entries.
    parents: Vec<Arc<ComputedStyle>>,
    pub reused: usize,
    pub computed: usize,
    /// FEATHER_STYLE_CHECK: compute reused styles too, and say where they differ.
    check: bool,
}

impl StyleCache {
    /// Start a build of `doc`: what of the last one can be reused.
    pub fn begin(&mut self, data: &StyleData, env: &MediaEnv, doc: Arc<Document>, state: Arc<ElementState>) -> StyleSession {
        let key = Key::of(data, env, &doc);
        let last = self.last.take();
        let old = last.filter(|b| b.key.same(&key) && b.doc.root() == doc.root());
        let dirty = match &old {
            Some(b) => invalidated(&b.doc, &doc, &b.state, &state, data),
            None => Err("a new page, stylesheets or viewport"),
        };
        let (old, dirty, why) = match dirty {
            Ok(d) => (old, d, ""),
            Err(why) => (None, HashSet::new(), why),
        };
        StyleSession {
            old,
            dirty,
            why,
            off: false,
            key,
            doc,
            state,
            styles: HashMap::new(),
            parents: Vec::new(),
            reused: 0,
            computed: 0,
            check: std::env::var_os("FEATHER_STYLE_CHECK").is_some(),
        }
    }

    /// Keep a finished build's styles for the next.
    pub fn finish(&mut self, session: StyleSession) {
        self.last = Some(Built { key: session.key, doc: session.doc, state: session.state, styles: session.styles });
    }

    /// The elements to restyle going from `installed` (the document the
    /// shown layout was built from, and the last build's) to `doc`, when
    /// all that changed is a few elements' attributes of the kind that only
    /// restyle (`style`, `class`, `aria-*`, ...): same tree, same text, same
    /// interaction state, viewport and stylesheets. In document order;
    /// `None` if there's more to it (or more than `limit` elements).
    /// Scripts that animate by setting styles (a video's progress bar, many
    /// times a second) are restyled this way, not as a whole page.
    pub fn attribute_restyle(&self, data: &StyleData, env: &MediaEnv, installed: &Arc<Document>, doc: &Arc<Document>, state: &Arc<ElementState>, limit: usize) -> Option<Vec<NodeId>> {
        let debug = std::env::var_os("FEATHER_STYLE_DEBUG").is_some();
        let last = self.last.as_ref()?;
        if !Arc::ptr_eq(&last.doc, installed) || !last.key.same(&Key::of(data, env, doc)) || *last.state != **state || last.doc.root() != doc.root() {
            if debug {
                eprintln!("attribute restyle: not the last build's document, styles or state ({} {} {})", Arc::ptr_eq(&last.doc, installed), last.key.same(&Key::of(data, env, doc)), *last.state == **state);
            }
            return None;
        }
        if let Err(why) = same_but_restyling_attributes(&last.doc, doc) {
            if debug {
                eprintln!("attribute restyle: {why}");
            }
            return None;
        }
        let dirty = invalidated(&last.doc, doc, &last.state, state, data).ok()?;
        if dirty.len() > limit * 2 {
            return None;
        }
        let elements: Vec<NodeId> = doc.descendants(doc.root()).filter(|n| dirty.contains(n) && doc.element(*n).is_some()).collect();
        (elements.len() <= limit).then_some(elements)
    }

    /// After [`StyleCache::attribute_restyle`]'s elements were restyled
    /// (and the layout patched): `doc` is the last build's document, with
    /// these elements' styles (each with its parent's).
    pub fn advance(&mut self, doc: Arc<Document>, styles: Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)>) {
        if let Some(last) = &mut self.last {
            last.doc = doc;
            for (id, parent, style) in styles {
                last.styles.insert(id, Entry { parent, style });
            }
        }
    }
}

/// Attributes whose changes act on the page only through styles (selectors
/// and the `style` attribute), never through what boxes are built.
fn restyles_only(name: &str) -> bool {
    matches!(name, "style" | "class" | "role" | "tabindex" | "hidden" | "title" | "jsaction" | "jslog" | "jscontroller" | "jsname" | "jsmodel" | "jsdata")
        || name.starts_with("aria-")
        || name.starts_with("data-")
}

/// Whether `new` is `old` with only [`restyles_only`] attributes changed,
/// and none on elements whose attributes say what they show (images,
/// media, frames, form controls, SVG).
fn same_but_restyling_attributes(old: &Document, new: &Document) -> Result<(), String> {
    let tree_of = |doc: &Document, n: NodeId| doc.shadow.as_ref().and_then(|s| s.tree.get(&n).copied());
    // A `<style>` or `<script>` given the same text again (new text nodes,
    // same contents): nothing changed. (Different CSS changes the style
    // key, so it never gets here.)
    let text_of = |doc: &Document, n: NodeId| -> String { doc.children(n).iter().filter_map(|&c| doc.get(c).and_then(|c| c.text())).collect() };
    let opaque = |doc: &Document, n: NodeId| doc.element(n).is_some_and(|e| e.is_html() && matches!(&*e.name.local, "style" | "script"));
    let mut stack = vec![new.root()];
    while let Some(n) = stack.pop() {
        let (Some(a), Some(b)) = (old.get(n), new.get(n)) else { return Err("a node added".into()) };
        if old.parent(n) != new.parent(n) || tree_of(old, n) != tree_of(new, n) {
            return Err("a node moved".into());
        }
        if opaque(new, n) && old.children(n) != new.children(n) {
            if text_of(old, n) != text_of(new, n) {
                return Err(format!("<{}> text changed", new.element(n).map(|e| e.name.local.to_string()).unwrap_or_default()));
            }
        } else {
            if old.children(n) != new.children(n) {
                return Err(format!("children changed in <{}>", new.element(n).map(|e| e.name.local.to_string()).unwrap_or_default()));
            }
            stack.extend(new.children(n).iter().copied());
        }
        match (old.element(n), new.element(n)) {
            (Some(x), Some(y)) => {
                if x.name != y.name {
                    return Err("an element renamed".into());
                }
                if x.attrs != y.attrs {
                    let local = &*y.name.local;
                    let shows = matches!(local, "img" | "picture" | "source" | "video" | "audio" | "iframe" | "frame" | "object" | "embed" | "canvas" | "input" | "textarea" | "select" | "option" | "button" | "progress" | "meter" | "slot" | "template")
                        || !y.is_html();
                    if shows {
                        return Err(format!("attributes of <{}>", y.name.local));
                    }
                    let differ = |p: &[page::Attribute], q: &[page::Attribute]| {
                        p.iter().filter(|at| !q.iter().any(|bt| bt.name == at.name && bt.value == at.value)).find(|at| !restyles_only(&at.name.local)).map(|at| at.name.local.to_string())
                    };
                    if let Some(name) = differ(&x.attrs, &y.attrs).or_else(|| differ(&y.attrs, &x.attrs)) {
                        return Err(format!("{name}= on <{}>", y.name.local));
                    }
                }
            }
            (None, None) => {
                if a.text() != b.text() {
                    return Err(format!("text changed in <{}>", new.parent(n).and_then(|p| new.element(p)).map(|e| e.name.local.to_string()).unwrap_or_default()));
                }
            }
            _ => return Err("a node replaced".into()),
        }
    }
    // (Every node's children are the same lists, from the root down: no
    // node was removed either, but in a `<style>` or `<script>`.)
    Ok(())
}

impl StyleSession {
    /// `id`'s style given its parent's: the last build's if nothing it
    /// depends on changed, else `compute()`'s.
    pub fn style(&mut self, doc: &Document, id: NodeId, parent: &ComputedStyle, compute: impl FnOnce() -> ComputedStyle) -> ComputedStyle {
        let root = doc.parent(id).is_some_and(|p| doc.element(p).is_none()) && doc.element(id).is_some();
        let reusable = !self.off && !root && !self.dirty.contains(&id);
        let hit = reusable
            .then(|| self.old.as_ref()?.styles.get(&id))
            .flatten()
            .filter(|e| *e.parent == *parent)
            .map(|e| (e.parent.clone(), e.style.clone()));
        let (parent_arc, style) = match hit {
            Some((p, s)) => {
                self.reused += 1;
                if self.check {
                    let fresh = compute();
                    if fresh != *s {
                        let e = doc.element(id);
                        eprintln!(
                            "style cache: stale style for {:?} <{}> class={:?}",
                            id,
                            e.map(|e| e.name.local.to_string()).unwrap_or_default(),
                            e.and_then(|e| e.attr("class")).unwrap_or("")
                        );
                    }
                }
                (p, s)
            }
            None => {
                self.computed += 1;
                let style = Arc::new(compute());
                if root {
                    // A different root font size changes every `rem`.
                    let old_root = self.old.as_ref().and_then(|b| b.styles.get(&id)).map(|e| e.style.clone());
                    if old_root.is_none_or(|o| *o != *style) {
                        self.off = true;
                        if self.why.is_empty() {
                            self.why = "the root's style changed";
                        }
                    }
                }
                (self.parent_arc(parent), style)
            }
        };
        let out = (*style).clone();
        self.styles.insert(id, Entry { parent: parent_arc, style });
        out
    }

    /// The parent style as an `Arc` shared by its children's entries.
    fn parent_arc(&mut self, parent: &ComputedStyle) -> Arc<ComputedStyle> {
        // Siblings come one after another (with their subtrees between):
        // the last few parents seen cover nearly every lookup.
        if let Some(p) = self.parents.iter().rev().take(8).find(|p| ***p == *parent) {
            return p.clone();
        }
        let p = Arc::new(parent.clone());
        self.parents.push(p.clone());
        p
    }
}

/// The nodes whose subtrees must be restyled going from `old` to `new`, or
/// `None` if everything must.
fn invalidated(old: &Document, new: &Document, old_state: &ElementState, new_state: &ElementState, data: &StyleData) -> Result<HashSet<NodeId>, &'static str> {
    let has = data.has_keys();
    let tree_of = |doc: &Document, n: NodeId| doc.shadow.as_ref().and_then(|s| s.tree.get(&n).copied());
    let element_children = |doc: &Document, n: NodeId| -> Vec<NodeId> { doc.children(n).iter().copied().filter(|&c| doc.element(c).is_some()).collect() };
    let is_empty = |doc: &Document, n: NodeId| doc.children(n).iter().all(|&c| doc.element(c).is_none() && doc.get(c).and_then(|c| c.text()).is_none_or(str::is_empty));

    let mut changed: Vec<NodeId> = Vec::new();
    for n in new.descendants(new.root()) {
        let Some(e) = new.element(n) else { continue };
        let same = old.element(n).is_some_and(|o| {
            o.name == e.name
                && o.attrs == e.attrs
                && old.parent(n) == new.parent(n)
                && tree_of(old, n) == tree_of(new, n)
                && is_empty(old, n) == is_empty(new, n)
        });
        if !same {
            if has.as_ref().is_some_and(|h| h.matters(new, n) || old.element(n).is_some() && h.matters(old, n)) {
                if std::env::var_os("FEATHER_STYLE_DEBUG").is_some() {
                    let h = has.as_ref().expect("checked");
                    let e = new.element(n).expect("an element");
                    let hit: Vec<&String> = h.names.iter().filter(|k| *k == &*e.name.local || e.attrs.iter().any(|a| &*a.name.local == k.as_str() || a.value.split_ascii_whitespace().any(|c| c == k.as_str()))).collect();
                    eprintln!("style cache: :has() key {hit:?} (any {}) on <{}> {:?}", h.any, e.name.local, e.attr("class"));
                }
                return Err("a change :has() looks for");
            }
            changed.push(n);
            continue;
        }
        // Children added, removed or reordered: each one's place among its
        // siblings may have changed.
        if element_children(old, n) != element_children(new, n) {
            changed.extend(element_children(new, n));
        }
    }
    // Removed elements: their parents' children changed (handled above);
    // a `:has()` may have matched them.
    if let Some(h) = &has {
        for n in old.descendants(old.root()) {
            if old.element(n).is_some() && new.element(n).is_none() && h.matters(old, n) {
                return Err("a removal :has() looks for");
            }
        }
    }
    // Interaction state: the elements whose state changed.
    if old_state != new_state {
        let from = changed.len();
        let diff = |a: &HashSet<NodeId>, b: &HashSet<NodeId>| a.symmetric_difference(b).copied().collect::<Vec<_>>();
        changed.extend(diff(&old_state.hovered, &new_state.hovered));
        changed.extend(diff(&old_state.active, &new_state.active));
        let hover_changed = changed.len();
        if old_state.focused != new_state.focused {
            // `:focus-within`: the focused element's ancestors too.
            for f in [old_state.focused, new_state.focused].into_iter().flatten() {
                let mut at = Some(f);
                while let Some(n) = at {
                    changed.push(n);
                    at = new.parent(n);
                }
            }
        }
        changed.extend([old_state.target, new_state.target].into_iter().flatten().filter(|_| old_state.target != new_state.target));
        if old_state.checked != new_state.checked {
            let empty = HashSet::new();
            let a = old_state.checked.as_deref().unwrap_or(&empty);
            let b = new_state.checked.as_deref().unwrap_or(&empty);
            // Before the user touched any control, attributes decided.
            if old_state.checked.is_none() || new_state.checked.is_none() {
                return Err("checked state");
            }
            changed.extend(diff(a, b));
        }
        if let Some(h) = &has
            && (changed[from..hover_changed].iter().any(|&n| h.state_matters(0, new, n))
                || changed[hover_changed..].iter().any(|&n| h.state_matters(1, new, n)))
        {
            return Err("interaction state :has() looks at");
        }
    }
    // A changed element restyles its subtree (descendant combinators) and
    // its later siblings' (`+`, `~`).
    let mut roots = HashSet::new();
    for n in changed {
        if new.get(n).is_none() || !roots.insert(n) {
            continue;
        }
        if let Some(p) = new.parent(n) {
            let siblings = new.children(p);
            if let Some(ix) = siblings.iter().position(|&c| c == n) {
                roots.extend(siblings[ix + 1..].iter().copied().filter(|&c| new.element(c).is_some()));
            }
        }
    }
    let mut dirty = HashSet::new();
    let mut stack = vec![(new.root(), false)];
    while let Some((n, inside)) = stack.pop() {
        let inside = inside || roots.contains(&n);
        if inside {
            dirty.insert(n);
        }
        stack.extend(new.children(n).iter().map(|&c| (c, inside)));
    }
    Ok(dirty)
}

#[cfg(test)]
mod tests {
    use super::HasKeys;

    #[test]
    fn has_keys_name_what_has_arguments_match() {
        let mut k = HasKeys::default();
        k.add(".host:has(.video-playing) .scrim");
        k.add("dialog[modern]:has(ytd-text-inline-expander[hidden])");
        k.add("#c:has(>[role=dialog]:not([aria-hidden=true]) .x)");
        k.add(".p:has(> .ytp-mute-button[style*=\"display: none\"])");
        assert!(k.names.contains("video-playing"));
        assert!(k.names.contains("ytd-text-inline-expander"));
        assert!(!k.names.contains("hidden"), "the tag says enough");
        assert!(k.names.contains("role=dialog"));
        assert!(!k.names.contains("aria-hidden"), "inside :not() of a keyed compound");
        assert!(k.names.contains("x"));
        assert!(k.names.contains("ytp-mute-button"));
        assert!(!k.names.contains("style"));
        assert!(!k.names.contains("host"));
        assert!(!k.any && k.state_any == [false, false]);
        k.add(".a:has(.b:hover)");
        assert!(k.state_names[0].contains("b") && k.state_any == [false, false]);
        k.add("a:has(:focus-visible)");
        assert!(k.state_any == [false, true] && !k.any);
        k.add("a:has(> :first-child)");
        assert!(k.any);
    }
}

#[cfg(test)]
mod reuse_tests {
    use std::sync::Arc;

    use page::{Document, NodeId};

    use crate::{ComputedStyle, ElementState, MediaEnv, StyleCache, StyleData};

    const PAGE: &str = r#"<html><head><style>
        .on { color: rgb(255, 0, 0) }
        .on .kid { color: rgb(0, 128, 0) }
        .a + .b { color: rgb(0, 0, 255) }
        li:last-child { color: rgb(1, 2, 3) }
        .box:has(.flag) .inner { color: rgb(9, 9, 9) }
        </style></head><body>
        <div id="p"><span class="kid" id="k">x</span></div>
        <p id="a">a</p><p class="b" id="b">b</p>
        <ul id="l"><li id="l1">1</li><li id="l2">2</li></ul>
        <div class="box"><div class="inner" id="i">i</div><div id="f"></div></div>
        </body></html>"#;

    fn find(doc: &Document, id: &str) -> NodeId {
        doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(id)).expect(id)
    }

    fn set_class(doc: &mut Document, id: &str, class: &str) {
        let n = find(doc, id);
        let e = doc.element_mut(n).expect("element");
        e.attrs.retain(|a| &*a.name.local != "class");
        e.attrs.push(page::Attribute { name: page::markup5ever::QualName::new(None, page::markup5ever::ns!(), "class".into()), value: class.into() });
    }

    /// Every element's style, top down: through the cache and fresh.
    fn styles(doc: &Arc<Document>, data: &StyleData, cache: &mut StyleCache) -> Vec<(NodeId, ComputedStyle, ComputedStyle)> {
        let env = MediaEnv::default();
        let state = Arc::new(ElementState::default());
        let styler = data.styler_with_state(env, state.clone());
        let mut session = cache.begin(data, &env, doc.clone(), state);
        let mut out = Vec::new();
        let mut stack = vec![(doc.root(), ComputedStyle::root_parent())];
        while let Some((n, parent)) = stack.pop() {
            for &c in doc.children(n).iter().rev() {
                if doc.element(c).is_none() {
                    continue;
                }
                let cached = session.style(doc, c, &parent, || styler.compute(doc, c, &parent));
                let fresh = styler.compute(doc, c, &parent);
                out.push((c, cached.clone(), fresh));
                stack.push((c, cached));
            }
        }
        cache.finish(session);
        out
    }

    #[test]
    fn reused_styles_match_fresh_ones_after_changes() {
        let url = url::Url::parse("https://example.com/").unwrap();
        let mut doc = html::parse(PAGE, url);
        let data = StyleData::for_document(&doc, &|_| None);
        let mut cache = StyleCache::default();
        styles(&Arc::new(doc.clone()), &data, &mut cache);
        // An ancestor's class, a sibling's, a new last child, a :has() flag.
        set_class(&mut doc, "p", "on");
        set_class(&mut doc, "a", "a");
        let li = doc.create_element(page::markup5ever::QualName::new(None, page::markup5ever::ns!(html), "li".into()), vec![]);
        let list = find(&doc, "l");
        doc.append(list, li);
        set_class(&mut doc, "f", "flag");
        let doc = Arc::new(doc);
        let all = styles(&doc, &data, &mut cache);
        for (n, cached, fresh) in &all {
            assert!(cached == fresh, "stale style for {:?} {:?}", n, doc.element(*n).and_then(|e| e.attr("id")));
        }
        let color = |id: &str| all.iter().find(|(n, ..)| *n == find(&doc, id)).map(|(_, s, _)| s.color).expect(id);
        assert_eq!(color("k"), crate::Color::Rgba(0x008000ff));
        assert_eq!(color("b"), crate::Color::Rgba(0x0000ffff));
        assert_ne!(color("l2"), crate::Color::Rgba(0x010203ff), "no longer the last child");
        assert_eq!(color("i"), crate::Color::Rgba(0x090909ff));
        // Nothing changed: everything reused.
        let mut again = StyleCache::default();
        styles(&doc, &data, &mut again);
        let env = MediaEnv::default();
        let session = again.begin(&data, &env, doc.clone(), Arc::new(ElementState::default()));
        assert!(session.why.is_empty());
    }
}
