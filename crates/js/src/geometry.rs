//! Layout geometry scripts read: where the tab laid elements out (the
//! rects it reports for measured elements, or asked for on the spot), the
//! viewport, and the element getters built on them (`getBoundingClientRect`,
//! `clientWidth`, `offsetTop`...). A child of `dom`, for the DOM's fields.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use page::NodeId;

use super::{Dom, Shared, arg_node, node_from_number};
use crate::FromJs;

/// What the tab has said about layout.
pub(crate) struct Geometry {
    /// Where measured elements are, `[x, y, w, h]`, viewport-relative (the
    /// last rect of ones no longer painted is kept).
    pub rects: HashMap<NodeId, [f32; 4]>,
    /// Elements scripts asked about: the tab keeps them a box and reports
    /// where it is.
    pub measured: HashSet<NodeId>,
    /// Painted in the tab's latest frame.
    pub painted: HashSet<NodeId>,
    /// Measured, and the tab says it didn't paint them (off screen, no box).
    pub unpainted: HashSet<NodeId>,
    pub viewport: (f32, f32),
}

impl Default for Geometry {
    fn default() -> Self {
        Self { rects: HashMap::new(), measured: HashSet::new(), painted: HashSet::new(), unpainted: HashSet::new(), viewport: (1280.0, 800.0) }
    }
}

impl Geometry {
    /// A layout from the tab: whether the viewport changed size.
    pub fn apply(&mut self, rects: &[(NodeId, [f32; 4])], gone: &[NodeId], viewport: (f32, f32)) -> bool {
        let resized = std::mem::replace(&mut self.viewport, viewport) != viewport;
        for &(n, r) in rects {
            self.rects.insert(n, r);
            self.painted.insert(n);
            self.unpainted.remove(&n);
        }
        for &n in gone {
            self.painted.remove(&n);
            self.unpainted.insert(n);
        }
        resized
    }
}

/// How many ancestors stand in for an element with no box of its own.
const NEAR_ANCESTORS: usize = 4;

/// Where the tab has `node`, asked and waited for (briefly: the tab answers
/// between frames). `None` before it has shown the page.
fn locate(d: &Shared, node: NodeId) -> Option<[f32; 4]> {
    let tab = {
        let dom = d.borrow();
        if !dom.laid_out {
            return None;
        }
        dom.tab.clone()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Instant::now();
    tab.send_blocking(FromJs::Locate(node, tx)).ok()?;
    let answer = rx.recv_timeout(Duration::from_millis(30));
    if d.borrow().debug {
        eprintln!("js: locate {node:?} -> {answer:?} in {:?}", t.elapsed());
    }
    answer.ok().flatten()
}

/// Where `node` is: the tab's rect for it. The first time it's asked about,
/// the tab is told to keep it a box (a new layout), and asked where it is.
pub(crate) fn measure(d: &Shared, node: NodeId) -> Option<[f32; 4]> {
    let ask = {
        let mut dom = d.borrow_mut();
        let new = dom.geometry.measured.insert(node);
        if new {
            dom.changed();
        }
        new && !dom.geometry.rects.contains_key(&node)
    };
    if ask && let Some(r) = locate(d, node) {
        d.borrow_mut().geometry.rects.insert(node, r);
    }
    d.borrow().geometry.rects.get(&node).copied()
}

/// Where `node` is painted now: its own box if it has ever had one (not
/// painted now: nowhere); one that never got a box (a 0x0 lazy `<img>`)
/// goes by its nearest painted ancestor.
pub(super) fn painted_rect(dom: &Dom, node: NodeId) -> Option<[f32; 4]> {
    let g = &dom.geometry;
    if g.unpainted.contains(&node) {
        return None;
    }
    if let Some(r) = g.rects.get(&node) {
        return g.painted.contains(&node).then_some(*r);
    }
    let mut n = node;
    for _ in 0..NEAR_ANCESTORS {
        n = dom.doc.parent(n).filter(|&p| dom.doc.element(p).is_some())?;
        if g.painted.contains(&n)
            && let Some(r) = g.rects.get(&n)
        {
            return Some(*r);
        }
    }
    None
}

/// Elements whose box is as wide as their content (0 before layout).
const INLINE_TAGS: &[&str] = &[
    "span", "a", "b", "i", "em", "strong", "small", "label", "img", "svg", "code", "abbr", "sup", "sub", "br", "input", "button", "select", "textarea",
    "video", "canvas", "iframe",
];

/// The width of an element the tab hasn't laid out yet (scripts measure
/// what they just made, where a browser lays out on the spot): a block in
/// block flow fills its container, so the nearest laid-out ancestor's
/// width, else the viewport's. Anything else is as wide as its content: 0
/// here (flex items in a row, inline and table boxes: GitHub collapsed its
/// breadcrumbs, each crumb "as wide as the page"). Carousels size their
/// slides from this: NASA's were 0px wide.
fn estimated_width(dom: &mut Dom, node: NodeId) -> f32 {
    let Some(tag) = dom.doc.element(node).map(|e| e.name.local.to_string()) else { return 0.0 };
    if INLINE_TAGS.contains(&tag.as_str()) || !super::is_connected(dom, node) {
        return 0.0;
    }
    let display = super::computed_value(dom, node, "display", None);
    if !matches!(display.as_str(), "block" | "flex" | "grid" | "list-item" | "flow-root") {
        return 0.0;
    }
    if let Some(parent) = dom.doc.parent(node).filter(|&p| dom.doc.element(p).is_some()) {
        let pd = super::computed_value(dom, parent, "display", None);
        let row = pd.contains("flex") && !super::computed_value(dom, parent, "flex-direction", None).contains("column");
        if (pd.contains("grid") || row) || pd.contains("inline") || pd.contains("table") {
            return 0.0;
        }
    }
    let g = &dom.geometry;
    let mut n = dom.doc.parent(node);
    while let Some(p) = n.filter(|&p| dom.doc.element(p).is_some()) {
        if g.painted.contains(&p)
            && let Some(r) = g.rects.get(&p)
        {
            return r[2];
        }
        n = dom.doc.parent(p);
    }
    g.viewport.0
}

/// The size `getComputedStyle` gives `width`/`height`: as laid out, or a
/// block's estimated width.
pub(crate) fn laid_out_size(d: &Shared, node: NodeId) -> Option<(f32, f32)> {
    if let Some(r) = measure(d, node) {
        return Some((r[2], r[3]));
    }
    let w = estimated_width(&mut d.borrow_mut(), node);
    (w > 0.0).then_some((w, 0.0))
}

/// The page's root element (`<html>`) and body.
fn roots(dom: &Dom) -> (Option<NodeId>, Option<NodeId>) {
    let root = dom.doc.root();
    let html = dom.doc.children(root).iter().copied().find(|&c| dom.doc.element(c).is_some());
    (html, dom.doc.body())
}

/// A size an element without a rect has: `<html>` and `<body>` span the
/// viewport, a block (across) its container.
fn root_size(dom: &mut Dom, node: NodeId, axis: usize) -> f32 {
    let (html, body) = roots(dom);
    if Some(node) == html || Some(node) == body {
        return if axis == 0 { dom.geometry.viewport.0 } else { dom.geometry.viewport.1 };
    }
    if axis == 0 { estimated_width(dom, node) } else { 0.0 }
}

/// The element `this` is (a wrapper's `__id`).
pub(super) fn this_node(d: &Shared, this: &JsValue, cx: &mut Context) -> Option<NodeId> {
    let id = this.as_object()?.get(js_string!("__id"), cx).ok()?.as_number()?;
    let id = node_from_number(id);
    d.borrow().doc.get(id).map(|_| id)
}

/// A `DOMRect`.
pub(super) fn dom_rect(r: [f32; 4], cx: &mut Context) -> JsResult<JsValue> {
    crate::dom_geometry::rect(r, cx)
}

fn rect_array(r: Option<[f32; 4]>, cx: &mut Context) -> JsValue {
    r.map_or(JsValue::null(), |r| JsArray::from_iter(r.map(JsValue::from), cx).into())
}

type Method = fn(&Shared, NodeId, &mut Context) -> JsResult<JsValue>;

/// A native method (or getter) on elements: `f` with the element `this` is
/// (`undefined` when it isn't one).
fn method(d: &Shared, name: &str, f: Method, cx: &mut Context) -> JsObject {
    let d = d.clone();
    // SAFETY: the closure captures only the Rc'd DOM, which holds no
    // garbage-collected values.
    let native = unsafe {
        NativeFunction::from_closure(move |this, _, cx| match this_node(&d, this, cx) {
            Some(n) => f(&d, n, cx),
            None => Ok(JsValue::undefined()),
        })
    };
    FunctionObjectBuilder::new(cx.realm(), native).name(JsString::from(name)).build().into()
}

fn rounded(v: f32) -> JsResult<JsValue> {
    Ok(JsValue::from(v.round()))
}

/// The element's rect, or its estimate (`width`/`height` 0 count as none:
/// what's 0 wide now likely isn't laid out).
fn box_size(d: &Shared, n: NodeId, axis: usize) -> f32 {
    let r = measure(d, n).map(|r| r[2 + axis]).filter(|&v| v != 0.0);
    r.unwrap_or_else(|| if axis == 0 { root_size(&mut d.borrow_mut(), n, 0) } else { 0.0 })
}

/// The viewport, for the root element's `client*`.
fn client_size(d: &Shared, n: NodeId, axis: usize) -> f32 {
    let dom = d.borrow();
    if roots(&dom).0 == Some(n) {
        return if axis == 0 { dom.geometry.viewport.0 } else { dom.geometry.viewport.1 };
    }
    drop(dom);
    box_size(d, n, axis)
}

/// Where `node`'s scrolling box is scrolled and how big its content is
/// (scrollLeft, scrollTop, scrollWidth, scrollHeight), asked of the tab
/// (`None`: it doesn't scroll, or nothing's showing yet).
fn element_scroll(d: &Shared, node: NodeId) -> Option<[f32; 4]> {
    let tab = {
        let dom = d.borrow();
        if !dom.laid_out {
            return None;
        }
        dom.tab.clone()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    tab.send_blocking(FromJs::ElementScroll(node, tx)).ok()?;
    rx.recv_timeout(Duration::from_millis(30)).ok().flatten()
}

/// `<html>` or `<body>`: their scrolling is the page's.
fn is_root(d: &Shared, node: NodeId) -> bool {
    let dom = d.borrow();
    let (html, body) = roots(&dom);
    Some(node) == html || Some(node) == body
}

/// Scroll the page to (x, y), as `window.scrollTo` does.
fn scroll_page(d: &Shared, x: f32, y: f32, smooth: bool) {
    let tab = {
        let mut dom = d.borrow_mut();
        dom.scroll = (x.max(0.0), y.max(0.0));
        dom.tab.clone()
    };
    tab.send_blocking(FromJs::ScrollTo { x: x.max(0.0), y: y.max(0.0), smooth }).ok();
}

/// Scroll `node` (its box, or the page for the root) to (x, y).
fn scroll_node(d: &Shared, node: NodeId, x: f32, y: f32, smooth: bool) {
    if is_root(d, node) {
        scroll_page(d, x, y, smooth);
        return;
    }
    let tab = d.borrow().tab.clone();
    tab.send_blocking(FromJs::ScrollElement { node, x: x.max(0.0), y: y.max(0.0) }).ok();
}

/// Where `node` is scrolled now: (left, top).
fn scrolled_to(d: &Shared, node: NodeId) -> (f32, f32) {
    if is_root(d, node) {
        return d.borrow().scroll;
    }
    element_scroll(d, node).map_or((0.0, 0.0), |i| (i[0], i[1]))
}

/// `scrollTo(x, y)` / `scrollTo({ left, top, behavior })` arguments,
/// relative to `base`: the target and whether it's smooth.
fn scroll_args(a: &[JsValue], now: (f32, f32), base: (f32, f32), cx: &mut Context) -> JsResult<(f32, f32, bool)> {
    let num = |v: &JsValue, cx: &mut Context| -> JsResult<Option<f32>> {
        if v.is_undefined() { Ok(None) } else { Ok(Some(v.to_number(cx)? as f32).filter(|n| n.is_finite())) }
    };
    let (x, y, smooth) = match a.first().and_then(|v| v.as_object()) {
        Some(o) => {
            let left = o.get(js_string!("left"), cx)?;
            let top = o.get(js_string!("top"), cx)?;
            let behavior = o.get(js_string!("behavior"), cx)?;
            (num(&left, cx)?, num(&top, cx)?, behavior.as_string().is_some_and(|b| b.to_std_string_escaped() == "smooth"))
        }
        None => (num(&a.first().cloned().unwrap_or_default(), cx)?, num(&a.get(1).cloned().unwrap_or_default(), cx)?, false),
    };
    Ok((x.map_or(now.0, |x| base.0 + x), y.map_or(now.1, |y| base.1 + y), smooth))
}

type ArgMethod = fn(&Shared, NodeId, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// A native method (or setter) on elements that takes arguments.
fn arg_method(d: &Shared, name: &str, f: ArgMethod, cx: &mut Context) -> JsObject {
    let d = d.clone();
    // SAFETY: the closure captures only the Rc'd DOM, which holds no
    // garbage-collected values.
    let native = unsafe {
        NativeFunction::from_closure(move |this, a, cx| match this_node(&d, this, cx) {
            Some(n) => f(&d, n, a, cx),
            None => Ok(JsValue::undefined()),
        })
    };
    FunctionObjectBuilder::new(cx.realm(), native).name(JsString::from(name)).build().into()
}

/// Install the element geometry methods on `proto` (`Element.prototype`).
fn install_on(d: &Shared, proto: &JsObject, cx: &mut Context) -> JsResult<()> {
    let methods: [(&str, Method); 2] = [
        ("getBoundingClientRect", |d, n, cx| {
            let r = measure(d, n).unwrap_or_else(|| [0.0, 0.0, estimated_width(&mut d.borrow_mut(), n), 0.0]);
            dom_rect(r, cx)
        }),
        ("getClientRects", |d, n, cx| {
            let rects = match measure(d, n) {
                Some(r) => vec![dom_rect(r, cx)?],
                None => Vec::new(),
            };
            Ok(JsArray::from_iter(rects, cx).into())
        }),
    ];
    for (name, f) in methods {
        let f = method(d, name, f, cx);
        proto.define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().value(f).writable(true).configurable(true).enumerable(false), cx)?;
    }
    let getters: [(&str, Method); 6] = [
        ("clientWidth", |d, n, _| rounded(client_size(d, n, 0))),
        ("clientHeight", |d, n, _| rounded(client_size(d, n, 1))),
        ("offsetWidth", |d, n, _| rounded(box_size(d, n, 0))),
        ("offsetHeight", |d, n, _| rounded(box_size(d, n, 1))),
        // Relative to the page (the body: no other offset parents here).
        ("offsetTop", |d, n, _| {
            let r = measure(d, n);
            rounded(r.map_or(0.0, |r| r[1] + d.borrow().scroll.1))
        }),
        ("offsetLeft", |d, n, _| {
            let r = measure(d, n);
            rounded(r.map_or(0.0, |r| r[0] + d.borrow().scroll.0))
        }),
    ];
    for (name, f) in getters {
        let f = method(d, name, f, cx);
        proto.define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().get(f).configurable(true).enumerable(false), cx)?;
    }
    // Scrolling: an element's own box, or the page for the root.
    let scroll_getters: [(&str, Method, ArgMethod); 4] = [
        ("scrollTop", |d, n, _| Ok(JsValue::from(scrolled_to(d, n).1.round())), |d, n, a, cx| {
            let y = a.first().cloned().unwrap_or_default().to_number(cx)? as f32;
            if y.is_finite() {
                let x = scrolled_to(d, n).0;
                scroll_node(d, n, x, y, false);
            }
            Ok(JsValue::undefined())
        }),
        ("scrollLeft", |d, n, _| Ok(JsValue::from(scrolled_to(d, n).0.round())), |d, n, a, cx| {
            let x = a.first().cloned().unwrap_or_default().to_number(cx)? as f32;
            if x.is_finite() {
                let y = scrolled_to(d, n).1;
                scroll_node(d, n, x, y, false);
            }
            Ok(JsValue::undefined())
        }),
        // What it scrolls; a box that doesn't scroll is as big as itself.
        ("scrollWidth", |d, n, _| {
            let w = element_scroll(d, n).map(|i| i[2]).unwrap_or_else(|| box_size(d, n, 0));
            rounded(w)
        }, |_, _, _, _| Ok(JsValue::undefined())),
        ("scrollHeight", |d, n, _| {
            let h = element_scroll(d, n).map(|i| i[3]).unwrap_or_else(|| client_size(d, n, 1));
            rounded(h)
        }, |_, _, _, _| Ok(JsValue::undefined())),
    ];
    for (name, get, set) in scroll_getters {
        let get = method(d, name, get, cx);
        let set = arg_method(d, name, set, cx);
        proto.define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().get(get).set(set).configurable(true).enumerable(false), cx)?;
    }
    let scroll_methods: [(&str, ArgMethod); 4] = [
        ("scrollTo", |d, n, a, cx| {
            let now = scrolled_to(d, n);
            let (x, y, smooth) = scroll_args(a, now, (0.0, 0.0), cx)?;
            scroll_node(d, n, x, y, smooth);
            Ok(JsValue::undefined())
        }),
        ("scroll", |d, n, a, cx| {
            let now = scrolled_to(d, n);
            let (x, y, smooth) = scroll_args(a, now, (0.0, 0.0), cx)?;
            scroll_node(d, n, x, y, smooth);
            Ok(JsValue::undefined())
        }),
        ("scrollBy", |d, n, a, cx| {
            let now = scrolled_to(d, n);
            let (x, y, smooth) = scroll_args(a, now, now, cx)?;
            scroll_node(d, n, x, y, smooth);
            Ok(JsValue::undefined())
        }),
        // The page scrolls to put the element in view (`block`: start,
        // center, end or nearest; `true`/`false` as start/end).
        ("scrollIntoView", |d, n, a, cx| {
            let Some([_, y, _, h]) = measure(d, n) else { return Ok(JsValue::undefined()) };
            let (block, smooth) = match a.first() {
                Some(v) if v.is_object() => {
                    let o = v.as_object().expect("an object");
                    let block = o.get(js_string!("block"), cx)?;
                    let block = if block.is_undefined() { "start".to_owned() } else { block.to_string(cx)?.to_std_string_escaped() };
                    let behavior = o.get(js_string!("behavior"), cx)?;
                    (block, behavior.as_string().is_some_and(|b| b.to_std_string_escaped() == "smooth"))
                }
                Some(v) if v.is_boolean() && !v.to_boolean() => ("end".to_owned(), false),
                _ => ("start".to_owned(), false),
            };
            let (sx, sy) = d.borrow().scroll;
            let vh = d.borrow().geometry.viewport.1;
            let target = match block.as_str() {
                "center" => sy + y - (vh - h) / 2.0,
                "end" => sy + y + h - vh,
                "nearest" if y >= 0.0 && y + h <= vh => return Ok(JsValue::undefined()),
                "nearest" if y < 0.0 => sy + y,
                "nearest" => sy + y + h - vh,
                _ => sy + y,
            };
            scroll_page(d, sx, target, smooth);
            Ok(JsValue::undefined())
        }),
    ];
    for (name, f) in scroll_methods {
        let f = arg_method(d, name, f, cx);
        proto.define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().value(f).writable(true).configurable(true).enumerable(false), cx)?;
    }
    Ok(())
}

/// Natives: `N.geometry(Element.prototype)` installs the element methods;
/// `N.rect(id)` (what the tab last said, without asking), `N.measure(id)`,
/// `N.paintedRect(id)`, `N.unpainted(id)`; `innerWidth`/`innerHeight` on
/// the window.
pub(crate) fn install(natives: &JsObject, dom: &Shared, cx: &mut Context) {
    let natives_fn = |name: &str, f: fn(&Shared, &[JsValue], &mut Context) -> JsResult<JsValue>, cx: &mut Context| {
        let d = dom.clone();
        // SAFETY: as in `method`.
        let native = unsafe { NativeFunction::from_closure(move |_, a, cx| f(&d, a, cx)) };
        let f = FunctionObjectBuilder::new(cx.realm(), native).name(JsString::from(name)).build();
        natives.set(JsString::from(name), f, false, cx).ok();
    };
    natives_fn("geometry", |d, a, cx| {
        if let Some(proto) = a.first().and_then(|p| p.as_object()) {
            install_on(d, &proto, cx)?;
        }
        Ok(JsValue::undefined())
    }, cx);
    natives_fn("rect", |d, a, cx| {
        let r = arg_node(&d.borrow(), a, 0).and_then(|n| d.borrow().geometry.rects.get(&n).copied());
        Ok(rect_array(r, cx))
    }, cx);
    natives_fn("measure", |d, a, cx| {
        let n = arg_node(&d.borrow(), a, 0);
        Ok(rect_array(n.and_then(|n| measure(d, n)), cx))
    }, cx);
    natives_fn("paintedRect", |d, a, cx| {
        let dom = d.borrow();
        let r = arg_node(&dom, a, 0).and_then(|n| painted_rect(&dom, n));
        drop(dom);
        Ok(rect_array(r, cx))
    }, cx);
    natives_fn("unpainted", |d, a, _| {
        let dom = d.borrow();
        Ok(JsValue::from(arg_node(&dom, a, 0).is_some_and(|n| dom.geometry.unpainted.contains(&n))))
    }, cx);
    // The viewport's size, as the tab last reported it.
    let global = cx.global_object();
    for (name, axis) in [("innerWidth", 0), ("innerHeight", 1)] {
        let d = dom.clone();
        // SAFETY: as in `method`.
        let get = unsafe {
            NativeFunction::from_closure(move |_, _, _| {
                let v = d.borrow().geometry.viewport;
                Ok(JsValue::from(if axis == 0 { v.0 } else { v.1 }))
            })
        };
        let get = FunctionObjectBuilder::new(cx.realm(), get).name(JsString::from(name)).build();
        let set = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()))).build();
        global
            .define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().get(get).set(set).configurable(true).enumerable(true), cx)
            .ok();
    }
}
