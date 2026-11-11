//! `IntersectionObserver` and `ResizeObserver`, native classes fed by the
//! tab's layouts (`geometry.rs`). A child of `dom`, for the DOM's fields.
//!
//! An IntersectionObserver target intersects when the box it was last
//! painted in overlaps the root (the viewport, grown by `rootMargin`). One
//! never painted is off screen: the page list only draws what's in view
//! (and measured elements always get a box). Without this everything was
//! "visible", and infinite feeds loaded page after page in the background.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};
use page::NodeId;

use super::page;
use super::geometry::{dom_rect, measure, painted_rect, this_node};


/// Observers with targets, which each layout is checked against.
#[derive(Trace, Finalize, JsData, Default)]
struct Live {
    intersection: GcRefCell<Vec<JsObject>>,
    resize: GcRefCell<Vec<JsObject>>,
}

/// The live-observer list (a root: held by the context).
struct LiveList(JsObject);

fn live(cx: &Context) -> JsObject {
    cx.get_data::<LiveList>().expect("observers installed").0.clone()
}

fn with_live<R>(cx: &Context, f: impl FnOnce(&Live) -> R) -> R {
    let o = live(cx);
    let l = o.downcast_ref::<Live>().expect("the live list");
    f(&l)
}

fn add_live(list: &GcRefCell<Vec<JsObject>>, o: &JsObject) {
    let mut list = list.borrow_mut();
    if !list.iter().any(|x| JsObject::equals(x, o)) {
        list.push(o.clone());
    }
}

fn remove_live(list: &GcRefCell<Vec<JsObject>>, o: &JsObject) {
    list.borrow_mut().retain(|x| !JsObject::equals(x, o));
}

/// An uncaught error in a callback: reported (`error` event, console).
pub(crate) fn report_error(e: JsError, cx: &mut Context) {
    let report = cx.global_object().get(js_string!("reportError"), cx).ok().and_then(|f| f.as_callable());
    match report {
        Some(report) => {
            let e = e.into_opaque(cx).unwrap_or_default();
            report.call(&JsValue::undefined(), &[e], cx).ok();
        }
        None => eprintln!("js: {e}"),
    }
}

/// Call `f` in `ms` (the page's `setTimeout`).
fn later(ms: u32, f: NativeFunction, cx: &mut Context) {
    let f = boa_engine::object::FunctionObjectBuilder::new(cx.realm(), f).build();
    if let Some(set_timeout) = cx.global_object().get(js_string!("setTimeout"), cx).ok().and_then(|f| f.as_callable()) {
        set_timeout.call(&JsValue::undefined(), &[f.into(), JsValue::from(ms)], cx).ok();
    }
}

/// `this`, checked to be a `T`.
fn this_object<T: boa_engine::NativeObject>(this: &JsValue, name: &str) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<T>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message(format!("not a {name}")).into())
}

/// The node an element wrapper is.
fn node_of(v: &JsValue, cx: &mut Context) -> Option<NodeId> {
    this_node(&page(cx), v, cx)
}

fn number(v: f32) -> JsValue {
    JsValue::from(v)
}

/// An object with these properties.
fn record(props: &[(&str, JsValue)], cx: &mut Context) -> JsObject {
    let o = JsObject::with_object_proto(cx.intrinsics());
    for (k, v) in props {
        o.set(JsString::from(*k), v.clone(), false, cx).ok();
    }
    o
}

/// An instance of `C` (its prototype), with these own properties.
fn instance<C: Class>(props: &[(&str, JsValue)], cx: &mut Context) -> JsObject {
    let o = record(props, cx);
    if let Some(class) = cx.get_global_class::<C>() {
        o.set_prototype(Some(class.prototype()));
    }
    o
}

// ---- IntersectionObserver ------------------------------------------------

#[derive(Trace, Finalize)]
struct Watched {
    target: JsObject,
    /// Whether it was last reported intersecting (`None`: not yet).
    #[unsafe_ignore_trace]
    last: Option<bool>,
}

#[derive(Trace, Finalize, JsData)]
struct IntersectionObserver {
    callback: JsObject,
    root: Option<JsObject>,
    /// top, right, bottom, left: (amount, is a percentage).
    #[unsafe_ignore_trace]
    margin: [(f32, bool); 4],
    #[unsafe_ignore_trace]
    thresholds: Vec<f64>,
    targets: GcRefCell<Vec<Watched>>,
}

/// `parseFloat`: the number a string starts with (0 if none).
fn leading_number(s: &str) -> f32 {
    let end = s.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0))).map_or(s.len(), |(i, _)| i);
    s[..end].parse().unwrap_or(0.0)
}

fn parse_margin(s: &str) -> [(f32, bool); 4] {
    let parts: Vec<(f32, bool)> = s.split_whitespace().map(|v| (leading_number(v), v.ends_with('%'))).collect();
    let top = parts.first().copied().unwrap_or((0.0, false));
    let right = parts.get(1).copied().unwrap_or(top);
    let bottom = parts.get(2).copied().unwrap_or(top);
    let left = parts.get(3).copied().unwrap_or(right);
    [top, right, bottom, left]
}

impl Class for IntersectionObserver {
    const NAME: &'static str = "IntersectionObserver";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        class
            .method(js_string!("observe"), 1, NativeFunction::from_fn_ptr(io_observe))
            .method(js_string!("unobserve"), 1, NativeFunction::from_fn_ptr(io_unobserve))
            .method(js_string!("disconnect"), 0, NativeFunction::from_fn_ptr(io_disconnect))
            .method(js_string!("takeRecords"), 0, NativeFunction::from_fn_ptr(|_, _, cx| Ok(JsArray::new(cx)?.into())));
        Ok(())
    }

    fn data_constructor(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let callback = args.first().and_then(|c| c.as_callable()).ok_or_else(|| JsNativeError::typ().with_message("IntersectionObserver needs a callback"))?;
        let opts = args.get(1).and_then(|o| o.as_object());
        let get = |k: &str, cx: &mut Context| opts.as_ref().map(|o| o.get(JsString::from(k), cx).unwrap_or_default()).unwrap_or_default();
        let root = get("root", cx).as_object().filter(|r| r.has_property(js_string!("__featherNode"), cx).unwrap_or(false));
        let margin = get("rootMargin", cx);
        let margin = if margin.is_undefined() || margin.is_null() { "0px".to_owned() } else { super::js_value_string(&margin, cx) };
        let t = get("threshold", cx);
        let thresholds = match t.as_object().and_then(|o| JsArray::from_object(o).ok()) {
            Some(list) => {
                let n = list.length(cx)?;
                (0..n).map(|i| list.get(i, cx).and_then(|v| v.to_number(cx))).collect::<JsResult<Vec<f64>>>()?
            }
            None => vec![t.as_number().unwrap_or(0.0)],
        };
        Ok(Self { callback, root, margin: parse_margin(&margin), thresholds, targets: GcRefCell::new(Vec::new()) })
    }

    fn object_constructor(instance: &JsObject<Self>, _: &[JsValue], cx: &mut Context) -> JsResult<()> {
        let (root, margin, thresholds) = {
            let io = instance.borrow();
            let io = io.data();
            let margin = io.margin.iter().map(|(n, pct)| format!("{n}{}", if *pct { "%" } else { "px" })).collect::<Vec<_>>().join(" ");
            (io.root.clone(), margin, io.thresholds.clone())
        };
        let thresholds = JsArray::from_iter(thresholds.into_iter().map(JsValue::from), cx);
        let instance = instance.clone().upcast();
        instance.set(js_string!("root"), root.map_or(JsValue::null(), JsValue::from), false, cx)?;
        instance.set(js_string!("rootMargin"), JsString::from(margin), false, cx)?;
        instance.set(js_string!("thresholds"), thresholds, false, cx)?;
        Ok(())
    }
}

/// How many ancestors are measured with a target: a box with nothing in it
/// yet (an `<img>` waiting for its `src`) may not be painted, but where
/// they are, it is.
const NEAR_ANCESTORS: usize = 4;

/// What a check after `observe` passes along: the observer, and the target
/// whose wait for a first paint is over (reported even if never painted).
#[derive(Trace, Finalize)]
struct Check {
    observer: JsObject,
    settle: Option<JsObject>,
}

fn io_observe(this: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let target = a.first().cloned().unwrap_or_default();
    let (Some(node), Some(target)) = (node_of(&target, cx), target.as_object()) else { return Ok(JsValue::undefined()) };
    let observer = this_object::<IntersectionObserver>(this, "IntersectionObserver")?;
    let io = observer.downcast_ref::<IntersectionObserver>().expect("checked");
    if io.targets.borrow().iter().any(|w| JsObject::equals(&w.target, &target)) {
        return Ok(JsValue::undefined());
    }
    io.targets.borrow_mut().push(Watched { target: target.clone(), last: None });
    drop(io);
    with_live(cx, |l| add_live(&l.intersection, &observer));
    let d = page(cx);
    let mut n = Some(node);
    for _ in 0..=NEAR_ANCESTORS {
        let Some(e) = n.filter(|&e| d.borrow().doc.element(e).is_some()) else { break };
        measure(&d, e);
        n = d.borrow().doc.parent(e);
    }
    let check = |c: &Check, cx: &mut Context| io_check(&c.observer, c.settle.as_ref(), cx);
    later(0, NativeFunction::from_copy_closure_with_captures(move |_, _, c, cx| check(c, cx).map(|_| JsValue::undefined()), Check { observer: observer.clone(), settle: None }), cx);
    // The tab says where it's painted, or that it isn't (off screen,
    // nothing rendered). Should it never say (no layout comes), report
    // anyway: callers wait for a first entry.
    later(3000, NativeFunction::from_copy_closure_with_captures(move |_, _, c, cx| check(c, cx).map(|_| JsValue::undefined()), Check { observer, settle: Some(target) }), cx);
    Ok(JsValue::undefined())
}

fn io_unobserve(this: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let observer = this_object::<IntersectionObserver>(this, "IntersectionObserver")?;
    let io = observer.downcast_ref::<IntersectionObserver>().expect("checked");
    if let Some(target) = a.first().and_then(|t| t.as_object()) {
        io.targets.borrow_mut().retain(|w| !JsObject::equals(&w.target, &target));
    }
    Ok(JsValue::undefined())
}

fn io_disconnect(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let observer = this_object::<IntersectionObserver>(this, "IntersectionObserver")?;
    let io = observer.downcast_ref::<IntersectionObserver>().expect("checked");
    io.targets.borrow_mut().clear();
    drop(io);
    with_live(cx, |l| remove_live(&l.intersection, &observer));
    Ok(JsValue::undefined())
}

/// Report the targets whose intersection changed (`settle`: that one even
/// if it was never painted).
fn io_check(observer: &JsObject, settle: Option<&JsObject>, cx: &mut Context) -> JsResult<()> {
    let d = page(cx);
    let mut entries = Vec::new();
    let callback = {
        let Some(io) = observer.downcast_ref::<IntersectionObserver>() else { return Ok(()) };
        let targets: Vec<(JsObject, Option<bool>)> = io.targets.borrow().iter().map(|w| (w.target.clone(), w.last)).collect();
        let (vw, vh) = d.borrow().geometry.viewport;
        let root_node = io.root.as_ref().and_then(|r| node_of(&r.clone().into(), cx));
        let [rx, ry, rw, rh] = root_node.and_then(|n| d.borrow().geometry.rects.get(&n).copied()).unwrap_or([0.0, 0.0, vw, vh]);
        let px = |(n, pct): (f32, bool), size: f32| if pct { n * size / 100.0 } else { n };
        let [mt, mr, mb, ml] = io.margin;
        let (left, top, right, bottom) = (rx - px(ml, rw), ry - px(mt, rh), rx + rw + px(mr, rw), ry + rh + px(mb, rh));
        let now = super::now_ms(&d);
        for (target, last) in targets {
            if settle.is_some_and(|s| !JsObject::equals(s, &target)) {
                continue;
            }
            let Some(node) = node_of(&target.clone().into(), cx) else { continue };
            let (r, unpainted, connected) = {
                let dom = d.borrow();
                (painted_rect(&dom, node), dom.geometry.unpainted.contains(&node), super::is_connected(&dom, node))
            };
            if r.is_none() && settle.is_none() && !unpainted {
                continue;
            }
            let [x, y, w, h] = r.unwrap_or_default();
            let (ix, iy, ax, ay) = (x.max(left), y.max(top), (x + w).min(right), (y + h).min(bottom));
            let inside = r.is_some() && connected && ax >= ix && ay >= iy;
            let area = w * h;
            let ratio = if !inside { 0.0 } else if area > 0.0 { (ax - ix) * (ay - iy) / area } else { 1.0 };
            let visible = inside && io.thresholds.iter().any(|&t| t == 0.0 || f64::from(ratio) >= t);
            if last == Some(visible) {
                continue;
            }
            if let Some(w) = io.targets.borrow_mut().iter_mut().find(|w| JsObject::equals(&w.target, &target)) {
                w.last = Some(visible);
            }
            let bounding = dom_rect([x, y, w, h], cx)?;
            let intersection = dom_rect(if inside { [ix, iy, ax - ix, ay - iy] } else { [0.0; 4] }, cx)?;
            let root_bounds = dom_rect([rx, ry, rw, rh], cx)?;
            let entry = instance::<IntersectionObserverEntry>(
                &[
                    ("target", target.into()),
                    ("isIntersecting", visible.into()),
                    ("intersectionRatio", number(ratio)),
                    ("time", JsValue::from(now)),
                    ("boundingClientRect", bounding),
                    ("intersectionRect", intersection),
                    ("rootBounds", root_bounds),
                ],
                cx,
            );
            entries.push(JsValue::from(entry));
        }
        io.callback.clone()
    };
    if !entries.is_empty() {
        let entries = JsArray::from_iter(entries, cx);
        if let Err(e) = callback.call(&JsValue::undefined(), &[entries.into(), observer.clone().into()], cx) {
            report_error(e, cx);
        }
    }
    Ok(())
}

/// Real browsers' entry class (pages check its prototype before trusting
/// the observer; polyfills replace observers without it).
#[derive(Trace, Finalize, JsData)]
struct IntersectionObserverEntry;

impl Class for IntersectionObserverEntry {
    const NAME: &'static str = "IntersectionObserverEntry";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for k in ["target", "isIntersecting", "intersectionRatio", "boundingClientRect", "intersectionRect", "rootBounds", "time"] {
            class.property(JsString::from(k), JsValue::undefined(), Attribute::all());
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(Self)
    }

    fn object_constructor(instance: &JsObject<Self>, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
        if let Some(init) = args.first().and_then(|i| i.as_object()) {
            let instance = instance.clone().upcast();
            for key in init.own_property_keys(cx)? {
                let v = init.get(key.clone(), cx)?;
                instance.set(key, v, false, cx)?;
            }
        }
        Ok(())
    }
}

// ---- ResizeObserver ------------------------------------------------------

#[derive(Trace, Finalize)]
struct Measured {
    target: JsObject,
    /// The size last reported (`None`: none yet).
    #[unsafe_ignore_trace]
    last: Option<(f32, f32)>,
    /// Observed before it was laid out: scripts saw it as 0x0, so its first
    /// size is reported again (the first callback is often taken as the
    /// initial one).
    #[unsafe_ignore_trace]
    late: bool,
}

#[derive(Trace, Finalize, JsData)]
struct ResizeObserver {
    callback: JsObject,
    targets: GcRefCell<Vec<Measured>>,
}

impl Class for ResizeObserver {
    const NAME: &'static str = "ResizeObserver";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        class
            .method(js_string!("observe"), 1, NativeFunction::from_fn_ptr(ro_observe))
            .method(js_string!("unobserve"), 1, NativeFunction::from_fn_ptr(ro_unobserve))
            .method(js_string!("disconnect"), 0, NativeFunction::from_fn_ptr(ro_disconnect))
            .method(js_string!("takeRecords"), 0, NativeFunction::from_fn_ptr(|_, _, cx| Ok(JsArray::new(cx)?.into())));
        Ok(())
    }

    fn data_constructor(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<Self> {
        let callback = args.first().and_then(|c| c.as_callable()).ok_or_else(|| JsNativeError::typ().with_message("ResizeObserver needs a callback"))?;
        Ok(Self { callback, targets: GcRefCell::new(Vec::new()) })
    }
}

fn ro_observe(this: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let target = a.first().cloned().unwrap_or_default();
    let (Some(node), Some(target)) = (node_of(&target, cx), target.as_object()) else { return Ok(JsValue::undefined()) };
    let observer = this_object::<ResizeObserver>(this, "ResizeObserver")?;
    let ro = observer.downcast_ref::<ResizeObserver>().expect("checked");
    let laid_out = measure(&page(cx), node).is_some();
    {
        let mut targets = ro.targets.borrow_mut();
        targets.retain(|s| !JsObject::equals(&s.target, &target));
        targets.push(Measured { target, last: None, late: !laid_out });
    }
    drop(ro);
    with_live(cx, |l| add_live(&l.resize, &observer));
    if laid_out {
        later(0, NativeFunction::from_fn_ptr(|_, _, cx| deliver_resizes(cx).map(|_| JsValue::undefined())), cx);
    }
    Ok(JsValue::undefined())
}

fn ro_unobserve(this: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let observer = this_object::<ResizeObserver>(this, "ResizeObserver")?;
    let ro = observer.downcast_ref::<ResizeObserver>().expect("checked");
    if let Some(target) = a.first().and_then(|t| t.as_object()) {
        ro.targets.borrow_mut().retain(|s| !JsObject::equals(&s.target, &target));
    }
    Ok(JsValue::undefined())
}

fn ro_disconnect(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let observer = this_object::<ResizeObserver>(this, "ResizeObserver")?;
    let ro = observer.downcast_ref::<ResizeObserver>().expect("checked");
    ro.targets.borrow_mut().clear();
    drop(ro);
    with_live(cx, |l| remove_live(&l.resize, &observer));
    Ok(JsValue::undefined())
}

#[derive(Trace, Finalize, JsData)]
struct ResizeObserverEntry;

impl Class for ResizeObserverEntry {
    const NAME: &'static str = "ResizeObserverEntry";

    fn init(_: &mut ClassBuilder<'_>) -> JsResult<()> {
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

/// ResizeObserver callbacks for targets whose size is new.
fn deliver_resizes(cx: &mut Context) -> JsResult<()> {
    let d = page(cx);
    let observers = with_live(cx, |l| l.resize.borrow().clone());
    for observer in observers {
        let mut entries = Vec::new();
        let mut again = false;
        let callback = {
            let Some(ro) = observer.downcast_ref::<ResizeObserver>() else { continue };
            let targets: Vec<(JsObject, Option<(f32, f32)>)> = ro.targets.borrow().iter().map(|s| (s.target.clone(), s.last)).collect();
            for (target, last) in targets {
                let Some(node) = node_of(&target.clone().into(), cx) else { continue };
                let Some([_, _, w, h]) = d.borrow().geometry.rects.get(&node).copied() else { continue };
                if last == Some((w, h)) {
                    continue;
                }
                let mut late = false;
                if let Some(s) = ro.targets.borrow_mut().iter_mut().find(|s| JsObject::equals(&s.target, &target)) {
                    s.last = Some((w, h));
                    // A size that arrived after observe() also counts as a
                    // change from the 0x0 scripts saw.
                    if std::mem::take(&mut s.late) {
                        s.last = None;
                        late = true;
                    }
                }
                again |= late;
                let size = |cx: &mut Context| {
                    let s = record(&[("inlineSize", number(w)), ("blockSize", number(h))], cx);
                    JsValue::from(JsArray::from_iter([s.into()], cx))
                };
                let content_rect = dom_rect([0.0, 0.0, w, h], cx)?;
                let (border, content, device) = (size(cx), size(cx), size(cx));
                let entry = instance::<ResizeObserverEntry>(
                    &[
                        ("target", target.into()),
                        ("contentRect", content_rect),
                        ("borderBoxSize", border),
                        ("contentBoxSize", content),
                        ("devicePixelContentBoxSize", device),
                    ],
                    cx,
                );
                entries.push(JsValue::from(entry));
            }
            ro.callback.clone()
        };
        if !entries.is_empty() {
            let entries = JsArray::from_iter(entries, cx);
            if let Err(e) = callback.call(&JsValue::undefined(), &[entries.into(), observer.clone().into()], cx) {
                report_error(e, cx);
            }
        }
        if again {
            later(16, NativeFunction::from_fn_ptr(|_, _, cx| deliver_resizes(cx).map(|_| JsValue::undefined())), cx);
        }
    }
    Ok(())
}

/// A layout came: observers hear what changed.
pub(crate) fn deliver(cx: &mut Context) {
    if let Err(e) = deliver_resizes(cx) {
        report_error(e, cx);
    }
    let observers = with_live(cx, |l| l.intersection.borrow().clone());
    for o in observers {
        if let Err(e) = io_check(&o, None, cx) {
            report_error(e, cx);
        }
    }
}

/// Register the observer classes (globals), with the page they observe.
pub(crate) fn install(cx: &mut Context) {
    let list = JsObject::from_proto_and_data(None, Live::default());
    cx.insert_data(LiveList(list));
    for result in [
        cx.register_global_class::<IntersectionObserver>(),
        cx.register_global_class::<IntersectionObserverEntry>(),
        cx.register_global_class::<ResizeObserver>(),
        cx.register_global_class::<ResizeObserverEntry>(),
    ] {
        if let Err(e) = result {
            eprintln!("js: observers: {e}");
        }
    }
}
