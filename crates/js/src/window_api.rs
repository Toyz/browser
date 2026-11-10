//! `window.console`, `window.navigator` and `window.location`, native.

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::object::builtins::JsFunction;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};
use url::quirks;

use crate::dom::{Shared, js_value_string};

/// A native function capturing the page's DOM.
fn native(dom: &Shared, f: fn(&Shared, &[JsValue], &mut Context) -> JsResult<JsValue>) -> NativeFunction {
    let d = dom.clone();
    // SAFETY: the closure captures only the Rc'd DOM, which holds no
    // garbage-collected values.
    unsafe { NativeFunction::from_closure(move |_this, args, cx| f(&d, args, cx)) }
}

fn function(cx: &mut Context, name: &str, f: NativeFunction) -> JsFunction {
    FunctionObjectBuilder::new(cx.realm(), f).name(JsString::from(name)).build()
}

/// Install `console`, `navigator`, `location` and `history` (and the
/// runtime's `historyTraverse` hook) on the natives object.
pub(crate) fn install(natives: &JsObject, dom: &Shared, cx: &mut Context) {
    let console = console(dom, cx);
    let navigator = navigator(cx);
    let location = location(dom, cx);
    let history = history(dom, cx);
    let traverse = function(cx, "historyTraverse", native(dom, history_traverse));
    for (name, value) in [("console", console), ("navigator", navigator), ("location", location), ("history", history), ("historyTraverse", traverse.into())] {
        natives.set(JsString::from(name), value, false, cx).ok();
    }
}

/// What `console.log(a, b)` prints: strings as they are, errors with their
/// stack, elements as `<tag>`, everything else as JSON where it can be.
fn format(args: &[JsValue], cx: &mut Context) -> String {
    let mut parts = Vec::with_capacity(args.len());
    for a in args {
        let text = match a {
            v if v.is_string() => js_value_string(v, cx),
            v if v.is_object() => {
                let o = v.as_object().expect("an object");
                let stack = o.get(js_string!("stack"), cx).ok().filter(|s| s.is_string());
                let local = o.get(js_string!("localName"), cx).ok().filter(|s| s.is_string());
                if let Some(stack) = stack {
                    format!("{}\n{}", js_value_string(v, cx), js_value_string(&stack, cx))
                } else if let Some(local) = local {
                    format!("<{}>", js_value_string(&local, cx))
                } else {
                    let json = cx.global_object().get(js_string!("JSON"), cx).ok().and_then(|j| j.as_object());
                    let stringify = json.as_ref().and_then(|j| j.get(js_string!("stringify"), cx).ok()).and_then(|f| f.as_callable());
                    match (json, stringify) {
                        (Some(json), Some(f)) => match f.call(&json.into(), std::slice::from_ref(v), cx) {
                            Ok(s) if s.is_string() => js_value_string(&s, cx),
                            _ => js_value_string(v, cx),
                        },
                        _ => js_value_string(v, cx),
                    }
                }
            }
            v => js_value_string(v, cx),
        };
        parts.push(text);
    }
    parts.join(" ")
}

fn console(dom: &Shared, cx: &mut Context) -> JsValue {
    let mut init = ObjectInitializer::new(cx);
    // Printed when debugging (BROWSER_DEBUG), as `console.level: text`.
    macro_rules! level {
        ($name:literal) => {{
            let f = native(dom, |d, a, cx| {
                let text = format(a, cx);
                if crate::dom::debug_logs(d) {
                    eprintln!(concat!("console.", $name, ": {}"), text);
                }
                Ok(JsValue::undefined())
            });
            init.function(f, js_string!($name), 0);
        }};
    }
    level!("log");
    level!("info");
    level!("warn");
    level!("debug");
    level!("error");
    level!("trace");
    for name in ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "assert", "table", "dir", "dirxml", "clear"] {
        init.function(NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined())), JsString::from(name), 0);
    }
    init.build().into()
}

fn navigator(cx: &mut Context) -> JsValue {
    let ua = crate::USER_AGENT;
    let languages = boa_engine::object::builtins::JsArray::from_iter([js_string!("en-US").into(), js_string!("en").into()], cx);
    // `sendBeacon(url, data)`: a fire-and-forget POST, through `fetch`.
    let send_beacon = NativeFunction::from_fn_ptr(|_, a, cx| {
        let Some(fetch) = cx.global_object().get(js_string!("fetch"), cx)?.as_callable() else { return Ok(JsValue::from(false)) };
        let init = ObjectInitializer::new(cx)
            .property(js_string!("method"), js_string!("POST"), Attribute::all())
            .property(js_string!("body"), a.get(1).cloned().unwrap_or_default(), Attribute::all())
            .property(js_string!("mode"), js_string!("no-cors"), Attribute::all())
            .property(js_string!("credentials"), js_string!("include"), Attribute::all())
            .build();
        let url = a.first().cloned().unwrap_or_default();
        fetch.call(&JsValue::undefined(), &[url, init.into()], cx).ok();
        Ok(JsValue::from(true))
    });
    let resolved = |cx: &mut Context| -> JsFunction {
        let f = NativeFunction::from_fn_ptr(|_, _, cx| Ok(boa_engine::object::builtins::JsPromise::resolve(js_string!(""), cx)?.into()));
        function(cx, "", f)
    };
    let (write, read) = (resolved(cx), resolved(cx));
    let clipboard = ObjectInitializer::new(cx)
        .property(js_string!("writeText"), write, Attribute::all())
        .property(js_string!("readText"), read, Attribute::all())
        .build();
    let ro = Attribute::READONLY | Attribute::ENUMERABLE | Attribute::CONFIGURABLE;
    ObjectInitializer::new(cx)
        .property(js_string!("userAgent"), JsString::from(ua), ro)
        .property(js_string!("appVersion"), JsString::from(&ua[8..]), ro)
        .property(js_string!("language"), js_string!("en-US"), ro)
        .property(js_string!("languages"), languages, ro)
        .property(js_string!("platform"), js_string!("MacIntel"), ro)
        .property(js_string!("vendor"), js_string!(""), ro)
        .property(js_string!("onLine"), true, ro)
        .property(js_string!("cookieEnabled"), true, ro)
        .property(js_string!("hardwareConcurrency"), std::thread::available_parallelism().map_or(4, |n| n.get()) as i32, ro)
        .property(js_string!("maxTouchPoints"), 0, ro)
        .property(js_string!("webdriver"), false, ro)
        .property(js_string!("clipboard"), clipboard, ro)
        .function(send_beacon, js_string!("sendBeacon"), 2)
        .build()
        .into()
}

/// The parts of `location` with their `url::quirks` getter and setter.
const PARTS: &[(&str, fn(&url::Url) -> &str, fn(&mut url::Url, &str) -> Result<(), ()>)] = &[
    ("protocol", quirks::protocol, quirks::set_protocol),
    ("host", quirks::host, quirks::set_host),
    ("hostname", quirks::hostname, quirks::set_hostname),
    ("port", quirks::port, quirks::set_port),
    ("pathname", quirks::pathname, |u, v| {
        quirks::set_pathname(u, v);
        Ok(())
    }),
    ("search", quirks::search, |u, v| {
        quirks::set_search(u, v);
        Ok(())
    }),
    ("hash", quirks::hash, |u, v| {
        quirks::set_hash(u, v);
        Ok(())
    }),
];

fn location(dom: &Shared, cx: &mut Context) -> JsValue {
    let href_get = native(dom, |d, _, _| Ok(JsValue::from(JsString::from(d.borrow().doc.url.as_str()))));
    let href_set = native(dom, |d, a, cx| {
        let to = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
        crate::dom::navigate_to(d, &to);
        Ok(JsValue::undefined())
    });
    let (href_get, href_set) = (function(cx, "href", href_get), function(cx, "href", href_set));
    let go = native(dom, |d, a, cx| {
        let to = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
        crate::dom::navigate_to(d, &to);
        Ok(JsValue::undefined())
    });
    let reload = native(dom, |d, _, _| {
        let here = d.borrow().doc.url.to_string();
        crate::dom::navigate_to(d, &here);
        Ok(JsValue::undefined())
    });
    let to_string = native(dom, |d, _, _| Ok(JsValue::from(JsString::from(d.borrow().doc.url.as_str()))));
    let origin = native(dom, |d, _, _| Ok(JsValue::from(JsString::from(quirks::origin(&d.borrow().doc.url)))));
    let origin = function(cx, "origin", origin);
    let mut accessors = Vec::new();
    for (i, &(name, _, _)) in PARTS.iter().enumerate() {
        // (A part's index rides in the closure: one getter and setter each.)
        let d = dom.clone();
        // SAFETY: captures only the Rc'd DOM and an index.
        let get = unsafe {
            NativeFunction::from_closure(move |_, _, _| Ok(JsValue::from(JsString::from((PARTS[i].1)(&d.borrow().doc.url)))))
        };
        let d = dom.clone();
        // SAFETY: as above.
        let set = unsafe {
            NativeFunction::from_closure(move |_, a, cx| {
                let value = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
                let mut url = d.borrow().doc.url.clone();
                if (PARTS[i].2)(&mut url, &value).is_err() {
                    return Ok(JsValue::undefined());
                }
                if PARTS[i].0 == "hash" {
                    // A new entry in this document, then `hashchange`.
                    if let Some(f) = cx.global_object().get(js_string!("__hashNavigate"), cx)?.as_callable() {
                        f.call(&JsValue::undefined(), &[JsValue::from(JsString::from(url.as_str()))], cx)?;
                    }
                } else {
                    crate::dom::navigate_to(&d, url.as_str());
                }
                Ok(JsValue::undefined())
            })
        };
        accessors.push((name, function(cx, name, get), function(cx, name, set)));
    }
    let mut init = ObjectInitializer::new(cx);
    init.accessor(js_string!("href"), Some(href_get), Some(href_set), Attribute::ENUMERABLE | Attribute::CONFIGURABLE)
        .accessor(js_string!("origin"), Some(origin), None, Attribute::ENUMERABLE | Attribute::CONFIGURABLE)
        .function(go.clone(), js_string!("assign"), 1)
        .function(go, js_string!("replace"), 1)
        .function(reload, js_string!("reload"), 0)
        .function(to_string, js_string!("toString"), 0);
    for (name, get, set) in accessors {
        init.accessor(JsString::from(name), Some(get), Some(set), Attribute::ENUMERABLE | Attribute::CONFIGURABLE);
    }
    init.build().into()
}

/// The session history of this document: the entries `pushState` made
/// (address and state), and which one is current. The tab keeps the list
/// of addresses, and tells us when it traverses.
#[derive(Trace, Finalize, JsData)]
struct History {
    entries: GcRefCell<Vec<(JsValue, JsValue)>>,
    #[unsafe_ignore_trace]
    index: std::cell::Cell<usize>,
}

/// `this` as the history object.
fn this_history(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<History>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not a History").into())
}

/// `structuredClone(state)` (the state as it was when pushed); the value
/// itself where that throws.
fn clone_state(state: &JsValue, cx: &mut Context) -> JsValue {
    if state.is_null_or_undefined() {
        return JsValue::null();
    }
    crate::structured_clone::clone(state, cx).unwrap_or_else(|_| state.clone())
}

fn history(dom: &Shared, cx: &mut Context) -> JsValue {
    let here = JsValue::from(JsString::from(dom.borrow().doc.url.as_str()));
    let data = History { entries: GcRefCell::new(vec![(here, JsValue::null())]), index: std::cell::Cell::new(0) };
    let length = NativeFunction::from_fn_ptr(|this, _, _| {
        let h = this_history(this)?;
        Ok(JsValue::from(h.downcast_ref::<History>().expect("a History").entries.borrow().len() as u32))
    });
    let state = NativeFunction::from_fn_ptr(|this, _, _| {
        let h = this_history(this)?;
        let h = h.downcast_ref::<History>().expect("a History");
        Ok(h.entries.borrow()[h.index.get()].1.clone())
    });
    let (length, state) = (function(cx, "length", length), function(cx, "state", state));
    // pushState / replaceState: (state, title, url?).
    let d = dom.clone();
    // SAFETY: captures only the Rc'd DOM.
    let push = unsafe {
        NativeFunction::from_closure(move |this, a, cx| {
            let h = this_history(this)?;
            let href = match a.get(2).filter(|u| !u.is_null_or_undefined()) {
                Some(u) => crate::dom::js_value_string(u, cx),
                None => d.borrow().doc.url.to_string(),
            };
            let url = crate::dom::set_url(&d, &href, 1)?;
            let state = clone_state(&a.first().cloned().unwrap_or_default(), cx);
            let h = h.downcast_ref::<History>().expect("a History");
            let mut entries = h.entries.borrow_mut();
            entries.truncate(h.index.get() + 1);
            entries.push((JsValue::from(JsString::from(url.as_str())), state));
            h.index.set(entries.len() - 1);
            Ok(JsValue::undefined())
        })
    };
    let d = dom.clone();
    // SAFETY: captures only the Rc'd DOM.
    let replace = unsafe {
        NativeFunction::from_closure(move |this, a, cx| {
            let h = this_history(this)?;
            let url = match a.get(2).filter(|u| !u.is_null_or_undefined()) {
                Some(u) => {
                    let href = crate::dom::js_value_string(u, cx);
                    crate::dom::set_url(&d, &href, 2)?
                }
                None => d.borrow().doc.url.clone(),
            };
            let state = clone_state(&a.first().cloned().unwrap_or_default(), cx);
            let h = h.downcast_ref::<History>().expect("a History");
            h.entries.borrow_mut()[h.index.get()] = (JsValue::from(JsString::from(url.as_str())), state);
            Ok(JsValue::undefined())
        })
    };
    let back = native(dom, |d, _, _| {
        crate::dom::request_traverse(d, -1);
        Ok(JsValue::undefined())
    });
    let forward = native(dom, |d, _, _| {
        crate::dom::request_traverse(d, 1);
        Ok(JsValue::undefined())
    });
    let go = native(dom, |d, a, cx| {
        let n = a.first().map(|v| v.to_number(cx)).transpose()?.unwrap_or(0.0);
        let n = if n.is_finite() { n as i32 } else { 0 };
        if n == 0 {
            let here = d.borrow().doc.url.to_string();
            crate::dom::navigate_to(d, &here);
        } else {
            crate::dom::request_traverse(d, n);
        }
        Ok(JsValue::undefined())
    });
    let attrs = Attribute::ENUMERABLE | Attribute::CONFIGURABLE;
    let mut init = ObjectInitializer::with_native_data(data, cx);
    init.accessor(js_string!("length"), Some(length), None, attrs)
        .accessor(js_string!("state"), Some(state), None, attrs)
        .property(js_string!("scrollRestoration"), js_string!("auto"), Attribute::all())
        .function(push, js_string!("pushState"), 2)
        .function(replace, js_string!("replaceState"), 2)
        .function(back, js_string!("back"), 0)
        .function(forward, js_string!("forward"), 0)
        .function(go, js_string!("go"), 1);
    init.build().into()
}

/// The tab traversed this document's history by `delta`: the entry it
/// lands on becomes current, `popstate` fires (and `hashchange` when only
/// the fragment changed).
fn history_traverse(d: &Shared, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let delta = a.first().and_then(|v| v.as_number()).unwrap_or(0.0) as i64;
    let global = cx.global_object();
    let history = global.get(js_string!("history"), cx)?;
    let h = this_history(&history)?;
    let (from, to, state) = {
        let h = h.downcast_ref::<History>().expect("a History");
        let entries = h.entries.borrow();
        let from = entries[h.index.get()].0.clone();
        let index = (h.index.get() as i64 + delta).clamp(0, entries.len() as i64 - 1) as usize;
        h.index.set(index);
        (from, entries[index].0.clone(), entries[index].1.clone())
    };
    let (from, to) = (crate::dom::js_value_string(&from, cx), crate::dom::js_value_string(&to, cx));
    crate::dom::set_url(d, &to, 0)?;
    let event = |name: &str, cx: &mut Context| -> JsResult<JsObject> {
        let ctor = cx.global_object().get(js_string!("Event"), cx)?;
        let ctor = ctor.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no Event"))?;
        ctor.construct(&[JsValue::from(JsString::from(name))], None, cx)
    };
    let dispatch = global.get(js_string!("dispatchEvent"), cx)?.as_callable().ok_or_else(|| JsNativeError::typ().with_message("no dispatchEvent"))?;
    let pop = event("popstate", cx)?;
    pop.set(js_string!("state"), state, false, cx)?;
    dispatch.call(&global.clone().into(), &[pop.into()], cx)?;
    let base = |u: &str| u.split('#').next().unwrap_or("").to_owned();
    if base(&from) == base(&to) && from != to {
        let hc = event("hashchange", cx)?;
        hc.set(js_string!("oldURL"), JsString::from(from.as_str()), false, cx)?;
        hc.set(js_string!("newURL"), JsString::from(to.as_str()), false, cx)?;
        dispatch.call(&global.into(), &[hc.into()], cx)?;
    }
    Ok(JsValue::undefined())
}
