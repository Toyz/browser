//! `DOMRect`, `DOMPoint` and `DOMMatrix`, with their read-only bases,
//! native. Each pair shares one value type: `ReadOnly<V>` and `Writable<V>`
//! hold a `V`, the read-only prototype has the getters and the methods that
//! return new values, and the writable one (whose prototype chain goes
//! through the read-only one) adds the setters and the in-place methods.

use std::cell::Cell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::{JsFloat32Array, JsFloat64Array};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace, empty_trace};

/// A value the classes hold.
trait Geo: Copy + 'static {
    /// The read-only class's name and the writable one's.
    const NAMES: [&'static str; 2];
    const LENGTH: usize;
    fn from_args(a: &[JsValue], cx: &mut Context) -> JsResult<Self>;
    /// Accessors, methods and statics: `writable` for the writable class.
    fn init(class: &mut ClassBuilder<'_>, writable: bool) -> JsResult<()>;
}

struct ReadOnly<V>(Cell<V>);
struct Writable<V>(Cell<V>);

macro_rules! holder {
    ($t:ident, $i:literal) => {
        impl<V> Finalize for $t<V> {}
        // SAFETY: the value is plain numbers, no GC pointers.
        unsafe impl<V: 'static> Trace for $t<V> {
            empty_trace!();
        }
        impl<V> JsData for $t<V> {}
        impl<V: Geo> Class for $t<V> {
            const NAME: &'static str = V::NAMES[$i];
            const LENGTH: usize = V::LENGTH;
            fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
                V::init(class, $i == 1)
            }
            fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
                Ok(Self(Cell::new(V::from_args(a, cx)?)))
            }
        }
    };
}
holder!(ReadOnly, 0);
holder!(Writable, 1);

fn type_error(message: String) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message).into()
}

/// The value `this` holds (either class).
fn get<V: Geo>(this: &JsValue) -> JsResult<V> {
    if let Some(o) = this.as_object() {
        if let Some(d) = o.downcast_ref::<Writable<V>>() {
            return Ok(d.0.get());
        }
        if let Some(d) = o.downcast_ref::<ReadOnly<V>>() {
            return Ok(d.0.get());
        }
    }
    Err(type_error(format!("not a {}", V::NAMES[0])))
}

/// Change the value a writable `this` holds.
fn update<V: Geo>(this: &JsValue, f: impl FnOnce(&mut V)) -> JsResult<()> {
    let o = this.as_object();
    let d = o.as_ref().and_then(|o| o.downcast_ref::<Writable<V>>());
    let d = d.ok_or_else(|| type_error(format!("not a {}", V::NAMES[1])))?;
    let mut v = d.0.get();
    f(&mut v);
    d.0.set(v);
    Ok(())
}

/// A new instance holding `v`.
fn make<V: Geo>(v: V, writable: bool, cx: &mut Context) -> JsResult<JsValue> {
    let o: JsObject = if writable {
        Writable::from_data(Writable(Cell::new(v)), cx)?
    } else {
        ReadOnly::from_data(ReadOnly(Cell::new(v)), cx)?
    };
    Ok(o.into())
}

/// Argument `i` as a number, `default` when missing or undefined.
fn num(a: &[JsValue], i: usize, default: f64, cx: &mut Context) -> JsResult<f64> {
    match a.get(i) {
        Some(v) if !v.is_undefined() => v.to_number(cx),
        _ => Ok(default),
    }
}

/// Property `key` of a dictionary as a number, `default` when undefined.
fn member(o: Option<&JsObject>, key: &str, default: f64, cx: &mut Context) -> JsResult<f64> {
    let Some(o) = o else { return Ok(default) };
    let v = o.get(JsString::from(key), cx)?;
    if v.is_undefined() { Ok(default) } else { v.to_number(cx) }
}

fn function(class: &mut ClassBuilder<'_>, name: &str, length: usize, f: NativeFunction) -> boa_engine::object::builtins::JsFunction {
    let realm = class.context().realm().clone();
    FunctionObjectBuilder::new(&realm, f).name(JsString::from(name)).length(length).build()
}

fn accessor(class: &mut ClassBuilder<'_>, name: &str, get: NativeFunction, set: Option<NativeFunction>) {
    let get = function(class, &format!("get {name}"), 0, get);
    let set = set.map(|s| function(class, &format!("set {name}"), 1, s));
    class.accessor(JsString::from(name), Some(get), set, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

/// Numeric accessors `names[i]` reading `read(v, i)` (and, on the writable
/// class, writing through `write`).
fn numbers<V: Geo>(class: &mut ClassBuilder<'_>, names: &[&str], writable: bool, read: fn(&V, usize) -> f64, write: fn(&mut V, usize, f64)) {
    for (i, name) in names.iter().enumerate() {
        let get = NativeFunction::from_copy_closure(move |this, _, _| Ok(read(&get::<V>(this)?, i).into()));
        let set = writable.then(|| {
            NativeFunction::from_copy_closure(move |this, a, cx| {
                let n = num(a, 0, f64::NAN, cx)?;
                update::<V>(this, |v| write(v, i, n))?;
                Ok(JsValue::undefined())
            })
        });
        accessor(class, name, get, set);
    }
}

/// A plain object with these number (or boolean) members.
fn record(pairs: impl IntoIterator<Item = (&'static str, JsValue)>, cx: &mut Context) -> JsResult<JsValue> {
    let o = JsObject::with_object_proto(cx.intrinsics());
    for (k, v) in pairs {
        o.set(JsString::from(k), v, false, cx)?;
    }
    Ok(o.into())
}

/// `Math.min`/`Math.max`: NaN wins.
fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}
fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// Add `(name, length, f)` methods (statics with `statics`).
fn methods(class: &mut ClassBuilder<'_>, list: &[(&str, usize, Method)], statics: bool) {
    for &(name, len, f) in list {
        if statics {
            class.static_method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        } else {
            class.method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        }
    }
}

// ---- DOMRect --------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) struct Rect([f64; 4]);

impl Rect {
    fn edges(&self) -> [f64; 4] {
        let [x, y, w, h] = self.0;
        [min(y, y + h), max(x, x + w), max(y, y + h), min(x, x + w)]
    }

    fn from_init(init: Option<&JsValue>, cx: &mut Context) -> JsResult<Rect> {
        let o = init.and_then(JsValue::as_object);
        let mut v = [0.0; 4];
        for (slot, key) in v.iter_mut().zip(["x", "y", "width", "height"]) {
            *slot = member(o.as_ref(), key, 0.0, cx)?;
        }
        Ok(Rect(v))
    }
}

impl Geo for Rect {
    const NAMES: [&'static str; 2] = ["DOMRectReadOnly", "DOMRect"];
    const LENGTH: usize = 0;

    fn from_args(a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        Ok(Rect([num(a, 0, 0.0, cx)?, num(a, 1, 0.0, cx)?, num(a, 2, 0.0, cx)?, num(a, 3, 0.0, cx)?]))
    }

    fn init(class: &mut ClassBuilder<'_>, writable: bool) -> JsResult<()> {
        numbers::<Rect>(class, &["x", "y", "width", "height"], writable, |r, i| r.0[i], |r, i, n| r.0[i] = n);
        let from: Method = if writable {
            |_, a, cx| make(Rect::from_init(a.first(), cx)?, true, cx)
        } else {
            |_, a, cx| make(Rect::from_init(a.first(), cx)?, false, cx)
        };
        methods(class, &[("fromRect", 0, from)], true);
        if writable {
            return Ok(());
        }
        for (i, name) in ["top", "right", "bottom", "left"].into_iter().enumerate() {
            let get = NativeFunction::from_copy_closure(move |this, _, _| Ok(get::<Rect>(this)?.edges()[i].into()));
            accessor(class, name, get, None);
        }
        methods(
            class,
            &[("toJSON", 0, |this, _, cx| {
                let r = get::<Rect>(this)?;
                let [x, y, w, h] = r.0;
                let [top, right, bottom, left] = r.edges();
                record([("x", x), ("y", y), ("width", w), ("height", h), ("top", top), ("right", right), ("bottom", bottom), ("left", left)].map(|(k, v)| (k, v.into())), cx)
            })],
            false,
        );
        Ok(())
    }
}

/// A `DOMRect` for `[x, y, width, height]`.
pub(crate) fn rect(r: [f32; 4], cx: &mut Context) -> JsResult<JsValue> {
    make(Rect(r.map(f64::from)), true, cx)
}

// ---- DOMPoint -------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Point([f64; 4]);

impl Point {
    fn from_init(init: Option<&JsValue>, cx: &mut Context) -> JsResult<Point> {
        if let Some(v) = init {
            if let Ok(p) = get::<Point>(v) {
                return Ok(p);
            }
        }
        let o = init.and_then(JsValue::as_object);
        let mut v = [0.0, 0.0, 0.0, 1.0];
        for (slot, key) in v.iter_mut().zip(["x", "y", "z", "w"]) {
            *slot = member(o.as_ref(), key, *slot, cx)?;
        }
        Ok(Point(v))
    }
}

impl Geo for Point {
    const NAMES: [&'static str; 2] = ["DOMPointReadOnly", "DOMPoint"];
    const LENGTH: usize = 0;

    fn from_args(a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        Ok(Point([num(a, 0, 0.0, cx)?, num(a, 1, 0.0, cx)?, num(a, 2, 0.0, cx)?, num(a, 3, 1.0, cx)?]))
    }

    fn init(class: &mut ClassBuilder<'_>, writable: bool) -> JsResult<()> {
        numbers::<Point>(class, &["x", "y", "z", "w"], writable, |p, i| p.0[i], |p, i, n| p.0[i] = n);
        let from: Method = if writable {
            |_, a, cx| make(Point::from_init(a.first(), cx)?, true, cx)
        } else {
            |_, a, cx| make(Point::from_init(a.first(), cx)?, false, cx)
        };
        methods(class, &[("fromPoint", 0, from)], true);
        if writable {
            return Ok(());
        }
        methods(
            class,
            &[
                ("matrixTransform", 0, |this, a, cx| {
                    let p = get::<Point>(this)?;
                    let m = Matrix::from_init(a.first(), cx)?;
                    make(m.apply(p), true, cx)
                }),
                ("toJSON", 0, |this, _, cx| {
                    let [x, y, z, w] = get::<Point>(this)?.0;
                    record([("x", x), ("y", y), ("z", z), ("w", w)].map(|(k, v)| (k, v.into())), cx)
                }),
            ],
            false,
        );
        Ok(())
    }
}

// ---- DOMMatrix ------------------------------------------------------------

/// A 4×4 matrix, `m11, m12, ..., m44` in order (row vectors: a point is
/// `[x y z w] · m`), and whether it's 2D.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Matrix {
    m: [f64; 16],
    is2d: bool,
}

const IDENTITY: [f64; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
/// Where `a`..`f` are among `m11`..`m44`.
const SIX: [usize; 6] = [0, 1, 4, 5, 12, 13];
const NAMES_16: [&str; 16] = ["m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41", "m42", "m43", "m44"];

impl Matrix {
    const IDENTITY: Matrix = Matrix { m: IDENTITY, is2d: true };

    fn two_d(v: [f64; 6]) -> Matrix {
        let mut m = IDENTITY;
        for (i, x) in SIX.into_iter().zip(v) {
            m[i] = x;
        }
        Matrix { m, is2d: true }
    }

    fn three_d(m: [f64; 16]) -> Matrix {
        Matrix { m, is2d: false }
    }

    /// `self · other` as the DOM has it (`other` applied first).
    fn then(self, other: Matrix) -> Matrix {
        let (a, b) = (&self.m, &other.m);
        let mut m = [0.0; 16];
        for i in 0..4 {
            for j in 0..4 {
                m[i * 4 + j] = (0..4).map(|k| b[i * 4 + k] * a[k * 4 + j]).sum();
            }
        }
        Matrix { m, is2d: self.is2d && other.is2d }
    }

    fn translation(x: f64, y: f64, z: f64) -> Matrix {
        let mut m = IDENTITY;
        m[12] = x;
        m[13] = y;
        m[14] = z;
        Matrix { m, is2d: z == 0.0 }
    }

    fn scaling(x: f64, y: f64, z: f64) -> Matrix {
        let mut m = IDENTITY;
        m[0] = x;
        m[5] = y;
        m[10] = z;
        Matrix { m, is2d: z == 1.0 }
    }

    /// A rotation by `deg` about `(x, y, z)` (as CSS `rotate3d`).
    fn rotation(x: f64, y: f64, z: f64, deg: f64) -> Matrix {
        let len = (x * x + y * y + z * z).sqrt();
        if len == 0.0 || !len.is_finite() {
            return Matrix { m: IDENTITY, is2d: x == 0.0 && y == 0.0 };
        }
        let (x, y, z) = (x / len, y / len, z / len);
        let half = deg.to_radians() / 2.0;
        let (sc, sq) = (half.sin() * half.cos(), half.sin() * half.sin());
        let m = [
            1.0 - 2.0 * (y * y + z * z) * sq,
            2.0 * (x * y * sq + z * sc),
            2.0 * (x * z * sq - y * sc),
            0.0,
            2.0 * (x * y * sq - z * sc),
            1.0 - 2.0 * (x * x + z * z) * sq,
            2.0 * (y * z * sq + x * sc),
            0.0,
            2.0 * (x * z * sq + y * sc),
            2.0 * (y * z * sq - x * sc),
            1.0 - 2.0 * (x * x + y * y) * sq,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        Matrix { m, is2d: x == 0.0 && y == 0.0 }
    }

    fn skew(x_deg: f64, y_deg: f64) -> Matrix {
        let mut m = IDENTITY;
        m[4] = x_deg.to_radians().tan();
        m[1] = y_deg.to_radians().tan();
        Matrix { m, is2d: true }
    }

    fn is_identity(&self) -> bool {
        self.m == IDENTITY
    }

    fn inverse(self) -> Matrix {
        let m = &self.m;
        let mut inv = [0.0; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15] + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15] - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15] + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14] - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15] - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15] + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15] - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14] + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15] + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15] - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15] + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14] - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11] - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11] + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11] - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10] + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det == 0.0 || !det.is_finite() {
            return Matrix { m: [f64::NAN; 16], is2d: false };
        }
        Matrix { m: inv.map(|x| x / det), is2d: self.is2d }
    }

    fn apply(&self, p: Point) -> Point {
        let m = &self.m;
        let mut out = [0.0; 4];
        for (j, o) in out.iter_mut().enumerate() {
            *o = (0..4).map(|k| p.0[k] * m[k * 4 + j]).sum();
        }
        Point(out)
    }

    /// Set entry `i`, as the `m..`/`a`..`f` setters do.
    fn set(&mut self, i: usize, v: f64) {
        self.m[i] = v;
        let default = if i == 10 || i == 15 { 1.0 } else { 0.0 };
        if !SIX.contains(&i) && v != default {
            self.is2d = false;
        }
    }

    /// From 6 or 16 numbers.
    fn from_numbers(v: &[f64]) -> JsResult<Matrix> {
        match v.len() {
            6 => Ok(Matrix::two_d(v.try_into().expect("six"))),
            16 => Ok(Matrix::three_d(v.try_into().expect("sixteen"))),
            n => Err(type_error(format!("DOMMatrix: a sequence of 6 or 16 numbers, not {n}"))),
        }
    }

    /// A `DOMMatrixInit`: a matrix, a dictionary, or nothing (identity).
    fn from_init(init: Option<&JsValue>, cx: &mut Context) -> JsResult<Matrix> {
        let Some(v) = init.filter(|v| !v.is_null_or_undefined()) else { return Ok(Matrix::IDENTITY) };
        if let Ok(m) = get::<Matrix>(v) {
            return Ok(m);
        }
        let o = v.as_object().ok_or_else(|| type_error("DOMMatrix: init must be an object".into()))?;
        let mut m = IDENTITY;
        for (i, name) in NAMES_16.iter().enumerate() {
            m[i] = member(Some(&o), name, f64::NAN, cx)?;
        }
        for (i, name) in SIX.into_iter().zip(["a", "b", "c", "d", "e", "f"]) {
            let alias = member(Some(&o), name, f64::NAN, cx)?;
            if m[i].is_nan() {
                m[i] = alias;
            }
        }
        let mut three_d = false;
        for (i, x) in m.iter_mut().enumerate() {
            let default = IDENTITY[i];
            if x.is_nan() {
                *x = default;
            } else if !SIX.contains(&i) && *x != default {
                three_d = true;
            }
        }
        let is2d = o.get(js_string!("is2D"), cx)?;
        let is2d = if is2d.is_undefined() { !three_d } else { is2d.to_boolean() };
        if is2d && three_d {
            return Err(type_error("DOMMatrix: is2D with 3D components".into()));
        }
        Ok(Matrix { m, is2d })
    }

    fn from_sequence(v: &JsValue, cx: &mut Context) -> JsResult<Matrix> {
        let o = v.as_object().ok_or_else(|| type_error("DOMMatrix: not a sequence".into()))?;
        let len = o.get(js_string!("length"), cx)?.to_length(cx)?;
        let mut n = Vec::with_capacity(len.min(16) as usize);
        for i in 0..len.min(17) {
            n.push(o.get(i, cx)?.to_number(cx)?);
        }
        if len > 16 {
            return Err(type_error(format!("DOMMatrix: a sequence of 6 or 16 numbers, not {len}")));
        }
        Matrix::from_numbers(&n)
    }

    fn to_css(&self, cx: &mut Context) -> JsResult<String> {
        if self.m.iter().any(|x| !x.is_finite()) {
            return Err(crate::dom_exception::error("InvalidStateError", "the matrix has non-finite values", cx));
        }
        let mut parts = Vec::with_capacity(16);
        let picked: Vec<f64> = if self.is2d { SIX.iter().map(|&i| self.m[i]).collect() } else { self.m.to_vec() };
        for x in picked {
            parts.push(JsValue::from(x).to_string(cx)?.to_std_string_escaped());
        }
        Ok(format!("{}({})", if self.is2d { "matrix" } else { "matrix3d" }, parts.join(", ")))
    }
}

/// The transform functions that keep a matrix 2D.
const TWO_D: [&str; 12] = ["matrix", "translate", "translatex", "translatey", "scale", "scalex", "scaley", "rotate", "rotatez", "skew", "skewx", "skewy"];

/// A CSS `<transform-list>` (absolute lengths in px only), as `DOMMatrix`
/// strings are.
fn parse_transform(text: &str) -> Option<Matrix> {
    use cssparser::{Parser, Token};
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return Some(Matrix::IDENTITY);
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Number,
        Length,
        Angle,
    }
    let mut p = Parser::new(text);
    let mut out = Matrix::IDENTITY;
    while !p.is_exhausted() {
        let name = p.expect_function().ok()?.to_ascii_lowercase();
        let args: Vec<(f64, Kind)> = p
            .parse_nested_block(|p| {
                let mut args = Vec::new();
                while !p.is_exhausted() {
                    let arg = match p.next()?.clone() {
                        Token::Number { value, .. } => (f64::from(value), Kind::Number),
                        Token::Dimension { value, ref unit, .. } => {
                            let v = f64::from(value);
                            match unit.to_ascii_lowercase().as_str() {
                                "px" => (v, Kind::Length),
                                "deg" => (v, Kind::Angle),
                                "rad" => (v.to_degrees(), Kind::Angle),
                                "grad" => (v * 0.9, Kind::Angle),
                                "turn" => (v * 360.0, Kind::Angle),
                                _ => return Err(p.new_error_for_next_token::<()>()),
                            }
                        }
                        _ => return Err(p.new_error_for_next_token()),
                    };
                    args.push(arg);
                    let _ = p.try_parse(|p| p.expect_comma());
                }
                Ok(args)
            })
            .ok()?;
        // Each argument as the kind wanted (a bare 0 is any length or angle).
        let get = |i: usize, want: Kind| -> Option<f64> {
            let (v, kind) = *args.get(i)?;
            (kind == want || (kind == Kind::Number && v == 0.0)).then_some(v)
        };
        let count = |n: std::ops::RangeInclusive<usize>| n.contains(&args.len()).then_some(());
        let (len, ang, number) = (Kind::Length, Kind::Angle, Kind::Number);
        let step = match name.as_str() {
            "matrix" => {
                count(6..=6)?;
                args.iter().all(|a| a.1 == number).then_some(())?;
                Matrix::two_d(std::array::from_fn(|i| args[i].0))
            }
            "matrix3d" => {
                count(16..=16)?;
                args.iter().all(|a| a.1 == number).then_some(())?;
                Matrix::three_d(std::array::from_fn(|i| args[i].0))
            }
            "translate" => {
                count(1..=2)?;
                Matrix::translation(get(0, len)?, if args.len() > 1 { get(1, len)? } else { 0.0 }, 0.0)
            }
            "translatex" => Matrix::translation(get(0, len)?, 0.0, 0.0),
            "translatey" => Matrix::translation(0.0, get(0, len)?, 0.0),
            "translatez" => Matrix::translation(0.0, 0.0, get(0, len)?),
            "translate3d" => {
                count(3..=3)?;
                Matrix::translation(get(0, len)?, get(1, len)?, get(2, len)?)
            }
            "scale" => {
                count(1..=2)?;
                let x = get(0, number)?;
                Matrix::scaling(x, if args.len() > 1 { get(1, number)? } else { x }, 1.0)
            }
            "scalex" => Matrix::scaling(get(0, number)?, 1.0, 1.0),
            "scaley" => Matrix::scaling(1.0, get(0, number)?, 1.0),
            "scalez" => Matrix::scaling(1.0, 1.0, get(0, number)?),
            "scale3d" => {
                count(3..=3)?;
                Matrix::scaling(get(0, number)?, get(1, number)?, get(2, number)?)
            }
            "rotate" | "rotatez" => Matrix::rotation(0.0, 0.0, 1.0, get(0, ang)?),
            "rotatex" => Matrix::rotation(1.0, 0.0, 0.0, get(0, ang)?),
            "rotatey" => Matrix::rotation(0.0, 1.0, 0.0, get(0, ang)?),
            "rotate3d" => {
                count(4..=4)?;
                Matrix::rotation(get(0, number)?, get(1, number)?, get(2, number)?, get(3, ang)?)
            }
            "skew" => {
                count(1..=2)?;
                Matrix::skew(get(0, ang)?, if args.len() > 1 { get(1, ang)? } else { 0.0 })
            }
            "skewx" => Matrix::skew(get(0, ang)?, 0.0),
            "skewy" => Matrix::skew(0.0, get(0, ang)?),
            "perspective" => {
                let d = get(0, len)?;
                let mut m = IDENTITY;
                if d != 0.0 {
                    m[11] = -1.0 / d;
                }
                Matrix::three_d(m)
            }
            _ => return None,
        };
        // A 3D function makes the result 3D whatever its values.
        if !TWO_D.contains(&name.as_str()) {
            out.is2d = false;
        }
        out = out.then(step);
    }
    Some(out)
}

/// A matrix operation: the value it makes from `this`'s and the arguments.
type Op = fn(Matrix, &[JsValue], &mut Context) -> JsResult<Matrix>;

/// The operations, by their new-matrix name and their in-place name.
const OPS: &[(Option<&str>, Option<&str>, usize, Op)] = &[
    (Some("multiply"), Some("multiplySelf"), 0, |m, a, cx| Ok(m.then(Matrix::from_init(a.first(), cx)?))),
    (None, Some("preMultiplySelf"), 0, |m, a, cx| Ok(Matrix::from_init(a.first(), cx)?.then(m))),
    (Some("translate"), Some("translateSelf"), 0, |m, a, cx| {
        Ok(m.then(Matrix::translation(num(a, 0, 0.0, cx)?, num(a, 1, 0.0, cx)?, num(a, 2, 0.0, cx)?)))
    }),
    (Some("scale"), Some("scaleSelf"), 0, |m, a, cx| {
        let x = num(a, 0, 1.0, cx)?;
        let s = [x, num(a, 1, x, cx)?, num(a, 2, 1.0, cx)?];
        let o = [num(a, 3, 0.0, cx)?, num(a, 4, 0.0, cx)?, num(a, 5, 0.0, cx)?];
        Ok(scaled(m, s, o))
    }),
    (Some("scaleNonUniform"), None, 0, |m, a, cx| Ok(m.then(Matrix::scaling(num(a, 0, 1.0, cx)?, num(a, 1, 1.0, cx)?, 1.0)))),
    (Some("scale3d"), Some("scale3dSelf"), 0, |m, a, cx| {
        let s = num(a, 0, 1.0, cx)?;
        let o = [num(a, 1, 0.0, cx)?, num(a, 2, 0.0, cx)?, num(a, 3, 0.0, cx)?];
        Ok(scaled(m, [s, s, s], o))
    }),
    (Some("rotate"), Some("rotateSelf"), 0, |m, a, cx| {
        let mut r = [num(a, 0, 0.0, cx)?, num(a, 1, f64::NAN, cx)?, num(a, 2, f64::NAN, cx)?];
        if a.get(1).is_none_or(JsValue::is_undefined) && a.get(2).is_none_or(JsValue::is_undefined) {
            r = [0.0, 0.0, r[0]];
        }
        let [x, y, z] = r.map(|v| if v.is_nan() { 0.0 } else { v });
        let mut out = m.then(Matrix::rotation(0.0, 0.0, 1.0, z)).then(Matrix::rotation(0.0, 1.0, 0.0, y)).then(Matrix::rotation(1.0, 0.0, 0.0, x));
        out.is2d = m.is2d && x == 0.0 && y == 0.0;
        Ok(out)
    }),
    (Some("rotateFromVector"), Some("rotateFromVectorSelf"), 0, |m, a, cx| {
        let (x, y) = (num(a, 0, 0.0, cx)?, num(a, 1, 0.0, cx)?);
        let deg = if x == 0.0 && y == 0.0 { 0.0 } else { y.atan2(x).to_degrees() };
        Ok(m.then(Matrix::rotation(0.0, 0.0, 1.0, deg)))
    }),
    (Some("rotateAxisAngle"), Some("rotateAxisAngleSelf"), 0, |m, a, cx| {
        let r = Matrix::rotation(num(a, 0, 0.0, cx)?, num(a, 1, 0.0, cx)?, num(a, 2, 0.0, cx)?, num(a, 3, 0.0, cx)?);
        Ok(m.then(r))
    }),
    (Some("skewX"), Some("skewXSelf"), 0, |m, a, cx| Ok(m.then(Matrix::skew(num(a, 0, 0.0, cx)?, 0.0)))),
    (Some("skewY"), Some("skewYSelf"), 0, |m, a, cx| Ok(m.then(Matrix::skew(0.0, num(a, 0, 0.0, cx)?)))),
    (Some("flipX"), None, 0, |m, _, _| Ok(m.then(Matrix::scaling(-1.0, 1.0, 1.0)))),
    (Some("flipY"), None, 0, |m, _, _| Ok(m.then(Matrix::scaling(1.0, -1.0, 1.0)))),
    (Some("inverse"), Some("invertSelf"), 0, |m, _, _| Ok(m.inverse())),
    (None, Some("setMatrixValue"), 1, |_, a, cx| {
        let text = crate::dom::js_value_string(&a.first().cloned().unwrap_or_default(), cx);
        parse_transform(&text).ok_or_else(|| crate::dom_exception::error("SyntaxError", &format!("DOMMatrix: can't parse {text:?}"), cx))
    }),
];

fn scaled(m: Matrix, s: [f64; 3], o: [f64; 3]) -> Matrix {
    let mut out = m.then(Matrix::translation(o[0], o[1], o[2])).then(Matrix::scaling(s[0], s[1], s[2])).then(Matrix::translation(-o[0], -o[1], -o[2]));
    out.is2d = m.is2d && s[2] == 1.0 && o[2] == 0.0;
    out
}

impl Geo for Matrix {
    const NAMES: [&'static str; 2] = ["DOMMatrixReadOnly", "DOMMatrix"];
    const LENGTH: usize = 0;

    fn from_args(a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        match a.first() {
            None => Ok(Matrix::IDENTITY),
            Some(v) if v.is_undefined() => Ok(Matrix::IDENTITY),
            Some(v) if v.is_string() => {
                let text = crate::dom::js_value_string(v, cx);
                parse_transform(&text).ok_or_else(|| crate::dom_exception::error("SyntaxError", &format!("DOMMatrix: can't parse {text:?}"), cx))
            }
            Some(v) => Matrix::from_sequence(v, cx),
        }
    }

    fn init(class: &mut ClassBuilder<'_>, writable: bool) -> JsResult<()> {
        let read: fn(&Matrix, usize) -> f64 = |m, i| m.m[i];
        let write: fn(&mut Matrix, usize, f64) = |m, i, v| m.set(i, v);
        numbers::<Matrix>(class, &NAMES_16, writable, read, write);
        numbers::<Matrix>(class, &["a", "b", "c", "d", "e", "f"], writable, |m, i| m.m[SIX[i]], |m, i, v| m.set(SIX[i], v));
        // Statics, making the class they're called on.
        let statics: [(&str, usize, Method); 3] = if writable {
            [
                ("fromMatrix", 0, |_, a, cx| make(Matrix::from_init(a.first(), cx)?, true, cx)),
                ("fromFloat32Array", 1, |_, a, cx| make(Matrix::from_sequence(&a.first().cloned().unwrap_or_default(), cx)?, true, cx)),
                ("fromFloat64Array", 1, |_, a, cx| make(Matrix::from_sequence(&a.first().cloned().unwrap_or_default(), cx)?, true, cx)),
            ]
        } else {
            [
                ("fromMatrix", 0, |_, a, cx| make(Matrix::from_init(a.first(), cx)?, false, cx)),
                ("fromFloat32Array", 1, |_, a, cx| make(Matrix::from_sequence(&a.first().cloned().unwrap_or_default(), cx)?, false, cx)),
                ("fromFloat64Array", 1, |_, a, cx| make(Matrix::from_sequence(&a.first().cloned().unwrap_or_default(), cx)?, false, cx)),
            ]
        };
        methods(class, &statics, true);
        for &(fresh, in_place, len, op) in OPS {
            let name = if writable { in_place } else { fresh };
            let Some(name) = name else { continue };
            let f = if writable {
                NativeFunction::from_copy_closure(move |this, a, cx| {
                    let m = op(get::<Matrix>(this)?, a, cx)?;
                    update::<Matrix>(this, |v| *v = m)?;
                    Ok(this.clone())
                })
            } else {
                NativeFunction::from_copy_closure(move |this, a, cx| {
                    let m = op(get::<Matrix>(this)?, a, cx)?;
                    make(m, true, cx)
                })
            };
            class.method(JsString::from(name), len, f);
        }
        if writable {
            return Ok(());
        }
        accessor(class, "is2D", NativeFunction::from_fn_ptr(|this, _, _| Ok(get::<Matrix>(this)?.is2d.into())), None);
        accessor(class, "isIdentity", NativeFunction::from_fn_ptr(|this, _, _| Ok(get::<Matrix>(this)?.is_identity().into())), None);
        methods(
            class,
            &[
                ("transformPoint", 0, |this, a, cx| {
                    let m = get::<Matrix>(this)?;
                    let p = Point::from_init(a.first(), cx)?;
                    make(m.apply(p), true, cx)
                }),
                ("toFloat32Array", 0, |this, _, cx| {
                    let m = get::<Matrix>(this)?;
                    Ok(JsFloat32Array::from_iter(m.m.map(|x| x as f32), cx)?.into())
                }),
                ("toFloat64Array", 0, |this, _, cx| {
                    let m = get::<Matrix>(this)?;
                    Ok(JsFloat64Array::from_iter(m.m, cx)?.into())
                }),
                ("toString", 0, |this, _, cx| {
                    let css = get::<Matrix>(this)?.to_css(cx)?;
                    Ok(JsString::from(css).into())
                }),
                ("toJSON", 0, |this, _, cx| {
                    let m = get::<Matrix>(this)?;
                    let six = ["a", "b", "c", "d", "e", "f"].into_iter().zip(SIX).map(|(k, i)| (k, JsValue::from(m.m[i])));
                    let all = NAMES_16.into_iter().zip(m.m).map(|(k, v)| (k, JsValue::from(v)));
                    let flags = [("is2D", JsValue::from(m.is2d)), ("isIdentity", JsValue::from(m.is_identity()))];
                    record(six.chain(all).chain(flags), cx)
                }),
            ],
            false,
        );
        Ok(())
    }
}

// ---- install --------------------------------------------------------------

/// Register both classes of `V`, the writable one inheriting from the
/// read-only one.
fn register<V: Geo>(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<ReadOnly<V>>().and_then(|_| cx.register_global_class::<Writable<V>>()) {
        eprintln!("js: {}: {e}", V::NAMES[1]);
        return;
    }
    let (Some(ro), Some(rw)) = (cx.get_global_class::<ReadOnly<V>>(), cx.get_global_class::<Writable<V>>()) else { return };
    rw.prototype().set_prototype(Some(ro.prototype()));
    rw.constructor().set_prototype(Some(ro.constructor()));
}

/// Register `DOMRect`, `DOMPoint` and `DOMMatrix` (and the read-only
/// classes, and `WebKitCSSMatrix`, an old name for `DOMMatrix`).
pub(crate) fn install(cx: &mut Context) {
    register::<Rect>(cx);
    register::<Point>(cx);
    register::<Matrix>(cx);
    if let Some(m) = cx.get_global_class::<Writable<Matrix>>() {
        let _ = cx.global_object().set(js_string!("WebKitCSSMatrix"), m.constructor(), false, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Matrix, b: &[f64; 16]) -> bool {
        a.m.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn composition_applies_the_right_operand_first() {
        // translate(10) · scale(2): a point is scaled, then moved.
        let m = Matrix::IDENTITY.then(Matrix::translation(10.0, 0.0, 0.0)).then(Matrix::scaling(2.0, 2.0, 1.0));
        let p = m.apply(Point([1.0, 1.0, 0.0, 1.0]));
        assert_eq!(p.0, [12.0, 2.0, 0.0, 1.0]);
        assert!(m.is2d);
    }

    #[test]
    fn rotation_matches_the_2d_form() {
        let m = Matrix::rotation(0.0, 0.0, 1.0, 90.0);
        assert!(close(&m, &[0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn inverse_undoes() {
        let m = parse_transform("translate(5px, 7px) rotate(30deg) scale(2, 3)").unwrap();
        let i = m.then(m.inverse());
        assert!(close(&i, &IDENTITY));
        assert!(m.is2d);
        assert!(Matrix::scaling(0.0, 1.0, 1.0).inverse().m[0].is_nan());
    }

    #[test]
    fn parses_css() {
        let m = parse_transform("matrix(1, 2, 3, 4, 5, 6)").unwrap();
        assert_eq!([m.m[0], m.m[1], m.m[4], m.m[5], m.m[12], m.m[13]], [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert!(!parse_transform("translateZ(4px)").unwrap().is2d);
        assert!(parse_transform("translate(1em)").is_none());
        assert!(parse_transform("bogus(1)").is_none());
        assert_eq!(parse_transform("none"), Some(Matrix::IDENTITY));
    }
}
