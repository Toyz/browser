//! Snappy `:hover`: patch paint into the existing layout instead of
//! restyling the page.
//!
//! Almost every hover effect on the web changes only how things are painted
//! (a color, a background, an underline), not where they are. So when the
//! interaction state changes, restyle just the elements entering or leaving
//! it (and their descendants, for `.card:hover .title`), and if only paint
//! properties changed, copy those into the layout tree we already have. No
//! box building, no line breaking, no re-measuring: a few milliseconds on the
//! UI thread instead of a full restyle in the background.
//!
//! Anything else (sizes, display, `::before` content) returns `None`, and
//! the caller does a full restyle.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use page::{Document, NodeId};
use style::{ComputedStyle, Display, ElementState, Pseudo, Styler};

use crate::boxes::{Block, InlineObject, LayoutBox, LayoutTree, Paragraph, Row};

/// Restyling more elements than this isn't "a hover" any more.
const MAX_ELEMENTS: usize = 4000;
/// When state rules reach descendants (`.group:hover .x`), follow them only
/// into elements this small; bigger ones (the page's `<main>`, which the
/// pointer leaves all the time) are left to the background restyle.
const MAX_DESCENDANTS: usize = 400;

/// The result of trying to patch a layout for a new interaction state.
pub enum Patch {
    /// Nothing visible changed.
    Unchanged,
    /// Paint changed: this is the new layout (same geometry, so no
    /// re-measuring), and which elements changed (before, after).
    Painted(LayoutTree, Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)>),
    /// Something that affects layout changed: restyle the page (why).
    NeedsRestyle(&'static str),
}

/// The paint-only properties: changing these can't move anything.
fn copy_paint(to: &mut ComputedStyle, from: &ComputedStyle) {
    to.color = from.color;
    to.background = from.background;
    to.background_image = from.background_image;
    to.gradients = from.gradients.clone();
    to.background_repeat = from.background_repeat;
    to.background_position = from.background_position;
    to.background_size = from.background_size;
    to.bg_clip_border = from.bg_clip_border.clone();
    to.text_gradient = from.text_gradient.clone();
    to.border_color = from.border_color;
    to.border_radius = from.border_radius;
    to.opacity = from.opacity;
    to.translate = from.translate;
    to.rotate = from.rotate;
    to.scale = from.scale;
    to.shadows = from.shadows.clone();
    to.underline = from.underline;
    to.strikethrough = from.strikethrough;
    to.visible = from.visible;
    to.cursor = from.cursor;
    to.custom = from.custom.clone();
}

/// Elements whose state differs between `a` and `b`.
fn changed_roots(a: &ElementState, b: &ElementState) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = a.hovered.symmetric_difference(&b.hovered).copied().collect();
    out.extend(a.active.symmetric_difference(&b.active));
    if a.focused != b.focused {
        out.extend(a.focused.into_iter().chain(b.focused));
    }
    if a.target != b.target {
        out.extend(a.target.into_iter().chain(b.target));
    }
    if a.checked != b.checked {
        let empty = Default::default();
        let (x, y) = (a.checked.as_deref().unwrap_or(&empty), b.checked.as_deref().unwrap_or(&empty));
        // Without explicit state, the attributes decided: treat every
        // control in the other set as possibly changed.
        if a.checked.is_none() || b.checked.is_none() {
            out.extend(x.union(y));
        } else {
            out.extend(x.symmetric_difference(y));
        }
    }
    out
}

/// Computed styles along the way from the root, memoized.
struct Styles<'a> {
    doc: &'a Document,
    styler: &'a Styler,
    memo: HashMap<NodeId, Arc<ComputedStyle>>,
    root_parent: Arc<ComputedStyle>,
}

impl<'a> Styles<'a> {
    fn new(doc: &'a Document, styler: &'a Styler) -> Self {
        Self { doc, styler, memo: HashMap::new(), root_parent: Arc::new(ComputedStyle::root_parent()) }
    }

    fn get(&mut self, id: NodeId) -> Arc<ComputedStyle> {
        if let Some(s) = self.memo.get(&id) {
            return s.clone();
        }
        let parent = match self.doc.parent(id) {
            Some(p) if self.doc.element(p).is_some() => self.get(p),
            _ => self.root_parent.clone(),
        };
        let style = Arc::new(self.styler.compute(self.doc, id, &parent));
        self.memo.insert(id, style.clone());
        style
    }
}

/// Patch `layout` (styled with `old`) for the interaction state `new`.
/// `styler` is reused between calls (its state is swapped as needed).
/// `reaches_others`: the page has rules where state affects other elements
/// (see `StyleData::state_reaches_others`); then descendants restyle too.
pub fn patch(
    layout: &LayoutTree,
    doc: &Document,
    styler: &mut Styler,
    old: &Arc<ElementState>,
    new: &Arc<ElementState>,
    reaches_others: bool,
) -> Patch {
    let roots: Vec<NodeId> = changed_roots(old, new).into_iter().filter(|&n| doc.element(n).is_some()).collect();
    if roots.is_empty() {
        return Patch::Unchanged;
    }
    // Everything that might restyle: the elements themselves and everything inside.
    let mut affected: Vec<NodeId> = Vec::new();
    let mut seen = HashSet::new();
    for &root in &roots {
        let subtree: Vec<NodeId> = if reaches_others {
            let all: Vec<NodeId> = doc.descendants(root).filter(|&n| doc.element(n).is_some()).take(MAX_DESCENDANTS + 1).collect();
            if all.len() > MAX_DESCENDANTS { vec![root] } else { all }
        } else {
            vec![root]
        };
        for id in subtree {
            if seen.insert(id) {
                affected.push(id);
            }
        }
        if affected.len() > MAX_ELEMENTS {
            return Patch::NeedsRestyle("too many elements");
        }
    }

    styler.set_state(old.clone());
    let (before, before_pseudo) = {
        let mut styles = Styles::new(doc, styler);
        let before: Vec<_> = affected.iter().map(|&id| styles.get(id)).collect();
        let pseudo = pseudos(&roots, &mut styles);
        (before, pseudo)
    };
    styler.set_state(new.clone());
    let mut styles = Styles::new(doc, styler);
    let mut changes: HashMap<NodeId, Arc<ComputedStyle>> = HashMap::new();
    let mut changed: Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)> = Vec::new();
    // Blocks gaining or losing a background: fine for a box, but a container
    // that was split into rows is drawn differently with one (as a wrapper).
    let mut appearing: Vec<NodeId> = Vec::new();
    for (&id, old_style) in affected.iter().zip(&before) {
        let new_style = styles.get(id);
        if *new_style == **old_style {
            continue;
        }
        let mut probe = (*new_style).clone();
        copy_paint(&mut probe, old_style);
        if probe != **old_style {
            if std::env::var_os("BROWSER_DEBUG").is_some() {
                eprintln!("hover: layout change on {:?}: {}", doc.element(id).map(|e| e.name.local.to_string()), style_diff(old_style, &probe));
            }
            return Patch::NeedsRestyle("layout property changed");
        }
        // Inline boxes' backgrounds and borders are baked into their
        // paragraph's decorations (which don't know their element).
        if new_style.display == Display::Inline
            && (new_style.painted_background() != old_style.painted_background()
                || (new_style.has_border() && new_style.border_color != old_style.border_color))
        {
            return Patch::NeedsRestyle("inline box background");
        }
        if new_style.painted_background().is_some() != old_style.painted_background().is_some() {
            appearing.push(id);
        }
        changed.push((id, old_style.clone(), new_style.clone()));
        changes.insert(id, new_style);
    }
    if !appearing.is_empty() {
        let mut boxes = HashSet::new();
        for row in &layout.rows {
            box_nodes(&row.content, &mut boxes);
            row.absolutes.iter().for_each(|b| box_nodes(b, &mut boxes));
        }
        layout.fixed.iter().for_each(|b| box_nodes(b, &mut boxes));
        if appearing.iter().any(|n| !boxes.contains(n)) {
            return Patch::NeedsRestyle("background on a split container");
        }
    }
    // `a:hover::after { content: "→" }` and friends.
    if pseudos(&roots, &mut styles) != before_pseudo {
        return Patch::NeedsRestyle("::before/::after changed");
    }
    if changes.is_empty() {
        return Patch::Unchanged;
    }

    match apply(layout, &changes) {
        Some(tree) => Patch::Painted(tree, changed),
        None => Patch::Unchanged,
    }
}

/// Copy these elements' paint properties into the layout (`None`: none of
/// them are in it). Hover patches and animation frames both come through here.
pub fn apply(layout: &LayoutTree, changes: &HashMap<NodeId, Arc<ComputedStyle>>) -> Option<LayoutTree> {
    let mut tree = layout.clone();
    let mut touched = false;
    for row in &mut tree.rows {
        if row_mentions(row, changes) {
            patch_row(row, changes);
            touched = true;
        }
    }
    for b in &mut tree.fixed {
        if mentions(b, changes) {
            patch_box(b, changes);
            touched = true;
        }
    }
    touched.then_some(tree)
}

/// Frames of an animation of custom properties on `node`: its style at
/// `samples + 1` evenly spaced points, each a restyle with the properties
/// interpolated (`--angle: 0deg` to `360deg` turns a conic gradient).
pub fn custom_property_frames(
    doc: &Document,
    styler: &Styler,
    node: NodeId,
    keyframes: &[(f32, Vec<(Arc<str>, Arc<str>)>)],
    samples: usize,
) -> Vec<(f32, ComputedStyle)> {
    let mut styles = Styles::new(doc, styler);
    let parent = match doc.parent(node) {
        Some(p) if doc.element(p).is_some() => styles.get(p),
        _ => styles.root_parent.clone(),
    };
    let names: Vec<Arc<str>> = {
        let mut v: Vec<Arc<str>> = keyframes.iter().flat_map(|(_, c)| c.iter().map(|(n, _)| n.clone())).collect();
        v.sort();
        v.dedup();
        v
    };
    // `12.5deg` -> (12.5, "deg").
    let number = |v: &str| -> Option<(f32, String)> {
        let v = v.trim();
        let end = v.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(v.len());
        Some((v[..end].parse().ok()?, v[end..].to_owned()))
    };
    (0..=samples)
        .map(|k| {
            let p = k as f32 / samples.max(1) as f32;
            let overrides: Vec<(Arc<str>, Arc<str>)> = names
                .iter()
                .filter_map(|name| {
                    let at: Vec<(f32, &Arc<str>)> =
                        keyframes.iter().filter_map(|(o, c)| c.iter().find(|(n, _)| n == name).map(|(_, v)| (*o, v))).collect();
                    let after = at.iter().position(|(o, _)| *o >= p).unwrap_or(at.len() - 1);
                    let (o1, v1) = at[after];
                    let (o0, v0) = if after > 0 { at[after - 1] } else { at[after] };
                    let value = match (number(v0), number(v1)) {
                        (Some((a, ua)), Some((b, ub))) if ua == ub && o1 > o0 => {
                            let t = ((p - o0) / (o1 - o0)).clamp(0.0, 1.0);
                            format!("{}{ua}", a + (b - a) * t).into()
                        }
                        _ if p < (o0 + o1) / 2.0 => v0.clone(),
                        _ => v1.clone(),
                    };
                    Some((name.clone(), value))
                })
                .collect();
            (p, styler.compute_with_custom(doc, node, &parent, &overrides))
        })
        .collect()
}

/// Every element in the layout with CSS animations, and its style.
pub fn animated_elements(layout: &LayoutTree) -> Vec<(NodeId, Arc<ComputedStyle>)> {
    fn walk(b: &LayoutBox, out: &mut Vec<(NodeId, Arc<ComputedStyle>)>) {
        match b {
            LayoutBox::Block(block) => {
                if let Some(n) = block.node
                    && block.style.animations.is_some()
                {
                    out.push((n, block.style.clone()));
                }
                block.children.iter().for_each(|c| walk(c, out));
            }
            LayoutBox::Paragraph(p) => {
                for o in &p.objects {
                    match o {
                        InlineObject::Block(a) => {
                            if let Some(n) = a.node
                                && a.style.animations.is_some()
                            {
                                out.push((n, a.style.clone()));
                            }
                        }
                        InlineObject::BlockBox(b) => walk(&LayoutBox::Block((**b).clone()), out),
                        InlineObject::Image(_) => {}
                    }
                }
            }
            LayoutBox::Image(_) => {}
        }
    }
    let mut out = Vec::new();
    for row in &layout.rows {
        for w in row.wrappers.iter() {
            if let Some(n) = w.node
                && w.style.animations.is_some()
                && !out.iter().any(|(m, _)| *m == n)
            {
                out.push((n, w.style.clone()));
            }
        }
        walk(&row.content, &mut out);
        row.absolutes.iter().for_each(|b| walk(b, &mut out));
    }
    layout.fixed.iter().for_each(|b| walk(b, &mut out));
    out
}

fn pseudos(roots: &[NodeId], styles: &mut Styles) -> Vec<Option<ComputedStyle>> {
    let mut out = Vec::new();
    for &id in roots {
        let style = styles.get(id);
        for which in [Pseudo::Before, Pseudo::After] {
            out.push(styles.styler.compute_pseudo(styles.doc, id, &style, which));
        }
    }
    out
}

type Changes = HashMap<NodeId, Arc<ComputedStyle>>;

/// Which fields differ (debugging aid).
fn style_diff(a: &ComputedStyle, b: &ComputedStyle) -> String {
    let (a, b) = (format!("{a:?}"), format!("{b:?}"));
    let fields = |s: &str| s.split(", ").map(str::to_owned).collect::<Vec<_>>();
    let (fa, fb) = (fields(&a), fields(&b));
    fa.iter().zip(&fb).filter(|(x, y)| x != y).map(|(x, y)| format!("{x} -> {y}")).take(4).collect::<Vec<_>>().join("; ")
}

/// Elements that are boxes of their own in the layout (not dissolved into rows).
fn box_nodes(b: &LayoutBox, out: &mut HashSet<NodeId>) {
    match b {
        LayoutBox::Block(block) => block_nodes(block, out),
        LayoutBox::Paragraph(p) => para_nodes(p, out),
        LayoutBox::Image(_) => {}
    }
}

fn block_nodes(b: &Block, out: &mut HashSet<NodeId>) {
    out.extend(b.node);
    b.children.iter().for_each(|c| box_nodes(c, out));
}

fn para_nodes(p: &Paragraph, out: &mut HashSet<NodeId>) {
    for o in &p.objects {
        match o {
            InlineObject::Image(_) => {}
            InlineObject::Block(a) => {
                out.extend(a.node);
                if let Some(c) = &a.content {
                    para_nodes(c, out);
                }
            }
            InlineObject::BlockBox(b) => block_nodes(b, out),
        }
    }
}

fn patched(style: &Arc<ComputedStyle>, new: &ComputedStyle) -> Arc<ComputedStyle> {
    let mut s = (**style).clone();
    copy_paint(&mut s, new);
    Arc::new(s)
}

fn row_mentions(row: &Row, ch: &Changes) -> bool {
    row.wrappers.iter().any(|w| w.node.is_some_and(|n| ch.contains_key(&n)))
        || mentions(&row.content, ch)
        || row.absolutes.iter().any(|b| mentions(b, ch))
}

fn patch_row(row: &mut Row, ch: &Changes) {
    if row.wrappers.iter().any(|w| w.node.is_some_and(|n| ch.contains_key(&n))) {
        let mut wrappers = row.wrappers.to_vec();
        for w in &mut wrappers {
            if let Some(new) = w.node.and_then(|n| ch.get(&n)) {
                w.style = patched(&w.style, new);
            }
        }
        row.wrappers = wrappers.into();
    }
    patch_box(&mut row.content, ch);
    for b in &mut row.absolutes {
        patch_box(b, ch);
    }
}

fn mentions(b: &LayoutBox, ch: &Changes) -> bool {
    match b {
        LayoutBox::Block(block) => block_mentions(block, ch),
        LayoutBox::Paragraph(p) => para_mentions(p, ch),
        LayoutBox::Image(_) => false,
    }
}

fn block_mentions(b: &Block, ch: &Changes) -> bool {
    b.node.is_some_and(|n| ch.contains_key(&n)) || b.children.iter().any(|c| mentions(c, ch))
}

fn para_mentions(p: &Paragraph, ch: &Changes) -> bool {
    p.spans.iter().any(|s| s.style.node.is_some_and(|n| ch.contains_key(&n)))
        || p.objects.iter().any(|o| match o {
            InlineObject::Image(_) => false,
            InlineObject::Block(a) => {
                a.node.is_some_and(|n| ch.contains_key(&n)) || a.content.as_ref().is_some_and(|c| para_mentions(c, ch))
            }
            InlineObject::BlockBox(b) => block_mentions(b, ch),
        })
}

fn patch_box(b: &mut LayoutBox, ch: &Changes) {
    match b {
        LayoutBox::Block(block) => patch_block(block, ch),
        LayoutBox::Paragraph(p) => {
            if para_mentions(p, ch) {
                patch_para(Arc::make_mut(p), ch);
            }
        }
        LayoutBox::Image(_) => {}
    }
}

fn patch_block(b: &mut Block, ch: &Changes) {
    if let Some(new) = b.node.and_then(|n| ch.get(&n)) {
        b.style = patched(&b.style, new);
    }
    for c in &mut b.children {
        if mentions(c, ch) {
            patch_box(c, ch);
        }
    }
}

fn patch_para(p: &mut Paragraph, ch: &Changes) {
    for span in &mut p.spans {
        let Some(new) = span.style.node.and_then(|n| ch.get(&n)) else { continue };
        let s = &mut span.style;
        s.color = if new.visible { new.color } else { style::Color::TRANSPARENT };
        if s.background.is_some() {
            s.background = new.painted_background();
        }
        s.underline = new.underline;
        s.strikethrough = new.strikethrough;
        s.cursor = new.cursor;
        s.gradient = new.text_gradient.clone().filter(|_| new.visible);
    }
    for object in &mut p.objects {
        match object {
            InlineObject::Image(_) => {}
            InlineObject::Block(a) => {
                if let Some(new) = a.node.and_then(|n| ch.get(&n)) {
                    a.style = patched(&a.style, new);
                }
                if let Some(content) = &mut a.content
                    && para_mentions(content, ch)
                {
                    patch_para(Arc::make_mut(content), ch);
                }
            }
            InlineObject::BlockBox(b) => {
                if block_mentions(b, ch) {
                    patch_block(Arc::make_mut(b), ch);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(html: &str) -> (Document, style::StyleData) {
        let doc = html::parse(html, url::Url::parse("https://example.com/").unwrap());
        let styles = style::StyleData::for_document(&doc, &|_| None);
        (doc, styles)
    }

    fn find(doc: &Document, id: &str) -> NodeId {
        doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(id)).unwrap()
    }

    fn hovering(doc: &Document, id: NodeId) -> Arc<ElementState> {
        let mut state = ElementState::default();
        let mut n = Some(id);
        while let Some(i) = n {
            state.hovered.insert(i);
            n = doc.parent(i);
        }
        Arc::new(state)
    }

    #[test]
    fn color_hover_patches_in_place() {
        let (doc, styles) = setup(
            r#"<style>a:hover { color: red } .card:hover { background: #eee }</style>
               <div class="card" id="card" style="border: 1px solid #ccc"><p>Hello <a href="/x" id="link">there</a></p></div><p>after</p>"#,
        );
        let env = style::MediaEnv::default();
        let idle = Arc::new(ElementState::default());
        let mut styler = styles.styler_with_state(env, idle.clone());
        let layout = crate::build(&doc, &styler);

        let over = hovering(&doc, find(&doc, "link"));
        let Patch::Painted(patched, _) = patch(&layout, &doc, &mut styler, &idle, &over, true) else {
            panic!("expected a paint-only patch");
        };
        // Same as restyling from scratch.
        let fresh = crate::build(&doc, &styles.styler_with_state(env, over));
        assert_eq!(patched.rows, fresh.rows);
    }

    #[test]
    fn backgrounds_on_split_containers_need_a_restyle() {
        let (doc, styles) = setup(r#"<style>.c:hover { background: #eee }</style><div class="c"><p id="p">a</p><p>b</p></div>"#);
        let env = style::MediaEnv::default();
        let idle = Arc::new(ElementState::default());
        let mut styler = styles.styler_with_state(env, idle.clone());
        let layout = crate::build(&doc, &styler);
        let over = hovering(&doc, find(&doc, "p"));
        assert!(matches!(patch(&layout, &doc, &mut styler, &idle, &over, true), Patch::NeedsRestyle(_)));
    }

    #[test]
    fn layout_changes_need_a_restyle() {
        let (doc, styles) = setup(r#"<style>a:hover { font-size: 30px }</style><p><a href="/x" id="link">there</a></p>"#);
        let env = style::MediaEnv::default();
        let idle = Arc::new(ElementState::default());
        let mut styler = styles.styler_with_state(env, idle.clone());
        let layout = crate::build(&doc, &styler);
        let over = hovering(&doc, find(&doc, "link"));
        assert!(matches!(patch(&layout, &doc, &mut styler, &idle, &over, true), Patch::NeedsRestyle(_)));
    }
}
