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
    let mut changes: Changes = HashMap::new();
    let mut changed: Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)> = Vec::new();
    let mut appearing: Vec<NodeId> = Vec::new();
    for (&id, old_style) in affected.iter().zip(&before) {
        let new_style = styles.get(id);
        if let Err(why) = paint_only(doc, id, old_style, &new_style, &mut changes, &mut changed, &mut appearing) {
            return Patch::NeedsRestyle(why);
        }
    }
    // `a:hover::after { content: "→" }` and friends.
    if pseudos(&roots, &mut styles) != before_pseudo {
        return Patch::NeedsRestyle("::before/::after changed");
    }
    finish(layout, changes, changed, &appearing)
}

/// One element restyled from `old_style` to `new_style`: noted in `changes`
/// if only how it's painted changed, else why it can't be patched.
fn paint_only(
    doc: &Document,
    id: NodeId,
    old_style: &Arc<ComputedStyle>,
    new_style: &Arc<ComputedStyle>,
    changes: &mut Changes,
    changed: &mut Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)>,
    appearing: &mut Vec<NodeId>,
) -> Result<(), &'static str> {
    if **new_style == **old_style {
        return Ok(());
    }
    let mut probe = (**new_style).clone();
    copy_paint(&mut probe, old_style);
    if moves_only(old_style, new_style) {
        probe.inset = old_style.inset;
    }
    if probe != **old_style {
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            eprintln!("patch: layout change on {:?}: {}", doc.element(id).map(|e| e.name.local.to_string()), style_diff(old_style, &probe));
        }
        return Err("layout property changed");
    }
    // Inline boxes' backgrounds and borders are baked into their
    // paragraph's decorations (which don't know their element).
    if new_style.display == Display::Inline
        && (new_style.painted_background() != old_style.painted_background() || (new_style.has_border() && new_style.border_color != old_style.border_color))
    {
        return Err("inline box background");
    }
    if new_style.painted_background().is_some() != old_style.painted_background().is_some() {
        appearing.push(id);
    }
    changed.push((id, old_style.clone(), new_style.clone()));
    changes.insert((id, 0), new_style.clone());
    Ok(())
}

/// The patch for these changes, once each was found paint-only.
fn finish(layout: &LayoutTree, changes: Changes, changed: Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)>, appearing: &[NodeId]) -> Patch {
    // Blocks gaining or losing a background: fine for a box, but a container
    // that was split into rows is drawn differently with one (as a wrapper).
    if !appearing.is_empty() {
        let mut boxes = HashSet::new();
        for row in &layout.rows {
            box_nodes(&row.content, &mut boxes);
            row.absolutes.iter().for_each(|b| box_nodes(b, &mut boxes));
        }
        layout.pinned.iter().for_each(|p| box_nodes(&p.content, &mut boxes));
        if appearing.iter().any(|n| !boxes.contains(n)) {
            return Patch::NeedsRestyle("background on a split container");
        }
    }
    if changes.is_empty() {
        return Patch::Unchanged;
    }
    match apply(layout, &changes) {
        Some(tree) => Patch::Painted(tree, changed),
        None => Patch::Unchanged,
    }
}

/// The new styles of a DOM patch's elements, each with its parent's (what
/// the style cache keeps).
pub type Restyled = Vec<(NodeId, Arc<ComputedStyle>, Arc<ComputedStyle>)>;

/// Patch `layout`, built from `old`, for `new`: the same document but for
/// some elements' attributes (`StyleCache::attribute_restyle`), with
/// `elements` the ones to restyle. Each is styled in both; if only paint
/// changed, that's copied into the layout (as for `:hover`), with no box
/// building and nothing to re-measure. Also the elements' new styles.
pub fn patch_dom(layout: &LayoutTree, old: &Document, new: &Document, styler: &Styler, elements: &[NodeId]) -> (Patch, Restyled) {
    let mut before = Styles::new(old, styler);
    let mut after = Styles::new(new, styler);
    let mut changes: Changes = HashMap::new();
    let mut changed = Vec::new();
    let mut appearing = Vec::new();
    let mut restyled = Vec::with_capacity(elements.len());
    for &id in elements {
        let (old_style, new_style) = (before.get(id), after.get(id));
        if let Err(why) = paint_only(new, id, &old_style, &new_style, &mut changes, &mut changed, &mut appearing) {
            return (Patch::NeedsRestyle(why), Vec::new());
        }
        let parent = match new.parent(id) {
            Some(p) if new.element(p).is_some() => after.get(p),
            _ => after.root_parent.clone(),
        };
        restyled.push((id, parent, new_style));
    }
    if pseudos(elements, &mut before) != pseudos(elements, &mut after) {
        return (Patch::NeedsRestyle("::before/::after changed"), Vec::new());
    }
    (finish(layout, changes, changed, &appearing), restyled)
}

/// Copy these elements' paint properties into the layout (`None`: none of
/// them are in it). Hover patches and animation frames both come through here.
pub fn apply(layout: &LayoutTree, changes: &Changes) -> Option<LayoutTree> {
    let mut tree = layout.clone();
    let mut touched = false;
    for row in &mut tree.rows {
        if row_mentions(row, changes) {
            patch_row(row, changes);
            touched = true;
        }
    }
    // Fixed and sticky boxes alike (a pinned header's hover).
    for p in &mut tree.pinned {
        if mentions(&p.content, changes) {
            patch_box(&mut p.content, changes);
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
/// Every element (or `::before`/`::after` box) in the layout with CSS
/// animations, and its style.
pub fn animated_elements(layout: &LayoutTree) -> Vec<(AnimKey, Arc<ComputedStyle>)> {
    fn walk(b: &LayoutBox, out: &mut Vec<(AnimKey, Arc<ComputedStyle>)>) {
        match b {
            LayoutBox::Block(block) => {
                if let Some(k) = block_key(block)
                    && block.style.animations.is_some()
                {
                    out.push((k, block.style.clone()));
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
                                out.push(((n, 0), a.style.clone()));
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
                && !out.iter().any(|(m, _)| *m == (n, 0))
            {
                out.push(((n, 0), w.style.clone()));
            }
        }
        walk(&row.content, &mut out);
        row.absolutes.iter().for_each(|b| walk(b, &mut out));
    }
    layout.pinned.iter().for_each(|p| walk(&p.content, &mut out));
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

/// New paint styles: by element, or one of its `::before` (1) / `::after`
/// (2) boxes (0 is the element itself).
pub type AnimKey = (NodeId, u8);
type Changes = HashMap<AnimKey, Arc<ComputedStyle>>;

fn has(ch: &Changes, n: NodeId) -> bool {
    ch.contains_key(&(n, 0))
}

fn get(ch: &Changes, n: NodeId) -> Option<&Arc<ComputedStyle>> {
    ch.get(&(n, 0))
}

fn block_key(b: &Block) -> Option<AnimKey> {
    b.node.map(|n| (n, 0)).or(b.pseudo)
}

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
    if moves_only(style, new) {
        s.inset = new.inset;
    }
    Arc::new(s)
}

/// Whether `top`/`left`/... changing between these just moves the box: a
/// relatively positioned one is only shifted, an absolutely positioned one
/// is out of the flow (and laid out from its style each frame), so nothing
/// else moves. (A slider's knob at `left: 93px`, many times a second.)
fn moves_only(old: &ComputedStyle, new: &ComputedStyle) -> bool {
    old.position == new.position && matches!(new.position, style::Position::Absolute | style::Position::Relative)
}

fn row_mentions(row: &Row, ch: &Changes) -> bool {
    row.wrappers.iter().any(|w| w.node.is_some_and(|n| has(ch, n)))
        || mentions(&row.content, ch)
        || row.absolutes.iter().any(|b| mentions(b, ch))
}

fn patch_row(row: &mut Row, ch: &Changes) {
    if row.wrappers.iter().any(|w| w.node.is_some_and(|n| has(ch, n))) {
        let mut wrappers = row.wrappers.to_vec();
        for w in &mut wrappers {
            if let Some(new) = w.node.and_then(|n| get(ch, n)) {
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
    block_key(b).is_some_and(|k| ch.contains_key(&k)) || b.children.iter().any(|c| mentions(c, ch))
}

fn para_mentions(p: &Paragraph, ch: &Changes) -> bool {
    p.spans.iter().any(|s| s.style.node.is_some_and(|n| has(ch, n)))
        || p.objects.iter().any(|o| match o {
            InlineObject::Image(_) => false,
            InlineObject::Block(a) => {
                a.node.is_some_and(|n| has(ch, n)) || a.content.as_ref().is_some_and(|c| para_mentions(c, ch))
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
    if let Some(new) = block_key(b).and_then(|k| ch.get(&k)) {
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
        let Some(new) = span.style.node.and_then(|n| get(ch, n)) else { continue };
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
                if let Some(new) = a.node.and_then(|n| get(ch, n)) {
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

    /// `doc` with `id`'s `style` attribute set to `value`.
    fn restyled(doc: &Document, id: &str, value: &str) -> Document {
        let mut new = doc.clone();
        let n = find(&new, id);
        let e = new.element_mut(n).unwrap();
        e.attrs.iter_mut().find(|a| &*a.name.local == "style").expect("a style attribute").value = value.into();
        new
    }

    #[test]
    fn script_style_changes_patch_in_place() {
        let (doc, styles) = setup(
            r#"<div style="position: relative; height: 40px"><div id="bar" style="transform: scaleX(0.1); height: 4px; background: red"></div>
               <div id="knob" style="position: absolute; left: 10px; top: 0; width: 8px; height: 8px; background: white"></div></div><p>after</p>"#,
        );
        let env = style::MediaEnv::default();
        let state = Arc::new(ElementState::default());
        let styler = styles.styler_with_state(env, state.clone());
        let layout = crate::build(&doc, &styler);

        // A progress bar's transform and a knob's `left`: patched, as built afresh.
        let new = restyled(&restyled(&doc, "bar", "transform: scaleX(0.6); height: 4px; background: red"), "knob", "position: absolute; left: 90px; top: 0; width: 8px; height: 8px; background: white");
        let elements = vec![find(&new, "bar"), find(&new, "knob")];
        let (Patch::Painted(patched, changed), restyled_styles) = patch_dom(&layout, &doc, &new, &styler, &elements) else {
            panic!("expected a paint-only patch");
        };
        assert_eq!(changed.len(), 2);
        assert_eq!(restyled_styles.len(), 2);
        assert_eq!(patched.rows, crate::build(&new, &styler).rows);

        // A size: needs building.
        let taller = restyled(&doc, "bar", "transform: scaleX(0.1); height: 40px; background: red");
        assert!(matches!(patch_dom(&layout, &doc, &taller, &styler, &[find(&taller, "bar")]).0, Patch::NeedsRestyle(_)));
    }

    #[test]
    fn only_restyling_attribute_changes_are_patch_candidates() {
        let (doc, styles) = setup(r#"<div id="a" class="x">text</div><img id="i" src="/a.png"><p id="p">para</p>"#);
        let env = style::MediaEnv::default();
        let state = Arc::new(ElementState::default());
        let doc = Arc::new(doc);
        let mut cache = style::StyleCache::default();
        let session = cache.begin(&styles, &env, doc.clone(), state.clone());
        let (_, session) = crate::build_cached(&doc, &styles.styler_with_state(env, state.clone()), &Default::default(), Some(session));
        cache.finish(session.unwrap());

        let mut classed = (*doc).clone();
        let a = find(&classed, "a");
        classed.element_mut(a).unwrap().attrs.iter_mut().find(|at| &*at.name.local == "class").unwrap().value = "y".into();
        let found = cache.attribute_restyle(&styles, &env, &doc, &Arc::new(classed), &state, 100);
        assert!(found.is_some_and(|e| e.contains(&a)));

        // What an image shows, and new children: built.
        let mut src = (*doc).clone();
        let i = find(&src, "i");
        src.element_mut(i).unwrap().attrs.iter_mut().find(|at| &*at.name.local == "src").unwrap().value = "/b.png".into();
        assert!(cache.attribute_restyle(&styles, &env, &doc, &Arc::new(src), &state, 100).is_none());
        let mut grown = (*doc).clone();
        let p = find(&grown, "p");
        grown.append_text(p, " more");
        assert!(cache.attribute_restyle(&styles, &env, &doc, &Arc::new(grown), &state, 100).is_none());
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
