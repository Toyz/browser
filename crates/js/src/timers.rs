//! Timers: `setTimeout`, `setInterval`, `requestAnimationFrame`,
//! `requestIdleCallback` (and their cancels), native. The callbacks live in
//! a garbage-collected table; the runtime's loop fires them when due
//! (`Dom::timers`, then `__fireTimer`).

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsObject, JsResult, JsString, JsValue, NativeFunction, Source, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

use crate::dom::Shared;

/// What kind of timer, and so how it's called.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Timeout,
    /// Again every this many ms.
    Interval(f64),
    /// With the time, ms.
    Frame,
    /// With an `IdleDeadline`.
    Idle,
}

#[derive(Trace, Finalize)]
struct Timer {
    callback: JsValue,
    args: Vec<JsValue>,
    #[unsafe_ignore_trace]
    kind: Kind,
}

#[derive(Trace, Finalize, JsData)]
struct Table {
    timers: GcRefCell<HashMap<u32, Timer>>,
    #[unsafe_ignore_trace]
    next: Cell<u32>,
}

/// What each timer function closes over: the page's DOM (where due times
/// are kept) and the table.
#[derive(Trace, Finalize)]
struct Captures {
    #[unsafe_ignore_trace]
    dom: Shared,
    table: JsObject,
}

/// Run `f` on the timer table.
fn with_table<R>(c: &Captures, f: impl FnOnce(&Table) -> R) -> R {
    f(&c.table.downcast_ref::<Table>().expect("the timer table"))
}

/// Schedule `callback` in `ms` (as `kind`): its id.
fn add(c: &Captures, callback: JsValue, args: Vec<JsValue>, ms: f64, kind: Kind) -> u32 {
    let id = with_table(c, |t| {
        let id = t.next.get();
        t.next.set(id + 1);
        t.timers.borrow_mut().insert(id, Timer { callback, args, kind });
        id
    });
    c.dom.borrow_mut().timers.push((Instant::now() + Duration::from_secs_f64(ms.max(0.0) / 1000.0), id));
    id
}

fn clear(c: &Captures, id: u32) {
    with_table(c, |t| t.timers.borrow_mut().remove(&id));
    c.dom.borrow_mut().timers.retain(|(_, t)| *t != id);
}

fn delay(a: &[JsValue], i: usize, cx: &mut Context) -> f64 {
    a.get(i).map(|v| v.to_number(cx).unwrap_or(0.0)).filter(|n| n.is_finite()).unwrap_or(0.0).max(0.0)
}

fn id_of(a: &[JsValue], cx: &mut Context) -> u32 {
    a.first().map(|v| v.to_number(cx).unwrap_or(0.0)).unwrap_or(0.0) as u32
}

/// Install the timer functions on the natives object (`N.timers`), and the
/// runtime's `fireTimer` hook.
pub(crate) fn install(natives: &JsObject, dom: &Shared, cx: &mut Context) {
    let table = JsObject::from_proto_and_data(None, Table { timers: GcRefCell::new(HashMap::new()), next: Cell::new(1) });
    let f = |cx: &mut Context, name: &str, len: usize, body: fn(&JsValue, &[JsValue], &Captures, &mut Context) -> JsResult<JsValue>| {
        let captures = Captures { dom: dom.clone(), table: table.clone() };
        FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_copy_closure_with_captures(body, captures))
            .name(JsString::from(name))
            .length(len)
            .build()
    };
    let set_timeout = f(cx, "setTimeout", 2, |_, a, c, cx| {
        let ms = delay(a, 1, cx);
        Ok(add(c, a.first().cloned().unwrap_or_default(), a.iter().skip(2).cloned().collect(), ms, Kind::Timeout).into())
    });
    let set_interval = f(cx, "setInterval", 2, |_, a, c, cx| {
        let ms = delay(a, 1, cx);
        Ok(add(c, a.first().cloned().unwrap_or_default(), a.iter().skip(2).cloned().collect(), ms, Kind::Interval(ms.max(4.0))).into())
    });
    let frame = f(cx, "requestAnimationFrame", 1, |_, a, c, _| Ok(add(c, a.first().cloned().unwrap_or_default(), Vec::new(), 16.0, Kind::Frame).into()));
    let idle = f(cx, "requestIdleCallback", 1, |_, a, c, _| Ok(add(c, a.first().cloned().unwrap_or_default(), Vec::new(), 1.0, Kind::Idle).into()));
    let cancel = f(cx, "clearTimeout", 1, |_, a, c, cx| {
        clear(c, id_of(a, cx));
        Ok(JsValue::undefined())
    });
    let fire = f(cx, "fireTimer", 1, fire);
    let attrs = Attribute::all();
    let timers = ObjectInitializer::new(cx)
        .property(js_string!("setTimeout"), set_timeout, attrs)
        .property(js_string!("setInterval"), set_interval, attrs)
        .property(js_string!("requestAnimationFrame"), frame, attrs)
        .property(js_string!("requestIdleCallback"), idle, attrs)
        .property(js_string!("clearTimeout"), cancel.clone(), attrs)
        .property(js_string!("clearInterval"), cancel.clone(), attrs)
        .property(js_string!("cancelAnimationFrame"), cancel.clone(), attrs)
        .property(js_string!("cancelIdleCallback"), cancel, attrs)
        .build();
    natives.set(js_string!("timers"), timers, false, cx).ok();
    natives.set(js_string!("fireTimer"), fire, false, cx).ok();
}

/// Timer `id` is due: call it (an interval is scheduled again first).
fn fire(_: &JsValue, a: &[JsValue], c: &Captures, cx: &mut Context) -> JsResult<JsValue> {
    let id = id_of(a, cx);
    let due = with_table(c, |table| {
        let mut timers = table.timers.borrow_mut();
        let t = timers.get(&id)?;
        let (callback, args, kind) = (t.callback.clone(), t.args.clone(), t.kind);
        if !matches!(kind, Kind::Interval(_)) {
            timers.remove(&id);
        }
        Some((callback, args, kind))
    });
    let Some((callback, args, kind)) = due else { return Ok(JsValue::undefined()) };
    if let Kind::Interval(ms) = kind {
        c.dom.borrow_mut().timers.push((Instant::now() + Duration::from_secs_f64(ms / 1000.0), id));
    }
    let args = match kind {
        Kind::Frame => vec![JsValue::from(crate::dom::now_ms(&c.dom))],
        Kind::Idle => {
            let remaining = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::from(10))))
                .name(js_string!("timeRemaining"))
                .build();
            let deadline = ObjectInitializer::new(cx)
                .property(js_string!("didTimeout"), false, Attribute::all())
                .property(js_string!("timeRemaining"), remaining, Attribute::all())
                .build();
            vec![deadline.into()]
        }
        _ => args,
    };
    let result = match callback.as_callable() {
        Some(f) => f.call(&JsValue::undefined(), &args, cx),
        // `setTimeout("code", ms)`: run the code.
        None => {
            let code = callback.to_string(cx)?.to_std_string_escaped();
            cx.eval(Source::from_bytes(&code))
        }
    };
    if let Err(e) = result {
        // As an uncaught error would be: `error` event, console.
        let report = cx.global_object().get(js_string!("reportError"), cx).ok().and_then(|f| f.as_callable());
        match report {
            Some(report) => {
                let e = e.into_opaque(cx).unwrap_or_default();
                report.call(&JsValue::undefined(), &[e], cx).ok();
            }
            None => eprintln!("js: timer: {e}"),
        }
    }
    Ok(JsValue::undefined())
}
