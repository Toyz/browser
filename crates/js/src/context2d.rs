//! `CanvasRenderingContext2D`, `CanvasGradient` and `Path2D`, native: the
//! context keeps the state scripts read back (styles, transform, dash) and
//! records typed operations (`canvas::Op`) for its canvas, replayed onto
//! the bitmap before each snapshot (`draw_pending`). A child of `dom`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::{JsArray, JsUint8ClampedArray};
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};
use page::NodeId;
use tiny_skia::FillRule;

use super::{Dom, Shared, canvas_image, node_from_number, node_to_number, page};
use crate::canvas::{Canvas, Op, Source, color_fill, gradient_fill};

/// A context property's initial value.
#[derive(Clone, Copy)]
enum Initial {
    Str(&'static str),
    Num(f64),
    Bool(bool),
}

/// The context's properties, in the order their values are kept.
const PROPS: [(&str, Initial); 22] = [
    ("fillStyle", Initial::Str("#000000")),
    ("strokeStyle", Initial::Str("#000000")),
    ("lineWidth", Initial::Num(1.0)),
    ("lineCap", Initial::Str("butt")),
    ("lineJoin", Initial::Str("miter")),
    ("miterLimit", Initial::Num(10.0)),
    ("globalAlpha", Initial::Num(1.0)),
    ("globalCompositeOperation", Initial::Str("source-over")),
    ("lineDashOffset", Initial::Num(0.0)),
    ("font", Initial::Str("10px sans-serif")),
    ("textAlign", Initial::Str("start")),
    ("textBaseline", Initial::Str("alphabetic")),
    ("direction", Initial::Str("ltr")),
    ("shadowBlur", Initial::Num(0.0)),
    ("shadowColor", Initial::Str("rgba(0, 0, 0, 0)")),
    ("shadowOffsetX", Initial::Num(0.0)),
    ("shadowOffsetY", Initial::Num(0.0)),
    ("imageSmoothingEnabled", Initial::Bool(true)),
    ("imageSmoothingQuality", Initial::Str("low")),
    ("filter", Initial::Str("none")),
    ("letterSpacing", Initial::Str("0px")),
    ("fontKerning", Initial::Str("auto")),
];

fn initial_props() -> Vec<JsValue> {
    PROPS
        .iter()
        .map(|(_, v)| match *v {
            Initial::Str(s) => JsValue::from(JsString::from(s)),
            Initial::Num(n) => JsValue::from(n),
            Initial::Bool(b) => JsValue::from(b),
        })
        .collect()
}

const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(m: [f64; 6], n: [f64; 6]) -> [f64; 6] {
    [
        m[0] * n[0] + m[2] * n[1],
        m[1] * n[0] + m[3] * n[1],
        m[0] * n[2] + m[2] * n[3],
        m[1] * n[2] + m[3] * n[3],
        m[0] * n[4] + m[2] * n[5] + m[4],
        m[1] * n[4] + m[3] * n[5] + m[5],
    ]
}

#[derive(Trace, Finalize)]
struct Saved {
    props: Vec<JsValue>,
    #[unsafe_ignore_trace]
    matrix: [f64; 6],
    #[unsafe_ignore_trace]
    dash: Vec<f64>,
}

#[derive(Trace, Finalize, JsData)]
struct Context2d {
    canvas: JsObject,
    #[unsafe_ignore_trace]
    node: NodeId,
    props: GcRefCell<Vec<JsValue>>,
    #[unsafe_ignore_trace]
    matrix: Cell<[f64; 6]>,
    #[unsafe_ignore_trace]
    dash: RefCell<Vec<f64>>,
    stack: GcRefCell<Vec<Saved>>,
}

impl Context2d {
    fn reset(&self) {
        *self.props.borrow_mut() = initial_props();
        self.matrix.set(IDENTITY);
        self.dash.borrow_mut().clear();
        self.stack.borrow_mut().clear();
    }
}

/// `this`, as a context.
fn ctx_of(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<Context2d>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not a CanvasRenderingContext2D").into())
}

/// Run `f` on `this` context.
fn with_ctx<R>(this: &JsValue, f: impl FnOnce(&Context2d) -> R) -> JsResult<R> {
    let o = ctx_of(this)?;
    let c = o.downcast_ref::<Context2d>().expect("checked");
    Ok(f(&c))
}

/// Record operations for `this` context's canvas.
fn record(this: &JsValue, ops: impl IntoIterator<Item = Op>, cx: &Context) -> JsResult<()> {
    let node = with_ctx(this, |c| c.node)?;
    let d = page(cx);
    let mut dom = d.borrow_mut();
    dom.canvas_ops.entry(node).or_default().extend(ops);
    dom.canvases_dirty = true;
    Ok(())
}

/// The numbers among `a`; `None` if any of the first `need` isn't finite
/// (the call is ignored, as browsers do).
fn numbers(a: &[JsValue], need: usize, cx: &mut Context) -> JsResult<Option<Vec<f32>>> {
    let mut out = Vec::with_capacity(a.len());
    for (i, v) in a.iter().enumerate() {
        let n = v.to_number(cx)?;
        if i < need && !n.is_finite() {
            return Ok(None);
        }
        out.push(n as f32);
    }
    if out.len() < need {
        return Ok(None);
    }
    Ok(Some(out))
}

/// The path-building methods, shared by contexts and `Path2D`.
const PATH_METHODS: [(&str, usize); 10] = [
    ("moveTo", 2),
    ("lineTo", 2),
    ("closePath", 0),
    ("quadraticCurveTo", 4),
    ("bezierCurveTo", 6),
    ("arc", 5),
    ("arcTo", 5),
    ("ellipse", 7),
    ("rect", 4),
    ("roundRect", 4),
];

/// The operations a path method adds.
fn path_ops(name: &str, a: &[JsValue], cx: &mut Context) -> JsResult<Vec<Op>> {
    let need = PATH_METHODS.iter().find(|(n, _)| *n == name).map_or(0, |(_, need)| *need);
    // roundRect's radii may be a list: its first.
    let radius = if name == "roundRect" {
        let r = a.get(4).cloned().unwrap_or_default();
        let r = match r.as_object().and_then(|o| JsArray::from_object(o).ok()) {
            Some(list) => list.get(0, cx)?,
            None => r,
        };
        if r.is_undefined() { 0.0 } else { r.to_number(cx)? as f32 }
    } else {
        0.0
    };
    let Some(n) = numbers(a, need, cx)? else {
        return Ok(Vec::new());
    };
    let ccw = |i: usize| a.get(i).is_some_and(|v| v.to_boolean());
    Ok(match name {
        "moveTo" => vec![Op::MoveTo(n[0], n[1])],
        "lineTo" => vec![Op::LineTo(n[0], n[1])],
        "closePath" => vec![Op::ClosePath],
        "quadraticCurveTo" => vec![Op::Quad([n[0], n[1], n[2], n[3]])],
        "bezierCurveTo" => vec![Op::Bezier([n[0], n[1], n[2], n[3], n[4], n[5]])],
        "arc" => vec![Op::Ellipse([n[0], n[1], n[2], n[2], 0.0, n[3], n[4]], ccw(5))],
        // (Approximated: the corner, not the arc into it.)
        "arcTo" => vec![Op::LineTo(n[0], n[1]), Op::LineTo(n[2], n[3])],
        "ellipse" => vec![Op::Ellipse([n[0], n[1], n[2], n[3], n[4], n[5], n[6]], ccw(7))],
        "rect" => vec![Op::Rect([n[0], n[1], n[2], n[3]])],
        "roundRect" => vec![Op::RoundRect([n[0], n[1], n[2], n[3]], radius)],
        _ => Vec::new(),
    })
}

/// A path's operations (a `Path2D` argument), if `v` is one.
fn path_of(v: Option<&JsValue>) -> Option<Vec<Op>> {
    let o = v?.as_object()?;
    let p = o.downcast_ref::<Path2D>()?;
    Some(p.ops.borrow().clone())
}

/// fill/stroke/clip: the given path (replacing the current one) or the
/// current path, then `op`.
fn draw_path(this: &JsValue, path: Option<&JsValue>, op: Op, cx: &mut Context) -> JsResult<JsValue> {
    let ops = match path_of(path) {
        Some(p) => std::iter::once(Op::BeginPath).chain(p).chain([op]).collect(),
        None => vec![op],
    };
    record(this, ops, cx)?;
    Ok(JsValue::undefined())
}

fn rule_of(v: Option<&JsValue>) -> FillRule {
    match v.and_then(|v| v.as_string()).map(|s| s.to_std_string_escaped()) {
        Some(s) if s == "evenodd" => FillRule::EvenOdd,
        _ => FillRule::Winding,
    }
}

/// A property was set: kept (if valid), and recorded if drawing uses it.
fn set_prop(this: &JsValue, i: usize, v: &JsValue, cx: &mut Context) -> JsResult<()> {
    let (name, initial) = PROPS[i];
    let text = |cx: &mut Context| v.to_string(cx).map(|s| s.to_std_string_escaped());
    let num = |cx: &mut Context| v.to_number(cx);
    let (keep, op): (Option<JsValue>, Option<Op>) = match name {
        "fillStyle" | "strokeStyle" => {
            let fill = match v.as_object().as_ref().and_then(|o| o.downcast_ref::<CanvasGradient>().map(|g| g.fill())) {
                Some(fill) => Some(fill),
                None if v.is_string() => color_fill(&text(cx)?),
                None => None,
            };
            match fill {
                Some(f) => (Some(v.clone()), Some(if name == "fillStyle" { Op::FillStyle(f) } else { Op::StrokeStyle(f) })),
                None => (None, None),
            }
        }
        "lineWidth" | "miterLimit" => {
            let n = num(cx)?;
            if n.is_finite() && n > 0.0 {
                (Some(n.into()), Some(if name == "lineWidth" { Op::LineWidth(n as f32) } else { Op::Miter(n as f32) }))
            } else {
                (None, None)
            }
        }
        "globalAlpha" => {
            let n = num(cx)?;
            if (0.0..=1.0).contains(&n) { (Some(n.into()), Some(Op::Alpha(n as f32))) } else { (None, None) }
        }
        "lineDashOffset" => {
            let n = num(cx)?;
            if n.is_finite() { (Some(n.into()), Some(Op::DashOffset(n as f32))) } else { (None, None) }
        }
        "lineCap" | "lineJoin" | "globalCompositeOperation" => {
            let s = text(cx)?;
            let op = match name {
                "lineCap" => Op::cap(&s).map(Op::Cap),
                "lineJoin" => Op::join(&s).map(Op::Join),
                _ => Op::blend(&s).map(Op::Blend),
            };
            match op {
                Some(op) => (Some(JsString::from(s).into()), Some(op)),
                None => (None, None),
            }
        }
        _ => match initial {
            Initial::Num(_) => (Some(num(cx)?.into()), None),
            Initial::Bool(_) => (Some(v.to_boolean().into()), None),
            Initial::Str(_) => (Some(JsString::from(text(cx)?).into()), None),
        },
    };
    if let Some(keep) = keep {
        with_ctx(this, |c| c.props.borrow_mut()[i] = keep)?;
    }
    if let Some(op) = op {
        record(this, [op], cx)?;
    }
    Ok(())
}

/// The canvas element an `<img>`-or-canvas argument of `drawImage` is, or
/// its image's address.
fn image_source(v: &JsValue, cx: &mut Context) -> JsResult<Option<Source>> {
    let Some(o) = v.as_object() else { return Ok(None) };
    let d = page(cx);
    let id = o.get(js_string!("__featherNode"), cx)?.as_number().map(node_from_number);
    if let Some(id) = id {
        let dom = d.borrow();
        if let Some(e) = dom.doc.element(id) {
            if &*e.name.local == "canvas" {
                return Ok(Some(Source::Canvas(node_to_number(id) as u64)));
            }
            let current = o.get(js_string!("currentSrc"), cx).ok().filter(|s| s.is_string()).map(|s| s.as_string().unwrap().to_std_string_escaped());
            if let Some(src) = current.filter(|s| !s.is_empty()) {
                return Ok(Some(Source::Url(src)));
            }
            if let Some(url) = e.attr("src").and_then(|s| dom.doc.resolve_url(s)) {
                return Ok(Some(Source::Url(url.to_string())));
            }
        }
    }
    let src = o.get(js_string!("src"), cx)?;
    Ok(src.as_string().map(|s| s.to_std_string_escaped()).filter(|s| !s.is_empty()).map(Source::Url))
}

/// A new `ImageData` (the page's class) over `rgba`.
fn image_data(w: u32, h: u32, rgba: Vec<u8>, cx: &mut Context) -> JsResult<JsValue> {
    let data = JsUint8ClampedArray::from_iter(rgba, cx)?;
    let class = cx.global_object().get(js_string!("ImageData"), cx)?;
    match class.as_constructor() {
        Some(class) => Ok(class.construct(&[data.into(), w.into(), h.into()], None, cx)?.into()),
        None => {
            let o = JsObject::with_object_proto(cx.intrinsics());
            o.set(js_string!("width"), w, false, cx)?;
            o.set(js_string!("height"), h, false, cx)?;
            o.set(js_string!("data"), data, false, cx)?;
            Ok(o.into())
        }
    }
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn undefined(_: ()) -> JsValue {
    JsValue::undefined()
}

fn transform(this: &JsValue, m: [f64; 6], set: bool, cx: &mut Context) -> JsResult<JsValue> {
    with_ctx(this, |c| c.matrix.set(if set { m } else { mul(c.matrix.get(), m) }))?;
    let f = m.map(|v| v as f32);
    record(this, [if set { Op::SetTransform(f) } else { Op::Transform(f) }], cx).map(undefined)
}

/// The context's own methods (not the path ones).
const METHODS: &[(&str, usize, Method)] = &[
    ("save", 0, |this, _, cx| {
        with_ctx(this, |c| {
            let saved = Saved { props: c.props.borrow().clone(), matrix: c.matrix.get(), dash: c.dash.borrow().clone() };
            c.stack.borrow_mut().push(saved);
        })?;
        record(this, [Op::Save], cx).map(undefined)
    }),
    ("restore", 0, |this, _, cx| {
        with_ctx(this, |c| {
            if let Some(mut s) = c.stack.borrow_mut().pop() {
                *c.props.borrow_mut() = std::mem::take(&mut s.props);
                c.matrix.set(s.matrix);
                *c.dash.borrow_mut() = std::mem::take(&mut s.dash);
            }
        })?;
        record(this, [Op::Restore], cx).map(undefined)
    }),
    ("reset", 0, |this, _, cx| {
        with_ctx(this, Context2d::reset)?;
        record(this, [Op::Reset], cx).map(undefined)
    }),
    ("scale", 2, |this, a, cx| match numbers(a, 2, cx)? {
        Some(n) => transform(this, [n[0] as f64, 0.0, 0.0, n[1] as f64, 0.0, 0.0], false, cx),
        None => Ok(JsValue::undefined()),
    }),
    ("rotate", 1, |this, a, cx| match numbers(a, 1, cx)? {
        Some(n) => {
            let (s, c) = (n[0] as f64).sin_cos();
            transform(this, [c, s, -s, c, 0.0, 0.0], false, cx)
        }
        None => Ok(JsValue::undefined()),
    }),
    ("translate", 2, |this, a, cx| match numbers(a, 2, cx)? {
        Some(n) => transform(this, [1.0, 0.0, 0.0, 1.0, n[0] as f64, n[1] as f64], false, cx),
        None => Ok(JsValue::undefined()),
    }),
    ("transform", 6, |this, a, cx| match numbers(a, 6, cx)? {
        Some(n) => transform(this, [n[0], n[1], n[2], n[3], n[4], n[5]].map(f64::from), false, cx),
        None => Ok(JsValue::undefined()),
    }),
    ("setTransform", 6, |this, a, cx| {
        // (a, b, c, d, e, f), a matrix object, or nothing (identity).
        let m = match a.first().and_then(|v| v.as_object()) {
            Some(o) => {
                let mut m = [0.0; 6];
                for (i, k) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
                    let v = o.get(JsString::from(k), cx)?;
                    m[i] = if v.is_undefined() { IDENTITY[i] } else { v.to_number(cx)? };
                }
                m
            }
            None if a.is_empty() => IDENTITY,
            None => match numbers(a, 6, cx)? {
                Some(n) => [n[0], n[1], n[2], n[3], n[4], n[5]].map(f64::from),
                None => return Ok(JsValue::undefined()),
            },
        };
        transform(this, m, true, cx)
    }),
    ("resetTransform", 0, |this, _, cx| transform(this, IDENTITY, true, cx)),
    ("getTransform", 0, |this, _, cx| {
        let [a, b, c, d, e, f] = with_ctx(this, |c| c.matrix.get())?;
        let o = JsObject::with_object_proto(cx.intrinsics());
        for (k, v) in [("a", a), ("b", b), ("c", c), ("d", d), ("e", e), ("f", f), ("m11", a), ("m12", b), ("m21", c), ("m22", d), ("m41", e), ("m42", f)] {
            o.set(JsString::from(k), v, false, cx)?;
        }
        o.set(js_string!("is2D"), true, false, cx)?;
        o.set(js_string!("isIdentity"), [a, b, c, d, e, f] == IDENTITY, false, cx)?;
        Ok(o.into())
    }),
    ("beginPath", 0, |this, _, cx| record(this, [Op::BeginPath], cx).map(undefined)),
    ("fill", 0, |this, a, cx| {
        let path = a.first().filter(|v| v.is_object());
        let rule = rule_of(if path.is_some() { a.get(1) } else { a.first() });
        draw_path(this, path, Op::Fill(rule), cx)
    }),
    ("stroke", 0, |this, a, cx| draw_path(this, a.first(), Op::Stroke, cx)),
    ("clip", 0, |this, a, cx| draw_path(this, a.first().filter(|v| v.is_object()), Op::Clip, cx)),
    ("fillRect", 4, |this, a, cx| match numbers(a, 4, cx)? {
        Some(n) => record(this, [Op::FillRect([n[0], n[1], n[2], n[3]])], cx).map(undefined),
        None => Ok(JsValue::undefined()),
    }),
    ("strokeRect", 4, |this, a, cx| match numbers(a, 4, cx)? {
        Some(n) => record(this, [Op::StrokeRect([n[0], n[1], n[2], n[3]])], cx).map(undefined),
        None => Ok(JsValue::undefined()),
    }),
    ("clearRect", 4, |this, a, cx| match numbers(a, 4, cx)? {
        Some(n) => record(this, [Op::ClearRect([n[0], n[1], n[2], n[3]])], cx).map(undefined),
        None => Ok(JsValue::undefined()),
    }),
    ("isPointInPath", 2, |_, _, _| Ok(false.into())),
    ("isPointInStroke", 2, |_, _, _| Ok(false.into())),
    ("setLineDash", 1, |this, a, cx| {
        let Some(list) = a.first().and_then(|v| v.as_object()).and_then(|o| JsArray::from_object(o).ok()) else { return Ok(JsValue::undefined()) };
        let mut dash = Vec::new();
        for i in 0..list.length(cx)? {
            let n = list.get(i, cx)?.to_number(cx)?;
            if !n.is_finite() || n < 0.0 {
                return Ok(JsValue::undefined());
            }
            dash.push(n);
        }
        let op = Op::Dash(dash.iter().map(|&v| v as f32).collect());
        with_ctx(this, |c| *c.dash.borrow_mut() = dash)?;
        record(this, [op], cx).map(undefined)
    }),
    ("getLineDash", 0, |this, _, cx| {
        let dash = with_ctx(this, |c| c.dash.borrow().clone())?;
        Ok(JsArray::from_iter(dash.into_iter().map(JsValue::from), cx).into())
    }),
    // Text isn't drawn yet.
    ("fillText", 3, |_, _, _| Ok(JsValue::undefined())),
    ("strokeText", 3, |_, _, _| Ok(JsValue::undefined())),
    ("measureText", 1, |this, a, cx| {
        let text = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
        let font = with_ctx(this, |c| c.props.borrow()[9].clone())?.to_string(cx)?.to_std_string_escaped();
        let size = font
            .split_whitespace()
            .find_map(|w| w.strip_suffix("px").and_then(|n| n.split('/').next()).and_then(|n| n.parse::<f64>().ok()))
            .unwrap_or(10.0);
        let width = text.chars().count() as f64 * size * 0.55;
        let o = JsObject::with_object_proto(cx.intrinsics());
        for (k, v) in [
            ("width", width),
            ("actualBoundingBoxAscent", size * 0.8),
            ("actualBoundingBoxDescent", size * 0.2),
            ("actualBoundingBoxLeft", 0.0),
            ("actualBoundingBoxRight", width),
            ("fontBoundingBoxAscent", size * 0.8),
            ("fontBoundingBoxDescent", size * 0.2),
        ] {
            o.set(JsString::from(k), v, false, cx)?;
        }
        Ok(o.into())
    }),
    ("drawImage", 3, |this, a, cx| {
        let Some(mut source) = image_source(&a.first().cloned().unwrap_or_default(), cx)? else { return Ok(JsValue::undefined()) };
        let Some(rest) = numbers(&a[1.min(a.len())..], 0, cx)? else { return Ok(JsValue::undefined()) };
        // A canvas is drawn as it is now: what's been drawn on it so far
        // (painted first), not what it holds when this canvas is painted.
        // Google Maps draws each label on one small canvas, copies it onto
        // the map, and clears it for the next: replayed in no particular
        // order, the copies were blank.
        if let Source::Canvas(key) = source
            && let Some(pixels) = snapshot(&page(cx), node_from_number(key as f64))
        {
            source = Source::Pixels(pixels);
        }
        record(this, [Op::DrawImage(source, rest)], cx).map(undefined)
    }),
    ("createLinearGradient", 4, |_, a, cx| {
        let n = numbers(a, 4, cx)?.ok_or_else(|| JsNativeError::typ().with_message("non-finite gradient coordinates"))?;
        gradient(false, n, cx)
    }),
    ("createRadialGradient", 6, |_, a, cx| {
        let n = numbers(a, 6, cx)?.ok_or_else(|| JsNativeError::typ().with_message("non-finite gradient coordinates"))?;
        gradient(true, n, cx)
    }),
    // (Drawn as a radial one from the center.)
    ("createConicGradient", 3, |_, a, cx| {
        let n = numbers(a, 3, cx)?.ok_or_else(|| JsNativeError::typ().with_message("non-finite gradient coordinates"))?;
        gradient(true, vec![n[1], n[2], 0.0, n[1], n[2], 1.0], cx)
    }),
    ("createPattern", 2, |_, _, _| Ok(JsValue::null())),
    ("createImageData", 2, |_, a, cx| {
        let (w, h) = match a.first().and_then(|v| v.as_object()) {
            Some(other) => (other.get(js_string!("width"), cx)?.to_u32(cx)?, other.get(js_string!("height"), cx)?.to_u32(cx)?),
            None => (a.first().cloned().unwrap_or_default().to_u32(cx)?, a.get(1).cloned().unwrap_or_default().to_u32(cx)?),
        };
        image_data(w, h, vec![0; w as usize * h as usize * 4], cx)
    }),
    ("getImageData", 4, |this, a, cx| {
        let n = numbers(a, 4, cx)?.ok_or_else(|| JsNativeError::typ().with_message("non-finite coordinates"))?;
        let (w, h) = (n[2].abs().max(1.0) as u32, n[3].abs().max(1.0) as u32);
        let node = with_ctx(this, |c| c.node)?;
        let d = page(cx);
        draw_pending(&d);
        let rgba = d.borrow().canvases.get(&node).map_or_else(|| vec![0; w as usize * h as usize * 4], |c: &Canvas| c.image_data(n[0] as i32, n[1] as i32, w, h));
        image_data(w, h, rgba, cx)
    }),
    ("putImageData", 3, |this, a, cx| {
        let Some(img) = a.first().and_then(|v| v.as_object()) else { return Ok(JsValue::undefined()) };
        let Some(n) = numbers(&a[1.min(a.len())..], 2, cx)? else { return Ok(JsValue::undefined()) };
        let (w, h) = (img.get(js_string!("width"), cx)?.to_u32(cx)?, img.get(js_string!("height"), cx)?.to_u32(cx)?);
        let data = img.get(js_string!("data"), cx)?;
        let Some(data) = data.as_object().and_then(|o| JsUint8ClampedArray::from_object(o).ok()) else { return Ok(JsValue::undefined()) };
        let mut rgba: Vec<u8> = data.iter(cx).collect();
        rgba.resize(w as usize * h as usize * 4, 0);
        record(this, [Op::PutPixels { x: n[0] as i32, y: n[1] as i32, w, h, rgba }], cx).map(undefined)
    }),
    ("getContextAttributes", 0, |_, _, cx| {
        let o = JsObject::with_object_proto(cx.intrinsics());
        o.set(js_string!("alpha"), true, false, cx)?;
        o.set(js_string!("desynchronized"), false, false, cx)?;
        o.set(js_string!("colorSpace"), js_string!("srgb"), false, cx)?;
        o.set(js_string!("willReadFrequently"), false, false, cx)?;
        Ok(o.into())
    }),
];

impl Class for Context2d {
    const NAME: &'static str = "CanvasRenderingContext2D";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for &(name, len, f) in METHODS {
            class.method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        }
        for &(name, len) in &PATH_METHODS {
            let f = NativeFunction::from_copy_closure(move |this, a, cx| {
                let ops = path_ops(name, a, cx)?;
                record(this, ops, cx).map(undefined)
            });
            class.method(JsString::from(name), len, f);
        }
        for (i, (name, _)) in PROPS.iter().enumerate() {
            let get = NativeFunction::from_copy_closure(move |this, _, _| with_ctx(this, |c| c.props.borrow()[i].clone()));
            let set = NativeFunction::from_copy_closure(move |this, a, cx| set_prop(this, i, &a.first().cloned().unwrap_or_default(), cx).map(undefined));
            let realm = class.context().realm().clone();
            let get = FunctionObjectBuilder::new(&realm, get).name(JsString::from(*name)).build();
            let set = FunctionObjectBuilder::new(&realm, set).name(JsString::from(*name)).build();
            class.accessor(JsString::from(*name), Some(get), Some(set), Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        }
        let canvas = NativeFunction::from_fn_ptr(|this, _, _| with_ctx(this, |c| JsValue::from(c.canvas.clone())));
        let realm = class.context().realm().clone();
        let canvas = FunctionObjectBuilder::new(&realm, canvas).name(js_string!("canvas")).build();
        class.accessor(js_string!("canvas"), Some(canvas), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

// ---- CanvasGradient ------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct CanvasGradient {
    #[unsafe_ignore_trace]
    radial: bool,
    #[unsafe_ignore_trace]
    at: Vec<f32>,
    #[unsafe_ignore_trace]
    stops: RefCell<Vec<(f32, String)>>,
}

impl CanvasGradient {
    fn fill(&self) -> crate::canvas::Fill {
        gradient_fill(self.radial, &self.at, &self.stops.borrow())
    }
}

fn gradient(radial: bool, at: Vec<f32>, cx: &mut Context) -> JsResult<JsValue> {
    let proto = cx.get_global_class::<CanvasGradient>().map(|c| c.prototype());
    Ok(JsObject::from_proto_and_data(proto, CanvasGradient { radial, at, stops: RefCell::new(Vec::new()) }).into())
}

impl Class for CanvasGradient {
    const NAME: &'static str = "CanvasGradient";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let add = NativeFunction::from_fn_ptr(|this, a, cx| {
            let offset = a.first().cloned().unwrap_or_default().to_number(cx)?;
            let color = a.get(1).cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
            if !(0.0..=1.0).contains(&offset) {
                return Err(JsNativeError::range().with_message("IndexSizeError: offset out of range").into());
            }
            let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a CanvasGradient"))?;
            let g = o.downcast_ref::<CanvasGradient>().ok_or_else(|| JsNativeError::typ().with_message("not a CanvasGradient"))?;
            g.stops.borrow_mut().push((offset as f32, color));
            Ok(JsValue::undefined())
        });
        class.method(js_string!("addColorStop"), 2, add);
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

// ---- Path2D --------------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct Path2D {
    #[unsafe_ignore_trace]
    ops: RefCell<Vec<Op>>,
}

impl Class for Path2D {
    const NAME: &'static str = "Path2D";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for &(name, len) in &PATH_METHODS {
            let f = NativeFunction::from_copy_closure(move |this, a, cx| {
                let ops = path_ops(name, a, cx)?;
                let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a Path2D"))?;
                let p = o.downcast_ref::<Path2D>().ok_or_else(|| JsNativeError::typ().with_message("not a Path2D"))?;
                p.ops.borrow_mut().extend(ops);
                Ok(JsValue::undefined())
            });
            class.method(JsString::from(name), len, f);
        }
        let add = NativeFunction::from_fn_ptr(|this, a, _| {
            let other = path_of(a.first()).unwrap_or_default();
            if let Some(p) = this.as_object().as_ref().and_then(|o| o.downcast_ref::<Path2D>()) {
                p.ops.borrow_mut().extend(other);
            }
            Ok(JsValue::undefined())
        });
        class.method(js_string!("addPath"), 1, add);
        Ok(())
    }

    /// From another path (a copy); SVG path data isn't read.
    fn data_constructor(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(Path2D { ops: RefCell::new(path_of(a.first()).unwrap_or_default()) })
    }
}

// ---- the canvas element's side -------------------------------------------

/// `canvas.getContext("2d")`: its context (made the first time).
fn context_for(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some(canvas) = a.first().and_then(|v| v.as_object()) else { return Ok(JsValue::null()) };
    let existing = canvas.get(js_string!("__ctx"), cx)?;
    if existing.is_object() {
        return Ok(existing);
    }
    let Some(node) = canvas.get(js_string!("__featherNode"), cx)?.as_number().map(node_from_number) else { return Ok(JsValue::null()) };
    let data = Context2d {
        canvas: canvas.clone(),
        node,
        props: GcRefCell::new(initial_props()),
        matrix: Cell::new(IDENTITY),
        dash: RefCell::new(Vec::new()),
        stack: GcRefCell::new(Vec::new()),
    };
    let proto = cx.get_global_class::<Context2d>().map(|c| c.prototype());
    let ctx = JsObject::from_proto_and_data(proto, data);
    canvas.define_property_or_throw(js_string!("__ctx"), PropertyDescriptor::builder().value(ctx.clone()).writable(false).enumerable(false).configurable(false), cx)?;
    Ok(ctx.into())
}

/// The canvas's size was set: its bitmap is blank, its context as new.
fn reset(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some(canvas) = a.first().and_then(|v| v.as_object()) else { return Ok(JsValue::undefined()) };
    let Some(node) = canvas.get(js_string!("__featherNode"), cx)?.as_number().map(node_from_number) else { return Ok(JsValue::undefined()) };
    let ctx = canvas.get(js_string!("__ctx"), cx)?;
    if let Some(c) = ctx.as_object().as_ref().and_then(|o| o.downcast_ref::<Context2d>()) {
        c.reset();
    }
    let d = page(cx);
    let mut dom = d.borrow_mut();
    let ops = dom.canvas_ops.entry(node).or_default();
    ops.clear();
    ops.push(Op::Reset);
    dom.canvases_dirty = true;
    Ok(JsValue::undefined())
}

/// `canvas.toDataURL()`: its drawing so far, as a PNG.
fn data_url(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let node = match a.first().and_then(|v| v.as_object()) {
        Some(c) => c.get(js_string!("__featherNode"), cx)?.as_number().map(node_from_number),
        None => None,
    };
    let d = page(cx);
    draw_pending(&d);
    let url = node.and_then(|n| d.borrow_mut().canvases.get_mut(&n).map(|c| c.data_url()));
    Ok(JsString::from(url.as_deref().unwrap_or("data:,")).into())
}

/// A canvas's size, from its attributes (300 x 150 unless set).
fn canvas_size(dom: &Dom, node: NodeId) -> (u32, u32) {
    let e = dom.doc.element(node);
    let size = |name: &str, default: f64| match e.and_then(|e| e.attr(name)) {
        None => default,
        Some(v) => v.trim().parse::<f64>().unwrap_or(0.0),
    };
    (size("width", 300.0).max(0.0) as u32, size("height", 150.0).max(0.0) as u32)
}

/// Canvas `node`'s pixels now: what's recorded for it painted first.
fn snapshot(d: &Shared, node: NodeId) -> Option<std::sync::Arc<tiny_skia::Pixmap>> {
    let mut dom = d.borrow_mut();
    if let Some(ops) = dom.canvas_ops.remove(&node) {
        let base = dom.doc.url.clone();
        apply_ops(&mut dom, &base, node, ops);
    }
    dom.canvases.get(&node).map(|c| std::sync::Arc::new(c.pixmap().clone()))
}

/// Replay the operations recorded since the last time onto the canvases'
/// bitmaps.
pub(crate) fn draw_pending(d: &Shared) {
    let mut dom = d.borrow_mut();
    if dom.canvas_ops.is_empty() {
        return;
    }
    let pending: Vec<(NodeId, Vec<Op>)> = dom.canvas_ops.drain().collect();
    let base = dom.doc.url.clone();
    for (node, ops) in pending {
        apply_ops(&mut dom, &base, node, ops);
    }
}

/// Paint `ops` onto canvas `node`'s bitmap.
fn apply_ops(dom: &mut Dom, base: &url::Url, node: NodeId, ops: Vec<Op>) {
    {
        if dom.doc.get(node).is_none() {
            return;
        }
        dom.drawn.insert(node);
        let (w, h) = canvas_size(&dom, node);
        // A new size is a new, blank bitmap (and a DOM change: the
        // element's picture changes size).
        if dom.canvases.get(&node).is_none_or(|c| c.size() != (w.max(1), h.max(1)))
            && let Some(c) = Canvas::new(w, h)
        {
            dom.canvases.insert(node, c);
            dom.changed();
        }
        // Other canvases, for drawImage(canvas).
        let others: HashMap<u64, tiny_skia::Pixmap> = ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawImage(Source::Canvas(key), _) => Some((*key, dom.canvases.get(&node_from_number(*key as f64))?.pixmap().clone())),
                _ => None,
            })
            .collect();
        if let Some(canvas) = dom.canvases.get_mut(&node) {
            canvas.apply(&ops, &others, &|src| canvas_image(base, src));
        }
    }
}

/// Register the classes, and the canvas element's natives (`context2d`,
/// `canvasReset`, `canvasDataURL`).
pub(crate) fn install(natives: &JsObject, cx: &mut Context) {
    for result in [cx.register_global_class::<Context2d>(), cx.register_global_class::<CanvasGradient>(), cx.register_global_class::<Path2D>()] {
        if let Err(e) = result {
            eprintln!("js: canvas: {e}");
        }
    }
    let fns: [(&str, Method); 3] = [("context2d", context_for), ("canvasReset", reset), ("canvasDataURL", data_url)];
    for (name, f) in fns {
        let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(f)).name(JsString::from(name)).build();
        natives.set(JsString::from(name), f, false, cx).ok();
    }
}
